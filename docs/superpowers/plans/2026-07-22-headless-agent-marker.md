# Headless Agent Marker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an adapter announce that its session runs *headless* (alive, announcing over its socket, but with no window at all — e.g. pi embedded as a library or opencode `serve`), so the board renders it as `headless` and never tries to focus a window that does not exist.

**Architecture:** A new adapter-declared record field `headless` (boolean, default false), mirrored onto the live/dormant `Agent`, exactly like the existing `hidden`/`gui` fields. The board reads this one declared fact and changes three behaviors for a headless card: it shows a `headless` pill, `go` (Enter/double-click) is an informative no-op instead of a failing focus attempt, and `h` (toggle placement) refuses. Kill paths (`d`, card-move → Dormant) kill by pid, since there is no window to close. corral needs no per-harness logic — the party that knows (the adapter) declares it.

**Tech Stack:** Rust (workspace crates `corral-core`, `corral` TUI board, `corral-gui` iced board), TypeScript (`extensions/corral-pi.ts`), Nix VM e2e tests (`nix/tests/`).

## Global Constraints

- **`headless` is distinct from `hidden`.** `hidden` means "corral launched it in a headless `cage` and can reveal it by kill-and-resume into a visible terminal" (reversible by corral). `headless` means "the process inherently has no window and corral cannot give it one" (resuming as a terminal would be a *different* process). Never conflate them: a headless card must NOT trigger `apply_placement`/reveal.
- **TUI/GUI parity (hard rule, AGENTS.md).** Every behavior in this plan lands in BOTH `crates/board` and `crates/gui`, always. Shared decisions live in `corral-core`.
- **VM E2E parity (hard rule, AGENTS.md).** A user-facing board-behavior change updates `nix/tests/` in the same change (Task 7).
- **Lenient parsing.** A non-boolean `headless` value defaults to `false` (matches `hidden` parsing in `discovery.rs`). Absent defaults to `false`, so every existing record is unchanged.
- **Field/type names, verbatim:** JSON key `headless`; `RegistryEntry.headless: bool`; `Agent.headless: bool`; TUI `ui::headless_badge(agent) -> &'static str`; env signal `CORRAL_HEADLESS=1`.
- **Commits:** small, single-line, no co-authored attribution (AGENTS.md).
- **Comments document the current code only, and explain *why*** (AGENTS.md).

---

## File Structure

- `crates/core/src/discovery.rs` — parse `headless` into `RegistryEntry`; unit test.
- `crates/core/src/model.rs` — `Agent.headless`; stamp it in `sync_registry` (live + dormant); unit test.
- `crates/board/src/ui.rs` — `headless_badge`; unit test.
- `crates/board/src/main.rs` — `activate` no-op guard, `toggle_selected` guard, `dismiss_selected` + `commit_move` kill-by-pid for headless.
- `crates/gui/src/dashboard.rs` — headless pill render; `activate` guard; `act_toggle_hidden` guard; kill-by-pid for headless (two Kill sites).
- `CONVENTION.md` — document the `headless` record field (Task 1).
- `AGENTS.md` — architecture note + Known Limitation (Task 6).
- `extensions/corral-pi.ts` — write `headless` from `CORRAL_HEADLESS=1` (Task 5).
- `nix/tests/` — headless-announce scenario (Task 7).

---

### Task 1: Parse `headless` into `RegistryEntry` + document the field

**Files:**
- Modify: `crates/core/src/discovery.rs` (struct field near `hidden` ~line 58; parser near line 127; test module ~line 367)
- Modify: `CONVENTION.md` (record fields table, after the `hidden` row ~line 100)

