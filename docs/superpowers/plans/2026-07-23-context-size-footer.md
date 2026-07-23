# Context Size Footer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show entries count / age / context-window percent next to the model in the selected card's footer, for pi sessions only, in both the TUI and GUI.

**Architecture:** pi's extension broadcasts a new `context_update` session/update (entries count, context percent, a pre-formatted age string) at the same points it already broadcasts state, seeded on connect and persisted to the registry record like the existing `model` field. `crates/core` gains matching `RegistryEntry`/`Agent` fields, a `Board::apply` handler, and a single shared `Agent::footer_line()` formatter both shells call, so TUI/GUI parity is structural rather than duplicated.

**Tech Stack:** Rust (`crates/core`, `crates/board`, `crates/gui`), TypeScript (`extensions/corral-pi.ts`), Python (`nix/tests/`, VM e2e).

## Global Constraints

- TUI/GUI parity is a hard rule (`AGENTS.md`): any user-facing behavior ships in both `crates/board` and `crates/gui`.
- Any change to an adapter (here, `corral-pi.ts`) MUST update the matching `nix/tests/scenarios/pi.py` e2e scenario in the same change (`AGENTS.md` hard rule).
- No new dependencies (no chrono/time crate in Rust; age is a pre-formatted string produced by the TypeScript side).
- v1 ships for pi only; other adapters are unaffected and keep showing exactly today's `model: <x>`.
- Never fabricate values: when pi's own `getContextUsage()` returns an unknown percent, omit the percent segment rather than showing a placeholder.

---

### Task 1: `RegistryEntry` gains the three persisted fields

**Files:**
- Modify: `crates/core/src/discovery.rs`
- Modify (mechanical field addition only, see Step 3): `crates/core/src/curation.rs`, `crates/core/src/approved_commands.rs`, `crates/core/src/model.rs`, `crates/daemon/src/router.rs`, `crates/daemon/src/mailbox.rs`, `crates/daemon/tests/security.rs`

**Interfaces:**
- Produces: `RegistryEntry.entries: Option<u64>`, `RegistryEntry.context_percent: Option<u32>`, `RegistryEntry.context_age: Option<String>`, parsed from JSON keys `"entries"` (number), `"contextPercent"` (number), `"contextAge"` (string).

- [ ] **Step 1: Add the fields and write the parsing tests (failing first)**

In `crates/core/src/discovery.rs`, add to the `RegistryEntry` struct, right after the existing `model` field:

```rust
    /// The LLM model this session runs, as `"<provider>/<id>"` (e.g.
    /// `anthropic/claude-opus-4"`). Written by the adapter so a selected
    /// dormant card shows its last-known model; live cards refresh it over the
    /// socket (a `config_options_update` broadcast). Verbatim adapter string,
    /// shown as-is (corral never prettifies). Absent for a producer that does
    /// not report a model.
    pub model: Option<String>,
    /// Count of session-log entries (messages, tool calls, custom entries) —
    /// an honest size proxy for "how big this transcript is". Written by an
    /// adapter that can introspect its own transcript (pi only today); `None`
    /// for a producer that does not report it, which also gates the whole
    /// entries/percent/age footer group off (see `Agent::footer_line`).
    pub entries: Option<u64>,
    /// This session's context usage as a percentage of its model's context
    /// window (pi's own `ctx.getContextUsage()`), 0-100. `None` when the
    /// adapter's own estimate is unknown (e.g. right after compaction) or the
    /// adapter does not report it at all.
    pub context_percent: Option<u32>,
    /// A pre-formatted age string (e.g. `"3d"`, `"42m"`) for how long this
    /// session's transcript has existed, computed adapter-side from the
    /// session's own creation timestamp (durable across a resume). Kept as an
    /// opaque string rather than a raw timestamp: no ISO-8601 parsing
    /// dependency needed in Rust, matching how `model` is also carried as an
    /// opaque adapter string.
    pub context_age: Option<String>,
```

Add unit tests at the bottom of the `#[cfg(test)] mod tests` block in the same file, right after `model_field_parses_and_defaults_none`:

```rust
    #[test]
    fn context_fields_parse_and_default_none() {
        let json = r#"{"sessionId":"s1","entries":42,"contextPercent":12,"contextAge":"3d"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, Some(42));
        assert_eq!(e.context_percent, Some(12));
        assert_eq!(e.context_age.as_deref(), Some("3d"));
        // Absent -> None (older/unknown producer, or an adapter that never reports it).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.entries, None);
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age, None);
        // contextPercent can be legitimately absent (unknown estimate) even when
        // entries/contextAge are present.
        let json = r#"{"sessionId":"s3","entries":7,"contextAge":"5m"}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, Some(7));
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age.as_deref(), Some("5m"));
        // Non-numeric entries/contextPercent or non-string contextAge -> None,
        // never a garbled value.
        let json = r#"{"sessionId":"s4","entries":"lots","contextPercent":"high","contextAge":9}"#;
        let e = parse_registry_json(json).unwrap();
        assert_eq!(e.entries, None);
        assert_eq!(e.context_percent, None);
        assert_eq!(e.context_age, None);
    }
```

- [ ] **Step 2: Run the new test to verify it fails**

Run: `cargo test -p corral-core context_fields_parse_and_default_none`
Expected: FAIL to compile (`RegistryEntry` has no field `entries`) or FAIL the assertions.

- [ ] **Step 3: Parse the fields in `parse_registry_json`, then fix every other construction site**

In `crates/core/src/discovery.rs`, inside `parse_registry_json`, add right after `model: str_field("model"),`:

```rust
        model: str_field("model"),
        entries: v.get("entries").and_then(|x| x.as_u64()),
        context_percent: v
            .get("contextPercent")
            .and_then(|x| x.as_u64())
            .map(|n| n as u32),
        context_age: str_field("contextAge"),
```

Now fix every other place that builds a `RegistryEntry` struct literal (the compiler will list them — `cargo build --workspace --tests` after Step 1's struct-field addition will fail with "missing fields" at each). Every existing literal ends its field list with a `model: None,` (or, in `discovery.rs`'s own tests, similar) line; add these three lines immediately after every such `model: None,` line, in these files:

- `crates/core/src/curation.rs` (the `rec` test helper)
- `crates/core/src/approved_commands.rs` (the `rec` test helper)
- `crates/core/src/model.rs` (multiple test helpers/literals — every `RegistryEntry { ... model: None, }` in that file)
- `crates/daemon/src/router.rs` (`live_record`, `labeled_record`, `dir_record`, `dormant`, and the inline literal in a test near line 748)
- `crates/daemon/src/mailbox.rs` (the `rec` test helper)
- `crates/daemon/tests/security.rs` (the `mk_rec` test helper)

The three lines to insert after each `model: None,`:

```rust
            entries: None,
            context_percent: None,
            context_age: None,
```

(Match the existing indentation of the surrounding fields at each site.)

- [ ] **Step 4: Build and run the full test suite to verify everything compiles and passes**

