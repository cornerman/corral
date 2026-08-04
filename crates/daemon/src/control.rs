//! The control socket: how a sandboxed agent submits a cross-session action.
//! corral binds `~/.corral/corrald.sock` (its `~/.corral` is on the agent
//! sandbox allowlist, so the `corral_*` tools can reach it). The flow per
//! connection is a straight line: read one request line, parse the verb
//! (`message` / `spawn` / `stop`, plus the synchronous `list`), find the
//! recipient, ack the verdict, and (if routable) hand the submission to the
//! router. Submission thus fails loud when corral is down (the connect fails)
//! instead of piling up a silent file queue.
//!
//! The ack is synchronous and says only what is knowable at once (found /
//! approval_needed / not-found); the actual delivery and the operator approval gate
//! run later in the router. There is no wait for delivery.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use corral_core::{curation, discovery};

use crate::mailbox::{self, Ack, Submission, Target};

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
    tx: Sender<Submission>,
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
/// Max bytes of one request line. The timeout above bounds only *idle* time per
/// read, so a client that keeps writing never trips it: without a byte cap, one
/// connection grows the read buffer without limit (measured: 256 MiB from a
/// single sender) and 64 concurrent handlers multiply it into an OOM kill of the
/// singleton broker. A request is a `{"submit":"<path>"}` envelope, so this is
/// generous by orders of magnitude (T15).
const MAX_REQUEST: u64 = 8 * 1024;

/// One connection: read a request line, ack the verdict, enqueue if routable.
fn handle(conn: UnixStream, registry_dir: &Path, whitelist: &Path, tx: &Sender<Submission>) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    // Byte-capped read: a line that hits the cap without terminating is refused
    // as malformed rather than buffered further.
    let read = (&mut reader).take(MAX_REQUEST).read_line(&mut line);
    let mut conn = reader.into_inner();
    match read {
        Ok(n) if n as u64 == MAX_REQUEST && !line.ends_with('\n') => {
            let _ = reject(&mut conn, "the request line exceeds the size cap");
            return;
        }
        Ok(_) => {}
        Err(_) => return,
    }
    // Every request rides a submission envelope (`{"submit":"<outbox path>"}`):
    // corrald opens the file and derives the trusted `fromCwd` from where it
    // physically lives, so a self-reported sender cannot be forged (T2-send).
    let Some(path) = mailbox::parse_submit(line.trim()) else {
        let _ = reject(
            &mut conn,
            "the request is not a {\"submit\":\"<path>\"} envelope",
        );
        return;
    };
    // resolve_submission reads AND consumes the file, deriving the trusted
    // facts from the fd it holds; the raw `path` is never touched again.
    let (from_cwd, content) = match curation::resolve_submission(Path::new(&path)) {
        Ok(resolved) => resolved,
        Err(why) => {
            // The sender is unknown here (deriving it is what just failed), so
            // the path is all the operator gets; `{path:?}` keeps hostile text
            // from mangling the journal line.
            eprintln!("corrald: refused submission {path:?}: {why}");
            let _ = reject(&mut conn, &why.to_string());
            return;
        }
    };

    // A read-only roster query, answered synchronously and never routed. The
    // `fromCwd` is the authenticated one, so an agent cannot widen its roster
    // view by claiming another directory.
    if mailbox::is_list(&content) {
        let entries = discovery::scan_registry(registry_dir);
        let visible = |cwd: &str| mailbox::reachable(whitelist, &from_cwd, cwd);
        let roster = mailbox::build_roster(&entries, visible);
        let _ = writeln!(conn, "{}", mailbox::roster_json(&roster));
        return;
    }
    let Some(mut sub) = mailbox::parse(&content) else {
        let _ = reject(&mut conn, "the submission is not a valid corral request");
        return;
    };
    sub.from_cwd = from_cwd; // authenticated, overrides any content fromCwd
    let entries = discovery::scan_registry(registry_dir);
    // T2: the reply handle is checkable even though it is self-reported, because
    // the directory it must belong to is authenticated. A handle the registry
    // pins to another directory is a forgery, so refuse the whole submission
    // rather than deliver a tag that misdirects the recipient's reply. Checked
    // for every verb, including a stop that delivers no text: a forged handle is
    // evidence about the sender, not about the payload.
    if let Some(sid) = sub.from_session.as_deref() {
        if mailbox::session_claims_other_dir(&entries, sid, &sub.from_cwd) {
            let _ = reject(
                &mut conn,
                "the reply handle names a session in another directory",
            );
            return;
        }
    }
    // One authorization step for every verb (message, spawn, stop), including a
    // stop's `already_stopped` no-op: `authorize` resolves the target, reads the
    // whitelist, and returns the verdict plus the canonical target dir to stamp.
    let (target_cwd, verdict) = mailbox::authorize(whitelist, &sub, &entries);
    ack_and_route(&mut conn, sub, target_cwd, verdict, tx);
}

