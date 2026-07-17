//! The registry curator: corrald's periodic reflect of the untrusted raw index
//! into the sealed, vetted `state/registry/` the viewers read (security design
//! Mechanism 1). corrald is the only reader of agent-writable records and the
//! only writer of the vetted store.
//!
//! Identity + field validation live in `core::curation`; this module does the
//! IO: read the index, and sync the vetted set into `state/registry/` (write
//! present, remove vanished) atomically.
//!
//! The registration gate (`approved_commands`) is applied here: `refresh`
//! partitions the field-validated records on the approved store and publishes
//! only the **registered** set to `state/registry/`, returning the rest as
//! pending operator approval (T4).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use corral_core::approved_commands::{self, Template};
use corral_core::curation;

/// Refresh `state_registry_dir` from the raw `pointer_dir`, gated by the
/// `approved_file` registration store: curate + partition, publish only the
/// **registered** records (add/update present, delete vanished), and return the
/// deduplicated `(label, launch-set)` pairs still **pending** operator
/// approval. Only registered records are published, so corrald routes and
/// viewers render approved kinds only. Best-effort per file.
pub fn refresh(
    pointer_dir: &Path,
    state_registry_dir: &Path,
    approved_file: &Path,
) -> Vec<(String, Template)> {
    let approved = approved_commands::read_approved(approved_file);
    let split = curation::partition(curation::curate(pointer_dir), &approved);
    if std::fs::create_dir_all(state_registry_dir).is_err() {
        return split.pending;
    }
    // 0700 on the state dir (defense in depth; the sandbox seals it anyway).
    let _ = set_mode_700(state_registry_dir);

    let mut present = BTreeSet::new();
    for rec in &split.registered {
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
        let tmp =
            state_registry_dir.join(format!(".{}.{}.tmp", rec.session_id, std::process::id()));
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &target);
        }
    }
    // Prune vetted records that no longer exist in the raw pointer store.
    if let Ok(entries) = std::fs::read_dir(state_registry_dir) {
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if !present.contains(&name) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    split.pending
}

/// Ensure the raw dir-index file and its parent (`~/.corral`) exist, so an
/// agent's sandbox can be granted append on an already-present file. The
/// hardened sandbox grants the agent write on this single file only (never on
/// `~/.corral` itself, or it could rebind the control socket — SECURITY.md T6);
/// a Landlock file rule binds to the inode at sandbox-build time and is silently
/// Ensure the agent-writable pointer store (`~/.corral/input/registry/`) and its
/// parents exist, so the agent sandbox can be granted `filesystem.write` on the
/// `input/` DIRECTORY: a Landlock dir rule binds to the dir's inode at
/// sandbox-build time and is silently skipped if the path is absent, so the dir
/// MUST pre-exist before any agent launches. corrald owns this layout (it
/// starts before agents), keeping perms in one place with `state/`.
///
/// The rule binds the DIR inode, so files inside may be created, overwritten,
/// or removed freely (agents write their own pointer, the board deletes it on
/// dismiss) without the grant ever going stale — the robustness the directory
/// grant buys over a per-file rule.
///
/// Deliberately does NOT touch anything else. In particular it does NOT delete
/// the obsolete pre-`input/` path `~/.corral/registry` left by an earlier
/// layout: corrald curates, it does not run migrations, and a daemon silently
/// deleting a directory it does not own is surprising and destructive. The old
/// path is harmless (never read — corrald reads only `input/registry/`); the
/// operator removes it by hand if they want it gone.
pub fn ensure_input(pointer_dir: &Path) -> std::io::Result<()> {
    // 0700 on input/registry/, input/, and ~/.corral (same-user only, defense
    // in depth; the sandbox is the real seal). Only input/ is granted to the
    // agent. Stop at ~/.corral — never chmod the user's $HOME.
    std::fs::create_dir_all(pointer_dir)?;
    let _ = set_mode_700(pointer_dir);
    if let Some(input) = pointer_dir.parent() {
        let _ = set_mode_700(input);
        if let Some(corral) = input.parent() {
            let _ = set_mode_700(corral);
        }
    }
    Ok(())
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

/// A compact one-line display of a launch set, shown at registration so the
/// operator sees exactly what will run (argv + the launch-affecting flags).
pub fn describe(t: &Template) -> String {
    let mut parts = Vec::new();
    if let Some(s) = &t.spawn {
        parts.push(format!("spawn={}", s.join(" ")));
    }
    if let Some(r) = &t.resume {
        parts.push(format!("resume={}", r.join(" ")));
    }
    if t.gui {
        parts.push("gui".into());
    }
    if let Some(f) = &t.message_flag {
        parts.push(format!("messageFlag={f}"));
    }
    parts.join(" ")
}

/// Append one line to the audit log (security design: the operator's after-the
/// -fact record). Best-effort; a failure to log never blocks the daemon.
pub fn audit(log: &Path, line: &str) {
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {line}");
    }
}
