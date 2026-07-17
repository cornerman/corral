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

use corral_core::{curation, discovery};

use crate::mailbox::{self, Ack, Message, Target};

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
        // Bound concurrent handlers and time out each read, so a flood of
        // connections or a slow/silent client (slowloris) cannot exhaust the
        // daemon or block the accept loop (security design T15). Each
        // connection is handled on its own short-lived thread.
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for conn in listener.incoming().flatten() {
            if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONCURRENT {
                continue; // at capacity: drop (closes the connection)
            }
            let _ = conn.set_read_timeout(Some(READ_TIMEOUT));
            active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (registry_dir, whitelist, tx, active) = (
                registry_dir.clone(),
                whitelist.clone(),
                tx.clone(),
                active.clone(),
            );
            std::thread::spawn(move || {
                handle(conn, &registry_dir, &whitelist, &tx);
                active.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
    });
    Ok(())
}

/// Max concurrent control-socket handlers; beyond this a new connection is
/// dropped, so a connection flood cannot exhaust threads (T15).
const MAX_CONCURRENT: usize = 64;
/// Per-connection read timeout, so a client that connects and never sends a
/// full request line cannot hold a handler open (T15).
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// One connection: read a request line, ack the verdict, enqueue if routable.
fn handle(conn: UnixStream, registry_dir: &Path, whitelist: &Path, tx: &Sender<Message>) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut conn = reader.into_inner();
    // Every request rides a submission envelope (`{"submit":"<outbox path>"}`):
    // corrald opens the file and derives the trusted `fromCwd` from where it
    // physically lives, so a self-reported sender cannot be forged (T2-send).
    let Some(path) = mailbox::parse_submit(line.trim()) else {
        let _ = ack(&mut conn, "malformed");
        return;
    };
    let Some((from_cwd, content)) = curation::resolve_submission(Path::new(&path)) else {
        let _ = ack(&mut conn, "malformed");
        return;
    };
    // The request has been read; remove the outbox file (best-effort).
    let _ = std::fs::remove_file(&path);

    // A read-only roster query, answered synchronously and never routed. The
    // `fromCwd` is the authenticated one, so an agent cannot widen its roster
    // view by claiming another directory.
    if mailbox::is_list(&content) {
        let entries = discovery::scan_registry(registry_dir);
        let visible =
            |cwd: &str| cwd == from_cwd || mailbox::is_whitelisted(whitelist, &from_cwd, cwd);
        let roster = mailbox::build_roster(&entries, visible);
        let _ = writeln!(conn, "{}", mailbox::roster_json(&roster));
        return;
    }
    // A stop submission (`op:"stop"`) kills a live session; gated like a
    // message. Tried before parse_message, whose required `message` field a
    // stop line lacks. `from_cwd` is forced to the authenticated value.
    if let Some(mut msg) = mailbox::parse_stop(&content) {
        msg.from_cwd = from_cwd;
        handle_stop(&mut conn, msg, registry_dir, whitelist, tx);
        return;
    }
    let Some(mut msg) = mailbox::parse_message(&content) else {
        let _ = ack(&mut conn, "malformed");
        return;
    };
    msg.from_cwd = from_cwd; // authenticated, overrides any content fromCwd
    let target_cwd = resolve(&msg.target, registry_dir);
    let whitelisted = target_cwd
        .as_deref()
        .is_some_and(|t| mailbox::is_whitelisted(whitelist, &msg.from_cwd, t));
    let verdict = mailbox::classify(&msg.target, target_cwd.as_deref(), whitelisted, !msg.hidden);
    let _ = ack(&mut conn, verdict.wire());
    if verdict.routable() {
        let _ = tx.send(msg);
    }
}

