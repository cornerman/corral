# Resume-Template (Mechanism 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Kill corrald's constant "approve harness pi" re-prompt by making each adapter's launch commands a *stable template* carrying an explicit `{sessionId}`/`{cwd}` placeholder instead of concrete per-session values, so the approved launch set never flaps.

**Architecture:** Adapters write `spawnCommand`/`resumeCommand` in template form (e.g. `["pi","--session","{sessionId}"]`, `["cursor","{cwd}"]`). corrald stores those templates verbatim as the approved set — comparison becomes plain equality, so the old `normalize()` (concrete id → placeholder) is deleted from the security-matching path. The consumer substitutes the two placeholders with validated values (`denormalize`) at the single moment it launches. The record therefore carries no per-session data, so no single-session shape change can ever mismatch the approved set.

**Tech Stack:** Rust (workspace crates `corral-core`, `corral-daemon`, `corral` board, `corral-gui`); TypeScript adapters (`corral-pi`, `corral-opencode`, `corral-claude`); JavaScript adapter (`corral-cursor`). Tests: `cargo test` (Rust), `node --test` (cursor `lib.js`).

## Global Constraints

- **TUI/GUI parity (hard rule):** every launch-site change lands in BOTH `crates/board` and `crates/gui`. Shared substitution logic lives in `corral-core` so both consume it. Never land in one shell alone.
- **CONVENTION.md is the spec:** the placeholder contract is defined there; adapters and consumer are implemented from it.
- **Reserved placeholders — exactly two:** `{sessionId}` and `{cwd}`. No others (YAGNI). An adapter needing a literal `{sessionId}`/`{cwd}` argv element is unsupported (no real CLI has one).
- **Substitution values are already trusted:** `{sessionId}` → the record's `sessionId` (charset-validated `[A-Za-z0-9._-]`, no leading `-`); `{cwd}` → the record's `cwd` (the physically-derived dir corrald stamps). No new validation needed; substitution feeds `execve` argv elements, never a shell (verified: `crates/core/src/launch.rs:211` uses `Command::new("setsid").args(...)`, no `sh -c`).
- **No `normalize()` left:** the concrete-id → placeholder transform is deleted, not preserved. `candidate()` takes commands verbatim.
- **Commits:** small, single-line messages, no co-author trailer. TDD: failing test first.
- Run `cargo test` and `just lint` before each commit touching Rust.

## File Structure

- `crates/core/src/approved_commands.rs` — delete `normalize`; `candidate` clones commands verbatim; keep `denormalize`, `SESSION_PLACEHOLDER`, `CWD_PLACEHOLDER`, `registered` (already equality), `mode_approved`.
- `crates/core/src/model.rs` — add `Agent::resume_argv()` / `Agent::spawn_argv()` helpers that denormalize; these are the single choke point every board/gui launch site calls.
- `crates/core/src/discovery.rs` — add `RegistryEntry::resume_argv()` / `RegistryEntry::spawn_argv()` helpers (daemon/router side).
- `crates/core/src/placement.rs` — resume via `agent.resume_argv()`.
- `crates/daemon/src/router.rs` — resume via `entry.resume_argv()`, dir-spawn via `spawn_argv`.
- `crates/board/src/main.rs`, `crates/gui/src/dashboard.rs` — resume/spawn via the model helpers.
- `extensions/corral-pi.ts` — template `resumeCommand` always; unlink record for a never-persisted session on shutdown.
- `extensions/corral-opencode.ts`, `extensions/corral-claude/sidecar.ts` — template `resumeCommand`.
- `extensions/corral-cursor/lib.js` (+ `lib.test.js`) — template `spawnCommand`/`resumeCommand` with `{cwd}`.
- `CONVENTION.md`, `AGENTS.md` — document the placeholder contract.

---

### Task 1: Delete `normalize`, make `candidate` verbatim

**Files:**
- Modify: `crates/core/src/approved_commands.rs`

**Interfaces:**
- Produces: `candidate(record) -> Template` returns `Template { spawn: record.spawn_command.clone(), resume: record.resume_command.clone(), gui, message_flag }` (no normalization). `registered` and `mode_approved` unchanged in signature. `denormalize`, `SESSION_PLACEHOLDER`, `CWD_PLACEHOLDER` remain public (used by Tasks 2–3).

- [ ] **Step 1: Rewrite the two candidate/normalize tests to the template model**

Replace `normalize_substitutes_own_session_and_cwd` and the `normalize` usage in `denormalize_round_trips` with tests that assert `candidate` copies commands verbatim and that two sessions carrying the *same template* produce equal candidates:

```rust
#[test]
fn candidate_copies_commands_verbatim() {
    let mut r = rec(Some("pi"), "s1", None);
    r.spawn_command = Some(vec!["pi".into()]);
    r.resume_command = Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]);
    let c = candidate(&r);
    assert_eq!(c.spawn, Some(vec!["pi".into()]));
    assert_eq!(
        c.resume,
        Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()])
    );
    // A different session carrying the SAME template yields the SAME candidate.
    let mut r2 = rec(Some("pi"), "s2", None);
    r2.spawn_command = r.spawn_command.clone();
    r2.resume_command = r.resume_command.clone();
    assert_eq!(candidate(&r2), c);
}

#[test]
fn denormalize_substitutes_placeholders() {
    let tmpl = vec!["pi".into(), "--session".into(), SESSION_PLACEHOLDER.into()];
    assert_eq!(
        denormalize(&tmpl, "abc-123", None),
        vec!["pi", "--session", "abc-123"]
    );
    let tmpl2 = vec!["cursor".into(), CWD_PLACEHOLDER.into()];
    assert_eq!(denormalize(&tmpl2, "x", Some("/w")), vec!["cursor", "/w"]);
}
```

- [ ] **Step 2: Run the tests to verify they fail to compile (normalize still referenced)**

Run: `cargo test -p corral-core approved_commands 2>&1 | tail -20`
Expected: compile error or FAIL — old `normalize` test and helper still present.

- [ ] **Step 3: Delete `normalize`, simplify `candidate`**

Remove the entire `pub fn normalize(...)` function and its doc comment. Change `candidate`:

```rust
pub fn candidate(record: &RegistryEntry) -> Template {
    Template {
        spawn: record.spawn_command.clone(),
        resume: record.resume_command.clone(),
        gui: record.gui,
        message_flag: record.message_flag.clone(),
    }
}
```

Delete the now-removed `normalize_substitutes_own_session_and_cwd` test and the old `denormalize_round_trips` test (replaced in Step 1). Update `registered_only_when_label_and_commands_match` and other tests that build records with concrete ids so their `resume_command` uses `{sessionId}` (template form), since that is now what a real record carries. Example:

```rust
r.resume_command = Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]);
r2.resume_command = Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]);
assert!(registered(&r2, &approved)); // same template -> registered
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p corral-core approved_commands 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/approved_commands.rs
git commit -m "core: approved set stores launch templates verbatim, drop normalize"
```

---

### Task 2: Add denormalizing launch-argv helpers on `Agent` and `RegistryEntry`

**Files:**
- Modify: `crates/core/src/model.rs` (impl `Agent`)
- Modify: `crates/core/src/discovery.rs` (impl `RegistryEntry`)

**Interfaces:**
- Consumes: `approved_commands::denormalize`.
- Produces:
  - `Agent::resume_argv(&self) -> Option<Vec<String>>` — `resume_command` with `{sessionId}`/`{cwd}` substituted from `self.session_id`/`self.cwd`.
  - `Agent::spawn_argv(&self) -> Option<Vec<String>>` — same for `spawn_command`.
  - `RegistryEntry::resume_argv(&self) -> Option<Vec<String>>` and `RegistryEntry::spawn_argv(&self)` — same, using `self.session_id` (a `String`) and `self.cwd`.
- Every launch site (Tasks 3–4 boundary) calls these instead of touching `resume_command`/`spawn_command` directly, so no site can forget to substitute.

- [ ] **Step 1: Write failing tests**

In `crates/core/src/model.rs` tests:

```rust
#[test]
fn resume_argv_substitutes_session_and_cwd() {
    let mut a = /* build a dormant Agent */ ;
    a.session_id = Some("sess-1".into());
    a.cwd = Some("/w".into());
    a.resume_command = Some(vec!["pi".into(), "--session".into(), "{sessionId}".into()]);
    assert_eq!(
        a.resume_argv().unwrap(),
        vec!["pi", "--session", "sess-1"]
    );
    a.resume_command = None;
    assert_eq!(a.resume_argv(), None);
}
```

In `crates/core/src/discovery.rs` tests:

```rust
#[test]
fn registry_entry_resume_argv_substitutes() {
    let mut e = /* build RegistryEntry with session_id "s9", cwd "/p" */ ;
    e.resume_command = Some(vec!["cursor".into(), "{cwd}".into()]);
    assert_eq!(e.resume_argv().unwrap(), vec!["cursor", "/p"]);
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p corral-core resume_argv 2>&1 | tail -15`
Expected: FAIL — method not found.

- [ ] **Step 3: Implement the helpers**

In `crates/core/src/model.rs`, inside `impl Agent`:

```rust
/// The resume argv to run, with `{sessionId}`/`{cwd}` substituted from this
/// agent's identity. `None` when the agent carries no resume command.
pub fn resume_argv(&self) -> Option<Vec<String>> {
    let sid = self.session_id.as_deref().unwrap_or("");
    self.resume_command
        .as_ref()
        .map(|c| crate::approved_commands::denormalize(c, sid, self.cwd.as_deref()))
}

/// The spawn argv to run, with `{cwd}` substituted (a fresh spawn has no
/// `{sessionId}`). `None` when the agent carries no spawn command.
pub fn spawn_argv(&self) -> Option<Vec<String>> {
    let sid = self.session_id.as_deref().unwrap_or("");
    self.spawn_command
        .as_ref()
        .map(|c| crate::approved_commands::denormalize(c, sid, self.cwd.as_deref()))
}
```

In `crates/core/src/discovery.rs`, inside `impl RegistryEntry`:

```rust
/// Resume argv with `{sessionId}`/`{cwd}` substituted (see `Agent::resume_argv`).
pub fn resume_argv(&self) -> Option<Vec<String>> {
    self.resume_command
        .as_ref()
        .map(|c| crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref()))
}

/// Spawn argv with `{cwd}` substituted.
pub fn spawn_argv(&self) -> Option<Vec<String>> {
    self.spawn_command
        .as_ref()
        .map(|c| crate::approved_commands::denormalize(c, &self.session_id, self.cwd.as_deref()))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p corral-core 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/model.rs crates/core/src/discovery.rs
git commit -m "core: resume_argv/spawn_argv helpers substitute launch placeholders"
```

---

### Task 3: Route every launch site through the denormalizing helpers

**Files:**
- Modify: `crates/core/src/placement.rs:50` (apply_placement resume)
- Modify: `crates/daemon/src/router.rs:329` (session resume delivery), plus the dir-spawn site
- Modify: `crates/board/src/main.rs` (resume ~`:848,:862`, spawn Shift+Enter site)
- Modify: `crates/gui/src/dashboard.rs` (resume ~`:1440,:1464`, spawn site)

**Interfaces:**
- Consumes: `Agent::resume_argv/spawn_argv`, `RegistryEntry::resume_argv/spawn_argv` from Task 2.
- No new public API.

- [ ] **Step 1: placement.rs — resume via helper**

Replace the `agent.resume_command.as_deref().ok_or(...)` block with:

```rust
let command = agent
    .resume_argv()
    .ok_or("placement: agent has no resume command")?;
```

and pass `&command` to `launcher.launch(...)`. Update the placement test `missing_resume_command_is_an_error` (it sets `resume_command = None`, still valid) and any test asserting the launched argv to expect the substituted form (the stub launcher records argv; assert `["pi","--session","<sid>"]`).

- [ ] **Step 2: router.rs — resume + dir-spawn via helper**

In the session-resume match, replace `(Some(cwd), Some(command))` destructuring so `command` comes from `entry.resume_argv()`:

```rust
match (&entry.cwd, entry.resume_argv()) {
    (Some(cwd), Some(command)) => {
        let mut mode = entry.launch_mode();
        mode.hidden = msg.hidden;
        match launcher.launch(Path::new(cwd), &command, Some(&msg.tagged()), &mode) { ... }
    }
    _ => format!("route: session {session_id} not resumable"),
}
```

For dir-spawn, substitute via `spawn_argv` on the chosen entry (find the entry, call `entry.spawn_argv()`), so a cursor `{cwd}` spawn resolves. Update router tests that assert the launched argv to the substituted form.

- [ ] **Step 3: board/main.rs — resume + spawn via helper**

At each site that reads `agent.resume_command` to launch (the `Origin::Dormant` resume match near `:848` and `:862`, and the `m`-to-dormant path), switch to `agent.resume_argv()`. At the Shift+Enter spawn site, use `agent.spawn_argv()`. Keep the `(cwd, command)` guard shape.

- [ ] **Step 4: gui/dashboard.rs — resume + spawn via helper (parity)**

Mirror Step 3 in the GUI: resume sites near `:1440`/`:1464` use `agent.resume_argv()`, the spawn site uses `agent.spawn_argv()`. This is the hard-rule parity change — do not skip.

- [ ] **Step 5: Run the full suite + lint**

