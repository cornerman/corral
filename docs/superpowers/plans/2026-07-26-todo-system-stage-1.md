# Todo System Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `corral-todo`, a workspace crate whose CLI owns a todo.txt file under a lock and whose `watch` subcommand wakes exactly one dispatcher agent whenever that file changes.

**Architecture:** A pure core (parse one todo.txt line, coin ids, read and set state, decide how to wake) with a thin imperative shell (lock the file, rewrite it atomically, poll, talk to `corral-core`). Stage 1 changes nothing inside `corral`, `corral-gui` or `corrald`: it is a client of `corral-core` exactly as an outside program would be. The dispatcher's behavior is not code; it is the prose policy already written in `todo/DISPATCHER.md`, which the live todo directory symlinks as its `AGENTS.md`.

**Tech Stack:** Rust (pinned by `rust-toolchain.toml`), `corral-core` for registry discovery / prompt injection / launch, `libc` for `flock`, `tempfile` for tests. No new third-party dependency.

## Global Constraints

- Read `todo/SPEC.md` before starting. It is the specification; this plan implements its stage 1 only ("The File Format", "The CLI", "The Watcher", "Serialization and Loop Safety"). Do not implement anything from "Board Integration and Column Mapping" — that is stage 2 and has open design questions.
- The crate lives in `todo/`, not `crates/`, so the design docs and the code that implements them sit together. Workspace member path is `todo`.
- Crate name `corral-todo`, binary name `corral-todo`.
- Edition 2021, `version.workspace = true`, `edition.workspace = true`, `license.workspace = true`, matching `crates/core/Cargo.toml`.
- No new external crates. `serde_json` and `libc` are already in the workspace lockfile; `tempfile` is already a dev-dependency elsewhere. If you believe you need another, stop and ask.
- Every file gets a header comment saying what it is responsible for, in the style of `crates/core/Cargo.toml` and `crates/core/src/discovery.rs`.
- Comments explain **why** the code is the way it is, referring only to the current code. Never write a comment about what the code used to do.
- The file format is todo.txt (`https://github.com/todotxt/todo.txt`), not a dialect. A `key:value` value cannot contain a space. A task is exactly one line.
- Every write path takes an exclusive `flock` and rewrites through temp-file-plus-rename. There is no unlocked write anywhere.
- `set` with an unknown id exits nonzero with a message. Nothing fails quietly (AGENTS.md: "Fail fast and loud").
- Commit after every task, one line, no attribution, no "co-authored".
- Verify before claiming done: `cargo test -p corral-todo` and `cargo clippy -p corral-todo -- -D warnings` must both pass, and you must paste the output.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `todo/Cargo.toml` | Crate manifest. |
| `todo/src/item.rs` | Pure: one todo.txt line in and out (parse, render, read/set/remove a `key:value`, read state). |
| `todo/src/normalize.rs` | Pure: coin a missing `id:`, stamp a missing creation date. |
| `todo/src/state.rs` | Pure: apply a state change to an `Item` (progress / blocked / done / open). |
| `todo/src/store.rs` | Shell: the locked, atomic read-modify-write of `todo.txt`, plus the `done.txt` archive move. |
| `todo/src/wake.rs` | Pure: decide which of the three wake branches a registry scan implies. |
| `todo/src/watch.rs` | Shell: the poll loop (normalize, hash, compare) executing a `Wake`. |
| `todo/src/lib.rs` | Module wiring and the crate-level doc comment. |
| `todo/src/main.rs` | CLI argument parsing and subcommand dispatch. Hand-rolled; the workspace has no `clap`. |
| `todo/README.md` | First-run setup for a live todo directory. |

Tests are Rust unit tests in a `#[cfg(test)] mod tests` block at the bottom of each file, which is the convention every `crates/core` module follows. Do not create a `todo/tests/` directory.

---

### Task 1: Crate Scaffold and the todo.txt Line

**Files:**
- Create: `todo/Cargo.toml`, `todo/src/lib.rs`, `todo/src/item.rs`, `todo/src/main.rs`
- Modify: `Cargo.toml` (workspace `members`)

**Interfaces:**
- Consumes: nothing.
- Produces: `corral_todo::item::{Item, State}`; `Item { completed: bool, completion_date: Option<String>, priority: Option<char>, creation_date: Option<String>, rest: String }`; `Item::parse(&str) -> Option<Item>` (`None` for a blank line), `Item::render(&self) -> String`, `Item::key(&self, &str) -> Option<&str>`, `Item::set_key(&mut self, &str, &str)`, `Item::remove_key(&mut self, &str)`, `Item::state(&self) -> State`; `enum State { Open, Progress, Blocked, Done }`.

Design note to preserve: the item holds the prose plus its `key:value` tokens together in one `rest` string, and key access scans whitespace-separated tokens. That keeps the operator's own words, `+projects`, `@contexts` and token order byte-identical through a round trip, which a struct with a parsed-out key map would not.

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`:

```toml
members = ["crates/core", "crates/board", "crates/daemon", "crates/gui", "todo"]
```

Create `todo/Cargo.toml`:

```toml
# The todo system's one implementation of the todo.txt format: a pure core
# (line parse/render, id coinage, state changes, wake decision) plus the
# `corral-todo` CLI and its `watch` supervisor. Client of corral-core only;
# nothing in corral, corral-gui or corrald depends on this crate.
[package]
name = "corral-todo"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "todo.txt-backed task dispatch onto corral agents"

[dependencies]
corral-core = { path = "../crates/core" }
# flock(2) for the exclusive file lock; already a workspace dependency.
libc = "0.2"

