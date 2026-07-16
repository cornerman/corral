//! The control socket: how a sandboxed agent submits a cross-session message.
//! corral binds `~/.corral/corrald.sock` (its `~/.corral` is on the agent
//! sandbox allowlist, so the `corral_message_agent` tool can reach it). The
//! flow per connection is a straight line: read one request line, parse it,
//! find the recipient, ack the verdict, and (if routable) hand the message to
//! the router. Submission thus fails loud when corral is down (the connect
//! fails) instead of piling up a silent file queue.
//!
//! The ack is synchronous and says only what is knowable at once (found /
//! approval_needed / not-found); the actual delivery and the operator approval gate
//! run later in the router. There is no wait for delivery.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use corral_core::discovery;

use crate::mailbox::{self, Message, Target};

/// Whether another daemon is already serving this socket. A successful connect
/// proves a live listener; a connect failure means the socket is absent or
/// stale (a crashed prior run). This is the singleton guard: exactly one
/// corrald may own the control socket, unlike the multi-launchable board.
pub fn is_serving(socket: &Path) -> bool {
    UnixStream::connect(socket)
        .map(|s| {
            let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
        })
        .is_ok()
}

/// Bind the control socket and serve it on a background thread. Routable
/// messages are sent on `tx` for the main loop to enqueue into the router.
/// Fails loud on a bind error: the daemon's whole job is this socket, so it
/// must not run useless. Call `is_serving` first to reject a second daemon.
pub fn serve(
    socket: PathBuf,
    registry_dir: PathBuf,
    whitelist: PathBuf,
    tx: Sender<Message>,
) -> std::io::Result<()> {
    // Reclaim a stale socket from a crashed prior run, then bind. 0700 dir:
    // directory permissions are the only peer authentication.
    let _ = std::fs::remove_file(&socket);
    if let Some(parent) = socket.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            handle(conn, &registry_dir, &whitelist, &tx);
        }
    });
    Ok(())
}

/// One connection: read a request line, ack the verdict, enqueue if routable.
fn handle(conn: UnixStream, registry_dir: &Path, whitelist: &Path, tx: &Sender<Message>) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut conn = reader.into_inner();
    // A read-only roster query is answered synchronously and never routed. It
    // is ungated (any session is messageable subject to operator approval), but
    // the roster withholds an unreachable directory's cwd and description,
    // exposing only its sessionId as an addressable handle.
    if let Some(from_cwd) = mailbox::parse_list(line.trim()) {
        let entries = discovery::scan_registry(registry_dir);
        let visible =
            |cwd: &str| cwd == from_cwd || mailbox::is_whitelisted(whitelist, &from_cwd, cwd);
        let roster = mailbox::build_roster(&entries, visible);
        let _ = writeln!(conn, "{}", mailbox::roster_json(&roster));
        return;
    }
    let Some(msg) = mailbox::parse_message(line.trim()) else {
        let _ = ack(&mut conn, "malformed");
        return;
    };
    let target_cwd = resolve(&msg.target, registry_dir);
    let whitelisted = target_cwd
        .as_deref()
        .is_some_and(|t| mailbox::is_whitelisted(whitelist, &msg.from_cwd, t));
    let verdict = mailbox::classify(&msg.target, target_cwd.as_deref(), whitelisted, msg.hidden);
    let _ = ack(&mut conn, verdict.wire());
    if verdict.routable() {
        let _ = tx.send(msg);
    }
}

/// Resolve the recipient's directory: a session's cwd from the registry, or an
/// existing target directory. `None` means "no recipient found" (the ack then
/// reports why, per target kind).
fn resolve(target: &Target, registry_dir: &Path) -> Option<String> {
    match target {
        Target::Dir(d) => Path::new(d).is_dir().then(|| d.clone()),
        Target::Session(sid) => discovery::scan_registry(registry_dir)
            .into_iter()
            .find(|e| &e.session_id == sid)
            .and_then(|e| e.cwd),
    }
}

