//! Delivering a prompt to a live agent over its ACP socket. This is corral
//! driving an agent on the operator's behalf (message delivery), distinct from
//! the read-only watch connection. Fire-and-forget: the extension injects the
//! message synchronously when it reads the line, so we write and close without
//! waiting for the turn to end.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Deliver `text` as a user message to the agent listening on `socket`. Opens a
/// one-shot connection, writes a `session/prompt` request, and drops it. Closing
/// immediately does not lose the message: buffered bytes reach the peer before
/// the EOF, and the extension injects on the `data` event.
pub fn send_prompt(socket: &Path, text: &str) -> std::io::Result<()> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "session/prompt",
        "params": { "prompt": [{ "type": "text", "text": text }] },
    });
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all((req.to_string() + "\n").as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    #[test]
    fn send_prompt_writes_session_prompt_line() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let s = sock.clone();
        let h = std::thread::spawn(move || send_prompt(&s, "hello there").unwrap());
        let (conn, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(conn).read_line(&mut line).unwrap();
        h.join().unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "session/prompt");
        assert_eq!(v["params"]["prompt"][0]["text"], "hello there");
    }
}
