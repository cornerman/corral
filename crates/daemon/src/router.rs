//! Routing agent-initiated submissions. They arrive over the control socket
//! (`control.rs`) and are enqueued here; the `Router` owns the authorization
//! decisions and the submissions awaiting an operator decision. Pending
//! approvals are held in a list, each resolved independently by id, so an
//! un-approved item never blocks an authorized (or separately approved) one
//! behind it. corrald is the trusted cross-workdir bridge, so the authorization
//! gate lives here.
//!
//! Three verbs (`mailbox::Kind`): message an exact session (resuming it if
//! dormant), spawn a fresh agent in a directory, or stop a session. Session
//! addressing is what makes a reply land on the agent that actually asked,
//! since a directory can hold zero, one, or several sessions; a spawn names a
//! directory because nothing runs there yet to address.
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

use corral_core::approved_commands;
use corral_core::discovery::{self, RegistryEntry};
use corral_core::launch::{LaunchMode, Launcher};
use corral_core::placement;
use corral_core::prompt;

use crate::mailbox::{is_whitelisted, whitelist_add, Kind, Submission};

/// Terminate a process by pid. The real path is `placement::kill_pid`; tests
/// inject a recording stub so a unit test never kills a real process.
type Kill = Box<dyn Fn(u32) -> Result<(), String> + Send>;

/// The swarm charter, prepended to the first prompt of a freshly spawned
/// agent (ported from the subagents extension, adapted to corral's verbs
/// and its cross-box, sandboxed reality). It teaches a new agent that it is
/// part of a swarm reachable only through corral, to confirm the task before
/// working, to escalate uncertainty up, and to stay event-driven. The task
/// itself (the provenance-tagged message) follows this block.
const CHARTER: &str = concat!(
    "You are a coding agent reached through corral, a board that connects independent\n",
    "agent sessions running in separate working directories. Another agent spawned you to\n",
    "do a task. You are sandboxed to your own directory and cannot see any other agent's\n",
    "screen, thinking, or transcript.\n",
    "\n",
    "Your only channel to other agents is corral's tools:\n",
    "- corral_message_agent({target_session, message}): message one exact agent, named by\n",
    "  the session id in a message's [from <dir> (session <id>)] reply handle.\n",
    "- corral_spawn_agent({cwd, task, label?, window?}): start a fresh agent in a directory\n",
    "  with that task as its first prompt.\n",
    "- corral_list_agents(): see which sessions exist and which you may reach.\n",
    "- corral_stop_agent({target_session}): stop an agent you no longer need.\n",
    "A message you receive is tagged with its sender's directory and session id; reply by\n",
    "calling corral_message_agent(target_session = that id). Delivery is fire-and-forget: a\n",
    "turn that ends without a corral_message_agent call tells the sender nothing.\n",
    "\n",
    "Before starting work, confirm the task (task-confirmation handshake): your FIRST turn\n",
    "must message the agent that spawned you (using its session reply handle) with (1) the\n",
    "task in your own words and (2) your clarification questions. Ask generously; assume the\n",
    "task is underspecified. Then end your turn and wait for a go-ahead before working.\n",
    "\n",
    "Keep routine progress lateral or downward; reach up to your spawner for the handshake,\n",
    "blockers you cannot resolve, decisions only it can make, and final results. If you are\n",
    "genuinely unsure, first try to resolve it yourself (read code, use tools); if it is a\n",
    "judgment only someone above can make, escalate the question up to your spawner rather\n",
    "than guessing. You cannot reach the human directly; uncertainty flows up the chain.\n",
    "\n",
    "Event-driven: you run only when a message arrives. After you act, end your turn and go\n",
    "idle; you are re-woken when another agent messages you. Do not poll or busy-wait.",
);

/// An operator decision on a pending approval, produced by the tray or the
/// desktop notification and applied to the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// A submission awaiting an operator decision. The target directory the approval
/// actually grants (the whitelist is keyed on dir pairs, and every operator-
/// facing label shows it) rides on the submission itself, authenticated at the
/// control-socket boundary.
pub struct Pending {
    pub sub: Submission,
}

