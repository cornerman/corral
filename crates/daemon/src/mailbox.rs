//! Agent-initiated cross-session submissions. Three tools submit over the
//! control socket (`corrald.sock`) — `corral_message_agent`,
//! `corral_spawn_agent`, `corral_stop_agent` — and corrald is the trusted router
//! that authorizes, resolves the target, and performs the action. Parsing,
//! classification, and authorization are pure and unit-tested; the IO wrappers
//! are thin.
//!
//! One verb per tool, one `Kind` variant per verb: a submission can only carry
//! the fields its verb uses, so no field's meaning depends on another (make
//! illegal states unrepresentable). That is why no tool parameter is
//! conditionally meaningful.

use std::io::Write;
use std::path::Path;

use corral_core::curation;
use corral_core::discovery::RegistryEntry;

/// What a submission is authorized against: the directory pair is the
/// authorization unit, so every verb resolves to one of these. A spawn targets
/// a directory (nothing runs there yet); a message or a stop targets an exact
/// session, which resolves through the registry to its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Dir(String),
    Session(String),
}

/// The verb of a submission, one variant per agent-facing tool, each carrying
/// exactly the fields that verb uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `corral_message_agent`: deliver text to that exact session — injected
    /// over its socket when live, else resumed with the text as its first
    /// prompt (window placement inherited from the record, not chosen here).
    Message { session: String, text: String },
    /// `corral_spawn_agent`: start a **fresh** agent in `dir` carrying `task` as
    /// its first prompt. `label` picks the harness kind (`None` = the kind that
    /// dir already used). `hidden` is window placement only and plays no part in
    /// authorization, which keys on the `(sender dir -> target dir)` whitelist
    /// alone (see `classify`).
    Spawn {
        dir: String,
        task: String,
        label: Option<String>,
        hidden: bool,
    },
    /// `corral_stop_agent`: kill that session's process, leaving a dormant,
    /// resumable record. No body, no launch.
    Stop { session: String },
}

/// One queued cross-session submission: who sent it, and which verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub id: String,
    pub from_cwd: String,
    /// The target's canonical working directory: the single key every
    /// authorization, whitelist line, and operator label uses. Empty as parsed;
    /// `control.rs` stamps it from [`authenticate`] at the boundary, beside the
    /// authenticated `from_cwd`, and nothing downstream re-derives it. So a
    /// queued submission carries a target the daemon already proved is a real
    /// directory (parse, don't validate), and the router has no second
    /// resolution path that could disagree with the ack's (SECURITY.md T20).
    pub target_cwd: String,
    /// The sender's session id, so the receiver can reply to this exact agent.
    pub from_session: Option<String>,
    pub kind: Kind,
}

impl Submission {
    /// The directory-or-session this submission is addressed to; the single
    /// authorization axis reads it (a session resolves to its cwd).
    pub fn target(&self) -> Target {
        match &self.kind {
            Kind::Message { session, .. } | Kind::Stop { session } => {
                Target::Session(session.clone())
            }
            Kind::Spawn { dir, .. } => Target::Dir(dir.clone()),
        }
    }

    /// The text delivered to the target: a message's body or a spawn's task; a
    /// stop delivers nothing.
    pub fn body(&self) -> &str {
        match &self.kind {
            Kind::Message { text, .. } => text,
            Kind::Spawn { task, .. } => task,
            Kind::Stop { .. } => "",
        }
    }

    /// The delivered text: a provenance tag on its **own first line**, then the
    /// body verbatim to the end (security design T7). corrald builds the string,
    /// so nothing attacker-controlled can precede the first-line tag; the
    /// positional rule (stated in the charter and CONVENTION) is that only the
    /// first line is an authentic sender tag, and any `[from …]` inside the body
    /// is data. The sender directory shows as its basename (a reply uses the
    /// session id, not the cwd); when the sender's session is known it rides in
    /// full as the reply handle for `corral_message_agent(target_session = ..)`.
    pub fn tagged(&self) -> String {
        let from = basename(&self.from_cwd);
        let tag = match &self.from_session {
            Some(sid) => format!("[from {from} (session {sid})]"),
            None => format!("[from {from}]"),
        };
        format!("{tag}\n{}", self.body())
    }

