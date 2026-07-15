//! Agent-initiated cross-session messages. The `corral_message_agent` tool
//! submits a message over the control socket (`corrald.sock`); corral is the
//! trusted router that authorizes, resolves the target, and injects the
//! message. Parsing, classification, and authorization are pure and
//! unit-tested; the IO wrappers are thin.

use std::collections::BTreeMap;
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
}

impl Message {
    /// The delivered text, carrying in-band provenance so the receiving model
    /// and the watching human see it came from another agent, not the
    /// operator. Kept short so it does not dominate a board activity line: the
    /// sender directory is shown as its basename (the full cwd is not needed to
    /// reply, which uses the session id, and `list_corral_agents` still gives
    /// the full cwd for a reachable dir). When the sender's session is known it
    /// is included in full as a reply handle: the receiver answers with
    /// `corral_message_agent(target_session = ..)`.
    pub fn tagged(&self) -> String {
        let from = basename(&self.from_cwd);
        match &self.from_session {
            Some(sid) => format!("[from {from} (session {sid})] {}", self.message),
            None => format!("[from {from}] {}", self.message),
        }
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
}

impl Ack {
    /// The wire word sent back over the control socket.
    pub fn wire(self) -> &'static str {
        match self {
            Ack::Accepted => "accepted",
            Ack::ApprovalNeeded => "approval_needed",
            Ack::RecipientNotFound => "recipient_not_found",
            Ack::DirectoryNotKnown => "directory_not_known",
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
/// `hidden` is the requested spawn visibility: a visible spawn (`false`) always
/// needs the operator, so the whitelist alone never accepts it.
pub fn classify(target: &Target, target_cwd: Option<&str>, whitelisted: bool, hidden: bool) -> Ack {
    match target_cwd {
        None => match target {
            Target::Session(_) => Ack::RecipientNotFound,
            Target::Dir(_) => Ack::DirectoryNotKnown,
        },
        // A visible window is operator-gated regardless of the whitelist.
        Some(_) if !hidden => Ack::ApprovalNeeded,
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
        from_cwd: s("fromCwd")?,
        from_session: s("fromSession"),
        target,
        message: s("message")?,
        force_new: v.get("forceNew").and_then(|x| x.as_bool()).unwrap_or(false),
        label: s("label"),
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(true),
    })
}

/// One line in the capability roster a `list` query returns. Either a full
/// entry for an agent the caller may reach (its own directory or a whitelisted
/// pair, so dir / session / liveness are exposed) or an anonymous kind entry
/// that folds every unreachable directory to the distinct harness kinds present
/// (kind + description only). It never carries a title, name, or activity:
/// messaging is not reading, so the roster reveals that a kind of agent exists
/// and whether the caller may message it, never what any agent is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The harness kind (the record `label`, or `agent` if unlabeled).
    pub kind: String,
    pub description: Option<String>,
    /// Set only on a visible entry (`None` on an anonymous kind entry).
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    /// Liveness, meaningful only on a visible entry.
    pub live: bool,
    /// Whether the caller may message this without an approval popup.
    pub can_message: bool,
}

/// Build the capability roster for a caller. `visible(target_cwd)` reports
/// whether the caller may reach that directory (its own dir, or a whitelisted
/// `(from -> target)` pair). A visible agent becomes a full per-session entry
/// (so the caller can address a precise `target_session`); every other agent is
/// folded, by harness kind, into one anonymous entry (latest-seen description
/// wins), so a caller learns which kinds exist without learning who runs where.
pub fn build_roster(entries: &[RegistryEntry], visible: impl Fn(&str) -> bool) -> Vec<RosterEntry> {
    let mut roster = Vec::new();
    // kind -> (description, its last_seen) for the anonymous fold; the latest
    // last_seen wins the description, matching "latest description per kind".
    let mut anon: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for e in entries {
        let kind = e.label.clone().unwrap_or_else(|| "agent".into());
        if e.cwd.as_deref().is_some_and(&visible) {
            roster.push(RosterEntry {
                kind,
                description: e.description.clone(),
                cwd: e.cwd.clone(),
                session_id: Some(e.session_id.clone()),
                live: e.socket.is_some(),
                can_message: true,
            });
        } else {
            let slot = anon.entry(kind).or_insert((None, None));
            // None (no timestamp) sorts below any Some, so a timestamped record
            // wins over an undated one, and later wins over earlier.
            if e.last_seen >= slot.1 {
                *slot = (e.description.clone(), e.last_seen.clone());
            }
        }
    }
    roster.extend(
        anon.into_iter()
            .map(|(kind, (description, _))| RosterEntry {
                kind,
                description,
                cwd: None,
                session_id: None,
                live: false,
                can_message: false,
            }),
    );
    roster
}

/// Serialize a roster as the `list` reply line. A visible entry carries
/// `cwd`/`sessionId`; an anonymous entry omits them, so nothing identifies an
/// unreachable session.
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
            if let Some(s) = &r.session_id {
                m.insert("sessionId".into(), s.clone().into());
            }
            m.insert("live".into(), r.live.into());
            m.insert("canMessage".into(), r.can_message.into());
            serde_json::Value::Object(m)
        })
        .collect();
    serde_json::json!({ "status": "ok", "agents": agents }).to_string()
}

