# Export Agent Message History (`o` key) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `o` key (both shells) that fetches a selected **live** agent's full message history over ACP `session/load`, writes it to a JSON file, and opens it with `xdg-open`.

**Architecture:** A new `crates/core/src/history.rs` module opens a one-shot ACP connection to the agent's socket, sends `session/load`, collects every `session/update` notification the agent replays until the reply lands (or a 5s timeout), then writes the collected entries to a temp JSON file and launches `xdg-open` on it detached. Both shells wire the same key/menu entry to this one function. pi and opencode implement `session/load` for real from their in-process transcripts; the Claude sidecar implements it from Claude Code's on-disk JSONL transcript (path supplied by every hook's documented `transcript_path` field); cursor has no transcript API and keeps answering method-not-supported (already its default for unimplemented methods, so no code change there).

**Tech Stack:** Rust (corral-core/board/gui), TypeScript (pi/opencode/claude adapters, run via `node`/bun-compiled runtimes), Python (VM e2e test scenarios).

**Spec:** `docs/superpowers/specs/2026-07-23-history-export-design.md`

## Global Constraints

- TUI/GUI parity (hard rule): the key, footer hint, context-menu entry, and fetch/write/open logic live once in `corral-core`; both shells just call it. Never land this in one shell alone.
- VM e2e hard rule: any adapter/ACP-surface change updates the matching `nix/tests/` scenario in the same change.
- Live agents only; a dormant card's `o` is a no-op.
- Timeout for `fetch_history`: 5 seconds total.
- Temp file name: `corral-history-<sessionId>-<unix_ts>.json` under `std::env::temp_dir()`.
- `xdg-open` runs detached (`setsid --fork xdg-open <path>`), on the operator's own display — never inside a hidden agent's cage.
- No dormant-agent path, no tool-call replay (message entries only — user/assistant text), no v2 `session/resume` work. See spec's Out of Scope.

---

### Task 1: `history.rs` — the ACP collector (`fetch_history`)

**Files:**
- Create: `crates/core/src/history.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod history;`)

**Interfaces:**
- Produces: `pub enum HistoryError { Unsupported, Timeout, Io(std::io::Error) }` (implements `std::fmt::Display`), `pub fn fetch_history(socket: &Path, session_id: &str, cwd: &str) -> Result<Vec<serde_json::Value>, HistoryError>`.

- [ ] **Step 1: Check `crates/core/src/lib.rs` module list**

