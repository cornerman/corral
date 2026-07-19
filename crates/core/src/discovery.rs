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
    /// Whether this session runs hidden (inside a headless cage), so the board
    /// reveals it by resume rather than focusing a host window. Written by the
    /// adapter from the `CORRAL_HIDDEN` env signal corral sets at a hidden
    /// spawn. Absent/false is a normal visible session.
    pub hidden: bool,
    /// A one-line, human-readable description of this harness kind, authored
    /// by the adapter (e.g. "terminal TUI coding agent"). Consumers surface it
    /// in a capability roster so a caller can pick a kind to spawn; latest-seen
    /// per label wins. The string is adapter code, not model output. Absent
    /// for older/unknown producers.
    pub description: Option<String>,
    /// The LLM model this session runs, as `"<provider>/<id>"` (e.g.
    /// `anthropic/claude-opus-4`). Written by the adapter so a selected
    /// dormant card shows its last-known model; live cards refresh it over the
    /// socket (a `config_options_update` broadcast). Verbatim adapter string,
    /// shown as-is (corral never prettifies). Absent for a producer that does
    /// not report a model.
    pub model: Option<String>,
}

impl RegistryEntry {
    /// The launch options this record declared (gui + message flag), for
    /// `Launcher::launch`.
    pub fn launch_mode(&self) -> crate::launch::LaunchMode {
        crate::launch::LaunchMode {
            gui: self.gui,
            message_flag: self.message_flag.clone(),
            hidden: self.hidden,
        }
    }

    /// Resume argv with `{sessionId}`/`{cwd}` substituted (see
    /// `Agent::resume_argv`). `None` when the record announced no resume command.
    pub fn resume_argv(&self) -> Option<Vec<String>> {
        self.resume_command
            .as_ref()
            .map(|c| crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref()))
    }

    /// Spawn argv with `{cwd}` substituted. `None` when the record announced no
    /// spawn command.
    pub fn spawn_argv(&self) -> Option<Vec<String>> {
        self.spawn_command
            .as_ref()
            .map(|c| crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref()))
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
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
        description: str_field("description"),
        model: str_field("model"),
    })
}

/// Read a directory of `*.json` records and parse them. Used by viewers over
/// corrald's **vetted** `state/registry/` (trusted — corrald already
/// authenticated and validated every entry), and by corrald over its own
/// output. A missing directory is an empty result, not an error. Raw,
/// agent-writable records are never read here; only `curation` touches those.
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

/// Whether a `sessionId` is safe to trust and to substitute into a launch
/// argv (security design C3/T16). Restricted to `[A-Za-z0-9._-]`, non-empty,
/// and never leading with `-`, so a value like `--config=/evil` can never be
/// mistaken for a flag by a launched program. A record whose id fails this is
/// rejected at acceptance (wired in the identity phase).
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Derive the authenticated `cwd` from a record's *resolved physical path*
/// (security design T2/T3). A record physically lives at
/// `<cwd>/.corral/<name>.json`, so the cwd is the grandparent of a file whose
/// parent directory is named `.corral`. Any other shape yields `None`
/// (rejected). Pure: the caller supplies the canonical path derived from an
/// open fd (never a re-followed symlink, see the identity phase).
pub fn cwd_from_record_path(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    if parent.file_name()? != ".corral" {
        return None;
    }
    Some(parent.parent()?.to_string_lossy().into_owned())
}

/// Derive the authenticated `cwd` from an outbox submission's *resolved
/// physical path* (security design T2). A submission lives at
/// `<cwd>/.corral/outbox/<name>`, so the cwd is the great-grandparent of a file
/// under a directory named `outbox` under one named `.corral`. Any other shape
/// yields `None`, so corrald never derives a cwd from an arbitrary path.
pub fn cwd_from_outbox_path(path: &Path) -> Option<String> {
    let outbox = path.parent()?;
    if outbox.file_name()? != "outbox" {
        return None;
    }
    let corral = outbox.parent()?;
    if corral.file_name()? != ".corral" {
        return None;
    }
    Some(corral.parent()?.to_string_lossy().into_owned())
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
    fn model_field_parses_and_defaults_none() {
        let e = parse_registry_json(r#"{"sessionId":"s1","model":"anthropic/claude-opus-4"}"#)
            .unwrap();
        assert_eq!(e.model.as_deref(), Some("anthropic/claude-opus-4"));
        // Absent -> None (older/unknown producer).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.model, None);
        // Non-string -> None (never a garbled value).
        let e = parse_registry_json(r#"{"sessionId":"s3","model":42}"#).unwrap();
        assert_eq!(e.model, None);
    }

    #[test]
    fn session_id_charset_is_strict() {
        assert!(valid_session_id("6f1c2e7a-3b4d-4c5e-9a10"));
        assert!(valid_session_id("abc.def_123"));
        // Rejected: leading dash (flag injection), empty, and metacharacters.
        assert!(!valid_session_id("--config=/evil"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("a/b"));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id("a;rm"));
    }

    #[test]
    fn cwd_derives_from_record_physical_path() {
        assert_eq!(
            cwd_from_record_path(Path::new("/home/dev/x/.corral/abc.json")).as_deref(),
            Some("/home/dev/x")
        );
        // Not under a .corral dir -> rejected (cannot be attributed).
        assert_eq!(
            cwd_from_record_path(Path::new("/home/dev/x/abc.json")),
            None
        );
        assert_eq!(cwd_from_record_path(Path::new("/abc.json")), None);
    }

    #[test]
    fn cwd_derives_from_outbox_physical_path() {
        assert_eq!(
            cwd_from_outbox_path(Path::new("/home/dev/x/.corral/outbox/m1.json")).as_deref(),
            Some("/home/dev/x")
        );
        // Wrong shape (not under .corral/outbox) -> rejected.
        assert_eq!(
            cwd_from_outbox_path(Path::new("/home/dev/x/.corral/m1.json")),
            None
        );
        assert_eq!(cwd_from_outbox_path(Path::new("/etc/passwd")), None);
    }

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
    fn resume_and_spawn_argv_substitute_placeholders() {
        let json = r#"{"sessionId":"s9","cwd":"/p",
            "spawnCommand":["cursor","{cwd}"],
            "resumeCommand":["pi","--session","{sessionId}"]}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.resume_argv().unwrap(), vec!["pi", "--session", "s9"]);
        assert_eq!(e.spawn_argv().unwrap(), vec!["cursor", "/p"]);
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
    fn hidden_field_parses_true_false_and_absent() {
        let e = parse_registry_json(r#"{"sessionId":"s1","hidden":true}"#).unwrap();
        assert!(e.hidden);
        let e = parse_registry_json(r#"{"sessionId":"s2","hidden":false}"#).unwrap();
        assert!(!e.hidden);
        // Absent defaults to false (existing pi/opencode records).
        let e = parse_registry_json(r#"{"sessionId":"s3"}"#).unwrap();
        assert!(!e.hidden);
        // Non-boolean ignored leniently.
        let e = parse_registry_json(r#"{"sessionId":"s4","hidden":"yes"}"#).unwrap();
        assert!(!e.hidden);
        // launch_mode carries it.
        let e = parse_registry_json(r#"{"sessionId":"s5","hidden":true}"#).unwrap();
        assert!(e.launch_mode().hidden);
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