pub struct Router {
    whitelist: PathBuf,
    /// Allow-once decisions for this daemon run (by message id).
    approved: HashSet<String>,
    /// Submissions accepted over the control socket, awaiting routing.
    queue: VecDeque<Submission>,
    /// Messages parked awaiting an operator decision. A list, not one slot, so
    /// an unapproved message never blocks the queue behind it: each pending
    /// item is resolved independently by id, and an authorized message still
    /// delivers while others wait for approval (the head-of-line-blocking fix).
    pending: Vec<Pending>,
    /// How a `Stop` kills its target's process (real: `kill_pid`).
    kill: Kill,
}

impl Router {
    pub fn new(whitelist: PathBuf) -> Self {
        Self::with_kill(whitelist, Box::new(placement::kill_pid))
    }

    /// Construct with an injected kill (tests record pids instead of killing).
    pub fn with_kill(whitelist: PathBuf, kill: Kill) -> Self {
        Self {
            whitelist,
            approved: HashSet::new(),
            queue: VecDeque::new(),
            pending: Vec::new(),
            kill,
        }
    }

    /// Accept a submission from the control socket for routing on the next poll.
    pub fn enqueue(&mut self, sub: Submission) {
        self.queue.push_back(sub);
    }

    /// The first message awaiting an operator decision, if any (the tray shows
    /// one at a time). Every pending message is surfaced via `pending_messages`.
    pub fn pending(&self) -> Option<&Pending> {
        self.pending.first()
    }

    /// Every message awaiting an operator decision, so the loop can surface each
    /// one (a notification per pending message) and resolve any by id — no
    /// approval hides behind another.
    pub fn pending_messages(&self) -> impl Iterator<Item = &Pending> {
        self.pending.iter()
    }

    /// A specific pending message by id, for surfacing details / auditing a
    /// decision that may target any pending item, not just the first.
    pub fn pending_by_id(&self, id: &str) -> Option<&Pending> {
        self.pending.iter().find(|p| p.sub.id == id)
    }

    /// Route the queue: deliver every whitelisted or already-approved message,
    /// and park the rest for approval WITHOUT blocking the queue behind them —
    /// an authorized message still delivers this tick even while others await a
    /// decision (the head-of-line-blocking fix). Returns a status line when it
    /// acted. `entries` is a fresh registry scan (the daemon's view of who is
    /// live and dormant).
    pub fn poll(&mut self, entries: &[RegistryEntry], launcher: &dyn Launcher) -> Option<String> {
        // Release any parked message whose pair was meanwhile whitelisted (the
        // headless approval path: with no tray/notification/GUI an operator
        // edits ~/.corral/whitelist and the daemon picks it up next tick). The
        // rest stay parked, each still independently approvable by id.
        let mut i = 0;
        while i < self.pending.len() {
            if is_whitelisted(
                &self.whitelist,
                &self.pending[i].sub.from_cwd,
                &self.pending[i].sub.target_cwd,
            ) {
                let p = self.pending.remove(i);
                self.approved.insert(p.sub.id.clone());
                self.queue.push_back(p.sub);
            } else {
                i += 1;
            }
        }
        let mut statuses = Vec::new();
        while let Some(sub) = self.queue.pop_front() {
            // The gate, over the two authenticated fields the boundary stamped.
            // The whitelist is re-read every pass (an "allow always" or an
            // out-of-band file edit takes effect), but neither directory is
            // re-derived here — there is exactly one place that resolves them.
            let ok = self.approved.contains(&sub.id)
                || is_whitelisted(&self.whitelist, &sub.from_cwd, &sub.target_cwd);
            if !ok {
                // Park for approval, then keep draining: an authorized item
                // behind this one must not wait on it.
                if !self.pending.iter().any(|p| p.sub.id == sub.id) {
                    self.pending.push(Pending { sub });
                }
                continue;
            }
            statuses.push(deliver(&sub, entries, launcher, self.kill.as_ref()));
        }
        (!statuses.is_empty()).then(|| statuses.join("; "))
    }

