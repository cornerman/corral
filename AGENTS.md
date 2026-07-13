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

The registry record, workdir-local socket, and ACP surface are a harness-neutral
convention specified in `CONVENTION.md` (implement from that alone, no source
reading). This section is the architecture view of the same contract.

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
    |  broadcasts activity + state_update            |  shift+enter -> spawn agent (kitty)
    |  clears socket + unlinks on session_shutdown   |  m -> send prompt to agent
    |                                                |
  corral_message_agent tool -> ~/.corral/corrald.sock ----+  per submission (control.rs):
    (asks to message a target dir or session)            parse, find recipient,
    <- ack: accepted / blocked /                         ack the verdict, then
       recipient_not_found / directory_not_known         enqueue to the router:
                                                         authorize (whitelist +
                                                         operator popup), resolve
                                                         target dir/session (spawn
                                                         or resume if needed),
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
    records, one card per session, newest first). `State` is Running, Idle, or RequiresAction (the
    ACP v2 `state_update` vocabulary). `Column::ALL` is the single source of
    truth for the column set and order; navigation, hit-testing, and rendering
    all derive from it. Pure, unit-tested.
  - `src/watch.rs` — one reader thread per socket. Connects (stays fully open,
    never half-closes), seeds from `initialize` + `session/list`, then streams
    `state_update` notifications, plus `session_info_update` (title) and
    `tool_call` broadcasts, the latter summarized into a short card activity
    string ("edit model.rs"). Socket EOF reports the agent gone. Pure parse
    helpers are unit-tested.
  - `src/focus.rs` — `WindowFocuser` seam (focus and close a window).
    `SwayFocuser` correlates agent to window by a `/proc` parent-walk: the
    socket pid, walked up its PPid chain, hits the terminal process whose pid
    sway reports (works because the pi sandbox does not unshare the PID
    namespace). `focus` then runs `swaymsg [con_id=..] focus`; `close` kills
    that terminal pid (not a window-close request, which kitty's
    `confirm_os_window_close` would block). The tree walk is unit-tested.
  - `src/launch.rs` — `Launcher` seam. `KittyLauncher` runs `kitty -e pi`
    (spawn) or `kitty -e pi --session <path>` (resume a dormant session),
    always via `setsid --fork` so the window is detached from corral (survives
    the board exiting, no zombie, and — since it is not a descendant of corral
    — the focus parent-walk cannot climb into the board's own window). Both
    take an optional initial `message` submitted as pi's first prompt (a
    positional arg), so a message can be delivered atomically at launch. pi's
    parser has no `--` marker and treats a leading `-`/`@` as a flag/file, so
    such a message is space-guarded (pi trims it); `pi_args` is unit-tested.
  - `src/prompt.rs` — `send_prompt`: deliver a user message to a live agent by
    opening a one-shot connection to its socket and writing a `session/prompt`
    request (fire-and-forget). Unit-tested against a throwaway listener.
  - `src/mailbox.rs` — cross-session message types: parse a submitted message (a
    `Target` is a directory or an exact session id), `classify` it into an `Ack`
    (accepted / blocked / recipient_not_found / directory_not_known) from
    resolved facts, add the `[from agent in <dir> (session <id>)]`
    provenance/reply-handle tag, and read/append the `(sender -> target)`
    whitelist. Pure, unit-tested.
  - `src/control.rs` — the control socket (`~/.corral/corrald.sock`, override
    `$CORRAL_CONTROL_SOCKET`). A background thread accepts one submission per
    connection: read the request line, parse, find the recipient (registry
    scan for a session, dir-exists for a directory), ack the verdict, and (if
    routable) hand the message to the router over a channel. Submission thus
    fails loud when corral is down (connect fails) rather than piling up a
    silent file queue. Ack is synchronous; delivery and the approval gate run
    later in the router. Unit-tested against a throwaway socket.
  - `src/router.rs` — `Router`: routes agent-initiated messages (enqueued from
    the control socket) to a target directory (reuse a live agent or spawn one)
    or an exact session (deliver if live, else resume its dormant record).
    Delivery to a not-yet-live target hands the message to the launcher as the
    new session's first prompt (launch-with-message), so a spawn/resume
    delivers atomically with no wait-for-announce state; an already-live target
    gets it over its socket. Holds an in-memory queue (no file spool), the
    authorization decisions, and the one message awaiting operator approval;
    the event loop enqueues, polls, and forwards the decision (key or click).
    A live-but-undiscovered session defers for a later poll. Unit-tested
    (gating, spawn-with-message, session delivery, allow/deny, defer).
  - `src/notify.rs` — `ApprovalNotifier` seam. `NotifySendNotifier` mirrors a
    pending approval to a desktop notification with Allow once / Allow always /
    Deny buttons (`notify-send -A`), reporting the choice back on a channel
    tagged with the message id. Best-effort and non-blocking (a thread per
    notification); the in-board dialog always works too. Pure name mapping is
    unit-tested.
  - `src/nav.rs` — pure selection math: move the flat selection index within a
    column (up/down) or across columns (left/right) over the per-column
    counts. Unit-tested.
  - `src/ui.rs` — ratatui: four equal columns (from `Column::ALL`) divided by
    dim vertical rules, each with a bold heading over an underline and padded
    cards spaced for air, a `▍` selection bar. Each card is fixed height: a
    full-width title line, the working-directory basename on its own dim line,
    then a column-specific info line: what the agent is doing (from `tool_call`)
    or last did, plus an age whose meaning follows the column (time blocked in
    Requires Action, time since the last activity in Running, time idle in Idle,
    record age in Dormant). Fixed height keeps `hit_test` a `CARD_ROWS`
    division. Three
    live triage columns (Requires Action, Idle, Running)
    then a dim-gray Dormant column (resumable history). Plus a footer of
    clickable key-hint buttons (`footer_hit_test`, same pattern as the approval
    dialog) with any status on the spacer row above it. Owns the card, heading,
    separator, footer, and age/focus-label formatting.
  - `src/picker.rs` — the `/` jump picker: holds the board's agents
    (`board.selectable()`), grouped under dim directory-basename headers.
    Subsequence fuzzy filter matches title, path, or basename (so a directory
    query keeps every agent under it); a Tab scope filter cycles All/Live/
    Dormant. `selected_agent` maps the selection (which counts only agent rows,
    skipping headers) back to its agent. Enter goes to one, Shift+Enter spawns
    a fresh agent in its dir. Styling (state glyph + color) lives in `ui.rs`;
    picker stays free of ratatui. Unit-tested.
  - `src/main.rs` — the imperative shell: the event loop that scans + prunes
    the registry, spawns watchers, drains updates, polls the `Router`, draws,
    and dispatches input. Input modes are one `Overlay` enum (the `/` jump
    picker or message compose), exclusive by construction. Two verbs chosen by
    a modifier, on both the board and the picker: Enter goes to the selection
    (focus a live window, resume a dormant session), Shift+Enter spawns a new
    agent in the selection's dir. Keys: Up/Down within a column, Left/Right
    across columns, Enter/Shift+Enter as above, `/` open the jump picker, `m`
    message an agent (resume a dormant one to deliver), `d` close a live
    agent's window or forget a dormant record, `q` quit. A left click is
    two-stage (`click_action`): first click selects, a click on the
    already-selected card goes; the footer key-hints are also clickable buttons
    (go / new / jump / msg / delete / quit), sharing the key dispatch via
    `open_jump`/`open_compose`/`spawn_new`. Shift+Enter needs the kitty keyboard
    protocol (`main` pushes `DISAMBIGUATE_ESCAPE_CODES` where supported).