**Interfaces:**
- Produces: `RegistryEntry.headless: bool` (field), parsed from JSON key `"headless"`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/core/src/discovery.rs`, next to `hidden_field_parses_true_false_and_absent`:

```rust
    #[test]
    fn headless_field_parses_true_false_and_absent() {
        let e = parse_registry_json(r#"{"sessionId":"s1","headless":true}"#).unwrap();
        assert!(e.headless);
        let e = parse_registry_json(r#"{"sessionId":"s2","headless":false}"#).unwrap();
        assert!(!e.headless);
        // Absent defaults to false (every existing record).
        let e = parse_registry_json(r#"{"sessionId":"s3"}"#).unwrap();
        assert!(!e.headless);
        // Non-boolean ignored leniently.
        let e = parse_registry_json(r#"{"sessionId":"s4","headless":"yes"}"#).unwrap();
        assert!(!e.headless);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core headless_field_parses`
Expected: FAIL — `no field 'headless' on type RegistryEntry` (compile error).

- [ ] **Step 3: Add the struct field and parse it**

In `crates/core/src/discovery.rs`, add the field to `RegistryEntry` immediately after the `hidden` field (keep its doc comment):

```rust
    /// Whether this session runs **headless**: alive and announcing over its
    /// socket, but with no window at all (e.g. pi embedded as a library, an
    /// opencode `serve` server). Distinct from `hidden`: a hidden session runs
    /// in a corral-launched cage and is revealed by resume, but a headless
    /// session inherently has no window corral could ever give it. The board
    /// shows it as `headless` and never tries to focus or reveal it. Written by
    /// the adapter (e.g. from `CORRAL_HEADLESS=1`). Absent/false is normal.
    pub headless: bool,
```

In `parse_registry_json`, add after the `hidden:` line:

```rust
        headless: v.get("headless").and_then(|x| x.as_bool()).unwrap_or(false),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p corral-core headless_field_parses`
Expected: PASS.

- [ ] **Step 5: Document the field in CONVENTION.md**

In `CONVENTION.md`, add this row to the record-fields table right after the `hidden` row (~line 100):

```markdown
| `headless`  | boolean         | Optional; default `false`. `true` when the session runs **headless**: alive and announcing over its socket but with **no window at all** (e.g. an agent embedded as a library, or a long-running server mode). Distinct from `hidden` (§2b): a hidden session runs in a consumer-launched headless compositor and is revealed by resume, whereas a headless session inherently has no window the consumer could give it, so a consumer MUST NOT try to focus or reveal it and SHOULD show it as headless. The agent sets this itself when it detects it is running windowless (e.g. from a `CORRAL_HEADLESS=1` environment variable, or no controlling terminal in a server mode). Absent/false is a normal windowed session. |
```

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/discovery.rs CONVENTION.md
git commit -m "core: parse headless record field"
```

---

### Task 2: Stamp `headless` onto the `Agent` (live + dormant)

**Files:**
- Modify: `crates/core/src/model.rs` (`Agent` struct field near `hidden` ~line 107; dormant construction ~line 354; live loop ~line 385; test module)

**Interfaces:**
- Consumes: `RegistryEntry.headless` (Task 1).
- Produces: `Agent.headless: bool`, set on both live and dormant agents by `Board::sync_registry`.

- [ ] **Step 1: Write the failing test**

Find the `sync_registry` tests in `crates/core/src/model.rs` (search for a test that builds a `RegistryEntry` and calls `sync_registry`). Add a focused test. If a helper constructs a `RegistryEntry`, set `headless: true` on it; otherwise inline a minimal record. Example (adapt field names to the existing test helper):

```rust
    #[test]
    fn sync_registry_stamps_headless_onto_dormant() {
        let mut board = Board::default();
        let mut e = RegistryEntry {
            session_id: "h1".into(),
            headless: true,
            resume_command: Some(vec!["pi".into(), "--session".into(), "h1".into()]),
            ..sample_entry() // reuse the module's helper; if none, fill required fields
        };
        e.socket = None; // dormant: clean shutdown cleared the socket
        board.sync_registry(&[e], &HashSet::new());
        let dormant = board.dormant();
        assert_eq!(dormant.len(), 1);
        assert!(dormant[0].headless);
    }
```

If there is no `sample_entry()` helper, build the `RegistryEntry` with all its fields explicitly (mirror the nearest existing `sync_registry` test's construction, adding `headless: true`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral-core sync_registry_stamps_headless`
Expected: FAIL — `no field 'headless' on type Agent` (compile error).

- [ ] **Step 3: Add the field and stamp it**

Add the field to `Agent` in `crates/core/src/model.rs`, immediately after the `hidden` field:

```rust
    /// Whether this session runs headless (no window at all — library
    /// embedding, server mode). Stamped from the record's `headless` on both
    /// live and dormant agents. The board shows a `headless` badge and, unlike
    /// `hidden`, never tries to focus or reveal it (there is no window to give).
    pub headless: bool,
```

In `sync_registry`, dormant construction block (near the `hidden: e.hidden,` line ~354), add:

```rust
                headless: e.headless,
```

In the live-agent loop (near `a.hidden = e.hidden;` ~385), add:

```rust
                    a.headless = e.headless;
```

If `Agent` is built anywhere else in the crate (e.g. `watch.rs` seeding an `Upsert`, or a test helper), add `headless: false` there to satisfy the struct literal. Run `cargo build -p corral-core` to find every construction site.

- [ ] **Step 4: Run test + full core suite**

Run: `cargo test -p corral-core`
Expected: PASS (new test + all existing).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/model.rs crates/core/src/watch.rs
git commit -m "core: stamp headless onto live and dormant agents"
```

---

### Task 3: TUI board — headless badge + no-focus + no-toggle + kill-by-pid

**Files:**
- Modify: `crates/board/src/ui.rs` (`headless_badge`, next to `hidden_badge` ~line 422; render site of `hidden_badge`; test module)
- Modify: `crates/board/src/main.rs` (`activate` ~line 854; `toggle_selected` ~line 836; `dismiss_selected` ~line 928; `commit_move` Kill branch ~line 230)

**Interfaces:**
- Consumes: `Agent.headless` (Task 2).
- Produces: `ui::headless_badge(agent: &Agent) -> &'static str`.

- [ ] **Step 1: Write the failing test**

In the `crates/board/src/ui.rs` test module (where `hidden_badge` is tested), add:

```rust
    #[test]
    fn headless_badge_only_for_live_headless() {
        let mut a = sample_live_agent(); // reuse the module's helper
        a.headless = true;
        assert_eq!(headless_badge(&a), "headless");
        a.headless = false;
        assert_eq!(headless_badge(&a), "");
    }
```

Reuse whatever helper the existing `hidden_badge` test uses to build a live `Agent`; if it builds one inline, copy that and set `headless`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p corral headless_badge`
Expected: FAIL — `cannot find function 'headless_badge'`.

- [ ] **Step 3: Implement `headless_badge` and render it**

In `crates/board/src/ui.rs`, add next to `hidden_badge`:

```rust
/// The marker shown for a live headless agent (no window at all), a plain-text
/// `headless` word rendered as a muted pill, parallel to `hidden_badge`. Unlike
/// hidden, headless is not revealable — the badge just tells the operator why
/// `go` does nothing. Empty for a windowed or dormant agent.
pub fn headless_badge(agent: &Agent) -> &'static str {
    if agent.origin == Origin::Live && agent.headless {
        "headless"
    } else {
        ""
    }
}
```

Find where `hidden_badge` is rendered into the card's meta row (search `hidden_badge(` in `ui.rs`). Beside it, render the headless badge the same way (as a `tag_pill` when non-empty). A live agent is never both hidden and headless, so rendering both guards is safe. Example, adapting to the existing meta-row builder:

```rust
    let hb = headless_badge(agent);
    if !hb.is_empty() {
        spans.push(tag_pill(hb, dim));
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p corral headless_badge`
Expected: PASS.

- [ ] **Step 5: Guard `activate` (go is a no-op for headless)**

In `crates/board/src/main.rs` `fn activate`, add a branch BEFORE the `Origin::Live if agent.hidden` arm:

```rust
        // A headless agent has no window and cannot be revealed (resuming it as
        // a terminal would be a different process). Report why, do not focus.
        Origin::Live if agent.headless => {
            Err("headless session has no window to focus".into())
        }
```

- [ ] **Step 6: Guard `toggle_selected` (h refuses for headless)**

In `fn toggle_selected`, after `if let Some(agent) = board.selectable().get(selected).copied() {`, add at the top of the block:

```rust
        if agent.headless {
            *status = "headless session cannot be hidden or revealed".into();
            return;
        }
```

- [ ] **Step 7: Kill headless by pid (dismiss + card-move)**

In `fn dismiss_selected`, the `Origin::Live` arm currently branches `if agent.hidden`. Change the condition to also cover headless (a headless agent has no window to close):

```rust
            *status = if agent.hidden || agent.headless {
```

In `fn commit_move`, the `MoveAction::Kill` arm, change:

```rust
        MoveAction::Kill => {
            if agent.hidden || agent.headless {
                kill_pid(agent.pid).map_err(|e| format!("close: {e}"))
            } else {
                focuser.close(agent).map_err(|e| format!("close: {e}"))
            }
        }
```

- [ ] **Step 8: Build + test the board**

Run: `cargo test -p corral && cargo build -p corral`
Expected: PASS / builds clean.

- [ ] **Step 9: Commit**

```bash
git add crates/board/src/ui.rs crates/board/src/main.rs
git commit -m "board: render headless badge; no focus/reveal; kill by pid"
```

---

### Task 4: GUI board — headless pill + no-focus + no-toggle + kill-by-pid (parity)

**Files:**
- Modify: `crates/gui/src/dashboard.rs` (meta-row render ~line 1161; `activate` ~line 1474; `act_toggle_hidden` ~line 754; two `MoveAction::Kill`/close sites ~line 300 and ~line 772)

**Interfaces:**
- Consumes: `Agent.headless` (Task 2). Mirrors Task 3 behavior in iced.

- [ ] **Step 1: Render the headless pill**

In `crates/gui/src/dashboard.rs`, find the meta-row block that pushes the hidden pill:

```rust
        if agent.origin == Origin::Live && agent.hidden {
            meta_row = meta_row.push(tag_pill("hidden", s));
        }
```

Add directly after it (parallel, mutually exclusive in practice):

```rust
        // Headless: alive but windowless; the pill tells the operator why `go`
        // does nothing (unlike hidden, it cannot be revealed).
        if agent.origin == Origin::Live && agent.headless {
            meta_row = meta_row.push(tag_pill("headless", s));
        }
```

- [ ] **Step 2: Guard `activate` (go is a no-op for headless)**

In the GUI `fn activate` (`match agent.origin { ... }` ~line 1474), add BEFORE the `Origin::Live if agent.hidden` arm:

```rust
        Origin::Live if agent.headless => {
            Err("headless session has no window to focus".into())
        }
```

- [ ] **Step 3: Guard `act_toggle_hidden`**

In `fn act_toggle_hidden`, resolve the selected agent (mirror how the function already fetches it), and if `agent.headless`, set the status message and return an empty `Task`:

```rust
        // Headless agents have no window to hide or reveal.
        if agent.headless {
            self.status = "headless session cannot be hidden or revealed".into();
            return Task::none();
        }
```

Place this after the agent is obtained but before `apply_placement` is called. Match the function's existing selection/`status` field access exactly.

- [ ] **Step 4: Kill headless by pid (both Kill/close sites)**

At both sites where the GUI branches `let close = if agent.hidden { kill_pid(agent.pid) } else { self.focuser.close(&agent) };` (the card-move Kill ~line 300 and the dismiss path ~line 772), change the condition to:

```rust
                let close = if agent.hidden || agent.headless {
```

- [ ] **Step 5: Build the GUI (needs the devShell for graphics libs)**

Run: `nix develop -c cargo build -p corral-gui`
Expected: builds clean. (Per AGENTS.md, the GUI needs the devShell's `LD_LIBRARY_PATH`.)

- [ ] **Step 6: Commit**

```bash
git add crates/gui/src/dashboard.rs
git commit -m "gui: headless parity — pill, no focus/reveal, kill by pid"
```

---

### Task 5: corral-pi announces `headless` from `CORRAL_HEADLESS=1`

**Files:**
- Modify: `extensions/corral-pi.ts` (record construction near the `hidden` assignment ~line 738-753)

**Interfaces:**
- Produces: the record's `headless` field, set true when `process.env.CORRAL_HEADLESS === "1"`.

**Note:** This is the baseline declaration mechanism (mirroring how `hidden` is read from `CORRAL_HIDDEN`). Harness-specific *auto*-detection of headless mode (no controlling tty, server mode) is deferred; the env is the explicit signal an embedding host or server launch sets. The other adapters (opencode/claude/cursor) adopt the same field where their headless mode is known — tracked as follow-up, not part of this task.

- [ ] **Step 1: Read the current hidden handling**

Read `extensions/corral-pi.ts` around line 735-753 to see how `hidden` is derived and placed on the record.

- [ ] **Step 2: Set `headless` on the record**

Beside the existing `const hidden = process.env.CORRAL_HIDDEN === "1";`, add:

```ts
	// Headless: the embedding host / server launch sets CORRAL_HEADLESS=1 when
	// this pi runs with no window at all (library embedding), so the board shows
	// it as headless and never tries to focus a nonexistent window. Distinct
	// from hidden (a cage-launched window that reveal can bring back).
	const headless = process.env.CORRAL_HEADLESS === "1";
```

Add `headless` to the record object next to `hidden` (in the same literal where `hidden,` appears):

```ts
			hidden,
			headless,
```

- [ ] **Step 3: Typecheck the extension**

Run: `cd extensions && npx tsc --noEmit corral-pi.ts` (or the repo's configured typecheck; check `extensions/` for a `tsconfig`/`package.json`). If no toolchain is wired, verify by inspection that the field mirrors `hidden` exactly.
Expected: no type errors introduced.

- [ ] **Step 4: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: announce headless from CORRAL_HEADLESS env"
```

---

### Task 6: Documentation — AGENTS.md architecture note + Known Limitation

**Files:**
- Modify: `AGENTS.md` (a short "Headless Sessions" note after the "Hidden Agents" section; a Known Limitations bullet; the `discovery.rs`/`model.rs` field mentions in the Crates section)

- [ ] **Step 1: Add a "Headless Sessions" subsection**

In `AGENTS.md`, directly after the "## Hidden Agents" section, add:

```markdown
## Headless Sessions

A session can announce itself **headless**: alive, socket-bound, and driving
`state_update`, but with **no window at all** — e.g. pi embedded as a library or
a long-running server mode. It is distinct from a *hidden* session: hidden runs
in a corral-launched `cage` and is revealed by kill-and-resume into a visible
terminal, but a headless process inherently has no window corral could give it
(resuming it as a terminal would be a different process). The adapter declares
it with `headless: true` in the record (e.g. from a `CORRAL_HEADLESS=1` env it
detects), and the board reads that one field: it renders a `headless` pill (both
shells), makes `go` an informative no-op ("headless session has no window to
focus") instead of a failing focus, refuses `h` (nothing to hide/reveal), and
kills by pid on `d`/card-move (no window to close). `m` (message over the
socket) works unchanged. corral needs no per-harness logic — the party that
knows declares it, correctness-by-construction.
```

- [ ] **Step 2: Add a Known Limitation bullet**

In "## Known Limitations (v1, deliberate)", add:

```markdown
- A headless-announced session (record `headless: true`) that finishes its work
  and exits simply disappears from the board — a windowless library/server run
  is not resumable the way a terminal session is, so there is no dormant card
  unless the adapter also wrote a `resumeCommand`. The marker's job is only to
  make a *live* headless session render correctly and not be focus-clicked into
  an error; it does not keep an exited one around.
```

- [ ] **Step 3: Mention the field in the Crates section**

In the `src/discovery.rs` bullet, where it lists record fields, add a clause that `RegistryEntry` also carries the optional `headless` (adapter-declared windowless marker, distinct from `hidden`). In the `src/model.rs` bullet, note `Agent` carries `headless` stamped by `sync_registry` like `hidden`.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs: describe headless sessions"
```

---

### Task 7: VM E2E scenario — a headless-announced session renders headless

**Files:**
- Modify: `nix/tests/` (the pi scenario driver + the board-assertion helpers; read `nix/tests/` and the design doc `docs/superpowers/specs/2026-07-18-vm-e2e-smoke-test-design.md` first)

**Note:** Per the VM E2E hard rule, this user-facing board change ships with a matching `nix/tests/` assertion. Scope it minimally: launch one pi adapter with `CORRAL_HEADLESS=1` in its environment, assert the board reflects a `headless` card and that `go` does not focus (no window). Reuse the existing pi scenario's announce/reflect assertions as the template.

- [ ] **Step 1: Read the test harness**

Read `nix/tests/` (the Python driver and the flake checks) and the design doc to learn how a scenario launches an adapter, sets its environment, and asserts board state.

- [ ] **Step 2: Add the headless assertion to the pi scenario**

In the pi scenario, add a step that starts (or relaunches) a pi session with `CORRAL_HEADLESS=1` exported into its environment, then asserts:
- the vetted `state/registry/<id>.json` record has `"headless": true`;
- the board reflects the session with the `headless` marker (assert via the board's rendered output / state the driver already inspects for other cards);
- a `go`/focus action on that card does not raise a window (assert the same negative way the suite already checks a no-window case, or assert the status/behavior is the informative no-op).

Follow the exact assertion style the existing pi scenario uses (the driver already reads records and board state).

- [ ] **Step 3: Run the pi e2e check**

Run: `just e2e-one pi` (needs KVM; Linux-only, see AGENTS.md).
Expected: PASS, including the new headless assertion.

- [ ] **Step 4: Commit**

```bash
git add nix/tests
git commit -m "e2e: assert a headless-announced pi session renders headless"
```

---

## Self-Review

**Spec coverage:**
- Adapter declares headless → Task 1 (field/parse), Task 5 (pi writes it). ✓
- Board renders headless → Task 3 (TUI pill), Task 4 (GUI pill). ✓
- No focus / no reveal on headless → Task 3 + Task 4 (`activate` guard, `toggle` guard). ✓
- Kill works without a window → Task 3 + Task 4 (kill-by-pid). ✓
- Distinct from hidden (contract) → Global Constraints + CONVENTION (Task 1) + AGENTS (Task 6). ✓
- Parity (TUI/GUI) → Tasks 3 & 4 paired. ✓
- E2E (hard rule) → Task 7. ✓
- Docs → Task 1 (CONVENTION), Task 6 (AGENTS). ✓

**Type consistency:** `headless: bool` consistent across `RegistryEntry` (Task 1) and `Agent` (Task 2); `ui::headless_badge` name matches its render + test (Task 3); env `CORRAL_HEADLESS` consistent (Task 5, Task 6, Task 7). ✓

**Open follow-ups (out of scope, noted for the record):**
- opencode/claude/cursor adapters adopting `headless` where their headless mode is known.
- Auto-detecting headless (no controlling tty / server mode) instead of the explicit env — deliberately deferred: tty-absence is a leaky proxy (systemd, pipes) that would misfire, so the explicit `CORRAL_HEADLESS` signal is the v1 mechanism.