[dev-dependencies]
tempfile = "3"
```

Create `todo/src/lib.rs`:

```rust
//! The todo system's library half. See `todo/SPEC.md` for the design and
//! `todo/DISPATCHER.md` for the agent policy this CLI serves.
pub mod item;
```

Create `todo/src/main.rs` as a placeholder that compiles:

```rust
fn main() {
    eprintln!("corral-todo: no subcommand implemented yet");
    std::process::exit(2);
}
```

- [ ] **Step 2: Write the failing tests**

In `todo/src/item.rs`:

```rust
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
    fn an_unknown_status_value_reads_as_open() {
        // Forward-compatible: a status the dispatcher does not know must not
        // strand the item outside every column.
        assert_eq!(Item::parse("one id:a status:wat").unwrap().state(), State::Open);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p corral-todo`
Expected: FAIL, `cannot find type Item in this scope` / unresolved module.

- [ ] **Step 4: Implement `todo/src/item.rs`**

```rust
//! One todo.txt line, in and out. Pure: no IO, no clock.
//!
//! The prose and its `key:value` tokens stay together in `rest` and key access
//! scans whitespace-separated tokens, so an operator's own words, `+projects`,
//! `@contexts` and token order survive a parse/render round trip byte for byte.
//! A parsed-out key map would reorder and reflow them.

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
        if words.peek().is_none() {
            return None;
        }
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
            item.priority = words.next().and_then(|w| w.as_bytes().get(1).map(|b| *b as char));
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
        // Priority may also follow the dates in the wild; accept it there too.
        if item.priority.is_none() && words.peek().is_some_and(|w| is_priority(w)) {
            item.priority = words.next().and_then(|w| w.as_bytes().get(1).map(|b| *b as char));
        }
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
```

Add `pub mod item;` to `todo/src/lib.rs` (already done in Step 1).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all 8 tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock todo/Cargo.toml todo/src
git commit -m "todo: add the corral-todo crate with the todo.txt line core"
```

---

### Task 2: Normalization (Ids and Creation Dates)

**Files:**
- Create: `todo/src/normalize.rs`
- Modify: `todo/src/lib.rs`

**Interfaces:**
- Consumes: `item::Item`.
- Produces: `corral_todo::normalize::{normalize, coin_id}`; `normalize(items: &mut [Item], today: &str) -> bool` returning whether anything changed; `coin_id(text: &str, taken: &dyn Fn(&str) -> bool) -> String`.

Design note to preserve: ids are derived by hashing the item's own text, not drawn from a counter or a random source. That keeps the crate dependency-free, makes the function pure and therefore testable without a seed, and gives the same line the same id on a re-run. Collisions are resolved by re-hashing with a salt, so uniqueness is still guaranteed against the ids already in the file.

- [ ] **Step 1: Write the failing tests**

In `todo/src/normalize.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    #[test]
    fn coins_ids_and_dates_for_a_bare_line() {
        let mut items = vec![Item::parse("brain dump this").unwrap()];
        assert!(normalize(&mut items, "2026-07-26"));
        assert_eq!(items[0].creation_date.as_deref(), Some("2026-07-26"));
        assert!(items[0].key("id").is_some());
    }

    #[test]
    fn is_idempotent_so_normalizing_is_not_a_change() {
        let mut items = vec![Item::parse("brain dump this").unwrap()];
        normalize(&mut items, "2026-07-26");
        let before = items[0].render();
        assert!(!normalize(&mut items, "2026-07-27"));
        assert_eq!(items[0].render(), before);
    }

    #[test]
    fn ids_are_unique_even_for_identical_text() {
        let mut items = vec![
            Item::parse("same text").unwrap(),
            Item::parse("same text").unwrap(),
        ];
        normalize(&mut items, "2026-07-26");
        assert_ne!(items[0].key("id"), items[1].key("id"));
    }

    #[test]
    fn does_not_restamp_a_completed_line() {
        let mut items = vec![Item::parse("x 2026-07-25 2026-07-23 done thing id:b8c").unwrap()];
        assert!(!normalize(&mut items, "2026-07-26"));
    }

    #[test]
    fn coined_ids_are_short_and_lowercase_alphanumeric() {
        let id = coin_id("anything", &|_| false);
        assert_eq!(id.len(), 3);
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn coin_id_avoids_taken_ids() {
        let first = coin_id("anything", &|_| false);
        let second = coin_id("anything", &|id| id == first);
        assert_ne!(first, second);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p corral-todo normalize`
Expected: FAIL, unresolved module `normalize`.

- [ ] **Step 3: Implement `todo/src/normalize.rs`**

```rust
//! Coin a missing `id:` and stamp a missing creation date. Pure: the caller
//! supplies today's date, so the function is testable without a clock.
//!
//! Normalization runs inside every read, so there is no way to look at the
//! file and see an unidentified item. It must therefore be idempotent: the
//! watcher hashes the *normalized* file, so a normalization that kept changing
//! bytes would wake the dispatcher forever.

use crate::item::Item;

/// FNV-1a, the same small non-cryptographic hash `core::palette` uses to key a
/// path to a color. An id only needs to be short, stable and unique within one
/// file, so a hash of the item's own text beats a counter (no state to carry)
/// and beats randomness (no dependency, and reproducible in tests).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn base36(mut n: u64, len: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; len];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out).expect("base36 digits are ascii")
}

/// A short id for an item, avoiding every id `taken` reports. Three base36
/// characters (46656 values) is plenty for a human todo file and stays easy to
/// quote in a task prompt and read back out of a report.
pub fn coin_id(text: &str, taken: &dyn Fn(&str) -> bool) -> String {
    for salt in 0u32..10_000 {
        let candidate = base36(fnv1a(format!("{text}{salt}").as_bytes()), 3);
        if !taken(&candidate) {
            return candidate;
        }
    }
    // 10k salted attempts all colliding means the id space is effectively
    // full; failing loud beats returning a duplicate id.
    panic!("could not coin a free id after 10000 attempts; the id space is full");
}

/// Stamp every open item with an `id:` and a creation date. Returns whether
/// anything changed, which is how the caller knows a rewrite is needed.
///
/// A completed line is left alone: it is history, and restamping it would
/// rewrite the file on every read.
pub fn normalize(items: &mut [Item], today: &str) -> bool {
    let mut changed = false;
    let mut taken: Vec<String> = items
        .iter()
        .filter_map(|i| i.key("id").map(|s| s.to_string()))
        .collect();
    for index in 0..items.len() {
        if items[index].completed {
            continue;
        }
        if items[index].key("id").is_none() {
            // Salt the hash input with the item's position so two identical
            // lines in one file still get different ids.
            let text = format!("{}#{index}", items[index].rest);
            let id = coin_id(&text, &|c| taken.iter().any(|t| t == c));
            taken.push(id.clone());
            items[index].set_key("id", &id);
            changed = true;
        }
        if items[index].creation_date.is_none() {
            items[index].creation_date = Some(today.to_string());
            changed = true;
        }
    }
    changed
}
```

Add `pub mod normalize;` to `todo/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 5: Commit**

```bash
git add todo/src
git commit -m "todo: coin ids and stamp creation dates idempotently"
```

---

### Task 3: State Changes

**Files:**
- Create: `todo/src/state.rs`
- Modify: `todo/src/lib.rs`

**Interfaces:**
- Consumes: `item::{Item, State}`.
- Produces: `corral_todo::state::{Change, apply}`; `struct Change { pub state: State, pub target: Option<String>, pub worker: Option<String>, pub reason: Option<String> }`; `apply(item: &mut Item, change: &Change, today: &str) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests**

In `todo/src/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    fn change(state: State) -> Change {
        Change { state, target: None, worker: None, reason: None }
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
        assert_eq!(item.render(), "do it id:a7f status:progress target:/home/me/projects/api worker:01H2XABC");
    }

    #[test]
    fn progress_without_target_keeps_an_existing_one() {
        // The dispatcher records the target at dispatch and the worker later,
        // in a second call, so an omitted field must not erase the first.
        let mut item = Item::parse("do it id:a7f status:progress target:/srv/x").unwrap();
        let c = Change { state: State::Progress, target: None, worker: Some("W1".into()), reason: None };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert_eq!(item.key("target"), Some("/srv/x"));
        assert_eq!(item.key("worker"), Some("W1"));
    }

    #[test]
    fn blocked_appends_the_reason_to_the_task_text() {
        let mut item = Item::parse("port the parser tests id:m4z").unwrap();
        let c = Change { state: State::Blocked, target: None, worker: None, reason: Some("which fixture format?".into()) };
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
        assert_eq!(item.render(), "x 2026-07-26 2026-07-23 do it id:a7f worker:W1");
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
        assert_eq!(item.render(), "do it id:a7f", "a refused change must not half-apply");
    }

    #[test]
    fn a_reason_is_collapsed_to_one_line() {
        let mut item = Item::parse("do it id:a7f").unwrap();
        let c = Change { state: State::Blocked, target: None, worker: None, reason: Some("line one\nline two".into()) };
        apply(&mut item, &c, "2026-07-26").unwrap();
        assert!(!item.render().contains('\n'));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p corral-todo state`
Expected: FAIL, unresolved module `state`.

- [ ] **Step 3: Implement `todo/src/state.rs`**

```rust
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
        item.rest = format!("{} -- blocked: {} {}", prose.join(" "), flat, keys.join(" "))
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
            let value = change.state.status_value().expect("progress/blocked have a value");
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
```

Add `pub mod state;` to `todo/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS. If the `blocked_appends_the_reason` expectation and your key-reordering differ, fix the **test's** expected string only if the rendered line is still valid todo.txt with the keys after the prose; do not weaken the assertion to `contains`.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 5: Commit**

```bash
git add todo/src
git commit -m "todo: apply state changes with all-or-nothing validation"
```

---

### Task 4: The Locked Store and the Archive

**Files:**
- Create: `todo/src/store.rs`
- Modify: `todo/src/lib.rs`

**Interfaces:**
- Consumes: `item::Item`, `normalize::normalize`.
- Produces: `corral_todo::store::Store`; `Store::new(path: impl Into<PathBuf>) -> Store`, `Store::path(&self) -> &Path`, `Store::with_lock<T>(&self, f: impl FnOnce(&mut Vec<Item>) -> Result<T, String>) -> Result<T, String>`, `Store::read_normalized(&self) -> Result<Vec<Item>, String>`, `Store::archive(&self) -> Result<usize, String>`, `Store::today() -> String`.

Design notes to preserve: the lock is taken on a sidecar `<todo.txt>.lock`, not on `todo.txt` itself, because the write path replaces `todo.txt` by rename — a lock held on the replaced inode would protect a file nobody is reading any more. And `with_lock` normalizes before calling `f`, so no caller can see an unidentified item.

- [ ] **Step 1: Write the failing tests**

In `todo/src/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(contents: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, contents).unwrap();
        (dir, Store::new(path))
    }

    #[test]
    fn reads_and_normalizes_then_persists_the_ids() {
        let (_dir, store) = store_with("first thing\nsecond thing\n");
        let items = store.read_normalized().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.key("id").is_some()));
        // Re-reading must find the same ids, i.e. the normalization was written
        // back rather than recomputed each time.
        let again = store.read_normalized().unwrap();
        assert_eq!(items[0].key("id"), again[0].key("id"));
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("todo.txt"));
        assert!(store.read_normalized().unwrap().is_empty());
    }

    #[test]
    fn with_lock_writes_back_the_mutated_items() {
        let (_dir, store) = store_with("do it id:a7f\n");
        store
            .with_lock(|items| {
                items[0].set_key("status", "progress");
                Ok(())
            })
            .unwrap();
        let items = store.read_normalized().unwrap();
        assert_eq!(items[0].key("status"), Some("progress"));
    }

    #[test]
    fn an_error_from_the_closure_leaves_the_file_untouched() {
        let (_dir, store) = store_with("do it id:a7f\n");
        let err = store
            .with_lock(|items| {
                items[0].set_key("status", "progress");
                Err::<(), String>("nope".into())
            })
            .unwrap_err();
        assert_eq!(err, "nope");
        assert_eq!(store.read_normalized().unwrap()[0].key("status"), None);
    }

    #[test]
    fn archive_moves_completed_lines_to_done_txt() {
        let (dir, store) = store_with("x 2026-07-25 done one id:b8c\nopen one id:a7f\n");
        assert_eq!(store.archive().unwrap(), 1);
        let remaining = store.read_normalized().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key("id"), Some("a7f"));
        let done = std::fs::read_to_string(dir.path().join("done.txt")).unwrap();
        assert!(done.contains("done one id:b8c"));
    }

    #[test]
    fn archive_appends_rather_than_replacing_done_txt() {
        let (dir, store) = store_with("x 2026-07-25 second id:b8c\n");
        std::fs::write(dir.path().join("done.txt"), "x 2026-07-01 first id:z1z\n").unwrap();
        store.archive().unwrap();
        let done = std::fs::read_to_string(dir.path().join("done.txt")).unwrap();
        assert!(done.contains("first id:z1z") && done.contains("second id:b8c"));
    }

    #[test]
    fn today_is_an_iso_date() {
        let today = Store::today();
        assert_eq!(today.len(), 10);
        assert_eq!(today.matches('-').count(), 2);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p corral-todo store`
Expected: FAIL, unresolved module `store`.

- [ ] **Step 3: Implement `todo/src/store.rs`**

```rust
//! The one write path to `todo.txt`: exclusive `flock`, read, mutate, rewrite
//! through a temp file plus rename.
//!
//! Every mutation goes through `with_lock`, so a read-modify-write is atomic
//! against a second dispatcher session, against the watcher's own
//! normalization, and against an editor that honors the lock.

use crate::item::Item;
use crate::normalize::normalize;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub struct Store {
    path: PathBuf,
}

/// An exclusive `flock` held for the lifetime of the value.
struct Lock(std::fs::File);

impl Lock {
    /// Lock a sidecar `<path>.lock`, never `todo.txt` itself: the write path
    /// replaces `todo.txt` by rename, so a lock on that inode would guard a
    /// file no later reader opens.
    fn acquire(path: &Path) -> Result<Lock, String> {
        let lock_path = path.with_extension("txt.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| format!("cannot open lock file {}: {e}", lock_path.display()))?;
        // Blocking LOCK_EX: a todo file has one or two writers and a turn is
        // milliseconds, so waiting is simpler and more correct than a retry
        // loop with a timeout.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "cannot lock {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the fd releases the lock anyway; unlocking explicitly keeps
        // the release visible at the end of the critical section.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Store {
        Store { path: path.into() }
    }

    /// The todo file this store owns. The watcher needs it so its existence
    /// check and its reads cannot disagree about which file is watched.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Today's date in todo.txt's `YYYY-MM-DD` form, from the system clock.
    /// The pure modules take a date string instead of reading a clock, so this
    /// is the one place time enters the crate.
    pub fn today() -> String {
        // No chrono in the workspace: derive the civil date from the unix
        // timestamp with the standard days-from-civil inverse.
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let days = secs.div_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{y:04}-{m:02}-{d:02}")
    }

    fn read_items(&self) -> Result<Vec<Item>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text.lines().filter_map(Item::parse).collect()),
            // A todo file that does not exist yet is an empty list, not an
            // error: `add` is allowed to create it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read {}: {e}", self.path.display())),
        }
    }

    fn write_items(&self, items: &[Item]) -> Result<(), String> {
        let body: String = items
            .iter()
            .map(|i| i.render())
            .collect::<Vec<_>>()
            .join("\n");
        write_atomic(&self.path, &format!("{body}\n"))
    }

    /// Lock, read, normalize, hand the items to `f`, and rewrite only if `f`
    /// succeeded. An `Err` from `f` aborts the write, so a refused change
    /// leaves the file exactly as it was.
    pub fn with_lock<T>(
        &self,
        f: impl FnOnce(&mut Vec<Item>) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = Lock::acquire(&self.path)?;
        let mut items = self.read_items()?;
        normalize(&mut items, &Store::today());
        let out = f(&mut items)?;
        self.write_items(&items)?;
        Ok(out)
    }

    /// The normalized items. Reading normalizes and writes back, so there is
    /// no way to observe an item without an `id:`. One locked pass: the items
    /// the caller gets are exactly the ones just written.
    pub fn read_normalized(&self) -> Result<Vec<Item>, String> {
        self.with_lock(|items| Ok(items.clone()))
    }

    /// Move completed lines out to `done.txt` beside the todo file, the
    /// todo.txt archive convention. Returns how many lines moved.
    pub fn archive(&self) -> Result<usize, String> {
        let done_path = self.path.with_file_name("done.txt");
        self.with_lock(|items| {
            let (done, keep): (Vec<Item>, Vec<Item>) =
                items.iter().cloned().partition(|i| i.completed);
            if done.is_empty() {
                return Ok(0);
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&done_path)
                .map_err(|e| format!("cannot open {}: {e}", done_path.display()))?;
            for item in &done {
                writeln!(file, "{}", item.render())
                    .map_err(|e| format!("cannot append to {}: {e}", done_path.display()))?;
            }
            *items = keep;
            Ok(done.len())
        })
    }
}

