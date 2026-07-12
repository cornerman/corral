# corral

An attention board for coding-agent sessions running on your machine.

You launch pi agents from your own terminals. Corral shows every running agent
as a card in one of two columns, Working or Needs You, so you can see at a
glance which agent is idle and waiting for you. Press Enter to focus that
agent's window, `n` to spawn a new agent, `q` to quit. Corral never drives an
agent; it routes your attention and jumps you to the real window.

Discovery works through a filesystem convention, not a registry service. A pi
extension publishes each session on a unix socket in a well-known directory:

```
$XDG_RUNTIME_DIR/acp/pi-<pid>.sock
```

The socket speaks the [Agent Client Protocol](https://agentclientprotocol.com/)
(ACP) as newline-delimited JSON-RPC. `ls` enumerates sessions; connecting to a
socket talks ACP to that agent.

## Components

- **corral** (`crates/board`) — the attention board TUI. Scans
  `$XDG_RUNTIME_DIR/acp/`, holds a live watch connection per agent, and columns
  each by Working or Needs You. Enter focuses the agent's window via sway; `n`
  spawns a new agent via kitty. Window focus and agent spawn sit behind traits
  (`WindowFocuser`, `Launcher`), so the compositor and terminal are swappable
  and the core never names them.
- **corral-announce** (pi extension, `extensions/corral-announce.ts`) —
  announces an interactive pi session on the socket dir, no wrapper needed. The
  TUI stays in your terminal while ACP clients discover the session
  (`initialize`, `session/list`), watch its activity (message and tool
  broadcasts), send prompts, and cancel turns. It reports Working/idle state
  via a vendor `_corral/state` notification and `SessionInfo._meta`.

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
