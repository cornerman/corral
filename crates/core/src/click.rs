//! Double-click classification, shared by both shells so the click model is
//! identical (and unit-tested once). Neither ratatui nor iced reports a native
//! double-click, so each shell feeds every left press through a `ClickTracker`:
//! a second press on the same card within the threshold is a "go", every other
//! press just selects. The tracker owns the last-press memory; the shells own
//! only the rendering.

use std::time::{Duration, Instant};

/// The default double-click window (typical desktop default). A second press
/// on the same card within this of the first counts as a double-click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What a left press means once classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// Single click: select the card only, never navigate.
    Select,
    /// Double click: go to the card (same as Enter).
    Go,
}

/// Tracks the last left press so a quick second press on the same card is a
/// double-click. One instance per board.
pub struct ClickTracker {
    threshold: Duration,
    last: Option<(usize, Instant)>,
}

impl Default for ClickTracker {
    fn default() -> Self {
        Self::new(DOUBLE_CLICK)
    }
}

impl ClickTracker {
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            last: None,
        }
    }

    /// Classify a left press on card `idx` at time `now`. A press on the same
    /// card within the threshold of the previous one is `Go`; anything else is
    /// `Select`. A `Go` clears the memory so a triple click is not two gos.
    pub fn press(&mut self, idx: usize, now: Instant) -> ClickKind {
        let double = matches!(
            self.last,
            Some((last_idx, last_at))
                if last_idx == idx && now.duration_since(last_at) <= self.threshold
        );
        if double {
            self.last = None;
            ClickKind::Go
        } else {
            self.last = Some((idx, now));
            ClickKind::Select
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_card_within_threshold_goes() {
        let mut t = ClickTracker::new(Duration::from_millis(400));
        let base = Instant::now();
        assert_eq!(t.press(2, base), ClickKind::Select);
        assert_eq!(t.press(2, base + Duration::from_millis(200)), ClickKind::Go);
    }

    #[test]
    fn different_card_only_selects() {
        let mut t = ClickTracker::new(Duration::from_millis(400));
        let base = Instant::now();
        assert_eq!(t.press(2, base), ClickKind::Select);
        assert_eq!(
            t.press(3, base + Duration::from_millis(100)),
            ClickKind::Select
        );
    }

    #[test]
    fn over_threshold_only_selects() {
        let mut t = ClickTracker::new(Duration::from_millis(400));
        let base = Instant::now();
        assert_eq!(t.press(2, base), ClickKind::Select);
        assert_eq!(
            t.press(2, base + Duration::from_millis(500)),
            ClickKind::Select
        );
    }

    #[test]
    fn go_resets_so_triple_click_is_not_two_gos() {
        let mut t = ClickTracker::new(Duration::from_millis(400));
        let base = Instant::now();
        assert_eq!(t.press(2, base), ClickKind::Select);
        assert_eq!(t.press(2, base + Duration::from_millis(100)), ClickKind::Go);
        // Third quick press starts a fresh single click, not another go.
        assert_eq!(
            t.press(2, base + Duration::from_millis(200)),
            ClickKind::Select
        );
    }
}
