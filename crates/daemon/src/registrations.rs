//! Harness-registration approvals — the peer of the router's message
//! approvals (security design T4, and H3: the two are distinct consents with
//! separate stores and lifetimes, never bundled).
//!
//! Where the router gates a `(sender-dir -> target-dir)` message pair against
//! the whitelist, the `Registrar` gates a `(label, launch-set)` harness kind
//! against the approved-command store. The shapes are deliberately symmetric:
//! each holds what is pending, surfaces one item to the operator, and applies
//! the decision to its own store. Approval here is permanent (a kind is
//! trusted) and two-way (approve / deny) — there is no "once", unlike a
//! message.
//!
//! The pure decision logic is unit-tested; the store IO is a thin call into
//! `core::approved_commands`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use corral_core::approved_commands::{self, Template};

/// Owns the registration approval state: what is pending, and what the operator
/// has denied this session (so a denied kind is not re-prompted until its
/// launch set changes).
pub struct Registrar {
    approved_file: PathBuf,
    /// Kinds awaiting a decision (deduplicated by the curator, minus denied).
    pending: Vec<(String, Template)>,
    /// `(label, set)` the operator denied this run; kept out of `pending` so a
    /// deny does not immediately re-surface each tick.
    denied: BTreeSet<(String, Template)>,
}

impl Registrar {
    pub fn new(approved_file: PathBuf) -> Self {
        Self {
            approved_file,
            pending: Vec::new(),
            denied: BTreeSet::new(),
        }
    }

    /// Feed the curator's pending `(label, set)` list; keep those the operator
    /// has not denied. Order-preserving, so the first pending stays stable.
    pub fn observe(&mut self, pending: Vec<(String, Template)>) {
        self.pending = pending
            .into_iter()
            .filter(|p| !self.denied.contains(p))
            .collect();
    }

    /// The one registration to surface to the operator now, if any.
    pub fn current(&self) -> Option<&(String, Template)> {
        self.pending.first()
    }

    /// Approve a label's pending launch set: write it into the approved store
    /// (corrald is the sole writer). Returns `Ok(true)` when a matching pending
    /// entry was found and persisted, `Ok(false)` if the label is no longer
    /// pending (a stale click), or the IO error on write failure.
    pub fn approve(&mut self, label: &str) -> std::io::Result<bool> {
        let Some(idx) = self.pending.iter().position(|(l, _)| l == label) else {
            return Ok(false);
        };
        let (label, template) = self.pending.remove(idx);
        let mut approved = approved_commands::read_approved(&self.approved_file);
        approved.insert(label, template);
        approved_commands::write_approved(&self.approved_file, &approved)?;
        Ok(true)
    }

    /// Deny a label's pending launch set: drop it from pending and remember it,
    /// so it is not re-prompted until the set changes.
    pub fn deny(&mut self, label: &str) {
        if let Some(idx) = self.pending.iter().position(|(l, _)| l == label) {
            let entry = self.pending.remove(idx);
            self.denied.insert(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpl(spawn: &str) -> Template {
        Template {
            spawn: Some(vec![spawn.into()]),
            resume: None,
            gui: false,
            message_flag: None,
        }
    }

    #[test]
    fn approve_writes_the_store_and_clears_pending() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("approved-commands.json");
        let mut r = Registrar::new(file.clone());
        r.observe(vec![("pi".into(), tmpl("pi")), ("opencode".into(), tmpl("opencode"))]);
        assert_eq!(r.current().unwrap().0, "pi");

        assert!(r.approve("pi").unwrap());
        // Persisted to the store.
        let approved = approved_commands::read_approved(&file);
        assert_eq!(approved.get("pi"), Some(&tmpl("pi")));
        // No longer pending; the next kind surfaces.
        assert_eq!(r.current().unwrap().0, "opencode");
        // A stale approve of an already-cleared label is a no-op.
        assert!(!r.approve("pi").unwrap());
    }

    #[test]
    fn deny_keeps_it_from_resurfacing() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registrar::new(dir.path().join("approved-commands.json"));
        r.observe(vec![("pi".into(), tmpl("pi"))]);
        r.deny("pi");
        assert!(r.current().is_none());
        // The curator keeps reporting it (the record still exists), but a denied
        // set is filtered out, so it does not nag.
        r.observe(vec![("pi".into(), tmpl("pi"))]);
        assert!(r.current().is_none());
        // A *changed* set (new template) is a different pair, so it surfaces.
        r.observe(vec![("pi".into(), tmpl("pi-v2"))]);
        assert_eq!(r.current().unwrap().0, "pi");
    }
}
