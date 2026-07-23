# Context Size Footer — Design

## Problem

The board shows no indication of how large or old a session's chat history is.
The operator wants a quick read, for the currently selected card, of how big
its context is and how long it has been running — without the metric being
relative to any one model's context window (different models have different
window sizes, so a cross-session "biggest" ranking on percent-of-window would
not be comparable).

## Scope

Display-only. Extends the existing footer line that already shows
`model: <x>` for the selected card, in both shells (TUI and GUI — parity is a
hard rule per `AGENTS.md`). Does not add sorting, columns, badges on
unselected cards, or any new interaction.

v1 ships for **pi only**. Other adapters (opencode, claude, cursor) keep
showing exactly today's `model: <x>` in this slot — the same incremental,
per-adapter pattern corral already uses for the `model` field itself (probed
defensively per adapter, absent where the harness cannot supply it).

## Data Source (pi)

Three numbers, all obtainable from pi's existing extension API — no new
dependency in either the extension or Rust core:

- **entries** — `ctx.sessionManager.getEntries().length`. Counts every
  session-log entry (messages, tool calls, custom entries), an honest size
  proxy rather than a narrower "chat message count" that would undercount
  tool-heavy turns.
- **context percent** — `ctx.getContextUsage()` (`{ tokens, contextWindow,
  percent }`, a real, documented pi API). Only `percent` crosses into corral;
  raw `tokens`/`contextWindow` stay internal to the extension, since the board
  never needs them.
- **age** — `Date.now() - Date.parse(entries[0].timestamp)`, where
  `entries[0]` is the session file's own creation entry (`session-format.md`:
  the first logged entry carries the session's creation timestamp). Durable
  across a pi restart/resume without corral persisting its own start-time
  field, since it is read back from the transcript itself every time.

Age is formatted into a compact string (`"3d"`, `"42m"`) in the extension
using the same s/m/h/d unit scale `core::engine::age_label` already uses on
the Rust side; kept as an independent implementation since the two live in
different languages, and TS's `Date` arithmetic is trivial enough that
sharing code across the process boundary is not worth the interface.

## Wire Format

A new custom `session/update` broadcast, on the same footing as
`state_update`/`config_options_update` (corral-pi's own vocabulary, ahead of
any ACP stabilization):

```json
{ "sessionUpdate": "context_update", "entries": 42, "percent": 12, "age": "3d" }
```

`percent` is `null`/omitted when pi's own `getContextUsage()` returns an
unknown token estimate (e.g. immediately after compaction).

Broadcast at the same points `state_update` already fires (`turn_start` /
`turn_end`), plus seeded to a newly connected client — reusing the existing
connect-race handling in `watch.rs` (a value arriving before the seeding
`session/list` reply is stashed and stamped onto the seeding `Upsert`, the
same pattern already used for state and model).

The registry record persists the last-known `entries` / `contextPercent` /
`contextAge`, exactly like the existing `model` field, so a **dormant** pi
card still shows its last reading instead of going blank.

## Rust Core Changes

- `crates/core/src/discovery.rs`: `RegistryEntry` gains `entries: Option<u64>`,
  `context_percent: Option<u32>`, `context_age: Option<String>`.
- `crates/core/src/watch.rs`: `parse_config_context(line) -> Option<ContextInfo>`,
  analogous to the existing `parse_config_model`; a new `Update::SetContext(PathBuf,
  ContextInfo)` variant carrying the three fields.
- `crates/core/src/model.rs`: `Agent` gains the same three fields;
  `Board::apply` handles `SetContext`; `sync_registry` stamps the record's
  last-known values onto dormant agents and onto live agents only when still
  `None` — the identical pattern already used for `model`.
- Unit tests mirror the existing model-field tests: wire parsing, the
  connect-race seed, and `sync_registry` stamping (dormant + live-if-none).

## Footer Rendering (TUI + GUI)

The selected card's footer row, replacing today's `"model: <x>"`:

- pi with data: `"12% ctx · 42 entries · 3d · model: claude-opus-4"`
- percent unknown (e.g. right after compaction): `"42 entries · 3d · model: claude-opus-4"`
  (the percent segment is simply omitted for that tick, not shown as a
  placeholder)
- any other adapter, or a pi session before its first `context_update`:
  unchanged `"model: <x>"` — the whole new group is gated on
  `agent.entries.is_some()`

Implemented once in `crates/core` formatting shared by both shells where
practical (mirroring how the existing model line already renders
identically), so TUI/GUI parity cannot drift.

## Out of Scope (v1)

- opencode / claude / cursor support (no equivalent introspection API
  surfaced today; deferred until/unless one exists)
- Any cross-session "biggest" ranking, sort, badge, or column — this is a
  per-selected-card readout only
- Persisting raw token counts or the context window size (only the derived
  percent crosses into corral)
