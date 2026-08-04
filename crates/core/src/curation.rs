//! Registry curation: corrald's parsing boundary (security design Mechanism 1).
//!
//! corrald is the single reader of the agent-writable raw index and the single
//! writer of the vetted `state/registry/` the viewers read. "Parse, don't
//! validate": untrusted per-workdir records in, trusted records out.
//!
//! - The raw index (`~/.corral/registry`) is a newline-delimited list of
//!   directories. For each `D`, corrald canonicalizes it race-safely from a
//!   directory fd, scans `<D>/.corral/registry/*.json`, and attributes every
//!   record to that canonical `D` (its physical location = its identity).
//! - Being in the right directory authenticates only *who wrote the record*.
//!   Every field is still adversarial, so [`vet`] validates each before the
//!   record is trusted; a failure quarantines it (dropped, never emitted).
//!
//! Registration (the `approved_commands` gate) is applied by corrald *after*
//! curation, so this module stays about identity + field validation only.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::approved_commands::{self, Approved, Template};
use crate::discovery::{self, parse_registry_json, RegistryEntry};

/// Cap for free-text display fields, so a hostile record cannot bloat a card.
const MAX_TEXT: usize = 200;

/// Cap for an outbox submission file, so a hostile path cannot make corrald
/// read an unbounded amount (security design H1).
const MAX_SUBMISSION: u64 = 256 * 1024;

/// A dormant (socket-cleared) source record left untouched this long is pruned
/// by the curator (measured from the record file's mtime, which an adapter
/// refreshes on activity and clears-socket on shutdown). A live record is never
/// pruned. corrald owns this lifecycle, since it is the only reader of the
/// agent-written source records.
const DORMANT_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 60 * 60);

/// Resolve an agent's outbox submission (security design T2-send / T14). The
/// sender wrote its message to `<cwd>/.corral/outbox/<id>.json` and passed the
/// path over the control socket; corrald opens it and derives the trusted
/// `fromCwd` from where the file physically lives, ignoring any `fromCwd` in
/// the content. Returns `(cwd, content)`.
///
/// Consumes the file: on success it unlinks the *deduced* real path (the one it
/// just validated through the fd), never the caller's raw envelope string. The
/// raw path is untrusted input — the boundary reads and removes the file itself
/// so no caller is ever handed the attacker path back to act on.
///
/// Hardened against a confused-deputy path (corrald is unsandboxed):
/// - non-blocking open, so a FIFO in place cannot hang corrald;
/// - regular file only (reject FIFO/device/dir);
/// - size-capped;
/// - the fd's real path must match `<cwd>/.corral/outbox/<name>` — any other
///   location (a symlink target elsewhere, `/etc/...`) is rejected, so corrald
///   never reads an arbitrary file.
///
/// Every rejection carries a [`SubmissionError`] rather than a bare `None`: a
/// refused submission is acked to its sender as `malformed`, and without a
/// reason neither the operator's journal nor the calling agent can tell a typo
/// from an unreachable file (the mount-namespace case below).
pub fn resolve_submission(path: &Path) -> Result<(String, String), SubmissionError> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| SubmissionError::Unopenable)?;
    let meta = file.metadata().map_err(|_| SubmissionError::Unopenable)?;
    if !meta.is_file() {
        return Err(SubmissionError::NotRegularFile);
    }
    if meta.len() > MAX_SUBMISSION {
        return Err(SubmissionError::TooLarge);
    }
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| SubmissionError::Unopenable)?;
    let cwd = discovery::cwd_from_outbox_path(&real).ok_or(SubmissionError::OutsideOutbox)?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|_| SubmissionError::Unreadable)?;
    // Consume the outbox file via the deduced real path, so the caller never
    // touches the raw envelope path again (best-effort).
    let _ = std::fs::remove_file(&real);
    Ok((cwd, content))
}

/// Why corrald refused an outbox submission. Each variant is one rejection in
/// [`resolve_submission`], reported verbatim to the operator's journal and to
/// the calling agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionError {
    /// The path could not be opened at all. The common cause is not a typo: an
    /// agent whose workdir sits under a mount the sandbox made private (a
    /// per-sandbox tmpfs over `/tmp`, say) writes an outbox file only it can
    /// see, so corrald opens nothing. That breaks the whole location=identity
    /// premise for such a session, which is why the message names it.
    Unopenable,
    /// Not a regular file (a FIFO, device or directory in its place).
    NotRegularFile,
    /// Larger than the submission cap.
    TooLarge,
    /// The real path is not `<cwd>/.corral/outbox/<name>`.
    OutsideOutbox,
    /// Opened, but reading failed.
    Unreadable,
}

