# Corral Board Selection: One Rule (Enter go / Shift+Enter new) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the board's four action keys (`n`,`c`,`f` + Enter) into two verbs chosen by a modifier — Enter = go, Shift+Enter = new — on both the board and the `/` picker, make the mouse click two-stage, and fix the footer.

**Architecture:** All changes are in `crates/board`. `main.rs` (the imperative shell) loses the `n`/`c`/`f` key arms and the `Overlay::Spawn`/`Goto` variants, gaining one `Overlay::Jump`, a SHIFT branch on Enter, a two-stage `click_action`, and kitty keyboard-protocol enablement. `ui.rs` fixes the footer and the picker title. `picker.rs` sheds the now-dead `gather_dirs`/`selected_dir`.

**Tech Stack:** Rust, ratatui, crossterm 0.28.1.

## Global Constraints

- crossterm 0.28.1. Keyboard-protocol APIs: `crossterm::event::{KeyEvent, KeyModifiers, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags}` and `crossterm::terminal::supports_keyboard_enhancement`.
- Push only `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` (nothing else), so ordinary keys and the existing `KeyEventKind::Press` filter are unaffected.
- Shift+Enter detection: `key.modifiers.contains(KeyModifiers::SHIFT)` on `KeyCode::Enter`.
- Verify each task with `cargo test -p board` and `cargo clippy -p board -- -D warnings` (or `just test` / `just lint`).
- Commit after each task. Single-line commit messages, no attribution.

---

### Task 1: Enable the kitty keyboard protocol

Without this, Shift+Enter never arrives as a distinct key. No unit test (terminal side effect); verified by build + manual run in kitty.

**Files:**
- Modify: `crates/board/src/main.rs` (imports; `main()`)

- [ ] **Step 1: Extend the crossterm import**

In `crates/board/src/main.rs`, replace the `use crossterm::event::{...}` block with:

```rust
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
```

- [ ] **Step 2: Push/pop the flag in `main()`**

Replace the body of `main()` from `let mut terminal = ratatui::init();` through `ratatui::restore();` with:

```rust
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // Shift+Enter is only distinguishable from Enter under the kitty keyboard
    // protocol; push the disambiguate flag where the terminal supports it
    // (kitty does). Ordinary keys are unaffected.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let result = run(&mut terminal, &dir);
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
```

- [ ] **Step 3: Build**

Run: `cargo build -p board`
Expected: compiles (unused-import warnings for `KeyEvent`/`KeyModifiers` are fine; later tasks use them).

- [ ] **Step 4: Commit**

```bash
git add crates/board/src/main.rs
git commit -m "board: enable kitty keyboard protocol for shift+enter"
```

---

### Task 2: Two-stage mouse click (`click_action`)

**Files:**
- Modify: `crates/board/src/main.rs` (add `Click` enum + `click_action`; add tests mod; rewrite the left-click mouse arm)

**Interfaces:**
- Produces: `enum Click { Select, Go }`, `fn click_action(clicked: usize, selected: usize) -> Click`.

- [ ] **Step 1: Write the failing test**

Append to `crates/board/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_goes_only_on_the_already_selected_card() {
        assert!(matches!(click_action(3, 3), Click::Go));
        assert!(matches!(click_action(3, 1), Click::Select));
        assert!(matches!(click_action(0, 5), Click::Select));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p board click_goes_only`
Expected: FAIL — `cannot find function click_action` / `cannot find type Click`.

- [ ] **Step 3: Add the enum and function**

Add near the other free functions in `main.rs` (e.g. above `activate_selected`):

```rust
/// A left click first selects a card; only a click on the already-selected
/// card goes to it. Keeps a stray click from teleporting the operator's
/// window. Extracted pure so the rule is unit-tested.
enum Click {
    Select,
    Go,
}

fn click_action(clicked: usize, selected: usize) -> Click {
    if clicked == selected {
        Click::Go
    } else {
        Click::Select
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p board click_goes_only`
Expected: PASS.

- [ ] **Step 5: Wire it into the mouse arm**

In the `MouseEventKind::Down(MouseButton::Left)` arm of the main loop, replace:

```rust
                        if let Some(idx) = ui::hit_test(area, &board, m.column, m.row, scroll) {
                            selected = idx;
                            activate_selected(&focuser, &launcher, &board, selected, &mut status);
                        }
```

