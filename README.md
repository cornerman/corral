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
| `Shift+←/→` or drag | Move a card between columns to drive the agent's state (stop, continue, kill, resume) |
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

<p>
  <img src="docs/screenshot-message-approval.png" alt="message approval" width="300">
  <img src="docs/screenshot-tray.png" alt="corrald tray" width="300">
</p>

## Security: the Filesystem Is the Authority

Corral has no ports and no network surface. Every channel is a unix socket or a
file under `~/.corral` and each workdir's `.corral/` (both `0700`), and the only
credential is **directory permissions** — so the security model rides on one
idea: **physical location is identity.**

Each agent runs sandboxed to its own working directory, so it can write files
*only* there. It therefore writes its registry record and its outbox messages
inside its own `<cwd>/.corral/`. `corrald` — the single trusted broker — never
trusts what a file *says* about who wrote it; it derives that from **where the
file physically lives**. A record found under `evil/` is attributed to `evil/`,
full stop; an agent cannot claim another directory it cannot write, nor aim a
message's sender at a box it does not own.

From that one primitive the rest follows:

- **corrald curates.** It is the only reader of the agent-writable registry
  index; it authenticates and validates every field, then publishes a sealed,
  vetted `~/.corral/state/registry/` that the boards read. Boards render only
  vetted data.
- **A harness must be registered before it runs.** The exact launch command of
  each agent kind is approved by you once (corrald shows it); an unapproved or
  altered command is quarantined, never executed. So a planted record cannot
  turn into code execution.
- **The private side is sealed.** Agents may append to the registry index and
  connect to the socket; the whitelist, the approved-command store, the vetted
  registry, and the audit log live in `~/.corral/state/` and are never on an
  agent's sandbox allowlist — unwritable by construction.

The load-bearing precondition: each agent is boxed to its workdir (whole-process
sandbox). Without that, corral's gates are a convenience, not a boundary. The
full threat model, every mitigation, and the accepted risks are in
[SECURITY.md](SECURITY.md).

## Learn More

- [CONVENTION.md](CONVENTION.md) — the filesystem convention any agent joins by
  (so corral is not tied to any one harness).
- [SECURITY.md](SECURITY.md) — the threat model, mitigations, and accepted
  risks. Read it before trusting corral between mutually untrusted agents.
- [AGENTS.md](AGENTS.md) — architecture, crates, and the messaging daemon.
- [extensions/](extensions/) — the per-harness adapters (pi, opencode, Claude
  Code, Cursor).
