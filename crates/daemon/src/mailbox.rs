//! Agent-initiated cross-session messages. The `corral_message_agent` tool
//! submits a message over the control socket (`corrald.sock`); corral is the
//! trusted router that authorizes, resolves the target, and injects the
//! message. Parsing, classification, and authorization are pure and
//! unit-tested; the IO wrappers are thin.

use std::io::Write;
use std::path::Path;

use corral_core::discovery::RegistryEntry;

/// Who a message is addressed to. A directory reaches whoever works there
/// (spawning one if none); a session reaches exactly that agent (resuming it if
/// dormant) and is what a precise reply uses, since a directory can hold zero,
/// one, or several sessions over time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Dir(String),
    Session(String),
}

/// What a routed item does to its target. A message is delivered (injected /
/// spawned / resumed); a stop kills the target's live process, leaving a
/// dormant, resumable record. Both share the queue, whitelist, and approval
/// gate — the machinery is action-agnostic, so only `deliver` branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    /// Deliver the message text to the target (the default routed item).
    #[default]
    Deliver,
    /// Kill the target session's process (the `corral_stop_agent` tool).
    Stop,
}

/// One queued cross-session message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub from_cwd: String,
    /// The sender's session id, so the receiver can reply to this exact agent.
    pub from_session: Option<String>,
    pub target: Target,
    pub message: String,
    pub force_new: bool,
    /// Agent kind to start if a `target_dir` message spawns a fresh agent
    /// (matched against a registry record's `label`). `None` = caller did not
    /// choose; the router falls back to the dir's own record.
    pub label: Option<String>,
    /// Whether a spawn/resume this message triggers runs hidden (no window).
    /// Defaults true, so an uninvited agent never pops a window; `false`
    /// requests a visible window and always passes the operator approval gate
    /// (a visible window is a stronger action than a message, so the whitelist
    /// alone never authorizes it — see `classify`).
    pub hidden: bool,
    /// Deliver the message, or stop (kill) the target. A `Stop` carries a
    /// `Session` target, an empty body, and never a charter or spawn.
    pub action: Action,
}

impl Message {
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
        format!("{tag}\n{}", self.message)
    }

    /// Full human label for the target (used in the detail popup).
    pub fn target_label(&self) -> String {
        match &self.target {
            Target::Dir(d) => d.clone(),
            Target::Session(s) => format!("session {s}"),
        }
    }

    /// Compact target label for the tray menu: a directory target shows only
    /// its basename, so the `from → to` line stays short and symmetric with the
    /// basenamed sender.
    pub fn target_label_short(&self) -> String {
        match &self.target {
            Target::Dir(d) => basename(d).to_string(),
            Target::Session(s) => format!("session {s}"),
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
    /// A `target_dir` that is not an existing directory: nowhere to spawn.
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

/// Classify a parsed message from resolved facts (pure, trivially tested).
/// `target_cwd` is `Some` when the recipient is found (a known session's cwd,
/// or an existing target directory), else `None`. `whitelisted` is consulted
/// only when the recipient is found.
///
/// `force_approval` names an explicit gate reason: the action is stronger than
/// a plain message (a **visible** spawn, `hidden:false`), so it always needs
/// the operator, whitelisted or not. It replaces the old overloading of the
/// `hidden` flag — a stop, which never spawns, simply passes `false`.
pub fn classify(
    target: &Target,
    target_cwd: Option<&str>,
    whitelisted: bool,
    force_approval: bool,
) -> Ack {
    match target_cwd {
        None => match target {
            Target::Session(_) => Ack::RecipientNotFound,
            Target::Dir(_) => Ack::DirectoryNotKnown,
        },
        // A stronger-than-message action is operator-gated regardless of the
        // whitelist.
        Some(_) if force_approval => Ack::ApprovalNeeded,
        Some(_) if whitelisted => Ack::Accepted,
        Some(_) => Ack::ApprovalNeeded,
    }
}

/// Parse one mailbox JSON document. Requires `id`, `fromCwd`, `message`, and a
/// target (`targetSession` wins over `targetDir`); `forceNew` defaults to
/// false. Returns `None` on malformed JSON or a missing field.
pub fn parse_message(text: &str) -> Option<Message> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let target = match s("targetSession") {
        Some(sid) => Target::Session(sid),
        None => Target::Dir(s("targetDir")?),
    };
    Some(Message {
        id: s("id")?,
        // `fromCwd` is authenticated by corrald from the outbox file location
        // and overwritten there; the content field (if any) is not trusted.
        from_cwd: s("fromCwd").unwrap_or_default(),
        from_session: s("fromSession"),
        target,
        message: s("message")?,
        force_new: v.get("forceNew").and_then(|x| x.as_bool()).unwrap_or(false),
        label: s("label"),
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(true),
        action: Action::Deliver,
    })
}

/// Parse a stop submission (`{"op":"stop","id":..,"fromCwd":..,"targetSession":..}`).
/// A stop always targets an exact session (killing whoever-works-in-a-dir would
/// be ambiguous), so it requires `targetSession` and ignores `targetDir`. The
/// body is empty; a stop never spawns, so the stop path passes
/// `force_approval:false` to `classify` explicitly (a stop authorizes exactly
/// like a message) — no `hidden`-flag contortion. Returns `None` unless `op`
/// is `"stop"` (so the caller falls through to a message).
pub fn parse_stop(text: &str) -> Option<Message> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("op").and_then(|o| o.as_str()) != Some("stop") {
        return None;
    }
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    Some(Message {
        id: s("id")?,
        // Authenticated + overwritten by corrald (outbox location); not trusted.
        from_cwd: s("fromCwd").unwrap_or_default(),
        from_session: s("fromSession"),
        target: Target::Session(s("targetSession")?),
        message: String::new(),
        force_new: false,
        label: None,
        // A stop never spawns, so visibility is irrelevant; the stop path gates
        // it explicitly (force_approval:false), not via this flag.
        hidden: false,
        action: Action::Stop,
    })
}

