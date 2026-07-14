//! Routing agent-initiated messages. Messages arrive over the control socket
//! (`control.rs`) and are enqueued here; the `Router` owns the authorization
//! decisions and the one message awaiting an operator decision. corrald is the
//! trusted cross-workdir bridge, so the authorization gate lives here.
//!
//! A message targets either a directory (reach whoever works there, spawning
//! one if none) or an exact session id (reach precisely that agent, resuming it
//! if dormant). Session targeting is what makes a reply land on the agent that
//! actually asked, since a directory can hold zero, one, or several sessions.
//!
//! Liveness comes straight from the registry: a record with a `socket` is
//! live, one without is dormant. The daemon does not watch sockets (that is the
//! board's job), so it delivers optimistically and falls back on a connect
//! failure — a dead socket (crashed session) is resumed from its record rather
//! than tracked. Delivery to a not-yet-live target hands the message to the
//! launcher as the new session's first prompt (appended to the record's launch
//! command), atomic with no wait-for-announce dance.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use corral_core::discovery::RegistryEntry;
use corral_core::launch::Launcher;
use corral_core::prompt;

use crate::mailbox::{is_whitelisted, whitelist_add, Message, Target};

/// An operator decision on a pending approval, produced by the tray or the
/// desktop notification and applied to the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// A message awaiting an operator decision, with its resolved target directory
/// (for the whitelist, which is keyed on dir pairs).
struct Pending {
    msg: Message,
    target_cwd: String,
}

pub struct Router {
    whitelist: PathBuf,
    /// Allow-once decisions for this daemon run (by message id).
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

    /// The message awaiting an operator decision, if any.
    pub fn pending(&self) -> Option<&Message> {
        self.pending.as_ref().map(|p| &p.msg)
    }

    /// Route every whitelisted or already-approved message in the queue. Stops
    /// at the first message that needs a decision, storing it as `pending`.
    /// Returns a status line when it acted. While a decision is pending it only
    /// releases that message if its pair was meanwhile whitelisted, else it is a
    /// no-op (one pending at a time). `entries` is a fresh registry scan (the daemon's view
    /// of who is live and dormant).
    pub fn poll(&mut self, entries: &[RegistryEntry], launcher: &dyn Launcher) -> Option<String> {
        // Release a pending message without an interactive decision if its pair
        // was meanwhile whitelisted. This is the headless approval path: with no
        // tray/notification/GUI, an operator edits ~/.corral/whitelist and the
        // daemon picks it up on the next tick. Otherwise a decision is still
        // owed, so stay put (one pending at a time).
        if let Some(p) = self.pending.take() {
            if is_whitelisted(&self.whitelist, &p.msg.from_cwd, &p.target_cwd) {
                self.approved.insert(p.msg.id.clone());
                self.queue.push_front(p.msg);
            } else {
                self.pending = Some(p);
                return None;
            }
        }
        let mut status = None;
        while let Some(msg) = self.queue.pop_front() {
            let Some(target_cwd) = target_cwd(&msg, entries) else {
                // Resolvable at accept time but gone now (rare race). Drop it.
                status = Some("route: unknown target".into());
                continue;
            };
            let ok = self.approved.contains(&msg.id)
                || is_whitelisted(&self.whitelist, &msg.from_cwd, &target_cwd);
            if !ok {
                self.pending = Some(Pending { msg, target_cwd });
                break;
            }
            status = Some(deliver(&msg, entries, launcher));
        }
        status
    }

