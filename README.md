# corral

Discover and manage coding-agent sessions running on your machine.

Coding agents (pi, Claude Code, codex, gemini, ...) can speak the
[Agent Client Protocol](https://agentclientprotocol.com/) (ACP) as JSON-RPC on
stdio. corral makes such sessions *discoverable*: a wrapper publishes each
agent's stdio on a unix socket in a well-known directory, and a manager lists
every session it finds there. You keep launching agents from your own
terminals; nothing needs to be started "through" a GUI.

## Components

- **agentwrap** — runs an ACP-mode agent command and exposes its stdio on
  `$XDG_RUNTIME_DIR/acp/<label>-<pid>.sock`. Protocol-agnostic byte pump; one
  client at a time; the agent survives client disconnects; the socket is
  removed on exit.
- **corral** — scans `$XDG_RUNTIME_DIR/acp/`, probes each socket with ACP
  `initialize` + `session/list` requests, and shows a live-updating table of
  sessions with label, pid, status (live / busy / stale), agent identity,
  session title, and cwd.
- **corral-announce** (pi extension) — announces *interactive* pi TUI
  sessions on the same socket dir, no wrapper needed: the TUI stays in your
  terminal while ACP clients discover the session, watch its activity
  (messages, tool calls), send prompts into it, and cancel turns.

## Usage

```bash
# Terminal 1: start an agent, discoverable
agentwrap --name myproject -- claude-agent-acp

# Terminal 2: see all running sessions (updates every second)
corral
# or a single snapshot:
corral --once
```

Make it invisible with shell aliases (one-time setup per harness):

```bash
alias claude-acp='agentwrap -- claude-agent-acp'
```

Announce interactive pi sessions (one-time setup):

```bash
ln -s ~/projects/corral/extensions/corral-announce.ts ~/.pi/agent/extensions/
```

Any ACP client can connect to a discovered socket directly (e.g. with socat
bridging stdio to the socket) — corral is just one consumer of the convention.

## Development

```bash
direnv allow      # nix flake dev shell (pinned Rust, just)
just test         # run all tests
just lint         # fmt check + clippy
just manager      # run the session list
```

See [AGENTS.md](AGENTS.md) for architecture details.
