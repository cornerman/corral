//! Routing agent-initiated messages. Messages arrive over the control socket
//! (`control.rs`) and are enqueued here; the `Router` owns the authorization
//! decisions and the one message awaiting an operator decision, so the event
//! loop only enqueues, polls, and forwards key presses. corral is the trusted
//! cross-workdir bridge, so the authorization gate lives here.
//!
//! A message targets either a directory (reach whoever works there, spawning
//! one if none) or an exact session id (reach precisely that agent, resuming it
//! if dormant). Session targeting is what makes a reply land on the agent that
//! actually asked, since a directory can hold zero, one, or several sessions.
//!
//! Delivery to a not-yet-live target hands the message to the launcher as the
//! new session's first prompt (`pi "<message>"`), so a spawn/resume delivers
//! atomically with no wait-for-announce dance. A target that is already live
//! gets the message over its socket.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::launch::Launcher;
use crate::mailbox::{is_whitelisted, whitelist_add, Message, Target};
use crate::model::Board;
use crate::prompt;

/// A message awaiting an operator decision, with its resolved target directory
/// (for the whitelist, which is keyed on dir pairs).
struct Pending {
    msg: Message,
    target_cwd: String,
}

pub struct Router {
    whitelist: PathBuf,
    /// Allow-once decisions for this board run (by message id).
    approved: HashSet<String>,
    /// Messages accepted over the control socket, awaiting routing.
    queue: VecDeque<Message>,
    pending: Option<Pending>,
}

impl Router {
    pub fn new(whitelist: PathBuf) -> Self {
        Self {
            whitelist,
            approved: HashSet::new(),
            queue: VecDeque::new(),
            pending: None,
        }
    }

    /// Accept a message from the control socket for routing on the next poll.
    pub fn enqueue(&mut self, msg: Message) {
        self.queue.push_back(msg);
    }

    /// The message awaiting an operator decision, if any (for the overlay).
    pub fn pending(&self) -> Option<&Message> {
        self.pending.as_ref().map(|p| &p.msg)
    }

    /// Route every whitelisted or already-approved message in the queue. Stops
    /// at the first message that needs a decision, storing it as `pending`.
    /// Returns a status line when it acted. A no-op while a decision is pending
    /// (one at a time).
    pub fn poll(&mut self, board: &Board, launcher: &dyn Launcher) -> Option<String> {
        if self.pending.is_some() {
            return None;
        }
        let mut status = None;
        // A message whose live target has not yet been discovered is deferred
        // and retried on the next poll (it cannot be launch-delivered: the
        // agent is already running, just not watched yet).
        let mut deferred = Vec::new();
        while let Some(msg) = self.queue.pop_front() {
            let Some(target_cwd) = target_cwd(&msg, board) else {
                // Resolvable at accept time but gone now (rare race). Drop it.
                status = Some("route: unknown target session".into());
                continue;
            };
            let ok = self.approved.contains(&msg.id)
                || is_whitelisted(&self.whitelist, &msg.from_cwd, &target_cwd);
            if !ok {
                self.pending = Some(Pending { msg, target_cwd });
                break;
            }
            match deliver(&msg, board, launcher) {
                Some(s) => status = Some(s),
                None => deferred.push(msg),
            }
        }
        for m in deferred.into_iter().rev() {
            self.queue.push_front(m);
        }
        status
    }

    /// Allow the pending message once (this run), then route it on the next poll.
    pub fn allow_once(&mut self) {
        if let Some(p) = self.pending.take() {
            self.approved.insert(p.msg.id.clone());
            self.queue.push_front(p.msg);
        }
    }

    /// Allow the pending message and persist its `(from -> target)` dir pair.
    pub fn allow_always(&mut self) -> std::io::Result<()> {
        if let Some(p) = self.pending.take() {
            whitelist_add(&self.whitelist, &p.msg.from_cwd, &p.target_cwd)?;
            self.approved.insert(p.msg.id.clone());
            self.queue.push_front(p.msg);
        }
        Ok(())
    }

