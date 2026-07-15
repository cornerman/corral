<div align="center">

<pre>
┌───────┐
│   ∴   │
└───────┘
</pre>

# corral

**An attention board for your locally running coding agents — see who needs you,
jump to them, message across agents.**

</div>

You launch agents however you already do — pi, opencode and Claude Code in your
own terminals, Cursor and other GUI agents in their own windows. Corral shows
each session as a card in one of four columns, **Requires Action / Idle /
Running / Dormant**, so you can see at a glance which agent is blocked waiting
on you, then press Enter to jump straight to its window. It never drives an
agent on its own.

![corral board](docs/screenshot-board.png)

## Quick Start

```sh
# Try it now (opens the board):
nix run github:cornerman/corral

# Or install everything via home-manager (binaries + messaging daemon + agent adapters):
programs.corral.enable = true;
```

Then start your agents however you normally do. They appear on the board
automatically. That is the whole loop.

## Keys

| Key | Action |
|-----|--------|
| `Enter` | Go to the selected agent (focus its window, or resume a dormant one) |
| `Shift+Enter` | Spawn a new agent in that directory |
| `/` | Filter cards |
| `m` | Send a message to an agent |
| `d` | Close a live agent / forget a dormant one |
| `q` | Quit |

## The Three Binaries

- **`corral`** — the attention board as a terminal TUI. The zero-friction path:
  one small binary, runs over SSH, inherits the terminal's font and theme.
- **`corral-gui`** — the same board as a desktop (iced) window, for when no
  terminal is wanted. A pure viewer like `corral`; launch either as many times
  as you like. Add `--launcher` to either for an ephemeral popup.
- **`corrald`** — a headless singleton daemon that owns inter-agent messaging
  (the control socket, the approval gate, the tray). The boards never talk to
  it; both just reflect the shared filesystem registry.

## Messaging

Press `m` to send a message to any agent. Agents can also message each other
across sessions via the `corral_message_agent` tool; that cross-agent messaging
goes through `corrald`, which asks you to approve each new sender→recipient pair
(Allow once / always / Deny).

The approval arrives as a desktop notification, and mirrors to the `corrald`
tray, which also shows the daemon's status:

![message approval](docs/screenshot-message-approval.png)
![corrald tray](docs/screenshot-tray.png)

## Learn More

- [CONVENTION.md](CONVENTION.md) — the filesystem convention any agent joins by
  (so corral is not tied to any one harness).
- [AGENTS.md](AGENTS.md) — architecture, crates, and the messaging daemon.
- [extensions/](extensions/) — the per-harness adapters (pi, opencode, Claude
  Code, Cursor).
