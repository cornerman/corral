//! Fetching an agent's full message history over ACP `session/load`
//! (agentclientprotocol.com/protocol/session-setup#loading-sessions): the
//! Agent replays its entire conversation as `session/update` notifications
//! before responding to the `session/load` request itself. This module opens
//! a fresh, throwaway connection (same shape as `watch.rs`'s seed connection
//! and `prompt.rs`'s delivery connection), collects the replayed notifications,
//! and stops the instant the reply to the `session/load` request arrives.
//!
//! v1 `session/load` (not v2's draft `session/resume`+`replayFrom`, which is
//! an unmerged RFD and not implemented anywhere in corral) — see the design
//! doc's rationale.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// Bounds the whole fetch: connect + initialize + session/load + full replay.
/// Generous for a real session's replay, but still finite so a
/// non-conforming or hung agent cannot block the board indefinitely.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Why `fetch_history` failed, so callers can show a specific footer message.
#[derive(Debug)]
pub enum HistoryError {
    /// The agent answered `session/load` with a JSON-RPC error (most likely
    /// method-not-supported, e.g. cursor).
    Unsupported,
    /// No reply arrived within `TIMEOUT`.
    Timeout,
    /// A connection or I/O failure (socket gone, EOF before a reply, etc).
    Io(std::io::Error),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryError::Unsupported => write!(f, "history not supported"),
            HistoryError::Timeout => write!(f, "no reply"),
            HistoryError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for HistoryError {
    fn from(e: std::io::Error) -> Self {
        HistoryError::Io(e)
    }
}

/// Fetch `session_id`'s full history from the live agent at `socket`. Opens a
/// one-shot connection, sends `initialize` (id 0) then `session/load` (id 1,
/// per ACP with `sessionId`, `cwd`, and an empty `mcpServers` — corral asks an
/// already-running session to replay in place, not to actually reconnect MCP
/// servers), then reads lines until either the id-1 reply arrives (success:
/// return every collected notification's `update` value; error: `Unsupported`)
/// or `TIMEOUT` elapses (`Timeout`).
pub fn fetch_history(
    socket: &Path,
    session_id: &str,
    cwd: &str,
) -> Result<Vec<Value>, HistoryError> {
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = UnixStream::connect(socket)?;
    let init = serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}});
    let load = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/load",
        "params": { "sessionId": session_id, "cwd": cwd, "mcpServers": [] },
    });
    stream.write_all((init.to_string() + "\n").as_bytes())?;
    stream.write_all((load.to_string() + "\n").as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut collected = Vec::new();
    let mut line = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HistoryError::Timeout);
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err(HistoryError::Timeout), // closed before replying
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(HistoryError::Timeout)
            }
            Err(e) => return Err(HistoryError::Io(e)),
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("method").and_then(|m| m.as_str()) == Some("session/update") {
            if let Some(update) = msg.get("params").and_then(|p| p.get("update")) {
                collected.push(update.clone());
            }
            continue;
        }
        if msg.get("id").and_then(|i| i.as_i64()) == Some(1) {
            return if msg.get("error").is_some() {
                Err(HistoryError::Unsupported)
            } else {
                Ok(collected)
            };
        }
        // Anything else (e.g. the id-0 initialize reply): ignore.
    }
}

/// Wrap `entries` (the raw replayed `session/update` values from
/// `fetch_history`) plus identifying metadata into one JSON object, write it
/// to a temp file, and launch `xdg-open` on it detached — on the caller's own
/// display, which is why this is the board's job and not the agent's (a hidden
/// agent runs inside a headless cage with no real display; the board always
/// runs on the operator's own). Returns the path written.
pub fn write_and_open(
    agent: &crate::model::Agent,
    entries: Vec<Value>,
) -> std::io::Result<PathBuf> {
    let captured_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let session_id = agent
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let doc = serde_json::json!({
        "sessionId": session_id,
        "cwd": agent.cwd,
        "title": agent.title,
        "capturedAt": captured_at,
        "updates": entries,
    });
    let path =
        std::env::temp_dir().join(format!("corral-history-{session_id}-{captured_at}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
    // Detached like launch.rs's spawns: the board must not block on (or be
    // killed alongside) the viewer program.
    let _ = Command::new("setsid")
        .arg("--fork")
        .arg("xdg-open")
        .arg(&path)
        .spawn();
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn write_and_open_writes_json_with_wrapper_fields() {
        use crate::model::{Agent, Origin, State};
        use std::time::Instant;
        let agent = Agent {
            socket_path: std::path::PathBuf::from("/tmp/x.sock"),
            pid: 1,
            label: "pi".into(),
            session_id: Some("sess-1".into()),
            title: Some("fix bug".into()),
            cwd: Some("/tmp/proj".into()),
            state: State::Idle,
            origin: Origin::Live,
            spawn_command: None,
            resume_command: None,
            activity: None,
            gui: false,
            message_flag: None,
            hidden: false,
            model: None,
            state_since: Instant::now(),
            last_activity: Instant::now(),
        };
        let entries = vec![serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}})];
        let path = super::write_and_open(&agent, entries).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(v["sessionId"], "sess-1");
        assert_eq!(v["updates"][0]["sessionUpdate"], "agent_message_chunk");
        assert!(v.get("capturedAt").is_some());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn fetch_history_collects_notifications_then_stops_at_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hi\"}}}}\n").unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hello\"}}}}\n").unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n").unwrap();
        });
        let entries = super::fetch_history(&sock, "s1", "/tmp/proj").unwrap();
        h.join().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["sessionUpdate"], "user_message_chunk");
        assert_eq!(entries[1]["sessionUpdate"], "agent_message_chunk");
    }

    #[test]
    fn fetch_history_reports_unsupported_on_error_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("cursor-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"method not supported by corral-cursor: session/load\"}}\n").unwrap();
        });
        let err = super::fetch_history(&sock, "s1", "/tmp/proj").unwrap_err();
        h.join().unwrap();
        assert!(matches!(err, super::HistoryError::Unsupported));
    }

    #[test]
    fn fetch_history_times_out_when_agent_never_replies() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("silent-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(6));
        });
        let err = super::fetch_history(&sock, "s1", "/tmp/proj").unwrap_err();
        assert!(matches!(err, super::HistoryError::Timeout));
        drop(h); // outlives the test (sleeps 6s); do not join
    }
}