/// Write via a temp file in the same directory plus rename, so a reader sees
/// either the old file or the new one and never a truncated one.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("cannot replace {}: {e}", path.display()))
}
```

Add `pub mod store;` to `todo/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 5: Commit**

```bash
git add todo/src
git commit -m "todo: add the flock-guarded atomic store and done.txt archive"
```

---

### Task 5: The CLI

**Files:**
- Modify: `todo/src/main.rs`

**Interfaces:**
- Consumes: `item::{Item, State}`, `state::{Change, apply}`, `store::Store`.
- Produces: the `corral-todo` binary with subcommands `list`, `add`, `set`, `archive`. `watch` is added in Task 7.

The surface is fixed by `todo/SPEC.md` and must match exactly:

```
corral-todo list [--open|--status <s>]
corral-todo add "<text>"
corral-todo set <id> <state> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive
```

The todo file is `$CORRAL_TODO_FILE`, else `todo.txt` in the current directory. A `--file <path>` flag overrides both, which is what makes the CLI testable against a temp dir.

- [ ] **Step 1: Write the failing test**

Add to `todo/src/main.rs` a pure argument parser plus its tests, so the CLI's grammar is unit-tested without running a process:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        Command::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_list_filters() {
        assert_eq!(parse(&["list"]).unwrap(), Command::List { filter: None });
        assert_eq!(parse(&["list", "--open"]).unwrap(), Command::List { filter: Some(State::Open) });
        assert_eq!(
            parse(&["list", "--status", "progress"]).unwrap(),
            Command::List { filter: Some(State::Progress) }
        );
    }

    #[test]
    fn parses_add_joining_its_words() {
        assert_eq!(parse(&["add", "do", "a", "thing"]).unwrap(), Command::Add { text: "do a thing".into() });
    }

    #[test]
    fn parses_set_with_its_options() {
        assert_eq!(
            parse(&["set", "a7f", "progress", "--target", "/srv/x", "--worker", "W1"]).unwrap(),
            Command::Set {
                id: "a7f".into(),
                change: Change { state: State::Progress, target: Some("/srv/x".into()), worker: Some("W1".into()), reason: None },
            }
        );
    }

    #[test]
    fn rejects_an_unknown_state_and_an_unknown_flag() {
        assert!(parse(&["set", "a7f", "wat"]).is_err());
        assert!(parse(&["list", "--wat"]).is_err());
        assert!(parse(&["wat"]).is_err());
    }

    #[test]
    fn rejects_a_flag_with_no_value() {
        assert!(parse(&["set", "a7f", "blocked", "--reason"]).is_err());
    }

    #[test]
    fn requires_a_subcommand() {
        assert!(parse(&[]).is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p corral-todo --bin corral-todo`
Expected: FAIL, `cannot find type Command`.

- [ ] **Step 3: Implement `todo/src/main.rs`**

```rust
//! The `corral-todo` CLI: the only sanctioned writer of `todo.txt`.
//!
//! Argument parsing is hand-rolled into a pure `Command::parse` so the grammar
//! is unit-tested without spawning a process, and so the workspace gains no
//! argument-parsing dependency for four subcommands.

use corral_todo::item::{Item, State};
use corral_todo::state::{apply, Change};
use corral_todo::store::Store;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    List { filter: Option<State> },
    Add { text: String },
    Set { id: String, change: Change },
    Archive,
}

const USAGE: &str = "\
corral-todo list [--open|--status <open|progress|blocked|done>]
corral-todo add \"<text>\"
corral-todo set <id> <open|progress|blocked|done> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive
common: [--file <todo.txt>]  (else $CORRAL_TODO_FILE, else ./todo.txt)";

fn parse_state(word: &str) -> Result<State, String> {
    match word {
        "open" => Ok(State::Open),
        "progress" => Ok(State::Progress),
        "blocked" => Ok(State::Blocked),
        "done" => Ok(State::Done),
        other => Err(format!("unknown state {other:?}; expected open|progress|blocked|done")),
    }
}

/// Pull `--name <value>` out of the argument list, leaving the positionals.
fn take_flag(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(at) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(format!("{name} needs a value"));
    }
    args.remove(at);
    Ok(Some(args.remove(at)))
}

