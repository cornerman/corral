//! The single definition of the order work is taken in. Pure.
//!
//! It lives here rather than in the dispatcher's policy because ordering is a
//! deterministic rule: expressed as prose for an LLM it has to be re-derived,
//! and possibly got wrong, on every wake. `corral-todo list` emits items already
//! ordered, so the policy can say "take them in the order listed". Stage 2's
//! TODO column consumes the same function, so the board and the CLI cannot
//! disagree about what comes first.

use crate::item::Item;

/// Sort into dispatch order: by todo.txt priority `(A)`-`(Z)` first, then oldest
/// creation date first, then the order the file already had.
///
/// An item with no priority sorts after every prioritized one, and an item with
/// no creation date after every dated one, so "unmarked" always means "later"
/// and never jumps the queue. The sort is stable, so ties keep file order, which
/// makes the output reproducible and a diff readable.
pub fn dispatch_order(items: &mut [Item]) {
    items.sort_by(|a, b| {
        let key = |i: &Item| {
            (
                // `None` is greater than any `Some` in Option's own ordering,
                // which is exactly "unprioritized last".
                i.priority,
                i.creation_date.clone(),
            )
        };
        let (ap, ad) = key(a);
        let (bp, bd) = key(b);
        ap.is_none()
            .cmp(&bp.is_none())
            .then(ap.cmp(&bp))
            .then(ad.is_none().cmp(&bd.is_none()))
            .then(ad.cmp(&bd))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.key("id").unwrap()).collect()
    }

    #[test]
    fn priority_beats_age() {
        let mut items = vec![
            Item::parse("2026-07-01 ancient but unmarked id:old").unwrap(),
            Item::parse("(A) 2026-07-28 urgent and new id:urg").unwrap(),
        ];
        dispatch_order(&mut items);
        assert_eq!(ids(&items), vec!["urg", "old"]);
    }

    #[test]
    fn letters_order_a_before_c() {
        let mut items = vec![
            Item::parse("(C) 2026-07-01 middling id:c1").unwrap(),
            Item::parse("(A) 2026-07-28 top id:a1").unwrap(),
            Item::parse("(B) 2026-07-15 next id:b1").unwrap(),
        ];
        dispatch_order(&mut items);
        assert_eq!(ids(&items), vec!["a1", "b1", "c1"]);
    }

    #[test]
    fn oldest_first_within_one_priority() {
        let mut items = vec![
            Item::parse("(A) 2026-07-28 newer id:new").unwrap(),
            Item::parse("(A) 2026-07-01 older id:old").unwrap(),
        ];
        dispatch_order(&mut items);
        assert_eq!(ids(&items), vec!["old", "new"]);
    }

    #[test]
    fn unmarked_items_keep_file_order_among_themselves() {
        // A stable sort, so an operator's own sequence survives where the rule
        // has nothing to say.
        let mut items = vec![
            Item::parse("2026-07-10 first written id:f1").unwrap(),
            Item::parse("2026-07-10 second written id:s1").unwrap(),
            Item::parse("2026-07-10 third written id:t1").unwrap(),
        ];
        dispatch_order(&mut items);
        assert_eq!(ids(&items), vec!["f1", "s1", "t1"]);
    }

    #[test]
    fn an_undated_item_sorts_after_dated_ones() {
        let mut items = vec![
            Item::parse("no date at all id:und").unwrap(),
            Item::parse("2026-07-28 dated id:dat").unwrap(),
        ];
        dispatch_order(&mut items);
        assert_eq!(ids(&items), vec!["dat", "und"]);
    }
}