/// One line in the capability roster a `list` query returns. Every session is a
/// per-session entry the caller can address by `sessionId` (approval still
/// gates an unwhitelisted target). A reachable agent (its own directory or a
/// whitelisted pair) also exposes `description` and `cwd`; an unreachable one
/// hides both, so the caller gets an addressable handle without learning who
/// runs where. It never carries a title, name, or activity: messaging is not
/// reading, so the roster reveals that a session exists and lets the caller
/// message it, never what any agent is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The harness kind (the record `label`, or `agent` if unlabeled).
    pub kind: String,
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
/// by `sessionId`; a reachable one also carries `description` and `cwd`, an
/// unreachable one hides both.
pub fn build_roster(entries: &[RegistryEntry], visible: impl Fn(&str) -> bool) -> Vec<RosterEntry> {
    entries
        .iter()
        .map(|e| {
            let reachable = e.cwd.as_deref().is_some_and(&visible);
            RosterEntry {
                kind: e.label.clone().unwrap_or_else(|| "agent".into()),
                description: reachable.then(|| e.description.clone()).flatten(),
                cwd: reachable.then(|| e.cwd.clone()).flatten(),
                session_id: e.session_id.clone(),
                live: e.socket.is_some(),
            }
        })
        .collect()
}

/// Serialize a roster as the `list` reply line. Every entry carries `sessionId`
/// and `live`; a reachable entry adds `description`/`cwd`, an unreachable one
/// omits them so nothing identifies where it runs.
pub fn roster_json(roster: &[RosterEntry]) -> String {
    let agents: Vec<serde_json::Value> = roster
        .iter()
        .map(|r| {
            let mut m = serde_json::Map::new();
            m.insert("kind".into(), r.kind.clone().into());
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

    fn msg() -> Message {
        Message {
            id: "1".into(),
            from_cwd: "/a".into(),
            from_session: None,
            target: Target::Dir("/b".into()),
            message: "hi".into(),
            force_new: false,
            label: None,
            hidden: true,
            action: Action::Deliver,
        }
    }

    #[test]
    fn parses_and_tags() {
        let json = r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi"}"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m, msg());
        assert_eq!(m.tagged(), "[from a]\nhi");
        assert!(!m.force_new);
    }

    #[test]
    fn targets_a_session_with_reply_handle() {
        let json = r#"{"id":"1","fromCwd":"/a","fromSession":"sid-9",
            "targetSession":"sid-7","message":"hi"}"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m.target, Target::Session("sid-7".into()));
        assert_eq!(m.target_label(), "session sid-7");
        // The reply handle (sender's session) rides in the provenance tag; the
        // dir shows as its basename, the session id stays full.
        assert_eq!(m.tagged(), "[from a (session sid-9)]\nhi");
    }

    #[test]
    fn parses_label_when_present_else_none() {
        let with = parse_message(
            r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi","label":"opencode"}"#,
        )
        .unwrap();
        assert_eq!(with.label.as_deref(), Some("opencode"));
        // Absent -> None (the msg() fixture has no label key).
        let without =
            parse_message(r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi"}"#).unwrap();
        assert_eq!(without.label, None);
    }

    #[test]
    fn force_new_and_missing_fields() {
        let m = parse_message(
            r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi","forceNew":true}"#,
        )
        .unwrap();
        assert!(m.force_new);
        // Missing both target forms -> no target -> reject.
        assert_eq!(
            parse_message(r#"{"id":"1","fromCwd":"/a","message":"hi"}"#),
            None
        );
        assert_eq!(parse_message(r#"{"id":"1"}"#), None);
        assert_eq!(parse_message("nope"), None);
    }

    #[test]
    fn parse_stop_requires_op_and_session() {
        let m = parse_stop(
            r#"{"op":"stop","id":"1","fromCwd":"/a","fromSession":"s9","targetSession":"sid-7"}"#,
        )
        .unwrap();
        assert_eq!(m.action, Action::Stop);
        assert_eq!(m.target, Target::Session("sid-7".into()));
        assert_eq!(m.from_session.as_deref(), Some("s9"));
        assert!(m.message.is_empty(), "a stop carries no body");
        // Not a stop line -> None, so the caller falls through to a message.
        assert_eq!(
            parse_stop(r#"{"id":"1","fromCwd":"/a","targetSession":"x","message":"hi"}"#),
            None
        );
        // op:stop but no targetSession (a dir is not a valid stop target) -> None.
        assert_eq!(parse_stop(r#"{"op":"stop","id":"1","fromCwd":"/a"}"#), None);
        assert_eq!(parse_stop("nope"), None);
    }

    #[test]
    fn already_stopped_is_a_non_routable_success() {
        assert_eq!(Ack::AlreadyStopped.wire(), "already_stopped");
        assert!(!Ack::AlreadyStopped.routable(), "nothing left to kill");
    }

    #[test]
    fn classify_covers_every_ack() {
        let sess = Target::Session("sid".into());
        let dir = Target::Dir("/b".into());
        // Recipient found, no force -> whitelisted decides accepted vs approval.
        assert_eq!(classify(&sess, Some("/b"), true, false), Ack::Accepted);
        assert_eq!(
            classify(&sess, Some("/b"), false, false),
            Ack::ApprovalNeeded
        );
        assert_eq!(classify(&dir, Some("/b"), true, false), Ack::Accepted);
        // force_approval (a visible spawn) always needs the operator, even whitelisted.
        assert_eq!(classify(&dir, Some("/b"), true, true), Ack::ApprovalNeeded);
        // Recipient not found -> reason depends on the target kind (force moot).
        assert_eq!(classify(&sess, None, false, false), Ack::RecipientNotFound);
        assert_eq!(classify(&dir, None, false, false), Ack::DirectoryNotKnown);
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
    fn roster_exposes_every_session_and_hides_unreachable_cwd_and_description() {
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
        // Reachable /a entries expose cwd + description; liveness is preserved.
        let reachable: Vec<_> = roster.iter().filter(|r| r.cwd.is_some()).collect();
        assert_eq!(reachable.len(), 2);
        assert!(reachable
            .iter()
            .all(|r| r.cwd.as_deref() == Some("/a") && r.description.is_some()));
        assert_eq!(reachable[0].session_id, "s1");
        assert!(reachable[0].live && !reachable[1].live);
        // Unreachable /secret and /other still yield per-session entries, but
        // hide cwd and description; live stays exposed.
        let unreachable: Vec<_> = roster.iter().filter(|r| r.cwd.is_none()).collect();
        assert_eq!(unreachable.len(), 2);
        assert!(unreachable
            .iter()
            .all(|r| r.description.is_none() && !r.session_id.is_empty()));
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