    /// Apply an operator decision to the pending message.
    pub fn apply(&mut self, action: ApprovalAction) -> std::io::Result<()> {
        match action {
            ApprovalAction::AllowOnce => self.allow_once(),
            ApprovalAction::AllowAlways => self.allow_always()?,
            ApprovalAction::Deny => self.deny(),
        }
        Ok(())
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

/// Deliver one authorized message to its target, returning a status line.
fn deliver(msg: &Message, entries: &[RegistryEntry], launcher: &dyn Launcher) -> String {
    match &msg.target {
        Target::Dir(dir) => deliver_dir(msg, dir, entries, launcher),
        Target::Session(sid) => deliver_session(msg, sid, entries, launcher),
    }
}

/// Directory target: reuse a live agent in `dir` (over its socket), or spawn
/// one carrying the message as its first prompt. `force_new` always spawns a
/// dedicated agent. A live socket that fails to connect (crashed session) falls
/// through to a spawn.
fn deliver_dir(
    msg: &Message,
    dir: &str,
    entries: &[RegistryEntry],
    launcher: &dyn Launcher,
) -> String {
    if !msg.force_new {
        if let Some(sock) = live_socket_in_dir(entries, dir) {
            if prompt::send_prompt(&sock, &msg.tagged()).is_ok() {
                return format!("routed to {}", msg.target_label());
            }
            // Socket present but dead: fall through and spawn a fresh agent.
        }
    }
    // The spawn command rides in a record; corral names no agent kind. Reuse
    // any record (live or dormant) for this dir. A dir corral has never seen an
    // agent in has no known kind, so it cannot be spawned into.
    let Some(command) = spawn_command_for_dir(entries, dir) else {
        return format!("route: no known agent kind for {dir} (never announced there)");
    };
    match launcher.launch(Path::new(dir), command, Some(&msg.tagged())) {
        Ok(()) => format!("routed to {} (spawned)", msg.target_label()),
        Err(e) => format!("route spawn: {e}"),
    }
}

/// A spawn command announced by any record whose cwd is `dir`, live or dormant.
fn spawn_command_for_dir<'a>(entries: &'a [RegistryEntry], dir: &str) -> Option<&'a [String]> {
    entries
        .iter()
        .filter(|e| e.cwd.as_deref() == Some(dir))
        .find_map(|e| e.spawn_command.as_deref())
}

/// Session target: deliver to that exact agent over its socket if live, else
/// resume it from its record with the message as its first prompt. A live
/// socket that fails to connect (crashed) falls back to resume.
fn deliver_session(
    msg: &Message,
    session_id: &str,
    entries: &[RegistryEntry],
    launcher: &dyn Launcher,
) -> String {
    let Some(entry) = entries.iter().find(|e| e.session_id == session_id) else {
        return format!("route: session {session_id} not found");
    };
    if let Some(sock) = &entry.socket {
        if prompt::send_prompt(sock, &msg.tagged()).is_ok() {
            return format!("routed to {}", msg.target_label());
        }
        // Socket present but dead: fall through and resume from the record.
    }
    match (&entry.cwd, &entry.resume_command) {
        (Some(cwd), Some(command)) => {
            match launcher.launch(Path::new(cwd), command, Some(&msg.tagged())) {
                Ok(()) => format!("routed to {} (resumed)", msg.target_label()),
                Err(e) => format!("route resume: {e}"),
            }
        }
        _ => format!("route: session {session_id} not resumable"),
    }
}

/// The connectable socket of a live agent whose cwd is `dir`, if any.
fn live_socket_in_dir(entries: &[RegistryEntry], dir: &str) -> Option<PathBuf> {
    entries
        .iter()
        .find(|e| e.cwd.as_deref() == Some(dir) && e.socket.is_some())
        .and_then(|e| e.socket.clone())
}

