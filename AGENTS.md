# corral — Architecture and Setup

This file and README.md MUST always be kept up to date when the setup,
architecture, or interfaces change.

## What This Is

An attention board for locally running ACP agent sessions, plus the discovery
convention it rides on. Design premise: the user launches agents in arbitrary
terminals, never through a manager. Corral shows which agent needs attention
and jumps the user to its real window; it never drives an agent.

Agent-agnostic by design; pi is the current proof of concept, announced via the
`corral-announce` extension. The board discovers any `<label>-<pid>.sock` and
reads agent identity generically, so other ACP agents are a matter of giving
them a way to announce (see Future).

Discovery works through a filesystem convention, not a registry service:

```
$XDG_RUNTIME_DIR/acp/pi-<pid>.sock   (directory mode 0700)
```

A pi extension binds a socket there and unlinks it on exit. The filesystem is
the registry: `ls` enumerates sessions, connecting to a socket talks plain ACP
(JSON-RPC, newline-delimited, as on stdio) to that agent.

## Data Flow

```
your terminal (pi, interactive TUI)              another terminal
  pi -e extensions/corral-announce.ts              corral (attention board)
    |  binds $XDG_RUNTIME_DIR/acp/pi-<pid>.sock      |  scans $XDG_RUNTIME_DIR/acp/ (1s)
    |    on session_start                            |  one watch connection per socket:
    |  serves ACP beside the live TUI:               |    initialize + session/list (seed)
    |    initialize, session/list, prompt, cancel    |    streams state_update -> column
    |  broadcasts activity + state_update            |  Enter -> focus window (sway)
    |  unlinks socket on session_shutdown            |  n -> spawn agent (kitty)
```

## Crates

- `crates/board` — the attention board (binary name `corral`). The triage core
  (`model`, `watch`, `ui`) never names sway or kitty; both sit behind traits.
  - `src/discovery.rs` — scan the socket dir, parse `<label>-<pid>.sock`
    (pure, unit-tested).
  - `src/model.rs` — `Agent` (keyed by socket path) and `Board`, the state the
    UI renders; `State` is Running, Idle, or RequiresAction (the ACP v2
    `state_update` vocabulary). Pure, unit-tested.
  - `src/watch.rs` — one reader thread per socket. Connects (stays fully open,
    never half-closes), seeds from `initialize` + `session/list`, then streams
    `state_update` notifications. Socket EOF reports the agent gone. Pure
    parse helpers are unit-tested.
  - `src/focus.rs` — `WindowFocuser` seam. `SwayFocuser` correlates agent to
    window by a `/proc` parent-walk: the socket pid, walked up its PPid chain,
    hits the kitty process whose pid sway reports (works because the pi sandbox
    does not unshare the PID namespace), then `swaymsg [con_id=..] focus`. The
    tree walk is unit-tested.
  - `src/launch.rs` — `Launcher` seam. `KittyLauncher` runs `kitty -e pi`.
  - `src/ui.rs` — ratatui: three columns (Requires Action, Idle, Running) in
    attention priority, plus a help footer.
  - `src/main.rs` — orchestration: scan, spawn watchers, drain updates, handle
    keys (Up/Down select, Enter focus, `n` spawn, `q` quit).

## Extensions

- `extensions/corral-announce.ts` — pi extension announcing an interactive pi
  session on the socket dir. Serves `initialize`, `session/list` (id, title,
  cwd), `session/prompt` (injects via `pi.sendUserMessage`; queued as follow-up
  while busy; responds with stopReason once the message queue drains, coarse,
  documented in-file), `session/cancel` -> abort. Broadcasts to all connected
  clients: `session/update` message and tool events (whole messages on
  `message_end`; token deltas deferred), `session_info_update` on rename; and
  the standard `state_update` (running/idle/requires_action) on
  `turn_start`/`turn_end` and while the interactive `question` tool blocks on
  the user. A newly connected client is seeded with the current `state_update`.
  Serves multiple concurrent clients. Install: symlink into
  `~/.pi/agent/extensions/`.