/// Ack the verdict, then enqueue the submission only if its target resolved,
/// stamping the authenticated `target_cwd` beside the authenticated `from_cwd`
/// — from here on the authorized pair is fixed and nothing re-derives it.
///
/// An unresolved target can still be acked `approval_needed`: that is the
/// disclosure gate (T19) hiding from an unreachable caller whether the directory
/// exists. Such a submission has nowhere to go, so it is dropped here rather
/// than parked under an empty label, and the drop goes to corrald's journal so
/// the operator can see a typo that produced no popup.
fn ack_and_route(
    conn: &mut UnixStream,
    mut sub: Submission,
    target_cwd: Option<String>,
    verdict: Ack,
    tx: &Sender<Submission>,
) {
    let _ = ack(conn, verdict.wire());
    if !verdict.routable() {
        return;
    }
    match target_cwd {
        Some(cwd) => {
            sub.target_cwd = cwd;
            let _ = tx.send(sub);
        }
        None => eprintln!(
            "corrald: dropped submission to {} from {} (target does not resolve)",
            match sub.target() {
                Target::Dir(d) => format!("dir {d}"),
                Target::Session(s) => format!("session {s}"),
            },
            sub.from_cwd
        ),
    }
}

fn ack(conn: &mut UnixStream, status: &str) -> std::io::Result<()> {
    writeln!(conn, "{{\"status\":\"{status}\"}}")
}

/// Ack `malformed` with the reason attached, so the blocked agent learns why
/// its own submission was refused instead of reading a bare verdict. The reason
/// is about the caller's own request only, so it discloses nothing about other
/// agents or the host filesystem.
fn reject(conn: &mut UnixStream, reason: &str) -> std::io::Result<()> {
    writeln!(
        conn,
        "{{\"status\":\"malformed\",\"reason\":{}}}",
        json_string(reason)
    )
}

