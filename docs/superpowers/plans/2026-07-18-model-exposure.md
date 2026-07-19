# Live Model Display (Harness → Corral) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show which LLM model each agent session runs, live and display-only, in the corral footer for the selected card (TUI + GUI).

**Architecture:** The adapter broadcasts the current model as an ACP Session Config Option (`category: "model"`) in a `config_options_update` session/update — on every model change and once at connect. corral reads only `configOptions[category=="model"].currentValue`, stores it on the `Agent`, and renders it in the footer for the selected card. The model is also persisted in the registry record so a selected dormant card shows its last-known model. corral never selects a model (pure viewer); it ignores `options[]` and never sends `session/set_config_option`.

**Tech Stack:** Rust (corral-core, board=ratatui, gui=iced, daemon), TypeScript/JS adapters (pi, opencode, claude sidecar, cursor). Tests: `cargo test` via `just test`; adapter pure cores via `node --test` where present.

## Global Constraints

- TUI/GUI parity is a hard rule: every user-facing change lands in BOTH `crates/board` and `crates/gui`. Shared logic lives in `crates/core`.
- corral stays agent-neutral and display-only: show the model string verbatim, never prettify, never offer model selection.
- Model string format on the wire and record: `"<provider>/<id>"` (e.g. `anthropic/claude-opus-4`). corral shows it as-is.
- Missing model degrades gracefully: `Option<String>` everywhere; a `None` model renders as nothing (no placeholder text).
- Comments document why, referring only to current code (per AGENTS.md).
- Small, single-line-ish commits; no co-author attribution.
- ACP wire word is `config_options_update` (v1). Reserved category `model` (no `_` prefix). corral keys on `category == "model"`.
- Keep `AGENTS.md`, `README.md`, `CONVENTION.md` current when interfaces change (hard rule).

---

### Task 1: `RegistryEntry.model` field, parse, serialize, vet

Adds the persisted `model` field to the record type and every place that constructs, parses, or serializes a `RegistryEntry`. Rust breaks every struct literal when a field is added, so all constructors are fixed in this one task to keep the tree compiling.

**Files:**
- Modify: `crates/core/src/discovery.rs` (struct `RegistryEntry` ~line 22; `parse_registry_json` ~line 97; test constructors)
- Modify: `crates/core/src/curation.rs` (`vet` ~line 137; test `rec` helper ~line 277)
- Modify: `crates/daemon/src/curator.rs` (`record_json` ~line 118)
- Modify: `crates/core/src/model.rs` (any `RegistryEntry { .. }` test literals — `dormant_record`, `live_agent_gets_hidden_and_resume_from_record`, `dormant_agent_inherits_gui_from_record`)

**Interfaces:**
- Produces: `RegistryEntry.model: Option<String>` — the last-known model string (`"provider/id"`), parsed from the record's `model` key, sanitized in `vet`, serialized by `record_json` when `Some`.

- [ ] **Step 1: Write the failing test** in `crates/core/src/discovery.rs` (add to `mod tests`)

```rust
    #[test]
    fn model_field_parses_and_defaults_none() {
        let e = parse_registry_json(r#"{"sessionId":"s1","model":"anthropic/claude-opus-4"}"#).unwrap();
        assert_eq!(e.model.as_deref(), Some("anthropic/claude-opus-4"));
        // Absent -> None (older/unknown producer).
        let e = parse_registry_json(r#"{"sessionId":"s2"}"#).unwrap();
        assert_eq!(e.model, None);
        // Non-string -> None (never a garbled value).
        let e = parse_registry_json(r#"{"sessionId":"s3","model":42}"#).unwrap();
        assert_eq!(e.model, None);
    }
```

- [ ] **Step 2: Run it, expect a compile error** (field does not exist yet)

Run: `cd crates/core && cargo test model_field_parses`
Expected: FAIL — `no field 'model' on type RegistryEntry`

- [ ] **Step 3: Add the field to the struct** in `crates/core/src/discovery.rs`, after the `description` field (~line 64):

