# Corral Attention Board (v1)

## Purpose

Corral becomes a simple TUI that shows every running pi agent as a card in one
of three columns, Requires Action, Idle, or Running, so the operator sees at a
glance which agent is blocked on them. Enter or a left-click focuses that
agent's real window, `n` spawns a new agent, `q` quits. Corral previews and routes attention; it never
drives an agent (no prompt or cancel from the board). All interaction happens in
the real pi window after the operator jumps to it.

The scarce resource when running many parallel agents is operator attention. The
board's one job is to point it at the agent that needs it.

## Non-Goals (v1)

- Agent-to-agent communication channel (planned later; the architecture must not
  preclude it, but nothing is built now).
- Full requires_action coverage. `requires_action` is emitted today only for
  the interactive `question` tool; pi's built-in tool-approval prompt is not
  observable by an extension, so an approval-blocked agent shows as Running
  until pi exposes that gate (the platform-side companion, tracked in Future).
- Any in-board control of agents (prompt, steer, cancel, spawn-with-prompt).

Proof-of-concept scope, all swappable later behind existing seams: pi is the
only announced agent (via corral-announce), sway the only WindowFocuser, kitty
the only Launcher. Other ACP agents, compositors, and terminals are future
directions, not built now.

## State Model

Three states per agent, adopting the ACP v2 `state_update` vocabulary
(agentclientprotocol.com/rfds/v2/prompt):

- `running`: a turn is in progress (`turn_start` seen, no `turn_end` yet).
- `idle`: the turn ended, waiting for the next prompt.
- `requires_action`: blocked, needs user input to continue.

The board columns run in attention priority: Requires Action, Idle, Running.
`running`/`idle` come from turn events. `requires_action` is emitted while the
interactive `question` tool blocks on the user (the one input gate an extension
can see today); full coverage of pi's approval/elicitation prompts is the
platform-side follow-up in Future.

## Identity and Focus (Resolved Design Decisions)

- Agent identity key: the socket path (`$HOME/.corral/sockets/pi-<pid>.sock`,
  override the dir with `$CORRAL_ACP_DIR`; not `$XDG_RUNTIME_DIR`, which
  sandboxed agents cannot reach). The
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

One addition to the existing extension: track state and broadcast it.

- `turn_start` -> `running`; `turn_end` -> `idle`.
- While the `question` tool runs (`tool_execution_start`/`_end` for that tool),
  `requires_action`.
- Broadcast each transition as the standard `state_update` session/update, and
  seed a newly connected client with the current `state_update` so it can
  column immediately. `session/list` stays a stock `SessionInfo` (no state
  field).

Everything else in the extension already exists (socket bind, initialize,
session/list, prompt, cancel, message and tool broadcasts).

### ACP Conformance

corral tracks the ACP v2 Prompt Lifecycle RFD
(agentclientprotocol.com/rfds/v2/prompt), which adds a `state_update`
session/update with `running` / `idle` / `requires_action`, broadcast by the
agent to every connected client rather than only the prompt sender. That is the
first ACP mechanism giving a passive observer the turn state, and it converges
on corral's own model. corral-announce emits that exact shape and vocabulary now,
ahead of stabilization, so there is zero migration when v2 lands and any future
ACP agent works unchanged. Tradeoff: a strict v1-only client that rejects
unknown `sessionUpdate` variants would not recognize `state_update` until v2;
acceptable because corral is the consumer here. The rest of the surface is ACP
v1. This supersedes an earlier vendor-notification design (`_corral/state` +
`_meta`): relying on the emerging standard is preferred over a custom channel.

### Board Crate (crates/board)

New crate. The old `corral` crate and the obsolete `agentwrap` crate are removed;
the useful socket-scan and parse logic from the old crate is carried over.

Modules, each with one clear job:

- `discovery`: scan `$HOME/.corral/sockets/` (or `$CORRAL_ACP_DIR`) for `pi-<pid>.sock`, parse
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
- `ui`: ratatui. Three columns of cards (label, title, cwd) in attention
  priority. Up/Down or scroll select, Enter or left-click focuses via the seam,
  `n` spawns via the seam, `q` quits. Redraws on channel events.

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

- Missing `$HOME` and no `$CORRAL_ACP_DIR`: exit with a clear message.
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