    /// Deny the pending message: drop it.
    pub fn deny(&mut self) {
        self.pending = None;
    }
}

/// Deliver one authorized message to its target.
fn deliver(msg: &Message, board: &Board, launcher: &dyn Launcher) -> Option<String> {
    match &msg.target {
        Target::Dir(dir) => deliver_dir(msg, dir, board, launcher),
        Target::Session(sid) => deliver_session(msg, sid, board, launcher),
    }
}

/// Directory target: reuse a live agent in `dir`, or spawn one (and, for
/// `force_new`, always a dedicated agent) carrying the message as its first
/// prompt.
fn deliver_dir(msg: &Message, dir: &str, board: &Board, launcher: &dyn Launcher) -> Option<String> {
    if !msg.force_new {
        let in_dir = board.live_in_dir(dir);
        if let Some(agent) = in_dir.first() {
            let sock = agent.socket_path.clone();
            return Some(finish(&sock, msg));
        }
    }
    Some(match launcher.spawn(Path::new(dir), Some(&msg.tagged())) {
        Ok(()) => format!("routed to {} (spawned)", msg.target_label()),
        Err(e) => format!("route spawn: {e}"),
    })
}

/// Session target: deliver to that exact agent if live, else resume it from its
/// dormant record with the message as its first prompt. A live-but-undiscovered
/// session (socket not yet watched) returns `None` to be retried next poll.
fn deliver_session(
    msg: &Message,
    session_id: &str,
    board: &Board,
    launcher: &dyn Launcher,
) -> Option<String> {
    if let Some(agent) = board.live_by_session(session_id) {
        let sock = agent.socket_path.clone();
        return Some(finish(&sock, msg));
    }
    match board.dormant_by_session(session_id) {
        Some(d) => Some(match (&d.cwd, &d.resume) {
            (Some(cwd), Some(resume)) => {
                match launcher.resume(Path::new(cwd), resume, Some(&msg.tagged())) {
                    Ok(()) => format!("routed to {} (resumed)", msg.target_label()),
                    Err(e) => format!("route resume: {e}"),
                }
            }
            _ => format!("route: session {session_id} not resumable"),
        }),
        None => {
            if board.registry_has_session(session_id) {
                return None; // live socket, watcher not announced yet: retry
            }
            Some(format!("route: session {session_id} not found"))
        }
    }
}

/// Inject a delivered message over a live target's socket.
fn finish(socket: &Path, msg: &Message) -> String {
    match prompt::send_prompt(socket, &msg.tagged()) {
        Ok(()) => format!("routed to {}", msg.target_label()),
        Err(e) => format!("route send: {e}"),
    }
}

