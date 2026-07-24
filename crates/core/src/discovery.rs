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
    /// The window-owning pid **as the agent observed it in its own PID
    /// namespace** (from the record's `pid`, or a legacy `<label>-<pid>.sock`
    /// filename). `None` when the record names no pid. This is not yet a host
    /// pid: the consumer translates it via `pid_namespace` (see
    /// `resolve_host_pid`) before correlating it to a host window.
    pub pid: Option<u32>,
    /// The nsfs inode of `pid`'s PID namespace (record `pidNamespace`), the key
    /// that translates `pid` to a host pid. `None` means `pid` is already
    /// host-level (the agent shares the consumer's PID namespace).
    pub pid_namespace: Option<u64>,
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
    /// The window-owning pid as the agent observes it in its own PID namespace
    /// (pi/opencode: `getpid()`; cursor: the Electron pid; claude: the
    /// interactive Claude pid). Moved off the socket filename into the record
    /// so the filename can be a plain `<sessionId>.sock`. Consumers translate
    /// it to a host pid via `pid_namespace`. Absent for a producer that does
    /// not report it (then not window-correlatable).
    pub pid: Option<u32>,
    /// The nsfs inode of `pid`'s PID namespace,
    /// `stat("/proc/<pid>/ns/pid").st_ino`. Lets a host consumer translate the
    /// namespace-local `pid` to a host pid (the NSpid bridge). Absent means
    /// `pid` is already host-level (agent shares the consumer's PID namespace),
    /// preserving pre-bridge behavior.
    pub pid_namespace: Option<u64>,
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
    /// Count of session-log entries (messages, tool calls, custom entries) —
    /// an honest size proxy for "how big this transcript is". Written by an
    /// adapter that can introspect its own transcript (pi only today); `None`
    /// for a producer that does not report it, which also gates the whole
    /// entries/percent/age footer group off (see `Agent::footer_line`).
    pub entries: Option<u64>,
    /// This session's context usage as a percentage of its model's context
    /// window (pi's own `ctx.getContextUsage()`), 0-100. `None` when the
    /// adapter's own estimate is unknown (e.g. right after compaction) or the
    /// adapter does not report it at all.
    pub context_percent: Option<u32>,
    /// A pre-formatted age string (e.g. `"3d"`, `"42m"`) for how long this
    /// session's transcript has existed, computed adapter-side from the
    /// session's own creation timestamp (durable across a resume). Kept as an
    /// opaque string rather than a raw timestamp: no ISO-8601 parsing
    /// dependency needed in Rust, matching how `model` is also carried as an
    /// opaque adapter string.
    pub context_age: Option<String>,
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
        self.resume_command.as_ref().map(|c| {
            crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref())
        })
    }

    /// Spawn argv with `{cwd}` substituted. `None` when the record announced no
    /// spawn command.
    pub fn spawn_argv(&self) -> Option<Vec<String>> {
        self.spawn_command.as_ref().map(|c| {
            crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref())
        })
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
        pid: v.get("pid").and_then(|x| x.as_u64()).map(|n| n as u32),
        pid_namespace: v.get("pidNamespace").and_then(|x| x.as_u64()),
        spawn_command: cmd_field("spawnCommand"),
        resume_command: cmd_field("resumeCommand"),
        label: str_field("label"),
        last_seen: str_field("lastSeen"),
        gui: v.get("gui").and_then(|x| x.as_bool()).unwrap_or(false),
        message_flag: str_field("messageFlag"),
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
        description: str_field("description"),
        model: str_field("model"),
        entries: v.get("entries").and_then(|x| x.as_u64()),
        context_percent: v
            .get("contextPercent")
            .and_then(|x| x.as_u64())
            .map(|n| n as u32),
        context_age: str_field("contextAge"),
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
/// (no `socket`) yield `None`. `label`/`pid`/`pid_namespace` come from the
/// record; a legacy `<label>-<pid>.sock` filename is a fallback only for a
/// record written before those moved into the JSON (transition compatibility).
/// A live socket without a resolvable pid is still returned (watched); it is
/// just not window-correlatable (focus fails loud).
pub fn live_socket(entry: &RegistryEntry) -> Option<SocketEntry> {
    let path = entry.socket.clone()?;
    let legacy = path
        .file_name()
        .and_then(|n| parse_socket_filename(&n.to_string_lossy()));
    let label = entry
        .label
        .clone()
        .or_else(|| legacy.as_ref().map(|(l, _)| l.clone()))
        .unwrap_or_else(|| "agent".to_string());
    let pid = entry.pid.or_else(|| legacy.map(|(_, p)| p));
    Some(SocketEntry {
        path,
        label,
        pid,
        pid_namespace: entry.pid_namespace,
    })
}