    /// Full human label for the target, built from the **authenticated**
    /// `target_cwd`: authorization is keyed on the `(sender dir -> target dir)`
    /// pair, so the operator must always see that directory, and always the
    /// canonical one — a raw spawn `cwd` could name the same directory through
    /// `..` or a symlink whose basename says something else entirely. A session
    /// target names the dir *and* its session id — the id alone would hide
    /// where that agent works, which is the thing being approved. Used in the
    /// detail popup, the audit trail, and the router's status lines.
    pub fn target_label(&self) -> String {
        match &self.target() {
            Target::Dir(_) => self.target_cwd.clone(),
            Target::Session(s) => format!("{} (session {s})", self.target_cwd),
        }
    }

    /// Compact target label for the tray menu and the notification: the target
    /// directory's basename, so the `from → to` line stays short and symmetric
    /// with the basenamed sender; a session target keeps its full id after it.
    pub fn target_label_short(&self) -> String {
        let dir = basename(&self.target_cwd);
        match &self.target() {
            Target::Dir(_) => dir.to_string(),
            Target::Session(s) => format!("{dir} (session {s})"),
        }
    }
}

/// Last path component (ignoring a trailing slash); the whole string if there
/// is no slash.
pub fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

/// The synchronous verdict corral returns to a message submitter over the
/// control socket. It answers only what is knowable at once from the registry
/// and whitelist; actual delivery (and the operator approval gate) happens
/// afterward in the router. `Malformed` is handled before classification (a
/// parse failure), so it is not a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Target resolves and the `(sender -> target)` pair is whitelisted: the
    /// router will deliver it.
    Accepted,
    /// Target resolves but the pair is not whitelisted: held for the operator's
    /// approval. The sender is told now, not made to wait on a human. (The
    /// whitelist has no explicit deny, so this is "not yet approved", not
    /// "blocked".)
    ApprovalNeeded,
    /// A `target_session` that is not in the registry: nowhere to send.
    RecipientNotFound,
    /// A spawn `cwd` that is not an existing directory: nowhere to spawn.
    DirectoryNotKnown,
    /// A `corral_stop_agent` target that is already dormant (or whose process
    /// is gone): stopping it is a no-op success, not an error. Synchronous and
    /// never routed — nothing is left to kill.
    AlreadyStopped,
}

impl Ack {
    /// The wire word sent back over the control socket.
    pub fn wire(self) -> &'static str {
        match self {
            Ack::Accepted => "accepted",
            Ack::ApprovalNeeded => "approval_needed",
            Ack::RecipientNotFound => "recipient_not_found",
            Ack::DirectoryNotKnown => "directory_not_known",
            Ack::AlreadyStopped => "already_stopped",
        }
    }

    /// Whether the router should route this message (only resolvable targets).
    pub fn routable(self) -> bool {
        matches!(self, Ack::Accepted | Ack::ApprovalNeeded)
    }
}

/// Whether a claimed `fromSession` provably names a session in a **different**
/// directory than the authenticated sender (SECURITY.md T2). Physical location
/// authenticates the directory, so a reply handle pinned by the registry to
/// another directory is a forgery: the sender is trying to make a recipient's
/// reply land somewhere it could not itself reach.
///
/// An id absent from the registry passes. Absence is not proof of forgery: an
/// adapter writes its record and may message before corrald's next curation
/// tick publishes it, so rejecting unknown ids would break a legitimate first
/// message. Only a record pinning the id to another cwd is evidence, and
/// same-directory siblings remain mutually forgeable (accepted in T2: the
/// directory is the unit of identity).
pub fn session_claims_other_dir(entries: &[RegistryEntry], sid: &str, from_cwd: &str) -> bool {
    entries
        .iter()
        .any(|e| e.session_id == sid && e.cwd.as_deref().is_some_and(|c| c != from_cwd))
}

/// The resolved facts the verdict table judges, so [`classify`] stays pure and
/// exhaustively testable while all IO (registry scan, whitelist read, directory
/// canonicalization) happens in [`authorize`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Facts<'a> {
    /// The target's canonical working directory, or `None` when the recipient
    /// does not resolve (unknown session, non-existent directory).
    pub target_cwd: Option<&'a str>,
    /// The `(sender -> target)` pair is in the whitelist: authorized.
    pub whitelisted: bool,
    /// The caller may learn precise facts about this target (its own directory,
    /// or a whitelisted pair). Weaker than `whitelisted`: it gates disclosure,
    /// not delivery.
    pub reachable: bool,
    /// The target session resolves but nothing is running (socket cleared).
    /// Only a `Stop` cares.
    pub dormant: bool,
}

