# corral — Architecture and Setup

This file and README.md MUST always be kept up to date when the setup,
architecture, or interfaces change.

## What This Is

An attention board for locally running ACP agent sessions, plus the discovery
convention it rides on. Design premise: the user launches agents in arbitrary
terminals, never through a manager. Corral shows which agent needs attention
and jumps the user to its real window. It never drives an agent autonomously;
the operator may deliver a message to a selected agent (`m`), which corral
injects over that agent's socket.

Agent-agnostic by design; pi is the current proof of concept, announced via the
`corral-announce` extension. The board reads agent identity generically, so
other ACP agents are a matter of giving them a way to announce (see Future).

Discovery works through a per-session registry on the filesystem, not a
registry service. One record per session names a workdir-local socket:

```
$HOME/.corral/registry/<sessionId>.json   (dir 0700; override $CORRAL_REGISTRY_DIR)
  { sessionId, cwd, title, label, socket, resume, lastSeen }
<cwd>/.corral/<label>-<pid>.sock           (dir 0700; override $CORRAL_SOCKET_DIR)
```

A pi extension writes the record and binds the socket on `session_start`, then
on clean shutdown unlinks the socket and clears the record's `socket` to null
(leaving a dormant, resumable entry). The socket lives inside the session's own
working directory, so only that session (and unsandboxed tools like corral) can
reach it; that workdir-local isolation is the primitive a later messaging layer
relies on. Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it. Corral scans
the registry to find each socket (it could never scan the scattered workdirs
directly), then talks plain ACP (JSON-RPC, newline-delimited, as on stdio) to
that agent. A record with `socket == null` is dormant (rendering dormant
sessions is a later stage).

## Data Flow

```
your terminal (pi, interactive TUI)              another terminal
  pi -e extensions/corral-announce.ts              corral (attention board)
    |  writes ~/.corral/registry/<id>.json           |  scans the registry (1s)
    |  binds <cwd>/.corral/pi-<pid>.sock              |  one watch connection per live socket:
    |    on session_start                            |    initialize + session/list (seed)
    |  serves ACP beside the live TUI:               |    streams state_update -> column
    |    initialize, session/list, prompt, cancel    |  Enter -> focus (sway) or resume
    |  broadcasts activity + state_update            |  n -> spawn agent (kitty)
    |  clears socket + unlinks on session_shutdown   |  m -> send prompt to agent
    |                                                |
  message_agent tool -> ~/.corral/outbox/<id>.json --+  routes each mailbox file:
    (another agent asks to message a target dir)         authorize (whitelist +
                                                         operator popup), resolve
                                                         target dir (spawn if none),
                                                         inject with provenance tag
```

## Crates

