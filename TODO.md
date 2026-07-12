# corral TODO

Living list of remaining work. See AGENTS.md for architecture and
docs/superpowers/specs/ for the design.

## Unified Session Registry (designed, not built — see spec)

Supersedes the current flat `~/.corral/sockets` discovery and the idea of a
separate history store: one per-session file drives discovery, isolation, and
resume.

- [ ] corral-announce: bind the socket at `$WORKDIR/.corral/pi-<pid>.sock`
      (workdir-local = sandbox-isolated, verified); write
      `~/.corral/registry/<sessionId>.json`
      `{ sessionId, cwd, label, socket, resume, lastSeen }` on session_start;
      refresh on rename + turn_end; on clean shutdown unlink socket + clear
      `socket`. Skip ephemeral (no session file).
- [ ] board discovery: scan `~/.corral/registry/*.json`; `socket` connectable
      → live (persistent watch), else dormant. Identity = `sessionId`.
- [ ] board: dormant sessions dimmed in Idle (latest-per-cwd); `Agent.origin`
      Live|Dormant; Enter/click resumes via the Launcher seam (`resume`); `d`
      dismisses. Prune dead-target / >14-day / dismissed.

## Inter-Agent Messaging (designed, not built — see spec)

- [ ] Operator-initiated: board keybinding to send a `session/prompt` to the
      selected agent (optionally spawn-in-dir then send).
- [ ] Agent-initiated: a tool routing through corral to a target agent, gated
      by popup and/or whitelist. corral is the only cross-workdir bridge
      (sockets are workdir-isolated), so it needs the board (or a broker)
      running.

## Validation

- [x] `$HOME/.corral` is in the pi sandbox allow list (sandboxed sessions can
      announce there). Run corral itself with `cargo run` / `just board`.
- [ ] Live end-to-end run: real sandboxed pi sessions appear, focus jumps to
      the right window, the `question` tool flips the card to Requires Action.
      (Needs a fresh pi session; ones started before `.corral` was allowed
      still bind the old path.)

## Platform (pi) — the requires_action follow-up (C)

- [ ] Full `requires_action` coverage. Today corral-announce only detects the
      `question` tool. pi's built-in tool-approval confirms and other
      `ctx.ui.*` prompts (select, input, elicitation) are invisible to
      extensions. Wanted: pi emits a signal whenever any blocking UI prompt
      opens/closes, or pi speaks ACP v2 `state_update` natively.
- [ ] Track the ACP v2 Prompt Lifecycle RFD
      (agentclientprotocol.com/rfds/v2/prompt). When `state_update` stabilizes,
      corral already speaks it; retire any interim shim.

## Board Polish

- [ ] Column scrolling when a column has more cards than fit. `ui::hit_test`
      currently assumes no per-column scroll, so clicks past the visible area
      mis-map. Add a scroll offset per column and account for it in hit_test.
- [ ] Consider showing time-in-state (how long an agent has been Requires
      Action / Idle) to sharpen triage.
- [ ] `f` fuzzy-focus: open the picker over live sessions (filter by
      label/title/cwd), Enter focuses the chosen session's window. Reuses the
      Picker widget; faster than arrow nav when many agents are running.

## Extension (corral-announce)

- [ ] `agentInfo.version` is `"?"`: `require("@earendil-works/pi-coding-agent/
      package.json")` does not resolve. Find a supported way to read pi's
      version, or drop the field.
- [ ] `session/prompt` responses resolve for all waiting clients at once when
      the queue drains (no per-message turn attribution). Fine for now;
      revisit if multiple drivers need accurate stopReason routing.

## Future Features

- [ ] Multi-agent: let non-pi ACP agents announce (their own extension or a
      stdio-to-socket wrapper binding `<label>-<pid>.sock`). The board already
      discovers any socket and reads agentInfo generically.
- [ ] Agent-to-agent channel: corral brokers a link so two agents can talk.
- [ ] More compositors/terminals: new `WindowFocuser` / `Launcher`
      implementations behind the existing seams (sway/kitty are PoC).
