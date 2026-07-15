# Hidden Agents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let corral spawn and run agents in a headless compositor so they do background work without ever mapping a window on the host, and reveal them on demand by resume.

**Architecture:** A hidden agent runs inside a per-agent headless `cage` (`WLR_BACKENDS=headless … cage -- <argv>`), which never touches the host display server. Because a live surface cannot migrate between compositors, hide/reveal is always kill-and-resume-on-the-other-side, reusing the existing dormant-resume launch path. A new `hidden` record field (set via a `CORRAL_HIDDEN` env signal the adapter records) tells the board a card is hidden.

**Tech Stack:** Rust (workspace crates `corral-core`, `board`, `gui`, `daemon`), ratatui (TUI), iced (GUI), TypeScript pi extension, Nix flake, `cage`/wlroots.

## Global Constraints

- TUI (`corral`) and GUI (`corral-gui`) must reach feature parity: every user-facing key/badge lands in BOTH shells (hard rule from AGENTS.md).
- Shared logic lives in `corral-core`; only rendering differs between shells.
- `WLR_BACKENDS=headless` is mandatory on every cage launch — without it wlroots picks its X11 backend on an X11 host and opens a visible window.
- The hidden-spawn env signal is exactly `CORRAL_HIDDEN=1`; the record field is exactly `hidden` (JSON boolean, absent = false).
- No global installs: `cage` ships via `flake.nix` only.
- A hidden spawn with `cage` absent must fail loud (surface the error), never silently fall back to a visible window.
- Comments explain why, referring only to current code. Single-line, no-attribution git commits. TDD: failing test first.
- Run tests with `cargo test -p <crate>`; lint with `cargo clippy --all-targets -- -D warnings` (or `just test` / `just lint`).

---

### Task 1: `LaunchMode.hidden` + cage wrapping in `setsid_args`

**Files:**
- Modify: `crates/core/src/launch.rs` (`LaunchMode` struct, `setsid_args`, tests)

**Interfaces:**
- Consumes: existing `LaunchMode { gui, message_flag }`, `with_message`.
- Produces: `LaunchMode { gui: bool, message_flag: Option<String>, hidden: bool }`; `setsid_args` prepends `["env","WLR_BACKENDS=headless","CORRAL_HIDDEN=1","cage","--"]` when `hidden` is set (before the terminal-or-gui argv). Callers build `LaunchMode` by struct literal or via `Agent::launch_mode` / `RegistryEntry::launch_mode` (Task 2/3).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/launch.rs`:

```rust
#[test]
fn hidden_wraps_argv_in_headless_cage() {
    let term = vec!["xdg-terminal-exec".to_string()];
    let cmd = vec!["pi".to_string()];
    // Hidden terminal agent: cage wraps the terminal+command.
    let hidden = LaunchMode { gui: false, message_flag: None, hidden: true };
    assert_eq!(
        setsid_args(&hidden, &term, &cmd, None),
        vec![
            "env", "WLR_BACKENDS=headless", "CORRAL_HIDDEN=1", "cage", "--",
            "xdg-terminal-exec", "pi",
        ]
    );
    // Hidden GUI agent: cage wraps the command directly (no terminal).
    let hidden_gui = LaunchMode { gui: true, message_flag: None, hidden: true };
    let gui_cmd = vec!["quine".to_string(), "--corral".to_string()];
    assert_eq!(
        setsid_args(&hidden_gui, &term, &gui_cmd, None),
        vec![
            "env", "WLR_BACKENDS=headless", "CORRAL_HIDDEN=1", "cage", "--",
            "quine", "--corral",
        ]
    );
    // A launch message still appends inside the wrapped argv.
    assert_eq!(
        setsid_args(&hidden, &term, &cmd, Some("hi")),
        vec![
            "env", "WLR_BACKENDS=headless", "CORRAL_HIDDEN=1", "cage", "--",
            "xdg-terminal-exec", "pi", "hi",
        ]
    );
}
```

Also update the three existing `LaunchMode { … }` literals in this file's tests (`gui_launch_omits_the_terminal_prefix`) to add `hidden: false`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core launch::tests::hidden_wraps_argv_in_headless_cage`
Expected: FAIL to compile (`hidden` field unknown).

- [ ] **Step 3: Write minimal implementation**

In `crates/core/src/launch.rs`, extend the struct:

```rust
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LaunchMode {
    /// Run the command directly (a self-windowing GUI agent, e.g. quine)
    /// instead of wrapping it in a terminal.
    pub gui: bool,
    /// CLI flag that carries an initial launch message (e.g. `"--message"`).
    /// `None` passes the message as a trailing positional argument.
    pub message_flag: Option<String>,
    /// Run inside a headless `cage` so the window never maps on the host
    /// compositor. Set by a background ("hidden") spawn; the agent runs and
    /// announces normally, revealed later by resume in a real window.
    pub hidden: bool,
}
```

Rewrite `setsid_args` to wrap when hidden:

```rust
fn setsid_args(
    mode: &LaunchMode,
    terminal: &[String],
    command: &[String],
    message: Option<&str>,
) -> Vec<String> {
    let tail = with_message(command, message, mode.message_flag.as_deref());
    let mut inner = if mode.gui {
        tail
    } else {
        let mut args = terminal.to_vec();
        args.extend(tail);
        args
    };
    if mode.hidden {
        // WLR_BACKENDS=headless is load-bearing: without it wlroots picks its
        // X11 backend on an X11 host and opens a visible nested window, the
        // exact blink hidden mode avoids. cage brings XWayland, so terminal
        // and GUI agents alike render into its headless output. CORRAL_HIDDEN
        // signals the adapter to record `hidden` on the session.
        let mut wrapped = vec![
            "env".to_string(),
            "WLR_BACKENDS=headless".to_string(),
            "CORRAL_HIDDEN=1".to_string(),
            "cage".to_string(),
            "--".to_string(),
        ];
        wrapped.append(&mut inner);
        wrapped
    } else {
        inner
    }
}
```

Note: `TerminalLauncher::launch` still resolves the terminal for `!gui` even when hidden (cage runs it). No change needed there beyond calling `setsid_args`, which it already does.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p corral-core launch`
Expected: PASS (all launch tests, including the updated literals).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/launch.rs
git commit -m "core(launch): add hidden mode wrapping argv in headless cage"
```

---

### Task 2: Parse `hidden` from the registry record

**Files:**
- Modify: `crates/core/src/discovery.rs` (`RegistryEntry`, `parse_registry_json`, `launch_mode`, tests)