/// The target's working directory, for the dir-keyed whitelist. A dir target is
/// its own cwd; a session target resolves through the registry. `None` means a
/// session the daemon does not know about.
fn target_cwd(msg: &Message, entries: &[RegistryEntry]) -> Option<String> {
    match &msg.target {
        Target::Dir(d) => Some(d.clone()),
        Target::Session(sid) => entries
            .iter()
            .find(|e| &e.session_id == sid)
            .and_then(|e| e.cwd.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox;
    use std::cell::{Cell, RefCell};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// Records launch calls, classifying by the argv the record carried: a
    /// resume command contains `--session`, a fresh spawn does not.
    #[derive(Default)]
    struct StubLauncher {
        spawns: Cell<usize>,
        resumes: Cell<usize>,
        last_msg: RefCell<Option<String>>,
    }
    impl Launcher for StubLauncher {
        fn launch(
            &self,
            _cwd: &Path,
            command: &[String],
            message: Option<&str>,
        ) -> Result<(), String> {
            if command.iter().any(|a| a == "--session") {
                self.resumes.set(self.resumes.get() + 1);
            } else {
                self.spawns.set(self.spawns.get() + 1);
            }
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

    /// A record for `cwd` carrying a spawn command but no live socket: the
    /// daemon learns a dir's agent kind from any record there.
    fn dir_record(cwd: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: format!("rec{cwd}"),
            cwd: Some(cwd.into()),
            title: None,
            socket: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: None,
            label: Some("pi".into()),
            last_seen: None,
        }
    }

    fn dormant(session_id: &str, cwd: &str, resume: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: session_id.into(),
            cwd: Some(cwd.into()),
            title: None,
            socket: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), resume.into()]),
            label: Some("pi".into()),
            last_seen: None,
        }
    }

    #[test]
    fn unauthorized_message_becomes_pending_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        assert!(r.poll(&[], &launcher).is_none());
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
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher);
        assert!(r.pending().is_none(), "whitelisted needs no decision");
        assert_eq!(launcher.spawns.get(), 1, "spawned an agent in the target");
        assert_eq!(
            launcher.last_msg.borrow().as_deref(),
            Some("[from agent in /a] hi"),
            "the provenance-tagged message is the new session's first prompt"
        );
    }

    #[test]
    fn whitelist_edit_releases_an_already_pending_message() {
        // Headless approval: a message goes pending, then the pair is added to
        // the whitelist file out of band; the next poll releases and delivers
        // it with no operator decision.
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        let mut r = Router::new(whitelist.clone());
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher); // -> pending (not yet whitelisted)
        assert_eq!(r.pending().map(|m| m.id.as_str()), Some("1"));
        assert_eq!(launcher.spawns.get(), 0);

        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        r.poll(&entries, &launcher); // whitelist edit picked up -> delivered
        assert!(r.pending().is_none(), "released by the whitelist edit");
        assert_eq!(launcher.spawns.get(), 1);
        r.poll(&entries, &launcher); // nothing left to do
        assert_eq!(
            launcher.spawns.get(),
            1,
            "released message delivers only once"
        );
    }

    #[test]
    fn allow_always_persists_authorizes_and_delivers() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        let mut r = Router::new(whitelist.clone());
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher); // -> pending
        r.apply(ApprovalAction::AllowAlways).unwrap();
        assert!(mailbox::is_whitelisted(&whitelist, "/a", "/b"));
        r.poll(&entries, &launcher); // re-queued -> delivered
        assert_eq!(launcher.spawns.get(), 1);
        r.poll(&entries, &launcher); // no residual re-delivery
        assert_eq!(launcher.spawns.get(), 1, "allow_always delivers only once");
    }

    #[test]
    fn deny_drops_the_message() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        r.enqueue(dir_msg("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&[], &launcher); // -> pending
        r.apply(ApprovalAction::Deny).unwrap();
        assert!(r.pending().is_none());
        r.poll(&[], &launcher);
        assert_eq!(launcher.spawns.get(), 0, "denied -> never delivered");
    }

    #[test]
    fn dormant_session_target_is_resumed_with_message() {
        let tmp = tempfile::tempdir().unwrap();
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
        let entries = [dormant("sid-7", "/b", "/s/sid-7.jsonl")];

        r.poll(&entries, &launcher);
        assert!(r.pending().is_none(), "whitelisted: no operator prompt");
        assert_eq!(launcher.resumes.get(), 1, "dormant session is resumed");
        assert_eq!(
            launcher.last_msg.borrow().as_deref(),
            Some("[from agent in /a] hi")
        );
    }

    #[test]
    fn unknown_session_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        // Whitelist an unrelated pair; the point is the session does not exist.
        let mut r = Router::new(whitelist);
        r.enqueue(
            mailbox::parse_message(
                r#"{"id":"1","fromCwd":"/a","targetSession":"ghost","message":"hi"}"#,
            )
            .unwrap(),
        );
        let launcher = StubLauncher::default();

        let status = r.poll(&[], &launcher);
        assert!(status.unwrap().contains("unknown target"));
        assert!(r.pending().is_none());
        assert_eq!(launcher.spawns.get(), 0);
        assert_eq!(launcher.resumes.get(), 0);
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
        let entries = [RegistryEntry {
            session_id: "sid-7".into(),
            cwd: Some("/b".into()),
            title: None,
            socket: Some(sock.clone()),
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec![
                "pi".into(),
                "--session".into(),
                "/s/sid-7.jsonl".into(),
            ]),
            label: Some("pi".into()),
            last_seen: None,
        }];
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

        r.poll(&entries, &launcher);
        handle.join().unwrap();
        assert_eq!(launcher.spawns.get(), 0, "live session needs no spawn");
        assert_eq!(launcher.resumes.get(), 0, "live socket needs no resume");
    }
}