impl Command {
    fn parse(args: &[String]) -> Result<Command, String> {
        let Some((verb, tail)) = args.split_first() else {
            return Err(format!("no subcommand\n{USAGE}"));
        };
        let mut rest = tail.to_vec();
        match verb.as_str() {
            "list" => {
                let status = take_flag(&mut rest, "--status")?;
                let open = rest.iter().any(|a| a == "--open");
                rest.retain(|a| a != "--open");
                reject_unknown_flags(&rest)?;
                let filter = match (open, status) {
                    (true, Some(_)) => return Err("--open and --status are exclusive".into()),
                    (true, None) => Some(State::Open),
                    (false, Some(s)) => Some(parse_state(&s)?),
                    (false, None) => None,
                };
                Ok(Command::List { filter })
            }
            "add" => {
                reject_unknown_flags(&rest)?;
                if rest.is_empty() {
                    return Err("add needs task text".into());
                }
                Ok(Command::Add { text: rest.join(" ") })
            }
            "set" => {
                let target = take_flag(&mut rest, "--target")?;
                let worker = take_flag(&mut rest, "--worker")?;
                let reason = take_flag(&mut rest, "--reason")?;
                reject_unknown_flags(&rest)?;
                let id = rest.first().ok_or("set needs an id")?.clone();
                let state = parse_state(rest.get(1).ok_or("set needs a state")?)?;
                Ok(Command::Set { id, change: Change { state, target, worker, reason } })
            }
            "archive" => {
                reject_unknown_flags(&rest)?;
                Ok(Command::Archive)
            }
            other => Err(format!("unknown subcommand {other:?}\n{USAGE}")),
        }
    }
}

