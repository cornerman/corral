# Export Agent Message History (`o` key)

## Problem

corral's board watches every agent's ACP socket but never sees its actual
conversation: `crates/core/src/watch.rs` parses `state_update`, `tool_call`,
`session_info_update`, and `config_options_update`, and silently drops
`agent_message_chunk`/message content entirely. There is no way, from the
board, to look at what an agent actually said or was told.

ACP v1 already defines the exact mechanism for this: `session/load` restores a
session and replays its full prior history as `session/update` notifications
before responding (distinct from `session/resume`, which restores without
replay). corral's own `Known Limitations` section already names the gap:
corral-pi answers `session/load` with method-not-supported. This feature closes
that gap for the adapters where it is feasible, and wires it to the board as an
`o` key: dump the selected live agent's history to a JSON file and open it with
`xdg-open`.

v2's draft RFD proposes folding `session/load` into `session/resume` via a
`replayFrom` cursor. That RFD is unmerged (2026-07-02 draft) and `session/resume`
itself is not implemented anywhere in corral today, so this design targets the
real, shipped v1 method. If v2 lands, migrating is a mechanical rename inside
the one collector function; nothing in the board or the adapters' business
logic changes.

## Scope

- Live agents only. A dormant record has no running process to ask; reaching
  its history would mean launching it as a side effect of a passive "peek",
  which is a bigger and riskier action than this feature intends. `o` on a
  dormant card is a no-op.
- Adapter coverage: pi and opencode implement `session/load` for real (both
  hold the message history in-process). claude's sidecar implements it by
  reading Claude Code's on-disk per-session JSONL transcript file, marked
  UNVERIFIED like the rest of that adapter (unconfirmed in this repo). cursor
  has no API surface that exposes the Composer transcript at all, so it
  answers method-not-supported, consistent with its already-reduced feature
  set (no `session/cancel`, coarse state).
- TUI/GUI parity (hard rule): the key, the footer hint, the context-menu entry,
  and the fetch/write/open logic all live once in `corral-core`; both shells
  just call it.

## Protocol and Data Flow

```
board (operator's display)                   agent socket (pi/opencode/claude/cursor)
  o pressed on a live, selected card
    core::history::fetch_history(socket) --->  fresh connection:
      write initialize + session/load            initialize  -> reply
                                                  session/load -> replays history as
                                                                  session/update notifications,
                                                                  THEN replies to session/load
      <--- collects every session/update line
           arriving before the response
      <--- session/load response (or JSON-RPC
           error: method not found / other)
    on success: wrap + write temp .json,
      spawn `xdg-open <path>` detached
    on error/timeout: footer status line
      ("history not supported" / "no reply")
```

`fetch_history` opens a one-shot connection exactly like `watch.rs`'s seed
connection and `prompt.rs`'s delivery connection: write the two requests, then
read lines in a loop, routing each by shape:

- a `session/update` notification (no `id`) -> push its `params.update` value
  onto the collected list
- the reply to id 1 (the `session/load` request) -> stop; success or error
- anything else (e.g. the `initialize` reply, id 0) -> ignore

Bounded by a fixed timeout (5s, matching the order of magnitude of
`prompt.rs`'s `DRAIN_GRACE` but generous enough for a real replay of a long
session); on timeout, treat as failure ("no reply").

## corral-core

New module `crates/core/src/history.rs`:

- `pub fn fetch_history(socket: &Path, session_id: &str) -> Result<Vec<Value>, HistoryError>`
  — the connect/collect loop described above. `HistoryError` is a small enum
  (`Unsupported`, `Timeout`, `Io(io::Error)`) so the shells can print a
  specific footer message. Pure collection logic (given a byte stream) is
  unit-tested the same way `prompt.rs` tests are: a throwaway `UnixListener`
  thread plays a scripted response.
- `pub fn write_and_open(agent: &Agent, entries: Vec<Value>) -> io::Result<PathBuf>`
  — wraps `entries` plus `{sessionId, cwd, title, capturedAt}` into one JSON
  object, writes it to `std::env::temp_dir()` as
  `corral-history-<sessionId>-<unix_ts>.json`, then launches
  `setsid --fork xdg-open <path>` detached (same detach pattern as
  `launch.rs`, so the board never blocks on the viewer program) and returns the
  path. `xdg-open` runs on the operator's own display regardless of whether the
  source agent itself is hidden in a cage, which is why the board (not the
  agent) does the opening.

