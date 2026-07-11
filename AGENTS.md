# corral — Architecture and Setup

This file and README.md MUST always be kept up to date when the setup,
architecture, or interfaces change.

## What This Is

A discovery system for locally running ACP coding-agent sessions. Design
premise: agents are launched by the user in arbitrary terminals, never by a
manager. Discovery works through a filesystem convention, not a registry
service:

```
$XDG_RUNTIME_DIR/acp/<label>-<pid>.sock   (directory mode 0700)
```

Whoever runs an agent binds a socket there and unlinks it on exit. The
filesystem is the registry: `ls` enumerates sessions, connecting to a socket
talks plain ACP (JSON-RPC, newline-delimited, as on stdio) to that agent.

## Data Flow

```
your terminal                                    another terminal
  agentwrap --name foo -- claude-agent-acp         corral
    |  spawns child (ACP JSON-RPC on stdio)          |  scans $XDG_RUNTIME_DIR/acp/
    |  binds $XDG_RUNTIME_DIR/acp/foo-<pid>.sock     |  probes each socket:
    |  pumps bytes: socket <-> child stdio           |    ACP initialize -> live/busy/stale
    |  unlinks socket on exit                        |  renders table, 1s refresh
```

## Crates

- `crates/agentwrap` — the wrapper. Plain byte pump, no ACP knowledge except
  in comments. One client at a time: a concurrent connect is accepted and
  immediately closed (probers read that as "busy"). Child stdin stays open
  across client disconnects so the agent never sees EOF between reconnects.
  Child stdout backpressures via the pipe buffer while no client is
  connected; a chunk that fails mid-send is delivered to the next client.
  Cleanup (socket unlink + exit with child's status) happens in one place:
  the child-waiter thread. SIGINT/SIGTERM are forwarded to the child.
  - `src/naming.rs` — label/socket-path convention (pure, unit-tested)
  - `tests/wrap_roundtrip.rs` — end-to-end: wraps `cat`, round-trips bytes,
    reconnects, rejects concurrent clients, verifies cleanup
- `crates/corral` — the manager CLI. Scans the socket dir, probes each
  socket with an ACP `initialize` request (2s timeout), disconnects right
  after so real clients can attach. `--once` for a snapshot, default loops
  with a cleared screen every second.
  - `src/discovery.rs` — filename parsing + dir scan (pure, unit-tested)

## Interfaces to the Outside World

- CLI `agentwrap [--name <label>] [--] <command> [args...]` — stderr of the
  child passes through to the terminal; exit code mirrors the child.
- CLI `corral [--once]` — writes a table to stdout.
- Unix sockets in `$XDG_RUNTIME_DIR/acp/` (created 0700). No TCP ports, no
  network exposure. Peer authentication relies on the directory permissions.

## Known Limitations (v1, deliberate)

- One client per socket; multiplexing (board + editor concurrently) is a
  later layer.
- Busy-rejection is racy for back-to-back reconnects: right after a client
  disconnects, the next connect may still be bounced until the pump thread
  observes the EOF. Clients must treat a bounced connect as retryable.
- corral's probe sends `initialize` a second time when a real client later
  initializes the same agent; agents are expected to tolerate re-initialize.
- Sessions not started via agentwrap are invisible (no passive finder tier).

## Development Setup

- Nix flake (nixpkgs-unstable) + direnv; Rust pinned via rust-toolchain.toml
  through rust-overlay. `just` for commands: `just test`, `just lint`,
  `just watch` (cargo-watch), `just nix-build`.
- CI: GitHub Action runs `nix flake check` (build + tests via nix).
