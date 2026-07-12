# Corral Attention Board (v1)

## Purpose

Corral becomes a simple TUI that shows every running pi agent as a card in one
of two columns, Working or Needs You, so the operator sees at a glance which
agent is idle and waiting for input. Enter focuses that agent's real window, `n`
spawns a new agent, `q` quits. Corral previews and routes attention; it never
drives an agent (no prompt or cancel from the board). All interaction happens in
the real pi window after the operator jumps to it.

The scarce resource when running many parallel agents is operator attention. The
board's one job is to point it at the agent that needs it.

## Non-Goals (v1)

- Agent-to-agent communication channel (planned later; the architecture must not
  preclude it, but nothing is built now).
- An approval-blocked column. Pi's built-in tool-approval prompt is not
  observable by an extension without pi support, so it is not detectable
  reliably. Approval-blocked folds into Working for now.
- Any in-board control of agents (prompt, steer, cancel, spawn-with-prompt).

Proof-of-concept scope, all swappable later behind existing seams: pi is the
only announced agent (via corral-announce), sway the only WindowFocuser, kitty
the only Launcher. Other ACP agents, compositors, and terminals are future
directions, not built now.

## State Model

Two states per agent, derived from pi turn events:

- Working: a turn is in progress (`turn_start` seen, no `turn_end` yet).
- Needs You: the agent is idle (last event was `turn_end`), waiting for input.

Both are fully detectable today. No pi changes required.

## Identity and Focus (Resolved Design Decisions)

- Agent identity key: the socket path (`$XDG_RUNTIME_DIR/acp/pi-<pid>.sock`). The
  filesystem registry is the identity. One socket is one live agent; a restarted
  pi gets a new socket, correctly a new card. Identity needs no `session/list`.
- sessionId, title, cwd, state are displayed metadata only. Nothing is persisted
  to disk; corral rediscovers everything live on each run.
- Window focus uses a `/proc` parent-walk: take the socket's pid, walk the `PPid`
  chain, and the ancestor pid that appears in `swaymsg -t get_tree` is the kitty
  window hosting the agent. Focus that container by con id. This works because
  the pi sandbox (nono via bwrap) does not unshare the PID namespace, so the
  socket pid and the window pid share the host namespace. Zero storage, no
  extension change. Title matching is a documented fallback only.

## Architecture

### Extension Change (corral-announce.ts)

One addition to the existing extension: track Working/Needs-You and expose it.

- On `turn_start` set internal state to working; on `turn_end` set idle.
- Report state per session in `session/list` under `SessionInfo._meta`
  (`_meta["corral/state"]`).
- Broadcast the transition as a vendor ExtNotification `_corral/state` on each
  change, so a watching client learns transitions without polling.

Everything else in the extension already exists (socket bind, initialize,
session/list, prompt, cancel, message and tool broadcasts).

### ACP Conformance

Working/idle is not part of ACP. The `sessionUpdate` union is closed
(user_message_chunk, agent_message_chunk, agent_thought_chunk, tool_call,
tool_call_update, plan/plan_update/plan_removed, available_commands_update,
current_mode_update, config_option_update, session_info_update, usage_update),
and ACP signals turn end only via the `PromptResponse` `stopReason` to the
client that sent the prompt. Corral is a passive observer, so it needs an
out-of-band signal. Rather than add a bogus `sessionUpdate` variant (which a
strict client such as Zed would reject), state rides ACP's sanctioned vendor
seams: a `_corral/state` ExtNotification (unknown notifications are ignored by
conformant clients) and `SessionInfo._meta` in `session/list`. The rest of the
socket surface stays ACP-conformant.

### Board Crate (crates/board)

New crate. The old `corral` crate and the obsolete `agentwrap` crate are removed;
the useful socket-scan and parse logic from the old crate is carried over.

Modules, each with one clear job:

- `discovery`: scan `$XDG_RUNTIME_DIR/acp/` for `pi-<pid>.sock`, parse
  `<label>-<pid>`. Carried over from the old corral crate with its tests.
- `model`: `Agent { socket_path (key), pid, label, session_id, title, cwd,
  state }` and the shared board state (map keyed by socket path).
- `watch`: one reader thread per socket. Connects (stays fully open, never
  half-closes), sends `initialize` + `session/list` to seed title/cwd/state,
  then streams `session/update` notifications and updates the agent's state.
  Socket EOF removes the agent. Reader threads push changes to the UI thread
  over a channel.
- `focus` (seam): trait `WindowFocuser { fn focus(&self, agent: &Agent) }`. Sway
  implementation does the `/proc` parent-walk plus `swaymsg [con_id=..] focus`.
- `launch` (seam): trait `Launcher { fn spawn(&self, cwd: &Path) }`. Kitty
  implementation runs `kitty -e pi`.
- `ui`: ratatui. Two columns of cards (label, title, cwd). Up/Down select,
  Enter focuses via the seam, `n` spawns via the seam, `q` quits. Redraws on
  channel events.

The triage core (model, watch, ui) never names sway or kitty; both live behind
their seams, so the compositor and terminal can change without touching the
core, and tests substitute fakes.

## Data Flow

Synchronous, thread-based (small N of agents, no async runtime, KISS). Main
scans the acp directory every second. Each newly seen socket gets a reader
thread; each vanished socket's thread ends and its card is removed. Reader
threads seed from `initialize` + `session/list`, then stream `session/update`
and send model deltas over a channel to the single UI thread, which owns
rendering and input.

## Error Handling

- Missing `$XDG_RUNTIME_DIR`: exit with a clear message (unchanged from today).
- A socket that refuses connection or closes immediately: treat the agent as
  gone, drop the card. No stale rows.
- Focus with no matching sway window: no-op with a brief status line, never a
  crash.
- Spawn failure (kitty missing): surface the error in the status line.

## Testing

- Extension: extend the existing fake-pi harness so `turn_start` and `turn_end`
  produce the state broadcast and the `session/list` state field.
- Board: pure-function tests for socket parsing (carried over) and for the
  `session/update` to state transition. A fake unix-socket server emitting
  notifications drives the model end to end. `WindowFocuser` and `Launcher` are
  faked, so core tests need neither sway nor kitty.

## Repo Restructuring (One-Time)

Done while the sandbox is off, because it edits outside the new crate:

- Remove `crates/agentwrap` (obsolete; the extension replaced the wrapper).
- Remove `crates/corral` (the polling-table prototype), carrying `discovery`
  and its tests into `crates/board`.
- Rewrite the workspace `Cargo.toml` members to `["crates/board"]`.

After this, the pi sandbox confines ongoing board development to
`$HOME/projects/corral/crates/board` (writable), with the rest of the repo
read-only.
