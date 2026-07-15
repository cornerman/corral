//! The right-click context menu model, shared by both shells so the entries,
//! their order, and their labels are single-sourced (only the rendering
//! differs). Each entry maps to the same action a footer key already performs
//! on the selected card, and `label()` returns the exact footer verb for that
//! action — the footer is the source of truth for these strings, so the menu
//! and both footers cannot drift apart. Labels are fixed (not adapted to card
//! state): the action does the right per-state thing (`Go` focuses a live
//! window, reveals a hidden card, or resumes a dormant one), the label just
//! names the verb.

/// A context-menu entry, in display order. `Dismiss` is last because it is the
/// destructive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Go,
    Message,
    Spawn,
    ToggleHidden,
    Dismiss,
}

impl MenuAction {
    /// Every entry in display order (footer order, destructive last). Both
    /// shells iterate this to build the menu, so the order cannot drift.
    pub const ALL: [MenuAction; 5] = [
        MenuAction::Go,
        MenuAction::Message,
        MenuAction::Spawn,
        MenuAction::ToggleHidden,
        MenuAction::Dismiss,
    ];

    /// The label shown for this entry: the exact verb the footer prints for the
    /// same action, so the context menu reads identically to the footer key
    /// hint (footer is the source of truth; both shells' footers reuse these).
    pub fn label(self) -> &'static str {
        match self {
            MenuAction::Go => "go",
            MenuAction::Message => "msg",
            MenuAction::Spawn => "new",
            MenuAction::ToggleHidden => "hide/show",
            MenuAction::Dismiss => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_in_footer_order_dismiss_last() {
        assert_eq!(
            MenuAction::ALL,
            [
                MenuAction::Go,
                MenuAction::Message,
                MenuAction::Spawn,
                MenuAction::ToggleHidden,
                MenuAction::Dismiss,
            ]
        );
        assert_eq!(*MenuAction::ALL.last().unwrap(), MenuAction::Dismiss);
    }

    #[test]
    fn every_entry_has_a_label() {
        for a in MenuAction::ALL {
            assert!(!a.label().is_empty());
        }
    }
}