/// Judge one submission from resolved facts: the single verdict table, shared by
/// the synchronous ack and every verb (there is no second classification site).
///
/// The whitelist is the single authorization axis: a whitelisted pair goes
/// through, anything else asks the operator. Message, stop, hidden spawn and
/// visible spawn all authorize identically — the operator grants trust per
/// directory pair, and that grant covers every action the pair can take.
///
/// Disclosure is a second, weaker axis. Whether an arbitrary host path is a
/// directory is a fact about the filesystem *outside* the caller's sandbox, so
/// `DirectoryNotKnown` is told only to a caller that may reach that directory;
/// an unreachable pair always hears `ApprovalNeeded`, existing or not, and so
/// cannot use the ack as an existence oracle (SECURITY.md T19). Session facts
/// (existence, liveness) need no such gate: `list_corral_agents` publishes every
/// session id and its liveness to every caller by design, so reporting
/// `RecipientNotFound` / `AlreadyStopped` precisely leaks nothing new and keeps
/// a stale reply handle diagnosable.
pub fn classify(kind: &Kind, f: &Facts) -> Ack {
    match (kind, f.target_cwd) {
        (Kind::Spawn { .. }, None) if !f.reachable => Ack::ApprovalNeeded,
        (Kind::Spawn { .. }, None) => Ack::DirectoryNotKnown,
        (Kind::Message { .. } | Kind::Stop { .. }, None) => Ack::RecipientNotFound,
        // Dormant: nothing to kill, so the stop already succeeded. A message to a
        // dormant session still routes (it resumes the session).
        (Kind::Stop { .. }, Some(_)) if f.dormant => Ack::AlreadyStopped,
        (_, Some(_)) if f.whitelisted => Ack::Accepted,
        (_, Some(_)) => Ack::ApprovalNeeded,
    }
}

/// Authenticate a target into the canonical directory authorization keys on. A
/// spawn's `cwd` is canonicalized race-safely from a directory fd
/// ([`curation::canonical_dir`], the same authentication the sender's own cwd
/// gets), which both proves it is a directory and collapses `..`, trailing
/// slashes, and symlinks — so a whitelist line is a relation over real
/// directories and the operator's approval popup cannot be shown a path whose
/// basename lies about where the agent starts. A session target resolves
/// through the vetted registry, whose `cwd` is already canonical (stamped from
/// the record's physical location).
pub fn authenticate(target: &Target, entries: &[RegistryEntry]) -> Option<String> {
    match target {
        Target::Dir(d) => curation::canonical_dir(d),
        Target::Session(sid) => entries
            .iter()
            .find(|e| &e.session_id == sid)
            .and_then(|e| e.cwd.clone()),
    }
}

/// Whether the caller may learn precise facts about `target_cwd`: its own
/// directory, or a whitelisted pair. The roster's redaction predicate and the
/// ack's disclosure gate are this one function, so the two cannot drift.
pub fn reachable(whitelist: &Path, from_cwd: &str, target_cwd: &str) -> bool {
    target_cwd == from_cwd || is_whitelisted(whitelist, from_cwd, target_cwd)
}

/// The one authorization step, run at the boundary: authenticate the target,
/// read the whitelist, judge. Returns the canonical target directory (to stamp
/// onto the submission) and the verdict to ack. Every verb goes through this
/// same call, so there is no second classification site.
///
/// Downstream, the router gates on `is_whitelisted(from_cwd, target_cwd)` over
/// the stamped fields alone — it re-reads the whitelist (so an out-of-band edit
/// or an "allow always" takes effect) but never re-derives either directory.
pub fn authorize(
    whitelist: &Path,
    sub: &Submission,
    entries: &[RegistryEntry],
) -> (Option<String>, Ack) {
    let target = sub.target();
    let target_cwd = authenticate(&target, entries);
    // Reachability falls back to the raw target string when the directory does
    // not resolve, so a pair the operator once approved still gets the precise
    // `DirectoryNotKnown` after that directory is deleted.
    let raw = match &target {
        Target::Dir(d) => Some(d.clone()),
        Target::Session(_) => None,
    };
    let probe = target_cwd.clone().or(raw);
    let facts = Facts {
        target_cwd: target_cwd.as_deref(),
        whitelisted: target_cwd
            .as_deref()
            .is_some_and(|t| is_whitelisted(whitelist, &sub.from_cwd, t)),
        reachable: probe.is_some_and(|t| reachable(whitelist, &sub.from_cwd, &t)),
        dormant: match &target {
            Target::Session(sid) => entries
                .iter()
                .find(|e| &e.session_id == sid)
                .is_some_and(|e| e.socket.is_none()),
            Target::Dir(_) => false,
        },
    };
    let ack = classify(&sub.kind, &facts);
    (target_cwd, ack)
}

