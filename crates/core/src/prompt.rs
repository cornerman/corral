//! Delivering a prompt to a live agent over its ACP socket. This is corral
//! driving an agent on the operator's behalf (message delivery), distinct from
//! the read-only watch connection. Fire-and-forget: the extension injects the
//! message synchronously when it reads the line, so we do not wait for the
//! turn to end.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long to keep the connection open after writing. The agent writes its
/// connect-time seed then reads our request; we must stay connected across that
/// whole exchange (see `drain_grace`), not close the instant the first bytes
/// arrive.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Hold the connection open for the whole grace window, draining everything the
/// agent sends, then return (the caller drops the stream, closing it).
///
/// Both reasons are silent-message-loss bugs we hit:
///
/// 1. The agent injects our request only when its event loop reads our line. If
///    we close the instant its first seed bytes arrive (a few ms), we close
///    before that read and the request is dropped. Staying open the grace
///    window lets the agent read us first.
/// 2. The seed is now several writes (a `state_update` line AND a
///    `config_options_update` line since the model feature), more than one read
///    drains. A sender that read one chunk and closed left the agent's second
///    seed write to hit EPIPE; the agent's error handler then destroys the
///    connection, dropping our not-yet-read request with it. Draining every
///    chunk keeps us connected until the agent has finished its seed and read
///    our line.
///
/// Bounded by `DRAIN_GRACE` total, so a chatty agent that streams updates
/// without pause cannot hold us open indefinitely; by the time any update
/// arrives the agent has already read our line, so closing then is harmless.
fn drain_grace(stream: &mut UnixStream) {
    let deadline = Instant::now() + DRAIN_GRACE;
    let mut buf = [0u8; 1024];
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if stream.set_read_timeout(Some(deadline - now)).is_err() {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,    // EOF: the agent closed; nothing left to drain
            Ok(_) => continue, // drained a chunk; more of the seed may follow
            Err(_) => break,   // idle for the rest of the window (or error)
        }
    }
}

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
    drain_grace(&mut stream);
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
    drain_grace(&mut stream);
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
    fn send_prompt_stays_open_while_the_agent_finishes_its_seed() {
        // Regression (silent message loss): the agent writes its connect-time
        // seed in MORE THAN ONE write -- since the model feature, a
        // state_update line AND a config_options_update line. If send_prompt
        // reads a single chunk and closes early, the agent's still-pending
        // second seed write hits EPIPE, its error handler destroys the
        // connection, and our just-delivered (but not yet read) session/prompt
        // is dropped with it. The sender's contract is therefore to stay
        // connected and drain the whole grace window so the agent completes its
        // seed and reads our line first.
        //
        // This models that: the agent writes seed part 1, waits, then writes
        // part 2. Part 2's write succeeds ONLY if the sender kept the
        // connection open (the fix); the buggy single-read sender has already
        // closed, so part 2 fails with a broken pipe.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            // Seed part 1: larger than one 256-byte read.
            let part1 = format!("{{\"seed\":1,\"pad\":\"{}\"}}\n", "A".repeat(300));
            let p1 = w.write_all(part1.as_bytes());
            // The agent's event loop only writes its second seed line a moment
            // later; a sender that closed after one read is gone by now.
            std::thread::sleep(Duration::from_millis(60));
            let part2 = b"{\"seed\":2}\n";
            let p2 = w.write_all(part2);
            // Now read what the sender delivered (buffered; AF_UNIX hands it
            // over even across the sender's later close).
            let mut line = String::new();
            let n = BufReader::new(conn).read_line(&mut line).unwrap_or(0);
            tx.send((p1.is_ok(), p2.is_ok(), n, line)).unwrap();
        });
        send_prompt(&sock, "deliver-me").unwrap();
        let (p1_ok, p2_ok, n, line) = rx.recv().unwrap();
        assert!(p1_ok, "seed part 1 write failed");
        assert!(
            p2_ok,
            "agent's second seed write broke: sender closed early (before the \
             agent finished its seed), which destroys the connection and drops \
             the delivered message"
        );
        assert!(n > 0, "agent never received the delivered line");
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["method"], "session/prompt");
        assert_eq!(v["params"]["prompt"][0]["text"], "deliver-me");
        h.join().unwrap();
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
