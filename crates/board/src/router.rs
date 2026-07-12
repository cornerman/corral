//! Routing agent-initiated messages from the outbox. The `Router` owns all the
//! routing state (authorization decisions, in-flight spawns, and the one
//! message awaiting an operator decision) so the event loop only has to poll it
//! and forward key presses. corral is the trusted cross-workdir bridge, so the
//! authorization gate lives here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::launch::Launcher;
use crate::mailbox::{self, Message};
use crate::model::Board;
use crate::prompt;

/// A message being routed to a directory that had no (or, with `force_new`, no
/// dedicated) agent yet: the router spawned one and waits for it to announce.
struct RouteState {
    spawned: bool,
    /// Sockets already live in the target dir before the spawn, so the newly
    /// spawned agent can be told apart and receive the message.
    pre: HashSet<PathBuf>,
}

pub struct Router {
    outbox: PathBuf,
    whitelist: PathBuf,
    /// Allow-once decisions for this board run (by message id).
    approved: HashSet<String>,
    /// Decide-later decisions: left queued, not asked about again this run.
    deferred: HashSet<String>,
    /// Spawns in flight, keyed by message id.
    routing: HashMap<String, RouteState>,
    /// The one message awaiting an operator decision, with its mailbox file.
    pending: Option<(PathBuf, Message)>,
}

impl Router {
    pub fn new(outbox: PathBuf, whitelist: PathBuf) -> Self {
        Self {
            outbox,
            whitelist,
            approved: HashSet::new(),
            deferred: HashSet::new(),
            routing: HashMap::new(),
            pending: None,
        }
    }

    /// The message awaiting an operator decision, if any (for the overlay).
    pub fn pending(&self) -> Option<&Message> {
        self.pending.as_ref().map(|(_, m)| m)
    }

    /// Scan the outbox and deliver every whitelisted or already-approved
    /// message (spawning an agent in the target dir if needed). Stops at the
    /// first message that needs a decision, storing it as `pending`. Returns a
    /// status line when it acted. A no-op while a decision is pending (one at a
    /// time).
    pub fn poll(&mut self, board: &Board, launcher: &dyn Launcher) -> Option<String> {
        if self.pending.is_some() {
            return None;
        }
        let pending = mailbox::scan_outbox(&self.outbox);
        // Forget spawn-tracking for messages no longer queued.
        let ids: HashSet<&str> = pending.iter().map(|(_, m)| m.id.as_str()).collect();
        self.routing.retain(|k, _| ids.contains(k.as_str()));

        let mut status = None;
        for (file, msg) in pending {
            if self.deferred.contains(&msg.id) {
                continue;
            }
            let ok = self.approved.contains(&msg.id)
                || mailbox::is_whitelisted(&self.whitelist, &msg.from_cwd, &msg.target_dir);
            if !ok {
                self.pending = Some((file, msg));
                return status;
            }
            if let Some(s) = self.deliver(&file, &msg, board, launcher) {
                status = Some(s);
            }
        }
        status
    }

    /// Allow the pending message once (this run), then route it on the next poll.
    pub fn allow_once(&mut self) {
        if let Some((_, m)) = self.pending.take() {
            self.approved.insert(m.id);
        }
    }

    /// Allow the pending message and persist its `(from -> target)` pair.
    pub fn allow_always(&mut self) -> std::io::Result<()> {
        if let Some((_, m)) = self.pending.take() {
            mailbox::whitelist_add(&self.whitelist, &m.from_cwd, &m.target_dir)?;
            self.approved.insert(m.id);
        }
        Ok(())
    }

    /// Deny the pending message: drop its mailbox file.
    pub fn deny(&mut self) {
        if let Some((file, m)) = self.pending.take() {
            let _ = std::fs::remove_file(file);
            self.routing.remove(&m.id);
        }
    }

    /// Defer the pending message: leave it queued, stop asking this run.
    pub fn defer(&mut self) {
        if let Some((_, m)) = self.pending.take() {
            self.deferred.insert(m.id);
        }
    }

