//! Harness registration: the approved-command store (security design T4).
//!
//! A harness *kind* (a record's `label`) must be registered before any of its
//! agents can be used. corrald is the sole approver and the only writer of the
//! store; the viewers only read it and apply [`registered`] to filter the
//! registry. A record that does not fit a registered template is quarantined
//! (not launchable, not shown as an actionable card).
//!
//! Everything here is pure and unit-tested. The one predicate [`registered`] is
//! shared by corrald and both viewers, so enforcement cannot drift and a
//! reviewer checks it in one place.

use std::collections::BTreeMap;
use std::path::Path;

use crate::discovery::RegistryEntry;

/// The placeholder a record's own `sessionId` normalizes to, so every session
/// of a kind collapses to one template (a resume argv carries a unique id).
pub const SESSION_PLACEHOLDER: &str = "{sessionId}";
/// The placeholder a record's own `cwd` normalizes to (a launch argv may carry
/// the working directory, e.g. cursor).
pub const CWD_PLACEHOLDER: &str = "{cwd}";

/// The approved argv templates for one registered kind, normalized. `spawn` or
/// `resume` is `None` when that mode was never approved for the kind, so a
/// record carrying the un-approved mode does not fit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Template {
    pub spawn: Option<Vec<String>>,
    pub resume: Option<Vec<String>>,
}

/// The store: `label -> approved templates`. Read by everyone (corrald and both
/// viewers); written only by corrald on operator approval.
pub type Approved = BTreeMap<String, Template>;

/// Which launch mode an action needs, so a caller can ask "is this record's
/// spawn (or resume) approved?" without re-deriving the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Spawn,
    Resume,
}

/// Replace the record's own `sessionId` and `cwd` in an argv with placeholders,
/// so every session of a kind yields one template. Substitution is by exact
/// arg equality: an arg equal to the session id becomes `{sessionId}`, an arg
/// equal to the cwd becomes `{cwd}`. Session id is checked first (a cwd is
/// never a bare session id).
pub fn normalize(argv: &[String], session_id: &str, cwd: Option<&str>) -> Vec<String> {
    argv.iter()
        .map(|a| {
            if a == session_id {
                SESSION_PLACEHOLDER.to_string()
            } else if cwd == Some(a.as_str()) {
                CWD_PLACEHOLDER.to_string()
            } else {
                a.clone()
            }
        })
        .collect()
}