/// One process as the host sees it, for the NSpid bridge. `pid_ns_ino` is the
/// nsfs inode of its PID namespace; `deepest_nspid` is the rightmost `NSpid:`
/// value in `/proc/<host_pid>/status`, i.e. its pid in its own (deepest)
/// namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub host_pid: u32,
    pub pid_ns_ino: u64,
    pub deepest_nspid: u32,
}

/// A view of the host `/proc`, injectable so `resolve_host_pid` is unit-testable
/// without a real process tree.
pub trait ProcTable {
    /// nsfs inode of the *reader's own* PID namespace (`/proc/self/ns/pid`).
    fn self_pid_ns_ino(&self) -> Option<u64>;
    /// Every process the reader can see, with its namespace inode and deepest
    /// namespace-local pid.
    fn processes(&self) -> Vec<ProcInfo>;
}

/// Translate a namespace-local pid to a host pid (the NSpid bridge). The
/// namespace inode selects the sandbox; the namespace-local pid selects the
/// process within it. When the target shares the reader's own PID namespace the
/// pid is already host-level and returned directly (the common, no-sandbox
/// case). `None` when no host process matches (vanished, or a kernel without
/// `NSpid`).
pub fn resolve_host_pid(table: &impl ProcTable, ns_pid: u32, pidns_ino: u64) -> Option<u32> {
    if table.self_pid_ns_ino() == Some(pidns_ino) {
        return Some(ns_pid);
    }
    table
        .processes()
        .into_iter()
        .find(|p| p.pid_ns_ino == pidns_ino && p.deepest_nspid == ns_pid)
        .map(|p| p.host_pid)
}

/// Resolve a socket's `(pid, pid_namespace)` to a host pid for window
/// correlation. No pid -> `None` (not correlatable). No namespace -> the pid is
/// already host-level (pre-bridge behavior). Otherwise translate via the NSpid
/// bridge.
pub fn resolve_socket_host_pid(
    table: &impl ProcTable,
    pid: Option<u32>,
    pid_namespace: Option<u64>,
) -> Option<u32> {
    let pid = pid?;
    match pid_namespace {
        Some(ino) => resolve_host_pid(table, pid, ino),
        None => Some(pid),
    }
}

/// The real host `/proc`, used in production. Reads `/proc/self/ns/pid` for the
/// reader's namespace and scans numeric `/proc/<pid>` entries. Processes the
/// reader cannot stat (permission, or vanished) are skipped.
pub struct RealProc;

impl ProcTable for RealProc {
    fn self_pid_ns_ino(&self) -> Option<u64> {
        pid_ns_ino(Path::new("/proc/self"))
    }
    fn processes(&self) -> Vec<ProcInfo> {
        let Ok(rd) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let host_pid: u32 = e.file_name().to_str()?.parse().ok()?;
                let dir = e.path();
                Some(ProcInfo {
                    host_pid,
                    pid_ns_ino: pid_ns_ino(&dir)?,
                    deepest_nspid: deepest_nspid(&dir)?,
                })
            })
            .collect()
    }
}

/// nsfs inode of a `/proc/<pid>` (or `/proc/self`) entry's PID namespace.
/// `metadata` follows the magic `ns/pid` symlink to the nsfs node, whose
/// `st_ino` is the namespace identity (the key `lsns` correlates on).
fn pid_ns_ino(proc_dir: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(proc_dir.join("ns/pid"))
        .ok()
        .map(|m| m.ino())
}

