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
use std::os::unix::fs::OpenOptionsExt;
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
/// Hardened against a confused-deputy path (corrald is unsandboxed):
/// - non-blocking open, so a FIFO in place cannot hang corrald;
/// - regular file only (reject FIFO/device/dir);
/// - size-capped;
/// - the fd's real path must match `<cwd>/.corral/outbox/<name>` — any other
///   location (a symlink target elsewhere, `/etc/...`) is rejected, so corrald
///   never reads an arbitrary file.
pub fn resolve_submission(path: &Path) -> Option<(String, String)> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_SUBMISSION {
        return None;
    }
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    let cwd = discovery::cwd_from_outbox_path(&real)?;
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    Some((cwd, content))
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

/// Vet one raw record found physically in the canonical directory `dir` under
/// the filename `<file_stem>.json`. Returns the trusted entry (with `cwd`
/// stamped to `dir`), or `None` to quarantine it. Pure — no IO.
///
/// Rules (every field is adversarial; see the security design):
/// - `sessionId` must pass the charset gate and equal `file_stem`.
/// - `socket`, if present, must sit directly in `<dir>/.corral/` — so a card
///   can only ever drive a session in its own box (T17). A parent that is not
///   exactly `<dir>/.corral` (including any `..` escape) is rejected.
/// - `cwd` is overwritten with `dir`; any content `cwd` is ignored.
/// - `title`/`description` are sanitized for display.
pub fn vet(dir: &str, file_stem: &str, mut rec: RegistryEntry) -> Option<RegistryEntry> {
    if !discovery::valid_session_id(&rec.session_id) || rec.session_id != file_stem {
        return None;
    }
    if let Some(socket) = &rec.socket {
        let expected = Path::new(dir).join(".corral");
        if socket.parent() != Some(expected.as_path()) {
            return None; // socket aims outside this record's own box
        }
    }
    rec.cwd = Some(dir.to_string());
    rec.title = rec.title.map(sanitize);
    rec.description = rec.description.map(sanitize);
    Some(rec)
}

/// Scan one already-canonicalized directory's `<dir>/.corral/registry/*.json`,
/// vetting each record. IO, but pure given the filesystem.
pub fn curate_dir(dir: &str) -> Vec<RegistryEntry> {
    let regdir = Path::new(dir).join(".corral").join("registry");
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
            vet(dir, &stem, rec)
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
            spawn_command: None,
            resume_command: None,
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
        }
    }

    #[test]
    fn vet_stamps_cwd_and_ignores_content_cwd() {
        let out = vet("/home/dev/x", "s1", rec("s1", None)).unwrap();
        assert_eq!(out.cwd.as_deref(), Some("/home/dev/x"));
    }

    #[test]
    fn vet_rejects_bad_session_id_and_filename_mismatch() {
        assert_eq!(vet("/w", "s1", rec("--evil", None)), None); // charset
        assert_eq!(vet("/w", "s1", rec("other", None)), None); // != filename
    }

    #[test]
    fn vet_requires_socket_inside_own_corral() {
        // Socket directly in <dir>/.corral -> accepted.
        assert!(vet("/w", "s1", rec("s1", Some("/w/.corral/pi-1.sock"))).is_some());
        // Socket in another box -> rejected (T17).
        assert_eq!(
            vet("/w", "s1", rec("s1", Some("/victim/.corral/pi-1.sock"))),
            None
        );
        // `..` escape does not slip past the parent check.
        assert_eq!(
            vet(
                "/w",
                "s1",
                rec("s1", Some("/w/.corral/../../etc/pi-1.sock"))
            ),
            None
        );
    }

    #[test]
    fn vet_sanitizes_display_fields() {
        let mut r = rec("s1", None);
        r.title = Some("hi\u{7}\nthere".into());
        let out = vet("/w", "s1", r).unwrap();
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

        // A file not under .corral/outbox is rejected (corrald never reads an
        // arbitrary path).
        let stray = boxd.join(".corral").join("stray.json");
        std::fs::write(&stray, "{}").unwrap();
        assert_eq!(resolve_submission(&stray), None);
        // A missing file is rejected.
        assert_eq!(resolve_submission(&outbox.join("nope.json")), None);
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
