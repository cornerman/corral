# corral

An attention board for coding-agent sessions running on your machine.

You launch pi agents from your own terminals. Corral shows every running agent
as a card in one of four columns, Requires Action, Idle, Running (the ACP v2
state vocabulary), or Dormant (cleanly shut-down, resumable sessions), so you
can see at a glance which agent is blocked waiting on you. Press Enter to go to
the selected agent (focus a live window, resume a dormant session), Shift+Enter
to spawn a new agent in its dir, `/` to fuzzy-jump to any agent, `m` to send a
message, `d` to close a live agent or forget a dormant record, `q` to quit.
Corral
never drives an agent on its own; it routes your attention and jumps you to the
real window, and delivers a message only when you send one.

Discovery works through a per-session registry on the filesystem, not a
registry service. A pi extension writes one record per session and binds an ACP
socket inside that session's own working directory:

```
$HOME/.corral/registry/<sessionId>.json   # the record (override dir: $CORRAL_REGISTRY_DIR)
<cwd>/.corral/pi-<pid>.sock                # the socket it points at (override dir: $CORRAL_SOCKET_DIR)
```

The record names the socket while the session is live and clears it on clean
shutdown, leaving a dormant, resumable entry. The socket is workdir-local so
only that session (and unsandboxed tools like corral) can reach it. The socket
speaks the [Agent Client Protocol](https://agentclientprotocol.com/) (ACP) as
newline-delimited JSON-RPC: corral reads the registry to find each socket, then
talks ACP to that agent.

The registry record, workdir-local socket, and `state_update` broadcast are a
harness-neutral convention, specified independently of pi and corral in
[CONVENTION.md](CONVENTION.md), so any agent harness can join the board.

## Components

- **corral** (`crates/board`) — the attention board TUI, a pure viewer of the
  registry (launch as many as you like). Scans
  `$HOME/.corral/registry/`, holds a live watch connection per live socket, and columns
  each by Requires Action / Idle / Running / Dormant. Enter goes to the
  selected agent (focus a live window, or resume a dormant session by running
  the record's `resumeCommand`); Shift+Enter spawns a fresh agent of the
  selected card's kind in its cwd; `/` focuses a prominent centered filter box
  that narrows the cards by their whole content (title / cwd / activity /
  state) — while filtering, Enter goes and Shift+Enter spawns directly. Window
  focus is auto-selected by session (EWMH on X11; sway/Hyprland/niri on
  Wayland) and agent spawn resolves a terminal from the environment
  (`xdg-terminal-exec` → `$CORRAL_TERMINAL` → `$TERMINAL`), both behind traits
  (`WindowFocuser`, `Launcher`), so the compositor and terminal are swappable
  and the core never names them. `m` messages the selected agent directly and
  ungated (the operator is trusted).
- **corral-gui** (`crates/gui`) — the same attention board as a desktop
  (iced) window: a second parallel viewer for when no terminal is
  wanted. iced renders text via cosmic-text (crisp, shaped). Flat and
  base16-themed, follows the system light/dark; an underline-only centered
  filter over the four columns, click a card to go, Shift+Enter to spawn
  the selected card's kind, the same keys as the TUI. Theme is an env-selected
  base16 dark/light pair (Solarized by default) from built-ins plus
  `~/.config/corral/themes`. Drives the shared `corral-core::engine`. The
  daemon's tray “Open board” launches it.
- **corrald** (`crates/daemon`) — the headless message-routing daemon, a
  singleton owning inter-agent messaging: it binds the control socket
  (`$HOME/.corral/corrald.sock`), authorizes `(sender -> target)` directory
  pairs against a whitelist, and injects each approved message. The approval
  gate surfaces on a `ksni` system tray (Allow once / always / Deny, open board,
  quit) with a `notify-send` mirror. Run it under a systemd user service. The
  board and the daemon share only the registry and never talk to each other.
- **corral-core** (`crates/core`) — the shared lib (registry discovery, prompt
  delivery, the terminal launch seam, window-focus seam, on-disk paths), so the board and the daemon
  reuse one implementation without linking each other's UI dependencies.
- **corral-announce** (pi extension, `extensions/corral-announce.ts`) —
  announces an interactive pi session via the registry, no wrapper needed. The
  TUI stays in your terminal while ACP clients discover the session
  (`initialize`, `session/list`), watch its activity (message and tool
  broadcasts), send prompts, and cancel turns. It reports run state via the
  standard ACP v2 `state_update` notification (running/idle/requires_action).
  It also registers a `corral_message_agent` tool so a session can hand a message to
  another agent, addressed by directory or by exact session id (the latter lets
  a reply reach the precise agent that asked); corrald routes it, spawning or
  resuming a target if none is running and asking you to approve unfamiliar
  sender/target pairs.
- **corral-opencode** (opencode plugin, `extensions/corral-opencode.ts`) — the
  second adapter, proving the convention is harness-neutral. It announces an
  interactive opencode session exactly as `corral-announce` does for pi (same
  registry record with `label: "opencode"`, same workdir-local ACP socket, same
  `state_update` broadcast, the same `corral_message_agent` tool), so a mixed
  pi/opencode board reads at a glance. corral itself needed no change.

## Usage

```bash
# Announce interactive pi sessions (one-time setup):
ln -s ~/projects/corral/extensions/corral-announce.ts ~/.pi/agent/extensions/

# Announce interactive opencode sessions (one-time setup):
ln -s ~/projects/corral/extensions/corral-opencode.ts ~/.config/opencode/plugin/

# Run the board (any number of instances):
corral

# For inter-agent messaging, run the daemon once (ideally a systemd user
# service so it survives crashes). A second instance refuses to start.
corrald
```

Any ACP client can connect to a discovered socket directly (for example with
socat bridging stdio to the socket). Corral is one consumer of the convention.

## Development

```bash
direnv allow      # nix flake dev shell (pinned Rust, just)
just test         # run all tests
just lint         # fmt check + clippy
just board        # run the attention board
```

See [AGENTS.md](AGENTS.md) for architecture details.
