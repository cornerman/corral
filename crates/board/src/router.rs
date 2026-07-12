//! Routing agent-initiated messages from the outbox. The `Router` owns all the
//! routing state (authorization decisions, in-flight spawns, and the one
//! message awaiting an operator decision) so the event loop only has to poll it
//! and forward key presses. corral is the trusted cross-workdir bridge, so the
//! authorization gate lives here.
//!
//! A message targets either a directory (reach whoever works there, spawning
//! one if none) or an exact session id (reach precisely that agent, resuming it
//! if dormant). Session targeting is what makes a reply land on the agent that
//! actually asked, since a directory can hold zero, one, or several sessions.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::launch::Launcher;
use crate::mailbox::{self, Message, Target};
use crate::model::Board;
use crate::prompt;

/// A message being routed to a target that had no ready agent yet: the router
/// spawned (dir) or resumed (session) one and waits for it to announce.
struct RouteState {
    spawned: bool,
    /// For a directory target: sockets already live there before the spawn, so
    /// the newly spawned agent can be told apart. Empty for a session target
    /// (the session id already identifies the agent exactly).
    pre: HashSet<PathBuf>,
}

/// A message awaiting an operator decision, with its mailbox file and the
/// resolved target directory (for the whitelist, which is keyed on dir pairs).
struct Pending {
    file: PathBuf,
    msg: Message,
    target_cwd: String,
}

/// An operator message (`m`) to a specific session, delivered on the next poll
/// after resuming the session if it is dormant. No provenance tag and no
/// approval: the operator is trusted, unlike an agent-initiated message.
struct OpDelivery {
    session_id: String,
    text: String,
    resumed: bool,
}

pub struct Router {
    outbox: PathBuf,
    whitelist: PathBuf,
    /// Allow-once decisions for this board run (by message id).
    approved: HashSet<String>,
    /// Spawns/resumes in flight, keyed by message id.
    routing: HashMap<String, RouteState>,
    pending: Option<Pending>,
    /// Operator messages awaiting delivery (resume-then-send).
    ops: Vec<OpDelivery>,
}

impl Router {
    pub fn new(outbox: PathBuf, whitelist: PathBuf) -> Self {
        Self {
            outbox,
            whitelist,
            approved: HashSet::new(),
            routing: HashMap::new(),
            pending: None,
            ops: Vec::new(),
        }
    }

    /// Queue an operator message to a session by id (from the board's `m`).
    /// Delivered on the next poll, resuming the session first if dormant.
    pub fn operator_send(&mut self, session_id: String, text: String) {
        self.ops.push(OpDelivery {
            session_id,
            text,
            resumed: false,
        });
    }

    /// The message awaiting an operator decision, if any (for the overlay).
    pub fn pending(&self) -> Option<&Message> {
        self.pending.as_ref().map(|p| &p.msg)
    }