/// Handle a stop submission. The target is always a session: no record ->
/// `recipient_not_found`; a dormant record (socket cleared) -> `already_stopped`
/// (idempotent no-op, never routed, since nothing is running to kill); a live
/// record -> classify against the whitelist and route the kill on approval.
fn handle_stop(
    conn: &mut UnixStream,
    msg: Message,
    registry_dir: &Path,
    whitelist: &Path,
    tx: &Sender<Message>,
) {
    let Target::Session(sid) = &msg.target else {
        let _ = ack(conn, "malformed");
        return;
    };
    let entry = discovery::scan_registry(registry_dir)
        .into_iter()
        .find(|e| &e.session_id == sid);
    let verdict = match &entry {
        None => Ack::RecipientNotFound,
        // Dormant: nothing to kill, so the stop already succeeded.
        Some(e) if e.socket.is_none() => Ack::AlreadyStopped,
        Some(e) => {
            let whitelisted = e
                .cwd
                .as_deref()
                .is_some_and(|t| mailbox::is_whitelisted(whitelist, &msg.from_cwd, t));
            mailbox::classify(&msg.target, e.cwd.as_deref(), whitelisted, false)
        }
    };
    let _ = ack(conn, verdict.wire());
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

    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// Submit a request the authenticated way: write the request JSON to the
    /// sender's `<from>/.corral/outbox/<id>.json`, send the `{"submit":path}`
    /// envelope, return the ack. corrald derives `fromCwd` from the file's
    /// location, so `from` (a real dir) is the authenticated sender.
    fn submit(socket: &Path, from: &Path, body: &str) -> String {
        let outbox = from.join(".corral").join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let file = outbox.join(format!("m-{n}.json"));
        std::fs::write(&file, body).unwrap();
        let envelope = format!(r#"{{"submit":"{}"}}"#, file.display());
        let mut c = UnixStream::connect(socket).unwrap();
        c.write_all(format!("{envelope}\n").as_bytes()).unwrap();
        let mut buf = String::new();
        c.read_to_string(&mut buf).unwrap();
        buf.trim().to_string()
    }

    /// Returns (tmp, socket, registry, whitelist, from) where `from` is a real
    /// sender directory the outbox lives under.
    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("corrald.sock");
        let registry = tmp.path().join("registry");
        std::fs::create_dir(&registry).unwrap();
        let whitelist = tmp.path().join("whitelist");
        let from = tmp.path().join("from");
        std::fs::create_dir(&from).unwrap();
        (tmp, socket, registry, whitelist, from)
    }

    /// The canonical sender dir string corrald will derive for `from`.
    fn from_str(from: &Path) -> String {
        std::fs::canonicalize(from)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    // The control socket reads corrald's VETTED registry (a plain dir of
    // trusted JSON records), so tests write flat records there directly —
    // standing in for the curator's output.
    fn write_registry(dir: &Path, sid: &str, cwd: &str) {
        std::fs::write(
            dir.join(format!("{sid}.json")),
            format!(r#"{{"sessionId":"{sid}","cwd":"{cwd}","label":"pi"}}"#),
        )
        .unwrap();
    }

    /// A live record: a `socket` is set, so the daemon treats it as live.
    fn write_live_registry(dir: &Path, sid: &str, cwd: &str) {
        std::fs::write(
            dir.join(format!("{sid}.json")),
            format!(
                r#"{{"sessionId":"{sid}","cwd":"{cwd}","label":"pi","socket":"{cwd}/.corral/pi-9.sock"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn stop_live_whitelisted_is_accepted_and_enqueued() {
        let (tmp, socket, registry, whitelist, from) = setup();
        let cwd = tmp.path().to_str().unwrap();
        write_live_registry(&registry, "sid-7", cwd);
        mailbox::whitelist_add(&whitelist, &from_str(&from), cwd).unwrap();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"op":"stop","id":"1","fromCwd":"/a","targetSession":"sid-7"}"#,
        );
        assert_eq!(ack, r#"{"status":"accepted"}"#);
        let routed = rx.recv().unwrap();
        assert_eq!(routed.id, "1");
        assert_eq!(routed.action, mailbox::Action::Stop, "routed as a kill");
    }

    #[test]
    fn stop_live_unlisted_needs_approval() {
        let (tmp, socket, registry, whitelist, from) = setup();
        let cwd = tmp.path().to_str().unwrap();
        write_live_registry(&registry, "sid-7", cwd);
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"op":"stop","id":"1","fromCwd":"/a","targetSession":"sid-7"}"#,
        );
        assert_eq!(ack, r#"{"status":"approval_needed"}"#);
        assert_eq!(
            rx.recv().unwrap().id,
            "1",
            "held for approval, still routed"
        );
    }

    #[test]
    fn stop_dormant_is_already_stopped_and_not_routed() {
        let (tmp, socket, registry, whitelist, from) = setup();
        // A dormant record (no socket): nothing to kill.
        write_registry(&registry, "sid-7", tmp.path().to_str().unwrap());
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"op":"stop","id":"1","fromCwd":"/a","targetSession":"sid-7"}"#,
        );
        assert_eq!(ack, r#"{"status":"already_stopped"}"#);
        assert!(rx.try_recv().is_err(), "no-op success -> not enqueued");
    }

    #[test]
    fn stop_unknown_session_is_recipient_not_found() {
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"op":"stop","id":"1","fromCwd":"/a","targetSession":"ghost"}"#,
        );
        assert_eq!(ack, r#"{"status":"recipient_not_found"}"#);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn accepted_session_is_acked_and_enqueued() {
        let (tmp, socket, registry, whitelist, from) = setup();
        write_registry(&registry, "sid-7", tmp.path().to_str().unwrap());
        mailbox::whitelist_add(&whitelist, &from_str(&from), tmp.path().to_str().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {} // wait for bind

        let ack = submit(
            &socket,
            &from,
            r#"{"id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"accepted"}"#);
        assert_eq!(rx.recv().unwrap().id, "1", "routable -> enqueued");
    }

    #[test]
    fn unknown_session_is_recipient_not_found_and_not_enqueued() {
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"id":"1","fromCwd":"/a","targetSession":"ghost","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"recipient_not_found"}"#);
        assert!(rx.try_recv().is_err(), "rejected -> not enqueued");
    }

    #[test]
    fn missing_directory_is_directory_not_known() {
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, _rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"id":"1","fromCwd":"/a","targetDir":"/no/such/dir","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"directory_not_known"}"#);
    }

    #[test]
    fn resolvable_but_unlisted_needs_approval_and_still_enqueued() {
        let (tmp, socket, registry, whitelist, from) = setup();
        let dir = tmp.path().to_str().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
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
        let (tmp, socket, registry, whitelist, from) = setup();
        let reachable = tmp.path().join("reach");
        std::fs::create_dir(&reachable).unwrap();
        let reach = reachable.to_str().unwrap();
        // An unreachable session lives in a real dir the caller is not
        // whitelisted for; the roster must hide its path.
        let secret = tmp.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        let secret_path = secret.to_str().unwrap();
        write_registry(&registry, "visible-1", reach);
        write_registry(&registry, "hidden-1", secret_path);
        mailbox::whitelist_add(&whitelist, &from_str(&from), reach).unwrap();
        let (tx, _rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let reply = submit(&socket, &from, r#"{"op":"list","fromCwd":"/caller"}"#);
        assert!(reply.contains("\"status\":\"ok\""));
        // The whitelisted dir is fully exposed and addressable.
        assert!(reply.contains("visible-1") && reply.contains(reach));
        // An unreachable session is still addressable by its id, but its path
        // stays hidden.
        assert!(
            reply.contains("hidden-1"),
            "sessionId is the addressable handle"
        );
        assert!(
            !reply.contains(secret_path),
            "never leak an unreachable cwd"
        );
    }

    #[test]
    fn malformed_is_acked_without_enqueue() {
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        assert_eq!(
            submit(&socket, &from, "not json"),
            r#"{"status":"malformed"}"#
        );
        assert!(rx.try_recv().is_err());
    }
}