**Interfaces:**
- Consumes: `LaunchMode` from Task 1.
- Produces: `RegistryEntry.hidden: bool`; `RegistryEntry::launch_mode()` sets `hidden` from it.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/discovery.rs`:

```rust
#[test]
fn hidden_field_parses_true_false_and_absent() {
    let e = parse_registry_json(r#"{"sessionId":"s1","hidden":true}"#).unwrap();
    assert!(e.hidden);
    let e = parse_registry_json(r#"{"sessionId":"s2","hidden":false}"#).unwrap();
    assert!(!e.hidden);
    // Absent defaults to false (existing pi/opencode records).
    let e = parse_registry_json(r#"{"sessionId":"s3"}"#).unwrap();
    assert!(!e.hidden);
    // Non-boolean ignored leniently.
    let e = parse_registry_json(r#"{"sessionId":"s4","hidden":"yes"}"#).unwrap();
    assert!(!e.hidden);
    // launch_mode carries it.
    let e = parse_registry_json(r#"{"sessionId":"s5","hidden":true}"#).unwrap();
    assert!(e.launch_mode().hidden);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core discovery::tests::hidden_field_parses`
Expected: FAIL to compile (`hidden` field unknown).

- [ ] **Step 3: Write minimal implementation**

In `RegistryEntry`, add after `message_flag`:

```rust
    /// Whether this session runs hidden (inside a headless cage), so the board
    /// reveals it by resume rather than focusing a host window. Written by the
    /// adapter from the `CORRAL_HIDDEN` env signal corral sets at a hidden
    /// spawn. Absent/false is a normal visible session.
    pub hidden: bool,
```

In `parse_registry_json`, add to the constructed `RegistryEntry`:

```rust
        hidden: v.get("hidden").and_then(|x| x.as_bool()).unwrap_or(false),
```

In `RegistryEntry::launch_mode`:

```rust
    pub fn launch_mode(&self) -> crate::launch::LaunchMode {
        crate::launch::LaunchMode {
            gui: self.gui,
            message_flag: self.message_flag.clone(),
            hidden: self.hidden,
        }
    }
```

Update any `RegistryEntry { … }` struct literals in this file's tests to add `hidden: false`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p corral-core discovery`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/discovery.rs
git commit -m "core(discovery): parse hidden record field into launch mode"
```

---

### Task 3: `Agent.hidden`, stamp it and resume_command onto live agents

**Files:**
- Modify: `crates/core/src/model.rs` (`Agent`, `launch_mode`, `sync_registry`, tests)

**Interfaces:**
- Consumes: `RegistryEntry.hidden` (Task 2), `LaunchMode.hidden` (Task 1).
- Produces: `Agent.hidden: bool`; `Agent::launch_mode()` carries it; `sync_registry` stamps `hidden` AND `resume_command` onto live agents (reveal/hide need the resume argv on a live card).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/model.rs`:

```rust
#[test]
fn live_agent_gets_hidden_and_resume_from_record() {
    let mut b = Board::default();
    // A live agent keyed by socket; session id links it to a record.
    b.apply(Update::Upsert(agent("sess-1", State::Running)));
    let rec = RegistryEntry {
        session_id: "sess-1".into(),
        cwd: Some("/tmp/p".into()),
        title: None,
        socket: Some(PathBuf::from("/tmp/p/.corral/pi-9.sock")),
        spawn_command: Some(vec!["pi".into()]),
        resume_command: Some(vec!["pi".into(), "--session".into(), "sess-1".into()]),
        label: Some("pi".into()),
        last_seen: None,
        gui: false,
        message_flag: None,
        hidden: true,
    };
    b.sync_registry(&[rec], &HashSet::new());
    let live = b.in_state(State::Running);
    assert_eq!(live.len(), 1);
    assert!(live[0].hidden, "live agent must inherit hidden from its record");
    assert_eq!(
        live[0].resume_command.as_deref().unwrap(),
        ["pi", "--session", "sess-1"],
        "live agent must carry resume_command for reveal/hide"
    );
    assert!(live[0].launch_mode().hidden);
}
```

Note: the local `agent(path, state)` helper keys by `socket_path` = the string passed; pass `"sess-1"` and it also sets `session_id = Some("sess-1")`, matching the record.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core model::tests::live_agent_gets_hidden_and_resume_from_record`
Expected: FAIL to compile (`hidden` field unknown on `Agent`).

- [ ] **Step 3: Write minimal implementation**

In `Agent`, add after `message_flag`:

```rust
    /// Whether this session runs hidden (headless cage). Stamped from the
    /// record on both live and dormant agents; the board shows a `hidden`
    /// badge on a live hidden card and reveals it by resume instead of focus.
    pub hidden: bool,
```

In `Agent::launch_mode`:

```rust
    pub fn launch_mode(&self) -> crate::launch::LaunchMode {
        crate::launch::LaunchMode {
            gui: self.gui,
            message_flag: self.message_flag.clone(),
            hidden: self.hidden,
        }
    }
```

In `sync_registry`, the dormant-agent construction: add `hidden: e.hidden,` beside `gui: e.gui,`.

In `sync_registry`, the live-agent stamping loop, extend it:

```rust
        for a in self.live.values_mut() {
            if let Some(sid) = a.session_id.as_deref() {
                if let Some(e) = entries.iter().find(|e| e.session_id == sid) {
                    a.spawn_command = e.spawn_command.clone();
                    // Reveal/hide relaunch a live agent from its record, so a
                    // live card needs the resume argv too, not only spawn.
                    a.resume_command = e.resume_command.clone();
                    a.gui = e.gui;
                    a.message_flag = e.message_flag.clone();
                    a.hidden = e.hidden;
                }
            }
        }
```

Update the local `agent(...)` test helper and all `Agent { … }` / `RegistryEntry { … }` literals in this file's tests to add `hidden: false`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p corral-core model`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/model.rs
git commit -m "core(model): stamp hidden and resume_command onto live agents"
```

---

### Task 4: Placement decision + executor (`core/src/placement.rs`)

**Files:**
- Create: `crates/core/src/placement.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod placement;`)

**Interfaces:**
- Consumes: `Agent`, `Origin` (`model.rs`); `Launcher`, `LaunchMode` (`launch.rs`); `WindowFocuser` (`focus.rs`).
- Produces:
  - `pub enum Placement { Reveal, Hide, StartHidden }`
  - `pub fn placement_for(origin: Origin, hidden: bool) -> Placement` (pure)
  - `pub fn apply_placement(agent: &Agent, focuser: &dyn WindowFocuser, launcher: &dyn Launcher, kill: &dyn Fn(u32) -> Result<(), String>) -> Result<(), String>`
  - `pub fn kill_pid(pid: u32) -> Result<(), String>` (real kill, for shells to pass as `kill`)

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/placement.rs`:

```rust
//! Placement is which side an agent's window lives on: visible on the host, or
//! hidden in a headless cage. A live surface cannot migrate between
//! compositors, so changing placement is always kill-and-resume: stop the
//! current instance, relaunch from the persisted session on the other side.
//! `h` in either shell toggles placement via `placement_for` + `apply_placement`.

use std::path::Path;
use std::process::Command;

use crate::focus::WindowFocuser;
use crate::launch::Launcher;
use crate::model::{Agent, Origin};

/// What `h` does to the selected agent, decided purely from its placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Live + hidden: kill it, resume visible in the foreground.
    Reveal,
    /// Live + visible: close the window, resume hidden in a cage.
    Hide,
    /// Dormant: resume it hidden (start in the background).
    StartHidden,
}

/// Decide the placement toggle for an agent (pure).
pub fn placement_for(origin: Origin, hidden: bool) -> Placement {
    match (origin, hidden) {
        (Origin::Live, true) => Placement::Reveal,
        (Origin::Live, false) => Placement::Hide,
        (Origin::Dormant, _) => Placement::StartHidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::WindowFocuser;
    use crate::launch::{LaunchMode, Launcher};
    use crate::model::{Agent, Origin, State};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    fn live_agent(hidden: bool) -> Agent {
        Agent {
            socket_path: PathBuf::from("/tmp/p/.corral/pi-7.sock"),
            pid: 7,
            label: "pi".into(),
            session_id: Some("s1".into()),
            title: None,
            cwd: Some("/tmp/p".into()),
            state: State::Idle,
            origin: Origin::Live,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), "s1".into()]),
            activity: None,
            gui: false,
            message_flag: None,
            hidden,
        }
    }

    #[derive(Default)]
    struct Stub {
        closed: RefCell<Vec<u32>>,
        launched: RefCell<Vec<(Vec<String>, LaunchMode)>>,
        killed: RefCell<Vec<u32>>,
    }
    impl WindowFocuser for Stub {
        fn focus(&self, _a: &Agent) -> Result<(), String> { Ok(()) }
        fn close(&self, a: &Agent) -> Result<(), String> {
            self.closed.borrow_mut().push(a.pid);
            Ok(())
        }
    }
    impl Launcher for Stub {
        fn launch(&self, _cwd: &Path, command: &[String], _m: Option<&str>, mode: &LaunchMode)
            -> Result<(), String> {
            self.launched.borrow_mut().push((command.to_vec(), mode.clone()));
            Ok(())
        }
    }

    #[test]
    fn placement_dispatch_is_pure() {
        assert_eq!(placement_for(Origin::Live, true), Placement::Reveal);
        assert_eq!(placement_for(Origin::Live, false), Placement::Hide);
        assert_eq!(placement_for(Origin::Dormant, false), Placement::StartHidden);
        assert_eq!(placement_for(Origin::Dormant, true), Placement::StartHidden);
    }

    #[test]
    fn reveal_kills_pid_then_resumes_visible() {
        let s = Stub::default();
        let a = live_agent(true);
        apply_placement(&a, &s, &s, &|p| { s.killed.borrow_mut().push(p); Ok(()) }).unwrap();
        assert_eq!(*s.killed.borrow(), vec![7], "reveal kills the agent pid");
        assert!(s.closed.borrow().is_empty(), "reveal does not use focuser.close");
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].0, vec!["pi", "--session", "s1"]);
        assert!(!launched[0].1.hidden, "reveal resumes visible");
    }

    #[test]
    fn hide_closes_window_then_resumes_hidden() {
        let s = Stub::default();
        let a = live_agent(false);
        apply_placement(&a, &s, &s, &|_p| panic!("hide must not kill by pid")).unwrap();
        assert_eq!(*s.closed.borrow(), vec![7], "hide closes the host window");
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert!(launched[0].1.hidden, "hide resumes hidden");
    }

    #[test]
    fn start_hidden_resumes_dormant_without_kill() {
        let s = Stub::default();
        let mut a = live_agent(false);
        a.origin = Origin::Dormant;
        a.pid = 0;
        apply_placement(&a, &s, &s, &|_p| panic!("dormant has no process to kill")).unwrap();
        assert!(s.closed.borrow().is_empty());
        let launched = s.launched.borrow();
        assert_eq!(launched.len(), 1);
        assert!(launched[0].1.hidden, "start-hidden resumes hidden");
    }

    #[test]
    fn missing_resume_command_is_an_error() {
        let s = Stub::default();
        let mut a = live_agent(true);
        a.resume_command = None;
        assert!(apply_placement(&a, &s, &s, &|_| Ok(())).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core placement`
Expected: FAIL to compile (`apply_placement`, `kill_pid` not defined; module not declared).

- [ ] **Step 3: Write minimal implementation**

Add to `crates/core/src/lib.rs` beside the other `pub mod` lines:

```rust
pub mod placement;
```

Append the executor to `crates/core/src/placement.rs` (after `placement_for`, before `#[cfg(test)]`):

```rust
/// Execute the placement toggle for `agent`. Kill-and-resume in every branch:
/// there is no live surface migration between compositors. `kill` terminates a
/// pid (real: `kill_pid`); a stub in tests. Errors if the agent has no cwd or
/// resume command to relaunch from.
pub fn apply_placement(
    agent: &Agent,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    kill: &dyn Fn(u32) -> Result<(), String>,
) -> Result<(), String> {
    let placement = placement_for(agent.origin, agent.hidden);
    let cwd = agent
        .cwd
        .as_deref()
        .ok_or("placement: agent has no cwd to relaunch in")?;
    let command = agent
        .resume_command
        .as_deref()
        .ok_or("placement: agent has no resume command")?;
    // Target placement's launch mode: reveal -> visible, hide/start -> hidden.
    let mut mode = agent.launch_mode();
    match placement {
        Placement::Reveal => {
            // Hidden agent has no host window; kill its pid directly. cage
            // exits when its only app does, so the record then goes dormant.
            kill(agent.pid)?;
            mode.hidden = false;
        }
        Placement::Hide => {
            // Visible agent: close its host window (kill the window pid via the
            // focuser), then resume into a headless cage.
            focuser.close(agent).map_err(|e| format!("hide close: {e}"))?;
            mode.hidden = true;
        }
        Placement::StartHidden => {
            // Dormant: nothing running to kill; just resume hidden.
            mode.hidden = true;
        }
    }
    launcher
        .launch(Path::new(cwd), command, None, &mode)
        .map_err(|e| format!("placement resume: {e}"))
}

/// Terminate a process by pid via `kill(1)` (best-effort SIGTERM). The real
/// `kill` passed to `apply_placement`.
pub fn kill_pid(pid: u32) -> Result<(), String> {
    let ok = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map_err(|e| format!("kill failed: {e}"))?
        .success();
    if ok {
        Ok(())
    } else {
        Err("kill returned non-zero".into())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p corral-core placement`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/placement.rs crates/core/src/lib.rs
git commit -m "core(placement): hide/reveal/start-hidden via kill-and-resume"
```

---

### Task 5: Router spawns agent-initiated deliveries hidden by default

**Files:**
- Modify: `crates/daemon/src/router.rs` (`deliver_dir`, `deliver_session`, tests)

**Interfaces:**
- Consumes: `LaunchMode.hidden` (Task 1), `RegistryEntry::launch_mode` (Task 2).
- Produces: unchanged public surface; spawn/resume launches now set `hidden: true`.

- [ ] **Step 1: Write the failing test**

The `StubLauncher` in `router.rs` tests currently records only spawn vs resume counts. Extend it to also capture the last `LaunchMode.hidden`, then assert it. Add a field and assertion; example test to add to the `tests` module:

```rust
#[test]
fn dir_spawn_is_hidden_by_default() {
    let launcher = StubLauncher::default();
    let entries = vec![record_with_spawn("/work", "pi")]; // helper already in tests
    let msg = dir_message("/from", "/work", true); // force_new spawn
    deliver(&msg, &entries, &launcher);
    assert!(launcher.last_hidden.get(), "agent-initiated spawn must be hidden");
}
```

If the existing tests lack `record_with_spawn` / `dir_message` helpers, reuse whatever spawn-path test already exists (e.g. the test asserting `spawns == 1`) and add a `last_hidden` assertion to it instead of new helpers. Match the file's actual helper names.

Add to `StubLauncher`:

```rust
    last_hidden: Cell<bool>,
```

and in its `launch` impl set `self.last_hidden.set(mode.hidden);`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-daemon router`
Expected: FAIL (`last_hidden` unset/false — spawns are currently visible).

- [ ] **Step 3: Write minimal implementation**

In `deliver_dir`, where the mode is built for the spawn (currently from `spawn_command_for_label` / `spawn_command_for_dir` returning `(command, mode)`), set hidden before launching:

```rust
    let (command, mut mode) = match &msg.label {
        Some(label) => match spawn_command_for_label(entries, label) {
            Some(cm) => cm,
            None => return format!("route spawn: unknown label {label}"),
        },
        None => match spawn_command_for_dir(entries, dir) {
            Some(cm) => cm,
            None => return format!("route spawn: directory {dir} has no known agent kind"),
        },
    };
    // Agent-initiated spawns run hidden: an uninvited window must never pop up.
    mode.hidden = true;
    match launcher.launch(Path::new(dir), command, Some(&msg.tagged()), &mode) {
```

(Adapt the exact `match` arms to the current code; the change is `mut mode` + `mode.hidden = true;` before `launch`.)

In `deliver_session`, the dormant-resume branch: resume hidden too (a specific-session resume from an agent is also uninvited):

```rust
        (Some(cwd), Some(command)) => {
            let mut mode = entry.launch_mode();
            // Agent-initiated resume runs hidden, same rationale as dir spawn.
            mode.hidden = true;
            match launcher.launch(Path::new(cwd), command, Some(&msg.tagged()), &mode) {
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p corral-daemon`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/daemon/src/router.rs
git commit -m "daemon(router): spawn/resume agent-initiated deliveries hidden"
```

---

### Task 6: TUI — `h` toggle, hidden badge, reveal on Enter, hidden Shift+Enter

**Files:**
- Modify: `crates/board/src/main.rs` (key dispatch, `activate`/`go_to_selected`, `spawn_new`)
- Modify: `crates/board/src/ui.rs` (card badge rendering)

**Interfaces:**
- Consumes: `placement::{apply_placement, kill_pid}`, `Agent.hidden`, `Agent::launch_mode`.
- Produces: `h` key handling; a `hidden` badge on cards; Enter on a live hidden card reveals; Shift+Enter follows the selected card's hidden-ness.

- [ ] **Step 1: Write the failing test (ui badge helper)**

`ui.rs` already owns card formatting helpers. Add a small pure helper and test it. Add to `crates/board/src/ui.rs`:

```rust
/// The badge suffix shown after the kind badge: " hidden" for a live hidden
/// agent, empty otherwise. Kept pure so it is unit-tested without a terminal.
pub fn hidden_badge(agent: &corral_core::model::Agent) -> &'static str {
    use corral_core::model::Origin;
    if agent.origin == Origin::Live && agent.hidden {
        "hidden"
    } else {
        ""
    }
}
```

Add a test in `ui.rs`'s `tests` module (create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use corral_core::model::{Agent, Origin, State};
    use std::path::PathBuf;

    fn a(origin: Origin, hidden: bool) -> Agent {
        Agent {
            socket_path: PathBuf::new(), pid: 1, label: "pi".into(),
            session_id: None, title: None, cwd: None, state: State::Idle,
            origin, spawn_command: None, resume_command: None, activity: None,
            gui: false, message_flag: None, hidden,
        }
    }

    #[test]
    fn hidden_badge_only_for_live_hidden() {
        assert_eq!(hidden_badge(&a(Origin::Live, true)), "hidden");
        assert_eq!(hidden_badge(&a(Origin::Live, false)), "");
        assert_eq!(hidden_badge(&a(Origin::Dormant, true)), "");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral board::` (or `cargo test -p corral`)
Expected: FAIL to compile (`hidden_badge` not defined).

- [ ] **Step 3: Write minimal implementation**

Keep `hidden_badge` as above. In the card-drawing function of `ui.rs` (the two-row card: title row, then cwd-pill + kind-badge + activity row), append the badge next to the kind badge when non-empty, styled dim (reuse the existing dim style used for the kind badge). Example: after pushing the kind-badge span, push a dim ` hidden` span when `hidden_badge(agent)` is non-empty.

In `crates/board/src/main.rs`:

1. Add `use corral_core::placement::{apply_placement, kill_pid};`.

2. In `go_to_selected` / `activate` (the live branch), reveal a hidden agent instead of focusing:

```rust
    match agent.origin {
        Origin::Live if agent.hidden => {
            // No host window to focus; reveal = kill + resume visible.
            apply_placement(agent, focuser, launcher, &kill_pid)
                .map_err(|e| format!("reveal: {e}"))
        }
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, &agent.resume_command) {
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), command, None, &agent.launch_mode())
                .map_err(|e| format!("resume: {e}")),
            _ => Err("resume: dormant record missing cwd/resume command".into()),
        },
    }
```

3. Add an `h` key arm in the command-mode `match key.code` (beside `'m'`, `'d'`), and also in the filtering-mode key handler if command keys are dispatched there (mirror how `d`/`m` are handled — if they are command-mode-only, keep `h` command-mode-only too):

```rust
                    KeyCode::Char('h') => {
                        if let Some(agent) = board.selectable().get(selected).copied() {
                            status = match apply_placement(
                                agent, focuser.as_ref(), &launcher, &kill_pid,
                            ) {
                                Ok(()) => format!("toggling {}", ui::focus_label(agent)),
                                Err(e) => e,
                            };
                        }
                    }
```

4. In `spawn_new`, the spawn already uses `agent.launch_mode()`, which now carries `hidden` from the selected card. Because Shift+Enter should "spawn another of the same kind, same placement", `agent.launch_mode()` already yields `hidden: true` for a hidden selected card — no change needed beyond confirming it passes `agent.launch_mode()` (it does). Add a one-line comment noting Shift+Enter inherits the selected card's hidden placement.

5. Update the footer key-hint string (in `ui.rs` footer) to include `h hide/show`.

- [ ] **Step 4: Run tests + manual build**

Run: `cargo test -p corral && cargo build -p corral`
Expected: PASS + clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/board/src/main.rs crates/board/src/ui.rs
git commit -m "board: h toggles placement, hidden badge, reveal on enter"
```

---

### Task 7: GUI — `h` toggle, hidden badge, reveal on go, hidden spawn (parity)

**Files:**
- Modify: `crates/gui/src/dashboard.rs` (key dispatch `on_key`, `act_go`, `act_spawn`, `h` handling)
- Modify: `crates/gui/src/dashboard.rs` or its card view fn (badge rendering); footer hint

**Interfaces:**
- Consumes: same as Task 6 (`placement::{apply_placement, kill_pid}`, `Agent.hidden`).
- Produces: parity with the TUI: `h` key, hidden badge, reveal on go, spawn inherits placement.

- [ ] **Step 1: Write the failing test**

Reuse the pure `placement_for` decision (already tested in core) — the GUI wiring itself is thin iced glue not unit-tested here. Add one guard test in `dashboard.rs` if it has a `tests` module, else rely on core coverage and a build check. Minimal test (only if a `tests` module exists):

```rust
#[test]
fn go_reveals_a_live_hidden_agent() {
    use corral_core::model::{Origin};
    use corral_core::placement::{placement_for, Placement};
    // The GUI's go path must treat a live hidden agent as Reveal.
    assert_eq!(placement_for(Origin::Live, true), Placement::Reveal);
}
```

- [ ] **Step 2: Run test / build to verify current gap**

Run: `nix develop -c cargo build -p corral-gui`
Expected: builds today; the behavior gap (go on hidden focuses a non-existent window) is what we fix.

- [ ] **Step 3: Write minimal implementation**

1. Add `use corral_core::placement::{apply_placement, kill_pid};`.

2. In `act_go`, before the live-focus call, branch on hidden:

```rust
    // A live hidden agent has no host window; go reveals it (kill + resume).
    if agent.origin == Origin::Live && agent.hidden {
        self.status = match apply_placement(
            agent, self.focuser.as_ref(), &self.launcher, &kill_pid,
        ) {
            Ok(()) => "revealing".into(),
            Err(e) => e,
        };
        return Task::none();
    }
```

(Place it inside `act_go` where `agent` is resolved; keep the existing live-focus / dormant-resume branches otherwise.)

3. In `on_key`, add an `h` arm mirroring the TUI (iced key is `keyboard::Key::Character("h")`):

```rust
            keyboard::Key::Character(c) if c.as_str() == "h" => {
                if let Some(agent) = self.board.selectable().get(self.selected).copied() {
                    self.status = match apply_placement(
                        agent, self.focuser.as_ref(), &self.launcher, &kill_pid,
                    ) {
                        Ok(()) => "toggling".into(),
                        Err(e) => e,
                    };
                }
                Task::none()
            }
```

Make sure this sits with the other single-char command keys (`m`, `d`) and respects the same focused-filter guard they use.

4. `act_spawn` already builds the mode from the selected agent (`agent.launch_mode()`), so Shift+Enter inherits `hidden`. Confirm it uses `agent.launch_mode()`; add a comment.

5. In the card view builder, append a dim ` hidden` badge beside the kind badge when `agent.origin == Origin::Live && agent.hidden` (mirror `ui::hidden_badge` logic inline; iced has no shared ratatui helper).

6. Add `h hide/show` to the GUI footer key-hint row.

- [ ] **Step 4: Build to verify**

Run: `nix develop -c cargo build -p corral-gui && cargo test -p corral-gui`
Expected: clean build; tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gui/src/dashboard.rs
git commit -m "gui: h toggles placement, hidden badge, reveal on go (parity)"
```

---

### Task 8: pi extension writes `hidden` from `CORRAL_HIDDEN`

**Files:**
- Modify: `extensions/corral-pi.ts` (`writeRegistry`)

**Interfaces:**
- Consumes: `CORRAL_HIDDEN` env set by `core/launch.rs` (Task 1).
- Produces: `hidden` boolean in the registry record.

- [ ] **Step 1: Add the field**

In `writeRegistry`, capture the env once near the record build and include it:

```ts
		// A hidden spawn runs the agent inside a headless cage; corral sets
		// CORRAL_HIDDEN=1 in that environment. Record it so the board reveals
		// this session by resume instead of focusing a (non-existent) window.
		const hidden = process.env.CORRAL_HIDDEN === "1";
		const record = {
			sessionId,
			cwd: ctx.cwd,
			title: sessionTitle(ctx),
			label: "pi",
			socket,
			spawnCommand: ["pi"],
			resumeCommand: resumable ? ["pi", "--session", sessionId] : null,
			hidden,
			lastSeen: new Date().toISOString(),
		};
```

- [ ] **Step 2: Type/lint check**

Run: `cd extensions && npx tsc --noEmit corral-pi.ts 2>/dev/null || echo "no local ts toolchain — verify by inspection"`
Expected: no type errors (or manual inspection if no toolchain, per repo convention).

- [ ] **Step 3: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: record hidden from CORRAL_HIDDEN env"
```

---

### Task 9: opencode, claude, cursor adapters write `hidden`

**Files:**
- Modify: `extensions/corral-opencode.ts`
- Modify: `extensions/corral-claude/sidecar.ts` (the record writer)
- Modify: `extensions/corral-cursor/` record writer (`extension.js` / `lib.js`)

**Interfaces:**
- Same as Task 8: read `CORRAL_HIDDEN`, add `hidden` boolean to each record.

- [ ] **Step 1: Apply the same pattern per adapter**

In each adapter's registry-record construction, add:

```ts
hidden: process.env.CORRAL_HIDDEN === "1",
```

(for `extension.js`/`lib.js` use plain JS: `hidden: process.env.CORRAL_HIDDEN === "1",`). Place it beside the existing `label`/`gui` fields. These adapters are UNVERIFIED in-repo; match each file's existing record shape and add the one field.

- [ ] **Step 2: Inspect / build cursor lib test**

Run: `cd extensions/corral-cursor && node --test 2>/dev/null || echo "inspect only"`
Expected: existing cursor `lib.js` tests still PASS (or inspect).

- [ ] **Step 3: Commit**

```bash
git add extensions/corral-opencode.ts extensions/corral-claude/sidecar.ts extensions/corral-cursor
git commit -m "adapters: record hidden from CORRAL_HIDDEN (opencode, claude, cursor)"
```

---

### Task 10: Ship `cage` via the flake

**Files:**
- Modify: `flake.nix`

**Interfaces:**
- Produces: `cage` on the runtime PATH of `corral`, `corral-gui`, `corrald` (they exec `cage` for hidden spawns), and in the devShell.

- [ ] **Step 1: Add cage to the devShell and wrap the binaries**

In `flake.nix`, add `pkgs.cage` (and `pkgs.xwayland`, which cage needs for X11 agents) to the devShell `buildInputs`/`packages`. For the packaged binaries, extend the existing `wrapProgram` PATH (the same mechanism that wraps `corral-gui` with the driver library path) so `cage` is found at runtime:

```nix
    # cage renders hidden agents into a headless compositor; corral/corrald
    # exec it, so it must be on their runtime PATH. xwayland lets cage host
    # X11 agents.
    nativeBuildInputs = [ pkgs.makeWrapper ];
    postFixup = ''
      for b in corral corral-gui corrald; do
        wrapProgram $out/bin/$b --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.cage pkgs.xwayland ]}
      done
    '';
```

Adapt to the flake's actual package expression (it already wraps `corral-gui`; extend that wrap and add wraps for `corral`/`corrald`, or add `cage`/`xwayland` to the existing wrap's PATH prefix).

- [ ] **Step 2: Build via nix**

Run: `nix build .#default 2>&1 | tail -5 && ls result/bin`
Expected: build succeeds; `corral`, `corral-gui`, `corrald` present.

- [ ] **Step 3: Verify cage is reachable from the wrapped binary**

Run: `nix develop -c bash -c 'command -v cage'`
Expected: prints a cage path.

- [ ] **Step 4: Commit**

```bash
git add flake.nix
git commit -m "flake: ship cage (+xwayland) on corral/corrald/gui PATH"
```

---

### Task 11: Document the `hidden` field and `CORRAL_HIDDEN` signal

**Files:**
- Modify: `CONVENTION.md`
- Modify: `AGENTS.md`

**Interfaces:** documentation only.

- [ ] **Step 1: Update CONVENTION.md**

Add `hidden` to the registry record schema description (optional boolean, default false): "runs inside a headless compositor; the board reveals it by resume rather than focus." Document the `CORRAL_HIDDEN=1` env signal corral sets at a hidden spawn and that a cooperating adapter copies it into the record. Note the launch shape `env WLR_BACKENDS=headless CORRAL_HIDDEN=1 cage -- <argv>`.

- [ ] **Step 2: Update AGENTS.md**

In the architecture section, add a short "Hidden agents" paragraph: mechanism (per-agent headless cage), the physics (no surface migration → reveal = resume), the record `hidden` field, `h` toggle / enter-reveals / shift+enter-inherits-placement / m-does-not-unhide, corral_message_agent spawns hidden by default, and the `cage` dependency. Update the `launch.rs`, `discovery.rs`, `model.rs` bullets to mention `hidden`, add a `placement.rs` bullet, and note the daemon default. Add `cage` to Development Setup dependencies.

- [ ] **Step 3: Commit**

```bash
git add CONVENTION.md AGENTS.md
git commit -m "docs: document hidden agents (cage, CORRAL_HIDDEN, h toggle)"
```

---

### Task 12: Full workspace verification

**Files:** none (verification gate).

- [ ] **Step 1: Test + lint the whole workspace**

Run: `cargo test --workspace && cargo clippy --all-targets -- -D warnings`
Expected: all tests PASS, no clippy warnings.

- [ ] **Step 2: GUI build (needs devShell libs)**

Run: `nix develop -c cargo build -p corral-gui`
Expected: clean build.

- [ ] **Step 3: nix flake check**

Run: `nix flake check 2>&1 | tail -20`
Expected: passes (build + tests via nix).

- [ ] **Step 4: Commit any lint fixups**

```bash
git add -A && git commit -m "hidden agents: workspace test + lint green" || echo "nothing to commit"
```

---

## Self-Review

**Spec coverage:**
- Mechanism (headless cage, `WLR_BACKENDS=headless`, XWayland, CORRAL_HIDDEN) → Task 1, 10.
- Reveal/hide = kill-and-resume → Task 4.
- `hidden` record field + adapter cooperation → Task 2, 8, 9.
- `Agent.hidden` + live resume_command stamping → Task 3.
- Card badge + keys (`h`, enter-reveals, shift+enter-inherits, m-does-not-unhide) → Task 6 (TUI), 7 (GUI). `m` unchanged, so it does not unhide — satisfied by omission.
- corral_message_agent spawns hidden by default → Task 5.
- cage dependency via flake → Task 10.
- Docs → Task 11.
- v1 limits (fail loud if cage absent) → covered by no silent fallback; the launch simply errors and the shell shows it (Task 4/6/7 surface launch errors as status).

**Placeholder scan:** no TBD/TODO; every code step shows code. Adapter tasks (8/9) reference each file's existing record shape rather than repeating unknown surrounding code, which is correct for UNVERIFIED adapters.

**Type consistency:** `LaunchMode.hidden` (Task 1) used identically in Tasks 2–7; `Agent.hidden` (Task 3) used in Tasks 4/6/7; `apply_placement`/`kill_pid`/`placement_for`/`Placement` signatures (Task 4) match their call sites in Tasks 6/7; `hidden_badge` (Task 6) is TUI-only, GUI inlines the same condition (Task 7).