```rust
    /// The LLM model this session runs, as `"<provider>/<id>"` (e.g.
    /// `anthropic/claude-opus-4`). Written by the adapter so a selected
    /// dormant card shows its last-known model; live cards refresh it over the
    /// socket (a `config_options_update` broadcast). Verbatim adapter string,
    /// shown as-is (corral never prettifies). Absent for a producer that does
    /// not report a model.
    pub model: Option<String>,
```

- [ ] **Step 4: Parse it** in `parse_registry_json`, in the `RegistryEntry { .. }` literal after `description`:

```rust
        model: str_field("model"),
```

- [ ] **Step 5: Serialize it** in `crates/daemon/src/curator.rs` `record_json`, after the `description` block (~line 152):

```rust
    if let Some(m2) = &rec.model {
        m.insert("model".into(), m2.clone().into());
    }
```

- [ ] **Step 6: Sanitize it in `vet`** in `crates/core/src/curation.rs`, after `rec.description = rec.description.map(sanitize);` (~line 149):

```rust
    rec.model = rec.model.map(sanitize);
```

- [ ] **Step 7: Fix every other `RegistryEntry` literal.** Add `model: None,` to:
  - `crates/core/src/curation.rs` test helper `rec` (after `description: None,` ~line 290)
  - `crates/core/src/model.rs` tests: the `RegistryEntry` in `live_agent_gets_hidden_and_resume_from_record`, in `dormant_record`, and in `dormant_agent_inherits_gui_from_record` (each after `description: None,`)

- [ ] **Step 8: Add a curator round-trip test** in `crates/daemon/src/curator.rs` (`mod tests` if present, else create one). If no test module exists, make `record_json` reachable and assert:

```rust
    #[test]
    fn record_json_includes_model_when_set() {
        let mut rec = corral_core::discovery::parse_registry_json(
            r#"{"sessionId":"s1","model":"anthropic/claude-opus-4"}"#,
        )
        .unwrap();
        rec.cwd = Some("/tmp/p".into());
        let json = record_json(&rec).unwrap();
        assert!(json.contains("\"model\": \"anthropic/claude-opus-4\""));
        // Absent model is omitted, not written as null.
        rec.model = None;
        assert!(!record_json(&rec).unwrap().contains("model"));
    }
```

- [ ] **Step 9: Run all core + daemon tests**

Run: `just test`
Expected: PASS (all)

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/discovery.rs crates/core/src/curation.rs crates/daemon/src/curator.rs crates/core/src/model.rs
git commit -m "core: persist model in registry record (parse, vet, serialize)"
```

---

### Task 2: `Agent.model` field, `Update::SetModel`, board apply + registry stamp

Adds the live model to the in-memory `Agent`, the update variant that carries a change, and stamps the persisted model onto live and dormant agents in `sync_registry`.

**Files:**
- Modify: `crates/core/src/model.rs` (struct `Agent`; `enum Update`; `Board::apply`; `sync_registry`; `agent` test helper; `dormant` mapping)
- Modify: `crates/core/src/watch.rs` (the `Agent { .. }` seed literal ~line 175, add `model`)

**Interfaces:**
- Consumes: `RegistryEntry.model` from Task 1.
- Produces:
  - `Agent.model: Option<String>`
  - `Update::SetModel(PathBuf, String)`
  - `Board::apply` handles `SetModel` (set on the keyed live agent)
  - `sync_registry` sets `agent.model` from the matching record on both live and dormant agents.

- [ ] **Step 1: Write the failing test** in `crates/core/src/model.rs` `mod tests`:

```rust
    #[test]
    fn set_model_updates_live_agent() {
        let mut b = Board::default();
        b.apply(Update::Upsert(Box::new(agent("/s/pi-1.sock", State::Idle))));
        b.apply(Update::SetModel(
            PathBuf::from("/s/pi-1.sock"),
            "anthropic/claude-opus-4".into(),
        ));
        assert_eq!(
            b.in_state(State::Idle)[0].model.as_deref(),
            Some("anthropic/claude-opus-4")
        );
    }

    #[test]
    fn dormant_agent_inherits_model_from_record() {
        let mut b = Board::default();
        let mut rec = dormant_record("q1", "/tmp/q", "2026-06-01T00:00:00Z");
        rec.model = Some("anthropic/claude-sonnet-4".into());
        b.sync_registry(&[rec], &HashSet::new());
        assert_eq!(
            b.dormant()[0].model.as_deref(),
            Some("anthropic/claude-sonnet-4")
        );
    }
