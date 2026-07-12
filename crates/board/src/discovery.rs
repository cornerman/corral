//! Session discovery via the registry. Each announcing agent writes
//! `<registry>/<sessionId>.json` describing itself; the `socket` field points
//! at a workdir-local ACP socket (`<workdir>/.corral/<label>-<pid>.sock`, pi
//! uses `pi-<pid>.sock`) while the session is live, and is cleared on clean
//! shutdown. Corral reads the registry to find sockets it could never scan
//! for directly (they live inside each session's own workdir).

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Clone)]
pub struct SocketEntry {
    pub path: PathBuf,
    pub label: String,
    pub pid: u32,
}

/// One session's registry record. `socket` is present only while the session
/// is live; a record with `socket == None` is dormant (resumable via
/// `resume`, rendered later). Parsed leniently from JSON so an unknown extra
/// field never breaks discovery.
#[derive(Debug, PartialEq, Clone)]
pub struct RegistryEntry {
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub socket: Option<PathBuf>,
    pub resume: Option<String>,
}

/// Parse one registry JSON document. Requires `sessionId`; everything else is
/// optional. Returns `None` on malformed JSON or a missing id.
pub fn parse_registry_json(text: &str) -> Option<RegistryEntry> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    Some(RegistryEntry {
        session_id: str_field("sessionId")?,
        cwd: str_field("cwd"),
        title: str_field("title"),
        socket: str_field("socket").map(PathBuf::from),
        resume: str_field("resume"),
    })
}

/// Scan the registry directory for `*.json` records. A missing directory is an
/// empty result, not an error: no agent has announced yet.
pub fn scan_registry(dir: &Path) -> Vec<RegistryEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| parse_registry_json(&t))
        .collect()
}

/// The connectable socket of a live registry entry, if any. Dormant records
/// (no `socket`) and records whose socket filename breaks the `<label>-<pid>`
/// convention yield `None`.
pub fn live_socket(entry: &RegistryEntry) -> Option<SocketEntry> {
    let path = entry.socket.clone()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let (label, pid) = parse_socket_filename(&name)?;
    Some(SocketEntry { path, label, pid })
}

/// Parse `<label>-<pid>.sock`. The pid is everything after the *last* '-',
/// so labels themselves may contain dashes.
pub fn parse_socket_filename(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_suffix(".sock")?;
    let (label, pid) = stem.rsplit_once('-')?;
    if label.is_empty() {
        return None;
    }
    Some((label.to_string(), pid.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_filename() {
        assert_eq!(
            parse_socket_filename("claude-1234.sock"),
            Some(("claude".to_string(), 1234))
        );
    }

    #[test]
    fn label_may_contain_dashes() {
        assert_eq!(
            parse_socket_filename("claude-agent-acp-99.sock"),
            Some(("claude-agent-acp".to_string(), 99))
        );
    }

    #[test]
    fn rejects_non_sockets_and_malformed_names() {
        assert_eq!(parse_socket_filename("readme.txt"), None);
        assert_eq!(parse_socket_filename("nopid.sock"), None);
        assert_eq!(parse_socket_filename("label-notanumber.sock"), None);
        assert_eq!(parse_socket_filename("-42.sock"), None);
    }

    #[test]
    fn scan_missing_dir_is_empty() {
        assert!(scan_registry(Path::new("/nonexistent/definitely-not-here")).is_empty());
    }

    #[test]
    fn parses_live_registry_record() {
        let json = r#"{"sessionId":"abc","cwd":"/tmp/p","title":"fix bug",
            "socket":"/tmp/p/.corral/pi-42.sock","resume":"/s/abc.jsonl","lastSeen":"t"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.session_id, "abc");
        assert_eq!(e.cwd.as_deref(), Some("/tmp/p"));
        let sock = live_socket(&e).unwrap();
        assert_eq!(sock.label, "pi");
        assert_eq!(sock.pid, 42);
        assert_eq!(sock.path, PathBuf::from("/tmp/p/.corral/pi-42.sock"));
    }

    #[test]
    fn dormant_record_has_no_socket() {
        let e = parse_registry_json(r#"{"sessionId":"abc","socket":null}"#).unwrap();
        assert_eq!(e.socket, None);
        assert_eq!(live_socket(&e), None);
    }

    #[test]
    fn rejects_record_without_session_id() {
        assert_eq!(parse_registry_json(r#"{"cwd":"/tmp"}"#), None);
        assert_eq!(parse_registry_json("not json"), None);
    }
}
