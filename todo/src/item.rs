//! One todo.txt line, in and out. Pure: no IO, no clock.
//!
//! The prose and its `key:value` tokens stay together in `rest` and key access
//! scans whitespace-separated tokens, so an operator's own words, `+projects`,
//! `@contexts` and token order survive a parse/render round trip; a parsed-out
//! key map would reorder them. Whitespace is the one thing not preserved: runs
//! of spaces collapse to one, since parsing splits on whitespace.

/// The state of an item, read from the line itself rather than a side index.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum State {
    Open,
    Progress,
    Blocked,
    Done,
}

impl State {
    /// The `status:` value this state is written as, or `None` when the state
    /// needs no key (`Open` is the absence of one; `Done` is the `x` marker).
    pub fn status_value(self) -> Option<&'static str> {
        match self {
            State::Progress => Some("progress"),
            State::Blocked => Some("blocked"),
            State::Open | State::Done => None,
        }
    }
}

/// A todo.txt item: the optional leading fields in their fixed order, then
/// everything else verbatim.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Item {
    pub completed: bool,
    pub completion_date: Option<String>,
    pub priority: Option<char>,
    pub creation_date: Option<String>,
    pub rest: String,
}

fn is_date(token: &str) -> bool {
    // todo.txt dates are exactly YYYY-MM-DD; checking the shape is enough to
    // tell a date field from the first prose word.
    token.len() == 10
        && token.as_bytes()[4] == b'-'
        && token.as_bytes()[7] == b'-'
        && token
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
}

fn is_priority(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 3 && b[0] == b'(' && b[2] == b')' && b[1].is_ascii_uppercase()
}

impl Item {
    /// Parse one line. A blank line is not an item (`None`), so the store can
    /// keep the file's blank lines out of the item list.
    pub fn parse(line: &str) -> Option<Item> {
        let mut words = line.split_whitespace().peekable();
        words.peek()?;
        let mut item = Item {
            completed: false,
            completion_date: None,
            priority: None,
            creation_date: None,
            rest: String::new(),
        };
        if words.peek() == Some(&"x") {
            words.next();
            item.completed = true;
        }
        if words.peek().is_some_and(|w| is_priority(w)) {
            item.priority = words
                .next()
                .and_then(|w| w.as_bytes().get(1).map(|b| *b as char));
        }
        // A completed line carries completion then creation; an open line
        // carries only creation. Both are optional.
        if words.peek().is_some_and(|w| is_date(w)) {
            let first = words.next().unwrap().to_string();
            if item.completed && words.peek().is_some_and(|w| is_date(w)) {
                item.completion_date = Some(first);
                item.creation_date = words.next().map(|w| w.to_string());
            } else if item.completed {
                item.completion_date = Some(first);
            } else {
                item.creation_date = Some(first);
            }
        }
        // A priority appearing *after* the dates is deliberately not parsed: it
        // would render back before them (render has one fixed field order) and
        // so break the round trip. Left in `rest`, it survives untouched.
        item.rest = words.collect::<Vec<_>>().join(" ");
        Some(item)
    }

