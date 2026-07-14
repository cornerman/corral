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
    /// argv to spawn a fresh session of this kind, rooted at a cwd the consumer
    /// supplies (e.g. `["pi"]`). The consumer runs it verbatim and never parses
    /// it, so it stays agent-neutral. `None` means this producer did not
    /// announce a spawn command (not launchable-fresh by the consumer).
    pub spawn_command: Option<Vec<String>>,
    /// argv to relaunch this exact session (e.g. `["pi", "--session", "<file>"]`).
    /// `None` for an ephemeral (non-resumable) session. A dormant record is
    /// resumable exactly when this is set.
    pub resume_command: Option<Vec<String>>,
    /// The agent kind (e.g. `pi`). Live cards read this from the socket
    /// filename; dormant cards (no socket) rely on this field, so the board
    /// stays agent-agnostic. Absent means an older/unknown producer.
    pub label: Option<String>,
    /// ISO-8601 timestamp of the last observed activity. ISO-8601 sorts
    /// correctly as a plain string, so it doubles as the latest-per-cwd key.
    pub last_seen: Option<String>,
    /// Whether corral launches this agent's command directly (a self-windowing
    /// GUI app like quine) instead of wrapping it in a terminal. Absent/false
    /// means terminal-wrapped, so every existing pi/opencode record keeps its
    /// behavior; only an explicit `true` opts into direct launch.
    pub gui: bool,
    /// Optional CLI flag that carries an initial message on launch (e.g.
    /// `"--message"` for quine). When set, a consumer passes the message as
    /// this flag's value; when absent, the message is a trailing positional
    /// argument (see §2a). Lets a flag-based agent take a launch message
    /// without a positional.
    pub message_flag: Option<String>,
}

impl RegistryEntry {
    /// The launch options this record declared (gui + message flag), for
    /// `Launcher::launch`.
    pub fn launch_mode(&self) -> crate::launch::LaunchMode {
        crate::launch::LaunchMode {
            gui: self.gui,
            message_flag: self.message_flag.clone(),
        }
    }
}

/// Parse one registry JSON document. Requires `sessionId`; everything else is
/// optional. Returns `None` on malformed JSON or a missing id.
pub fn parse_registry_json(text: &str) -> Option<RegistryEntry> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    // A command is a JSON array of strings; a non-array or non-string element
    // yields None so a malformed command never launches a garbled argv.
    let cmd_field = |k: &str| {
        v.get(k).and_then(|x| x.as_array()).map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
    };
    Some(RegistryEntry {
        session_id: str_field("sessionId")?,
        cwd: str_field("cwd"),
        title: str_field("title"),
        socket: str_field("socket").map(PathBuf::from),
        spawn_command: cmd_field("spawnCommand"),
        resume_command: cmd_field("resumeCommand"),
        label: str_field("label"),
        last_seen: str_field("lastSeen"),
        gui: v.get("gui").and_then(|x| x.as_bool()).unwrap_or(false),
        message_flag: str_field("messageFlag"),
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
            "socket":"/tmp/p/.corral/pi-42.sock",
            "spawnCommand":["pi"],
            "resumeCommand":["pi","--session","/s/abc.jsonl"],"lastSeen":"t"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.session_id, "abc");
        assert_eq!(e.cwd.as_deref(), Some("/tmp/p"));
        assert_eq!(
            e.spawn_command.as_deref(),
            Some(["pi".to_string()].as_slice())
        );
        assert_eq!(
            e.resume_command.as_deref().unwrap(),
            ["pi", "--session", "/s/abc.jsonl"]
        );
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
    fn command_fields_absent_or_malformed_are_none() {
        // Absent commands are None (older producer): discoverable, not launchable.
        let e = parse_registry_json(r#"{"sessionId":"a"}"#).unwrap();
        assert_eq!(e.spawn_command, None);
        assert_eq!(e.resume_command, None);
        // A non-array command is ignored rather than launched as garbage.
        let e = parse_registry_json(r#"{"sessionId":"a","spawnCommand":"pi"}"#).unwrap();
        assert_eq!(e.spawn_command, None);
    }

    #[test]
    fn rejects_record_without_session_id() {
        assert_eq!(parse_registry_json(r#"{"cwd":"/tmp"}"#), None);
        assert_eq!(parse_registry_json("not json"), None);
    }

    #[test]
    fn message_flag_parses_when_present_else_none() {
        let e = parse_registry_json(r#"{"sessionId":"s1","messageFlag":"--message"}"#).unwrap();
        assert_eq!(e.message_flag.as_deref(), Some("--message"));
        // Absent -> None (positional message, the default for pi/opencode).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.message_flag, None);
    }

    #[test]
    fn gui_field_parses_true_false_and_absent() {
        // Explicit true.
        let e = parse_registry_json(r#"{"sessionId":"s1","gui":true}"#).unwrap();
        assert!(e.gui);
        // Explicit false.
        let e = parse_registry_json(r#"{"sessionId":"s2","gui":false}"#).unwrap();
        assert!(!e.gui);
        // Absent defaults to false (pi/opencode records have no such field).
        let e = parse_registry_json(r#"{"sessionId":"s3"}"#).unwrap();
        assert!(!e.gui);
        // A non-boolean value is ignored leniently, not an error.
        let e = parse_registry_json(r#"{"sessionId":"s4","gui":"yes"}"#).unwrap();
        assert!(!e.gui);
    }
}