fn reject_unknown_flags(args: &[String]) -> Result<(), String> {
    // Fail loud: a typo'd flag silently treated as task text would write
    // nonsense into the file.
    if let Some(bad) = args.iter().find(|a| a.starts_with("--")) {
        return Err(format!("unknown option {bad}\n{USAGE}"));
    }
    Ok(())
}

fn todo_path(explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CORRAL_TODO_FILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("todo.txt"))
}

/// One line per item: id, state, target, worker, then the text.
fn format_item(item: &Item) -> String {
    let state = match item.state() {
        State::Open => "open",
        State::Progress => "progress",
        State::Blocked => "blocked",
        State::Done => "done",
    };
    let mut out = format!("{:<4} {:<9}", item.key("id").unwrap_or("?"), state);
    if let Some(t) = item.key("target") {
        out.push_str(&format!(" target:{t}"));
    }
    if let Some(w) = item.key("worker") {
        out.push_str(&format!(" worker:{w}"));
    }
    let prose: Vec<&str> = item
        .rest
        .split_whitespace()
        .filter(|t| !t.contains(':'))
        .collect();
    out.push_str(&format!("  {}", prose.join(" ")));
    out
}

fn run(command: Command, store: &Store) -> Result<(), String> {
    match command {
        Command::List { filter } => {
            for item in store.read_normalized()? {
                if filter.is_none_or(|f| item.state() == f) {
                    println!("{}", format_item(&item));
                }
            }
            Ok(())
        }
        Command::Add { text } => {
            let id = store.with_lock(|items| {
                let item = Item::parse(&text).ok_or_else(|| "task text is empty".to_string())?;
                items.push(item);
                // Normalization runs on read, not after this push, so coin the
                // id here to be able to print it.
                let today = Store::today();
                corral_todo::normalize::normalize(items, &today);
                items
                    .last()
                    .and_then(|i| i.key("id"))
                    .map(|s| s.to_string())
                    .ok_or_else(|| "internal: added item got no id".to_string())
            })?;
            println!("{id}");
            Ok(())
        }
        Command::Set { id, change } => store.with_lock(|items| {
            let item = items
                .iter_mut()
                .find(|i| i.key("id") == Some(id.as_str()))
                // Fail loud: a set against an id that is not there means the
                // caller's picture of the file is wrong.
                .ok_or_else(|| format!("no item with id {id}"))?;
            apply(item, &change, &Store::today())
        }),
        Command::Archive => {
            let moved = store.archive()?;
            println!("archived {moved}");
            Ok(())
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let file = match take_flag(&mut args, "--file") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("corral-todo: {e}");
            std::process::exit(2);
        }
    };
    let command = match Command::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corral-todo: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(command, &Store::new(todo_path(file))) {
        eprintln!("corral-todo: {e}");
        std::process::exit(1);
    }
}
```

Note: `Option::is_none_or` needs a recent Rust; if the pinned toolchain rejects it, write `filter.map_or(true, |f| item.state() == f)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 5: Verify the CLI by hand against a temp file**

```bash
cd "$(mktemp -d)"
cargo run -q -p corral-todo -- --file todo.txt add "add a --dry-run flag to the deploy script"
cargo run -q -p corral-todo -- --file todo.txt list
ID=$(cargo run -q -p corral-todo -- --file todo.txt add "second thing")
cargo run -q -p corral-todo -- --file todo.txt set "$ID" progress --target /srv/x --worker W1
cargo run -q -p corral-todo -- --file todo.txt list --status progress
cargo run -q -p corral-todo -- --file todo.txt set "$ID" done
cargo run -q -p corral-todo -- --file todo.txt archive
cat todo.txt done.txt
cargo run -q -p corral-todo -- --file todo.txt set nope done; echo "exit=$?"   # expect exit=1
```

Expected: each item prints its id on add; the progress line shows `target:` and `worker:`; after `archive` the completed line is in `done.txt` and gone from `todo.txt`; the unknown id exits 1 with `no item with id nope`.

- [ ] **Step 6: Commit**

```bash
git add todo/src
git commit -m "todo: add the corral-todo CLI (list, add, set, archive)"
```

---

### Task 6: The Wake Decision

**Files:**
- Create: `todo/src/wake.rs`
- Modify: `todo/src/lib.rs`

**Interfaces:**
- Consumes: `corral_core::discovery::{RegistryEntry, live_socket}`, `corral_core::launch::LaunchMode`.
- Produces: `corral_todo::wake::{Wake, decide, WAKE_MESSAGE}`; `enum Wake { Inject { socket: PathBuf }, Resume { argv: Vec<String>, mode: LaunchMode }, Spawn { argv: Vec<String> } }`; `decide(entries: &[RegistryEntry], dispatch_argv: &[String]) -> Wake`.

Design notes to preserve. The watcher reads the todo directory's **own** `<dir>/.corral/registry`, not corrald's vetted `state/registry`: the record's physical location is what proves its directory, the watcher runs as the operator on the trusted side of the boundary, and depending on corrald's curation would make the wake path fail whenever corrald is down (the spec states the wake path does not need corrald). Records carry no `cwd` field, so the caller stamps `cwd = dir` from where they were read. Among several records the most recently seen one wins, because that is the session an operator most likely still has in mind.