Run: `grep -n "^pub mod" crates/core/src/lib.rs`
Expected: a list including `prompt`, `watch`, `model`, `menu`, etc. Add `pub mod history;` alongside them (alphabetical if the existing list is alphabetical, else appended — match the file's existing order).

- [ ] **Step 2: Write the failing test — success path (N notifications then a reply)**

```rust
// crates/core/src/history.rs (bottom of file, #[cfg(test)] mod tests)
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn fetch_history_collects_notifications_then_stops_at_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            // Read the initialize request (id 0), reply.
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            // Read the session/load request (id 1).
            line.clear();
            r.read_line(&mut line).unwrap();
            // Replay two message notifications, then the load reply.
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"user_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hi\"}}}}\n").unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"hello\"}}}}\n").unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n").unwrap();
        });
        let entries = fetch_history(&sock, "s1", "/tmp/proj").unwrap();
        h.join().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["sessionUpdate"], "user_message_chunk");
        assert_eq!(entries[1]["sessionUpdate"], "agent_message_chunk");
    }

    #[test]
    fn fetch_history_reports_unsupported_on_error_reply() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("cursor-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"method not supported by corral-cursor: session/load\"}}\n").unwrap();
        });
        let err = fetch_history(&sock, "s1", "/tmp/proj").unwrap_err();
        h.join().unwrap();
        assert!(matches!(err, HistoryError::Unsupported));
    }

    #[test]
    fn fetch_history_times_out_when_agent_never_replies() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("silent-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || {
            // Accept and read, but never reply to session/load at all.
            let (conn, _) = listener.accept().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = BufReader::new(conn);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}\n").unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            // Hold the connection open, silent, until the sender times out.
            std::thread::sleep(std::time::Duration::from_secs(6));
        });
        let err = fetch_history(&sock, "s1", "/tmp/proj").unwrap_err();
        assert!(matches!(err, HistoryError::Timeout));
        drop(h); // the listener thread outlives the test; do not join (it sleeps 6s)
    }
}
```

- [ ] **Step 2b: Run the tests to verify they fail**

Run: `cargo test -p corral-core history:: 2>&1 | tail -30`
Expected: FAIL — `fetch_history`, `HistoryError` not defined (module doesn't exist yet).

- [ ] **Step 3: Implement `fetch_history`**

```rust
// crates/core/src/history.rs (top of file, above the tests module)
//! Fetching an agent's full message history over ACP `session/load`
//! (agentclientprotocol.com/protocol/session-setup#loading-sessions): the
//! Agent replays its entire conversation as `session/update` notifications
//! before responding to the `session/load` request itself. This module opens
//! a fresh, throwaway connection (same shape as `watch.rs`'s seed connection
//! and `prompt.rs`'s delivery connection), collects the replayed notifications,
//! and stops the instant the reply to the `session/load` request arrives.
//!
//! v1 `session/load` (not v2's draft `session/resume`+`replayFrom`, which is
//! an unmerged RFD and not implemented anywhere in corral) — see the design
//! doc's rationale.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Bounds the whole fetch: connect + initialize + session/load + full replay.
/// Generous for a real session's replay, but still finite so a
/// non-conforming or hung agent cannot block the board indefinitely.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Why `fetch_history` failed, so callers can show a specific footer message.
#[derive(Debug)]
pub enum HistoryError {
    /// The agent answered `session/load` with a JSON-RPC error (most likely
    /// method-not-supported, e.g. cursor).
    Unsupported,
    /// No reply arrived within `TIMEOUT`.
    Timeout,
    /// A connection or I/O failure (socket gone, EOF before a reply, etc).
    Io(std::io::Error),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryError::Unsupported => write!(f, "history not supported"),
            HistoryError::Timeout => write!(f, "no reply"),
            HistoryError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for HistoryError {
    fn from(e: std::io::Error) -> Self {
        HistoryError::Io(e)
    }
}

/// Fetch `session_id`'s full history from the live agent at `socket`. Opens a
/// one-shot connection, sends `initialize` (id 0) then `session/load` (id 1,
/// per ACP with `sessionId`, `cwd`, and an empty `mcpServers` — corral asks an
/// already-running session to replay in place, not to actually reconnect MCP
/// servers), then reads lines until either the id-1 reply arrives (success:
/// return every collected notification's `update` value; error: `Unsupported`)
/// or `TIMEOUT` elapses (`Timeout`).
pub fn fetch_history(
    socket: &Path,
    session_id: &str,
    cwd: &str,
) -> Result<Vec<Value>, HistoryError> {
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = UnixStream::connect(socket)?;
    let init = serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}});
    let load = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/load",
        "params": { "sessionId": session_id, "cwd": cwd, "mcpServers": [] },
    });
    stream.write_all((init.to_string() + "\n").as_bytes())?;
    stream.write_all((load.to_string() + "\n").as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut collected = Vec::new();
    let mut line = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HistoryError::Timeout);
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err(HistoryError::Timeout), // closed before replying
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(HistoryError::Timeout)
            }
            Err(e) => return Err(HistoryError::Io(e)),
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("method").and_then(|m| m.as_str()) == Some("session/update") {
            if let Some(update) = msg.get("params").and_then(|p| p.get("update")) {
                collected.push(update.clone());
            }
            continue;
        }
        if msg.get("id").and_then(|i| i.as_i64()) == Some(1) {
            return if msg.get("error").is_some() {
                Err(HistoryError::Unsupported)
            } else {
                Ok(collected)
            };
        }
        // Anything else (e.g. the id-0 initialize reply): ignore.
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p corral-core history:: 2>&1 | tail -30`
Expected: PASS — 3 tests (`fetch_history_collects_notifications_then_stops_at_reply`,
`fetch_history_reports_unsupported_on_error_reply`,
`fetch_history_times_out_when_agent_never_replies`).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/history.rs crates/core/src/lib.rs
git commit -m "core: fetch_history — collect session/load replay over a fresh ACP connection"
```

---

### Task 2: `history.rs` — `write_and_open` + `MenuAction::History`

**Files:**
- Modify: `crates/core/src/history.rs`
- Modify: `crates/core/src/menu.rs`

**Interfaces:**
- Consumes: `HistoryError`, `Value` from Task 1; `crate::model::Agent` (fields `session_id: Option<String>`, `cwd: Option<String>`, `title: Option<String>`).
- Produces: `pub fn write_and_open(agent: &crate::model::Agent, entries: Vec<Value>) -> std::io::Result<PathBuf>`; `menu::MenuAction::History` (label `"history"`).

- [ ] **Step 1: Write the failing test for `write_and_open`**

```rust
// crates/core/src/history.rs, inside #[cfg(test)] mod tests
#[test]
fn write_and_open_writes_json_with_wrapper_fields() {
    let agent = crate::model::test_support::agent_fixture(); // see note below
    let entries = vec![serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}})];
    let path = write_and_open(&agent, entries).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(v["sessionId"], agent.session_id.clone().unwrap());
    assert_eq!(v["updates"][0]["sessionUpdate"], "agent_message_chunk");
    assert!(v.get("capturedAt").is_some());
    std::fs::remove_file(&path).ok();
}
```

Note: check whether `crate::model` already has test fixtures for `Agent` (`grep -n "fn.*-> Agent\|Agent {" crates/core/src/model.rs` — the model's own test module likely constructs one inline). If no shared fixture exists, build the `Agent` literal directly in this test instead of adding a `test_support` module — do not add a new pub test-support module for one test. Prefer:

```rust
#[test]
fn write_and_open_writes_json_with_wrapper_fields() {
    use crate::model::{Agent, Origin, State};
    use std::time::Instant;
    let agent = Agent {
        socket_path: std::path::PathBuf::from("/tmp/x.sock"),
        pid: 1,
        label: "pi".into(),
        session_id: Some("sess-1".into()),
        title: Some("fix bug".into()),
        cwd: Some("/tmp/proj".into()),
        state: State::Idle,
        origin: Origin::Live,
        spawn_command: None,
        resume_command: None,
        activity: None,
        gui: false,
        message_flag: None,
        hidden: false,
        model: None,
        state_since: Instant::now(),
        last_activity: Instant::now(),
    };
    let entries = vec![serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}})];
    let path = write_and_open(&agent, entries).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(v["sessionId"], "sess-1");
    assert_eq!(v["updates"][0]["sessionUpdate"], "agent_message_chunk");
    assert!(v.get("capturedAt").is_some());
    std::fs::remove_file(&path).ok();
}
```

Before writing this, run `grep -n "state_since\|last_activity" crates/core/src/model.rs` to confirm the exact remaining `Agent` field names/types (the struct excerpt in this plan's research may not list every field) and adjust the literal to match exactly — this is the one place in the plan where the engineer must check the live struct definition, since `Agent` has fields beyond those already named in this plan's file-structure notes.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p corral-core write_and_open 2>&1 | tail -20`
Expected: FAIL — `write_and_open` not defined.