## Extensions

- `extensions/corral-announce.ts` — pi extension announcing an interactive pi
  session: on `session_start` it writes the registry record and binds the
  workdir-local socket; on `session_shutdown` it clears the record's `socket`
  and unlinks. The record's `lastSeen` refreshes on `turn_end` and its `title`
  on rename. The title broadcasts whenever it changes, on rename and on
  `turn_end` (so the first-user-message fallback title reaches clients that
  connected before it existed, not only explicit renames). Serves `initialize`, `session/list` (id, title,
  cwd), `session/prompt` (injects via `pi.sendUserMessage`; queued as follow-up
  while busy; responds with stopReason once the message queue drains, coarse,
  documented in-file), `session/cancel` -> abort. Broadcasts to all connected
  clients: `session/update` message and tool events (whole messages on
  `message_end`; token deltas deferred), `session_info_update` on rename; and
  the standard `state_update` (running/idle/requires_action) on
  `turn_start`/`turn_end` and while the interactive `question` tool blocks on
  the user. A newly connected client is seeded with the current `state_update`.
  Serves multiple concurrent clients. Also registers a `corral_message_agent` tool
  (`target_dir` or `target_session`, `message`, `force_new`) that submits a
  cross-session message over `~/.corral/corrald.sock` (stamped with the
  sender's `fromSession` as a reply handle) and reports corral's ack (accepted
  / blocked / recipient_not_found / directory_not_known); a connect failure is
  surfaced as "corral not running" (fail loud, no silent queue). Install:
  symlink into
  `~/.pi/agent/extensions/`.

## Inter-Agent Messaging

Sandboxed agents cannot reach each other's sockets (each is workdir-local), so
corral is the sole trusted cross-workdir router. An agent calls
`corral_message_agent`, which submits the message over `~/.corral/corrald.sock`
(reachable because `~/.corral` is on the sandbox allowlist). corral parses it,
finds the recipient, and returns a synchronous ack: `recipient_not_found` /
`directory_not_known` if there is nowhere to send, `blocked` if the
`(sender-dir -> target-dir)` pair needs approval, else `accepted`. A connect
failure means corral is down, so submission fails loud instead of queuing
silently. Routable messages are then routed asynchronously: corral authorizes
the pair against the whitelist (or asks the operator, by key or mouse click:
Enter allow once, `a` allow always, Esc deny), resolves the target, and injects
the message over that agent's socket with a `[from agent in <dir> (session
<id>)]` provenance tag. The approval gate is not awaited by the sender (a
human is unbounded): a `blocked` message is acked at once and delivered after
approval, without a delivery ack. Delivery reuses
`prompt::send_prompt`, the same path as operator messaging (`m`). A pending
approval is also mirrored to a desktop notification (`notify-send -A`) whose
Allow/Deny buttons resolve it without switching to the board; best-effort, and
the in-board dialog stays available.

A message is addressed either by **directory** (`target_dir`: reach whoever
works there, spawning one if none, or a dedicated one for `force_new`) or by
**session id** (`target_session`: reach that exact agent, resuming it from its
dormant record if not live). Session addressing is what makes a reply precise:
the provenance tag carries the sender's session id as a reply handle, so the
receiver answers with `corral_message_agent(target_session = ..)` and it lands on the
agent that actually asked, never a sibling that happens to share the directory.
Authorization is always keyed on the `(sender-dir -> target-dir)` pair (a
session target resolves to its cwd), since directories are the stable, human-
meaningful unit. Fire-and-forget: no reply is auto-routed; the receiver sends a
new message back using the reply handle.

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
  h/l) switch columns; Enter or left-click goes to the selected agent (focus a
  live window, resume a dormant session with `pi --session`); Shift+Enter
  spawns a new agent in the selected agent's cwd; `/` opens a fuzzy jump picker
  over all agents, grouped by directory with a state-colored glyph per row and
  a Tab scope filter (All/Live/Dormant) (Enter goes, Shift+Enter spawns beside);
  `m` compose a
  message to any agent — delivered to a live one over its socket, or a dormant
  one by resuming it with the message as its first prompt; `d` close the
  selected live agent (kill its terminal process, closing the window; pi then
  goes dormant and resumable) or forget the selected dormant record; `q`/Esc
  quit. A left click is two-stage: first click selects, a click on the
  already-selected card goes. Shift+Enter needs the kitty keyboard protocol
  (corral pushes it where supported). Long columns scroll to keep the selection
  visible; live cards show time-in-state. Reads `$HOME` (or
  `$CORRAL_REGISTRY_DIR`) for the registry dir; the `/` picker offers only the
  agents already on the board; uses `swaymsg` and `kitty` for focus
  and spawn.
- pi extension `corral-announce` — see Extensions above.
- Registry records in `$HOME/.corral/registry/` and unix sockets in each
  `<cwd>/.corral/` (both created 0700; override with `$CORRAL_REGISTRY_DIR` /
  `$CORRAL_SOCKET_DIR`). No TCP ports, no network exposure. Peer authentication
  relies on the directory permissions.
- Inter-agent messaging: `corral_message_agent` submits over
  `$HOME/.corral/corrald.sock` (override `$CORRAL_CONTROL_SOCKET`), corral's
  control socket; no TCP, peer auth by directory permissions. corral authorizes
  `(sender -> target)` dir pairs against `$HOME/.corral/whitelist` (override
  `$CORRAL_WHITELIST`) plus an operator popup. A message accepted over the
  socket lives only in corral's memory until routed (no on-disk spool): a corral
  crash before routing loses it, an accepted tradeoff under the fire-and-forget
  contract and the systemd keep-alive.

## Known Limitations (v1, deliberate)

- `requires_action` is emitted today only for the interactive `question` tool
  (the one user-input gate an extension can observe). pi's built-in
  tool-approval prompt is not surfaced to extensions, so an approval-blocked
  agent still shows as Running until pi exposes that gate (see Future).
- Focus correlation assumes the pi process and its terminal window share the
  host PID namespace (true under the current nono/bwrap sandbox). If a sandbox
  unshares PIDs, the `/proc` parent-walk cannot reach the window pid.
  Board-spawned windows are detached (`setsid --fork`) so the walk terminates
  at the agent's own terminal rather than climbing into corral's window.
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
- Dormant sessions render as dormant (one card per resumable session, newest
  first), resume on Enter, dismiss
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