    /// Render back to a todo.txt line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.completed {
            out.push_str("x ");
        }
        if let Some(p) = self.priority {
            out.push('(');
            out.push(p);
            out.push_str(") ");
        }
        if let Some(d) = &self.completion_date {
            out.push_str(d);
            out.push(' ');
        }
        if let Some(d) = &self.creation_date {
            out.push_str(d);
            out.push(' ');
        }
        out.push_str(&self.rest);
        out.trim_end().to_string()
    }

    /// The value of a `key:value` token, if the line carries one.
    pub fn key(&self, name: &str) -> Option<&str> {
        self.rest
            .split_whitespace()
            .find_map(|t| t.strip_prefix(name)?.strip_prefix(':'))
    }

    /// Set a `key:value` token: replaced where it already is (so token order
    /// is stable), else appended at the end.
    pub fn set_key(&mut self, name: &str, value: &str) {
        let prefix = format!("{name}:");
        let mut found = false;
        let mut tokens: Vec<String> = self
            .rest
            .split_whitespace()
            .map(|t| {
                if t.starts_with(&prefix) {
                    found = true;
                    format!("{prefix}{value}")
                } else {
                    t.to_string()
                }
            })
            .collect();
        if !found {
            tokens.push(format!("{prefix}{value}"));
        }
        self.rest = tokens.join(" ");
    }

    /// Drop a `key:value` token if present.
    pub fn remove_key(&mut self, name: &str) {
        let prefix = format!("{name}:");
        self.rest = self
            .rest
            .split_whitespace()
            .filter(|t| !t.starts_with(&prefix))
            .collect::<Vec<_>>()
            .join(" ");
    }

    /// The item's state. `x` wins over any `status:` key, because every other
    /// todo.txt tool reads completion from the marker; an unrecognized status
    /// value reads as `Open` so a future value cannot strand an item.
    pub fn state(&self) -> State {
        if self.completed {
            return State::Done;
        }
        match self.key("status") {
            Some("progress") => State::Progress,
            Some("blocked") => State::Blocked,
            _ => State::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_plain_line() {
        let line = "2026-07-25 add a --dry-run flag id:a7f +deploy";
        assert_eq!(Item::parse(line).unwrap().render(), line);
    }

    #[test]
    fn round_trips_a_prioritized_completed_line() {
        let line = "x 2026-07-25 2026-07-23 bump the pinned toolchain id:b8c";
        let item = Item::parse(line).unwrap();
        assert!(item.completed);
        assert_eq!(item.completion_date.as_deref(), Some("2026-07-25"));
        assert_eq!(item.creation_date.as_deref(), Some("2026-07-23"));
        assert_eq!(item.render(), line);
    }

    #[test]
    fn reads_priority_and_keys() {
        let item = Item::parse("(A) 2026-07-25 review it id:k2q status:progress").unwrap();
        assert_eq!(item.priority, Some('A'));
        assert_eq!(item.key("id"), Some("k2q"));
        assert_eq!(item.key("status"), Some("progress"));
        assert_eq!(item.key("worker"), None);
    }

    #[test]
    fn blank_line_is_not_an_item() {
        assert!(Item::parse("   ").is_none());
    }

    #[test]
    fn set_key_replaces_in_place_and_appends_when_absent() {
        let mut item = Item::parse("do it id:a7f status:progress").unwrap();
        item.set_key("status", "blocked");
        assert_eq!(item.render(), "do it id:a7f status:blocked");
        item.set_key("worker", "01H2X");
        assert_eq!(item.render(), "do it id:a7f status:blocked worker:01H2X");
    }

    #[test]
    fn remove_key_drops_the_token() {
        let mut item = Item::parse("do it id:a7f status:progress worker:01H2X").unwrap();
        item.remove_key("worker");
        item.remove_key("status");
        assert_eq!(item.render(), "do it id:a7f");
    }

    #[test]
    fn state_comes_from_the_line_alone() {
        let s = |l: &str| Item::parse(l).unwrap().state();
        assert_eq!(s("open one id:a"), State::Open);
        assert_eq!(s("one id:a status:progress"), State::Progress);
        assert_eq!(s("one id:a status:blocked"), State::Blocked);
        assert_eq!(s("x 2026-07-25 one id:a"), State::Done);
        // A completed line wins over a stale status key, since `x` is what
        // every other todo.txt tool reads.
        assert_eq!(s("x 2026-07-25 one id:a status:progress"), State::Done);
    }

    #[test]
    fn collapses_whitespace_but_loses_nothing_else() {
        // The round trip is whitespace-normalized, not byte-exact. Asserted so
        // the behavior is a decision rather than a surprise.
        let item = Item::parse("   do   it    id:a7f").unwrap();
        assert_eq!(item.render(), "do it id:a7f");
    }

    #[test]
    fn a_priority_after_the_dates_stays_in_the_text() {
        // Parsing it would render it back before the dates and break the round
        // trip, so it is left alone.
        let line = "x 2026-07-25 (A) do it id:a7f";
        let item = Item::parse(line).unwrap();
        assert_eq!(item.priority, None);
        assert_eq!(item.render(), line);
    }

    #[test]
    fn an_unknown_status_value_reads_as_open() {
        // Forward-compatible: a status the dispatcher does not know must not
        // strand the item outside every column.
        assert_eq!(
            Item::parse("one id:a status:wat").unwrap().state(),
            State::Open
        );
    }
}