- [ ] **Step 3: Implement `write_and_open`**

```rust
// crates/core/src/history.rs, after fetch_history
use std::process::Command;
use std::time::SystemTime;

/// Wrap `entries` (the raw replayed `session/update` values from
/// `fetch_history`) plus identifying metadata into one JSON object, write it
/// to a temp file, and launch `xdg-open` on it detached — on the caller's own
/// display, which is why this is the board's job and not the agent's (a hidden
/// agent runs inside a headless cage with no real display; the board always
/// runs on the operator's own). Returns the path written.
pub fn write_and_open(agent: &crate::model::Agent, entries: Vec<Value>) -> std::io::Result<PathBuf> {
    let captured_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let session_id = agent.session_id.clone().unwrap_or_else(|| "unknown".to_string());
    let doc = serde_json::json!({
        "sessionId": session_id,
        "cwd": agent.cwd,
        "title": agent.title,
        "capturedAt": captured_at,
        "updates": entries,
    });
    let path = std::env::temp_dir().join(format!("corral-history-{session_id}-{captured_at}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
    // Detached like launch.rs's spawns: the board must not block on (or be
    // killed alongside) the viewer program.
    let _ = Command::new("setsid")
        .arg("--fork")
        .arg("xdg-open")
        .arg(&path)
        .spawn();
    Ok(path)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p corral-core write_and_open 2>&1 | tail -20`
Expected: PASS. (The `xdg-open` spawn is best-effort and unasserted; the test
only checks the file contents.)

- [ ] **Step 5: Add `MenuAction::History` and extend its tests**

```rust
// crates/core/src/menu.rs — replace the enum, ALL, and label()
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Go,
    Message,
    History,
    Spawn,
    ToggleHidden,
    Dismiss,
}

impl MenuAction {
    pub const ALL: [MenuAction; 6] = [
        MenuAction::Go,
        MenuAction::Message,
        MenuAction::History,
        MenuAction::Spawn,
        MenuAction::ToggleHidden,
        MenuAction::Dismiss,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MenuAction::Go => "go",
            MenuAction::Message => "msg",
            MenuAction::History => "history",
            MenuAction::Spawn => "new",
            MenuAction::ToggleHidden => "hide/show",
            MenuAction::Dismiss => "delete",
        }
    }
}
```

Update the existing test's expected array literal to match:

```rust
    #[test]
    fn entries_are_in_footer_order_dismiss_last() {
        assert_eq!(
            MenuAction::ALL,
            [
                MenuAction::Go,
                MenuAction::Message,
                MenuAction::History,
                MenuAction::Spawn,
                MenuAction::ToggleHidden,
                MenuAction::Dismiss,
            ]
        );
        assert_eq!(*MenuAction::ALL.last().unwrap(), MenuAction::Dismiss);
    }
```

(`every_entry_has_a_label` needs no change — it already iterates `ALL`.)

- [ ] **Step 6: Run all core tests**

Run: `cargo test -p corral-core 2>&1 | tail -40`
Expected: PASS, including the updated `menu::tests::entries_are_in_footer_order_dismiss_last`.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/history.rs crates/core/src/menu.rs
git commit -m "core: write_and_open history dump; add MenuAction::History"
```

---

### Task 3: TUI wiring (`o` key, footer hint, context menu)

**Files:**
- Modify: `crates/board/src/main.rs`
- Modify: `crates/board/src/ui.rs`

**Interfaces:**
- Consumes: `corral_core::history::{fetch_history, write_and_open, HistoryError}`, `corral_core::menu::MenuAction`, `board.selected_agent(selected)` (or however `main.rs` currently reads the selected `Agent` — check the existing `dismiss_selected`/`toggle_selected` functions in `main.rs` for the exact accessor, e.g. `board.agent_at(selected)`).

- [ ] **Step 1: Find the exact selected-agent accessor and status-setting convention**

Run: `grep -n "fn dismiss_selected\|fn toggle_selected\|board\.\(agent\|selected\)" crates/board/src/main.rs | head -20`

Read the matched function bodies (e.g. `toggle_selected`) to copy their exact
pattern for: getting the `Agent` at `selected`, checking `origin == Origin::Live`,
and writing to `status: &mut String`.

- [ ] **Step 2: Add the `history_selected` function**

Add near `dismiss_selected`/`toggle_selected` in `crates/board/src/main.rs`
(match their exact signature style — the snippet below assumes the same shape
seen for `toggle_selected`; adjust the accessor call if Step 1 found a
different one):

```rust
/// `o`: fetch the selected live agent's full message history over
/// session/load and open it. Live only — a dormant card has no process to
/// ask, so this is a no-op there (mirrors `dismiss_selected`'s per-origin
/// branch). Errors land in the status line like every other action.
fn history_selected(board: &Board, selected: usize, status: &mut String) {
    status.clear();
    let Some(agent) = board.agent_at(selected) else {
        *status = "history: no agent selected".into();
        return;
    };
    if agent.origin != Origin::Live {
        *status = "history: only available for live agents".into();
        return;
    }
    let socket = agent.socket_path.clone();
    let session_id = agent.session_id.clone().unwrap_or_default();
    let cwd = agent.cwd.clone().unwrap_or_default();
    *status = match corral_core::history::fetch_history(&socket, &session_id, &cwd) {
        Ok(entries) => match corral_core::history::write_and_open(agent, entries) {
            Ok(path) => format!("history: opened {}", path.display()),
            Err(e) => format!("history: {e}"),
        },
        Err(e) => format!("history: {e}"),
    };
}
```

If `board.agent_at(selected)` is not the real accessor name found in Step 1,
replace every occurrence above with the real one (e.g. it may take `&Board`
plus a `Column`/index pair, or return `&Agent` directly rather than
`Option<&Agent>` — match `dismiss_selected`'s exact call and adjust the `let
Some(agent) = ... else` to whatever that function's real pattern is).

- [ ] **Step 3: Wire the `o` key in both the normal and filter-input key match arms**

Run: `grep -n "KeyCode::Char('d')" crates/board/src/main.rs` to find both
occurrences (the normal board loop and, if `d`/`h` are excluded from the
filter-input's char-insert fallthrough, note where). Add an `o` arm
immediately after each `KeyCode::Char('h')` arm:

```rust
                    KeyCode::Char('o') => {
                        history_selected(board, selected, &mut status);
                    }
