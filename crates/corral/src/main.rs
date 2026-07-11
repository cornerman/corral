//! corral: list locally running ACP agent sessions.
//!
//! Discovery: scan $XDG_RUNTIME_DIR/acp/ for `<label>-<pid>.sock` files
//! created by agentwrap (or anything else following the convention). Each
//! socket is probed with an ACP `initialize` request to learn the agent's
//! identity and liveness. Default mode re-renders the table every second;
//! `--once` prints a single snapshot.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod discovery;
use discovery::SocketEntry;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
enum Status {
    /// Agent answered our initialize probe. Title/cwd come from an
    /// optional session/list follow-up; agents without it yield None.
    Live {
        agent: String,
        title: Option<String>,
        cwd: Option<String>,
    },
    /// Socket accepted the connection but closed it: another client is
    /// driving this session (agentwrap allows one client at a time).
    Busy,
    /// Connection failed: the wrapper is gone but its socket file remains.
    Stale,
}

fn main() {
    let once = std::env::args().any(|a| a == "--once");
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        eprintln!("corral: XDG_RUNTIME_DIR is not set");
        std::process::exit(1);
    };
    let dir = runtime_dir.join("acp");

    loop {
        let mut entries = discovery::scan(&dir);
        entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.pid.cmp(&b.pid)));
        let rows: Vec<(SocketEntry, Status)> = entries
            .into_iter()
            .map(|e| {
                let status = probe(&e.path);
                (e, status)
            })
            .collect();

        if !once {
            // Clear screen and move cursor home for a live-updating view.
            print!("\x1b[2J\x1b[H");
        }
        render(&rows);

        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn render(rows: &[(SocketEntry, Status)]) {
    println!(
        "{:<16} {:>8} {:<8} {:<18} {:<28} CWD",
        "SESSION", "PID", "STATUS", "AGENT", "TITLE"
    );
    if rows.is_empty() {
        println!("(no agent sockets found)");
        return;
    }
    for (entry, status) in rows {
        let (status_str, agent, title, cwd) = match status {
            Status::Live { agent, title, cwd } => (
                "live",
                agent.as_str(),
                title.as_deref().unwrap_or("-"),
                cwd.as_deref().unwrap_or("-"),
            ),
            Status::Busy => ("busy", "-", "-", "-"),
            Status::Stale => ("stale", "-", "-", "-"),
        };
        println!(
            "{:<16} {:>8} {:<8} {:<18} {:<28} {}",
            entry.label, entry.pid, status_str, agent, title, cwd
        );
    }
}

/// Probe a socket with an ACP initialize request. Read-only reconnaissance:
/// we disconnect right after the response, leaving the agent free for real
/// clients (agentwrap keeps the child alive across disconnects).
fn probe(path: &Path) -> Status {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return Status::Stale;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "corral", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    if stream
        .write_all((request.to_string() + "\n").as_bytes())
        .is_err()
    {
        return Status::Busy;
    }

    // Scan JSONL responses for the reply to id 0, skipping any notifications
    // the agent may emit first. EOF right away means agentwrap kicked us
    // because another client is connected.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return Status::Busy,
            Ok(_) => {
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if msg.get("id") == Some(&serde_json::json!(0)) {
                    let agent = describe_agent(&msg);
                    let (title, cwd) = query_session(&mut reader);
                    return Status::Live { agent, title, cwd };
                }
            }
        }
    }
}

/// Best-effort session/list follow-up on the already-open probe stream.
/// Agents without the method (or that never answer within the read
/// timeout) simply yield no title -- the row stays useful either way.
fn query_session(reader: &mut BufReader<UnixStream>) -> (Option<String>, Option<String>) {
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/list", "params": {}
    });
    if reader
        .get_mut()
        .write_all((request.to_string() + "\n").as_bytes())
        .is_err()
    {
        return (None, None);
    }
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return (None, None),
            Ok(_) => {
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if msg.get("id") != Some(&serde_json::json!(1)) {
                    continue;
                }
                let session = &msg["result"]["sessions"][0];
                return (
                    session["title"].as_str().map(String::from),
                    session["cwd"].as_str().map(String::from),
                );
            }
        }
    }
}

/// Human-readable agent identity from an initialize response.
fn describe_agent(response: &serde_json::Value) -> String {
    let info = &response["result"]["agentInfo"];
    match (info["name"].as_str(), info["version"].as_str()) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name.to_string(),
        // Older agents omit agentInfo; the protocol version is all we know.
        _ => match response["result"]["protocolVersion"].as_i64() {
            Some(v) => format!("(acp v{v})"),
            None => "(unknown)".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn describe_agent_with_info() {
        let resp = serde_json::json!({
            "id": 0,
            "result": {"protocolVersion": 1, "agentInfo": {"name": "pi", "version": "0.80.3"}}
        });
        assert_eq!(describe_agent(&resp), "pi 0.80.3");
    }

    #[test]
    fn describe_agent_without_info() {
        let resp = serde_json::json!({"id": 0, "result": {"protocolVersion": 1}});
        assert_eq!(describe_agent(&resp), "(acp v1)");
    }

    #[test]
    fn probe_live_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock = dir.path().join("fake-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["method"], "initialize");
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"protocolVersion": 1, "agentInfo": {"name": "fake", "version": "1.0"}}
            });
            let mut w = &stream;
            w.write_all((resp.to_string() + "\n").as_bytes()).unwrap();
            // Answer the session/list follow-up like corral-announce does.
            line.clear();
            reader.read_line(&mut line).unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["method"], "session/list");
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"sessions": [{"sessionId": "s1", "title": "fix bug", "cwd": "/tmp/p"}]}
            });
            w.write_all((resp.to_string() + "\n").as_bytes()).unwrap();
        });
        assert_eq!(
            probe(&sock),
            Status::Live {
                agent: "fake 1.0".to_string(),
                title: Some("fix bug".to_string()),
                cwd: Some("/tmp/p".to_string()),
            }
        );
        server.join().unwrap();
    }

    #[test]
    fn probe_live_agent_without_session_list() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock = dir.path().join("plain-4.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"protocolVersion": 1, "agentInfo": {"name": "fake", "version": "1.0"}}
            });
            let mut w = &stream;
            w.write_all((resp.to_string() + "\n").as_bytes()).unwrap();
            // Close without answering session/list: probe must degrade to no title.
        });
        assert_eq!(
            probe(&sock),
            Status::Live {
                agent: "fake 1.0".to_string(),
                title: None,
                cwd: None,
            }
        );
        server.join().unwrap();
    }

    #[test]
    fn probe_stale_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock = dir.path().join("gone-2.sock");
        // Bind and drop: the file exists but nothing listens.
        drop(UnixListener::bind(&sock).unwrap());
        assert_eq!(probe(&sock), Status::Stale);
    }

    #[test]
    fn probe_busy_socket() {
        let dir = tempfile::TempDir::new().unwrap();
        let sock = dir.path().join("busy-3.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // Accept-then-close mimics agentwrap with an active client.
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });
        assert_eq!(probe(&sock), Status::Busy);
        server.join().unwrap();
    }
}