## ACP Conformance

Corral tracks the ACP v2 Prompt Lifecycle RFD
(agentclientprotocol.com/rfds/v2/prompt), which adds a `state_update`
session/update with `running` / `idle` / `requires_action`, broadcast by the
agent to every client (not just the prompt sender). corral-announce emits that
exact shape and vocabulary now, ahead of stabilization, so there is zero
migration when v2 lands and any future ACP agent works unchanged. Tradeoff: a
strict v1-only client that rejects unknown `sessionUpdate` variants would not
recognize `state_update` until v2; acceptable because corral is the consumer
here. The rest of the surface (initialize, session/list, prompt, cancel,
message/tool updates) is ACP v1.

## Interfaces to the Outside World

- CLI `corral` — full-screen TUI. Keys: Up/Down (or j/k) select, Enter focus
  the selected agent's window, `n` spawn a new agent, `q`/Esc quit. Requires
  `$XDG_RUNTIME_DIR`; uses `swaymsg` and `kitty` for focus and spawn.
- pi extension `corral-announce` — see Extensions above.
- Unix sockets in `$XDG_RUNTIME_DIR/acp/` (created 0700). No TCP ports, no
  network exposure. Peer authentication relies on the directory permissions.

## Known Limitations (v1, deliberate)

- `requires_action` is emitted today only for the interactive `question` tool
  (the one user-input gate an extension can observe). pi's built-in
  tool-approval prompt is not surfaced to extensions, so an approval-blocked
  agent still shows as Running until pi exposes that gate (see Future).
- Focus correlation assumes the pi process and its terminal window share the
  host PID namespace (true under the current nono/bwrap sandbox). If a sandbox
  unshares PIDs, the `/proc` parent-walk cannot reach the window pid.
- A transient watch read error reports the agent gone; the next 1s scan
  reconnects. A genuinely dead socket (crashed pi) reconnects-and-drops cheaply
  once per second until its file disappears.
- corral-announce answers `session/new`/`session/load` with method-not-
  supported: clients can discover, watch, and prompt running pi sessions, but
  attaching with history replay is not yet served.
- corral-announce's `session/prompt` responses resolve for all waiting clients
  at once when the queue drains (no per-message turn attribution).
- Approvals stay in the pi TUI; socket clients never receive
  `session/request_permission`.
- Agent-to-agent communication (opening a channel between two agents) is a
  planned later layer, not built.

## Future

- More than pi. Today only pi announces, via `corral-announce`. Other ACP
  agents (Claude Code, codex, gemini) are a planned direction: each needs a way
  to bind a `<label>-<pid>.sock` (its own extension, or a stdio-to-socket
  wrapper), after which the board discovers it unchanged. Agents that do not
  emit `state_update` simply default to Idle.
- Full requires_action coverage. pi core (or a native ACP `state_update`
  implementation in pi) emitting a signal whenever any `ctx.ui.*` prompt opens
  (approvals, select, input, elicitation), so the board catches every
  user-input gate, not just the `question` tool. This is the platform-side
  companion to corral's display, and the standard end-state per the ACP v2
  RFD.
- Agent-to-agent channels: corral brokering a link so two agents can talk.
- More than sway and kitty. `SwayFocuser` and `KittyLauncher` are the PoC
  implementations for the maintainer's setup; other compositors and terminals
  drop in as new `WindowFocuser` / `Launcher` implementations behind the same
  seams, with no change to the triage core.

## Development Setup

- Nix flake (nixpkgs-unstable) + direnv; Rust pinned via rust-toolchain.toml
  through rust-overlay. `just` for commands: `just test`, `just lint`,
  `just board`, `just watch` (cargo-watch), `just nix-build`.
- CI: GitHub Action runs `nix flake check` (build + tests via nix).