- [ ] **Step 1: Write the failing tests**

In `todo/src/wake.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
            ..Default::default()
        }
    }

    #[test]
    fn a_live_record_is_injected() {
        let entries = vec![entry("S1", Some("/home/me/todos/.corral/S1.sock"), "2026-07-26T10:00:00Z")];
        assert_eq!(
            decide(&entries, &["pi".to_string()]),
            Wake::Inject { socket: PathBuf::from("/home/me/todos/.corral/S1.sock") }
        );
    }

    #[test]
    fn a_dormant_record_is_resumed_with_its_own_session_id() {
        let entries = vec![entry("S1", None, "2026-07-26T10:00:00Z")];
        let Wake::Resume { argv, .. } = decide(&entries, &["pi".to_string()]) else {
            panic!("expected a resume");
        };
        // The session id must survive, or every worker's reply handle breaks.
        assert_eq!(argv, vec!["pi", "--session", "S1"]);
    }

    #[test]
    fn a_resume_stays_hidden_so_no_window_appears() {
        let entries = vec![entry("S1", None, "2026-07-26T10:00:00Z")];
        let Wake::Resume { mode, .. } = decide(&entries, &["pi".to_string()]) else {
            panic!("expected a resume");
        };
        assert!(mode.hidden);
    }

    #[test]
    fn no_record_spawns_the_configured_argv() {
        assert_eq!(
            decide(&[], &["pi".to_string()]),
            Wake::Spawn { argv: vec!["pi".to_string()] }
        );
    }

    #[test]
    fn the_most_recently_seen_record_wins() {
        let entries = vec![
            entry("OLD", None, "2026-07-20T10:00:00Z"),
            entry("NEW", Some("/home/me/todos/.corral/NEW.sock"), "2026-07-26T10:00:00Z"),
        ];
        assert_eq!(
            decide(&entries, &["pi".to_string()]),
            Wake::Inject { socket: PathBuf::from("/home/me/todos/.corral/NEW.sock") }
        );
    }

    #[test]
    fn a_dormant_record_with_no_resume_command_falls_back_to_a_fresh_spawn() {
        let mut e = entry("S1", None, "2026-07-26T10:00:00Z");
        e.resume_command = None;
        assert_eq!(
            decide(&[e], &["pi".to_string()]),
            Wake::Spawn { argv: vec!["pi".to_string()] }
        );
    }

    #[test]
    fn the_wake_message_names_the_file_not_the_work() {
        // The file, not the message, tells the dispatcher what to do, so the
        // message must carry no task detail.
        assert!(WAKE_MESSAGE.contains("todo.txt"));
        assert!(!WAKE_MESSAGE.contains("spawn"));
    }
}
```

If `RegistryEntry` has no `Default`, construct it with every field spelled out instead of `..Default::default()`; check `crates/core/src/discovery.rs` and follow whatever its own tests do.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p corral-todo wake`
Expected: FAIL, unresolved module `wake`.

- [ ] **Step 3: Implement `todo/src/wake.rs`**

```rust
//! Which of the three wake branches a registry scan implies. Pure: takes
//! records, returns an intent, performs no IO.
//!
//! The branches exist so the dispatcher's session id survives across wakes: a
//! live session is injected into, a dormant one is resumed through its own
//! `resumeCommand` (same session id, so every worker's reply handle stays
//! valid), and only a directory with no record at all gets a fresh launch.

use corral_core::discovery::{live_socket, RegistryEntry};
use corral_core::launch::LaunchMode;
use std::path::PathBuf;

/// The one-line nudge the dispatcher receives. It deliberately carries no task
/// detail: the file is the state, so the dispatcher reads it rather than
/// trusting a message that could be stale by the time it lands.
pub const WAKE_MESSAGE: &str =
    "todo.txt changed. Read it with corral-todo and run your dispatcher loop.";

// No `Eq`: `LaunchMode` in the `Resume` variant is only `PartialEq`.
#[derive(Debug, PartialEq)]
pub enum Wake {
    /// Write the wake into a live session's socket.
    Inject { socket: PathBuf },
    /// Relaunch this exact session, carrying the wake as its launch message.
    Resume { argv: Vec<String>, mode: LaunchMode },
    /// Launch the configured dispatcher argv fresh, carrying the wake.
    Spawn { argv: Vec<String> },
}

pub fn decide(entries: &[RegistryEntry], dispatch_argv: &[String]) -> Wake {
    let fresh = || Wake::Spawn { argv: dispatch_argv.to_vec() };
    // Most recently seen first: that is the session the operator most likely
    // still has in mind, and `last_seen` is ISO-8601 so it sorts as a string.
    let newest = entries
        .iter()
        .max_by(|a, b| a.last_seen.cmp(&b.last_seen));
    let Some(entry) = newest else {
        return fresh();
    };
    if let Some(live) = live_socket(entry) {
        return Wake::Inject { socket: live.socket };
    }
    match entry.resume_argv() {
        Some(argv) => Wake::Resume {
            argv,
            // Hidden always: the dispatcher is background machinery, and a
            // window popping up on every todo edit would be intolerable.
            mode: LaunchMode { hidden: true, ..entry.launch_mode() },
        },
        None => fresh(),
    }
}
```

Check `SocketEntry`'s field name in `crates/core/src/discovery.rs` and use whatever it actually is rather than assuming `socket`.

Add `pub mod wake;` to `todo/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 5: Commit**

```bash
git add todo/src
git commit -m "todo: decide the three dispatcher wake branches"
```

---

### Task 7: The Watch Loop

**Files:**
- Create: `todo/src/watch.rs`
- Modify: `todo/src/lib.rs`, `todo/src/main.rs`

**Interfaces:**
- Consumes: `store::Store`, `wake::{Wake, decide, WAKE_MESSAGE}`, `corral_core::prompt::send_prompt`, `corral_core::launch::{Launcher, TerminalLauncher}`, `corral_core::discovery::scan_registry`.
- Produces: `corral_todo::watch::{fingerprint, Watcher}`; `fingerprint(items: &[Item]) -> u64`; `Watcher::new(store: Store, dir: PathBuf, dispatch_argv: Vec<String>, interval: Duration)`, `Watcher::tick(&mut self, launcher: &dyn Launcher) -> Result<Option<Wake>, String>`, `Watcher::run(&mut self) -> !`; and the `corral-todo watch [--dir <dir>] [--interval <secs>] -- <argv...>` subcommand.

Design notes to preserve: hash the **normalized** items, not the raw bytes, so id and date stamping is not itself a change (a fresh brain-dump costs one wake, not two). And `tick` returns the `Wake` it performed so the loop is testable without a real agent.

- [ ] **Step 1: Write the failing tests**

