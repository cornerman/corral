//! One reader thread per socket. Connects, seeds the agent from
//! `initialize` + `session/list`, then streams `session/update` notifications
//! and reports state transitions. The connection stays fully open (never
//! half-closed), so the agent keeps broadcasting to us.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::discovery::SocketEntry;
use crate::model::{Agent, State, Update};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// State a fresh, unclassified agent shows as: visible and flagged for the
/// operator rather than silently hidden.
const DEFAULT_STATE: State = State::NeedsYou;

/// Extract a state transition from a single JSONL line, if it is a
/// `session/update` carrying our `session_state` update. Pure, so it is unit
/// tested without a socket.
pub fn parse_state_notification(line: &str) -> Option<State> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "session_state" {
        return None;
    }
    State::from_wire(update.get("state")?.as_str()?)
}

/// Parse the `session/list` reply into (session_id, title, cwd, state) for the
/// first session. Pure helper for testing.
pub fn parse_session_list(
    msg: &serde_json::Value,
) -> (Option<String>, Option<String>, Option<String>, State) {
    let s = &msg["result"]["sessions"][0];
    let state = s["state"]
        .as_str()
        .and_then(State::from_wire)
        .unwrap_or(DEFAULT_STATE);
    (
        s["sessionId"].as_str().map(String::from),
        s["title"].as_str().map(String::from),
        s["cwd"].as_str().map(String::from),
        state,
    )
}

/// Spawn the reader thread for one socket. It owns the connection for the
/// agent's lifetime and sends `Gone` when the socket closes.
pub fn spawn(entry: SocketEntry, tx: Sender<Update>) -> JoinHandle<()> {
    thread::spawn(move || {
        if run(&entry, &tx).is_none() {
            let _ = tx.send(Update::Gone(entry.path.clone()));
        }
    })
}

/// Returns `Some(())` only on a clean end (which does not currently happen: the
/// loop runs until the socket closes, then returns `None` so the caller reports
/// the agent gone).
fn run(entry: &SocketEntry, tx: &Sender<Update>) -> Option<()> {
    let stream = UnixStream::connect(&entry.path).ok()?;
    stream.set_read_timeout(None).ok()?;

    // initialize + session/list on one connection.
    let mut w = stream.try_clone().ok()?;
    let init = serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}});
    let list = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/list","params":{}});
    w.write_all((init.to_string() + "\n").as_bytes()).ok()?;
    w.write_all((list.to_string() + "\n").as_bytes()).ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut seeded = false;

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return None, // socket closed -> agent gone
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        // Seed once from the session/list reply (id 1).
        if !seeded && msg.get("id") == Some(&serde_json::json!(1)) {
            let (session_id, title, cwd, state) = parse_session_list(&msg);
            let _ = tx.send(Update::Upsert(Agent {
                socket_path: entry.path.clone(),
                pid: entry.pid,
                label: entry.label.clone(),
                session_id,
                title,
                cwd,
                state,
            }));
            seeded = true;
            continue;
        }

        // Live state transitions.
        if let Some(state) = parse_state_notification(&line) {
            let _ = tx.send(Update::SetState(entry.path.clone(), state));
        }
    }
}

// CONNECT_TIMEOUT is reserved for a future non-blocking connect; documented to
// avoid an unused warning while the blocking connect is used.
#[allow(dead_code)]
const _: Duration = CONNECT_TIMEOUT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_notification() {
        let working = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"session_state","state":"working"}}}"#;
        assert_eq!(parse_state_notification(working), Some(State::Working));
        let idle = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"session_state","state":"idle"}}}"#;
        assert_eq!(parse_state_notification(idle), Some(State::NeedsYou));
    }

    #[test]
    fn ignores_other_notifications() {
        let chunk = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#;
        assert_eq!(parse_state_notification(chunk), None);
        assert_eq!(parse_state_notification("not json"), None);
        let reply = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        assert_eq!(parse_state_notification(reply), None);
    }

    #[test]
    fn session_list_seeds_fields_and_defaults_state() {
        let msg = serde_json::json!({
            "id": 1,
            "result": {"sessions": [{"sessionId": "abc", "title": "fix bug", "cwd": "/tmp/p", "state": "working"}]}
        });
        let (sid, title, cwd, state) = parse_session_list(&msg);
        assert_eq!(sid.as_deref(), Some("abc"));
        assert_eq!(title.as_deref(), Some("fix bug"));
        assert_eq!(cwd.as_deref(), Some("/tmp/p"));
        assert_eq!(state, State::Working);
    }

    #[test]
    fn session_list_without_state_defaults_needs_you() {
        let msg = serde_json::json!({
            "id": 1, "result": {"sessions": [{"sessionId": "abc"}]}
        });
        let (_, _, _, state) = parse_session_list(&msg);
        assert_eq!(state, DEFAULT_STATE);
    }
}
