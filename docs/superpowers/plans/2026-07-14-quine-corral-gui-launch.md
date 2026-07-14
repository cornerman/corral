# Quine / Corral GUI-Agent Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let corral launch a self-windowing GUI agent (quine) directly, without wrapping its command in a terminal, driven by a new optional `gui` boolean on the registry record.

**Architecture:** Quine already serves corral's ACP convention natively via a new compiled-in `--corral` interface (owned by the quine repo, out of scope here). The only corral-side gap is launch: `TerminalLauncher` always wraps spawn/resume in a terminal, which pops an unwanted empty terminal beside quine's own Wayland window. We add a `gui` flag that flows registry record -> `Agent` -> `Launcher::launch`; when true the launcher runs `setsid --fork <command>` directly. Absent/false preserves today's terminal-wrapped behavior, so pi and opencode records are untouched. Focus needs no change: quine's window carries `_NET_WM_PID` = quine's own pid = the socket-key pid, so the existing pid-matching focusers already find it.

**Tech Stack:** Rust (workspace: `corral-core` lib, `corral` TUI, `corral-gui` iced, `corral-daemon`). `serde_json` for lenient record parsing. `cargo test` / `cargo clippy`.

## Global Constraints

- The `gui` record field is **optional**; absent or `false` MUST mean terminal-wrapped (backward compatibility for all existing pi/opencode records, including dormant ones on disk). Only `true` triggers direct launch.
- Record parsing stays **lenient**: an unknown/mistyped field never breaks discovery (follow the existing `parse_registry_json` style — `as_bool` with `unwrap_or(false)`).
- corral names **no agent kind**: the `gui` flag rides in the record; corral never infers GUI-ness from `label`.
- Comments document why the current code is shaped as it is, referring only to current code (per AGENTS.md).
- Commits are single-line, no co-author attribution.
- `setsid --fork` is retained in BOTH branches: it detaches the launched window from corral so the focus parent-walk cannot climb into corral's own window.

---

### Task 1: Parse `gui` on the registry record

**Files:**
- Modify: `crates/core/src/discovery.rs` (the `RegistryEntry` struct and `parse_registry_json`)
- Test: `crates/core/src/discovery.rs` (inline `#[cfg(test)]` module)

**Interfaces:**
- Produces: `RegistryEntry.gui: bool` — true when the record's `"gui"` JSON field is boolean true, else false (absent, non-bool, or false).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/discovery.rs`:

```rust
    #[test]
    fn gui_field_parses_true_false_and_absent() {
        // Explicit true.
        let e = parse_registry_json(r#"{"sessionId":"s1","gui":true}"#).unwrap();
        assert!(e.gui);
        // Explicit false.
        let e = parse_registry_json(r#"{"sessionId":"s2","gui":false}"#).unwrap();
        assert!(!e.gui);
        // Absent defaults to false (pi/opencode records have no such field).
        let e = parse_registry_json(r#"{"sessionId":"s3"}"#).unwrap();
        assert!(!e.gui);
        // A non-boolean value is ignored leniently, not an error.
        let e = parse_registry_json(r#"{"sessionId":"s4","gui":"yes"}"#).unwrap();
        assert!(!e.gui);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core gui_field_parses -- --nocapture`
Expected: FAIL — compile error, `RegistryEntry` has no field `gui`.

- [ ] **Step 3: Add the field and parse it**

In `crates/core/src/discovery.rs`, add to the `RegistryEntry` struct (after `last_seen`):

```rust
    /// Whether corral launches this agent's command directly (a self-windowing
    /// GUI app like quine) instead of wrapping it in a terminal. Absent/false
    /// means terminal-wrapped, so every existing pi/opencode record keeps its
    /// behavior; only an explicit `true` opts into direct launch.
    pub gui: bool,
```

In `parse_registry_json`, add to the `RegistryEntry { .. }` literal (after `last_seen: str_field("lastSeen"),`):

```rust
        gui: v.get("gui").and_then(|x| x.as_bool()).unwrap_or(false),
```

- [ ] **Step 4: Fix the other `RegistryEntry` literals so the crate compiles**

Search for every `RegistryEntry {` literal outside `parse_registry_json` and add `gui: false,`:

Run: `rg -n "RegistryEntry \{" crates`

Add `gui: false,` to each such literal (test fixtures in `crates/core/src/discovery.rs`, `crates/daemon/src/router.rs` tests, and any others the search reports). Use `false` — fixtures test terminal agents.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p corral-core discovery`
Expected: PASS, including `gui_field_parses_true_false_and_absent`.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/discovery.rs crates/daemon/src/router.rs
git commit -m "core: parse optional gui flag on registry record"
```

---

### Task 2: Carry `gui` on the `Agent` model

**Files:**
- Modify: `crates/core/src/model.rs` (the `Agent` struct; `sync_registry` dormant-construction and live-stamp)
- Modify: `crates/core/src/watch.rs` (the live `Agent` literal ~line 178)
- Modify: `crates/core/src/picker.rs`, `crates/board/src/ui.rs` (test/fixture `Agent` literals)
- Test: `crates/core/src/model.rs` (inline `tests` module)

**Interfaces:**
- Consumes: `RegistryEntry.gui` (Task 1).
- Produces: `Agent.gui: bool` — set from the matching record for both dormant and live agents.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/model.rs`. A GUI record with a cleared socket becomes a dormant `Agent` whose `gui` is true:

```rust
    #[test]
    fn dormant_agent_inherits_gui_from_record() {
        let mut board = Board::default();
        let rec = RegistryEntry {
            session_id: "q1".into(),
            cwd: Some("/tmp/q".into()),
            title: None,
            socket: None, // cleared => dormant
            spawn_command: Some(vec!["quine".into(), "--corral".into()]),
            resume_command: Some(vec![
                "quine".into(),
                "--session".into(),
                "q1".into(),
                "--corral".into(),
            ]),
            label: Some("quine".into()),
            last_seen: None,
            gui: true,
        };
        board.sync_registry(&[rec], &std::collections::HashSet::new());
        let dormant = board.dormant();
        assert_eq!(dormant.len(), 1);
        assert!(dormant[0].gui, "dormant quine card must carry gui=true");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core dormant_agent_inherits_gui`
Expected: FAIL — compile error, `Agent` has no field `gui`.

- [ ] **Step 3: Add the field and propagate it**

In `crates/core/src/model.rs`, add to the `Agent` struct (after `activity`):

```rust
    /// Whether corral launches this agent's command directly (self-windowing
    /// GUI app) instead of terminal-wrapped. Stamped from the record's `gui`
    /// on both dormant and live agents, so spawn/resume beside any card picks
    /// the right launch mode.
    pub gui: bool,
```

In `sync_registry`, the dormant-construction `Agent { .. }` literal, add (after `activity: None,`):

```rust
                gui: e.gui,
```

In `sync_registry`, the live-stamp loop (where `a.spawn_command = e.spawn_command.clone();`), add alongside it:

```rust
                    a.gui = e.gui;
```

- [ ] **Step 4: Fix the remaining `Agent` literals so the workspace compiles**

Run: `rg -n "Agent \{" crates`

Add `gui: false,` to each `Agent { .. }` literal that does not yet set it: the live-agent literal in `crates/core/src/watch.rs` (~line 178, next to `spawn_command: None,`), and the fixture literals in `crates/core/src/picker.rs` and `crates/board/src/ui.rs`. (`watch.rs` gets `false`; the engine's live-stamp in `sync_registry` overwrites it from the record each scan, exactly as it does for `spawn_command`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p corral-core`
Expected: PASS, including `dormant_agent_inherits_gui_from_record`.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/model.rs crates/core/src/watch.rs crates/core/src/picker.rs crates/board/src/ui.rs
git commit -m "core: carry gui flag on Agent from record"
```

---

### Task 3: Direct-launch branch in the launcher

**Files:**
- Modify: `crates/core/src/launch.rs` (`Launcher` trait, `TerminalLauncher`, a new pure `setsid_args` helper)
- Test: `crates/core/src/launch.rs` (inline `tests` module)

**Interfaces:**
- Produces: `Launcher::launch(&self, cwd: &Path, command: &[String], message: Option<&str>, gui: bool) -> Result<(), String>` — the new trailing `gui` param.
- Produces: `fn setsid_args(gui: bool, terminal: &[String], command: &[String], message: Option<&str>) -> Vec<String>` — pure argv builder for everything after `setsid --fork`; GUI omits the terminal prefix.

This task changes the trait signature, so every call site must compile. To keep it self-contained and testable, callers pass a literal `false` here; Task 4 threads the real `agent.gui` / `entry.gui` values.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/core/src/launch.rs` (extend the `use super::...` line to include `setsid_args`):

```rust
    #[test]
    fn gui_launch_omits_the_terminal_prefix() {
        let term = vec!["xdg-terminal-exec".to_string()];
        let cmd = vec!["quine".to_string(), "--corral".to_string()];
        // GUI: run the command directly, no terminal prefix.
        assert_eq!(
            setsid_args(true, &term, &cmd, None),
            vec!["quine".to_string(), "--corral".to_string()]
        );
        // Non-GUI: terminal prefix in front, exactly as before.
        assert_eq!(
            setsid_args(false, &term, &cmd, None),
            vec![
                "xdg-terminal-exec".to_string(),
                "quine".to_string(),
                "--corral".to_string()
            ]
        );
        // The message is appended in both modes (space-guard still applies).
        assert_eq!(
            setsid_args(true, &term, &cmd, Some("hi")),
            vec!["quine".to_string(), "--corral".to_string(), "hi".to_string()]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core gui_launch_omits`
Expected: FAIL — compile error, `setsid_args` not found.

- [ ] **Step 3: Add the pure helper and use it in `launch`**

In `crates/core/src/launch.rs`, add the pure helper (near `with_message`):

```rust
/// Build the argv that follows `setsid --fork`. A GUI agent is run directly
/// (its command only); a terminal agent gets the resolved terminal prefix in
/// front. The initial message, if any, is appended in both modes via
/// `with_message` (so its leading -/@ space-guard still applies).
fn setsid_args(
    gui: bool,
    terminal: &[String],
    command: &[String],
    message: Option<&str>,
) -> Vec<String> {
    let tail = with_message(command, message);
    if gui {
        tail
    } else {
        let mut args = terminal.to_vec();
        args.extend(tail);
        args
    }
}
```

Update the trait method signature and its doc in the `Launcher` trait:

```rust
    /// ... existing doc ...
    /// `gui` selects the launch mode: `true` runs the command directly (a
    /// self-windowing GUI agent), `false` wraps it in a resolved terminal.
    fn launch(
        &self,
        cwd: &Path,
        command: &[String],
        message: Option<&str>,
        gui: bool,
    ) -> Result<(), String>;
```

Rewrite `TerminalLauncher::launch`. A GUI launch needs no terminal, so terminal resolution only runs (and only errors) in the terminal branch:

```rust
    fn launch(
        &self,
        cwd: &Path,
        command: &[String],
        message: Option<&str>,
        gui: bool,
    ) -> Result<(), String> {
        if command.is_empty() {
            return Err("launch: empty command".into());
        }
        // A GUI agent draws its own window, so it needs no terminal (and must
        // not resolve one). setsid --fork still detaches it from corral so the
        // focus parent-walk cannot climb into corral's window.
        let terminal = if gui {
            Vec::new()
        } else {
            resolve_terminal().ok_or(
                "no terminal found: install xdg-terminal-exec, or set $CORRAL_TERMINAL \
                 (e.g. \"alacritty -e\") or $TERMINAL",
            )?
        };
        let args = setsid_args(gui, &terminal, command, message);
        let ok = Command::new("setsid")
            .arg("--fork")
            .args(&args)
            .current_dir(cwd)
            .status()
            .map_err(|e| format!("terminal launch failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("terminal launch returned non-zero".into())
        }
    }
```

- [ ] **Step 4: Update every call site to pass `false` (real values come in Task 4)**

Run: `rg -n "\.launch\(" crates | rg -v "fn launch"`

Append `, false` as the new fourth argument at each of these 7 call sites:
- `crates/board/src/main.rs:140`, `:539`, `:581`
- `crates/gui/src/dashboard.rs:428`, `:471`, `:923`
- `crates/daemon/src/router.rs:178`, `:212`

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p corral-core gui_launch_omits && cargo build --workspace`
Expected: PASS and a clean workspace build.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/launch.rs crates/board/src/main.rs crates/gui/src/dashboard.rs crates/daemon/src/router.rs
git commit -m "core: add gui direct-launch branch to Launcher"
```

---

### Task 4: Thread the real `gui` flag through every launch call

**Files:**
- Modify: `crates/board/src/main.rs` (`activate`, `spawn_new`, `ComposeTarget::Dormant` deliver, `ComposeTarget` enum + `open_compose`)
- Modify: `crates/gui/src/dashboard.rs` (`act_spawn`, `activate`, `deliver`, `ComposeTarget` enum + `compose_for`)
- Modify: `crates/daemon/src/router.rs` (`deliver_to_dir` spawn, `deliver_session`, `spawn_command_for_dir`)

**Interfaces:**
- Consumes: `Agent.gui` (Task 2), `RegistryEntry.gui` (Task 1), `Launcher::launch(.., gui)` (Task 3).
- Produces: correct launch mode per agent; the `ComposeTarget::Dormant` variant gains a `gui: bool` so a resumed-to-deliver launch knows its mode.

- [ ] **Step 1: Board — resume and spawn use `agent.gui`**

In `crates/board/src/main.rs`, in `activate` (the `Origin::Dormant` resume launch):

```rust
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), command, None, agent.gui)
                .map_err(|e| format!("resume: {e}")),
```

In `spawn_new` (the spawn launch):

```rust
    match launcher.launch(&cwd, command, None, agent.gui) {
```

- [ ] **Step 2: Board — carry `gui` on the dormant compose target**

In `crates/board/src/main.rs`, add a `gui: bool` field to the `ComposeTarget::Dormant` variant (find `enum ComposeTarget` and the `Dormant { cwd, resume_command }` variant):

```rust
    Dormant {
        cwd: String,
        resume_command: Vec<String>,
        gui: bool,
    },
```

In `open_compose`, set it from the agent:

```rust
        Origin::Dormant => match (&a.cwd, &a.resume_command) {
            (Some(cwd), Some(command)) => Some(ComposeTarget::Dormant {
                cwd: cwd.clone(),
                resume_command: command.clone(),
                gui: a.gui,
            }),
            _ => None,
        },
```

At the dormant deliver launch (around line 140, where the `ComposeTarget::Dormant` is destructured and `launcher.launch(Path::new(cwd), resume_command, Some(text), false)` is called), destructure `gui` and pass it:

```rust
                            ComposeTarget::Dormant { cwd, resume_command, gui } => {
                                match launcher.launch(Path::new(cwd), resume_command, Some(text), *gui) {
```

(Match the exact surrounding destructure; the change is adding `gui` to the pattern and replacing the trailing `false` with `*gui`.)

- [ ] **Step 3: GUI — spawn, resume, and dormant deliver use `gui`**

In `crates/gui/src/dashboard.rs`, `act_spawn`:

```rust
                match self.launcher.launch(&cwd, command, None, a.gui) {
```

`activate` (dormant resume):

```rust
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), command, None, agent.gui)
                .map_err(|e| format!("resume: {e}")),
```

Add `gui: bool` to this crate's `ComposeTarget::Dormant` variant (find the `enum ComposeTarget`):

```rust
    Dormant {
        cwd: String,
        resume_command: Vec<String>,
        gui: bool,
    },
```

In `compose_for`, set it:

```rust
        Origin::Dormant => ComposeTarget::Dormant {
            cwd: agent.cwd.clone()?,
            resume_command: agent.resume_command.clone()?,
            gui: agent.gui,
        },
```

In `deliver`, destructure and pass `gui`:

```rust
            ComposeTarget::Dormant {
                cwd,
                resume_command,
                gui,
            } => match self
                .launcher
                .launch(Path::new(cwd), resume_command, Some(text), *gui)
            {
```

- [ ] **Step 4: Daemon — session and dir routing use `gui`**

In `crates/daemon/src/router.rs`, `deliver_session` (the resume launch) uses the record it already holds:

```rust
            match launcher.launch(Path::new(cwd), command, Some(&msg.tagged()), entry.gui) {
```

For `deliver_to_dir`, extend the dir lookup to also report `gui`. Change `spawn_command_for_dir` to return the record's `gui` alongside the command:

```rust
/// A spawn command announced by any record whose cwd is `dir`, live or dormant,
/// with that record's gui launch mode.
fn spawn_command_for_dir<'a>(entries: &'a [RegistryEntry], dir: &str) -> Option<(&'a [String], bool)> {
    entries
        .iter()
        .filter(|e| e.cwd.as_deref() == Some(dir))
        .find_map(|e| e.spawn_command.as_deref().map(|c| (c, e.gui)))
}
```

Update its caller in `deliver_to_dir`:

```rust
    let Some((command, gui)) = spawn_command_for_dir(entries, dir) else {
        return format!("route: no known agent kind for {dir} (never announced there)");
    };
    match launcher.launch(Path::new(dir), command, Some(&msg.tagged()), gui) {
```

- [ ] **Step 5: Build and test the whole workspace**

Run: `cargo build --workspace && cargo test --workspace`
Expected: clean build, all tests pass.

- [ ] **Step 6: Lint**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. (Fixture `Launcher` mocks in tests may need their `launch` signature updated to the new 4-arg form; do so if the build reports it, ignoring the `gui` arg in the mock body.)

- [ ] **Step 7: Commit**

```bash
git add crates/board/src/main.rs crates/gui/src/dashboard.rs crates/daemon/src/router.rs
git commit -m "board/gui/daemon: launch gui agents directly via the gui flag"
```

---

### Task 5: Document the `gui` field and quine as a worked kind

**Files:**
- Modify: `CONVENTION.md` (the registry record field list)
- Modify: `AGENTS.md` (crate docs for `launch.rs`; Future/limitations note)
- Modify: `README.md` (if it enumerates record fields or worked agent kinds)

**Interfaces:** none (documentation only).

- [ ] **Step 1: Document the field in CONVENTION.md**

In `CONVENTION.md`, in the registry record field list, add an entry for `gui` (optional boolean, default false = terminal-wrapped, true = launched directly for self-windowing GUI agents). Keep the existing table/prose style. State the failproof default explicitly: a record without `gui` is terminal-wrapped, so no existing agent changes.

- [ ] **Step 2: Update AGENTS.md**

In `AGENTS.md`, in the `src/launch.rs` bullet, note that `launch` takes a `gui` flag: a GUI agent (e.g. quine) is run directly with `setsid --fork <command>` (no terminal resolution), while `setsid --fork` still detaches it so the focus walk cannot climb into corral. Add quine to the "Future / more than pi" narrative as a third worked kind that serves the convention natively (no adapter file), distinguished by `gui: true` and focused as its own Wayland/X11 window (pid = window pid, no terminal ancestor).

- [ ] **Step 3: Update README.md if needed**

Run: `rg -n "spawnCommand|resumeCommand|opencode|worked" README.md`
If README enumerates record fields or worked agent kinds, add `gui` / quine consistently with AGENTS.md. If it does not, leave it.

- [ ] **Step 4: Commit**

```bash
git add CONVENTION.md AGENTS.md README.md
git commit -m "docs: document gui record field and quine as a native worked kind"
```

---

## Self-Review

**Spec coverage:**
- Optional `gui` field, absent=terminal (backward compat) — Task 1 (parse + default), Task 5 (documented).
- Flag flows record -> Agent (live + dormant) — Task 2.
- Direct-launch branch (no terminal, setsid retained) — Task 3.
- All 7 call sites + both `ComposeTarget::Dormant` variants + daemon dir/session routing thread the real flag — Task 4.
- No focus change (pid = window pid) — noted in plan preamble and Task 5 docs; no code task needed.
- corral names no agent kind (flag rides in record, never inferred from label) — enforced by design; Task 1/2/4 read `gui` only from the record/Agent.

**Placeholder scan:** No TBD/TODO; every code step shows exact code; commands have expected output.

**Type consistency:** `gui: bool` used uniformly on `RegistryEntry`, `Agent`, both `ComposeTarget::Dormant` variants, `Launcher::launch(.., gui: bool)`, `setsid_args(gui, ..)`, and `spawn_command_for_dir -> Option<(&[String], bool)>`. Call sites pass `agent.gui` / `entry.gui` / destructured `*gui`.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-14-quine-corral-gui-launch.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Which approach?