`crates/core/src/menu.rs`: add `MenuAction::History` to `ALL` (after `Message`,
before `Spawn` — inspection actions grouped before mutating ones; `Dismiss`
stays last) with `label() -> "history"`.

## Both Shells

- **Key:** `o` on a selected live card calls `fetch_history` then
  `write_and_open` on a background thread (network + timeout + file I/O must
  not block the render loop), reporting the outcome as the existing transient
  status-line mechanism each shell already has for other actions. `o` on a
  dormant or requires-action-but-not-live card does nothing (mirrors how other
  keys already degrade per card state).
- **Footer hint:** add `o history` to the key-hint string in both `ui.rs`
  (TUI) and the GUI footer, next to `m msg`.
- **Context menu:** add the `History` entry to both shells' right-click menu
  (`core::menu::MenuAction::ALL` already drives both, so this is automatic
  once the shells match on the new variant — each needs its dispatch arm).

## Adapter Changes

- **`extensions/corral-pi.ts`:** implement `session/load`. On request, iterate
  `ctx.sessionManager.getEntries()` (already used for the title fallback scan)
  and, for each entry, write the appropriate `session/update` notification
  (`agent_message_chunk` for a message entry, `tool_call` for a tool entry — a
  format overlap with the existing live-broadcast code, reasonably shared into
  a helper) directly to the requesting connection, flushing after each; only
  once every entry has been written does it send the `session/load` response.
  Order matters: notifications must be flushed before the reply, since
  `fetch_history` stops collecting the instant it sees the reply.
- **`extensions/corral-opencode.ts`:** implement `session/load` the same way,
  sourcing entries from the SDK's session message list (mirroring how
  `session/list` already uses `client.session.*`), replaying each as
  `agent_message_chunk`/`user_message_chunk`.
- **`extensions/corral-claude/sidecar.ts`:** implement `session/load` by
  reading Claude Code's per-session on-disk JSONL transcript, parsing each line
  and replaying it the same way. Marked UNVERIFIED in-file (file location and
  line format are assumed from public documentation, not confirmed against a
  real Claude Code install in this repo), consistent with the rest of that
  adapter's UNVERIFIED annotations.
- **`extensions/corral-cursor/`:** no change — it already answers
  method-not-supported for everything it does not implement; `session/load`
  falls into that bucket. Document explicitly in its README/AGENTS entry.

## Known Limitations (update)

Replace the existing bullet ("corral-pi answers session/new/session/load with
method-not-supported...") with: pi and opencode serve `session/load` with full
replay; claude serves it best-effort from its on-disk transcript (UNVERIFIED);
cursor does not support it (no transcript API). `session/new` remains
unsupported everywhere (out of scope here — no client needs to create a second
session on an existing agent).

## Testing

- `crates/core/src/history.rs`: unit tests for the collector against a
  scripted `UnixListener` (success with N notifications then a reply; a
  method-not-found error reply; a timeout with no reply at all), same harness
  style as `prompt.rs`'s tests.
- `crates/core/src/menu.rs`: extend the existing exhaustive
  order/label tests to cover `History`.
- VM e2e (hard rule): extend the pi and opencode scenarios in `nix/tests/` to
  press `o` (or invoke the equivalent board action) against a session that has
  taken at least one turn against the stub LLM, and assert a history file
  appears (the smoke test's `xdg-open` can be stubbed/no-op'd in the VM the
  same way other launch side effects already are, if a real opener is not
  available headless — follow whatever pattern the existing scenarios use for
  launch verification). claude/cursor scenarios assert the method-not-supported
  status line instead.

## Out of Scope

- `session/new` (unrelated method, no current use case).
- Getting ahead of the v2 `session/resume`+`replayFrom` RFD — it is an
  unmerged draft and `session/resume` is not implemented anywhere in corral
  yet; revisit once v2 stabilizes (mechanical change confined to
  `history.rs`).
- Any dormant-agent path (see Scope).
- A rendered/human-formatted transcript view — v1 ships the raw JSON only.
