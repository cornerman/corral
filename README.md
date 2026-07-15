<div align="center">

<pre>
┌───────┐
│   ∴   │
└───────┘
</pre>

# corral

**An attention board for the coding agents running in your terminals.**

</div>

You launch agents (pi, opencode, Claude Code, Cursor) in your own terminals, as
usual. Corral shows each session as a card in one of four columns, **Requires
Action / Idle / Running / Dormant**, so you can see at a glance which agent is
blocked waiting on you, then press Enter to jump straight to its window. It
never drives an agent on its own.

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

`corral` is the terminal board; `corral-gui` is the same board as a desktop
window. Add `--launcher` to either for an ephemeral popup.

## Learn More

- [CONVENTION.md](CONVENTION.md) — the filesystem convention any agent joins by
  (so corral is not tied to any one harness).
- [AGENTS.md](AGENTS.md) — architecture, crates, and the messaging daemon.
- [extensions/](extensions/) — the per-harness adapters (pi, opencode, Claude
  Code, Cursor).