impl std::fmt::Display for SubmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Unopenable => {
                "cannot open the submitted file (missing, or written inside a mount namespace corrald cannot see, e.g. a workdir under a sandbox-private /tmp)"
            }
            Self::NotRegularFile => "the submitted path is not a regular file",
            Self::TooLarge => "the submitted file exceeds the size cap",
            Self::OutsideOutbox => "the submitted file is not under <cwd>/.corral/outbox/",
            Self::Unreadable => "the submitted file could not be read",
        };
        f.write_str(s)
    }
}

/// Read the raw pointer store (`~/.corral/input/registry/`) into a deduplicated
/// list of the directories agents announced from. Each file is one session's
/// pointer, named `<sessionId>`, whose content is the cwd it runs in; we take
/// its first non-blank line. A missing dir is empty. The paths are still
/// untrusted (an agent wrote them); [`canonical_dir`] authenticates each. Only
/// the distinct set of cwds matters here — corrald scans each pointed-at
/// `<cwd>/.corral/registry/` for every session's real record.
pub fn read_pointers(pointer_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(pointer_dir) else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for e in entries.filter_map(Result::ok) {
        if !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Some(cwd) = text.lines().map(str::trim).find(|l| !l.is_empty()) else {
            continue;
        };
        if seen.insert(cwd.to_string()) {
            out.push(cwd.to_string());
        }
    }
    out
}

/// Canonicalize a listed directory race-safely: open it as a directory
/// (non-blocking, so a hostile FIFO in its place cannot hang us) and read the
/// real path the fd points at via `/proc/self/fd`. A listed dir that is a
/// symlink therefore resolves to its true target, so records are attributed to
/// where they *physically* live, not to the attacker-chosen listed path.
/// Returns `None` if the path is not a directory or cannot be opened.
pub fn canonical_dir(dir: &str) -> Option<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(dir)
        .ok()?;
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    Some(real.to_string_lossy().into_owned())
}

/// Strip control characters and cap length: a display field is attacker text.
fn sanitize(s: String) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    if cleaned.chars().count() > MAX_TEXT {
        cleaned.chars().take(MAX_TEXT).collect()
    } else {
        cleaned
    }
}

/// Where a record's `socket` field physically leads (T17). A record names its
/// own socket, and corrald/the boards traverse that path **unsandboxed**, so
/// the value is a borrowed-authority hazard: a lexical check on the string is
/// not enough, because a symlink an agent plants inside its own `.corral` may
/// name a peer's socket it could never open itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketPlace {
    /// The socket is a real socket physically inside `<dir>/.corral/`.
    Inside,
    /// Nothing is there (not yet bound, or a crashed session whose socket was
    /// unlinked while the record still names it). Not evidence of an attack, so
    /// the record survives as dormant rather than being quarantined — a crashed
    /// session must stay visible and resumable.
    Absent,
    /// The path resolves somewhere else, or is a symlink, or is not a socket:
    /// the record is aiming corral at something outside its own box.
    Foreign,
}