- `crates/board` — the attention board (binary name `corral`). The triage core
  (`model`, `watch`, `ui`) never names sway or kitty; both sit behind traits.
  - `src/discovery.rs` — scan the registry dir, parse each `<sessionId>.json`
    record, and resolve a live record to its socket (parsing the
    `<label>-<pid>.sock` filename). Pure, unit-tested.
  - `src/model.rs` — `Agent` (`Origin` Live or Dormant) and `Board`. Live
    agents are keyed by socket path (driven by watchers); dormant agents are a
    derived view of the registry (cleanly shut-down, resumable, not-live
    records, latest-per-cwd). `State` is Running, Idle, or RequiresAction (the
    ACP v2 `state_update` vocabulary). Pure, unit-tested.
  - `src/watch.rs` — one reader thread per socket. Connects (stays fully open,
    never half-closes), seeds from `initialize` + `session/list`, then streams
    `state_update` notifications. Socket EOF reports the agent gone. Pure
    parse helpers are unit-tested.
  - `src/focus.rs` — `WindowFocuser` seam. `SwayFocuser` correlates agent to
    window by a `/proc` parent-walk: the socket pid, walked up its PPid chain,
    hits the kitty process whose pid sway reports (works because the pi sandbox
    does not unshare the PID namespace), then `swaymsg [con_id=..] focus`. The
    tree walk is unit-tested.
  - `src/launch.rs` — `Launcher` seam. `KittyLauncher` runs `kitty -e pi`
    (spawn) or `kitty -e pi --session <path>` (resume a dormant session).
  - `src/prompt.rs` — `send_prompt`: deliver a user message to a live agent by
    opening a one-shot connection to its socket and writing a `session/prompt`
    request (fire-and-forget). Unit-tested against a throwaway listener.
  - `src/mailbox.rs` — the outbox: parse `message_agent` mailbox files, add the
    `[from agent in <dir>]` provenance tag, and read/append the
    `(sender -> target)` whitelist. Pure, unit-tested.
  - `src/ui.rs` — ratatui: four columns (Requires Action, Idle, Running,
    Dormant) in attention priority, plus a help footer. Dormant cards dimmed.
  - `src/picker.rs` — the `c` spawn directory picker: candidate dirs (board
    cwds + `$CORRAL_PROJECT_ROOTS` subdirs, default ~/projects) and a
    subsequence fuzzy filter. Unit-tested.
  - `src/main.rs` — orchestration: scan + prune the registry, spawn watchers,
    rebuild the dormant view, drain updates, handle keys and mouse (Up/Down
    within a column, Left/Right across columns, Enter or click focus/resume,
    `m` message a live agent, `n`/`c` spawn, `d` dismiss dormant, `q` quit).

## Extensions

- `extensions/corral-announce.ts` — pi extension announcing an interactive pi
  session: on `session_start` it writes the registry record and binds the
  workdir-local socket; on `session_shutdown` it clears the record's `socket`
  and unlinks. The record's `lastSeen` refreshes on `turn_end` and its `title`
  on rename. Serves `initialize`, `session/list` (id, title,
  cwd), `session/prompt` (injects via `pi.sendUserMessage`; queued as follow-up
  while busy; responds with stopReason once the message queue drains, coarse,
  documented in-file), `session/cancel` -> abort. Broadcasts to all connected
  clients: `session/update` message and tool events (whole messages on
  `message_end`; token deltas deferred), `session_info_update` on rename; and
  the standard `state_update` (running/idle/requires_action) on
  `turn_start`/`turn_end` and while the interactive `question` tool blocks on
  the user. A newly connected client is seeded with the current `state_update`.
  Serves multiple concurrent clients. Also registers a `message_agent` tool
  (`target_dir`, `message`, `force_new`) that queues a cross-session message as
  `~/.corral/outbox/<id>.json` for corral to route. Install: symlink into
  `~/.pi/agent/extensions/`.

## Inter-Agent Messaging

Sandboxed agents cannot reach each other's sockets (each is workdir-local), so
corral is the sole trusted cross-workdir router. An agent calls `message_agent`,
which drops a mailbox file in `~/.corral/outbox`; corral picks it up on the next
tick, authorizes the `(sender-dir -> target-dir)` pair against the whitelist (or
asks the operator: `a` allow once, `A` allow always, `d` deny, `esc` later),
resolves the target directory to a live agent (spawning one there if none runs,
or a dedicated one for `force_new`), and injects the message over that agent's
socket with a `[from agent in <dir>]` provenance tag. Delivery reuses
`prompt::send_prompt`, the same path as operator messaging (`m`). Fire-and-
forget: no reply is routed back (a response channel is a planned v2).

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

- CLI `corral` — full-screen TUI, four columns: Requires Action, Idle, Running,
  Dormant. Up/Down (or j/k, or scroll) move within a column; Left/Right (or
  h/l) switch columns; Enter or left-click focuses a live agent's window or
  resumes a dormant session (`pi --session`); `m` compose a message delivered
  to a live agent over its socket; `n` spawn in the selected agent's
  cwd; `c` open a fuzzy directory picker to create one elsewhere; `f` fuzzy-focus a
  live agent by title/cwd; `d` dismiss the selected dormant record; `q`/Esc
  quit. Long columns scroll to keep the selection visible; live cards show
  time-in-state. Reads `$HOME` (or
  `$CORRAL_REGISTRY_DIR`) for the registry dir and `$CORRAL_PROJECT_ROOTS`
  (colon-separated, default `~/projects`) for picker candidates; uses `swaymsg`
  and `kitty` for focus and spawn.
