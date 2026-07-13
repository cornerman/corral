//! The outbox: agent-initiated cross-session messages. The `corral_message_agent`
//! tool drops a `<id>.json` mailbox file here; corral is the trusted router
//! that authorizes, resolves the target directory, and injects the message.
//! Parsing and authorization are pure and unit-tested; the IO wrappers are
//! thin.

use std::io::Write;
use std::path::{Path, PathBuf};

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

    /// Human label for the target, shown in the approval overlay.
    pub fn target_label(&self) -> String {
        match &self.target {
            Target::Dir(d) => d.clone(),
            Target::Session(s) => format!("session {s}"),
        }
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

/// Read every pending mailbox file, newest last by filename. A missing
/// directory is empty (no messages queued yet). Each entry pairs the file path
/// (to delete on delivery) with its parsed message.
pub fn scan_outbox(dir: &Path) -> Vec<(PathBuf, Message)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Message)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let msg = parse_message(&std::fs::read_to_string(&p).ok()?)?;
            Some((p, msg))
        })
        .collect();
    // Stable order so the approval prompt does not jump between scans.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
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
    fn scan_outbox_reads_json_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("m.json"),
            r#"{"id":"1","fromCwd":"/a","targetDir":"/b","message":"hi"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("note.txt"), "ignore").unwrap();
        let got = scan_outbox(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.id, "1");
        assert!(scan_outbox(std::path::Path::new("/nonexistent/x")).is_empty());
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