```

- [ ] **Step 4: Add the context-menu dispatch arm**

Run: `grep -n "MenuAction::ToggleHidden =>" crates/board/src/main.rs` to find
the menu-action match (inside the `KeyCode`/mouse click handling, likely near
where `MenuAction::Dismiss` is handled). Add:

```rust
                    MenuAction::History => history_selected(board, selected, &mut status),
```

adjacent to the other `MenuAction::*` arms, in `MenuAction::ALL` order.

- [ ] **Step 5: Add the footer key hint in `ui.rs`**

Run: `grep -n "m msg\|\"m\"\|footer" crates/board/src/ui.rs | head -20` to find
the footer hint string (a `&str` or `Vec` of `(key, verb)` pairs). Add an
`o history` entry immediately after `m msg`, matching whatever format the
existing hints use (likely a literal joined string like `"↑↓ move  ←→ col  ⏎ go
 ⇧⏎ new  m msg  d del  h hide  q quit"` — insert `o history` in the same
style, right after `m msg`).

- [ ] **Step 6: Build and run the TUI test suite**

Run: `cargo build -p corral-board 2>&1 | tail -30`
Expected: builds clean.

Run: `cargo test -p corral-core -p corral-board 2>&1 | tail -40`
Expected: all pass (no board-side unit tests exercise key dispatch directly;
this task's correctness is verified by manual smoke-testing in Task 9's e2e
scenario, not a new board unit test — the board's `main.rs` has no existing
per-key unit test pattern to extend).

- [ ] **Step 7: Commit**

```bash
git add crates/board/src/main.rs crates/board/src/ui.rs
git commit -m "board: o key fetches and opens the selected agent's history"
```

---

### Task 4: GUI wiring (`o` key, footer hint, context menu)

**Files:**
- Modify: `crates/gui/src/dashboard.rs`