with:

```rust
                        if let Some(idx) = ui::hit_test(area, &board, m.column, m.row, scroll) {
                            match click_action(idx, selected) {
                                Click::Select => selected = idx,
                                Click::Go => {
                                    selected = idx;
                                    activate_selected(
                                        &focuser, &launcher, &board, selected, &mut status,
                                    );
                                }
                            }
                        }
```

- [ ] **Step 6: Test + commit**

Run: `cargo test -p board`
Expected: PASS.

```bash
git add crates/board/src/main.rs
git commit -m "board: two-stage left click (select, then go on second click)"
```

---

### Task 3: Merge the pickers into `Overlay::Jump` with Enter/Shift+Enter

Collapse `Overlay::Spawn` and `Overlay::Goto` into one `Jump`, teach `picker_input` the Shift+Enter spawn outcome, and act on it. `/` will open it (Task 4).

**Files:**
- Modify: `crates/board/src/main.rs` (`Overlay` enum, `PickerInput`, `picker_input`, `handle_overlay`, the draw match, the tests mod)

**Interfaces:**
- Consumes: `Click`/`click_action` (Task 2), `activate` (existing), `launch::default_cwd` (existing), `goto_label` (existing).
- Produces: `Overlay::Jump(Picker, Vec<model::Agent>)`; `PickerInput::SubmitSpawn`; `picker_input(&mut Picker, KeyEvent) -> PickerInput`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` mod in `main.rs`:

```rust
    #[test]
    fn shift_enter_in_picker_is_spawn() {
        let mut p = Picker::new(vec!["a".into()]);
        let plain = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let shift = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(matches!(picker_input(&mut p, plain), PickerInput::Submit));
        assert!(matches!(picker_input(&mut p, shift), PickerInput::SubmitSpawn));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p board shift_enter_in_picker`
Expected: FAIL — no `PickerInput::SubmitSpawn`, and `picker_input` takes `KeyCode`.

- [ ] **Step 3: Add the `SubmitSpawn` outcome and take a `KeyEvent`**

Replace the `PickerInput` enum and `picker_input` fn with:

```rust
/// The outcome of a key press inside a picker overlay.
enum PickerInput {
    /// The query or selection changed; keep the picker open.
    Continue,
    /// Esc: close without acting.
    Cancel,
    /// Enter: go to the current selection, then close.
    Submit,
    /// Shift+Enter: spawn a new agent in the selection's dir, then close.
    SubmitSpawn,
}

fn picker_input(p: &mut Picker, key: KeyEvent) -> PickerInput {
    match key.code {
        KeyCode::Esc => PickerInput::Cancel,
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => PickerInput::SubmitSpawn,
        KeyCode::Enter => PickerInput::Submit,
        KeyCode::Up => {
            p.up();
            PickerInput::Continue
        }
        KeyCode::Down => {
            p.down();
            PickerInput::Continue
        }
        KeyCode::Backspace => {
            p.backspace();
            PickerInput::Continue
        }
        KeyCode::Char(c) => {
            p.push(c);
            PickerInput::Continue
        }
        _ => PickerInput::Continue,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p board shift_enter_in_picker`
Expected: PASS.

- [ ] **Step 5: Collapse the `Overlay` enum**

Replace the `Overlay` enum with:

```rust
/// The active input overlay. Exactly one can be open, so the modes are
/// exclusive by construction: no parallel `Option`s to keep consistent.
enum Overlay {
    /// `/`: fuzzy-pick any agent to go to (Enter) or spawn beside (Shift+Enter).
    Jump(Picker, Vec<model::Agent>),
    /// `m`: compose a message to an agent.
    Compose(Compose),
}
```

- [ ] **Step 6: Rewrite `handle_overlay`'s picker arm**

In `handle_overlay`, replace the two arms `Overlay::Spawn(p) => ...` and `Overlay::Goto(p, targets) => ...` with this single arm (the `Overlay::Compose(c) => ...` arm stays unchanged), and update the `picker_input` calls to pass `key`:

```rust
        Overlay::Jump(p, targets) => match picker_input(p, key) {
            PickerInput::Continue => Some(ov),
            PickerInput::Cancel => None,
            PickerInput::Submit => {
                if let Some(a) = p.selected_original().and_then(|i| targets.get(i)) {
                    if let Err(e) = activate(a, focuser, launcher) {
                        *status = e;
                    }
                }
                None
            }
            PickerInput::SubmitSpawn => {
                if let Some(a) = p.selected_original().and_then(|i| targets.get(i)) {
                    let cwd = launch::default_cwd(Some(a));
                    if let Err(e) = launcher.spawn(&cwd) {
                        *status = format!("spawn: {e}");
                    }
                }
                None
            }
        },
```

- [ ] **Step 7: Update the draw match**

In the `terminal.draw(...)` closure, replace the two `Some(Overlay::Spawn(p))` / `Some(Overlay::Goto(p, _))` arms with one (render_picker loses its `verb` arg in Task 5; for now pass the label so it compiles — it will be simplified in Task 5):

```rust
                Some(Overlay::Jump(p, _)) => ui::render_picker(f, p, "jump"),
```

- [ ] **Step 8: Test + commit**

Run: `cargo test -p board`
Expected: PASS (the `f`/`c`/`n` key arms still reference the old overlays — they are replaced in Task 4; if the crate does not yet compile because of those arms, complete Step 1-3 of Task 4 before running. To keep this task self-contained, proceed to Task 4 now and commit together.)

> Note: Tasks 3 and 4 share a compile boundary (removing the old key arms). Implement Task 4 immediately, then run tests and commit once for both. The commit message:

```bash
git add crates/board/src/main.rs
git commit -m "board: single / picker, enter go / shift+enter new"
```

---

### Task 4: Board keys — `/` opens the picker, Enter/Shift+Enter verbs, drop `n`/`c`/`f`

**Files:**
- Modify: `crates/board/src/main.rs` (the board `match key.code` arms)

- [ ] **Step 1: Replace the Enter arm with a SHIFT branch**

In the main loop's `Event::Key(key) if key.kind == KeyEventKind::Press => match key.code { ... }`, replace:

```rust
                    KeyCode::Enter => {
                        activate_selected(&focuser, &launcher, &board, selected, &mut status)
                    }
```

with:

```rust
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        // Shift+Enter: spawn a new agent in the selected dir.
                        status.clear();
                        let cwd = launch::default_cwd(board.selectable().get(selected).copied());
                        if let Err(e) = launcher.spawn(&cwd) {
                            status = format!("spawn: {e}");
                        }
                    }
                    KeyCode::Enter => {
                        activate_selected(&focuser, &launcher, &board, selected, &mut status)
                    }
```

- [ ] **Step 2: Replace the `n`/`c`/`f` arms with a single `/` arm**

Delete the three arms `KeyCode::Char('n') => {...}`, `KeyCode::Char('c') => {...}`, and `KeyCode::Char('f') => {...}`, and in their place add:

```rust
                    KeyCode::Char('/') => {
                        // Fuzzy-pick any agent: Enter goes to it, Shift+Enter
                        // spawns a fresh agent in its dir.
                        status.clear();
                        let targets: Vec<model::Agent> =
                            board.selectable().into_iter().cloned().collect();
                        if !targets.is_empty() {
                            let labels = targets.iter().map(goto_label).collect();
                            overlay = Some(Overlay::Jump(Picker::new(labels), targets));
                        }
                    }
```

- [ ] **Step 3: Build + test**

Run: `cargo test -p board`
Expected: PASS. (No `Overlay::Spawn`/`Goto` or `n`/`c`/`f` references remain.)

- [ ] **Step 4: Commit** (shared with Task 3 — see Task 3 Step 8 message)

```bash
git add crates/board/src/main.rs
git commit -m "board: single / picker, enter go / shift+enter new"
```

---

### Task 5: Footer, picker title, and dead-code removal

**Files:**
- Modify: `crates/board/src/ui.rs` (`render`'s `help` string; `render_picker` signature/title; the draw call in `main.rs`)
- Modify: `crates/board/src/picker.rs` (remove `gather_dirs`, `selected_dir`; drop now-unused imports; fix the one test line)

- [ ] **Step 1: Fix the footer help line**

In `crates/board/src/ui.rs` `render`, replace:

```rust
    let help =
        "↑/↓ move   ⏎ focus/resume   m msg   f find   n new   c create   d close/forget   q quit";
```

with:

```rust
    let help =
        "↑↓←→ move   ⏎ go   ⇧⏎ new   / jump   m msg   d close/forget   q quit";
```

- [ ] **Step 2: Simplify `render_picker` (drop the `verb` param)**

In `ui.rs`, change the signature and title:

```rust
pub fn render_picker(frame: &mut Frame, picker: &Picker) {
    let area = centered(frame.area(), 70, 60);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" jump — type to filter, ⏎ go, ⇧⏎ new, esc cancel ")
        .borders(Borders::ALL);
```

- [ ] **Step 3: Update the call site in `main.rs`**

In the `terminal.draw` closure, replace:

```rust
                Some(Overlay::Jump(p, _)) => ui::render_picker(f, p, "jump"),
```

with:

```rust
                Some(Overlay::Jump(p, _)) => ui::render_picker(f, p),
```

- [ ] **Step 4: Remove dead picker code**

In `crates/board/src/picker.rs`: delete the `pub fn gather_dirs(...)` function and its doc comment, delete the `pub fn selected_dir(&self) -> Option<String> { ... }` method, and remove the now-unused imports `use std::collections::BTreeSet;` and `use crate::model::Board;`. Then fix the one test line in `matches_filter_and_selection` — replace:

```rust
        assert_eq!(p.selected_dir().as_deref(), Some("/home/u/projects/nixos"));
```

with:

```rust
        assert_eq!(p.matches(), vec!["/home/u/projects/nixos"]);
```

- [ ] **Step 5: Build, test, lint**

Run: `cargo test -p board && cargo clippy -p board -- -D warnings`
Expected: PASS, no warnings (no unused imports, no dead code).

- [ ] **Step 6: Commit**

```bash
git add crates/board/src/ui.rs crates/board/src/main.rs crates/board/src/picker.rs
git commit -m "board: honest footer, single picker title, drop dead dir-picker code"
```

---

### Task 6: Documentation

**Files:**
- Modify: `AGENTS.md`, `README.md` (corral repo root)

- [ ] **Step 1: Update AGENTS.md**

In `AGENTS.md`, update every place that names the old keys/behavior to the new model. Specifically:
- `src/picker.rs` bullet: it is no longer "the `c` spawn directory picker". Rewrite to: "the `/` jump picker: fuzzy-filter the board's agents (`board.selectable()`); Enter goes to one, Shift+Enter spawns a fresh agent in its dir. Subsequence fuzzy filter. Unit-tested."
- `src/main.rs` bullet and the "Interfaces to the Outside World" CLI paragraph: replace the key list with the new one — `Up/Down` (or `j/k`, scroll) within a column; `Left/Right` (or `h/l`) across columns; Enter or left-click go to the selected agent (focus a live window, resume a dormant session); Shift+Enter spawn a new agent in the selected dir; `/` open the fuzzy jump picker (Enter go, Shift+Enter new); `m` message; `d` close a live agent or forget a dormant record; `q`/Esc quit. Note the two-stage click (first click selects, a click on the already-selected card goes) and that Shift+Enter needs the kitty keyboard protocol.
- Remove mentions of `n`/`c`/`f` as distinct keys.

- [ ] **Step 2: Update README.md**

Mirror the same key-model change in `README.md` wherever the keybindings or usage are described (search for `n spawn`, `c `, `f fuzzy`, `focus/resume`).

- [ ] **Step 3: Verify no stale key references remain**

Run: `grep -nE "\bc create\b|\bf find\b|\bn new\b|focus/resume|c spawn|f fuzzy|gather_dirs" AGENTS.md README.md`
Expected: no matches.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md README.md
git commit -m "docs: board key model (enter go / shift+enter new, / jump)"
```

---

## Self-Review Notes

- **Spec coverage:** omni-picker→Task 3/4; Enter/Shift+Enter verbs→Task 3/4; drop n/c/f→Task 4; two-stage click→Task 2; footer→Task 5; kitty protocol→Task 1; dead-code removal→Task 5; docs→Task 6.
- **Compile boundary:** Tasks 3 and 4 must land together (removing `Overlay::Spawn`/`Goto` and the `n`/`c`/`f` arms is one atomic compile). They share one commit.
- **No behavior lost:** spawning in an arbitrary known project (old `c`) is now Shift+Enter on that project's agent in the `/` picker; spawning beside the selected card (old `n`) is Shift+Enter on the board. Starting in a never-used dir remains a manual terminal launch (unchanged premise).
