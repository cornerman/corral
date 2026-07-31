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
    // Drop pointers whose session record is long gone before scanning, so the
    // scan stays proportional to the live sessions rather than to every session
    // this machine ever ran.
    curation::prune_orphan_pointers(pointer_dir);
    let approved = approved_commands::read_approved(approved_file);
    let split = curation::partition(curation::curate(pointer_dir), &approved);
    if std::fs::create_dir_all(state_registry_dir).is_err() {
        return split.pending;
    }
    // 0700 on the state dir (defense in depth; the sandbox seals it anyway).
    let _ = set_mode_700(state_registry_dir);

    let mut present = BTreeSet::new();
    for rec in &split.registered {
        let name = vetted_name(rec.cwd.as_deref().unwrap_or_default(), &rec.session_id);
        present.insert(name.clone());
        let Ok(json) = record_json(rec) else { continue };
        let target = state_registry_dir.join(&name);
        // Write only on change, so viewers watching state/registry do not see a
        // stream of identical rewrites.
        if std::fs::read_to_string(&target).ok().as_deref() == Some(json.as_str()) {
            continue;
        }
        // Atomic write (tmp + rename) so a scanning viewer never reads a partial.
        let tmp = state_registry_dir.join(format!(".{}.{}.tmp", name, std::process::id()));
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

/// The vetted store's filename for one record: the session id prefixed with a
/// hash of its **authenticated cwd**. Keyed on both, because a session id alone
/// is not unique across directories: any workdir may write a record naming any
/// id (it need only match its own filename), so a `<sessionId>.json` name let a
/// peer's record occupy a victim's slot and evict it from every board — taking
/// a live session off the operator's screen and pointing operator actions at the
/// squatter's directory. Both records are published instead, each attributed to
/// where it really lives; a session id claimed twice is then visible and every
/// id-addressed action fails closed (`discovery::unique_session`).
///
/// The hash is for uniqueness only, never integrity: the trusted cwd is inside
/// the record, and a viewer reads that, not the filename.
fn vetted_name(cwd: &str, session_id: &str) -> String {
    // FNV-1a 64: dependency-free and stable across runs (unlike DefaultHasher,
    // which is randomly seeded), so a record keeps one filename and the
    // write-on-change check keeps working.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in cwd.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}-{session_id}.json")
}

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
    // The window-owning pid + its PID-namespace inode (the NSpid bridge), so a
    // board reading the vetted store can translate to a host pid for focus/kill.
    if let Some(p) = rec.pid {
        m.insert("pid".into(), p.into());
    }
    if let Some(ns) = rec.pid_namespace {
        m.insert("pidNamespace".into(), ns.into());
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
    if let Some(m2) = &rec.model {
        m.insert("model".into(), m2.clone().into());
    }
    if let Some(e) = rec.entries {
        m.insert("entries".into(), e.into());
    }
    if let Some(p) = rec.context_percent {
        m.insert("contextPercent".into(), p.into());
    }
    if let Some(a) = &rec.context_age {
        m.insert("contextAge".into(), a.clone().into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vetted_name_separates_one_session_id_across_directories() {
        // The H2 regression: two directories claiming one session id must not
        // share a filename, or the later write evicts the earlier record.
        let a = vetted_name("/home/dev/victim", "shared");
        let b = vetted_name("/home/dev/attacker", "shared");
        assert_ne!(a, b, "a squatted id must not collide in the vetted store");
        // Stable across calls, so write-on-change still suppresses rewrites.
        assert_eq!(a, vetted_name("/home/dev/victim", "shared"));
        assert!(a.ends_with("-shared.json"));
    }

    #[test]
    fn record_json_includes_model_when_set() {
        let mut rec = corral_core::discovery::parse_registry_json(
            r#"{"sessionId":"s1","model":"anthropic/claude-opus-4"}"#,
        )
        .unwrap();
        rec.cwd = Some("/tmp/p".into());
        let json = record_json(&rec).unwrap();
        assert!(json.contains("\"model\": \"anthropic/claude-opus-4\""));
        // Absent model is omitted, not written as null.
        rec.model = None;
        assert!(!record_json(&rec).unwrap().contains("model"));
    }

    #[test]
    fn record_json_roundtrips_pid_and_namespace() {
        // Regression: record_json hand-lists fields, so pid/pidNamespace must be
        // written or the board reading the vetted store cannot translate to a
        // host pid (focus/kill silently break for sandboxed agents).
        let mut rec = corral_core::discovery::parse_registry_json(
            r#"{"sessionId":"s1","pid":42,"pidNamespace":4026532999}"#,
        )
        .unwrap();
        rec.cwd = Some("/tmp/p".into());
        let json = record_json(&rec).unwrap();
        assert!(json.contains("\"pid\": 42"));
        assert!(json.contains("\"pidNamespace\": 4026532999"));
        let back = corral_core::discovery::parse_registry_json(&json).unwrap();
        assert_eq!(back.pid, Some(42));
        assert_eq!(back.pid_namespace, Some(4026532999));
        // Absent -> omitted, not null.
        rec.pid = None;
        rec.pid_namespace = None;
        let json = record_json(&rec).unwrap();
        assert!(!json.contains("pid"));
        assert!(!json.contains("pidNamespace"));
    }

    #[test]
    fn record_json_includes_context_fields_when_set() {
        // Regression: record_json hand-lists fields, so a new RegistryEntry
        // field must be added here explicitly or the vetted state/registry/
        // output silently drops it even though the raw record carries it.
        let mut rec = corral_core::discovery::parse_registry_json(
            r#"{"sessionId":"s1","entries":42,"contextPercent":12,"contextAge":"3d"}"#,
        )
        .unwrap();
        rec.cwd = Some("/tmp/p".into());
        let json = record_json(&rec).unwrap();
        assert!(json.contains("\"entries\": 42"));
        assert!(json.contains("\"contextPercent\": 12"));
        assert!(json.contains("\"contextAge\": \"3d\""));
        // Absent fields are omitted, not written as null.
        rec.entries = None;
        rec.context_percent = None;
        rec.context_age = None;
        let json = record_json(&rec).unwrap();
        assert!(!json.contains("entries"));
        assert!(!json.contains("contextPercent"));
        assert!(!json.contains("contextAge"));
    }
}