Run: `cargo test 2>&1 | tail -20 && just lint 2>&1 | tail -5`
Expected: PASS, no clippy errors. (GUI tests need the devShell `LD_LIBRARY_PATH`; run under `nix develop` if they fail to link.)

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/placement.rs crates/daemon/src/router.rs crates/board/src/main.rs crates/gui/src/dashboard.rs
git commit -m "launch: substitute {sessionId}/{cwd} at every resume/spawn site"
```

---

### Task 4: corral-pi — always write the resume template; unlink never-persisted records

**Files:**
- Modify: `extensions/corral-pi.ts:650,668-669` (writeRegistry) and the `stop()`/shutdown path

**Interfaces:**
- Produces: a pi record whose `resumeCommand` is always `["pi","--session","{sessionId}"]` (stable), and whose record is removed (not left dormant) when the session was never persisted.

- [ ] **Step 1: Write the resume template unconditionally**

Replace lines 650 and 669:

```ts
// resumeCommand is a stable TEMPLATE: corral substitutes {sessionId} at launch.
// Always present so the approved launch set never flaps (see Mechanism 3).
resumeCommand: ["pi", "--session", "{sessionId}"],
```

Keep computing `resumable` from `fs.existsSync(sessionFile)` but repurpose it (Step 2), not to null the command.

- [ ] **Step 2: Track persistence; unlink the record for a never-persisted session on stop**

Add a module-level `let everPersisted = false;`. In `writeRegistry`, set `everPersisted ||= resumable;`. In `stop()` (which runs without `ctx`), when `!everPersisted` and `recordFile` is known, `fs.rmSync(recordFile, { force: true })` and remove the pointer, instead of `clearSocketInRegistry()`. A never-persisted session then leaves no dormant card that would resume to "No session found" (the concern the old null-gate solved), while a persisted session still clears its socket and stays resumable.

- [ ] **Step 3: Manual verification note (no unit harness for the .ts)**

Document in the task PR: start pi, confirm `<cwd>/.corral/registry/<id>.json` shows `"resumeCommand": ["pi","--session","{sessionId}"]` from the first write (before any turn). Take a turn, confirm it is unchanged. This is validated live in the Validation task of TODO.md.

- [ ] **Step 4: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: write stable {sessionId} resume template; drop empty dormant record"
```

---

### Task 5: corral-opencode — resume template

**Files:**
- Modify: `extensions/corral-opencode.ts:432`

- [ ] **Step 1: Template the resume command**

Replace `resumeCommand: ["opencode", "--session", activeSessionId]` with:

```ts
resumeCommand: ["opencode", "--session", "{sessionId}"],
```

opencode auto-persists, so no unlink handling is needed (its records are always resumable). Leave the surrounding `resumeCommand is always set once the id is` comment updated to say the id is now a `{sessionId}` placeholder.

- [ ] **Step 2: Commit**

```bash
git add extensions/corral-opencode.ts
git commit -m "corral-opencode: write {sessionId} resume template"
```

---

### Task 6: corral-claude — resume template

**Files:**
- Modify: `extensions/corral-claude/sidecar.ts:385`

- [ ] **Step 1: Template the resume command**

Replace `resumeCommand: ["claude", "--resume", sessionId]` with:

```ts
resumeCommand: ["claude", "--resume", "{sessionId}"],
```

Update the adjacent comment (`resumeCommand resumes this exact session`) to note corral substitutes `{sessionId}` at launch. Claude sessions persist on start, so no unlink handling.

- [ ] **Step 2: Commit**

```bash
git add extensions/corral-claude/sidecar.ts
git commit -m "corral-claude: write {sessionId} resume template"
```

---

### Task 7: corral-cursor — `{cwd}` template for spawn and resume

**Files:**
- Modify: `extensions/corral-cursor/lib.js:41-42`
- Modify: `extensions/corral-cursor/lib.test.js` (the record-shape test)

**Interfaces:**
- Produces: a cursor record with `spawnCommand: ["cursor","{cwd}"]`, `resumeCommand: ["cursor","{cwd}"]`, so a cursor kind is approved once for all directories (no per-cwd re-prompt).

- [ ] **Step 1: Update the record-shape test to expect `{cwd}`**

In `lib.test.js`, change the assertions on the built record to expect the placeholder:

```js
assert.deepEqual(record.spawnCommand, ["cursor", "{cwd}"]);
assert.deepEqual(record.resumeCommand, ["cursor", "{cwd}"]);
```

- [ ] **Step 2: Run to verify fail**

Run: `node --test extensions/corral-cursor/ 2>&1 | tail -15`
Expected: FAIL — record still carries the concrete cwd.

- [ ] **Step 3: Template the commands**

In `lib.js`, replace the concrete `cwd` with the placeholder:

```js
spawnCommand: ["cursor", "{cwd}"],
resumeCommand: ["cursor", "{cwd}"],
```