    /// Apply an operator decision to the pending message named by `id`. A stale
    /// id (already resolved, or superseded) is a harmless no-op, so a late click
    /// on an old notification never disturbs another message.
    pub fn apply(&mut self, id: &str, action: ApprovalAction) -> std::io::Result<()> {
        let Some(pos) = self.pending.iter().position(|p| p.sub.id == id) else {
            return Ok(());
        };
        match action {
            ApprovalAction::AllowOnce => {
                let p = self.pending.remove(pos);
                self.approved.insert(p.sub.id.clone());
                self.queue.push_back(p.sub);
            }
            ApprovalAction::AllowAlways => {
                // Queue first, persist second: a whitelist write that fails
                // (unrepresentable path, IO error) must still deliver the
                // submission the operator just allowed, not swallow it.
                let p = self.pending.remove(pos);
                self.approved.insert(p.sub.id.clone());
                let (from, target) = (p.sub.from_cwd.clone(), p.sub.target_cwd.clone());
                self.queue.push_back(p.sub);
                whitelist_add(&self.whitelist, &from, &target)?;
            }
            ApprovalAction::Deny => {
                self.pending.remove(pos);
            }
        }
        Ok(())
    }
}

/// Perform one authorized submission, one arm per verb, returning a status line.
fn deliver(
    sub: &Submission,
    entries: &[RegistryEntry],
    launcher: &dyn Launcher,
    kill: &dyn Fn(u32) -> Result<(), String>,
) -> String {
    match &sub.kind {
        Kind::Stop { session } => deliver_stop(sub, session, entries, kill),
        Kind::Message { session, .. } => deliver_session(sub, session, entries, launcher),
        // The canonical `target_cwd` is the dir started in, never the raw `cwd`
        // the sender wrote: the operator approved that path and the whitelist is
        // keyed on it, so spawning anywhere else would act outside the grant.
        Kind::Spawn { label, hidden, .. } => spawn(
            sub,
            &sub.target_cwd,
            label.as_deref(),
            *hidden,
            entries,
            launcher,
        ),
    }
}

/// Kill the target session's process by the pid parsed from its socket
/// filename, leaving a dormant, resumable record (the adapter or corral's
/// dead-socket sweep then clears the socket). Never spawns or resumes: a
/// target gone by routing time is a no-op, since the sender was already acked
/// `accepted`/`already_stopped`.
fn deliver_stop(
    sub: &Submission,
    sid: &str,
    entries: &[RegistryEntry],
    kill: &dyn Fn(u32) -> Result<(), String>,
) -> String {
    // Fails closed when two directories claim the id: a kill must never land on
    // whichever record the scan returned first.
    let Some(entry) = discovery::unique_session(entries, sid) else {
        return format!("stop: session {sid} gone or ambiguous");
    };
    match discovery::live_socket(entry) {
        // Translate the agent-observed pid to a host pid (the NSpid bridge)
        // before killing; corrald runs on the host, so RealProc sees the whole
        // tree. No host pid -> not correlatable, so we cannot kill it.
        Some(sock) => match discovery::resolve_socket_host_pid(
            &discovery::RealProc,
            sock.pid,
            sock.pid_namespace,
        ) {
            Some(host) => match kill(host) {
                Ok(()) => format!("stopped {}", sub.target_label()),
                Err(e) => format!("stop kill: {e}"),
            },
            None => format!("stop: {} has no correlatable host pid", sub.target_label()),
        },
        None => format!("stop: {} already dormant", sub.target_label()),
    }
}