/// Parse a `list` roster query (`{"op":"list","fromCwd":"/a"}`), returning the
/// caller's cwd. `None` means it is not a list query, so the caller falls
/// through to message parsing.
pub fn parse_list(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    (v.get("op").and_then(|o| o.as_str()) == Some("list"))
        .then(|| v.get("fromCwd").and_then(|c| c.as_str()).map(String::from))
        .flatten()
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
        }
    }

    #[test]
    fn parses_and_tags() {
        let json = r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi"}"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m, msg());
        assert_eq!(m.tagged(), "[from a] hi");
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
        assert_eq!(m.tagged(), "[from a (session sid-9)] hi");
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
    fn classify_covers_every_ack() {
        let sess = Target::Session("sid".into());
        let dir = Target::Dir("/b".into());
        // Recipient found + hidden spawn -> whitelisted decides accepted vs approval.
        assert_eq!(classify(&sess, Some("/b"), true, true), Ack::Accepted);
        assert_eq!(
            classify(&sess, Some("/b"), false, true),
            Ack::ApprovalNeeded
        );
        assert_eq!(classify(&dir, Some("/b"), true, true), Ack::Accepted);
        // A visible spawn (hidden=false) always needs the operator, even whitelisted.
        assert_eq!(classify(&dir, Some("/b"), true, false), Ack::ApprovalNeeded);
        // Recipient not found -> reason depends on the target kind (visibility moot).
        assert_eq!(classify(&sess, None, false, true), Ack::RecipientNotFound);
        assert_eq!(classify(&dir, None, false, true), Ack::DirectoryNotKnown);
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
        }
    }

    #[test]
    fn roster_exposes_visible_dirs_and_folds_the_rest_anonymously() {
        let entries = [
            rec("s1", "/a", "pi", true, Some("terminal agent")),
            rec("s2", "/a", "pi", false, Some("terminal agent")),
            rec("s3", "/secret", "quine", true, Some("gui app")),
            rec("s4", "/other", "pi", true, Some("terminal agent")),
        ];
        // Caller sees only /a.
        let roster = build_roster(&entries, |cwd| cwd == "/a");
        // Two visible per-session entries for /a (live + dormant), addressable.
        let visible: Vec<_> = roster.iter().filter(|r| r.cwd.is_some()).collect();
        assert_eq!(visible.len(), 2);
        assert!(visible
            .iter()
            .all(|r| r.can_message && r.cwd.as_deref() == Some("/a")));
        assert_eq!(visible[0].session_id.as_deref(), Some("s1"));
        assert!(visible[0].live && !visible[1].live);
        // /secret and /other fold to distinct anonymous kinds: pi + quine, one
        // each, no cwd/session, not messageable.
        let anon: Vec<_> = roster.iter().filter(|r| r.cwd.is_none()).collect();
        assert_eq!(anon.len(), 2, "pi and quine, folded once each");
        assert!(anon
            .iter()
            .all(|r| !r.can_message && r.session_id.is_none()));
        let kinds: Vec<_> = anon.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"pi") && kinds.contains(&"quine"));
    }

    #[test]
    fn roster_json_hides_title_and_omits_cwd_for_anonymous() {
        let entries = [rec("s1", "/secret", "pi", true, Some("terminal agent"))];
        let json = roster_json(&build_roster(&entries, |_| false));
        assert!(!json.contains("secret title"), "never leak the title");
        assert!(!json.contains("/secret"), "never leak an unreachable cwd");
        assert!(!json.contains("s1"), "never leak an unreachable session id");
        assert!(json.contains("\"kind\":\"pi\"") && json.contains("terminal agent"));
        assert!(json.contains("\"canMessage\":false"));
    }

    #[test]
    fn parse_list_matches_only_the_list_op() {
        assert_eq!(
            parse_list(r#"{"op":"list","fromCwd":"/a"}"#).as_deref(),
            Some("/a")
        );
        assert_eq!(
            parse_list(r#"{"id":"1","fromCwd":"/a","message":"hi"}"#),
            None
        );
        assert_eq!(parse_list("nope"), None);
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
