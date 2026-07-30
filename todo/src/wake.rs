//! How to wake the dispatcher: an ordered fallback chain, decided from the
//! registry alone. Pure — takes records, returns intents, performs no IO.
//!
//! The chain exists so the dispatcher's session id survives across wakes: a
//! live session is injected into, a dormant one is resumed through its own
//! `resumeCommand` (same session id, so every worker's reply handle stays
//! valid), and a fresh launch is the last resort.
//!
//! It is a chain rather than one choice because a record's `socket` field being
//! set does not prove the socket connects — a crashed dispatcher leaves it set.
//! The caller tries each step until one succeeds, the same fallback `corrald`'s
//! router uses, so neither needs dead-socket bookkeeping.

use corral_core::discovery::{live_socket, RegistryEntry};
use corral_core::launch::LaunchMode;
use std::path::PathBuf;

/// The file holding the dispatcher's operating policy, inside the todo
/// directory. Deliberately **not** `AGENTS.md`: that name is ambient, so it
/// would govern every agent that ever runs in the todo directory, including the
/// operator's own interactive session. The policy belongs to one role, so it
/// carries that role's name and is loaded only by the wake messages below.
pub const POLICY_FILE: &str = "DISPATCHER.md";

/// The nudge an established dispatcher receives.
///
/// It names the policy file on *every* wake, not only the first. Nothing loads
/// the file automatically, so the prompt is the only path by which the policy
/// reaches the model; naming it each time means a long-lived dispatcher whose
/// context has been compacted can still find its own law. Costs a handful of
/// tokens per wake, which is why it points at the file rather than inlining it.
///
/// It carries no task detail: the file is the state, so the dispatcher reads
/// `todo.txt` rather than trusting a message that could be stale on arrival.
pub const WAKE_MESSAGE: &str = concat!(
    "todo.txt changed. Run your dispatcher loop as specified in DISPATCHER.md ",
    "in this directory (re-read it if it is not already in your context).",
);

/// The first prompt of a brand-new dispatcher, which additionally tells the
/// session what it is — context a resumed or live session already has.
pub const FIRST_PROMPT: &str = concat!(
    "You are the todo dispatcher for this directory. Read DISPATCHER.md here — ",
    "it is your operating policy — and follow it exactly. Then run your loop: ",
    "todo.txt changed, so decide what to dispatch, answer, or close.",
);

/// One way to reach the dispatcher. No `Eq`: `LaunchMode` is only `PartialEq`.
#[derive(Debug, PartialEq)]
pub enum Wake {
    /// Write the wake into a live session's socket.
    Inject { socket: PathBuf },
    /// Relaunch this exact session (argv from the record's `resumeCommand`),
    /// carrying the wake as its launch message.
    Resume { argv: Vec<String>, mode: LaunchMode },
    /// Start a brand-new dispatcher from the configured argv. Distinct from
    /// `Resume` — though both just launch — because only a session with no
    /// history needs to be told where its policy lives.
    Spawn { argv: Vec<String>, mode: LaunchMode },
}

impl Wake {
    /// Which branch of the chain this is, for a log line.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Wake::Inject { .. } => "inject",
            Wake::Resume { .. } => "resume",
            Wake::Spawn { .. } => "spawn",
        }
    }

    /// The text delivered with this wake.
    pub fn message(&self) -> &'static str {
        match self {
            // A resumed or live session already carries the policy in its
            // transcript or its system prompt, so it gets the bare nudge.
            Wake::Inject { .. } | Wake::Resume { .. } => WAKE_MESSAGE,
            Wake::Spawn { .. } => FIRST_PROMPT,
        }
    }

    /// The argv and launch options, for the two variants that launch.
    pub fn launch_args(&self) -> Option<(&[String], &LaunchMode)> {
        match self {
            Wake::Inject { .. } => None,
            Wake::Resume { argv, mode } | Wake::Spawn { argv, mode } => Some((argv, mode)),
        }
    }
}

