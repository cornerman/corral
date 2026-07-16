//! The registry curator: corrald's periodic reflect of the untrusted raw index
//! into the sealed, vetted `state/registry/` the viewers read (security design
//! Mechanism 1). corrald is the only reader of agent-writable records and the
//! only writer of the vetted store.
//!
//! Identity + field validation live in `core::curation`; this module does the
//! IO: read the index, and sync the vetted set into `state/registry/` (write
//! present, remove vanished) atomically.
//!
//! NOTE: the registration gate (`approved_commands`) is layered on next — for
//! now every authenticated + field-validated record is written, so discovery
//! works end-to-end. Until registration lands, this store is not yet the
//! "registered only" set the design specifies.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use corral_core::curation;

/// Refresh `state_registry_dir` from the raw `index_file`: curate the vetted
/// records and make the directory match them (add/update present, delete
/// vanished). Best-effort per file; a single unwritable record never aborts the
/// sync.
///
/// NOTE: the registration gate (`curation::partition` over the approved store)
/// is not applied here yet, so today every authenticated + field-validated
/// record is published. Wiring the gate + the operator approval surface is the
/// next phase; until then this store is not yet the "registered only" set the
/// design specifies.
pub fn refresh(index_file: &Path, state_registry_dir: &Path) {
    let vetted = curation::curate(index_file);
    if std::fs::create_dir_all(state_registry_dir).is_err() {
        return;
    }
    // 0700 on the state dir (defense in depth; the sandbox seals it anyway).
    let _ = set_mode_700(state_registry_dir);

    let mut present = BTreeSet::new();
    for rec in &vetted {
        let name = format!("{}.json", rec.session_id);
        present.insert(name.clone());
        let Ok(json) = record_json(rec) else { continue };
        let target = state_registry_dir.join(&name);
        // Write only on change, so viewers watching state/registry do not see a
        // stream of identical rewrites.
        if std::fs::read_to_string(&target).ok().as_deref() == Some(json.as_str()) {
            continue;
        }
        // Atomic write (tmp + rename) so a scanning viewer never reads a partial.
        let tmp = state_registry_dir.join(format!(".{}.{}.tmp", rec.session_id, std::process::id()));
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &target);
        }
    }
    // Prune vetted records that no longer exist in the raw index.
    if let Ok(entries) = std::fs::read_dir(state_registry_dir) {
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if !present.contains(&name) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
}

/// Serialize a vetted entry back to the record JSON shape the viewers parse.
/// The `cwd` is the authenticated one corrald stamped, now trusted content.
fn record_json(rec: &corral_core::discovery::RegistryEntry) -> Result<String, serde_json::Error> {
    let mut m = serde_json::Map::new();
    m.insert("sessionId".into(), rec.session_id.clone().into());
    if let Some(cwd) = &rec.cwd {
        m.insert("cwd".into(), cwd.clone().into());
    }
    if let Some(t) = &rec.title {
        m.insert("title".into(), t.clone().into());
    }
    if let Some(s) = &rec.socket {
        m.insert("socket".into(), s.to_string_lossy().into_owned().into());
    }
    if let Some(c) = &rec.spawn_command {
        m.insert("spawnCommand".into(), c.clone().into());
    }
    if let Some(c) = &rec.resume_command {
        m.insert("resumeCommand".into(), c.clone().into());
    }
    if let Some(l) = &rec.label {
        m.insert("label".into(), l.clone().into());
    }
    if let Some(ls) = &rec.last_seen {
        m.insert("lastSeen".into(), ls.clone().into());
    }
    if rec.gui {
        m.insert("gui".into(), true.into());
    }
    if let Some(f) = &rec.message_flag {
        m.insert("messageFlag".into(), f.clone().into());
    }
    if rec.hidden {
        m.insert("hidden".into(), true.into());
    }
    if let Some(d) = &rec.description {
        m.insert("description".into(), d.clone().into());
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(m))
}

#[cfg(unix)]
fn set_mode_700(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Append one line to the audit log (security design: the operator's after-the
/// -fact record). Best-effort; a failure to log never blocks the daemon.
/// Wired into registration/authz decisions in the registration phase.
#[allow(dead_code)]
pub fn audit(log: &Path, line: &str) {
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {line}");
    }
}