**Interfaces:**
- Consumes: same `corral_core::history` functions as Task 3; iced's `Task::perform` for the async fetch (iced already runs an async runtime — see `Message::Tick`/`iced::time::every` usage in this file for the existing async wiring pattern).
- Produces: `Message::History` variant, `Message::HistoryDone(Result<String, String>)` (a formatted status string either way, mirroring the TUI's status-line pattern).

- [ ] **Step 1: Locate the selected-agent accessor and the existing async `Task::perform` pattern**

Run: `grep -n "fn act_go\|fn act_spawn\|fn act_toggle_hidden\|Task::perform" crates/gui/src/dashboard.rs`

Read `act_toggle_hidden` (or `act_dismiss` if that exists) to copy its exact
pattern for getting the selected `Agent`, and check whether any existing action
already uses `Task::perform` for a blocking call (if none does, this is the
first — introduce it following iced 0.13's `Task::perform(future, Message)`
API, matching how `Message::Tick`/`iced::time::every` are already registered in
`subscription()`).

- [ ] **Step 2: Add `Message::History` / `Message::HistoryDone` variants**

In the `pub enum Message` block (near `Message::Dismiss`, `Message::ToggleHidden`):

```rust
    History,
    HistoryDone(String), // formatted status string, success or failure
```

- [ ] **Step 3: Add the `act_history` method**

Add near `act_toggle_hidden` (copy its exact accessor pattern from Step 1;
adjust names below if they differ):

```rust
    /// `o`: fetch the selected live agent's full history over session/load and
    /// open it, off the UI thread (fetch_history can block up to 5s). Mirrors
    /// the TUI's history_selected, but async since iced must not block its
    /// event loop.
    fn act_history(&mut self) -> Task<Message> {
        let Some(agent) = self.selected_agent() else {
            self.status = "history: no agent selected".into();
            return Task::none();
        };
        if agent.origin != Origin::Live {
            self.status = "history: only available for live agents".into();
            return Task::none();
        }
        let socket = agent.socket_path.clone();
        let session_id = agent.session_id.clone().unwrap_or_default();
        let cwd = agent.cwd.clone().unwrap_or_default();
        let agent_clone = agent.clone();
        Task::perform(
            async move {
                match corral_core::history::fetch_history(&socket, &session_id, &cwd) {
                    Ok(entries) => match corral_core::history::write_and_open(&agent_clone, entries) {
                        Ok(path) => format!("history: opened {}", path.display()),
                        Err(e) => format!("history: {e}"),
                    },
                    Err(e) => format!("history: {e}"),
                }
            },
            Message::HistoryDone,
        )
    }
```

Replace `self.selected_agent()` with whatever accessor Step 1 actually found
(e.g. it may be a free function taking `&self.board` and `self.selected`
rather than a method). If `Agent` does not already derive `Clone`, check
`crates/core/src/model.rs`'s `#[derive(...)]` on `Agent` — it almost certainly
already derives `Clone` (dormant/live agents are copied around the engine
freely); if not, add `Clone` to its derive list as part of this step and
re-run `cargo test -p corral-core` to confirm nothing else broke.

- [ ] **Step 4: Wire `Message::History` / `Message::HistoryDone` into `update`**

In the `match message` block of `update()`, near `Message::ToggleHidden => return self.act_toggle_hidden(),`:

```rust
            Message::History => return self.act_history(),
            Message::HistoryDone(status) => {
                self.status = status;
            }
```

- [ ] **Step 5: Wire the `o` key in `on_key`**

Run: `grep -n "\"h\" => return self.update(Message::ToggleHidden)" crates/gui/src/dashboard.rs`

Add immediately after:

```rust
                "o" => return self.update(Message::History),
```

- [ ] **Step 6: Add the context-menu dispatch arm**

Run: `grep -n "MenuAction::ToggleHidden =>" crates/gui/src/dashboard.rs`

Add adjacent to the other `MenuAction::*` arms inside the `Message::MenuPick` handler:

```rust
                    MenuAction::History => self.update(Message::History),
```

- [ ] **Step 7: Add the footer key hint**

Run: `grep -n "m msg\|footer" crates/gui/src/dashboard.rs | head -20`

Add `o history` immediately after `m msg`, matching the existing footer text's
exact formatting style.

- [ ] **Step 8: Build**

Run: `nix develop -c cargo build -p corral-gui 2>&1 | tail -40`
Expected: builds clean (GUI needs the devShell's `LD_LIBRARY_PATH`, per
AGENTS.md Development Setup).

- [ ] **Step 9: Run the full workspace test suite**

Run: `nix develop -c cargo test 2>&1 | tail -60`
Expected: all crates pass.

- [ ] **Step 10: Commit**

```bash
git add crates/gui/src/dashboard.rs
git commit -m "gui: o key fetches and opens the selected agent's history (parity with board)"
```

---

### Task 5: `corral-pi.ts` — implement `session/load`

**Files:**
- Modify: `extensions/corral-pi.ts`

- [ ] **Step 1: Flip the capability flag**

Find:
```typescript
			case "initialize":
				reply({
					protocolVersion: 1,
					agentCapabilities: { loadSession: false },
```
Change `loadSession: false` to `loadSession: true`.

- [ ] **Step 2: Add the `session/load` case**

In the `switch (msg.method)` block, add a case before `default:`:

```typescript
			case "session/load": {
				if (!currentCtx) return fail(-32603, "no active session");
				const ctxAtRequest = currentCtx;
				(async () => {
					try {
						const entries = await ctxAtRequest.sessionManager.getEntries();
						for (const e of entries as Array<{
							type?: string;
							message?: { role?: string; content?: unknown };
						}>) {
							if (e.type !== "message") continue;
							const role = e.message?.role;
							if (role !== "user" && role !== "assistant") continue;
							const text = messageText(e.message as { content?: unknown });
							if (!text) continue;
							conn.write(
								sessionUpdateLine({
									sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
									content: { type: "text", text },
								}),
							);
						}
						reply(sessionInfo(ctxAtRequest));
					} catch (e) {
						fail(-32603, `session/load failed: ${e}`);
					}
				})();
				break;
			}
```

Note the deliberate scope cut vs. the design doc's parenthetical: only
`type: "message"` entries (role user/assistant) are replayed — not
`tool_call`/other `SessionTreeEntry` types (`thinking_level_change`,
`model_change`, `compaction`, `branch_summary`, `custom`, `label`,
`session_info`), since there is no established ACP mapping for those and the
feature's ask is message history, not a full tool-call log (YAGNI). This
matches `message_end`'s existing filter (`role !== "user" && role !== "assistant"
=> return`) one function above this one in the file.

`cwd`/`mcpServers` from `msg.params` are intentionally ignored: this is the one
already-running in-process session replaying itself, not a fresh session
restore, so there is nothing to reconnect.

- [ ] **Step 2b: Sanity-check syntax**

Run: `cd extensions && node --check corral-pi.ts 2>&1 || true` (TypeScript
syntax is a superset Node's `--check` mostly tolerates via type-stripping in
>=22.18/24 per this repo's node version floor; if it errors on TS-only syntax
unrelated to this change, skip and instead run whatever existing lint/build
command this repo uses for extensions — check `package.json`/`Justfile` for an
extensions-specific check target first).

- [ ] **Step 3: Update the file's header doc comment**

Find the served-surface comment block near the top of the file:
```
 *   session/prompt        inject a user message (queued as follow-up while
```
Add a line above or below the `session/prompt` line:
```
 *   session/load           replay the full message history (user/assistant
 *                          text only) as session/update notifications, then
 *                          respond (ACP v1 session/load; agentClientProtocol.com
 *                          /protocol/session-setup#loading-sessions)
```

- [ ] **Step 4: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: implement session/load (replay message history)"
```

---

### Task 6: `corral-opencode.ts` — implement `session/load`

**Files:**
- Modify: `extensions/corral-opencode.ts`

- [ ] **Step 1: Flip the capability flag**

Find:
```typescript
					agentCapabilities: { loadSession: false },
```
Change to `loadSession: true`.

- [ ] **Step 2: Add the `session/load` case**

In the `switch (msg.method)` block, before `default:`:

```typescript
			case "session/load": {
				if (!activeSessionId) return fail(-32603, "no active session");
				const sid = activeSessionId;
				(async () => {
					try {
						// SDK-documented: session.messages({path}) -> {info: Message, parts: Part[]}[]
						// (opencode.ai/docs/sdk/#sessions). UNVERIFIED at runtime in this repo
						// (opencode is untypechecked here, per the file's existing UNVERIFIED
						// posture), so every field access stays guarded.
						const res = (await client.session.messages({ path: { id: sid } })) as {
							data?: Array<{ info?: { role?: string }; parts?: Array<{ type?: string; text?: string }> }>;
						};
						const list = Array.isArray(res?.data) ? res.data : [];
						for (const m of list) {
							const role = m?.info?.role;
							if (role !== "user" && role !== "assistant") continue;
							const text = (m.parts ?? [])
								.filter((p) => p?.type === "text" && typeof p.text === "string")
								.map((p) => p.text)
								.join("\n");
							if (!text) continue;
							conn.write(
								JSON.stringify({
									jsonrpc: "2.0",
									method: "session/update",
									params: {
										sessionId: sid,
										update: {
											sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
											content: { type: "text", text },
										},
									},
								}) + "\n",
							);
						}
						reply({ sessionId: sid, title: activeTitle, cwd: activeCwd });
					} catch (e) {
						fail(-32603, `session/load failed: ${e}`);
					}
				})();
				break;
			}
```

Run `grep -n "function broadcast\|acpUpdateLine\|JSON.stringify({\s*jsonrpc" extensions/corral-opencode.ts` first — if this file already has a
`sessionUpdateLine`-equivalent helper (unlike pi, it may inline the envelope
each time, as this snippet assumes), reuse it instead of inlining the
`JSON.stringify` above to avoid duplicating the envelope shape.

- [ ] **Step 3: Update the header doc comment**

Same style as Task 5 Step 3, added to this file's served-surface list.

- [ ] **Step 4: Commit**

```bash
git add extensions/corral-opencode.ts
git commit -m "corral-opencode: implement session/load (replay message history)"
```

---

### Task 7: `corral-claude/sidecar.ts` — implement `session/load` from the on-disk transcript

**Files:**
- Modify: `extensions/corral-claude/sidecar.ts`

- [ ] **Step 1: Capture `transcript_path` from hook events**

Claude Code includes `transcript_path` as a **documented common field on every
hook input** (code.claude.com/docs/en/hooks#common-input-fields): "Path to
conversation JSON." Add a module-level variable near the other session state
(`title`, `sessionId`, `cwd`):

```typescript
// Path to Claude Code's own on-disk transcript for this session, captured
// from the first hook event that carries it (every event does, per the
// Claude Code hooks reference's common input fields). Used to serve
// session/load, since the sidecar has no in-process transcript of its own.
let transcriptPath: string | undefined;
```

In `handleControl`, where hook events are dispatched (near `touchRegistry();`
and `setModelFromHook(ev);`), add:

```typescript
	if (typeof ev.transcript_path === "string" && ev.transcript_path) {
		transcriptPath = ev.transcript_path;
	}
```

- [ ] **Step 2: Flip the capability flag**

Find:
```typescript
				agentCapabilities: { loadSession: false },
```
Change to `loadSession: true`.

- [ ] **Step 3: Add the `session/load` case**

In the `switch (msg.method)` block of `handleAcp`, before `default:`:

```typescript
		case "session/load": {
			if (!transcriptPath) return fail(-32603, "no transcript available yet");
			const path = transcriptPath;
			(async () => {
				try {
					// Claude Code's transcript is one JSON object per line
					// (documented as "conversation JSON" but the line schema itself
					// is UNVERIFIED here — no Claude Code install in this repo).
					// Best-effort: skip any line that doesn't match the expected
					// {type, message:{role, content}} shape rather than throwing.
					const raw = fs.readFileSync(path, "utf8");
					for (const line of raw.split("\n")) {
						const trimmed = line.trim();
						if (!trimmed) continue;
						let entry: { type?: string; message?: { role?: string; content?: unknown } };
						try {
							entry = JSON.parse(trimmed);
						} catch {
							continue;
						}
						const role = entry.message?.role;
						if (entry.type !== "user" && entry.type !== "assistant") continue;
						if (role !== "user" && role !== "assistant") continue;
						const text = transcriptText(entry.message?.content);
						if (!text) continue;
						conn.write(
							acpUpdateLine(sessionId, {
								sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
								content: { type: "text", text },
							}),
						);
					}
					reply({ sessionId, title, cwd });
				} catch (e) {
					fail(-32603, `session/load failed: ${e}`);
				}
			})();
			break;
		}
```

Run `grep -n "function acpUpdateLine\|acpUpdateLine(" extensions/corral-claude/sidecar.ts extensions/corral-claude/lib.js 2>/dev/null` first to confirm
this helper's real name/signature (it is used elsewhere in the file per the
`broadcast()` helper seen during research: `acpUpdateLine(sessionId, update)`);
adjust the call above if the signature differs.

- [ ] **Step 4: Add the `transcriptText` helper**

Add near the top-level helpers (alongside wherever this file's other small
pure helpers live):

```typescript
// Best-effort extraction of the plain-text content from a transcript
// message's content field. Claude's transcript content is either a bare
// string or an array of content blocks (text/tool_use/tool_result/...);
// UNVERIFIED exact shape in this repo, so tolerate both and skip anything
// that doesn't look like text rather than throwing.
function transcriptText(content: unknown): string {
	if (typeof content === "string") return content;
	if (Array.isArray(content)) {
		return content
			.filter((b): b is { type?: string; text?: string } => typeof b === "object" && b !== null)
			.filter((b) => b.type === "text" && typeof b.text === "string")
			.map((b) => b.text as string)
			.join("\n");
	}
	return "";
}
```

- [ ] **Step 5: Update the file's header doc comment**

Add a line documenting `session/load` and its UNVERIFIED status (transcript
line schema), matching the file's existing UNVERIFIED-annotation style (see
its existing notes on `last_assistant_message`/hook payload fields).

- [ ] **Step 6: Commit**

```bash
git add extensions/corral-claude/sidecar.ts
git commit -m "corral-claude: implement session/load from the on-disk transcript (UNVERIFIED)"
```

---

### Task 8: `corral-cursor` — document-only (no code change)

**Files:**
- Modify: `extensions/corral-cursor/README.md` (or wherever this adapter's own
  limitations are documented — check for a "Limitations" or "Known gaps"
  section; if none exists, add one)

- [ ] **Step 1: Confirm no code change is needed**

Run: `grep -n "method not supported by corral-cursor" extensions/corral-cursor/lib.js`

Confirm the existing default-case error (`err(-32601, ...)`) already covers
`session/load` — it does, since `lib.js`'s `switch (msg.method)` has no
`session/load` case and falls through to that default. No code change.

- [ ] **Step 2: Document it**

Add a line (or bullet) to `extensions/corral-cursor/README.md` noting:
`session/load` (history export) is not supported — Cursor exposes no API to
read the Composer chat transcript; the adapter answers method-not-supported
like every other unimplemented method.

- [ ] **Step 3: Commit**

```bash
git add extensions/corral-cursor/README.md
git commit -m "corral-cursor: document session/load as unsupported"
```

---

### Task 9: VM e2e — `acp.py load` helper + scenario assertions

**Files:**
- Modify: `nix/tests/acp.py`
- Modify: `nix/tests/scenarios/pi.py`
- Modify: `nix/tests/scenarios/opencode.py`
- Modify: `nix/tests/scenarios/claude.py`
- Modify: `nix/tests/scenarios/cursor.py`

**Interfaces:**
- Produces: `acp.py load <socket> <sessionId> [secs]` — prints
  `{"ok":true,"chunks":N}` on success (N = number of
  user_message_chunk/agent_message_chunk notifications seen before the reply)
  or `{"ok":false,"error":"<message or 'unsupported'>"}`.

- [ ] **Step 1: Add `cmd_load` to `acp.py`**

Add after `cmd_cancel`:

```python
def cmd_load(path, sid, secs):
    # session/load replays history as session/update notifications, then
    # replies to the request itself (id 2, after id 1 = initialize here).
    # Count only message chunks (the feature's scope); ignore anything else.
    deadline = time.time() + secs
    s = connect(path, timeout=secs + 2)
    send(s, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    send(s, {"jsonrpc": "2.0", "id": 2, "method": "session/load",
              "params": {"sessionId": sid, "cwd": "", "mcpServers": []}})
    chunks = 0
    buf = b""
    while time.time() < deadline:
        try:
            chunk = s.recv(4096)
        except socket.timeout:
            break
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            msg = json.loads(line)
            if msg.get("method") == "session/update":
                upd = msg.get("params", {}).get("update", {})
                if upd.get("sessionUpdate") in ("user_message_chunk", "agent_message_chunk"):
                    chunks += 1
                continue
            if msg.get("id") == 2:
                if "error" in msg:
                    print(json.dumps({"ok": False, "error": "unsupported"}))
                    return
                print(json.dumps({"ok": True, "chunks": chunks}))
                return
    print(json.dumps({"ok": False, "error": "timeout"}))
```

Update the module docstring's `Usage:` block to add:
```
  acp.py load <socket> <sid> [secs]    -> session/load; prints
                                          {"ok":true,"chunks":N} or
                                          {"ok":false,"error":...}
```

Add the dispatch in `if __name__ == "__main__":`:

```python
    elif op == "load":
        cmd_load(sys.argv[2], sys.argv[3],
                  int(sys.argv[4]) if len(sys.argv) > 4 else 15)
```

- [ ] **Step 2: Extend `pi.py`'s scenario**

Run: `grep -n "acp(f\"prompt\|acp(f\"state" nix/tests/scenarios/pi.py | head -5`
to find a point after a real turn has completed (e.g. right after the existing
`acp(f"state {sock_a} idle 30")` following the "plain turn" section). Add
immediately after:

```python
# --- 2b. history export: session/load replays the turn we just ran --------
load_res = json.loads(acp(f"load {sock_a} {sid_a} 15"))
assert load_res.get("ok"), f"pi session/load failed: {load_res}"
assert load_res["chunks"] >= 2, f"expected at least a user+assistant chunk: {load_res}"
```

- [ ] **Step 3: Extend `opencode.py`'s scenario**

Read the file (`Read nix/tests/scenarios/opencode.py`) to find its equivalent
post-turn point (after its own prompt+state-idle sequence), and add the same
two lines pattern, adjusted to that file's actual socket/session-id variable
names (found via `grep -n "sock\|sid" nix/tests/scenarios/opencode.py`).

- [ ] **Step 4: Extend `claude.py`'s scenario**

Read the file (`Read nix/tests/scenarios/claude.py`). After its own
prompt+turn-completion point, add the same assertion pattern (claude
implements `session/load` per Task 7, so it should also succeed with
`chunks >= 2`) — but note claude's chunk count may undercount if its
`Stop`-hook-driven `agent_message_chunk` broadcast (the live one, not the
replay) and the transcript-replay categorization differ; if the scenario's
existing turn only exchanges one user + one final assistant message, `>= 1` is
the safer initial assertion for this adapter specifically (loosen only claude's
assertion, not pi's/opencode's).

- [ ] **Step 5: Extend `cursor.py`'s scenario**

Read the file (`Read nix/tests/scenarios/cursor.py`). Add, after its live
socket is established:

```python
load_res = json.loads(acp(f"load {sock} {sid} 5"))
assert not load_res.get("ok") and load_res.get("error") == "unsupported", \
    f"cursor should answer session/load as unsupported: {load_res}"
```

(adjust `sock`/`sid` variable names to whatever `cursor.py` actually calls
them — check via `grep -n "sock\|sid" nix/tests/scenarios/cursor.py` first).

- [ ] **Step 6: Run the affected e2e checks**

Run: `just e2e-one pi` (then `opencode`, `claude`, `cursor` in turn) —
these need KVM; if this sandbox cannot run them (no `/dev/kvm`), report that
explicitly instead of skipping silently, per this project's Sandbox rule
("prompt the user and ask him to fix specific... whatever things you expect to
do").

- [ ] **Step 7: Commit**

```bash
git add nix/tests/acp.py nix/tests/scenarios/pi.py nix/tests/scenarios/opencode.py nix/tests/scenarios/claude.py nix/tests/scenarios/cursor.py
git commit -m "e2e: assert session/load history replay (pi/opencode/claude) and unsupported (cursor)"
```

---

### Task 10: Documentation — AGENTS.md, README key table

**Files:**
- Modify: `AGENTS.md`
- Modify: `README.md` (only if it has a key table that needs the new `o` entry)

- [ ] **Step 1: Update the Known Limitations bullet**

Find in `AGENTS.md`:
```
- corral-pi answers `session/new`/`session/load` with method-not-
  supported: clients can discover, watch, and prompt running pi sessions, but
  attaching with history replay is not yet served.
```
Replace with:
```
- corral-pi and corral-opencode serve `session/load` with full message-history
  replay (user/assistant text only, not tool calls); corral-claude serves it
  best-effort from Claude Code's on-disk transcript (UNVERIFIED — no Claude
  Code install in this repo); corral-cursor does not support it (no transcript
  API). `session/new` remains unsupported everywhere (no client needs to
  create a second session on an existing agent). See `docs/superpowers/specs/
  2026-07-23-history-export-design.md`.
```

- [ ] **Step 2: Add `history.rs` to the `crates/core` module list**

In `AGENTS.md`'s `crates/core` bullet list (alongside `src/prompt.rs`,
`src/menu.rs`, etc.), add an entry:

```
  - `src/history.rs` — `fetch_history`: open a fresh ACP connection, send
    `session/load`, collect the replayed `session/update` notifications until
    the reply lands (5s timeout); `write_and_open`: wrap the collected entries
    plus session metadata into one JSON file (temp dir,
    `corral-history-<sessionId>-<unix_ts>.json`) and launch `xdg-open` on it
    detached, on the caller's own display (so a hidden/caged agent's history
    still opens somewhere real). Backs the `o` key in both shells. Unit-tested
    against a scripted `UnixListener`, like `prompt.rs`.
```

- [ ] **Step 3: Add `o` to the TUI/GUI key documentation**

In `AGENTS.md`'s "Interfaces to the Outside World" section, find the `corral`
and `corral-gui` key lists (`m` compose a message..., `d` close...) and add
`o` in the same sentence style:
```
`o` fetch the selected live agent's full message history (session/load) and
open it with xdg-open (no-op on a dormant card);
```

Check `README.md` for a key table (`grep -n "| Key\|m msg\|d del" README.md`)
— if one exists, add an `o` row (`history` / "fetch + open the selected
agent's full message history").

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md README.md
git commit -m "docs: document session/load history export (o key)"
```

---

## Final Verification

- [ ] Run `cargo test` (or `nix develop -c cargo test` for the GUI crate) across the whole workspace once more from the worktree root; confirm 0 failures.
- [ ] Run `cargo build` and `nix develop -c cargo build` once more; confirm both binaries build.
- [ ] Skim every commit's diff (`git log --oneline` since the branch point, `git diff main...HEAD` per file) against the spec's Scope/Out-of-Scope sections; confirm nothing beyond them landed.
- [ ] Hand off to `finishing-a-development-branch` for the merge decision once all tasks are checked off.