    /// Scan the outbox and deliver every whitelisted or already-approved
    /// message. Stops at the first message that needs a decision, storing it as
    /// `pending`. Returns a status line when it acted. A no-op while a decision
    /// is pending (one at a time).
    pub fn poll(&mut self, board: &Board, launcher: &dyn Launcher) -> Option<String> {
        // Operator messages deliver regardless of any pending agent approval.
        let mut op_status = self.pump_operator(board, launcher);
        if self.pending.is_some() {
            return op_status;
        }
        let pending = mailbox::scan_outbox(&self.outbox);
        // Forget spawn-tracking for messages no longer queued.
        let ids: HashSet<&str> = pending.iter().map(|(_, m)| m.id.as_str()).collect();
        self.routing.retain(|k, _| ids.contains(k.as_str()));

        let mut status = op_status.take();
        for (file, msg) in pending {
            let Some(target_cwd) = target_cwd(&msg, board) else {
                // A session target corral has never seen: nothing to deliver
                // to. Drop it rather than prompt for an undeliverable message.
                let _ = std::fs::remove_file(&file);
                self.routing.remove(&msg.id);
                status = Some("route: unknown target session".into());
                continue;
            };
            let ok = self.approved.contains(&msg.id)
                || mailbox::is_whitelisted(&self.whitelist, &msg.from_cwd, &target_cwd);
            if !ok {
                self.pending = Some(Pending {
                    file,
                    msg,
                    target_cwd,
                });
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
        if let Some(p) = self.pending.take() {
            self.approved.insert(p.msg.id);
        }
    }

    /// Allow the pending message and persist its `(from -> target)` dir pair.
    pub fn allow_always(&mut self) -> std::io::Result<()> {
        if let Some(p) = self.pending.take() {
            mailbox::whitelist_add(&self.whitelist, &p.msg.from_cwd, &p.target_cwd)?;
            self.approved.insert(p.msg.id);
        }
        Ok(())
    }

    /// Deny the pending message: drop its mailbox file.
    pub fn deny(&mut self) {
        if let Some(p) = self.pending.take() {
            let _ = std::fs::remove_file(p.file);
            self.routing.remove(&p.msg.id);
        }
    }

    /// Deliver queued operator messages: send to the live session, or resume a
    /// dormant one (once) and wait for it to announce. Returns a status line.
    fn pump_operator(&mut self, board: &Board, launcher: &dyn Launcher) -> Option<String> {
        let mut status = None;
        self.ops.retain_mut(|op| {
            if let Some(agent) = board.live_by_session(&op.session_id) {
                let sock = agent.socket_path.clone();
                status = Some(match prompt::send_prompt(&sock, &op.text) {
                    Ok(()) => "message delivered".into(),
                    Err(e) => format!("send: {e}"),
                });
                return false; // done
            }
            if op.resumed {
                return true; // waiting for the resumed session to announce
            }
            op.resumed = true;
            match board.dormant_by_session(&op.session_id) {
                Some(d) => match (&d.cwd, &d.resume) {
                    (Some(cwd), Some(resume)) => {
                        if let Err(e) = launcher.resume(Path::new(cwd), resume) {
                            status = Some(format!("resume: {e}"));
                        }
                        true // wait for it to come back live
                    }
                    _ => {
                        status = Some("send: session not resumable".into());
                        false
                    }
                },
                None => {
                    status = Some("send: session not found".into());
                    false
                }
            }
        });
        status
    }

    /// Deliver one authorized message to its target.
    fn deliver(
        &mut self,
        file: &Path,
        msg: &Message,
        board: &Board,
        launcher: &dyn Launcher,
    ) -> Option<String> {
        match &msg.target {
            Target::Dir(dir) => self.deliver_dir(file, msg, dir, board, launcher),
            Target::Session(sid) => self.deliver_session(file, msg, sid, board, launcher),
        }
    }

    /// Directory target: reuse a live agent in `dir`, or spawn one (and, for
    /// `force_new`, always a dedicated agent) and wait for it to announce.
    fn deliver_dir(
        &mut self,
        file: &Path,
        msg: &Message,
        dir: &str,
        board: &Board,
        launcher: &dyn Launcher,
    ) -> Option<String> {
        let in_dir = board.live_in_dir(dir);
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
                .spawn(Path::new(dir))
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

    /// Session target: deliver to that exact agent if live, else resume it from
    /// its dormant record and wait for it to reannounce under the same id.
    fn deliver_session(
        &mut self,
        file: &Path,
        msg: &Message,
        session_id: &str,
        board: &Board,
        launcher: &dyn Launcher,
    ) -> Option<String> {
        if let Some(agent) = board.live_by_session(session_id) {
            let sock = agent.socket_path.clone();
            return Some(self.finish(file, &sock, msg));
        }
        // Not live: resume from the dormant record, once.
        let (cwd, resume) = match board.dormant_by_session(session_id) {
            Some(d) => (d.cwd.clone(), d.resume.clone()),
            None => {
                let _ = std::fs::remove_file(file);
                self.routing.remove(&msg.id);
                return Some(format!("route: session {session_id} not found"));
            }
        };
        let r = self
            .routing
            .entry(msg.id.clone())
            .or_insert_with(|| RouteState {
                spawned: false,
                pre: HashSet::new(),
            });
        if !r.spawned {
            r.spawned = true;
            return match (cwd, resume) {
                (Some(cwd), Some(resume)) => launcher
                    .resume(Path::new(&cwd), &resume)
                    .err()
                    .map(|e| format!("route resume: {e}")),
                _ => Some(format!("route: session {session_id} not resumable")),
            };
        }
        // Waiting for the resumed session to come back live; the next poll's
        // live_by_session branch delivers it.
        None
    }

    /// Inject a delivered message over the target socket and drop the mailbox
    /// file.
    fn finish(&mut self, file: &Path, socket: &Path, msg: &Message) -> String {
        let status = match prompt::send_prompt(socket, &msg.tagged()) {
            Ok(()) => format!("routed to {}", msg.target_label()),
            Err(e) => format!("route send: {e}"),
        };
        let _ = std::fs::remove_file(file);
        self.routing.remove(&msg.id);
        status
    }
}

/// The target's working directory, used for the dir-keyed whitelist. A dir
/// target is its own cwd; a session target resolves through the board (live or
/// dormant). `None` means a session corral does not know about.
fn target_cwd(msg: &Message, board: &Board) -> Option<String> {
    match &msg.target {
        Target::Dir(d) => Some(d.clone()),
        Target::Session(sid) => board.by_session(sid).and_then(|a| a.cwd.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Origin, State, Update};
    use std::cell::Cell;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// Records spawn/resume calls; never launches anything.
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

    fn outbox_in(dir: &Path) -> PathBuf {
        let o = dir.join("outbox");
        std::fs::create_dir(&o).unwrap();
        o
    }

    fn write_dir_msg(outbox: &Path, id: &str, from: &str, target: &str) {
        std::fs::write(
            outbox.join(format!("{id}.json")),
            format!(r#"{{"id":"{id}","fromCwd":"{from}","targetDir":"{target}","message":"hi"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn unauthorized_message_becomes_pending_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = outbox_in(tmp.path());
        write_dir_msg(&outbox, "1", "/a", "/b");
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
        let outbox = outbox_in(tmp.path());
        write_dir_msg(&outbox, "1", "/a", "/b");
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
        let outbox = outbox_in(tmp.path());
        write_dir_msg(&outbox, "1", "/a", "/b");
        let whitelist = tmp.path().join("whitelist");
        let mut r = Router::new(outbox, whitelist.clone());
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        r.poll(&Board::default(), &launcher); // -> pending
        r.allow_always().unwrap();
        assert!(mailbox::is_whitelisted(&whitelist, "/a", "/b"));
        r.poll(&Board::default(), &launcher);
        assert_eq!(launcher.spawns.get(), 1);
    }

    #[test]
    fn unknown_session_target_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = outbox_in(tmp.path());
        std::fs::write(
            outbox.join("1.json"),
            r#"{"id":"1","fromCwd":"/a","targetSession":"ghost","message":"hi"}"#,
        )
        .unwrap();
        let mut r = Router::new(outbox.clone(), tmp.path().join("whitelist"));
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        r.poll(&Board::default(), &launcher);
        assert!(r.pending().is_none(), "no one to ask about");
        assert!(!outbox.join("1.json").exists(), "dropped as undeliverable");
    }

    #[test]
    fn delivers_to_a_live_session_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = outbox_in(tmp.path());
        // A live agent listening on a socket: session "sid-7", cwd "/b".
        let sock = tmp.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                // Seed like the real extension so send_prompt's drain returns.
                let _ = c.write_all(b"{\"seed\":true}\n");
                let mut buf = [0u8; 512];
                let _ = c.read(&mut buf);
            }
        });
        let mut board = Board::default();
        board.apply(Update::Upsert(Agent {
            socket_path: sock.clone(),
            pid: 1,
            label: "pi".into(),
            session_id: Some("sid-7".into()),
            title: None,
            cwd: Some("/b".into()),
            state: State::Idle,
            origin: Origin::Live,
            resume: None,
            activity: None,
        }));
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        std::fs::write(
            outbox.join("1.json"),
            r#"{"id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
        )
        .unwrap();
        let mut r = Router::new(outbox.clone(), whitelist);
        let launcher = StubLauncher {
            spawns: Cell::new(0),
        };

        r.poll(&board, &launcher);
        handle.join().unwrap();
        assert!(!outbox.join("1.json").exists(), "delivered, file removed");
        assert_eq!(launcher.spawns.get(), 0, "live session needs no spawn");
    }
}