```

- [ ] **Step 2: Run it, expect compile error**

Run: `cd crates/core && cargo test set_model_updates_live_agent`
Expected: FAIL — no field `model`, no variant `SetModel`

- [ ] **Step 3: Add the field to `Agent`** (in `crates/core/src/model.rs`, after `hidden: bool` / before `state_since`):

```rust
    /// The LLM model this agent runs, `"<provider>/<id>"` (e.g.
    /// `anthropic/claude-opus-4`). Live agents refresh it from a
    /// `config_options_update` broadcast; `sync_registry` also stamps the
    /// record's last-known model onto both live and dormant agents. Shown
    /// verbatim in the footer for the selected card. `None` until reported.
    pub model: Option<String>,
```

- [ ] **Step 4: Add the `Update` variant** (in `enum Update`, after `SetActivity`):

```rust
    /// The current model (from a `config_options_update` broadcast): a
    /// `"provider/id"` string shown in the footer for the selected card.
    SetModel(PathBuf, String),
```

- [ ] **Step 5: Handle it in `Board::apply`** (after the `SetActivity` arm):

```rust
            Update::SetModel(path, model) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.model = Some(model);
                }
            }
```

- [ ] **Step 6: Stamp model in `sync_registry`.** In the dormant `.map(|e| Agent { .. })` literal, after `hidden: e.hidden,`:

```rust
                model: e.model.clone(),
```

  And in the live-agent stamping loop (`for a in self.live.values_mut()`, inside the `if let Some(e) = entries.iter().find(..)` block), after `a.hidden = e.hidden;`:

```rust
                    // The record carries the last-known model; the socket
                    // refreshes it live via config_options_update. Stamp the
                    // record value so a live card is never blank before its
                    // first model broadcast.
                    if a.model.is_none() {
                        a.model = e.model.clone();
                    }
```

  (Guard on `is_none()` so a fresher live broadcast is never overwritten by a staler record value.)

- [ ] **Step 7: Fix the `agent` test helper** in `model.rs` (`mod tests`), add after `hidden: false,`:

```rust
            model: None,
```

- [ ] **Step 8: Fix the `watch.rs` seed literal.** In `crates/core/src/watch.rs`, in the `Update::Upsert(Box::new(Agent { .. }))` seed (~line 175), after `hidden: false,`:

```rust
                // Seeded from the config_options_update the extension sends on
                // connect (stashed below), else None until the first broadcast.
                model: seed_model.clone(),
```

  (This references `seed_model`, added in Task 3; if Task 3 is not yet done, temporarily use `model: None,` and change it in Task 3. Prefer doing Task 3 immediately after so the seed lands.)

- [ ] **Step 9: Run core tests**

Run: `cd crates/core && cargo test`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/core/src/model.rs crates/core/src/watch.rs
git commit -m "core: carry live model on Agent, SetModel update, registry stamp"
```

---

### Task 3: Parse `config_options_update` in the watcher, seed the model

Extracts the model from the ACP config-option broadcast and wires the connect-race seed exactly like `seed_state`.

**Files:**
- Modify: `crates/core/src/watch.rs` (`parse_config_model` new fn; `run` loop seed + emit; `mod tests`)

**Interfaces:**
- Consumes: `Update::SetModel` and `Agent.model` from Task 2.
- Produces: `pub fn parse_config_model(line: &str) -> Option<String>` — returns the `model`-category option's `currentValue` from a `config_options_update` session/update line, else `None`.

- [ ] **Step 1: Write the failing test** in `crates/core/src/watch.rs` `mod tests`:

```rust
    #[test]
    fn parses_config_model() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"config_options_update","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"anthropic/claude-opus-4"}]}}}"#;
        assert_eq!(
            parse_config_model(line).as_deref(),
            Some("anthropic/claude-opus-4")
        );
        // No model-category option present -> None.
        let other = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"config_options_update","configOptions":[{"id":"mode","category":"mode","currentValue":"ask"}]}}}"#;
        assert_eq!(parse_config_model(other), None);
        // Not a config_options_update.
        let state = r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"state_update","state":"idle"}}}"#;
        assert_eq!(parse_config_model(state), None);
        assert_eq!(parse_config_model("not json"), None);
    }
```

- [ ] **Step 2: Run it, expect failure**

Run: `cd crates/core && cargo test parses_config_model`
Expected: FAIL — `parse_config_model` not found

- [ ] **Step 3: Implement `parse_config_model`** in `crates/core/src/watch.rs` (next to `parse_state_notification`):

```rust
/// Extract the current model from an ACP `config_options_update` session/update
/// (agentclientprotocol.com — Session Config Options). Returns the
/// `currentValue` of the option whose `category` is `"model"`, else `None`.
/// corral is display-only, so it reads only the current value and ignores the
/// selectable `options`/`type`. Pure, unit tested.
pub fn parse_config_model(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    if msg.get("method")? != "session/update" {
        return None;
    }
    let update = msg.get("params")?.get("update")?;
    if update.get("sessionUpdate")? != "config_options_update" {
        return None;
    }
    update
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| o.get("category").and_then(|c| c.as_str()) == Some("model"))
        .and_then(|o| o.get("currentValue"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
```

- [ ] **Step 4: Add the `seed_model` stash and emit** in `run`. After `let mut seed_state = DEFAULT_STATE;` (~line 173):

```rust
    // Like seed_state: the extension sends its config_options_update model seed
    // BEFORE the session/list reply, so it is read before the agent exists in
    // the board. A SetModel for an absent socket is dropped, so stash the seed
    // and stamp it onto the Upsert instead. None until the first broadcast.
    let mut seed_model: Option<String> = None;
```

  In the loop, after the `parse_state_notification` block and before the title block, add:

```rust
        // Live model change; before the Upsert stash it for the seed instead of
        // emitting a SetModel that would be dropped.
        if let Some(model) = parse_config_model(&line) {
            if seeded {
                let _ = tx.send(Update::SetModel(entry.path.clone(), model));
            } else {
                seed_model = Some(model);
            }
            continue;
        }
```

  Ensure the seed `Agent` literal uses `model: seed_model.clone(),` (from Task 2 Step 8).

- [ ] **Step 5: Add a seed-race test** in `watch.rs` `mod tests` (mirror `preseed_state_lands_on_upsert`):

```rust
    #[test]
    fn preseed_model_lands_on_upsert() {
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
        // Model config seed BEFORE the session/list reply (the real order).
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"config_options_update\",\"configOptions\":[{\"id\":\"model\",\"category\":\"model\",\"currentValue\":\"anthropic/claude-opus-4\"}]}}}\n").unwrap();
        conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"sessions\":[{\"sessionId\":\"s\",\"title\":\"t\",\"cwd\":\"/tmp\"}]}}\n").unwrap();
        let upsert = loop {
            match rx.recv().unwrap() {
                Update::Upsert(a) => break a,
                _ => continue,
            }
        };
        assert_eq!(upsert.model.as_deref(), Some("anthropic/claude-opus-4"));
        drop(conn);
        let _ = h.join();
    }
```

- [ ] **Step 6: Run core tests**

Run: `cd crates/core && cargo test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/watch.rs
git commit -m "core: parse config_options_update model, seed live model"
```

---

### Task 4: Footer model in the TUI board

Shows the selected card's model in the footer, right-aligned on the status row so a transient status (left) and the model (right) coexist.

**Files:**
- Modify: `crates/board/src/ui.rs` (`render` signature ~line 726; status-row rendering ~line 764)
- Modify: `crates/board/src/main.rs` (compute selected model, pass to `render` ~line 356)

