//! Agent-initiated cross-session messages. The `corral_message_agent` tool
//! submits a message over the control socket (`corrald.sock`); corral is the
//! trusted router that authorizes, resolves the target, and injects the
//! message. Parsing, classification, and authorization are pure and
//! unit-tested; the IO wrappers are thin.

use std::io::Write;
use std::path::Path;

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
}

impl Message {
    /// The delivered text, carrying in-band provenance so the receiving model
    /// and the watching human see it came from another agent, not the
    /// operator. When the sender's session is known it is included as a reply
    /// handle: the receiver answers with `corral_message_agent(target_session = ..)`.
    pub fn tagged(&self) -> String {
        match &self.from_session {
            Some(sid) => format!(
                "[from agent in {} (session {})] {}",
                self.from_cwd, sid, self.message
            ),
            None => format!("[from agent in {}] {}", self.from_cwd, self.message),
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
    /// approval. The sender is told now, not made to wait on a human.
    Blocked,
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
            Ack::Blocked => "blocked",
            Ack::RecipientNotFound => "recipient_not_found",
            Ack::DirectoryNotKnown => "directory_not_known",
        }
    }

    /// Whether the router should route this message (only resolvable targets).
    pub fn routable(self) -> bool {
        matches!(self, Ack::Accepted | Ack::Blocked)
    }
}

/// Classify a parsed message from resolved facts (pure, trivially tested).
/// `target_cwd` is `Some` when the recipient is found (a known session's cwd,
/// or an existing target directory), else `None`. `whitelisted` is consulted
/// only when the recipient is found.
pub fn classify(target: &Target, target_cwd: Option<&str>, whitelisted: bool) -> Ack {
    match target_cwd {
        None => match target {
            Target::Session(_) => Ack::RecipientNotFound,
            Target::Dir(_) => Ack::DirectoryNotKnown,
        },
        Some(_) if whitelisted => Ack::Accepted,
        Some(_) => Ack::Blocked,
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
    })
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
        }
    }

    #[test]
    fn parses_and_tags() {
        let json = r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi"}"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m, msg());
        assert_eq!(m.tagged(), "[from agent in /a] hi");
        assert!(!m.force_new);
    }

    #[test]
    fn targets_a_session_with_reply_handle() {
        let json = r#"{"id":"1","fromCwd":"/a","fromSession":"sid-9",
            "targetSession":"sid-7","message":"hi"}"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m.target, Target::Session("sid-7".into()));
        assert_eq!(m.target_label(), "session sid-7");
        // The reply handle (sender's session) rides in the provenance tag.
        assert_eq!(m.tagged(), "[from agent in /a (session sid-9)] hi");
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
        // Recipient found -> whitelisted decides accepted vs blocked.
        assert_eq!(classify(&sess, Some("/b"), true), Ack::Accepted);
        assert_eq!(classify(&sess, Some("/b"), false), Ack::Blocked);
        assert_eq!(classify(&dir, Some("/b"), true), Ack::Accepted);
        // Recipient not found -> reason depends on the target kind.
        assert_eq!(classify(&sess, None, false), Ack::RecipientNotFound);
        assert_eq!(classify(&dir, None, false), Ack::DirectoryNotKnown);
        // Only resolvable targets are routed onward.
        assert!(Ack::Accepted.routable());
        assert!(Ack::Blocked.routable());
        assert!(!Ack::RecipientNotFound.routable());
        assert!(!Ack::DirectoryNotKnown.routable());
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