- pi extension `corral-announce` — see Extensions above.
- Registry records in `$HOME/.corral/registry/` and unix sockets in each
  `<cwd>/.corral/` (both created 0700; override with `$CORRAL_REGISTRY_DIR` /
  `$CORRAL_SOCKET_DIR`). No TCP ports, no network exposure. Peer authentication
  relies on the directory permissions.
- Inter-agent messaging: `message_agent` writes `$HOME/.corral/outbox/` (override
  `$CORRAL_OUTBOX_DIR`); corral authorizes `(sender -> target)` dir pairs against
  `$HOME/.corral/whitelist` (override `$CORRAL_WHITELIST`) plus an operator popup.

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
- Inter-agent messaging is fire-and-forget (v1): corral injects the message and
  does not capture a reply back to the sender. A response channel is a clean v2.
- Delivery policy when the target dir's agent is Running: v1 reuses it and lets
  the extension queue the message as a follow-up (it can intrude on a
  human-driven session; the provenance tag makes that visible). Alternatives
  (never-inject-Running, always-new) are deferred until real use decides.
- `force_new` targets the agent that appears after corral's spawn (a socket not
  present before it); if several agents start in one dir at once the newcomer
  is picked arbitrarily. Adequate for v1.
- Each project dir where pi runs gains a `<cwd>/.corral/` holding the session
  socket. Deliberate: workdir-local is the sandbox-isolation primitive. Add it
  to a global gitignore if the stray dir bothers you.
- Dormant sessions render as dormant (latest-per-cwd), resume on Enter, dismiss
  on `d`, and are pruned when their session file is gone or the record is >14
  days stale. A crashed session (no clean shutdown, so its registry `socket`
  stays set) is caught by a staleness sweep: the board records sockets whose
  watcher fails to connect (`dead_sockets`) and treats a dead-socketed record
  as dormant, so a crashed agent stays resumable instead of vanishing. A
  freshly starting session (socket set, not yet proven dead) stays on the live
  path and never flickers through the Dormant column.

## Future

- More than pi. The board core is agent-agnostic; the only pi-specific piece is
  the `corral-announce` adapter. The stable contract any ACP agent joins by:
  (1) write `~/.corral/registry/<sessionId>.json` with `label` set to the agent
  kind and `socket` pointing at (2) a workdir-local `<label>-<pid>.sock`
  speaking ACP (initialize, session/list, session/prompt), and (3) broadcast
  `state_update`. A non-cooperating agent can be wrapped by a generic
  stdio-to-socket-plus-registry shim instead of a bespoke extension. Missing
  `state_update` just defaults the card to Idle; missing `label` defaults it to
  `pi`.
- Full requires_action coverage. pi core (or a native ACP `state_update`
  implementation in pi) emitting a signal whenever any `ctx.ui.*` prompt opens
  (approvals, select, input, elicitation), so the board catches every
  user-input gate, not just the `question` tool. This is the platform-side
  companion to corral's display, and the standard end-state per the ACP v2
  RFD.
- More than sway and kitty. `SwayFocuser` and `KittyLauncher` are the PoC
  implementations for the maintainer's setup; other compositors and terminals
  drop in as new `WindowFocuser` / `Launcher` implementations behind the same
  seams, with no change to the triage core.

## Development Setup

- Nix flake (nixpkgs-unstable) + direnv; Rust pinned via rust-toolchain.toml
  through rust-overlay. `just` for commands: `just test`, `just lint`,
  `just board`, `just watch` (cargo-watch), `just nix-build`.
- CI: GitHub Action runs `nix flake check` (build + tests via nix).