**Interfaces:**
- Consumes: `Agent.model` from Task 2.
- Produces: `ui::render` gains a `model: Option<&str>` parameter (selected card's model), rendered right-aligned on the status row.

- [ ] **Step 1: Read the current `render` + status-row code** to confirm the exact signature and the spacer/status `Rect`.

Run: `sed -n '726,790p' crates/board/src/ui.rs`

- [ ] **Step 2: Add the `model` parameter to `render`.** Change the signature (append the param last):

```rust
pub fn render(
    frame: &mut Frame,
    board: &Board,
    selected: usize,
    status: &str,
    list_states: &mut [ratatui::widgets::ListState],
    meta: &CardMeta,
    model: Option<&str>,
) {
```

- [ ] **Step 3: Render the model right-aligned on the status row.** In the `if !status.is_empty()` block region (~line 766), replace the single-status render so the row carries the status on the left and `model: <x>` on the right. Use the same `spacer` Rect the status uses:

```rust
    // The status row carries a transient action status on the left and the
    // selected card's model on the right (display-only; corral never selects a
    // model). Both are dim so the columns stay the focus.
    {
        let spacer = /* the existing status Rect for this row */;
        let left = Line::from(status.dim());
        frame.render_widget(Paragraph::new(left), spacer);
        if let Some(m) = model {
            let text = format!("model: {m}");
            let w = text.chars().count() as u16;
            let right = Rect {
                x: spacer.x + spacer.width.saturating_sub(w),
                y: spacer.y,
                width: w.min(spacer.width),
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(text.dim())),
                right,
            );
        }
    }
```

  NOTE to implementer: reuse the exact `Rect` the current code computes for the status row (do not invent a new one). Keep the existing clickable-footer rendering below untouched.

- [ ] **Step 4: Pass the selected model from `main.rs`.** Before `terminal.draw`, compute:

```rust
        let selected_model = board
            .selectable()
            .get(selected)
            .and_then(|a| a.model.as_deref());
```

  And update the `ui::render` call:

```rust
            ui::render(f, board, selected, &status, &mut list_states, &meta, selected_model);
```

- [ ] **Step 5: Build the board**

Run: `cargo build -p corral`
Expected: compiles

- [ ] **Step 6: Manual sanity (optional if no display) + run tests**

Run: `just test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/board/src/ui.rs crates/board/src/main.rs
git commit -m "board: show selected card's model in the footer"
```

---

### Task 5: Footer model in the GUI board (parity)

Mirror Task 4 in the iced GUI: the bottom key-hint bar shows the selected card's model.

**Files:**
- Modify: `crates/gui/src/dashboard.rs` (the footer/key-hint view; the selected card lookup)

**Interfaces:**
- Consumes: `Agent.model` from Task 2.

- [ ] **Step 1: Locate the footer view and the `selected` card lookup.**

Run: `grep -n "footer\|key-hint\|fn view\|selectable\|selected" crates/gui/src/dashboard.rs`

- [ ] **Step 2: Compute the selected model** where the footer is built:

```rust
        let selected_model = board
            .selectable()
            .get(self.selected)
            .and_then(|a| a.model.as_deref());
```

- [ ] **Step 3: Append the model to the footer row.** Add a right-aligned (or trailing) dim text element to the existing bottom key-hint `row!`/container, e.g.:

```rust
        // Selected card's model, display-only (parity with the TUI footer).
        let model_text = selected_model
            .map(|m| format!("model: {m}"))
            .unwrap_or_default();
```

  Add `text(model_text).size(<footer size>)` styled dim, pushed to the far end of the footer row (use `Space::with_width(Length::Fill)` before it if the row is a horizontal layout).

- [ ] **Step 4: Build the GUI** (needs the devShell LD_LIBRARY_PATH)

Run: `nix develop -c cargo build -p corral-gui`
Expected: compiles

- [ ] **Step 5: Run tests**

Run: `just test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/gui/src/dashboard.rs
git commit -m "gui: show selected card's model in the footer (parity)"
```

---

### Task 6: pi adapter — emit and persist the model

pi has `ctx.model` (`{provider, id}`) and the `model_select` event. Broadcast the model as a `config_options_update` on connect and on change, and write it into the record.

**Files:**
- Modify: `extensions/corral-pi.ts`

**Interfaces:**
- Produces: on the socket, a `config_options_update` session/update carrying the model option; in the record, a `model` field.

- [ ] **Step 1: Add a `currentModel` helper and state.** Near the other session state vars, add a module-scope `let currentModel: string | undefined;`. Add a helper:

```typescript
	// The current model as "<provider>/<id>" for the ACP config option, or
	// undefined when pi has not resolved one. Read from ctx.model; corral shows
	// it verbatim (never prettified).
	function modelString(ctx: ExtensionContext): string | undefined {
		const m = (ctx as { model?: { provider?: string; id?: string } }).model;
		if (!m?.provider || !m?.id) return undefined;
		return `${m.provider}/${m.id}`;
	}
```

- [ ] **Step 2: Add the config-option broadcast helper** (next to `broadcastState`):

```typescript
	// Broadcast the current model as an ACP Session Config Option
	// (category "model"). corral reads currentValue for display; it never
	// selects a model, so options[] is omitted. Sent on model_select and as a
	// per-connection seed.
	function modelConfigLine(): string | undefined {
		if (!currentCtx || !currentModel) return undefined;
		return sessionUpdateLine({
			sessionUpdate: "config_options_update",
			configOptions: [
				{
					id: "model",
					name: "Model",
					category: "model",
					type: "select",
					currentValue: currentModel,
				},
			],
		});
	}

	function broadcastModel() {
		const line = modelConfigLine();
		if (!line) return;
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}
```

- [ ] **Step 3: Seed model on connect.** In the `net.createServer((conn) => { .. })` handler, right after the existing `state_update` seed write, add:

```typescript
			if (currentModel) {
				const line = modelConfigLine();
				if (line && !conn.destroyed) conn.write(line);
			}
```

- [ ] **Step 4: Set + broadcast on `session_start` and `model_select`.** In `session_start`, after `currentCtx = ctx;`, add:

```typescript
		currentModel = modelString(ctx);
```

  Add a new handler:

```typescript
	pi.on("model_select", async (event, ctx) => {
		currentCtx = ctx;
		currentModel = `${event.model.provider}/${event.model.id}`;
		broadcastModel();
		// Persist so a dormant card shows the last-known model.
		if (currentCtx) writeRegistry(currentCtx, socketPath ?? null);
	});
```

- [ ] **Step 5: Write `model` into the record.** In `writeRegistry`, add to the `record` object (after `hidden,`):

```typescript
			// Last-known model as "<provider>/<id>", so a dormant card shows it.
			model: currentModel,
```

  (An `undefined` value is dropped by `JSON.stringify`, so no `model` key is written when unknown — matching corral's `Option<String>` parse.)

- [ ] **Step 6: Typecheck the extension** (if a pi/tsc path exists in the repo; else visual review).

Run: `just lint` (or the repo's TS check if present)
Expected: no new errors

- [ ] **Step 7: Commit**

```bash
git add extensions/corral-pi.ts
git commit -m "corral-pi: broadcast + persist session model (ACP config option)"
```

---

### Task 7: opencode adapter — emit and persist the model (defensive)

opencode's SDK exposes the session's model; the exact field is UNVERIFIED here (no opencode toolchain). Probe defensively, emit if available, else skip (footer stays blank for opencode). Mirror the pi shape.

**Files:**
- Modify: `extensions/corral-opencode.ts`

- [ ] **Step 1: Find the model source.** Search the file for how it reads session/message metadata (the SDK client, `session.idle`, message events). The model typically rides an assistant message (`message.modelID` / `providerID`) or session info. Read the surrounding code:

Run: `grep -n "model\|providerID\|session\|message\|broadcast\|state_update\|writeRegistry\|record" extensions/corral-opencode.ts`

- [ ] **Step 2: Add a defensively-probed `currentModel`** string, set from whatever the SDK exposes (guard every access with optional chaining; flag UNVERIFIED in a comment):

```typescript
	// UNVERIFIED (no opencode toolchain here): the model is probed from the
	// assistant message metadata. Shape guarded so a miss just leaves the model
	// unreported (corral shows nothing), never throwing into the plugin host.
	let currentModel: string | undefined;
	function setModelFrom(obj: unknown) {
		const o = obj as { providerID?: string; modelID?: string; provider?: string; model?: string };
		const provider = o?.providerID ?? o?.provider;
		const model = o?.modelID ?? o?.model;
		if (provider && model) currentModel = `${provider}/${model}`;
	}
```

  Call `setModelFrom(..)` where assistant messages / session info are observed, then `broadcastModel()`.

- [ ] **Step 3: Add the same `config_options_update` broadcast + connect seed + record field** as pi Task 6 Steps 2–3, 5 (copy the shape; opencode already has a `broadcast`/socket-write and `writeRegistry` equivalent — reuse those). Include `model: currentModel` in the record object.

- [ ] **Step 4: Visual review** (no typecheck in repo).

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-opencode.ts
git commit -m "corral-opencode: broadcast + persist session model (defensive, UNVERIFIED)"
```

---

### Task 8: Claude adapter (sidecar) — emit and persist the model (defensive)

Claude Code hook payloads carry a `model` field on many events; UNVERIFIED here. The sidecar owns the socket, so it broadcasts. Probe defensively.

**Files:**
- Modify: `extensions/corral-claude/sidecar.ts` (broadcast + record)
- Possibly: `extensions/corral-claude/hook.ts` (forward the model from a hook payload to the sidecar if the sidecar has no other source)

- [ ] **Step 1: Determine the model source.** Claude hook JSON typically includes `model` (e.g. on `UserPromptSubmit`/`Stop`). Read how the hook shim forwards events to the sidecar and whether `model` is already in the payload:

Run: `grep -n "model\|payload\|broadcast\|state_update\|writeRegistry\|record\|ctl" extensions/corral-claude/sidecar.ts extensions/corral-claude/hook.ts`

- [ ] **Step 2: Capture the model in the sidecar** from the hook event payload it already receives over the control socket (guard defensively):

```typescript
	// UNVERIFIED: Claude hook payloads carry a `model` field. Guarded so a
	// missing field just leaves the model unreported.
	let currentModel: string | undefined;
	function setModelFromHook(payload: unknown) {
		const m = (payload as { model?: string }).model;
		if (typeof m === "string" && m) currentModel = m; // Claude reports a bare id; keep verbatim
	}
```

  Call it where the sidecar handles each hook event.

- [ ] **Step 3: Add the `config_options_update` broadcast + connect seed + record `model` field** (same shape as pi Task 6). Reuse the sidecar's existing socket-broadcast and registry-write helpers.

- [ ] **Step 4: Visual review.**

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-claude/
git commit -m "corral-claude: broadcast + persist session model (defensive, UNVERIFIED)"
```

---

### Task 9: Cursor adapter — emit and persist the model (defensive / likely no source)

Cursor exposes no API to read the Composer model; UNVERIFIED and likely unavailable. If no model source exists, the deliverable is: confirm no source, and leave the adapter emitting no model (footer blank for cursor). Only add plumbing if a source is found.

**Files:**
- Modify (only if a source exists): `extensions/corral-cursor/extension.js`, `extensions/corral-cursor/lib.js`

- [ ] **Step 1: Investigate whether Cursor exposes the model** to the extension host or the state hook payload:

Run: `grep -n "model\|state-hook\|payload\|broadcast\|writeRegistry\|record" extensions/corral-cursor/extension.js extensions/corral-cursor/lib.js extensions/corral-cursor/state-hook.js`

- [ ] **Step 2a: If a source exists**, add the same `config_options_update` broadcast + connect seed + record `model` field as pi Task 6, guarded defensively; add a `lib.js` pure test with `node --test` if a `currentValue` formatter is introduced.

- [ ] **Step 2b: If no source exists**, add a one-line in-file comment documenting that Cursor exposes no model to the extension, so the model is deliberately unreported, and make no functional change.

- [ ] **Step 3: Commit**

```bash
git add extensions/corral-cursor/
git commit -m "corral-cursor: report session model where available (else document absence)"
```

---

### Task 10: Documentation — CONVENTION.md, AGENTS.md, README

Document the new record field and the config-option surface per the hard rule.

**Files:**
- Modify: `CONVENTION.md` (record fields §1; the ACP surface / broadcasts section)
- Modify: `AGENTS.md` (the `RegistryEntry` field list under `crates/core`; the ACP Conformance section; the extension descriptions; Known Limitations if a model is unavailable per adapter)
- Modify (if it lists record fields or state broadcasts): `README.md`

- [ ] **Step 1: Add the `model` record field to `CONVENTION.md` §1** (record schema), one entry:

```
model (optional string): the LLM model as "<provider>/<id>" (e.g.
"anthropic/claude-opus-4"). Display-only; consumers show it verbatim for the
selected session. Latest-seen value per session. Absent when the adapter cannot
report a model.
```

- [ ] **Step 2: Document the `config_options_update` broadcast in `CONVENTION.md`** (the state-broadcast / ACP surface section), noting corral reads only `configOptions[category=="model"].currentValue`, that it is broadcast on model change and seeded on connect, and that corral is display-only (never sends `session/set_config_option`).

- [ ] **Step 3: Update `AGENTS.md`:**
  - Add `model` to the `RegistryEntry` field description under `crates/core` / `discovery.rs`.
  - In `src/watch.rs`, note it now parses `config_options_update` → live model.
  - In `src/model.rs`, note `Agent.model` and that `sync_registry` stamps it.
  - In the ACP Conformance section, add the `config_options_update` model option (v1 shape now, folds into v2 `state_update`; corral display-only).
  - In each Extension bullet, note it broadcasts + persists the model (pi verified; opencode/claude/cursor UNVERIFIED/defensive; cursor may be unavailable).
  - In the board/gui interface descriptions, note the footer shows the selected card's model.

- [ ] **Step 4: Update `README.md`** only if it enumerates record fields or state broadcasts (keep it short — do not grow it into a manual).

- [ ] **Step 5: Verify docs match code** (re-read the field name/shape against Tasks 1–6).

- [ ] **Step 6: Commit**

```bash
git add CONVENTION.md AGENTS.md README.md
git commit -m "docs: model record field + config_options_update surface"
```

---

## Self-Review

**Spec coverage:**
- Live model over ACP config option → Tasks 3 (parse), 6–9 (emit). ✓
- Display-only, no selection → corral never sends `set_config_option`; only `currentValue` read (Task 3). ✓
- Footer, selected card only → Tasks 4 (TUI), 5 (GUI). ✓
- Dormant last-known model → persisted in record (Task 1), stamped in `sync_registry` (Task 2), shown by the same footer. ✓
- All four adapters → Tasks 6–9 (pi verified; others defensive/UNVERIFIED; cursor may be unavailable). ✓
- Parity (TUI+GUI) → Tasks 4 and 5 both land. ✓
- Docs → Task 10. ✓

**Placeholder scan:** Task 4 Step 3 intentionally defers the exact status `Rect` to the implementer ("reuse the existing status Rect") because it must match live code; every other step carries concrete code. Adapter Tasks 7–9 are defensive-by-necessity (UNVERIFIED APIs) and say exactly what to probe and the guard shape.

**Type consistency:** `model: Option<String>` on both `RegistryEntry` (Task 1) and `Agent` (Task 2); `Update::SetModel(PathBuf, String)` (Task 2) matched by the watcher emit (Task 3); `parse_config_model(&str) -> Option<String>` (Task 3) consumed in `run`; `ui::render` gains `model: Option<&str>` (Task 4) fed by `a.model.as_deref()` (Tasks 4/5). Wire word `config_options_update`, category `model`, `currentValue` consistent across Tasks 3 and 6–9. Model string `"<provider>/<id>"` consistent throughout.
