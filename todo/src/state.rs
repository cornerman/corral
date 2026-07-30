//! Apply a state change to one item. Pure: the caller supplies today's date.
//!
//! A change is validated fully before any field is written, so a refused
//! change never leaves the item half-applied.

use crate::item::{Item, State};

/// A requested state change. `target` and `worker` are recorded at different
/// moments (dispatch, then handshake), so `None` means "leave as is" rather
/// than "clear" — only a move to `Open` clears them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub state: State,
    pub target: Option<String>,
    pub worker: Option<String>,
    pub reason: Option<String>,
}

/// A todo.txt `key:value` value is whitespace-delimited, so a value with a
/// space in it cannot be written at all.
fn check_value(name: &str, value: &str) -> Result<(), String> {
    if value.split_whitespace().count() != 1 {
        return Err(format!(
            "{name} value {value:?} contains a space; todo.txt key values cannot"
        ));
    }
    Ok(())
}

pub fn apply(item: &mut Item, change: &Change, today: &str) -> Result<(), String> {
    if let Some(t) = &change.target {
        check_value("target", t)?;
    }
    if let Some(w) = &change.worker {
        check_value("worker", w)?;
    }

    if let Some(reason) = &change.reason {
        // One task is one line, so a reason joins the task text instead of
        // living on a note line of its own.
        let flat = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        let keys: Vec<String> = item
            .rest
            .split_whitespace()
            .filter(|t| t.contains(':'))
            .map(|t| t.to_string())
            .collect();
        let prose: Vec<String> = item
            .rest
            .split_whitespace()
            .filter(|t| !t.contains(':'))
            .map(|t| t.to_string())
            .collect();
        item.rest = format!(
            "{} -- blocked: {} {}",
            prose.join(" "),
            flat,
            keys.join(" ")
        )
        .trim()
        .to_string();
    }

    match change.state {
        State::Done => {
            item.completed = true;
            item.completion_date = Some(today.to_string());
            item.remove_key("status");
        }
        State::Open => {
            item.completed = false;
            item.completion_date = None;
            item.remove_key("status");
            item.remove_key("target");
            item.remove_key("worker");
        }
        State::Progress | State::Blocked => {
            item.completed = false;
            item.completion_date = None;
            let value = change
                .state
                .status_value()
                .expect("progress/blocked have a value");
            item.set_key("status", value);
        }
    }

    if change.state != State::Open {
        if let Some(t) = &change.target {
            item.set_key("target", t);
        }
        if let Some(w) = &change.worker {
            item.set_key("worker", w);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    fn change(state: State) -> Change {
        Change {
            state,
            target: None,
            worker: None,
            reason: None,
        }
    }

    #[test]
    fn progress_records_target_and_worker() {
        let mut item = Item::parse("do it id:a7f").unwrap();
        let c = Change {
            state: State::Progress,
            target: Some("/home/me/projects/api".into()),
            worker: Some("01H2XABC".into()),
            reason: None,
        };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert_eq!(
            item.render(),
            "do it id:a7f status:progress target:/home/me/projects/api worker:01H2XABC"
        );
    }

    #[test]
    fn progress_without_target_keeps_an_existing_one() {
        // The dispatcher records the target at dispatch and the worker later,
        // in a second call, so an omitted field must not erase the first.
        let mut item = Item::parse("do it id:a7f status:progress target:/srv/x").unwrap();
        let c = Change {
            state: State::Progress,
            target: None,
            worker: Some("W1".into()),
            reason: None,
        };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert_eq!(item.key("target"), Some("/srv/x"));
        assert_eq!(item.key("worker"), Some("W1"));
    }

    #[test]
    fn blocked_appends_the_reason_to_the_task_text() {
        let mut item = Item::parse("port the parser tests id:m4z").unwrap();
        let c = Change {
            state: State::Blocked,
            target: None,
            worker: None,
            reason: Some("which fixture format?".into()),
        };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert_eq!(
            item.render(),
            "port the parser tests -- blocked: which fixture format? id:m4z status:blocked"
        );
    }

    #[test]
    fn done_marks_completion_and_drops_the_status_key() {
        let mut item = Item::parse("2026-07-23 do it id:a7f status:progress worker:W1").unwrap();
        apply(&mut item, &change(State::Done), "2026-07-26").unwrap();
        assert_eq!(
            item.render(),
            "x 2026-07-26 2026-07-23 do it id:a7f worker:W1"
        );
        assert_eq!(item.state(), State::Done);
    }

    #[test]
    fn open_clears_progress_metadata_so_the_item_is_dispatchable_again() {
        let mut item = Item::parse("do it id:a7f status:progress target:/srv/x worker:W1").unwrap();
        apply(&mut item, &change(State::Open), "2026-07-26").unwrap();
        assert_eq!(item.render(), "do it id:a7f");
    }

    #[test]
    fn a_target_containing_a_space_is_refused_loudly() {
        // todo.txt has no quoting: a key value cannot contain a space, so
        // mangling it silently is not an option.
        let mut item = Item::parse("do it id:a7f").unwrap();
        let c = Change {
            state: State::Progress,
            target: Some("/home/me/my projects/api".into()),
            worker: None,
            reason: None,
        };
        let err = apply(&mut item, &c, "2026-07-26").unwrap_err();
        assert!(err.contains("space"), "{err}");
        assert_eq!(
            item.render(),
            "do it id:a7f",
            "a refused change must not half-apply"
        );
    }

    #[test]
    fn a_reason_is_collapsed_to_one_line() {
        let mut item = Item::parse("do it id:a7f").unwrap();
        let c = Change {
            state: State::Blocked,
            target: None,
            worker: None,
            reason: Some("line one\nline two".into()),
        };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert!(!item.render().contains('\n'));
    }
}