fn ack(conn: &mut UnixStream, status: &str) -> std::io::Result<()> {
    writeln!(conn, "{{\"status\":\"{status}\"}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;

    /// Connect, send one request line, return the ack line.
    fn submit(socket: &Path, body: &str) -> String {
        let mut c = UnixStream::connect(socket).unwrap();
        c.write_all(format!("{body}\n").as_bytes()).unwrap();
        let mut buf = String::new();
        c.read_to_string(&mut buf).unwrap();
        buf.trim().to_string()
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("corrald.sock");
        let registry = tmp.path().join("registry");
        std::fs::create_dir(&registry).unwrap();
        let whitelist = tmp.path().join("whitelist");
        (tmp, socket, registry, whitelist)
    }

    fn write_registry(dir: &Path, sid: &str, cwd: &str) {
        std::fs::write(
            dir.join(format!("{sid}.json")),
            format!(r#"{{"sessionId":"{sid}","cwd":"{cwd}","label":"pi"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn accepted_session_is_acked_and_enqueued() {
        let (tmp, socket, registry, whitelist) = setup();
        write_registry(&registry, "sid-7", tmp.path().to_str().unwrap());
        mailbox::whitelist_add(&whitelist, "/a", tmp.path().to_str().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {} // wait for bind

        let ack = submit(
            &socket,
            r#"{"id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"accepted"}"#);
        assert_eq!(rx.recv().unwrap().id, "1", "routable -> enqueued");
    }

    #[test]
    fn unknown_session_is_recipient_not_found_and_not_enqueued() {
        let (_tmp, socket, registry, whitelist) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            r#"{"id":"1","fromCwd":"/a","targetSession":"ghost","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"recipient_not_found"}"#);
        assert!(rx.try_recv().is_err(), "rejected -> not enqueued");
    }

    #[test]
    fn missing_directory_is_directory_not_known() {
        let (_tmp, socket, registry, whitelist) = setup();
        let (tx, _rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            r#"{"id":"1","fromCwd":"/a","targetDir":"/no/such/dir","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"directory_not_known"}"#);
    }

    #[test]
    fn resolvable_but_unlisted_needs_approval_and_still_enqueued() {
        let (tmp, socket, registry, whitelist) = setup();
        let dir = tmp.path().to_str().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &format!(r#"{{"id":"1","fromCwd":"/a","targetDir":"{dir}","message":"hi"}}"#),
        );
        assert_eq!(ack, r#"{"status":"approval_needed"}"#);
        assert_eq!(
            rx.recv().unwrap().id,
            "1",
            "approval_needed still routes (for approval)"
        );
    }

    #[test]
    fn list_query_exposes_whitelisted_dir_and_hides_unreachable_paths() {
        let (tmp, socket, registry, whitelist) = setup();
        let reachable = tmp.path().join("reach");
        std::fs::create_dir(&reachable).unwrap();
        let reach = reachable.to_str().unwrap();
        write_registry(&registry, "visible-1", reach);
        write_registry(&registry, "hidden-1", "/secret/dir");
        mailbox::whitelist_add(&whitelist, "/caller", reach).unwrap();
        let (tx, _rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let reply = submit(&socket, r#"{"op":"list","fromCwd":"/caller"}"#);
        assert!(reply.contains("\"status\":\"ok\""));
        // The whitelisted dir is fully exposed and addressable.
        assert!(reply.contains("visible-1") && reply.contains(reach));
        // An unreachable session is still addressable by its id, but its path
        // stays hidden.
        assert!(reply.contains("hidden-1"), "sessionId is the addressable handle");
        assert!(!reply.contains("/secret/dir"), "never leak an unreachable cwd");
    }

    #[test]
    fn malformed_is_acked_without_enqueue() {
        let (_tmp, socket, registry, whitelist) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        assert_eq!(submit(&socket, "not json"), r#"{"status":"malformed"}"#);
        assert!(rx.try_recv().is_err());
    }
}