/// The wake steps to try in order. Always non-empty: the last step is a fresh
/// spawn, which needs no record.
pub fn plan(entries: &[RegistryEntry], dispatch_argv: &[String]) -> Vec<Wake> {
    // Hidden throughout: the dispatcher is background machinery, and a window
    // popping up on every todo edit would be intolerable.
    let hidden = |mode: LaunchMode| LaunchMode {
        hidden: true,
        ..mode
    };
    let mut steps = Vec::new();
    // Most recently seen wins: that is the session the operator most likely
    // still has in mind, and `last_seen` is ISO-8601 so it sorts as a string.
    if let Some(entry) = entries.iter().max_by(|a, b| a.last_seen.cmp(&b.last_seen)) {
        if let Some(live) = live_socket(entry) {
            steps.push(Wake::Inject { socket: live.path });
        }
        if let Some(argv) = entry.resume_argv() {
            steps.push(Wake::Resume {
                argv,
                mode: hidden(entry.launch_mode()),
            });
        }
    }
    steps.push(Wake::Spawn {
        argv: dispatch_argv.to_vec(),
        mode: hidden(LaunchMode::default()),
    });
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RegistryEntry` derives no `Default`, so every field is spelled out,
    /// the same way `crates/core/src/curation.rs`'s own tests build one.
    fn entry(session: &str, socket: Option<&str>, last_seen: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: session.into(),
            cwd: Some("/home/me/todos".into()),
            title: None,
            socket: socket.map(PathBuf::from),
            pid: Some(42),
            pid_namespace: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]),
            label: Some("pi".into()),
            last_seen: Some(last_seen.into()),
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

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn hidden_mode() -> LaunchMode {
        LaunchMode {
            hidden: true,
            ..LaunchMode::default()
        }
    }

    #[test]
    fn a_live_record_is_injected_first_then_falls_back() {
        let entries = vec![entry(
            "S1",
            Some("/home/me/todos/.corral/S1.sock"),
            "2026-07-26T10:00:00Z",
        )];
        let steps = plan(&entries, &argv(&["pi"]));
        // Inject, then resume that same session, then a fresh spawn: a record
        // with a socket set is not proof the socket connects.
        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[0],
            Wake::Inject {
                socket: PathBuf::from("/home/me/todos/.corral/S1.sock")
            }
        );
        let Wake::Resume { argv: resume, .. } = &steps[1] else {
            panic!("expected a resume");
        };
        assert_eq!(resume, &argv(&["pi", "--session", "S1"]));
        assert_eq!(
            steps[2],
            Wake::Spawn {
                argv: argv(&["pi"]),
                mode: hidden_mode()
            }
        );
    }

    #[test]
    fn only_a_fresh_spawn_is_told_where_its_policy_lives() {
        // A live or resumed session already has the policy; repeating it every
        // wake would be noise.
        let entries = vec![entry(
            "S1",
            Some("/home/me/todos/.corral/S1.sock"),
            "2026-07-26T10:00:00Z",
        )];
        let steps = plan(&entries, &argv(&["pi"]));
        assert_eq!(steps[0].message(), WAKE_MESSAGE);
        assert_eq!(steps[1].message(), WAKE_MESSAGE);
        assert_eq!(steps[2].message(), FIRST_PROMPT);
    }

    #[test]
    fn every_message_names_the_policy_file_and_inlines_nothing() {
        // Nothing loads the policy automatically, so each wake must say where
        // it is; a compacted dispatcher has no other way back to it.
        for message in [WAKE_MESSAGE, FIRST_PROMPT] {
            assert!(message.contains(POLICY_FILE), "{message}");
            // A pointer, not a copy: the file stays the single source.
            assert!(message.len() < 400, "{message}");
        }
        // The ambient name must not appear: it would govern unrelated sessions.
        assert!(!WAKE_MESSAGE.contains("AGENTS.md"));
        assert!(!FIRST_PROMPT.contains("AGENTS.md"));
    }

    #[test]
    fn a_dormant_record_is_resumed_with_its_own_session_id() {
        let steps = plan(&[entry("S1", None, "2026-07-26T10:00:00Z")], &argv(&["pi"]));
        let Wake::Resume { argv: resume, mode } = &steps[0] else {
            panic!("expected a resume");
        };
        // The session id must survive, or every worker's reply handle breaks.
        assert_eq!(resume, &argv(&["pi", "--session", "S1"]));
        // Hidden, or a window pops up on every todo edit.
        assert!(mode.hidden);
    }

    #[test]
    fn no_record_plans_a_single_fresh_spawn() {
        assert_eq!(
            plan(&[], &argv(&["pi"])),
            vec![Wake::Spawn {
                argv: argv(&["pi"]),
                mode: hidden_mode()
            }]
        );
    }

    #[test]
    fn the_most_recently_seen_record_wins() {
        let entries = vec![
            entry("OLD", None, "2026-07-20T10:00:00Z"),
            entry(
                "NEW",
                Some("/home/me/todos/.corral/NEW.sock"),
                "2026-07-26T10:00:00Z",
            ),
        ];
        assert_eq!(
            plan(&entries, &argv(&["pi"]))[0],
            Wake::Inject {
                socket: PathBuf::from("/home/me/todos/.corral/NEW.sock")
            }
        );
    }

    #[test]
    fn a_dormant_record_with_no_resume_command_plans_only_a_fresh_spawn() {
        let mut e = entry("S1", None, "2026-07-26T10:00:00Z");
        e.resume_command = None;
        assert_eq!(
            plan(&[e], &argv(&["pi"])),
            vec![Wake::Spawn {
                argv: argv(&["pi"]),
                mode: hidden_mode()
            }]
        );
    }

    #[test]
    fn every_step_is_hidden() {
        let entries = vec![entry(
            "S1",
            Some("/x/.corral/S1.sock"),
            "2026-07-26T10:00:00Z",
        )];
        for step in plan(&entries, &argv(&["pi"])) {
            if let Some((_, mode)) = step.launch_args() {
                assert!(mode.hidden, "the dispatcher is background machinery");
            }
        }
    }

    #[test]
    fn each_branch_names_itself_for_the_log() {
        let entries = vec![entry(
            "S1",
            Some("/x/.corral/S1.sock"),
            "2026-07-26T10:00:00Z",
        )];
        let names: Vec<&str> = plan(&entries, &argv(&["pi"]))
            .iter()
            .map(|s| s.kind_name())
            .collect();
        assert_eq!(names, vec!["inject", "resume", "spawn"]);
    }

    #[test]
    fn the_wake_message_names_the_file_not_the_work() {
        // The file, not the message, tells the dispatcher what to do, so the
        // message must carry no task detail.
        assert!(WAKE_MESSAGE.contains("todo.txt"));
        assert!(!WAKE_MESSAGE.contains("spawn"));
    }
}