/// The deepest (rightmost) `NSpid:` value in `/proc/<pid>/status` — the
/// process's pid in its own namespace. man7 proc_pid_status(5): leftmost is the
/// pid in the reader's (host) namespace, rightmost the deepest.
fn deepest_nspid(proc_dir: &Path) -> Option<u32> {
    let status = std::fs::read_to_string(proc_dir.join("status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("NSpid:"))?;
    line.split_whitespace().last()?.parse().ok()
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
        let e =
            parse_registry_json(r#"{"sessionId":"s1","model":"anthropic/claude-opus-4"}"#).unwrap();
        assert_eq!(e.model.as_deref(), Some("anthropic/claude-opus-4"));
        // Absent -> None (older/unknown producer).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.model, None);
        // Non-string -> None (never a garbled value).
        let e = parse_registry_json(r#"{"sessionId":"s3","model":42}"#).unwrap();
        assert_eq!(e.model, None);
    }

    #[test]
    fn context_fields_parse_and_default_none() {
        let json = r#"{"sessionId":"s1","entries":42,"contextPercent":12,"contextAge":"3d"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, Some(42));
        assert_eq!(e.context_percent, Some(12));
        assert_eq!(e.context_age.as_deref(), Some("3d"));
        // Absent -> None (older/unknown producer, or an adapter that never reports it).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.entries, None);
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age, None);
        // contextPercent can be legitimately absent (unknown estimate) even when
        // entries/contextAge are present.
        let json = r#"{"sessionId":"s3","entries":7,"contextAge":"5m"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, Some(7));
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age.as_deref(), Some("5m"));
        // Non-numeric entries/contextPercent or non-string contextAge -> None,
        // never a garbled value.
        let json = r#"{"sessionId":"s4","entries":"lots","contextPercent":"high","contextAge":9}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, None);
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age, None);
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
        // Legacy filename (no record pid) still resolves label+pid for a
        // record written before pid/label moved into the JSON.
        let sock = live_socket(&e).unwrap();
        assert_eq!(sock.label, "pi");
        assert_eq!(sock.pid, Some(42));
        assert_eq!(sock.pid_namespace, None);
        assert_eq!(sock.path, PathBuf::from("/tmp/p/.corral/pi-42.sock"));
    }

    #[test]
    fn live_socket_reads_pid_label_ns_from_record() {
        // New shape: <sessionId>.sock filename, structured fields in the record.
        let json = r#"{"sessionId":"abc","label":"pi","pid":42,"pidNamespace":4026532999,
            "socket":"/tmp/p/.corral/abc.sock"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.pid, Some(42));
        assert_eq!(e.pid_namespace, Some(4026532999));
        let sock = live_socket(&e).unwrap();
        assert_eq!(sock.label, "pi");
        assert_eq!(sock.pid, Some(42));
        assert_eq!(sock.pid_namespace, Some(4026532999));
        assert_eq!(sock.path, PathBuf::from("/tmp/p/.corral/abc.sock"));
    }

    #[test]
    fn live_socket_without_pid_is_still_watched_uncorrelatable() {
        // A <sessionId>.sock with no record pid: still a live socket (watched),
        // just not window-correlatable (pid None).
        let e = parse_registry_json(
            r#"{"sessionId":"abc","label":"pi","socket":"/tmp/p/.corral/abc.sock"}"#,
        )
        .unwrap();
        let sock = live_socket(&e).unwrap();
        assert_eq!(sock.label, "pi");
        assert_eq!(sock.pid, None);
    }

    #[test]
    fn live_socket_missing_label_defaults_to_agent() {
        let e = parse_registry_json(
            r#"{"sessionId":"abc","pid":9,"socket":"/tmp/p/.corral/abc.sock"}"#,
        )
        .unwrap();
        assert_eq!(live_socket(&e).unwrap().label, "agent");
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

    struct FakeProc {
        self_ino: Option<u64>,
        procs: Vec<ProcInfo>,
    }
    impl ProcTable for FakeProc {
        fn self_pid_ns_ino(&self) -> Option<u64> {
            self.self_ino
        }
        fn processes(&self) -> Vec<ProcInfo> {
            self.procs.clone()
        }
    }

    fn proc(host_pid: u32, ns: u64, nspid: u32) -> ProcInfo {
        ProcInfo {
            host_pid,
            pid_ns_ino: ns,
            deepest_nspid: nspid,
        }
    }

    #[test]
    fn resolve_translates_namespaced_pid_to_host_pid() {
        // Two sandboxes each have a process with namespace-local pid 7; the
        // namespace inode disambiguates, and NSpid picks the process within.
        let table = FakeProc {
            self_ino: Some(1000), // corral's own ns, unrelated to either sandbox
            procs: vec![
                proc(34521, 5001, 7), // sandbox A
                proc(34600, 5002, 7), // sandbox B, same namespaced pid
                proc(34700, 5001, 9), // sandbox A, different process
            ],
        };
        assert_eq!(resolve_host_pid(&table, 7, 5001), Some(34521));
        assert_eq!(resolve_host_pid(&table, 7, 5002), Some(34600));
        assert_eq!(resolve_host_pid(&table, 9, 5001), Some(34700));
        // No process with that (ns, nspid) pair.
        assert_eq!(resolve_host_pid(&table, 8, 5001), None);
        assert_eq!(resolve_host_pid(&table, 7, 9999), None);
    }

    #[test]
    fn resolve_shortcuts_when_target_shares_reader_namespace() {
        // Agent in the reader's own PID namespace: the pid is already host-level,
        // returned without scanning (works even with an empty process list).
        let table = FakeProc {
            self_ino: Some(1000),
            procs: vec![],
        };
        assert_eq!(resolve_host_pid(&table, 42, 1000), Some(42));
    }

    #[test]
    fn resolve_socket_host_pid_handles_optionals() {
        let table = FakeProc {
            self_ino: Some(1000),
            procs: vec![proc(34521, 5001, 7)],
        };
        // No pid -> not correlatable.
        assert_eq!(resolve_socket_host_pid(&table, None, Some(5001)), None);
        // No namespace -> pid is already host-level (pre-bridge behavior).
        assert_eq!(resolve_socket_host_pid(&table, Some(999), None), Some(999));
        // Both present -> translated.
        assert_eq!(resolve_socket_host_pid(&table, Some(7), Some(5001)), Some(34521));
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
