//! The pure mapping from a card move (source column → destination column) to
//! the real agent action it triggers, plus the confirmation predicate the
//! shells use to decide when a pending move has landed. This is the single
//! source of the transition table; both the TUI and the GUI consume it, so the
//! two shells can never disagree on what dragging a card means.
//!
//! Moving a card is only a trigger: the shell fires the `MoveAction`, then
//! waits for the agent's own state to reach the target column before relocating
//! the card (see `confirms`). The board never paints a state the agent has not
//! reached.

use crate::model::Column;

/// The real agent action a card move triggers. Computed purely from the source
/// and destination columns via `action_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveAction {
    /// Running → Idle, or Requires Action → Idle: abort the current turn
    /// (`session/cancel`). Aborting also unblocks a pending `question`.
    Cancel,
    /// Idle → Running: start a turn by sending the literal nudge `"continue"`
    /// (`session/prompt`; an empty prompt is rejected by corral-pi).
    Nudge,
    /// Any live column → Dormant: kill the window pid (the agent goes dormant
    /// and resumable), the same effect as the `d` key.
    Kill,
    /// Dormant → Idle: resume the session (it comes back idle).
    Resume,
    /// Dormant → Running: resume, then nudge, since a plain resume lands idle
    /// and the only way to honor a drop onto Running is to start a turn.
    ResumeAndNudge,
    /// No meaningful transition: same column, or any move *into* Requires
    /// Action (corral cannot make an agent open a question).
    NoOp,
}

/// The action that moving a card from `from` to `to` triggers. See `MoveAction`
/// for the vocabulary and the design table.
pub fn action_for(from: Column, to: Column) -> MoveAction {
    use Column::*;
    use MoveAction::*;
    // Requires Action is never a valid destination.
    if to == RequiresAction || from == to {
        return NoOp;
    }
    match (from, to) {
        (Running, Idle) | (RequiresAction, Idle) => Cancel,
        (Idle, Running) => Nudge,
        (RequiresAction | Idle | Running, Dormant) => Kill,
        (Dormant, Idle) => Resume,
        (Dormant, Running) => ResumeAndNudge,
        // (Running, RequiresAction) etc. are already caught by the guard above.
        _ => NoOp,
    }
}

/// Whether a pending move to `target` has landed, given the agent's current
/// column `now`. A move confirms exactly when the agent has reached the target
/// column (its own `state_update` or a live/dormant registry transition moved
/// it there). Until then the card stays put with an in-flight badge.
pub fn confirms(target: Column, now: Column) -> bool {
    target == now
}

#[cfg(test)]
mod tests {
    use super::*;
    use Column::*;

    #[test]
    fn live_state_transitions() {
        assert_eq!(action_for(Running, Idle), MoveAction::Cancel);
        assert_eq!(action_for(RequiresAction, Idle), MoveAction::Cancel);
        assert_eq!(action_for(Idle, Running), MoveAction::Nudge);
    }

    #[test]
    fn kill_from_any_live_column() {
        assert_eq!(action_for(RequiresAction, Dormant), MoveAction::Kill);
        assert_eq!(action_for(Idle, Dormant), MoveAction::Kill);
        assert_eq!(action_for(Running, Dormant), MoveAction::Kill);
    }

    #[test]
    fn resume_from_dormant() {
        assert_eq!(action_for(Dormant, Idle), MoveAction::Resume);
        assert_eq!(action_for(Dormant, Running), MoveAction::ResumeAndNudge);
    }

    #[test]
    fn requires_action_is_never_a_destination() {
        for from in Column::ALL {
            assert_eq!(action_for(from, RequiresAction), MoveAction::NoOp);
        }
    }

    #[test]
    fn same_column_is_noop() {
        for c in Column::ALL {
            assert_eq!(action_for(c, c), MoveAction::NoOp);
        }
    }

    #[test]
    fn confirms_on_reaching_target() {
        assert!(confirms(Idle, Idle));
        assert!(!confirms(Idle, Running));
    }
}