/// `corral_spawn_agent`: start a fresh agent in `dir` carrying the task as its
/// first prompt. Always a new agent — talking to one that already runs there is
/// `corral_message_agent`'s job (addressed by session id), so no flag chooses
/// between the two.
fn spawn(
    sub: &Submission,
    dir: &str,
    label: Option<&str>,
    hidden: bool,
    entries: &[RegistryEntry],
    launcher: &dyn Launcher,
) -> String {
    // The spawn command rides in a record; corral names no agent kind. A
    // caller-chosen `label` wins (resolved from any record of that kind, so it
    // works even where the kind never ran), else reuse any record for this dir.
    // A dir corral has never seen an agent in, with no label given, has no
    // known kind and cannot be spawned into. The record's launch mode (gui +
    // message flag) rides along so a GUI kind launches directly.
    let (command, mut mode) = match label {
        Some(label) => match spawn_command_for_label(entries, label) {
            Some(c) => c,
            None => return format!("route spawn: unknown label {label}"),
        },
        None => match spawn_command_for_dir(entries, dir) {
            Some(c) => c,
            None => return format!("route: no known agent kind for {dir} (never announced there)"),
        },
    };
    // Spawns default hidden so an uninvited window never pops up; a caller that
    // asked for `window: "visible"` gets one. Window placement only: the
    // whitelist alone authorized this submission (control.rs classify).
    mode.hidden = hidden;
    // A fresh spawn is a brand-new agent that does not yet know it is part of a
    // swarm: prepend the charter so it confirms the task and communicates only
    // through corral. A resume (deliver_session) already has its transcript, so
    // it gets no charter.
    let first_prompt = format!("{CHARTER}\n\n{}", sub.tagged());
    // Substitute `{cwd}` in the spawn template with the target dir (a fresh
    // spawn has no `{sessionId}`).
    let launch_command = approved_commands::denormalize(command, "", Some(dir));
    match launcher.launch(Path::new(dir), &launch_command, Some(&first_prompt), &mode) {
        Ok(()) => format!("routed to {} (spawned)", sub.target_label()),
        Err(e) => format!("route spawn: {e}"),
    }
}

/// A spawn command announced by any record whose cwd is `dir`, live or dormant,
/// with that record's launch mode (so a GUI agent is launched directly).
fn spawn_command_for_dir<'a>(
    entries: &'a [RegistryEntry],
    dir: &str,
) -> Option<(&'a [String], LaunchMode)> {
    entries
        .iter()
        .filter(|e| e.cwd.as_deref() == Some(dir))
        .find_map(|e| e.spawn_command.as_deref().map(|c| (c, e.launch_mode())))
}

/// A spawn command from any record whose `label` matches, in any directory, so
/// a caller-chosen kind can be started even in a dir that never hosted it,
/// with that record's launch mode.
fn spawn_command_for_label<'a>(
    entries: &'a [RegistryEntry],
    label: &str,
) -> Option<(&'a [String], LaunchMode)> {
    entries
        .iter()
        .filter(|e| e.label.as_deref() == Some(label))
        .find_map(|e| e.spawn_command.as_deref().map(|c| (c, e.launch_mode())))
}

