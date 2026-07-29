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

    /// Full human label for the target, built from the target's **resolved**
    /// working directory: authorization is keyed on the `(sender dir -> target
    /// dir)` pair, so the operator must always see that directory. A session
    /// target names the dir *and* its session id — the id alone would hide
    /// where that agent works, which is the thing being approved. Used in the
    /// detail popup, the audit trail, and the router's status lines.
    pub fn target_label(&self, target_cwd: &str) -> String {
        match &self.target() {
            Target::Dir(_) => target_cwd.to_string(),
            Target::Session(s) => format!("{target_cwd} (session {s})"),
        }
    }

    /// Compact target label for the tray menu and the notification: the target
    /// directory's basename, so the `from → to` line stays short and symmetric
    /// with the basenamed sender; a session target keeps its full id after it.
    pub fn target_label_short(&self, target_cwd: &str) -> String {
        let dir = basename(target_cwd);
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

/// Classify a parsed message from resolved facts (pure, trivially tested).
/// `target_cwd` is `Some` when the recipient is found (a known session's cwd,
/// or an existing target directory), else `None`. `whitelisted` is consulted
/// only when the recipient is found.
///
/// The whitelist is the single authorization axis: a whitelisted pair goes
/// through, anything else asks the operator. Message, stop, hidden spawn and
/// visible spawn all authorize identically — the operator grants trust per
/// directory pair, and that grant covers every action the pair can take.
pub fn classify(target: &Target, target_cwd: Option<&str>, whitelisted: bool) -> Ack {
    match target_cwd {
        None => match target {
            Target::Session(_) => Ack::RecipientNotFound,
            Target::Dir(_) => Ack::DirectoryNotKnown,
        },
        Some(_) if whitelisted => Ack::Accepted,
        Some(_) => Ack::ApprovalNeeded,
    }
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

/// Whether this `(sender, target)` directory pair is pre-authorized. The
/// whitelist file has one `<from> -> <target>` pair per line; a missing file
/// authorizes nothing.
pub fn is_whitelisted(file: &Path, from: &str, target: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(file) else {
        return false;
    };
    text.lines().any(|l| match l.split_once(SEP) {
        Some((f, t)) => f.trim() == from && t.trim() == target,
        None => false,
    })
}

/// Append a `(from -> target)` pair to the whitelist, creating the file. Used
/// by the operator's "allow always" choice.
pub fn whitelist_add(file: &Path, from: &str, target: &str) -> std::io::Result<()> {
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
        assert_eq!(m.target_label("/work/proj"), "/work/proj (session sid-7)");
        assert_eq!(m.target_label_short("/work/proj"), "proj (session sid-7)");
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

    #[test]
    fn classify_covers_every_ack() {
        let sess = Target::Session("sid".into());
        let dir = Target::Dir("/b".into());
        // Recipient found -> the whitelist alone decides accepted vs approval.
        assert_eq!(classify(&sess, Some("/b"), true), Ack::Accepted);
        assert_eq!(classify(&sess, Some("/b"), false), Ack::ApprovalNeeded);
        assert_eq!(classify(&dir, Some("/b"), true), Ack::Accepted);
        assert_eq!(classify(&dir, Some("/b"), false), Ack::ApprovalNeeded);
        // Recipient not found -> reason depends on the target kind.
        assert_eq!(classify(&sess, None, false), Ack::RecipientNotFound);
        assert_eq!(classify(&dir, None, false), Ack::DirectoryNotKnown);
        // Only resolvable targets are routed onward.
        assert!(Ack::Accepted.routable());
        assert!(Ack::ApprovalNeeded.routable());
        assert!(!Ack::RecipientNotFound.routable());
        assert!(!Ack::DirectoryNotKnown.routable());
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