/// Parse one submission JSON document. The `op` field names the verb
/// (`message` / `spawn` / `stop`, CONVENTION.md v3) and each verb requires its
/// own fields, so an unparseable combination cannot reach the router:
///
/// - `message`: `targetSession` + `message`
/// - `spawn`: `cwd` + `task`, optional `label`, `hidden` defaulting to true
///   (an uninvited agent never pops a window)
/// - `stop`: `targetSession`
///
/// `id` is always required. `fromCwd` is authenticated by corrald from the
/// outbox file's location and overwritten there, so the content field (if any)
/// is not trusted. Returns `None` on malformed JSON, an unknown `op`, or a
/// missing field.
pub fn parse(text: &str) -> Option<Submission> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let kind = match v.get("op").and_then(|o| o.as_str())? {
        "message" => Kind::Message {
            session: s("targetSession")?,
            text: s("message")?,
        },
        "spawn" => Kind::Spawn {
            dir: s("cwd")?,
            task: s("task")?,
            label: s("label"),
            hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(true),
        },
        "stop" => Kind::Stop {
            session: s("targetSession")?,
        },
        _ => return None,
    };
    Some(Submission {
        id: s("id")?,
        from_cwd: s("fromCwd").unwrap_or_default(),
        // Both directories of the authorized pair are stamped by corrald from
        // authenticated facts, never parsed from agent-supplied content.
        target_cwd: String::new(),
        from_session: s("fromSession"),
        kind,
    })
}

/// One line in the capability roster a `list` query returns. Every session is a
/// per-session entry the caller can address by `sessionId` (approval still
/// gates an unwhitelisted target). A reachable agent (its own directory or a
/// whitelisted pair) also exposes `title`, `description`, and `cwd`; an
/// unreachable one hides all three, so the caller gets an addressable handle
/// without learning who runs where. It never carries activity: messaging is
/// not reading the transcript, so the roster reveals enough to decide who to
/// message (which session, what task) and lets the caller message it, never
/// what any agent is doing right now. `title` rides the same gate as
/// `cwd`/`description`: the operator already trusts the pair enough to let a
/// message through, so showing the task name is strictly weaker than the
/// messaging already permitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The harness kind (the record `label`, or `agent` if unlabeled).
    pub kind: String,
    /// The session title (its current task). Set only on a reachable entry;
    /// hidden on an unreachable one. Activity stays hidden either way.
    pub title: Option<String>,
    /// Set only on a reachable entry (hidden on an unreachable one).
    pub description: Option<String>,
    /// Set only on a reachable entry (hidden on an unreachable one).
    pub cwd: Option<String>,
    pub session_id: String,
    pub live: bool,
}

/// Build the capability roster for a caller. `visible(target_cwd)` reports
/// whether the caller may reach that directory (its own dir, or a whitelisted
/// `(from -> target)` pair). Every session yields a per-session entry addressed
/// by `sessionId`; a reachable one also carries `title`, `description`, and
/// `cwd`, an unreachable one hides all three.
pub fn build_roster(entries: &[RegistryEntry], visible: impl Fn(&str) -> bool) -> Vec<RosterEntry> {
    entries
        .iter()
        .map(|e| {
            let reachable = e.cwd.as_deref().is_some_and(&visible);
            RosterEntry {
                kind: e.label.clone().unwrap_or_else(|| "agent".into()),
                title: reachable.then(|| e.title.clone()).flatten(),
                description: reachable.then(|| e.description.clone()).flatten(),
                cwd: reachable.then(|| e.cwd.clone()).flatten(),
                session_id: e.session_id.clone(),
                live: e.socket.is_some(),
            }
        })
        .collect()
}

