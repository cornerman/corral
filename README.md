# corral

An attention board for coding-agent sessions running on your machine.

You launch pi agents from your own terminals. Corral shows every running agent
as a card in one of three columns, Requires Action, Idle, or Running (the ACP
v2 state vocabulary), so you can see at a glance which agent is blocked waiting
on you. Press Enter to focus that
agent's window, `n` to spawn a new agent, `q` to quit. Corral never drives an
agent; it routes your attention and jumps you to the real window.

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

## Components

- **corral** (`crates/board`) — the attention board TUI. Scans
  `$HOME/.corral/registry/`, holds a live watch connection per live socket, and columns
  each by Requires Action / Idle / Running. Enter focuses the agent's window
  via sway; `n` spawns a new agent via kitty in the selected agent's cwd; `N`
  opens a fuzzy directory picker (roots from `$CORRAL_PROJECT_ROOTS`, default
  `~/projects`). Window focus and agent spawn sit behind traits
  (`WindowFocuser`, `Launcher`), so the compositor and terminal are swappable
  and the core never names them.
- **corral-announce** (pi extension, `extensions/corral-announce.ts`) —
  announces an interactive pi session via the registry, no wrapper needed. The
  TUI stays in your terminal while ACP clients discover the session
  (`initialize`, `session/list`), watch its activity (message and tool
  broadcasts), send prompts, and cancel turns. It reports run state via the
  standard ACP v2 `state_update` notification (running/idle/requires_action).

## Usage

```bash
# Announce interactive pi sessions (one-time setup):
ln -s ~/projects/corral/extensions/corral-announce.ts ~/.pi/agent/extensions/

# Then just run the board:
corral
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