/// Resolve where a record's `socket` really leads, by physical location rather
/// than by string shape (T17). IO, kept beside the other identity derivations.
///
/// A symlink is refused outright: the convention says an agent *binds* its
/// socket, so a link is never legitimate, and following one would let an agent
/// borrow corrald's authority to reach a path it cannot open itself. The parent
/// is then canonicalized from a directory fd and compared with the record's own
/// canonical `<dir>/.corral`, so neither `..` nor a symlinked `.corral` can
/// make a foreign socket read as local. Any other IO error reads as `Absent`
/// (fail closed without quarantining: the socket is simply not published).
pub fn locate_socket(dir: &str, socket: &Path) -> SocketPlace {
    // The box's own `.corral` must be a real directory. A symlinked one would
    // canonicalize to its target on BOTH sides of the comparison below, so a
    // peer's socket would compare equal to "my own" (see `real_dir`).
    let corral = Path::new(dir).join(".corral");
    if !real_dir(&corral) {
        return SocketPlace::Foreign;
    }
    let Ok(meta) = std::fs::symlink_metadata(socket) else {
        return SocketPlace::Absent; // nothing there (or unreadable): not live
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_socket() {
        return SocketPlace::Foreign;
    }
    let Some(parent) = socket
        .parent()
        .and_then(|p| canonical_dir(&p.to_string_lossy()))
    else {
        return SocketPlace::Foreign;
    };
    if Some(parent) == canonical_dir(&corral.to_string_lossy()) {
        SocketPlace::Inside
    } else {
        SocketPlace::Foreign
    }
}

/// Whether `path` is a real directory rather than a symlink to one.
/// `symlink_metadata` does not follow, so a symlink reports as a symlink and
/// fails the `is_dir` test.
///
/// Load-bearing for identity: an agent may replace its own `.corral` (or the
/// `registry` inside it) with a link to a peer's, which would make corrald read
/// the *peer's* records and stamp them with the *attacker's* cwd — importing a
/// victim's live socket into a card the attacker owns. Physical location is only
/// identity if the path from the box to the record is not itself redirectable.
fn real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Vet one raw record found physically in the canonical directory `dir` under
/// the filename `<file_stem>.json`. Returns the trusted entry (with `cwd`
/// stamped to `dir`), or `None` to quarantine it. Pure — the caller resolves
/// the socket's real location ([`locate_socket`]) and passes it in, so the
/// verdict table stays testable without a filesystem.
///
/// Rules (every field is adversarial; see the security design):
/// - `sessionId` must pass the charset gate and equal `file_stem`.
/// - a `socket` that leads outside `<dir>/.corral/` quarantines the record; one
///   that leads nowhere is cleared, leaving a dormant (resumable) record.
/// - `cwd` is overwritten with `dir`; any content `cwd` is ignored.
/// - `title`/`description` are sanitized for display.
pub fn vet(
    dir: &str,
    file_stem: &str,
    mut rec: RegistryEntry,
    place: SocketPlace,
) -> Option<RegistryEntry> {
    if !discovery::valid_session_id(&rec.session_id) || rec.session_id != file_stem {
        return None;
    }
    if rec.socket.is_some() {
        match place {
            SocketPlace::Inside => {}
            // Aiming at another box (T17): quarantine, never publish.
            SocketPlace::Foreign => return None,
            // Nothing bound there: publish as dormant, so a crashed session
            // stays visible and resumable instead of vanishing.
            SocketPlace::Absent => rec.socket = None,
        }
    }
    rec.cwd = Some(dir.to_string());
    rec.title = rec.title.map(sanitize);
    rec.description = rec.description.map(sanitize);
    rec.model = rec.model.map(sanitize);
    Some(rec)
}

/// Scan one already-canonicalized directory's `<dir>/.corral/registry/*.json`,
/// vetting each record. IO, but pure given the filesystem.
pub fn curate_dir(dir: &str) -> Vec<RegistryEntry> {
    let corral = Path::new(dir).join(".corral");
    let regdir = corral.join("registry");
    // Neither hop may be a symlink, or a box could import another box's records
    // and have them attributed to itself (see `real_dir`).
    if !real_dir(&corral) || !real_dir(&regdir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&regdir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).ok()?;
            let rec = parse_registry_json(&text)?;
            // Prune a dormant record (socket cleared) whose file has gone stale
            // past the horizon; live records (socket set) are never pruned.
            if rec.socket.is_none() {
                let stale = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > DORMANT_MAX_AGE);
                if stale {
                    let _ = std::fs::remove_file(&p);
                    return None;
                }
            }
            let place = match &rec.socket {
                Some(s) => locate_socket(dir, s),
                None => SocketPlace::Absent,
            };
            vet(dir, &stem, rec, place)
        })
        .collect()
}

/// Curate the whole registry: read the pointer store, canonicalize each listed
/// dir, and vet every record under it. Deduplicated by canonical dir. The
/// result is the authenticated + field-validated set; corrald then applies the
/// registration gate before writing `state/registry/`.
pub fn curate(pointer_dir: &Path) -> Vec<RegistryEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for listed in read_pointers(pointer_dir) {
        let Some(dir) = canonical_dir(&listed) else {
            continue;
        };
        if !seen.insert(dir.clone()) {
            continue; // a symlink and its target both listed: curate once
        }
        out.extend(curate_dir(&dir));
    }
    out
}