Update the adjacent comment to note corral substitutes `{cwd}` (the trusted dir) at launch.

- [ ] **Step 4: Run to verify pass**

Run: `node --test extensions/corral-cursor/ 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js
git commit -m "corral-cursor: write {cwd} launch template (one approval per kind, not per dir)"
```

---

### Task 8: Document the placeholder contract (CONVENTION.md + AGENTS.md)

**Files:**
- Modify: `CONVENTION.md` (fields table lines ~95–102, examples ~121–134, launch §2a ~142–148)
- Modify: `AGENTS.md` (the `approved_commands.rs` / `launch.rs` descriptions)

- [ ] **Step 1: CONVENTION.md — define the placeholders and update the "verbatim" claim**

Add to §2, near the `spawnCommand`/`resumeCommand` rows, a placeholder contract paragraph:

> `spawnCommand`/`resumeCommand` are argv **templates**. Two reserved tokens are substituted by the consumer at launch: `{sessionId}` → the record's `sessionId`, `{cwd}` → the record's working directory. An argv element equal to a token is replaced whole; every other element is run verbatim. Templates carry no per-session data, so a kind's approved launch set is stable across sessions and directories. There are exactly two tokens; a literal `{sessionId}`/`{cwd}` argv element is unsupported.

Change line 96 (`resumeCommand`) to show `["pi","--session","{sessionId}"]`, line 102 ("runs verbatim and never parses") to "runs verbatim except for substituting the two reserved placeholders (§ above)", line 122 example to the `{sessionId}` template, and the quine example (133–134) to `{sessionId}`. Update §2a to note the message is appended after placeholder substitution.

- [ ] **Step 2: AGENTS.md — update the crate descriptions**

In the `src/approved_commands.rs` bullet, replace the `normalize` mention: the store now holds templates verbatim, `registered` is plain equality, and `denormalize` substitutes `{sessionId}`/`{cwd}` at launch. In the `src/launch.rs` bullet, note that resume/spawn argvs are denormalized (via `Agent::resume_argv`/`spawn_argv`) before running.

- [ ] **Step 3: Commit**

```bash
git add CONVENTION.md AGENTS.md
git commit -m "docs: specify {sessionId}/{cwd} launch-command placeholder contract"
```

---

### Task 9: Full verification + stale approved-commands note

**Files:**
- None (verification), plus a one-line operator note.

- [ ] **Step 1: Full workspace test + lint**

Run: `cargo test 2>&1 | tail -20 && just lint 2>&1 | tail -5`
Expected: all green.

- [ ] **Step 2: Grep for leftover concrete-id assumptions**

Run: `grep -rn "normalize\b" crates/ ; grep -rn '"--session", *[a-z_]*id' crates/ extensions/`
Expected: no `normalize` outside `denormalize`; no adapter writing a concrete id into a resume command.

- [ ] **Step 3: Note the one-time operator migration**

The existing `~/.corral/state/approved-commands.json` holds the old spawn-only pi entry. After deploy, corrald will prompt once to approve the new template shape (`{ "pi": { "spawn": ["pi"], "resume": ["pi","--session","{sessionId}"] } }`); approve it once and the re-prompt stops permanently. No code deletes the old store (corrald does not run migrations). Record this in the PR description; the redeploy of `corrald` is deployment glue owned by `~/nixos`.

- [ ] **Step 4: Final commit if any doc/whitespace fixups remain**

```bash
git commit -am "resume-template: final cleanup" || true
```

---

## Self-Review

**Spec coverage:** re-prompt root cause (flapping resume) fixed by stable templates (Tasks 1,4–7); `normalize` deleted (Task 1); substitution at launch (Tasks 2–3); parity in both shells (Task 3 Steps 3–4); all four adapters (Tasks 4–7); convention + architecture docs (Task 8); verification + operator migration (Task 9). The never-persisted dormant regression the old null-gate prevented is handled in Task 4 Step 2.

**Placeholder scan:** no TBD/TODO; every code step shows the code.

**Type consistency:** `resume_argv`/`spawn_argv` defined in Task 2 return `Option<Vec<String>>` and are consumed with that type in Task 3; `candidate` returns `Template` with fields `spawn/resume/gui/message_flag` consistent with `approved_commands.rs`.

**Open risk to watch during execution:** confirm `corral-pi`'s `stop()` can reach `recordFile` and the pointer path without `ctx` (Task 4 Step 2 relies on the already-captured `recordFile` module var and the crash-safe `clearSocketInRegistry` pattern). If not, fall back to accepting the never-persisted dormant edge (a rare "No session found" on resuming a session that never took a turn) and drop the unlink, keeping only the template change.