Run: `cargo build --workspace --tests && cargo test --workspace`
Expected: PASS. If the compiler reports any further "missing field" errors (a construction site this plan's grep missed), add the same three `None` lines there too, indentation-matched, and re-run.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/discovery.rs crates/core/src/curation.rs crates/core/src/approved_commands.rs crates/core/src/model.rs crates/daemon/src/router.rs crates/daemon/src/mailbox.rs crates/daemon/tests/security.rs
git commit -m "core: add entries/context_percent/context_age to RegistryEntry"
```

---

### Task 2: `Agent` gains the fields, `Update::SetContext`, `sync_registry` stamping, and `footer_line()`

**Files:**
- Modify: `crates/core/src/model.rs`
- Modify (mechanical field addition only): `crates/core/src/placement.rs`, `crates/core/src/focus.rs`, `crates/core/src/picker.rs`, `crates/board/src/ui.rs` (test fixtures only in this task — the rendering wiring is Task 4)

**Interfaces:**
- Consumes: `RegistryEntry.entries`/`context_percent`/`context_age` (Task 1).
- Produces: `Agent.entries: Option<u64>`, `Agent.context_percent: Option<u32>`, `Agent.context_age: Option<String>`; `Update::SetContext(PathBuf, ContextInfo)` where `ContextInfo { entries: u64, percent: Option<u32>, age: String }`; `Agent::footer_line(&self) -> Option<String>`.

- [ ] **Step 1: Add the `Agent` fields**

In `crates/core/src/model.rs`, add to the `Agent` struct right after the existing `model` field:

```rust
    /// The LLM model this agent runs, `"<provider>/<id>"` (e.g.
    /// `anthropic/claude-opus-4`). Live agents refresh it from a
    /// `config_options_update` broadcast; `sync_registry` also stamps the
    /// record's last-known model onto both live and dormant agents. Shown
    /// verbatim in the footer for the selected card. `None` until reported.
    pub model: Option<String>,
    /// Count of session-log entries (pi only today). `None` gates the whole
    /// entries/percent/age footer group off — see `footer_line`.
    pub entries: Option<u64>,
    /// Context usage as a percentage of the model's context window (pi's own
    /// estimate), 0-100. `None` when unknown (e.g. right after compaction) or
    /// unreported.
    pub context_percent: Option<u32>,
    /// A pre-formatted age string (e.g. `"3d"`) for how long this session's
    /// transcript has existed. `None` when unreported.
    pub context_age: Option<String>,
```

- [ ] **Step 2: Write the failing tests for `Update::SetContext`, `sync_registry` stamping, and `footer_line`**

Add to the `#[cfg(test)] mod tests` block in `crates/core/src/model.rs`, right after `dormant_agent_inherits_model_from_record`:

```rust
    #[test]
    fn set_context_updates_live_agent() {
        let mut b = Board::default();
        b.apply(Update::Upsert(Box::new(agent("/s/pi-1.sock", State::Idle))));
        b.apply(Update::SetContext(
            PathBuf::from("/s/pi-1.sock"),
            ContextInfo { entries: 42, percent: Some(12), age: "3d".into() },
        ));
        let a = &b.in_state(State::Idle)[0];
        assert_eq!(a.entries, Some(42));
        assert_eq!(a.context_percent, Some(12));
        assert_eq!(a.context_age.as_deref(), Some("3d"));
    }

    #[test]
    fn dormant_agent_inherits_context_from_record() {
        let mut b = Board::default();
        let mut rec = dormant_record("q1", "/tmp/q", "2026-06-01T00:00:00Z");
        rec.entries = Some(7);
        rec.context_percent = Some(4);
        rec.context_age = Some("1h".into());
        b.sync_registry(&[rec], &HashSet::new());
        let d = &b.dormant()[0];
        assert_eq!(d.entries, Some(7));
        assert_eq!(d.context_percent, Some(4));
        assert_eq!(d.context_age.as_deref(), Some("1h"));
    }

    #[test]
    fn live_agent_context_not_overwritten_by_staler_record() {
        // Mirrors the model field's precedent: a live broadcast wins over a
        // record only stamped when the live agent has no value yet.
        let mut b = Board::default();
        b.apply(Update::Upsert(Box::new(agent("sess-1", State::Running))));
        b.apply(Update::SetContext(
            PathBuf::from("sess-1"),
            ContextInfo { entries: 99, percent: Some(50), age: "9d".into() },
        ));
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
            hidden: false,
            description: None,
            model: None,
            entries: Some(1),
            context_percent: Some(1),
            context_age: Some("1s".into()),
        };
        b.sync_registry(&[rec], &HashSet::new());
        let a = &b.in_state(State::Running)[0];
        assert_eq!(a.entries, Some(99), "live value must win over the stale record");
    }

    #[test]
    fn footer_line_formats_full_and_partial_and_none() {
        let mut a = agent("x", State::Idle);
        // Nothing known at all.
        assert_eq!(a.footer_line(), None);
        // Model only (every non-pi adapter, or pi before its first broadcast).
        a.model = Some("anthropic/claude-opus-4".into());
        assert_eq!(a.footer_line().as_deref(), Some("model: anthropic/claude-opus-4"));
        // Full pi group.
        a.entries = Some(42);
        a.context_percent = Some(12);
        a.context_age = Some("3d".into());
        assert_eq!(
            a.footer_line().as_deref(),
            Some("12% ctx · 42 entries · 3d · model: anthropic/claude-opus-4")
        );
        // Percent unknown (e.g. right after compaction): omit just that segment.
        a.context_percent = None;
        assert_eq!(
            a.footer_line().as_deref(),
            Some("42 entries · 3d · model: anthropic/claude-opus-4")
        );
        // entries known but no model reported: no trailing " · model: ..".
        a.model = None;
        assert_eq!(a.footer_line().as_deref(), Some("42 entries · 3d"));
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p corral-core set_context_updates_live_agent dormant_agent_inherits_context_from_record live_agent_context_not_overwritten_by_staler_record footer_line_formats_full_and_partial_and_none`
Expected: FAIL to compile (no `ContextInfo`, no `Update::SetContext`, no `footer_line`).

- [ ] **Step 4: Implement `ContextInfo`, `Update::SetContext`, `Board::apply` handling, `sync_registry` stamping, and `footer_line`**

In `crates/core/src/model.rs`, add near the top (after the `State`/`Origin`/`Column` definitions, before `Agent`):

```rust
/// One `context_update` broadcast's payload (see `watch::parse_config_context`):
/// entries count, context-window percent (`None` when the adapter's own
/// estimate is unknown), and a pre-formatted age string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInfo {
    pub entries: u64,
    pub percent: Option<u32>,
    pub age: String,
}
```

In the `Update` enum, add right after `SetModel`:

```rust
    /// The current model (from a `config_options_update` broadcast): a
    /// `"provider/id"` string shown in the footer for the selected card.
    SetModel(PathBuf, String),
    /// Context size/age (from a `context_update` broadcast, pi only): shown
    /// alongside the model in the footer for the selected card.
    SetContext(PathBuf, ContextInfo),
```

In `Board::apply`, add a new match arm right after the `Update::SetModel` arm:

```rust
            Update::SetModel(path, model) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.model = Some(model);
                }
            }
            Update::SetContext(path, info) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.entries = Some(info.entries);
                    a.context_percent = info.percent;
                    a.context_age = Some(info.age);
                }
            }
```

In `sync_registry`, the dormant-agent-construction closure currently has `model: e.model.clone(),`; change that one line to also carry the three new fields:

```rust
                model: e.model.clone(),
                entries: e.entries,
                context_percent: e.context_percent,
                context_age: e.context_age.clone(),
```

Still in `sync_registry`, the live-agent-stamping loop currently has:

```rust
                    if a.model.is_none() {
                        a.model = e.model.clone();
                    }
```

Change it to also stamp the context fields the same way (only when the live agent has no value yet, so a fresher live broadcast is never overwritten):

```rust
                    if a.model.is_none() {
                        a.model = e.model.clone();
                    }
                    if a.entries.is_none() {
                        a.entries = e.entries;
                        a.context_percent = e.context_percent;
                        a.context_age = e.context_age.clone();
                    }
```

Add `footer_line` as a method on `impl Agent` (place it right after `matches_query`):

```rust
    /// The footer line for the selected card: context size/age (pi only, when
    /// reported) followed by the model, display-only. The entries/percent/age
    /// group is gated as a whole on `entries` being known (they are broadcast
    /// together); within it, percent is separately omitted when unknown (e.g.
    /// right after compaction). `None` when neither group has anything to show.
    /// Shared by both shells so TUI/GUI parity is structural, not duplicated.
    pub fn footer_line(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(entries) = self.entries {
            if let Some(pct) = self.context_percent {
                parts.push(format!("{pct}% ctx"));
            }
            parts.push(format!("{entries} entries"));
            if let Some(age) = &self.context_age {
                parts.push(age.clone());
            }
        }
        if let Some(m) = &self.model {
            parts.push(format!("model: {m}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
```

- [ ] **Step 5: Fix every other `Agent` struct literal**

`cargo build --workspace --tests` will now fail with "missing fields" wherever an `Agent { .. }` literal exists without the three new fields. Every existing literal has a `model: None,` line (test fixtures) — add the three lines immediately after it, indentation-matched:

```rust
            entries: None,
            context_percent: None,
            context_age: None,
```

Fix these files (all test-fixture literals with `model: None,`):
- `crates/core/src/model.rs` (the `agent()` test helper)
- `crates/core/src/placement.rs`
- `crates/core/src/focus.rs`
- `crates/core/src/picker.rs`
- `crates/board/src/ui.rs` (two test fixtures)

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo build --workspace --tests && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/model.rs crates/core/src/placement.rs crates/core/src/focus.rs crates/core/src/picker.rs crates/board/src/ui.rs
git commit -m "core: add ContextInfo/SetContext, sync_registry stamping, footer_line"
```

---

### Task 3: `watch.rs` parses the `context_update` broadcast, with connect-race seeding

**Files:**
- Modify: `crates/core/src/watch.rs`

**Interfaces:**
- Consumes: `ContextInfo`, `Update::SetContext` (Task 2).
- Produces: `parse_config_context(line: &str) -> Option<ContextInfo>`.

- [ ] **Step 1: Write the failing parse test**

Add to the `#[cfg(test)] mod tests` block in `crates/core/src/watch.rs`, right after `parses_config_model`:

```rust
    #[test]
    fn parses_config_context() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"context_update","entries":42,"percent":12,"age":"3d"}}}"#;
        let info = parse_config_context(line).unwrap();
        assert_eq!(info.entries, 42);
        assert_eq!(info.percent, Some(12));
        assert_eq!(info.age, "3d");
        // percent omitted (unknown estimate, e.g. right after compaction).
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"context_update","entries":7,"age":"5m"}}}"#;
        let info = parse_config_context(line).unwrap();
        assert_eq!(info.entries, 7);
        assert_eq!(info.percent, None);
        assert_eq!(info.age, "5m");
        // Not a context_update.
        let state = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_config_context(state), None);
        assert_eq!(parse_config_context("not json"), None);
        // Missing required fields (entries/age) -> None, never a garbled value.
        let bad = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"context_update","percent":12}}}"#;
        assert_eq!(parse_config_context(bad), None);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p corral-core parses_config_context`
Expected: FAIL to compile (`parse_config_context` undefined).

- [ ] **Step 3: Implement `parse_config_context`**

In `crates/core/src/watch.rs`, add right after `parse_config_model`:

```rust
/// Extract a `context_update` session/update (corral-pi's own vocabulary, pi
/// only today): entries count, optional context-window percent (`None` when
/// the adapter's own estimate is unknown), and a pre-formatted age string.
/// `entries` and `age` are required; a line missing either yields `None`
/// rather than a half-populated value. Pure, unit tested.
pub fn parse_config_context(line: &str) -> Option<crate::model::ContextInfo> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "context_update" {
        return None;
    }
    let entries = update.get("entries")?.as_u64()?;
    let age = update.get("age")?.as_str()?.to_string();
    let percent = update
        .get("percent")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    Some(crate::model::ContextInfo { entries, percent, age })
}
```

- [ ] **Step 4: Wire the connect-race seed and live dispatch in `run`**

In `crates/core/src/watch.rs`'s `run` function, add a seed variable right after the existing `seed_model` declaration:

```rust
    let mut seed_model: Option<String> = None;
    // Like seed_model/seed_state: the extension sends its context_update seed
    // BEFORE the session/list reply, so stash it and stamp it onto the Upsert
    // instead of emitting a SetContext that would be dropped. None until the
    // first broadcast (a fresh pi session with no context to report yet, or a
    // non-pi adapter, which never sends this broadcast at all).
    let mut seed_context: Option<crate::model::ContextInfo> = None;
```

In the `Upsert` construction inside the `session/list` reply branch, add the three fields right after `model: seed_model.clone(),`:

```rust
                model: seed_model.clone(),
                entries: seed_context.as_ref().map(|c| c.entries),
                context_percent: seed_context.as_ref().and_then(|c| c.percent),
                context_age: seed_context.as_ref().map(|c| c.age.clone()),
```

Right after the existing model-change block (`if let Some(model) = parse_config_model(&line) { ... }`), add:

```rust
        // Live context change; before the Upsert stash it for the seed instead
        // of emitting a SetContext that would be dropped.
        if let Some(info) = parse_config_context(&line) {
            if seeded {
                let _ = tx.send(Update::SetContext(entry.path.clone(), info));
            } else {
                seed_context = Some(info);
            }
            continue;
        }
```

- [ ] **Step 5: Add a connect-race integration test**

Add to the `#[cfg(test)] mod tests` block, right after `preseed_model_lands_on_upsert`:

```rust
    #[test]
    fn preseed_context_lands_on_upsert() {
        use std::io::Write as _;
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc::channel;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("pi-1.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = channel();
        let entry = SocketEntry { path: sock.clone(), pid: 1, label: "pi".into() };
        let h = spawn(entry, tx);
        let (mut conn, _) = listener.accept().unwrap();
        // Context seed BEFORE the session/list reply (the real order).
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"context_update\",\"entries\":42,\"percent\":12,\"age\":\"3d\"}}}\n").unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessions\":[{\"sessionId\":\"s\",\"title\":\"t\",\"cwd\":\"/tmp\"}]}}\n").unwrap();
        let upsert = loop {
            match rx.recv().unwrap() {
                Update::Upsert(a) => break a,
                _ => continue,
            }
        };
        assert_eq!(upsert.entries, Some(42));
        assert_eq!(upsert.context_percent, Some(12));
        assert_eq!(upsert.context_age.as_deref(), Some("3d"));
        drop(conn);
        let _ = h.join();
    }
```

- [ ] **Step 6: Run the full suite to verify it passes**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/watch.rs
git commit -m "core: parse and seed the context_update broadcast"
```

---

### Task 4: TUI wiring — `crates/board` renders `footer_line()`

**Files:**
- Modify: `crates/board/src/ui.rs`
- Modify: `crates/board/src/main.rs`

**Interfaces:**
- Consumes: `Agent::footer_line()` (Task 2).

- [ ] **Step 1: Change `ui::render`'s signature and body to take the whole footer text**

In `crates/board/src/ui.rs`, the `render` function currently takes `model: Option<&str>` and formats it. Change the parameter name and drop the inline formatting — the caller now supplies the already-formatted line:

```rust
    meta: &CardMeta,
    footer_text: Option<&str>,
) {
```

(replacing the existing `meta: &CardMeta, model: Option<&str>,` parameter pair — same position, just the renamed/repurposed last parameter)

Then where the function currently does:

```rust
    if let Some(m) = model {
        let text = format!("model: {m}");
        let w = text.chars().count() as u16;
```

change to:

```rust
    if let Some(text) = footer_text {
        let w = text.chars().count() as u16;
```

The `Paragraph::new(Line::from(text.dim()))` line right after stays exactly as-is: ratatui's `Stylize::dim()` is implemented for `&str` the same as for `String`, so `text: &str` needs no `.to_string()`.

- [ ] **Step 2: Update the caller in `main.rs`**

In `crates/board/src/main.rs`, the block computing `selected_model` currently reads:

```rust
        // The selected card's model, display-only in the footer (corral never
        // selects a model). None when nothing selected or unreported.
        let selected_model = board
            .selectable()
            .get(selected)
            .and_then(|a| a.model.as_deref());
```

Change to:

```rust
        // The selected card's footer line (context size/age + model,
        // display-only). None when nothing selected or unreported.
        let selected_footer = board.selectable().get(selected).and_then(|a| a.footer_line());
```

And update the `ui::render(...)` call passing `selected_model` to instead pass `selected_footer.as_deref()`:

```rust
            ui::render(
                f,
                board,
                selected,
                &status,
                &mut list_states,
                &meta,
                selected_footer.as_deref(),
            );
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build --workspace`
Expected: PASS (no test changes needed here — the formatting logic itself is already covered by `footer_line_formats_full_and_partial_and_none` in Task 2; this task is pure wiring).

- [ ] **Step 4: Commit**

```bash
git add crates/board/src/ui.rs crates/board/src/main.rs
git commit -m "board: render the shared footer_line instead of a bare model string"
```

---

### Task 5: GUI wiring — `crates/gui` renders `footer_line()`

**Files:**
- Modify: `crates/gui/src/dashboard.rs`

**Interfaces:**
- Consumes: `Agent::footer_line()` (Task 2).

- [ ] **Step 1: Replace the model-only formatting**

In `crates/gui/src/dashboard.rs`, the block currently reads:

```rust
        // The selected card's model, display-only (parity with the TUI footer),
        // pushed to the far end of the row by a Fill spacer.
        let model_text = self
            .selected_agent()
            .and_then(|a| a.model.as_deref())
            .map(|m| format!("model: {m}"))
            .unwrap_or_default();
```

Change to:

```rust
        // The selected card's footer line (context size/age + model,
        // display-only, parity with the TUI footer), pushed to the far end of
        // the row by a Fill spacer. Shared formatting: core::model::Agent::footer_line.
        let footer_text = self
            .selected_agent()
            .and_then(|a| a.footer_line())
            .unwrap_or_default();
```

And update its one use further down from `text(model_text)` to `text(footer_text)`.

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build --workspace` (the GUI needs the `nix develop` devShell's `LD_LIBRARY_PATH` to link; if building outside it, `nix develop -c cargo build -p corral-gui` per `AGENTS.md` Development Setup).
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/gui/src/dashboard.rs
git commit -m "gui: render the shared footer_line instead of a bare model string"
```

---

### Task 6: `corral-pi.ts` computes and broadcasts `context_update`, persists it

**Files:**
- Modify: `extensions/corral-pi.ts`

**Interfaces:**
- Produces: the `context_update` wire broadcast (`{ sessionUpdate: "context_update", entries, percent, age }`), persisted registry record fields `entries`/`contextPercent`/`contextAge`.

- [ ] **Step 1: Add the age-formatting helper and the context-info builder**

In `extensions/corral-pi.ts`, add near the `modelString` function (same section, after it):

```typescript
	// A compact age string: "8s" / "5m" / "2h" / "3d" — mirrors
	// core::engine::age_label's unit scale exactly (kept independent since the
	// two live in different languages and the arithmetic is trivial).
	function ageLabel(ms: number): string {
		const s = Math.floor(ms / 1000);
		if (s < 60) return `${s}s`;
		if (s < 3600) return `${Math.floor(s / 60)}m`;
		if (s < 86400) return `${Math.floor(s / 3600)}h`;
		return `${Math.floor(s / 86400)}d`;
	}

	// Entries count, context-window percent, and age, from pi's own
	// introspection APIs (docs/extensions.md: ctx.sessionManager.getEntries(),
	// ctx.getContextUsage()). undefined until the session has at least one
	// entry (a session_start-only context has nothing to size yet). age is
	// derived from the session file's own creation entry (session-format.md:
	// the first logged entry carries the session's creation timestamp), so it
	// stays correct across a resume without corral persisting its own
	// start-time field.
	function contextInfo(
		ctx: ExtensionContext,
	): { entries: number; percent: number | null; age: string } | undefined {
		const entries = ctx.sessionManager.getEntries() as Array<{ timestamp?: string }>;
		if (entries.length === 0) return undefined;
		const createdAt = Date.parse(entries[0]?.timestamp ?? "");
		if (Number.isNaN(createdAt)) return undefined;
		const usage = ctx.getContextUsage();
		return {
			entries: entries.length,
			percent: usage?.percent ?? null,
			age: ageLabel(Date.now() - createdAt),
		};
	}
```

- [ ] **Step 2: Track the current context info and broadcast it**

Add a module-level variable near `currentModel`:

```typescript
	let currentModel: string | undefined;
	// Last-known context info (entries/percent/age), refreshed at turn_start
	// and turn_end (see below) and persisted to the registry record so a
	// dormant card still shows its last reading.
	let currentContext: { entries: number; percent: number | null; age: string } | undefined;
```

Add a broadcast function near `broadcastModel`:

```typescript
	// Broadcast the current context info as corral-pi's own context_update
	// session/update (not an ACP-standard shape, same footing as state_update).
	// Sent on turn_start/turn_end and as a per-connection seed.
	function contextUpdateLine(): string | undefined {
		if (!currentCtx || !currentContext) return undefined;
		return sessionUpdateLine({
			sessionUpdate: "context_update",
			entries: currentContext.entries,
			...(currentContext.percent !== null ? { percent: currentContext.percent } : {}),
			age: currentContext.age,
		});
	}

	function broadcastContext() {
		const line = contextUpdateLine();
		if (!line) return;
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}
```

- [ ] **Step 3: Refresh and broadcast at `turn_start`/`turn_end`, and seed new connections**

Change the `turn_start` and `turn_end` handlers to take `ctx` and refresh context:

```typescript
	pi.on("turn_start", async (_event, ctx) => {
		currentState = "running";
		broadcastState();
		currentContext = contextInfo(ctx);
		broadcastContext();
	});

	pi.on("turn_end", async (_event, ctx) => {
		currentState = "idle";
		broadcastState();
		// The title may have become available this turn (the first user message
		// becomes the fallback title); push it to clients if it changed.
		broadcastTitleIfChanged();
		currentContext = contextInfo(ctx);
		broadcastContext();
		// Refresh lastSeen so age-based pruning of dormant records is accurate.
		if (currentCtx) writeRegistry(currentCtx, socketPath ?? null);
	});
```

In the `server.on("connection", ...)` handler, right after the existing model-seed block (`if (currentModel) { ... }`), add:

```typescript
			// Seed the context info too, so a card shows it before the first turn.
			if (currentContext) {
				const line = contextUpdateLine();
				if (line && !conn.destroyed) conn.write(line);
			}
```

- [ ] **Step 4: Persist the fields in `writeRegistry`**

In `writeRegistry`, the `record` object currently has:

```typescript
			// Last-known model as "<provider>/<id>", so a dormant card shows it.
			// An undefined value is dropped by JSON.stringify, so no `model` key
			// is written when unknown (matching corral's Option<String> parse).
			model: currentModel,
			lastSeen: new Date().toISOString(),
```

Change to:

```typescript
			// Last-known model as "<provider>/<id>", so a dormant card shows it.
			// An undefined value is dropped by JSON.stringify, so no `model` key
			// is written when unknown (matching corral's Option<String> parse).
			model: currentModel,
			// Last-known context size/age, so a dormant card still shows it.
			// undefined fields are dropped by JSON.stringify (matching corral's
			// Option parse); percent is only included when known.
			entries: currentContext?.entries,
			contextPercent: currentContext?.percent ?? undefined,
			contextAge: currentContext?.age,
			lastSeen: new Date().toISOString(),
```

- [ ] **Step 5: Manual sanity check (no TS test harness in this repo)**

There is no `tsconfig`/type-check command for `extensions/` in this repo (pi type-strips `.ts` at load, per `AGENTS.md`: "node runs the `.ts` directly via native type-stripping, no build step"). Verify by reading the diff once more for typos, then rely on Task 7's VM e2e scenario as the executable check.

- [ ] **Step 6: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: broadcast and persist context_update (entries/percent/age)"
```

---

### Task 7: VM e2e scenario + docs

**Files:**
- Modify: `nix/tests/acp.py`
- Modify: `nix/tests/scenarios/pi.py`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: the `context_update` broadcast (Task 6), `RegistryEntry.entries`/`context_percent`/`context_age` (Task 1).

- [ ] **Step 1: Add a `context` op to `acp.py`**

In `nix/tests/acp.py`, add to the module docstring's usage list, right after the `model` line:

```
  acp.py context <socket> [secs]        -> wait for a context_update
                                           broadcast; prints
                                           {"ok":true,"entries":..,"age":..}
```

Add the command function right after `cmd_model`:

```python
def cmd_context(path, secs):
    # Wait for the context_update seed (sent on connect, like state_update and
    # config_options_update). Read every line without rpc() so the seed is not
    # discarded while awaiting the init reply.
    deadline = time.time() + secs
    s = connect(path, timeout=secs + 2)
    send(s, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
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
            upd = msg.get("params", {}).get("update", {})
            if upd.get("sessionUpdate") == "context_update":
                print(json.dumps({"ok": True, "entries": upd.get("entries"),
                                  "percent": upd.get("percent"),
                                  "age": upd.get("age")}))
                return
    print(json.dumps({"ok": False}))
    sys.exit(1)
```

Add the dispatch branch right after the `model` branch:

```python
    elif op == "context":
        cmd_context(sys.argv[2],
                    int(sys.argv[3]) if len(sys.argv) > 3 else 15)
```

- [ ] **Step 2: Assert it in the `pi` scenario**

In `nix/tests/scenarios/pi.py`, section "2. a plain turn: running -> idle" currently ends with:

```python
acp(f"prompt {sock_a} {sid_a} 'smoke:reply operator-turn'")
acp(f"state {sock_a} idle 30")
assert stub_saw("operator-turn"), "stub never saw the operator turn"
```

Add right after it:

```python
# Context exposure: after a turn, pi has at least one session-log entry, so
# the live broadcast and the persisted record must both carry it.
context_res = json.loads(acp(f"context {sock_a} 20"))
assert isinstance(context_res.get("entries"), int) and context_res["entries"] >= 1, \
    f"pi did not broadcast entries: {context_res}"
assert context_res.get("age"), f"pi did not broadcast an age string: {context_res}"
recs = wait_records(
    lambda rs: any(r.get("sessionId") == sid_a and r.get("entries")
                   for r in rs),
    timeout=30, desc="A's record carries entries after a turn")
rec_a = next(r for r in recs if r.get("sessionId") == sid_a)
assert rec_a.get("entries", 0) >= 1, f"record missing entries: {rec_a}"
assert rec_a.get("contextAge"), f"record missing contextAge: {rec_a}"
```

- [ ] **Step 3: Update `AGENTS.md`**

In `AGENTS.md`, in the `corral-pi.ts` extension bullet, find the sentence about the model broadcast:

```
It also broadcasts the current model (from `ctx.model`, refreshed on
`model_select`) as a `config_options_update` config option and persists it in
the record, seeded on connect like state (verified against pi's types).
```

Add right after it:

```
It also broadcasts a corral-pi-specific `context_update` session/update
(entries count from `ctx.sessionManager.getEntries().length`, context-window
percent from `ctx.getContextUsage()`, and a pre-formatted age string derived
from the session's own creation entry), refreshed on `turn_start`/`turn_end`
and persisted in the record (`entries`/`contextPercent`/`contextAge`), seeded
on connect like state and model. Shown next to the model in the footer for the
selected card (`core::model::Agent::footer_line`), pi only for now — other
adapters have no equivalent introspection API surfaced today.
```

- [ ] **Step 4: Run the pi e2e check**

Run: `just e2e-one e2e-pi`
Expected: PASS (needs KVM; Linux-only per `AGENTS.md`). If KVM is unavailable in this environment, note that explicitly rather than claiming a pass — this is the only executable verification for the TypeScript changes in Task 6.

- [ ] **Step 5: Commit**

```bash
git add nix/tests/acp.py nix/tests/scenarios/pi.py AGENTS.md
git commit -m "e2e+docs: cover the context_update broadcast for pi"
```
