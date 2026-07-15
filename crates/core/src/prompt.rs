//! Delivering a prompt to a live agent over its ACP socket. This is corral
//! driving an agent on the operator's behalf (message delivery), distinct from
//! the read-only watch connection. Fire-and-forget: the extension injects the
//! message synchronously when it reads the line, so we do not wait for the
//! turn to end.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// How long to keep the connection open after writing. The agent writes a seed
/// line on connect; if we close instantly that write hits EPIPE and the
/// agent's error handler destroys the connection before our request line is
/// read, silently dropping the message. Holding the read side open until the
/// seed arrives (a few ms locally; this is only the cap) lets the agent inject
/// our message first. Then we close.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Deliver `text` as a user message to the agent listening on `socket`. Opens a
/// one-shot connection, writes a `session/prompt` request, briefly drains, then
/// closes (see `DRAIN_GRACE` for why the drain is required).
pub fn send_prompt(socket: &Path, text: &str) -> std::io::Result<()> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "session/prompt",
        "params": { "prompt": [{ "type": "text", "text": text }] },
    });
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all((req.to_string() + "\n").as_bytes())?;
    stream.flush()?;
    // Keep the connection open until the agent's seed line arrives, so its
    // write does not EPIPE and drop our request unread. One read suffices; the
    // timeout is only a safety cap.
    stream.set_read_timeout(Some(DRAIN_GRACE))?;
    let mut buf = [0u8; 256];
    let _ = stream.read(&mut buf);
    Ok(())
}

/// Cancel the agent's current turn by opening a one-shot connection to its
/// socket and writing a `session/cancel` notification (no id, per ACP). Used by
/// the card-move feature to move a Running or Requires-Action agent to Idle
/// (aborting also unblocks a pending `question`). Fire-and-forget like
/// `send_prompt`, including the same drain grace so the agent's connect-time
/// seed write does not EPIPE and drop our notification unread.
pub fn send_cancel(socket: &Path) -> std::io::Result<()> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {},
    });
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all((req.to_string() + "\n").as_bytes())?;
    stream.flush()?;
    stream.set_read_timeout(Some(DRAIN_GRACE))?;
    let mut buf = [0u8; 256];
    let _ = stream.read(&mut buf);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn send_prompt_writes_session_prompt_line() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let s = sock.clone();
        let h = std::thread::spawn(move || send_prompt(&s, "hello there").unwrap());
        let (conn, _) = listener.accept().unwrap();
        // Mimic the extension's connect-time seed write, so the sender's grace
        // read returns promptly (and the seed write does not EPIPE).
        let mut w = conn.try_clone().unwrap();
        w.write_all(b"{\"seed\":true}\n").unwrap();
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).unwrap();
        h.join().unwrap();
        // The first line the sender wrote is the request (seed came from us).
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "session/prompt");
        assert_eq!(v["params"]["prompt"][0]["text"], "hello there");
    }

    #[test]
    fn send_cancel_writes_session_cancel_line() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let s = sock.clone();
        let h = std::thread::spawn(move || send_cancel(&s).unwrap());
        let (conn, _) = listener.accept().unwrap();
        let mut w = conn.try_clone().unwrap();
        w.write_all(b"{\"seed\":true}\n").unwrap();
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).unwrap();
        h.join().unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "session/cancel");
        // A notification carries no id per ACP.
        assert!(v.get("id").is_none());
    }
}