/// The path of a directory's raw per-project record store, where an announcing
/// agent writes `<sessionId>.json`. A helper so producers and corrald agree.
pub fn record_dir(cwd: &str) -> PathBuf {
    Path::new(cwd).join(".corral").join("registry")
}

/// Forget a dormant session (the board's `d`): delete both its authoritative
/// workdir record (`<cwd>/.corral/registry/<id>.json`) and its home pointer
/// (`~/.corral/input/registry/<id>`). corrald reflects the removal out of
/// `state/registry/` on its next scan (deleting the vetted copy directly would
/// be futile — it would be re-curated). Both deletions are idempotent: an
/// already-missing file is not an error, so a double `d` is harmless. Returns
/// the first genuine IO error for the shell to surface.
pub fn forget_dormant(cwd: &str, session_id: &str) -> std::io::Result<()> {
    let record = record_dir(cwd).join(format!("{session_id}.json"));
    let pointer = crate::paths::input_registry_dir().map(|d| d.join(session_id));
    let mut first_err = remove_if_present(&record);
    if let Some(p) = pointer {
        let e = remove_if_present(&p);
        if first_err.is_ok() {
            first_err = e;
        }
    }
    first_err
}

/// How long a pointer is left alone before it may be pruned. An agent writes
/// its pointer first and its workdir record a moment later, so a young pointer
/// without a record is a normal announce in progress, not an orphan.
const POINTER_GRACE: std::time::Duration = std::time::Duration::from_secs(300);

/// Delete pointers whose session record is gone. The workdir record is the
/// authority and `curate_dir` already prunes it once a dormant session passes
/// `DORMANT_MAX_AGE`, but nothing used to remove the matching pointer, so the
/// store grew without bound (642 files after nine days) and every scan re-read
/// all of them — the dominant cost in corrald's idle CPU. Pruning here keeps
/// the scan proportional to the sessions that actually exist.
pub fn prune_orphan_pointers(pointer_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(pointer_dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        if !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = e.path();
        // Old enough to judge? Anything younger (or with an unreadable mtime)
        // is left alone.
        let ripe = matches!(
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok()),
            Some(age) if age >= POINTER_GRACE
        );
        if !ripe {
            continue;
        }
        let Some(session_id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let record = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(str::to_string)
            })
            .map(|cwd| record_dir(&cwd).join(format!("{session_id}.json")));
        // No readable cwd (empty or garbage pointer) counts as orphaned too.
        if record.is_none_or(|r| !r.exists()) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Remove a file, treating "already gone" as success (idempotent dismiss).
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The result of applying the registration gate to the vetted set: the
/// `registered` records (safe to publish to `state/registry/` and route to),
/// and the deduplicated `pending` launch-sets that still need operator
/// approval before their kind may be used.
#[derive(Debug, Default)]
pub struct Split {
    pub registered: Vec<RegistryEntry>,
    /// Distinct `(label, launch-set)` pairs awaiting registration. Deduplicated
    /// so a flood of unregistered sessions of one novel kind yields one prompt,
    /// not one per session (the approval-flood defense).
    pub pending: Vec<(String, Template)>,
}

