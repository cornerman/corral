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

/// The columns a card can be moved *into*, left to right. Requires Action is
/// excluded: corral cannot make an agent open a question, so it is a valid
/// source but never a destination.
pub const DESTINATIONS: [Column; 3] = [Column::Idle, Column::Running, Column::Dormant];

/// The columns the ghost may rest on when moving a card out of `source`, in
/// display order: every destination, plus the source itself so the operator
/// can drop back where they started to cancel (`action_for(source, source)` is
/// a no-op). Requires Action is thus a stop only when it *is* the source.
pub fn stops(source: Column) -> Vec<Column> {
    Column::ALL
        .into_iter()
        .filter(|c| *c == source || DESTINATIONS.contains(c))
        .collect()
}

/// Slide the ghost one step across `stops(source)`, clamped at the ends.
/// `right` steps toward Dormant, else toward Idle. Shared by both shells so
/// keyboard-move and drag-target agree on where the ghost can rest.
pub fn slide_target(source: Column, current: Column, right: bool) -> Column {
    let stops = stops(source);
    let i = stops.iter().position(|&c| c == current).unwrap_or(0);
    let next = if right {
        (i + 1).min(stops.len() - 1)
    } else {
        i.saturating_sub(1)
    };
    stops[next]
}

/// The column a move first targets when entering move mode from `source` in a
/// given direction: one step off the source across `stops(source)` (clamped, so
/// grabbing at an edge and pressing into the wall rests on the source = a
/// cancel until the operator slides the other way).
pub fn initial_target(source: Column, right: bool) -> Column {
    slide_target(source, source, right)
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
    fn stops_include_source_and_destinations() {
        // A destination source: stops are just the destinations.
        assert_eq!(stops(Running), vec![Idle, Running, Dormant]);
        // Requires Action source: it is a stop too (so you can cancel by
        // dropping back home), in display order.
        assert_eq!(
            stops(RequiresAction),
            vec![RequiresAction, Idle, Running, Dormant]
        );
    }

    #[test]
    fn slide_clamps_within_stops() {
        // From a Running-origin card (stops = Idle/Running/Dormant).
        assert_eq!(slide_target(Running, Idle, false), Idle); // clamp left
        assert_eq!(slide_target(Running, Idle, true), Running);
        assert_eq!(slide_target(Running, Running, true), Dormant);
        assert_eq!(slide_target(Running, Dormant, true), Dormant); // clamp right
                                                                   // A Requires-Action-origin card can slide back onto Requires Action.
        assert_eq!(slide_target(RequiresAction, Idle, false), RequiresAction);
        assert_eq!(slide_target(RequiresAction, RequiresAction, true), Idle);
    }

    #[test]
    fn can_rest_on_source_to_cancel() {
        // Sliding back to the source column yields a no-op action = cancel.
        assert_eq!(
            action_for(Running, slide_target(Running, Idle, true)),
            MoveAction::NoOp
        );
        assert_eq!(
            action_for(RequiresAction, slide_target(RequiresAction, Idle, false)),
            MoveAction::NoOp
        );
    }

    #[test]
    fn initial_target_steps_off_source() {
        assert_eq!(initial_target(Running, false), Idle);
        assert_eq!(initial_target(Running, true), Dormant);
        assert_eq!(initial_target(Idle, true), Running);
        assert_eq!(initial_target(Idle, false), Idle); // clamp at left stop
                                                       // Requires Action steps right to Idle; left clamps onto itself (cancel).
        assert_eq!(initial_target(RequiresAction, true), Idle);
        assert_eq!(initial_target(RequiresAction, false), RequiresAction);
    }

    #[test]
    fn confirms_on_reaching_target() {
        assert!(confirms(Idle, Idle));
        assert!(!confirms(Idle, Running));
    }
}