/// Serialize a roster as the `list` reply line. Every entry carries `sessionId`
/// and `live`; a reachable entry adds `title`/`description`/`cwd`, an
/// unreachable one omits all three so nothing identifies where it runs or what
/// it works on.
pub fn roster_json(roster: &[RosterEntry]) -> String {
    let agents: Vec<serde_json::Value> = roster
        .iter()
        .map(|r| {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), r.kind.clone().into());
            if let Some(t) = &r.title {
                m.insert("title".into(), t.clone().into());
            }
            if let Some(d) = &r.description {
                m.insert("description".into(), d.clone().into());
            }
            if let Some(c) = &r.cwd {
                m.insert("cwd".into(), c.clone().into());
            }
            m.insert("sessionId".into(), r.session_id.clone().into());
            m.insert("live".into(), r.live.into());
            serde_json::Value::Object(m)
        })
        .collect();
    serde_json::json!({ "status": "ok", "agents": agents }).to_string()
}

/// Parse a submission envelope (`{"submit":"<outbox path>"}`), returning the
/// path. Every control request rides one: the real request JSON lives in the
/// sender's `<cwd>/.corral/outbox/<id>.json`, and corrald derives the trusted
/// `fromCwd` from that file's physical location rather than trusting a
/// self-reported field (security design T2-send). `None` means the line is not
/// a submit envelope.
pub fn parse_submit(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("submit").and_then(|s| s.as_str()).map(String::from)
}

/// Parse a `list` roster query (`{"op":"list"}`), returning whether the content
/// is a list request. The caller supplies the authenticated `fromCwd` (derived
/// from the outbox location), so the content's own `fromCwd` is ignored.
pub fn is_list(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("op").and_then(|o| o.as_str()).map(|o| o == "list"))
        .unwrap_or(false)
}

/// The `(from -> target)` separator in the whitelist file.
const SEP: &str = " -> ";

/// Whether a path may appear in the whitelist. A directory name may legally
/// contain the separator, which would make the line ambiguous: `split_once`
/// takes the *first* occurrence, so `"/a -> /evil" -> "/b"` would parse as the
/// pair `(/a, /evil -> /b)` and record a grant for a directory the operator
/// never saw. The grammar has no escaping, so such a path is refused outright —
/// it can never be whitelisted (fail-closed: the operator can still allow a
/// single submission).
fn representable(path: &str) -> bool {
    !path.contains(SEP)
}

/// Whether this `(sender, target)` directory pair is pre-authorized. The
/// whitelist file has one `<from> -> <target>` pair per line; a missing file
/// authorizes nothing. Both paths must be representable in the grammar, so a
/// path that could straddle the separator never matches (see [`representable`]).
pub fn is_whitelisted(file: &Path, from: &str, target: &str) -> bool {
    if !representable(from) || !representable(target) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    text.lines().any(|l| match l.split_once(SEP) {
        Some((f, t)) => f.trim() == from && t.trim() == target,
        None => false,
    })
}