/// Minimal JSON string escaping for a reason we author ourselves (quotes and
/// backslashes only; the reasons carry no control characters).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
        assert!(
            matches!(routed.kind, mailbox::Kind::Stop { .. }),
            "routed as a kill"
        );
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
    fn t2_forged_reply_handle_from_another_dir_is_refused() {
        // A sender in `from` claims a reply handle that the registry pins to a
        // different directory: provable forgery, so the message never routes.
        let (tmp, socket, registry, whitelist, from) = setup();
        let victim = tmp.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        write_registry(&registry, "sid-victim", victim.to_str().unwrap());
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        mailbox::whitelist_add(&whitelist, &from_str(&from), target.to_str().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            &format!(
                r#"{{"op":"spawn","id":"1","fromCwd":"/a","fromSession":"sid-victim","cwd":"{}","task":"hi"}}"#,
                target.to_str().unwrap()
            ),
        );
        assert_malformed(&ack, "reply handle");
        assert!(rx.try_recv().is_err(), "a forged handle must not route");
    }

    #[test]
    fn t2_unknown_reply_handle_still_routes() {
        // An id absent from the registry is not evidence of forgery: a fresh
        // record may not be curated yet, so a legitimate first message survives.
        let (tmp, socket, registry, whitelist, from) = setup();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        mailbox::whitelist_add(&whitelist, &from_str(&from), target.to_str().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            &format!(
                r#"{{"op":"spawn","id":"1","fromCwd":"/a","fromSession":"not-curated-yet","cwd":"{}","task":"hi"}}"#,
                target.to_str().unwrap()
            ),
        );
        assert_eq!(ack, r#"{"status":"accepted"}"#);
        assert_eq!(rx.recv().unwrap().id, "1");
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
            r#"{"op":"message","id":"1","fromCwd":"/a","targetSession":"sid-7","message":"hi"}"#,
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
            r#"{"op":"message","id":"1","fromCwd":"/a","targetSession":"ghost","message":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"recipient_not_found"}"#);
        assert!(rx.try_recv().is_err(), "rejected -> not enqueued");
    }

    #[test]
    fn missing_directory_is_not_disclosed_to_an_unreachable_caller() {
        // Whether an arbitrary host path exists is a fact outside the caller's
        // sandbox, so an unwhitelisted pair hears the same `approval_needed`
        // either way and cannot use the ack as an existence oracle (T19).
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            r#"{"op":"spawn","id":"1","cwd":"/no/such/dir","task":"hi"}"#,
        );
        assert_eq!(ack, r#"{"status":"approval_needed"}"#);
        // Nowhere to spawn, so it is dropped rather than parked under an empty
        // target label: the ack hides the reason, the daemon does not invent one.
        assert!(rx.try_recv().is_err(), "unresolved target must not enqueue");
    }

    #[test]
    fn missing_directory_is_directory_not_known_for_a_reachable_caller() {
        // A pair the operator already approved gets the precise diagnosis, so a
        // typo or a deleted project dir stays debuggable where trust exists.
        let (tmp, socket, registry, whitelist, from) = setup();
        let gone = tmp.path().join("gone").display().to_string();
        mailbox::whitelist_add(&whitelist, &from_str(&from), &gone).unwrap();
        let (tx, _rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            &format!(r#"{{"op":"spawn","id":"1","cwd":"{gone}","task":"hi"}}"#),
        );
        assert_eq!(ack, r#"{"status":"directory_not_known"}"#);
    }

    #[test]
    fn a_spawn_cwd_is_stamped_canonical_before_routing() {
        // The queued submission carries the canonicalized target, so the
        // whitelist key, the operator's label, and the spawn cwd are all the real
        // dir -- not the `..`/symlink spelling the sender chose.
        let (tmp, socket, registry, whitelist, from) = setup();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let canon = std::fs::canonicalize(&real).unwrap().display().to_string();
        let sneaky = format!("{}/../real/", real.display());
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let ack = submit(
            &socket,
            &from,
            &format!(r#"{{"op":"spawn","id":"1","cwd":"{sneaky}","task":"hi"}}"#),
        );
        assert_eq!(ack, r#"{"status":"approval_needed"}"#);
        let sub = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(sub.target_cwd, canon);
        assert_eq!(sub.target_label(), canon);
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
            &format!(r#"{{"op":"spawn","id":"1","fromCwd":"/a","cwd":"{dir}","task":"hi"}}"#),
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
    fn an_oversized_request_line_is_refused_without_unbounded_buffering() {
        // The H3 regression: the read timeout bounds idle time, not volume, so a
        // sender that keeps writing could grow the buffer without limit. The cap
        // must refuse it instead, and the daemon must keep serving afterwards.
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let mut c = UnixStream::connect(&socket).unwrap();
        // Far more than MAX_REQUEST, with no newline anywhere.
        let chunk = vec![b'A'; 64 * 1024];
        for _ in 0..16 {
            if c.write_all(&chunk).is_err() {
                break; // the daemon hung up on us: also a refusal
            }
        }
        let mut buf = String::new();
        let _ = c.read_to_string(&mut buf);
        assert!(
            buf.trim().is_empty() || buf.trim().starts_with(r#"{"status":"malformed""#),
            "unexpected ack: {buf}"
        );
        assert!(rx.try_recv().is_err(), "an oversized line must not enqueue");

        // The daemon still serves a legitimate submission afterwards.
        assert_malformed(&submit(&socket, &from, "not json"), "valid corral request");
    }

    #[test]
    fn malformed_is_acked_without_enqueue() {
        let (_tmp, socket, registry, whitelist, from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        assert_malformed(&submit(&socket, &from, "not json"), "valid corral request");
        assert!(rx.try_recv().is_err());
    }

    /// A submission corrald cannot open is the sandbox-private-mount case (an
    /// agent whose workdir lives under a mount only it sees): the ack must name
    /// that cause, since the caller has no other way to learn why it is stuck.
    #[test]
    fn unopenable_submission_is_acked_with_the_reason() {
        let (tmp, socket, registry, whitelist, _from) = setup();
        let (tx, rx) = mpsc::channel();
        serve(socket.clone(), registry, whitelist, tx).unwrap();
        while UnixStream::connect(&socket).is_err() {}

        let missing = tmp.path().join("gone/.corral/outbox/m.json");
        let mut c = UnixStream::connect(&socket).unwrap();
        c.write_all(format!("{{\"submit\":\"{}\"}}\n", missing.display()).as_bytes())
            .unwrap();
        let mut buf = String::new();
        c.read_to_string(&mut buf).unwrap();
        assert_malformed(buf.trim(), "mount namespace corrald cannot see");
        assert!(rx.try_recv().is_err());
    }

    /// Every refusal is `malformed` plus a reason naming what went wrong.
    fn assert_malformed(ack: &str, expect: &str) {
        assert!(
            ack.starts_with(r#"{"status":"malformed","reason":"#),
            "expected a malformed ack with a reason, got: {ack}"
        );
        assert!(ack.contains(expect), "reason lacks {expect:?}: {ack}");
    }
}