/// The target's working directory, used for the dir-keyed whitelist. A dir
/// target is its own cwd; a session target resolves through the board (live or
/// dormant). `None` means a session corral does not know about.
fn target_cwd(msg: &Message, board: &Board) -> Option<String> {
    match &msg.target {
        Target::Dir(d) => Some(d.clone()),
        // A discovered agent's own cwd first, then the registry, so a live
        // session whose watcher has not announced yet still resolves its cwd.
        Target::Session(sid) => board
            .by_session(sid)
            .and_then(|a| a.cwd.clone())
            .or_else(|| board.registry_session_cwd(sid)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox;
    use crate::model::{Agent, Origin, State, Update};
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// Records spawn/resume calls and the initial message they carried.
    #[derive(Default)]
    struct StubLauncher {
        spawns: Cell<usize>,
        last_msg: RefCell<Option<String>>,
    }
    impl Launcher for StubLauncher {
        fn spawn(&self, _cwd: &Path, message: Option<&str>) -> Result<(), String> {
            self.spawns.set(self.spawns.get() + 1);
            *self.last_msg.borrow_mut() = message.map(str::to_owned);
            Ok(())
        }
        fn resume(&self, _cwd: &Path, _resume: &str, message: Option<&str>) -> Result<(), String> {
            *self.last_msg.borrow_mut() = message.map(str::to_owned);
            Ok(())
        }
    }

    fn dir_msg(id: &str, from: &str, target: &str) -> Message {
        mailbox::parse_message(&format!(
            r#"{{"id":"{id}","fromCwd":"{from}","targetDir":"{target}","message":"hi"}}"#
        ))
        .unwrap()
    }

    #[test]
    fn unauthorized_message_becomes_pending_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        assert!(r.poll(&Board::default(), &launcher).is_none());
        assert_eq!(r.pending().map(|m| m.id.as_str()), Some("1"));
        assert_eq!(launcher.spawns.get(), 0, "no delivery before approval");
    }

    #[test]
    fn whitelisted_message_with_no_agent_spawns_with_message() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&Board::default(), &launcher);
        assert!(r.pending().is_none(), "whitelisted needs no decision");
        assert_eq!(launcher.spawns.get(), 1, "spawned an agent in the target");
        assert_eq!(
            launcher.last_msg.borrow().as_deref(),
            Some("[from agent in /a] hi"),
            "the provenance-tagged message is the new session's first prompt"
        );
    }

    #[test]
    fn allow_always_persists_authorizes_and_delivers() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        let mut r = Router::new(whitelist.clone());
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&Board::default(), &launcher); // -> pending
        r.allow_always().unwrap();
        assert!(mailbox::is_whitelisted(&whitelist, "/a", "/b"));
        r.poll(&Board::default(), &launcher); // re-queued -> delivered
        assert_eq!(launcher.spawns.get(), 1);
    }

    #[test]
    fn deny_drops_the_message() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&Board::default(), &launcher); // -> pending
        r.deny();
        assert!(r.pending().is_none());
        r.poll(&Board::default(), &launcher);
        assert_eq!(launcher.spawns.get(), 0, "denied -> never delivered");
    }

    #[test]
    fn known_but_undiscovered_session_is_kept_not_dropped() {
        // Right after corral starts, watchers have not announced, so a
        // currently-live session is neither on the live board nor dormant (its
        // record still names a socket). A session-addressed message must wait
        // for the watcher, not be dropped as "unknown target".
        let tmp = tempfile::tempdir().unwrap();
        let mut board = Board::default();
        board.sync_registry(
            &[crate::discovery::RegistryEntry {
                session_id: "sid-7".into(),
                cwd: Some("/b".into()),
                title: None,
                socket: Some(tmp.path().join("pi-1.sock")),
                resume: Some("/s/sid-7.jsonl".into()),
                label: Some("pi".into()),
                last_seen: None,
            }],
            &HashSet::new(),
        );
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        r.enqueue(
            mailbox::parse_message(
                r#"{"id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
            )
            .unwrap(),
        );
        let launcher = StubLauncher::default();

        assert!(
            r.poll(&board, &launcher).is_none(),
            "deferred, not delivered"
        );
        assert_eq!(launcher.spawns.get(), 0, "live socket needs no resume");
        assert!(r.pending().is_none(), "whitelisted: no operator prompt");
        // Still queued for a later poll once the watcher announces.
        assert!(r.poll(&board, &launcher).is_none());
    }

    #[test]
    fn delivers_to_a_live_session_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        // A live agent listening on a socket: session "sid-7", cwd "/b".
        let sock = tmp.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
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
        let mut r = Router::new(whitelist);
        r.enqueue(
            mailbox::parse_message(
                r#"{"id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
            )
            .unwrap(),
        );
        let launcher = StubLauncher::default();

        r.poll(&board, &launcher);
        handle.join().unwrap();
        assert_eq!(launcher.spawns.get(), 0, "live session needs no spawn");
    }
}