/// Reverse of [`normalize`]: substitute the placeholders back to build the real
/// launch argv from a registered template. The caller guards `cwd`/`session_id`
/// values for argv-safety before this (see the launch path); an absent cwd
/// substitutes empty, which a template without `{cwd}` never triggers.
pub fn denormalize(template: &[String], session_id: &str, cwd: Option<&str>) -> Vec<String> {
    template
        .iter()
        .map(|a| match a.as_str() {
            SESSION_PLACEHOLDER => session_id.to_string(),
            CWD_PLACEHOLDER => cwd.unwrap_or("").to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// The candidate template a record proposes, built by normalizing whichever of
/// its spawn/resume commands are present. corrald shows this for approval and
/// stores it verbatim; a record with neither command proposes an empty
/// template.
pub fn candidate(record: &RegistryEntry) -> Template {
    let norm = |argv: &Vec<String>| normalize(argv, &record.session_id, record.cwd.as_deref());
    Template {
        spawn: record.spawn_command.as_ref().map(norm),
        resume: record.resume_command.as_ref().map(norm),
    }
}

/// Whether a record is a full citizen: its `label` is registered, and every
/// command it carries matches the registered template exactly. A record with
/// no label, an unregistered label, or any command that deviates from (or is
/// absent in) the stored template is **not** registered, so it is quarantined.
pub fn registered(record: &RegistryEntry, approved: &Approved) -> bool {
    let Some(label) = record.label.as_deref() else {
        return false; // no kind to register against
    };
    let Some(tmpl) = approved.get(label) else {
        return false; // kind never registered
    };
    let cand = candidate(record);
    // Each command the record carries must equal the stored counterpart. A
    // command present in the record but absent (or different) in the template
    // is a deviation, so the record does not fit.
    if cand.spawn.is_some() && cand.spawn != tmpl.spawn {
        return false;
    }
    if cand.resume.is_some() && cand.resume != tmpl.resume {
        return false;
    }
    true
}

/// Whether a specific launch mode of a record is approved to execute. Stricter
/// than [`registered`]: the needed command must be present in the record *and*
/// match the stored template, so a launch never runs an un-approved argv.
pub fn mode_approved(record: &RegistryEntry, approved: &Approved, mode: Mode) -> bool {
    if !registered(record, approved) {
        return false;
    }
    let cand = candidate(record);
    match mode {
        Mode::Spawn => cand.spawn.is_some(),
        Mode::Resume => cand.resume.is_some(),
    }
}

/// Read the approved-command store. A missing or unreadable file is an empty
/// store (nothing registered yet), never an error: the caller then quarantines
/// everything until the operator registers a kind.
pub fn read_approved(path: &Path) -> Approved {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Approved::new();
    };
    parse_approved(&text)
}

/// Parse the store JSON (`{ "<label>": { "spawn": [...], "resume": [...] } }`).
/// Lenient: a malformed document is an empty store, and a non-array command is
/// dropped, so a corrupt file never launches a garbled argv.
pub fn parse_approved(text: &str) -> Approved {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Approved::new();
    };
    let Some(obj) = v.as_object() else {
        return Approved::new();
    };
    let cmd = |t: &serde_json::Value, k: &str| {
        t.get(k).and_then(|x| x.as_array()).map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
    };
    obj.iter()
        .map(|(label, t)| {
            (
                label.clone(),
                Template {
                    spawn: cmd(t, "spawn"),
                    resume: cmd(t, "resume"),
                },
            )
        })
        .collect()
}

/// Serialize the store to the JSON written under `state/`. corrald is the only
/// caller (sole writer); the write itself is done by the daemon so this stays
/// pure.
pub fn approved_json(approved: &Approved) -> String {
    let obj: serde_json::Map<String, serde_json::Value> = approved
        .iter()
        .map(|(label, t)| {
            let mut m = serde_json::Map::new();
            if let Some(s) = &t.spawn {
                m.insert("spawn".into(), s.clone().into());
            }
            if let Some(r) = &t.resume {
                m.insert("resume".into(), r.clone().into());
            }
            (label.clone(), serde_json::Value::Object(m))
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// Register a record's candidate template under its label, returning the
/// updated store. Used by corrald after operator approval. A record with no
/// label is returned unchanged (nothing to register).
pub fn register(mut approved: Approved, record: &RegistryEntry) -> Approved {
    if let Some(label) = record.label.as_deref() {
        approved.insert(label.to_string(), candidate(record));
    }
    approved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(label: Option<&str>, sid: &str, cwd: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            session_id: sid.into(),
            cwd: cwd.map(String::from),
            title: None,
            socket: None,
            spawn_command: None,
            resume_command: None,
            label: label.map(String::from),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: false,
            description: None,
        }
    }

    #[test]
    fn normalize_substitutes_own_session_and_cwd() {
        let argv = vec![
            "pi".into(),
            "--session".into(),
            "abc-123".into(),
            "/home/dev/x".into(),
        ];
        let n = normalize(&argv, "abc-123", Some("/home/dev/x"));
        assert_eq!(
            n,
            vec!["pi", "--session", SESSION_PLACEHOLDER, CWD_PLACEHOLDER]
        );
        // A different session's argv normalizes identically -> one template.
        let other = vec!["pi".into(), "--session".into(), "zzz-999".into(), "/home/dev/x".into()];
        assert_eq!(normalize(&other, "zzz-999", Some("/home/dev/x")), n);
    }

    #[test]
    fn denormalize_round_trips() {
        let tmpl = vec!["pi".into(), "--session".into(), SESSION_PLACEHOLDER.into()];
        assert_eq!(
            denormalize(&tmpl, "abc-123", None),
            vec!["pi", "--session", "abc-123"]
        );
    }

    #[test]
    fn registered_only_when_label_and_commands_match() {
        let mut r = rec(Some("pi"), "s1", Some("/w"));
        r.spawn_command = Some(vec!["pi".into()]);
        r.resume_command = Some(vec!["pi".into(), "--session".into(), "s1".into()]);
        let approved = register(Approved::new(), &r);
        // The same shape (any session) is registered.
        let mut r2 = rec(Some("pi"), "s2", Some("/w2"));
        r2.spawn_command = Some(vec!["pi".into()]);
        r2.resume_command = Some(vec!["pi".into(), "--session".into(), "s2".into()]);
        assert!(registered(&r2, &approved));
        assert!(mode_approved(&r2, &approved, Mode::Spawn));
        assert!(mode_approved(&r2, &approved, Mode::Resume));
    }

    #[test]
    fn deviating_argv_is_quarantined() {
        let mut r = rec(Some("pi"), "s1", None);
        r.spawn_command = Some(vec!["pi".into()]);
        let approved = register(Approved::new(), &r);
        // An attacker claims label pi but a bash spawn -> deviates -> not registered.
        let mut evil = rec(Some("pi"), "s9", None);
        evil.spawn_command = Some(vec!["bash".into(), "-c".into(), "rm -rf ~".into()]);
        assert!(!registered(&evil, &approved));
        assert!(!mode_approved(&evil, &approved, Mode::Spawn));
    }

    #[test]
    fn unregistered_label_and_no_label_are_quarantined() {
        let approved = Approved::new();
        let mut r = rec(Some("pi"), "s1", None);
        r.spawn_command = Some(vec!["pi".into()]);
        assert!(!registered(&r, &approved), "empty store registers nothing");
        assert!(!registered(&rec(None, "s1", None), &approved), "no label");
    }

    #[test]
    fn adding_a_deviating_mode_quarantines() {
        // Template has spawn only; a record adding an (unapproved) resume deviates.
        let mut r = rec(Some("pi"), "s1", None);
        r.spawn_command = Some(vec!["pi".into()]);
        let approved = register(Approved::new(), &r);
        let mut with_resume = rec(Some("pi"), "s1", None);
        with_resume.spawn_command = Some(vec!["pi".into()]);
        with_resume.resume_command = Some(vec!["pi".into(), "--session".into(), "s1".into()]);
        assert!(!registered(&with_resume, &approved));
    }

    #[test]
    fn store_json_round_trips() {
        let mut r = rec(Some("pi"), "s1", None);
        r.spawn_command = Some(vec!["pi".into()]);
        r.resume_command = Some(vec!["pi".into(), "--session".into(), "s1".into()]);
        let approved = register(Approved::new(), &r);
        let json = approved_json(&approved);
        assert_eq!(parse_approved(&json), approved);
        // Placeholders survive the round trip.
        assert!(json.contains(SESSION_PLACEHOLDER));
    }

    #[test]
    fn malformed_store_is_empty() {
        assert!(parse_approved("not json").is_empty());
        assert!(parse_approved("[]").is_empty());
    }
}