    /// Deliver one authorized message: reuse the live agent in the target dir,
    /// or spawn one (and, for `force_new`, always a dedicated agent) and wait
    /// for it to announce before injecting.
    fn deliver(
        &mut self,
        file: &Path,
        msg: &Message,
        board: &Board,
        launcher: &dyn Launcher,
    ) -> Option<String> {
        let in_dir = board.live_in_dir(&msg.target_dir);
        if !msg.force_new {
            if let Some(agent) = in_dir.first() {
                let sock = agent.socket_path.clone();
                return Some(self.finish(file, &sock, msg));
            }
        }
        let r = self
            .routing
            .entry(msg.id.clone())
            .or_insert_with(|| RouteState {
                spawned: false,
                pre: in_dir.iter().map(|a| a.socket_path.clone()).collect(),
            });
        if !r.spawned {
            r.spawned = true;
            return launcher
                .spawn(Path::new(&msg.target_dir))
                .err()
                .map(|e| format!("route spawn: {e}"));
        }
        // Deliver to the agent that appeared after our spawn.
        if let Some(agent) = in_dir.iter().find(|a| !r.pre.contains(&a.socket_path)) {
            let sock = agent.socket_path.clone();
            return Some(self.finish(file, &sock, msg));
        }
        None
    }

    /// Inject a delivered message over the target socket and drop the mailbox
    /// file.
    fn finish(&mut self, file: &Path, socket: &Path, msg: &Message) -> String {
        let status = match prompt::send_prompt(socket, &msg.tagged()) {
            Ok(()) => format!("routed to {}", msg.target_dir),
            Err(e) => format!("route send: {e}"),
        };
        let _ = std::fs::remove_file(file);
        self.routing.remove(&msg.id);
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Records spawn calls; never launches anything.
    struct StubLauncher {
        spawns: Cell<usize>,
    }
    impl Launcher for StubLauncher {
        fn spawn(&self, _cwd: &Path) -> Result<(), String> {
            self.spawns.set(self.spawns.get() + 1);
            Ok(())
        }
        fn resume(&self, _cwd: &Path, _resume: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn write_msg(dir: &Path, id: &str, from: &str, target: &str) {
        std::fs::write(
            dir.join(format!("{id}.json")),
            format!(r#"{{"id":"{id}","fromCwd":"{from}","targetDir":"{target}","message":"hi"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn unauthorized_message_becomes_pending_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir(&outbox).unwrap();
        write_msg(&outbox, "1", "/a", "/b");
        let mut r = Router::new(outbox, tmp.path().join("whitelist"));
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        assert!(r.poll(&Board::default(), &launcher).is_none());
        assert_eq!(r.pending().map(|m| m.id.as_str()), Some("1"));
        assert_eq!(launcher.spawns.get(), 0, "no delivery before approval");
    }

    #[test]
    fn whitelisted_message_with_no_agent_spawns_target() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir(&outbox).unwrap();
        write_msg(&outbox, "1", "/a", "/b");
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(outbox, whitelist);
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        r.poll(&Board::default(), &launcher);
        assert!(r.pending().is_none(), "whitelisted needs no decision");
        assert_eq!(launcher.spawns.get(), 1, "spawned an agent in the target");
    }

    #[test]
    fn allow_always_persists_and_authorizes() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = tmp.path().join("outbox");
        std::fs::create_dir(&outbox).unwrap();
        write_msg(&outbox, "1", "/a", "/b");
        let whitelist = tmp.path().join("whitelist");
        let mut r = Router::new(outbox, whitelist.clone());
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        r.poll(&Board::default(), &launcher); // -> pending
        r.allow_always().unwrap();
        assert!(mailbox::is_whitelisted(&whitelist, "/a", "/b"));
        // Now authorized: next poll delivers (spawns the target).
        r.poll(&Board::default(), &launcher);
        assert_eq!(launcher.spawns.get(), 1);
    }
}