/// Append a `(from -> target)` pair to the whitelist, creating the file. Used
/// by the operator's "allow always" choice. Refuses a pair the grammar cannot
/// represent unambiguously, rather than writing a line that would parse back as
/// a different pair.
pub fn whitelist_add(file: &Path, from: &str, target: &str) -> std::io::Result<()> {
    if !representable(from) || !representable(target) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path contains the whitelist separator {SEP:?}: {from} -> {target}"),
        ));
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    writeln!(f, "{from}{SEP}{target}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_sub() -> Submission {
        Submission {
            id: "1".into(),
            from_cwd: "/a".into(),
            target_cwd: String::new(),
            from_session: None,
            kind: Kind::Spawn {
                dir: "/b".into(),
                task: "hi".into(),
                label: None,
                hidden: true,
            },
        }
    }

    #[test]
    fn parses_a_spawn_and_tags_its_task() {
        let json = r#"{"op":"spawn","id":"1","fromCwd":"/a","cwd":"/b","task":"hi"}"#;
        let m = parse(json).unwrap();
        assert_eq!(m, spawn_sub());
        assert_eq!(m.tagged(), "[from a]\nhi");
        // A spawn targets the directory: nothing runs there to address yet.
        assert_eq!(m.target(), Target::Dir("/b".into()));
    }

    #[test]
    fn targets_a_session_with_reply_handle() {
        let json = r#"{"op":"message","id":"1","fromCwd":"/a","fromSession":"sid-9",
            "targetSession":"sid-7","message":"hi"}"#;
        let m = parse(json).unwrap();
        assert_eq!(m.target(), Target::Session("sid-7".into()));
        // The resolved target dir stays visible beside the session id, in both
        // the full and the compact form (it is the authorization axis).
        let m = Submission {
            target_cwd: "/work/proj".into(),
            ..m
        };
        assert_eq!(m.target_label(), "/work/proj (session sid-7)");
        assert_eq!(m.target_label_short(), "proj (session sid-7)");
        // The reply handle (sender's session) rides in the provenance tag; the
        // dir shows as its basename, the session id stays full.
        assert_eq!(m.tagged(), "[from a (session sid-9)]\nhi");
    }

    #[test]
    fn spawn_carries_label_and_window_placement() {
        let with = parse(
            r#"{"op":"spawn","id":"1","cwd":"/b","task":"hi","label":"opencode","hidden":false}"#,
        )
        .unwrap();
        assert_eq!(
            with.kind,
            Kind::Spawn {
                dir: "/b".into(),
                task: "hi".into(),
                label: Some("opencode".into()),
                hidden: false,
            }
        );
        // Omitted -> no kind chosen, and hidden (an uninvited window never pops).
        let without = parse(r#"{"op":"spawn","id":"1","cwd":"/b","task":"hi"}"#).unwrap();
        assert_eq!(without.kind, spawn_sub().kind);
    }

    #[test]
    fn each_verb_requires_its_own_fields() {
        // A message needs a session and a body; a spawn needs a dir and a task;
        // a stop needs a session. Nothing else parses, so no half-formed
        // submission reaches the router.
        assert_eq!(parse(r#"{"op":"message","id":"1","message":"hi"}"#), None);
        assert_eq!(
            parse(r#"{"op":"message","id":"1","targetSession":"s"}"#),
            None
        );
        assert_eq!(parse(r#"{"op":"spawn","id":"1","cwd":"/b"}"#), None);
        assert_eq!(parse(r#"{"op":"spawn","id":"1","task":"hi"}"#), None);
        assert_eq!(parse(r#"{"op":"stop","id":"1"}"#), None);
        // A missing id, an unknown verb, or no verb at all: rejected.
        assert_eq!(parse(r#"{"op":"stop","targetSession":"s"}"#), None);
        assert_eq!(parse(r#"{"op":"nuke","id":"1"}"#), None);
        assert_eq!(
            parse(r#"{"id":"1","targetSession":"s","message":"hi"}"#),
            None
        );
        assert_eq!(parse("nope"), None);
    }

    #[test]
    fn stop_targets_a_session_and_carries_no_body() {
        let m = parse(
            r#"{"op":"stop","id":"1","fromCwd":"/a","fromSession":"s9","targetSession":"sid-7"}"#,
        )
        .unwrap();
        assert_eq!(
            m.kind,
            Kind::Stop {
                session: "sid-7".into()
            }
        );
        assert_eq!(m.target(), Target::Session("sid-7".into()));
        assert_eq!(m.from_session.as_deref(), Some("s9"));
        assert!(m.body().is_empty(), "a stop carries no body");
    }

    #[test]
    fn already_stopped_is_a_non_routable_success() {
        assert_eq!(Ack::AlreadyStopped.wire(), "already_stopped");
        assert!(!Ack::AlreadyStopped.routable(), "nothing left to kill");
    }

    #[test]
    fn session_claims_other_dir_needs_a_record_pinning_it_elsewhere() {
        let entries = [
            rec("mine", "/a", "pi", true, None),
            rec("theirs", "/b", "pi", true, None),
        ];
        // Pinned to another dir -> provable forgery.
        assert!(session_claims_other_dir(&entries, "theirs", "/a"));
        // My own session in my own dir -> fine.
        assert!(!session_claims_other_dir(&entries, "mine", "/a"));
        // Absent from the registry -> not yet curated, so not evidence.
        assert!(!session_claims_other_dir(&entries, "unknown", "/a"));
    }

    /// Facts for a resolved target.
    fn found(whitelisted: bool) -> Facts<'static> {
        Facts {
            target_cwd: Some("/b"),
            whitelisted,
            reachable: whitelisted,
            dormant: false,
        }
    }

    #[test]
    fn classify_covers_every_ack() {
        let msg = Kind::Message {
            session: "sid".into(),
            text: "hi".into(),
        };
        let stop = Kind::Stop {
            session: "sid".into(),
        };
        let spawn = Kind::Spawn {
            dir: "/b".into(),
            task: "hi".into(),
            label: None,
            hidden: true,
        };
        // Recipient found -> the whitelist alone decides accepted vs approval,
        // identically for every verb (one authorization axis).
        for kind in [&msg, &stop, &spawn] {
            assert_eq!(classify(kind, &found(true)), Ack::Accepted);
            assert_eq!(classify(kind, &found(false)), Ack::ApprovalNeeded);
        }
        // An unknown session is reported precisely to anyone: the roster already
        // publishes every session id and its liveness.
        assert_eq!(classify(&msg, &Facts::default()), Ack::RecipientNotFound);
        assert_eq!(classify(&stop, &Facts::default()), Ack::RecipientNotFound);
        // A dormant session: stopping it already succeeded; a message still
        // routes (it resumes the session).
        let dorm = Facts {
            dormant: true,
            ..found(true)
        };
        assert_eq!(classify(&stop, &dorm), Ack::AlreadyStopped);
        assert_eq!(classify(&msg, &dorm), Ack::Accepted);
        // Directory existence is disclosed only to a reachable caller...
        let gone_reachable = Facts {
            reachable: true,
            ..Facts::default()
        };
        assert_eq!(classify(&spawn, &gone_reachable), Ack::DirectoryNotKnown);
        // ...an unreachable one hears the same thing whether or not it exists,
        // so the ack is no existence oracle for host paths (T19).
        assert_eq!(classify(&spawn, &Facts::default()), Ack::ApprovalNeeded);
        assert_eq!(classify(&spawn, &found(false)), Ack::ApprovalNeeded);
        // Only resolvable targets are routed onward.
        assert!(Ack::Accepted.routable());
        assert!(Ack::ApprovalNeeded.routable());
        assert!(!Ack::RecipientNotFound.routable());
        assert!(!Ack::DirectoryNotKnown.routable());
        assert!(!Ack::AlreadyStopped.routable());
    }

    #[test]
    fn whitelist_refuses_a_path_straddling_the_separator() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("whitelist");
        // A dir named with the separator inside cannot be granted: the line
        // would parse back as a different pair.
        assert!(whitelist_add(&file, "/a -> /evil", "/b").is_err());
        assert!(whitelist_add(&file, "/a", "/b -> /evil").is_err());
        assert!(!file.exists(), "nothing written");
        // Nor does a hand-written ambiguous line authorize either pair it could
        // be read as.
        std::fs::write(&file, "/a -> /evil -> /b\n").unwrap();
        assert!(!is_whitelisted(&file, "/a -> /evil", "/b"));
        assert!(!is_whitelisted(&file, "/a", "/evil -> /b"));
    }

    #[test]
    fn authenticate_canonicalizes_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let canon = std::fs::canonicalize(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // A trailing slash, a `..` hop, and a symlink all resolve to one key, so
        // the whitelist is a relation over real directories.
        for spelling in [
            format!("{}/", real.display()),
            format!("{}/../real", real.display()),
            link.display().to_string(),
        ] {
            assert_eq!(
                authenticate(&Target::Dir(spelling.clone()), &[]).as_deref(),
                Some(canon.to_string_lossy().as_ref()),
                "{spelling}"
            );
        }
        // A path that is not a directory does not resolve.
        assert_eq!(
            authenticate(
                &Target::Dir(tmp.path().join("nope").display().to_string()),
                &[]
            ),
            None
        );
    }

    /// A registry entry with just the fields the roster reads.
    fn rec(sid: &str, cwd: &str, label: &str, live: bool, desc: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            session_id: sid.into(),
            cwd: Some(cwd.into()),
            title: Some("secret title".into()),
            socket: live.then(|| std::path::PathBuf::from(format!("{cwd}/.corral/{label}-1.sock"))),
            pid: None,
            pid_namespace: None,
            spawn_command: None,
            resume_command: None,
            label: Some(label.into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: desc.map(String::from),
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    #[test]
    fn roster_exposes_every_session_and_hides_unreachable_fields() {
        let entries = [
            rec("s1", "/a", "pi", true, Some("terminal agent")),
            rec("s2", "/a", "pi", false, Some("terminal agent")),
            rec("s3", "/secret", "quine", true, Some("gui app")),
            rec("s4", "/other", "pi", true, Some("terminal agent")),
        ];
        // Caller sees only /a.
        let roster = build_roster(&entries, |cwd| cwd == "/a");
        // Every session is a per-session entry addressable by sessionId.
        assert_eq!(roster.len(), 4);
        assert!(roster.iter().all(|r| !r.session_id.is_empty()));
        // Reachable /a entries expose title + cwd + description; liveness preserved.
        let reachable: Vec<_> = roster.iter().filter(|r| r.cwd.is_some()).collect();
        assert_eq!(reachable.len(), 2);
        assert!(reachable
            .iter()
            .all(|r| r.title.as_deref() == Some("secret title")
                && r.cwd.as_deref() == Some("/a")
                && r.description.is_some()));
        assert_eq!(reachable[0].session_id, "s1");
        assert!(reachable[0].live && !reachable[1].live);
        // Unreachable /secret and /other still yield per-session entries, but
        // hide title, cwd, and description; live stays exposed.
        let unreachable: Vec<_> = roster.iter().filter(|r| r.cwd.is_none()).collect();
        assert_eq!(unreachable.len(), 2);
        assert!(unreachable
            .iter()
            .all(|r| r.title.is_none() && r.description.is_none() && !r.session_id.is_empty()));
        let sids: Vec<_> = unreachable.iter().map(|r| r.session_id.as_str()).collect();
        assert!(sids.contains(&"s3") && sids.contains(&"s4"));
    }

    #[test]
    fn roster_json_hides_title_cwd_and_description_for_unreachable() {
        let entries = [rec("s1", "/secret", "pi", true, Some("terminal agent"))];
        let json = roster_json(&build_roster(&entries, |_| false));
        assert!(!json.contains("secret title"), "never leak the title");
        assert!(!json.contains("/secret"), "never leak an unreachable cwd");
        assert!(
            !json.contains("terminal agent"),
            "hide unreachable description"
        );
        // The sessionId is the addressable handle, so it is exposed.
        assert!(json.contains("\"sessionId\":\"s1\""));
        assert!(json.contains("\"kind\":\"pi\"") && json.contains("\"live\":true"));
        assert!(!json.contains("canMessage"));
    }

    #[test]
    fn roster_json_exposes_title_for_reachable() {
        let entries = [rec("s1", "/a", "pi", true, Some("terminal agent"))];
        let json = roster_json(&build_roster(&entries, |cwd| cwd == "/a"));
        // A reachable session surfaces its title alongside cwd + description.
        assert!(
            json.contains("\"title\":\"secret title\""),
            "expose reachable title"
        );
        assert!(json.contains("\"cwd\":\"/a\""));
        assert!(json.contains("\"description\":\"terminal agent\""));
    }

    #[test]
    fn is_list_matches_only_the_list_op() {
        assert!(is_list(r#"{"op":"list"}"#));
        assert!(is_list(r#"{"op":"list","fromCwd":"/a"}"#));
        assert!(!is_list(r#"{"id":"1","message":"hi"}"#));
        assert!(!is_list("nope"));
        // parse_submit reads the envelope path.
        assert_eq!(
            parse_submit(r#"{"submit":"/w/.corral/outbox/m.json"}"#).as_deref(),
            Some("/w/.corral/outbox/m.json")
        );
        assert_eq!(parse_submit(r#"{"op":"list"}"#), None);
    }

    #[test]
    fn whitelist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("whitelist");
        assert!(!is_whitelisted(&file, "/a", "/b"));
        whitelist_add(&file, "/a", "/b").unwrap();
        assert!(is_whitelisted(&file, "/a", "/b"));
        // A different pair is still not authorized.
        assert!(!is_whitelisted(&file, "/a", "/c"));
    }
}
