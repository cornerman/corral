//! One reader thread per socket. Connects, seeds the agent from
//! `initialize` + `session/list`, then streams `session/update` notifications
//! and reports state transitions. The connection stays fully open (never
//! half-closed), so the agent keeps broadcasting to us.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use crate::discovery::SocketEntry;
use crate::model::{Agent, Origin, State, Update};

/// State a fresh, unclassified agent shows as: visible and flagged for the
/// operator rather than silently hidden.
const DEFAULT_STATE: State = State::Idle;

/// Extract a state transition from a single JSONL line, if it is the standard
/// ACP `state_update` session/update (agentclientprotocol.com/rfds/v2/prompt).
/// Pure, so it is unit tested without a socket.
pub fn parse_state_notification(line: &str) -> Option<State> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "state_update" {
        return None;
    }
    State::from_wire(update.get("state")?.as_str()?)
}

/// Extract the current model from an ACP `config_options_update` session/update
/// (agentclientprotocol.com — Session Config Options). Returns the
/// `currentValue` of the option whose `category` is `"model"`, else `None`.
/// corral is display-only, so it reads only the current value and ignores the
/// selectable `options`/`type`. Pure, unit tested.
pub fn parse_config_model(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "config_options_update" {
        return None;
    }
    update
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| o.get("category").and_then(|c| c.as_str()) == Some("model"))
        .and_then(|o| o.get("currentValue"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Extract a title change from a `session_info_update` notification. Returns
/// `Some(new_title)` (which may itself be `None`, meaning cleared) if the line
/// is that notification, else `None`. Pure.
pub fn parse_title_notification(line: &str) -> Option<Option<String>> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "session_info_update" {
        return None;
    }
    Some(
        update
            .get("title")
            .and_then(|t| t.as_str())
            .map(String::from),
    )
}

/// Argument names, in priority order, whose value best summarizes a tool call.
/// Keyed on the argument name rather than the tool name, to stay loosely
/// coupled to any one agent's tool set; an unrecognized tool still shows its
/// name. `question` leads so a blocked agent surfaces what it is asking.
const ARG_KEYS: [&str; 10] = [
    "question",
    "path",
    "file",
    "file_path",
    "filename",
    "command",
    "cmd",
    "pattern",
    "query",
    "message",
];

/// A short summary of a tool call's most salient argument, if one of the known
/// argument names is present. Paths collapse to their last segment; other
/// values (commands, patterns, questions) take their first line. Pure.
fn tool_arg(raw: &serde_json::Value) -> Option<String> {
    for key in ARG_KEYS {
        let Some(s) = raw.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        let short = if matches!(key, "path" | "file" | "file_path" | "filename") {
            s.rsplit('/').next().unwrap_or(s)
        } else {
            s.lines().next().unwrap_or(s)
        };
        return Some(short.to_string());
    }
    None
}

/// Summarize a `tool_call` session/update into a card activity string like
/// "edit model.rs" or "bash cargo test". Returns `None` for other lines. Pure.
pub fn parse_tool_call(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "tool_call" {
        return None;
    }
    let tool = update
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("tool");
    match update.get("rawInput").and_then(tool_arg) {
        Some(arg) => Some(format!("{tool} {arg}")),
        None => Some(tool.to_string()),
    }
}

/// Parse the `session/list` reply into (session_id, title, cwd) for the first
/// session. State is not carried here; it arrives via the `state_update`
/// notification the extension sends on connect. Pure helper for testing.
pub fn parse_session_list(
    msg: &serde_json::Value,
) -> (Option<String>, Option<String>, Option<String>) {
    let s = &msg["result"]["sessions"][0];
    (
        s["sessionId"].as_str().map(String::from),
        s["title"].as_str().map(String::from),
        s["cwd"].as_str().map(String::from),
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
    // The extension writes its connect-time state_update seed BEFORE the
    // session/list reply, so that state line is read before the agent exists
    // in the board. A SetState for an absent socket is dropped, so we stash the
    // latest pre-seed state here and stamp it onto the Upsert instead of
    // DEFAULT_STATE; without this the card is stuck Idle until the next real
    // transition. A stateless agent keeps DEFAULT_STATE.
    let mut seed_state = DEFAULT_STATE;
    // Like seed_state: the extension sends its config_options_update model seed
    // BEFORE the session/list reply, so it is read before the agent exists in
    // the board. A SetModel for an absent socket is dropped, so stash the seed
    // and stamp it onto the Upsert instead. None until the first broadcast.
    let mut seed_model: Option<String> = None;

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
            let (session_id, title, cwd) = parse_session_list(&msg);
            let _ = tx.send(Update::Upsert(Box::new(Agent {
                socket_path: entry.path.clone(),
                pid: entry.pid,
                label: entry.label.clone(),
                session_id,
                title,
                cwd,
                state: seed_state,
                origin: Origin::Live,
                // The socket cannot report launch commands; Board::sync_registry
                // stamps spawn_command from the matching record each scan.
                spawn_command: None,
                resume_command: None,
                activity: None,
                // sync_registry stamps the real value from the record each scan.
                gui: false,
                message_flag: None,
                hidden: false,
                // Seeded from the config_options_update the extension sends on
                // connect (stashed below), else None until the first broadcast.
                model: seed_model.clone(),
                // Clocks start at seed time; Board::apply keeps them across a
                // reconnect and resets state_since on the next transition.
                state_since: std::time::Instant::now(),
                last_activity: std::time::Instant::now(),
            })));
            seeded = true;
            continue;
        }

        // Live state transitions. Before the Upsert (session/list reply) the
        // agent is not in the board yet, so stash the state for the seed rather
        // than emitting a SetState that would be dropped.
        if let Some(state) = parse_state_notification(&line) {
            if seeded {
                let _ = tx.send(Update::SetState(entry.path.clone(), state));
            } else {
                seed_state = state;
            }
            continue;
        }
        // Live model change; before the Upsert stash it for the seed instead of
        // emitting a SetModel that would be dropped.
        if let Some(model) = parse_config_model(&line) {
            if seeded {
                let _ = tx.send(Update::SetModel(entry.path.clone(), model));
            } else {
                seed_model = Some(model);
            }
            continue;
        }
        // Rename: keep the displayed title current without a reconnect.
        if let Some(title) = parse_title_notification(&line) {
            let _ = tx.send(Update::SetTitle(entry.path.clone(), title));
        }
        // Current tool activity: what the agent is doing right now.
        if let Some(activity) = parse_tool_call(&line) {
            let _ = tx.send(Update::SetActivity(entry.path.clone(), activity));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_notification() {
        let running = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"state_update","state":"running"}}}"#;
        assert_eq!(parse_state_notification(running), Some(State::Running));
        let idle = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_state_notification(idle), Some(State::Idle));
        let blocked = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"state_update","state":"requires_action"}}}"#;
        assert_eq!(
            parse_state_notification(blocked),
            Some(State::RequiresAction)
        );
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
    fn parses_title_notification() {
        let renamed = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"session_info_update","title":"fix bug"}}}"#;
        assert_eq!(
            parse_title_notification(renamed),
            Some(Some("fix bug".to_string()))
        );
        let cleared = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"session_info_update","title":null}}}"#;
        assert_eq!(parse_title_notification(cleared), Some(None));
        let state = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_title_notification(state), None);
    }

    #[test]
    fn parses_tool_call_activity() {
        let edit = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"1","title":"edit","status":"in_progress","rawInput":{"path":"/home/u/p/src/model.rs"}}}}"#;
        assert_eq!(parse_tool_call(edit).as_deref(), Some("edit model.rs"));
        let bash = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","title":"bash","rawInput":{"command":"cargo test --all"}}}}"#;
        assert_eq!(
            parse_tool_call(bash).as_deref(),
            Some("bash cargo test --all")
        );
        // A blocked agent surfaces its question text.
        let q = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","title":"question","rawInput":{"question":"which db?"}}}}"#;
        assert_eq!(parse_tool_call(q).as_deref(), Some("question which db?"));
        // No known arg: just the tool name.
        let noarg = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","title":"think","rawInput":{}}}}"#;
        assert_eq!(parse_tool_call(noarg).as_deref(), Some("think"));
        // Not a tool_call.
        let state = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_tool_call(state), None);
    }

    #[test]
    fn parses_config_model() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"config_options_update","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"anthropic/claude-opus-4"}]}}}"#;
        assert_eq!(
            parse_config_model(line).as_deref(),
            Some("anthropic/claude-opus-4")
        );
        // No model-category option present -> None.
        let other = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"config_options_update","configOptions":[{"id":"mode","category":"mode","currentValue":"ask"}]}}}"#;
        assert_eq!(parse_config_model(other), None);
        // Not a config_options_update.
        let state = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_config_model(state), None);
        assert_eq!(parse_config_model("not json"), None);
    }

    #[test]
    fn preseed_model_lands_on_upsert() {
        use std::io::Write as _;
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc::channel;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = channel();
        let entry = SocketEntry {
            path: sock.clone(),
            pid: 1,
            label: "pi".into(),
        };
        let h = spawn(entry, tx);
        let (mut conn, _) = listener.accept().unwrap();
        // Model config seed BEFORE the session/list reply (the real order).
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"config_options_update\",\"configOptions\":[{\"id\":\"model\",\"category\":\"model\",\"currentValue\":\"anthropic/claude-opus-4\"}]}}}\n").unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessions\":[{\"sessionId\":\"s\",\"title\":\"t\",\"cwd\":\"/tmp\"}]}}\n").unwrap();
        let upsert = loop {
            match rx.recv().unwrap() {
                Update::Upsert(a) => break a,
                _ => continue,
            }
        };
        assert_eq!(upsert.model.as_deref(), Some("anthropic/claude-opus-4"));
        drop(conn);
        let _ = h.join();
    }

    #[test]
    fn preseed_state_lands_on_upsert() {
        use std::io::Write as _;
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc::channel;
        // A live agent that is Running when the board connects: the extension
        // seeds state_update BEFORE the session/list reply. The Upsert must
        // carry Running, not the Idle default (the ordering-race regression).
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = channel();
        let entry = SocketEntry {
            path: sock.clone(),
            pid: 1,
            label: "pi".into(),
        };
        let h = spawn(entry, tx);
        let (mut conn, _) = listener.accept().unwrap();
        // Seed state first, then the session/list reply (id 1) — the real order.
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"state_update\",\"state\":\"running\"}}}\n").unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessions\":[{\"sessionId\":\"s\",\"title\":\"t\",\"cwd\":\"/tmp\"}]}}\n").unwrap();
        let upsert = loop {
            match rx.recv().unwrap() {
                Update::Upsert(a) => break a,
                _ => continue,
            }
        };
        assert_eq!(upsert.state, State::Running);
        drop(conn);
        let _ = h.join();
    }

    #[test]
    fn session_list_seeds_fields() {
        let msg = serde_json::json!({
            "id": 1,
            "result": {"sessions": [{"sessionId": "abc", "title": "fix bug", "cwd": "/tmp/p"}]}
        });
        let (sid, title, cwd) = parse_session_list(&msg);
        assert_eq!(sid.as_deref(), Some("abc"));
        assert_eq!(title.as_deref(), Some("fix bug"));
        assert_eq!(cwd.as_deref(), Some("/tmp/p"));
    }
}