/// Apply the registration gate (security design T4): partition the vetted
/// records into those whose kind + launch-set is already registered and those
/// still pending. Pure over the injected `approved` store. Only `registered`
/// records are ever published or routed; `pending` drives the operator prompt.
pub fn partition(vetted: Vec<RegistryEntry>, approved: &Approved) -> Split {
    let mut split = Split::default();
    let mut seen = std::collections::BTreeSet::new();
    for rec in vetted {
        if approved_commands::registered(&rec, approved) {
            split.registered.push(rec);
        } else if let Some(label) = rec.label.clone() {
            // A kind with no label can never be registered; drop it silently.
            let cand = approved_commands::candidate(&rec);
            if seen.insert((label.clone(), cand.clone())) {
                split.pending.push((label, cand));
            }
        }
    }
    split
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sid: &str, socket: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            session_id: sid.into(),
            cwd: Some("/lie".into()),
            title: Some("t".into()),
            socket: socket.map(PathBuf::from),
            pid: None,
            pid_namespace: None,
            spawn_command: None,
            resume_command: None,
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
            model: None,
            entries: None,
            context_percent: None,
            context_age: None,
        }
    }

    #[test]
    fn vet_stamps_cwd_and_ignores_content_cwd() {
        let out = vet("/home/dev/x", "s1", rec("s1", None), SocketPlace::Absent).unwrap();
        assert_eq!(out.cwd.as_deref(), Some("/home/dev/x"));
    }

    #[test]
    fn vet_rejects_bad_session_id_and_filename_mismatch() {
        let p = SocketPlace::Absent;
        assert_eq!(vet("/w", "s1", rec("--evil", None), p), None); // charset
        assert_eq!(vet("/w", "s1", rec("other", None), p), None); // != filename
    }

    #[test]
    fn vet_quarantines_a_foreign_socket_and_dormants_an_absent_one() {
        let sock = Some("/w/.corral/pi-1.sock");
        // Physically inside its own box -> published live.
        let ok = vet("/w", "s1", rec("s1", sock), SocketPlace::Inside).unwrap();
        assert!(ok.socket.is_some());
        // Leads outside the box (T17: symlink, `..`, foreign path) -> quarantined,
        // so corral never connects there and the operator's `m` cannot land in
        // another session.
        assert_eq!(vet("/w", "s1", rec("s1", sock), SocketPlace::Foreign), None);
        // Nothing bound (crashed session): the record survives as dormant, so it
        // stays visible and resumable rather than disappearing from the board.
        let dormant = vet("/w", "s1", rec("s1", sock), SocketPlace::Absent).unwrap();
        assert_eq!(dormant.socket, None);
    }

    #[test]
    fn locate_socket_refuses_a_symlink_into_another_box() {
        // The H1 attack: an agent may write only its own workdir, but a symlink
        // stores an unresolved string, so it can name a peer's socket it could
        // never open itself and let the unsandboxed consumer traverse it.
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        let attacker = tmp.path().join("attacker");
        std::fs::create_dir_all(victim.join(".corral")).unwrap();
        std::fs::create_dir_all(attacker.join(".corral")).unwrap();
        let victim_sock = victim.join(".corral").join("victim.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&victim_sock).unwrap();

        let link = attacker.join(".corral").join("attacker.sock");
        std::os::unix::fs::symlink(&victim_sock, &link).unwrap();
        let dir = attacker.to_string_lossy().into_owned();
        assert_eq!(locate_socket(&dir, &link), SocketPlace::Foreign);

        // Its own genuinely bound socket is accepted.
        let own = attacker.join(".corral").join("own.sock");
        let _own_listener = std::os::unix::net::UnixListener::bind(&own).unwrap();
        assert_eq!(locate_socket(&dir, &own), SocketPlace::Inside);

        // A symlinked `.corral` DIRECTORY cannot launder a foreign socket
        // either. Canonicalizing both sides would otherwise compare the victim's
        // dir against itself and read as "my own box".
        let laundered = tmp.path().join("laundered");
        std::fs::create_dir_all(&laundered).unwrap();
        std::os::unix::fs::symlink(victim.join(".corral"), laundered.join(".corral")).unwrap();
        assert_eq!(
            locate_socket(
                &laundered.to_string_lossy(),
                &laundered.join(".corral/victim.sock")
            ),
            SocketPlace::Foreign
        );
        // ...and the same laundering cannot import the victim's records either.
        let pointers = tmp.path().join("input-laundered");
        std::fs::create_dir_all(&pointers).unwrap();
        std::fs::create_dir_all(victim.join(".corral").join("registry")).unwrap();
        std::fs::write(
            victim.join(".corral/registry/v1.json"),
            r#"{"sessionId":"v1","label":"pi"}"#,
        )
        .unwrap();
        std::fs::write(pointers.join("v1"), format!("{}\n", laundered.display())).unwrap();
        assert!(
            curate(&pointers).is_empty(),
            "a symlinked .corral must not import a peer's records"
        );

        // A path with nothing at it is Absent, not an attack.
        assert_eq!(
            locate_socket(&dir, &attacker.join(".corral/gone.sock")),
            SocketPlace::Absent
        );
        // A regular file where a socket should be is refused (never connected to).
        let plain = attacker.join(".corral").join("plain.sock");
        std::fs::write(&plain, b"x").unwrap();
        assert_eq!(locate_socket(&dir, &plain), SocketPlace::Foreign);
    }

    #[test]
    fn curate_drops_a_record_whose_socket_symlinks_out_of_its_box() {
        // End to end through the real curation path: the vetted set must not
        // carry the attacker's record at all.
        let tmp = tempfile::tempdir().unwrap();
        let victim = tmp.path().join("victim");
        let attacker = tmp.path().join("attacker");
        std::fs::create_dir_all(victim.join(".corral")).unwrap();
        std::fs::create_dir_all(attacker.join(".corral").join("registry")).unwrap();
        let victim_sock = victim.join(".corral").join("victim.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&victim_sock).unwrap();
        let link = attacker.join(".corral").join("evil.sock");
        std::os::unix::fs::symlink(&victim_sock, &link).unwrap();
        std::fs::write(
            attacker.join(".corral/registry/evil.json"),
            format!(
                r#"{{"sessionId":"evil","label":"pi","socket":"{}"}}"#,
                link.display()
            ),
        )
        .unwrap();
        let pointers = tmp.path().join("input");
        std::fs::create_dir_all(&pointers).unwrap();
        std::fs::write(pointers.join("evil"), format!("{}\n", attacker.display())).unwrap();

        assert!(
            curate(&pointers).is_empty(),
            "a record aiming at another box must never be vetted"
        );
    }

    #[test]
    fn vet_sanitizes_display_fields() {
        let mut r = rec("s1", None);
        r.title = Some("hi\u{7}\nthere".into());
        let out = vet("/w", "s1", r, SocketPlace::Absent).unwrap();
        assert_eq!(out.title.as_deref(), Some("hithere"));
    }

    #[test]
    fn read_pointers_dedups_by_cwd_and_skips_non_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("registry");
        std::fs::create_dir_all(&dir).unwrap();
        // Two sessions in the same cwd, one in another; content is the cwd.
        std::fs::write(dir.join("sid-a"), "/a\n").unwrap();
        std::fs::write(dir.join("sid-b"), "  /b  \n").unwrap();
        std::fs::write(dir.join("sid-c"), "/a\n").unwrap();
        // A subdir is not a pointer file and is skipped.
        std::fs::create_dir(dir.join("sub")).unwrap();
        let mut got = read_pointers(&dir);
        got.sort();
        assert_eq!(got, vec!["/a", "/b"]);
        // Missing dir is empty.
        assert!(read_pointers(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn prune_orphan_pointers_keeps_live_and_young_ones() {
        use std::time::{Duration, SystemTime};
        let tmp = tempfile::tempdir().unwrap();
        let pointers = tmp.path().join("input");
        std::fs::create_dir_all(&pointers).unwrap();
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(record_dir(cwd.to_str().unwrap())).unwrap();
        // A session whose record still exists: keep, however old the pointer.
        std::fs::write(record_dir(cwd.to_str().unwrap()).join("live.json"), "{}\n").unwrap();
        let keep = pointers.join("live");
        std::fs::write(&keep, format!("{}\n", cwd.display())).unwrap();
        // A session whose record is gone: prune.
        let drop = pointers.join("gone");
        std::fs::write(&drop, format!("{}\n", cwd.display())).unwrap();
        // Same, but written just now: an announce may still be in flight.
        let young = pointers.join("young");
        std::fs::write(&young, format!("{}\n", cwd.display())).unwrap();

        // Age the two older pointers past the grace period.
        let old = SystemTime::now() - POINTER_GRACE - Duration::from_secs(60);
        for p in [&keep, &drop] {
            std::fs::File::open(p)
                .unwrap()
                .set_modified(old)
                .expect("set mtime");
        }

        prune_orphan_pointers(&pointers);

        assert!(keep.exists(), "pointer with a live record must survive");
        assert!(
            young.exists(),
            "pointer inside the grace period must survive"
        );
        assert!(!drop.exists(), "orphaned pointer must be pruned");
        // Idempotent: a second pass on the same dir changes nothing.
        prune_orphan_pointers(&pointers);
        assert!(keep.exists());
    }

    #[test]
    fn forget_dormant_deletes_record_and_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        let rec = record_dir(cwd.to_str().unwrap());
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(rec.join("sid-7.json"), "{}").unwrap();
        let ptrdir = tmp.path().join("input-registry");
        std::fs::create_dir_all(&ptrdir).unwrap();
        std::fs::write(ptrdir.join("sid-7"), cwd.to_str().unwrap()).unwrap();
        // Point CORRAL_INPUT_REGISTRY at our temp pointer dir for the duration.
        std::env::set_var("CORRAL_INPUT_REGISTRY", &ptrdir);
        forget_dormant(cwd.to_str().unwrap(), "sid-7").unwrap();
        std::env::remove_var("CORRAL_INPUT_REGISTRY");
        assert!(!rec.join("sid-7.json").exists());
        assert!(!ptrdir.join("sid-7").exists());
    }

    #[test]
    fn resolve_submission_derives_cwd_and_rejects_bad_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let boxd = tmp.path().join("proj");
        let outbox = boxd.join(".corral").join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let msg = outbox.join("m1.json");
        std::fs::write(&msg, r#"{"id":"1","message":"hi"}"#).unwrap();
        let (cwd, content) = resolve_submission(&msg).unwrap();
        assert_eq!(cwd, std::fs::canonicalize(&boxd).unwrap().to_string_lossy());
        assert!(content.contains("\"message\":\"hi\""));
        // The boundary consumes the file, so the caller never re-touches it.
        assert!(
            !msg.exists(),
            "resolve_submission must unlink the outbox file"
        );

        // A file not under .corral/outbox is rejected (corrald never reads an
        // arbitrary path).
        let stray = boxd.join(".corral").join("stray.json");
        std::fs::write(&stray, "{}").unwrap();
        assert_eq!(
            resolve_submission(&stray),
            Err(SubmissionError::OutsideOutbox)
        );
        // A missing file is rejected, with the reason the sandbox-private-mount
        // case needs.
        assert_eq!(
            resolve_submission(&outbox.join("nope.json")),
            Err(SubmissionError::Unopenable)
        );
    }

    #[test]
    fn partition_gates_on_registration_and_dedups_pending() {
        use crate::approved_commands::{register, Approved};
        let mut pi = rec("s1", None);
        pi.spawn_command = Some(vec!["pi".into()]);
        let approved = register(Approved::new(), &pi);
        // A registered pi session is published; two unregistered opencode
        // sessions collapse to one pending prompt (approval-flood defense).
        let mut oc1 = rec("s2", None);
        oc1.label = Some("opencode".into());
        oc1.spawn_command = Some(vec!["opencode".into()]);
        let mut oc2 = rec("s3", None);
        oc2.label = Some("opencode".into());
        oc2.spawn_command = Some(vec!["opencode".into()]);
        let split = partition(vec![pi, oc1, oc2], &approved);
        assert_eq!(split.registered.len(), 1);
        assert_eq!(split.registered[0].session_id, "s1");
        assert_eq!(
            split.pending.len(),
            1,
            "one prompt per novel kind, not per session"
        );
        assert_eq!(split.pending[0].0, "opencode");
    }

    #[test]
    fn curate_scans_project_records_and_attributes_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let boxd = tmp.path().join("proj");
        let regdir = boxd.join(".corral").join("registry");
        std::fs::create_dir_all(&regdir).unwrap();
        std::fs::write(
            regdir.join("s1.json"),
            r#"{"sessionId":"s1","cwd":"/lie","label":"pi"}"#,
        )
        .unwrap();
        // A filename/sessionId mismatch is quarantined.
        std::fs::write(regdir.join("s2.json"), r#"{"sessionId":"nope"}"#).unwrap();

        // The pointer store: one per-session file naming the workdir.
        let ptrdir = tmp.path().join("input-registry");
        std::fs::create_dir_all(&ptrdir).unwrap();
        std::fs::write(ptrdir.join("s1"), format!("{}\n", boxd.to_string_lossy())).unwrap();

        let vetted = curate(&ptrdir);
        assert_eq!(vetted.len(), 1);
        assert_eq!(vetted[0].session_id, "s1");
        // cwd is the real canonical dir, not the content lie.
        assert_eq!(
            vetted[0].cwd.as_deref(),
            Some(
                std::fs::canonicalize(&boxd)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }
}