In `todo/src/watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records what it was asked to launch instead of launching anything.
    struct FakeLauncher(Mutex<Vec<(PathBuf, Vec<String>, Option<String>, LaunchMode)>>);

    impl Launcher for FakeLauncher {
        fn launch(
            &self,
            cwd: &Path,
            command: &[String],
            message: Option<&str>,
            mode: &LaunchMode,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push((
                cwd.to_path_buf(),
                command.to_vec(),
                message.map(|m| m.to_string()),
                mode.clone(),
            ));
            Ok(())
        }
    }

    fn watcher(dir: &Path) -> Watcher {
        Watcher::new(
            Store::new(dir.join("todo.txt")),
            dir.to_path_buf(),
            vec!["pi".to_string()],
            Duration::from_secs(5),
        )
    }

    #[test]
    fn normalizing_a_fresh_dump_costs_exactly_one_wake() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "brand new idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        // First tick sees a change (nothing was known before).
        assert!(w.tick(&launcher).unwrap().is_some());
        // Second tick must be quiet: normalization stamped an id and a date,
        // and hashing the normalized items is what keeps that from counting.
        assert!(w.tick(&launcher).unwrap().is_none());
    }

    #[test]
    fn an_edit_wakes_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, "one idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        assert!(w.tick(&launcher).unwrap().is_none());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("another idea\n");
        std::fs::write(&path, text).unwrap();
        assert!(w.tick(&launcher).unwrap().is_some());
    }

    #[test]
    fn with_no_record_it_spawns_the_dispatcher_hidden_with_the_wake_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "an idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        let calls = launcher.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, dir.path());
        assert_eq!(calls[0].1, vec!["pi".to_string()]);
        assert_eq!(calls[0].2.as_deref(), Some(WAKE_MESSAGE));
        assert!(calls[0].3.hidden, "a dispatcher must never pop a window");
    }

    #[test]
    fn a_missing_todo_file_is_not_a_wake_and_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        assert!(w.tick(&launcher).unwrap().is_none());
        assert!(launcher.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_fingerprint_ignores_line_order_changes_only_when_content_matches() {
        let a = vec![Item::parse("one id:a").unwrap()];
        let b = vec![Item::parse("one id:a").unwrap()];
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let c = vec![Item::parse("one id:a status:progress").unwrap()];
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p corral-todo watch`
Expected: FAIL, unresolved module `watch`.

- [ ] **Step 3: Implement `todo/src/watch.rs`**

```rust
//! The poll loop: normalize, fingerprint, and on a change ensure exactly one
//! dispatcher is awake in the todo directory.
//!
//! Polling rather than inotify on purpose: a few seconds of latency is
//! invisible for a todo board, the fingerprint gives idempotence for free, and
//! there is no watch-descriptor bookkeeping to get wrong when an editor
//! replaces the file instead of writing into it.
//!
//! This runs as its own process rather than inside the dispatcher because the
//! thing that must not fail is *noticing*: wake logic inside the dispatcher
//! dies with the dispatcher, and a supervisor belongs outside the process whose
//! liveness is in question.

use crate::item::Item;
use crate::store::Store;
use crate::wake::{decide, Wake, WAKE_MESSAGE};
use corral_core::discovery::scan_registry;
use corral_core::launch::{LaunchMode, Launcher};
use corral_core::prompt::send_prompt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A content hash of the **normalized** items. Normalized, so that stamping an
/// id or a creation date is not itself seen as a change; otherwise a fresh
/// brain-dump would wake the dispatcher twice.
pub fn fingerprint(items: &[Item]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in items
        .iter()
        .map(|i| i.render())
        .collect::<Vec<_>>()
        .join("\n")
        .bytes()
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub struct Watcher {
    store: Store,
    dir: PathBuf,
    dispatch_argv: Vec<String>,
    interval: Duration,
    seen: Option<u64>,
}

impl Watcher {
    pub fn new(
        store: Store,
        dir: PathBuf,
        dispatch_argv: Vec<String>,
        interval: Duration,
    ) -> Watcher {
        Watcher { store, dir, dispatch_argv, interval, seen: None }
    }

    /// The todo directory's own registry. Deliberately not corrald's vetted
    /// `state/registry`: the record's physical location proves its directory,
    /// the watcher runs as the operator on the trusted side of that boundary,
    /// and the wake path must keep working while corrald is down.
    fn records(&self) -> Vec<corral_core::discovery::RegistryEntry> {
        let mut entries = scan_registry(&self.dir.join(".corral").join("registry"));
        for entry in &mut entries {
            // Records carry no `cwd` field (CONVENTION v2); the directory they
            // were read from is the trusted value.
            entry.cwd = Some(self.dir.to_string_lossy().to_string());
        }
        entries
    }

    /// One poll. Returns the wake it performed, or `None` when the file did not
    /// change. Returning the action is what makes the loop testable without a
    /// real agent.
    pub fn tick(&mut self, launcher: &dyn Launcher) -> Result<Option<Wake>, String> {
        if !self.store_exists() {
            // Nothing to watch yet. Not an error: the operator may create the
            // file after starting the service.
            return Ok(None);
        }
        let items = self.store.read_normalized()?;
        let print = fingerprint(&items);
        if self.seen == Some(print) {
            return Ok(None);
        }
        self.seen = Some(print);
        let wake = decide(&self.records(), &self.dispatch_argv);
        match &wake {
            Wake::Inject { socket } => send_prompt(socket, WAKE_MESSAGE)
                .map_err(|e| format!("cannot wake the dispatcher over {}: {e}", socket.display()))?,
            Wake::Resume { argv, mode } => {
                launcher.launch(&self.dir, argv, Some(WAKE_MESSAGE), mode)?
            }
            Wake::Spawn { argv } => launcher.launch(
                &self.dir,
                argv,
                Some(WAKE_MESSAGE),
                // Hidden: the dispatcher is background machinery.
                &LaunchMode { hidden: true, ..LaunchMode::default() },
            )?,
        }
        Ok(Some(wake))
    }

    fn store_exists(&self) -> bool {
        self.dir.join("todo.txt").exists()
    }

    /// Poll forever. A failed wake is reported and the loop continues, because
    /// the next edit deserves another try; the fingerprint is already updated,
    /// so a permanently broken wake logs once per edit rather than every tick.
    pub fn run(&mut self, launcher: &dyn Launcher) {
        loop {
            if let Err(e) = self.tick(launcher) {
                eprintln!("corral-todo watch: {e}");
            }
            std::thread::sleep(self.interval);
        }
    }
}
```

Note the `store_exists` body above hardcodes `todo.txt` under `dir`, which would disagree with the store whenever `--file` points elsewhere. Use the store's own path instead (`Store::path`, produced by Task 4):