/// Session target: deliver to that exact agent over its socket if live, else
/// resume it from its record with the message as its first prompt. A live
/// socket that fails to connect (crashed) falls back to resume.
fn deliver_session(
    sub: &Submission,
    session_id: &str,
    entries: &[RegistryEntry],
    launcher: &dyn Launcher,
) -> String {
    let Some(entry) = discovery::unique_session(entries, session_id) else {
        return format!("route: session {session_id} not found or ambiguous");
    };
    if let Some(sock) = &entry.socket {
        if prompt::send_prompt(sock, &sub.tagged()).is_ok() {
            return format!("routed to {}", sub.target_label());
        }
        // Socket present but dead: fall through and resume from the record.
    }
    match (entry.cwd.clone(), entry.resume_argv()) {
        (Some(cwd), Some(command)) => {
            // A resume inherits the record's own window placement (its
            // `hidden` flag rides in `launch_mode`): the session already has a
            // placement the operator chose, and a messager does not get to
            // change it.
            let mode = entry.launch_mode();
            match launcher.launch(Path::new(&cwd), &command, Some(&sub.tagged()), &mode) {
                Ok(()) => format!("routed to {} (resumed)", sub.target_label()),
                Err(e) => format!("route resume: {e}"),
            }
        }
        _ => format!("route: session {session_id} not resumable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mailbox;
    use std::cell::{Cell, RefCell};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    /// Records launch calls, classifying by the argv the record carried: a
    /// resume command contains `--session`, a fresh spawn does not.
    #[derive(Default)]
    struct StubLauncher {
        spawns: Cell<usize>,
        resumes: Cell<usize>,
        last_msg: RefCell<Option<String>>,
        last_command: RefCell<Option<Vec<String>>>,
        last_hidden: Cell<bool>,
    }
    impl Launcher for StubLauncher {
        fn launch(
            &self,
            _cwd: &Path,
            command: &[String],
            message: Option<&str>,
            mode: &LaunchMode,
        ) -> Result<(), String> {
            if command.iter().any(|a| a == "--session") {
                self.resumes.set(self.resumes.get() + 1);
            } else {
                self.spawns.set(self.spawns.get() + 1);
            }
            *self.last_msg.borrow_mut() = message.map(str::to_owned);
            *self.last_command.borrow_mut() = Some(command.to_vec());
            self.last_hidden.set(mode.hidden);
            Ok(())
        }
    }

    /// A no-op kill for `deliver` tests that never take the Stop branch.
    fn no_kill(_pid: u32) -> Result<(), String> {
        Ok(())
    }

    /// A live record whose socket filename carries `pid`, so `deliver_stop`
    /// parses that pid to kill.
    fn live_record(session_id: &str, cwd: &str, pid: u32) -> RegistryEntry {
        RegistryEntry {
            session_id: session_id.into(),
            cwd: Some(cwd.into()),
            title: None,
            socket: Some(PathBuf::from(format!("{cwd}/.corral/pi-{pid}.sock"))),
            pid: None,
            pid_namespace: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), "x".into()]),
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    /// A submission as the control-socket boundary hands it over: parsed, then
    /// stamped with the canonical `target_cwd` `mailbox::authorize` derived. The
    /// router only ever sees stamped submissions, so every test builds them this
    /// way.
    fn stamped(mut sub: Submission, target_cwd: &str) -> Submission {
        sub.target_cwd = target_cwd.into();
        sub
    }

    fn stop_msg(id: &str, from: &str, sid: &str, target_cwd: &str) -> Submission {
        stamped(
            mailbox::parse(&format!(
                r#"{{"op":"stop","id":"{id}","fromCwd":"{from}","targetSession":"{sid}"}}"#
            ))
            .unwrap(),
            target_cwd,
        )
    }

    fn msg_sub(id: &str, from: &str, sid: &str, target_cwd: &str) -> Submission {
        stamped(
            mailbox::parse(&format!(
                r#"{{"op":"message","id":"{id}","fromCwd":"{from}","targetSession":"{sid}","message":"hi"}}"#
            ))
            .unwrap(),
            target_cwd,
        )
    }

    fn spawn_sub(id: &str, from: &str, target: &str) -> Submission {
        stamped(
            mailbox::parse(&format!(
                r#"{{"op":"spawn","id":"{id}","fromCwd":"{from}","cwd":"{target}","task":"hi"}}"#
            ))
            .unwrap(),
            target,
        )
    }

    fn spawn_sub_label(id: &str, from: &str, target: &str, label: &str) -> Submission {
        stamped(
            mailbox::parse(&format!(
                r#"{{"op":"spawn","id":"{id}","fromCwd":"{from}","cwd":"{target}","task":"hi","label":"{label}"}}"#
            ))
            .unwrap(),
            target,
        )
    }

    /// A record whose `label` and single-word spawn command are `label`, in
    /// `cwd`. Lets a test assert which kind the router chose to spawn.
    fn labeled_record(cwd: &str, label: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: format!("rec-{label}"),
            cwd: Some(cwd.into()),
            title: None,
            socket: None,
            pid: None,
            pid_namespace: None,
            spawn_command: Some(vec![label.into()]),
            resume_command: None,
            label: Some(label.into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    /// A record for `cwd` carrying a spawn command but no live socket: the
    /// daemon learns a dir's agent kind from any record there.
    fn dir_record(cwd: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: format!("rec{cwd}"),
            cwd: Some(cwd.into()),
            title: None,
            socket: None,
            pid: None,
            pid_namespace: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: None,
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    fn dormant(session_id: &str, cwd: &str, resume: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: session_id.into(),
            cwd: Some(cwd.into()),
            title: None,
            socket: None,
            pid: None,
            pid_namespace: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), resume.into()]),
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    #[test]
    fn spawn_never_reuses_a_live_agent_in_that_dir() {
        // A spawn is unconditionally a fresh agent: talking to one that already
        // works there is corral_message_agent's job (by session id), so no live
        // record in the dir can absorb a spawn.
        let mut live = dir_record("/b");
        live.socket = Some(PathBuf::from("/b/.corral/pi-1.sock"));
        let launcher = StubLauncher::default();
        deliver(&spawn_sub("1", "/a", "/b"), &[live], &launcher, &no_kill);
        assert_eq!(launcher.spawns.get(), 1, "spawn is always a new agent");
    }

    #[test]
    fn resume_inherits_the_record_window_placement() {
        // A message to a dormant session resumes it as the operator last placed
        // it (the record's own hidden flag); the messager has no say.
        let mut rec = dormant("sid-7", "/b", "/s/sid-7.jsonl");
        rec.hidden = true;
        let sub = msg_sub("1", "/a", "sid-7", "/b");
        let launcher = StubLauncher::default();
        deliver(&sub, &[rec], &launcher, &no_kill);
        assert_eq!(launcher.resumes.get(), 1);
        assert!(launcher.last_hidden.get(), "a hidden session stays hidden");
    }

    #[test]
    fn dir_spawn_is_hidden_by_default() {
        // A spawn defaults to no window, so an uninvited agent never pops one up.
        let entries = [dir_record("/b")];
        let launcher = StubLauncher::default();
        deliver(&spawn_sub("1", "/a", "/b"), &entries, &launcher, &no_kill);
        assert_eq!(launcher.spawns.get(), 1);
        assert!(
            launcher.last_hidden.get(),
            "agent-initiated spawn must be hidden"
        );
    }

    #[test]
    fn visible_request_launches_unhidden() {
        // A spawn asking for a visible window (window:"visible" on the tool,
        // hidden:false on the wire) gets one; the whitelist alone authorized it.
        let msg = stamped(
            mailbox::parse(
                r#"{"op":"spawn","id":"1","fromCwd":"/a","cwd":"/b","task":"hi","hidden":false}"#,
            )
            .unwrap(),
            "/b",
        );
        let entries = [dir_record("/b")];
        let launcher = StubLauncher::default();
        deliver(&msg, &entries, &launcher, &no_kill);
        assert_eq!(launcher.spawns.get(), 1);
        assert!(
            !launcher.last_hidden.get(),
            "visible request must not be hidden"
        );
    }

    #[test]
    fn unauthorized_message_becomes_pending_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        r.enqueue(spawn_sub("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        assert!(r.poll(&[], &launcher).is_none());
        assert_eq!(r.pending().map(|p| p.sub.id.as_str()), Some("1"));
        assert_eq!(launcher.spawns.get(), 0, "no delivery before approval");
    }

    #[test]
    fn caller_label_chooses_the_spawned_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        // /b has only pi; opencode was seen in another dir. The caller's label
        // must win over the dir's own kind.
        let entries = [dir_record("/b"), labeled_record("/c", "opencode")];
        r.enqueue(spawn_sub_label("1", "/a", "/b", "opencode"));
        let launcher = StubLauncher::default();
        r.poll(&entries, &launcher);
        assert_eq!(launcher.spawns.get(), 1);
        let cmd = launcher.last_command.borrow();
        assert_eq!(cmd.as_deref(), Some(["opencode".to_string()].as_slice()));
    }

    #[test]
    fn unknown_label_fails_loud_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        let entries = [dir_record("/b")];
        r.enqueue(spawn_sub_label("1", "/a", "/b", "ghost"));
        let launcher = StubLauncher::default();
        let status = r.poll(&entries, &launcher);
        assert_eq!(launcher.spawns.get(), 0);
        assert!(status.unwrap().contains("unknown label ghost"));
    }

    #[test]
    fn whitelisted_message_with_no_agent_spawns_with_message() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        r.enqueue(spawn_sub("1", "/a", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher);
        assert!(r.pending().is_none(), "whitelisted needs no decision");
        assert_eq!(launcher.spawns.get(), 1, "spawned an agent in the target");
        let first = launcher.last_msg.borrow();
        let first = first.as_deref().unwrap();
        assert!(
            first.ends_with("[from a]\nhi"),
            "the provenance-tagged message is the tail of the first prompt"
        );
        assert!(
            first.contains("task-confirmation handshake"),
            "a fresh spawn is prefixed with the swarm charter"
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
        r.enqueue(spawn_sub("1", "/a", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher); // -> pending (not yet whitelisted)
        assert_eq!(r.pending().map(|p| p.sub.id.as_str()), Some("1"));
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
        r.enqueue(spawn_sub("1", "/a", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dir_record("/b")];

        r.poll(&entries, &launcher); // -> pending
        r.apply("1", ApprovalAction::AllowAlways).unwrap();
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
        r.enqueue(spawn_sub("1", "/a", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&[], &launcher); // -> pending
        r.apply("1", ApprovalAction::Deny).unwrap();
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
        r.enqueue(msg_sub("1", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();
        let entries = [dormant("sid-7", "/b", "/s/sid-7.jsonl")];

        r.poll(&entries, &launcher);
        assert!(r.pending().is_none(), "whitelisted: no operator prompt");
        assert_eq!(launcher.resumes.get(), 1, "dormant session is resumed");
        assert_eq!(launcher.last_msg.borrow().as_deref(), Some("[from a]\nhi"));
    }

    #[test]
    fn a_squatted_session_id_delivers_to_nobody() {
        // Two records claim one id (a peer squatting a victim's session id). The
        // authorized message must reach neither, and a stop must kill nothing.
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let (mut r, killed) = recording_router(whitelist);
        let entries = [
            live_record("sid-7", "/b", 4242),
            live_record("sid-7", "/evil", 5555),
        ];
        r.enqueue(msg_sub("1", "/a", "sid-7", "/b"));
        r.enqueue(stop_msg("2", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();

        let status = r.poll(&entries, &launcher).unwrap();
        assert!(status.contains("ambiguous"), "status was: {status}");
        assert!(
            killed.lock().unwrap().is_empty(),
            "no kill on an ambiguous id"
        );
        assert_eq!(launcher.spawns.get(), 0);
        assert_eq!(launcher.resumes.get(), 0, "no resume on an ambiguous id");
    }

    #[test]
    fn a_session_gone_by_routing_time_is_dropped() {
        // The boundary rejects an unknown session outright (recipient_not_found),
        // so the only way the router sees one is a session that disappeared
        // between accept and routing. Authorized, but nothing to deliver to.
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        r.enqueue(msg_sub("1", "/a", "ghost", "/b"));
        let launcher = StubLauncher::default();

        let status = r.poll(&[], &launcher);
        assert!(status.unwrap().contains("session ghost not found"));
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
            pid: None,
            pid_namespace: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec![
                "pi".into(),
                "--session".into(),
                "/s/sid-7.jsonl".into(),
            ]),
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }];
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let mut r = Router::new(whitelist);
        r.enqueue(msg_sub("1", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher);
        handle.join().unwrap();
        assert_eq!(launcher.spawns.get(), 0, "live session needs no spawn");
        assert_eq!(launcher.resumes.get(), 0, "live socket needs no resume");
    }

    /// A Router whose kill records pids into a shared `Vec` (the kill closure
    /// must be `Send`, so `Arc<Mutex<_>>`, not `Rc<RefCell<_>>`).
    fn recording_router(whitelist: PathBuf) -> (Router, Arc<Mutex<Vec<u32>>>) {
        let killed = Arc::new(Mutex::new(Vec::new()));
        let sink = killed.clone();
        let r = Router::with_kill(
            whitelist,
            Box::new(move |pid| {
                sink.lock().unwrap().push(pid);
                Ok(())
            }),
        );
        (r, killed)
    }

    #[test]
    fn stop_kills_the_live_target_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let (mut r, killed) = recording_router(whitelist);
        let entries = [live_record("sid-7", "/b", 4242)];
        r.enqueue(stop_msg("1", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher);
        assert!(r.pending().is_none(), "whitelisted: no operator prompt");
        assert_eq!(*killed.lock().unwrap(), vec![4242], "killed the socket pid");
        assert_eq!(launcher.spawns.get(), 0, "a stop never spawns");
        assert_eq!(launcher.resumes.get(), 0, "a stop never resumes");
    }

    #[test]
    fn stop_dormant_target_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap();
        let (mut r, killed) = recording_router(whitelist);
        // Dormant record (no socket): nothing to kill.
        let entries = [dormant("sid-7", "/b", "/s/sid-7.jsonl")];
        r.enqueue(stop_msg("1", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();

        let status = r.poll(&entries, &launcher);
        assert!(killed.lock().unwrap().is_empty(), "nothing to kill");
        assert!(status.unwrap().contains("already dormant"));
    }

    #[test]
    fn stop_honors_the_approval_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        // Not whitelisted: the stop must go pending, killing nothing until allowed.
        let (mut r, killed) = recording_router(whitelist);
        let entries = [live_record("sid-7", "/b", 4242)];
        r.enqueue(stop_msg("1", "/a", "sid-7", "/b"));
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher);
        assert_eq!(r.pending().map(|p| p.sub.id.as_str()), Some("1"));
        assert!(killed.lock().unwrap().is_empty(), "no kill before approval");
        r.apply("1", ApprovalAction::AllowOnce).unwrap();
        r.poll(&entries, &launcher);
        assert_eq!(*killed.lock().unwrap(), vec![4242], "allowed -> killed");
    }

    #[test]
    fn authorized_message_is_not_blocked_by_a_pending_one() {
        // The head-of-line-blocking regression: an unapproved message A must not
        // stall an authorized message B behind it. B delivers this same poll.
        let tmp = tempfile::tempdir().unwrap();
        let whitelist = tmp.path().join("whitelist");
        mailbox::whitelist_add(&whitelist, "/a", "/b").unwrap(); // B's pair only
        let mut r = Router::new(whitelist);
        let entries = [dir_record("/unlisted"), dir_record("/b")];
        r.enqueue(spawn_sub("A", "/a", "/unlisted")); // needs approval
        r.enqueue(spawn_sub("B", "/a", "/b")); // whitelisted
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher);
        assert_eq!(launcher.spawns.get(), 1, "B delivered despite A pending");
        assert_eq!(
            r.pending().map(|p| p.sub.id.as_str()),
            Some("A"),
            "A still parked for approval"
        );
    }

    #[test]
    fn multiple_unauthorized_messages_all_become_pending() {
        // Two unapproved messages must both be parked (each surfaced for its own
        // approval), not one hidden in the queue behind the other.
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        let entries = [dir_record("/b"), dir_record("/c")];
        r.enqueue(spawn_sub("A", "/a", "/b"));
        r.enqueue(spawn_sub("B", "/a", "/c"));
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher);
        let ids: Vec<&str> = r.pending_messages().map(|p| p.sub.id.as_str()).collect();
        assert_eq!(ids, vec!["A", "B"], "both parked for approval");
        assert_eq!(launcher.spawns.get(), 0);
    }

    #[test]
    fn apply_by_id_resolves_the_named_message_only() {
        // Approving the second pending message delivers it; the first stays
        // parked (out-of-order approval works, no head-of-line dependency).
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        let entries = [dir_record("/b"), dir_record("/c")];
        r.enqueue(spawn_sub("A", "/a", "/b"));
        r.enqueue(spawn_sub("B", "/a", "/c"));
        let launcher = StubLauncher::default();

        r.poll(&entries, &launcher); // A, B both pending
        r.apply("B", ApprovalAction::AllowOnce).unwrap();
        r.poll(&entries, &launcher); // B delivers
        assert_eq!(launcher.spawns.get(), 1);
        assert_eq!(
            r.pending().map(|p| p.sub.id.as_str()),
            Some("A"),
            "A remains parked"
        );
    }

    #[test]
    fn apply_with_a_stale_id_is_a_noop() {
        // A late click on a superseded notification must not panic or disturb a
        // live pending message.
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Router::new(tmp.path().join("whitelist"));
        let entries = [dir_record("/b")];
        r.enqueue(spawn_sub("A", "/a", "/b"));
        let launcher = StubLauncher::default();
        r.poll(&entries, &launcher);
        r.apply("ghost", ApprovalAction::AllowOnce).unwrap();
        assert_eq!(
            r.pending().map(|p| p.sub.id.as_str()),
            Some("A"),
            "A untouched"
        );
    }
}