```rust
    fn store_exists(&self) -> bool {
        self.store.path().exists()
    }
```

- [ ] **Step 4: Add the `watch` subcommand**

In `todo/src/main.rs`, extend the enum and the parser:

```rust
    Watch { dir: Option<String>, interval: u64, dispatch_argv: Vec<String> },
```

Parse it as: everything after a literal `--` is `dispatch_argv` (which must be non-empty, since the spec forbids defaulting the harness), `--dir` and `--interval` are flags, and `--interval` defaults to 5.

```rust
            "watch" => {
                let dir = take_flag(&mut rest, "--dir")?;
                let interval = take_flag(&mut rest, "--interval")?
                    .map(|s| s.parse::<u64>().map_err(|e| format!("--interval: {e}")))
                    .transpose()?
                    .unwrap_or(5);
                let at = rest.iter().position(|a| a == "--");
                let dispatch_argv = match at {
                    Some(at) => rest.split_off(at + 1),
                    None => Vec::new(),
                };
                if dispatch_argv.is_empty() {
                    // Never default the harness: corral's own rule is that it
                    // does not name an agent kind, and neither does this.
                    return Err("watch needs a dispatcher argv after --, e.g. `watch --dir ~/todos -- pi`".into());
                }
                reject_unknown_flags(&rest)?;
                Ok(Command::Watch { dir, interval, dispatch_argv })
            }
```

Add its tests beside the others:

```rust
    #[test]
    fn parses_watch_with_its_dispatch_argv() {
        assert_eq!(
            parse(&["watch", "--dir", "/home/me/todos", "--interval", "2", "--", "pi"]).unwrap(),
            Command::Watch {
                dir: Some("/home/me/todos".into()),
                interval: 2,
                dispatch_argv: vec!["pi".into()],
            }
        );
    }

    #[test]
    fn watch_refuses_to_default_the_harness() {
        assert!(parse(&["watch", "--dir", "/home/me/todos"]).is_err());
    }
```

And run it in `run`:

```rust
        Command::Watch { dir, interval, dispatch_argv } => {
            let dir = dir.map(PathBuf::from).unwrap_or_else(|| {
                store.path().parent().map(Path::to_path_buf).unwrap_or_default()
            });
            let mut watcher = Watcher::new(
                Store::new(dir.join("todo.txt")),
                dir,
                dispatch_argv,
                Duration::from_secs(interval),
            );
            watcher.run(&TerminalLauncher);
            Ok(())
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p corral-todo` — expect all tests PASS.
Run: `cargo clippy -p corral-todo -- -D warnings` — expect clean.

- [ ] **Step 6: Commit**

```bash
git add todo/src
git commit -m "todo: add the watch loop that wakes one dispatcher per change"
```

---

### Task 8: Ship It (Flake, Just, Docs)

**Files:**
- Modify: `flake.nix`, `justfile`, `AGENTS.md`, `README.md`, `todo/SPEC.md`
- Create: `todo/README.md`

- [ ] **Step 1: Add the binary to the flake package**

Read `flake.nix` first. The package builds the workspace; confirm `corral-todo` lands in `$out/bin` and add it to whatever explicit binary list or `wrapProgram` set exists. `corral-todo` needs **no** graphics libraries and no wrapper, unlike `corral-gui`; it does need `cage` on PATH for a hidden launch, which the flake already puts on the runtime PATH of the other binaries — extend that to this one.

Verify: `nix build` then `./result/bin/corral-todo` prints the usage and exits 2.

- [ ] **Step 2: Add `just` recipes**

In `justfile`, beside the existing ones:

```just
# Watch a todo directory and wake its dispatcher (see todo/SPEC.md).
todo-watch dir harness="pi":
    cargo run -p corral-todo -- watch --dir {{dir}} -- {{harness}}
```

`just test` and `just lint` already cover the workspace, so they need no change. Confirm that by running both.

- [ ] **Step 3: Write `todo/README.md`**

Cover exactly the spec's "Prerequisites and First Run", in the inverted-pyramid style `README.md` uses: create `~/todos` outside this repo (because pi concatenates every `AGENTS.md` up the tree, and a directory nested here would feed corral's ~10k-word architecture document into every dispatcher), `ln -s <repo>/todo/DISPATCHER.md ~/todos/AGENTS.md`, make it a git repo, ensure `corrald` runs and `corral-todo` plus the harness are on PATH, seed both whitelist directions per worker directory (`~/.corral/whitelist`, one line per direction, `<todo dir> -> <worker dir>` and back), and run `corral-todo watch --dir ~/todos -- pi` (under a systemd user service with restart-on-failure, which is deployment glue for `~/nixos`, not code here).

- [ ] **Step 4: Update the repository docs**

In `AGENTS.md`, replace the "Todo System (`todo/`, Design Only)" heading and its stage-1 sentence: stage 1 is now implemented. State what ships (the `corral-todo` crate at `todo/`: `item`/`normalize`/`state`/`store`/`wake`/`watch` plus the CLI, a fifth workspace member, consuming `corral-core` as an outside program would), keep the stage-2 paragraph as still-unbuilt, and add `corral-todo` to the "Interfaces to the Outside World" list with its subcommands and the `$CORRAL_TODO_FILE` override. In `README.md`, add at most one line pointing at `todo/README.md` — the file is deliberately short and must not grow into a manual. In `todo/SPEC.md`, change the status header to say stage 1 is implemented and stage 2 is not.

Do **not** touch `nix/tests/`: that rule covers corral's adapters, board and daemon, and stage 1 changes none of them. Say so in the commit body if you like, but add no scenario.

- [ ] **Step 5: Verify everything**

```bash
just test
just lint
nix build
```

Paste all three outputs. Every one must pass before you claim this task done.

- [ ] **Step 6: Commit**

```bash
git add flake.nix justfile AGENTS.md README.md todo/
git commit -m "todo: ship corral-todo in the flake and document first run"
```

---

## Not In This Plan

Deliberately excluded, and why:

- **The board integration** (TODO column, quick-add, card moves writing task state). Stage 2 in `todo/SPEC.md`, blocked on two open design questions there: the drop granularity inside a stacked `PROGRESS` column, and whether the third column holds done tasks or only dormant records. It also changes `core::model::Column::ALL` for every agent, so it needs its own plan.
- **A worker liveness sweep.** `todo/SPEC.md`'s known limits accept that a worker which stops without reporting leaves its item at `status:progress` until a human looks. `target:` and `worker:` are recorded so a later sweep is cheap.
- **A mechanical sender in `corral-todo`.** The outbox-submit path is specified in `todo/SPEC.md` for whenever one is wanted; the MVP leaves all sending to the dispatcher's own tools, since a second sender is a second interface with no use yet.
- **Anything inside `corral`, `corral-gui` or `corrald`.** Stage 1 touches none of them.
