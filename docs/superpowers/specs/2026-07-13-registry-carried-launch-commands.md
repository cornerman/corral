# Registry-Carried Launch Commands

Spec — 2026-07-13

## Problem

corral hardcodes pi's launch grammar. `crates/core/src/launch.rs` bakes in
`kitty -e pi`, the `--session <path>` resume flag, and the positional-message
convention (with a `-`/`@` guard). This blocks a second agent kind: the board
cannot open opencode from an opencode card, because it only knows how to run
pi.

Three things are hardcoded across the code; two are already behind seams, one
is not:

- **kitty** — the terminal seam (`Launcher`/`KittyLauncher`). Opens a window
  running a command. Agent-neutral already; only its selection is fixed.
- **sway** — the focuser seam (`WindowFocuser`/`SwayFocuser`). Finds/focuses/
  closes a window by a `/proc` parent-walk. Agent-neutral already.
- **pi** — NOT behind a seam. Its CLI grammar is fused into `launch.rs`.

## Decision

The registry record carries the launch commands. Each announce adapter (pi's
`corral-announce`, a future opencode equivalent) stamps into its own record the
argv to spawn a fresh sibling of its kind and the argv to resume this exact
session. corral reads the argv and runs it; it never branches on kind and never
names pi.

Rationale (the user's model): "Enter/Shift+Enter on an opencode card opens
opencode — corral can see that from the extension, which writes the commands."
The kind is implicit in the record; the command is data, not code.

### Record fields (replaces `resume`)

The `resume` string field (a session-file path consumed as `pi --session
<resume>`) is replaced by two argv fields:

| Field           | Type              | Meaning |
|-----------------|-------------------|---------|
| `spawnCommand`  | string[] \| null  | argv to start a fresh session of this kind (rooted at a cwd the consumer supplies). e.g. `["pi"]`. |
| `resumeCommand` | string[] \| null  | argv to relaunch this exact session. e.g. `["pi","--session","<sessionId>"]` (the id, not the file path: corral resumes in the session's cwd so pi's per-project `--session <id>` lookup resolves it, and the id is already the record key). `null` when not resumable (ephemeral). |

Both optional: an agent that omits them is discoverable and drivable but not
launchable (clean degradation).

### Launcher collapses to one method

`spawn` and `resume` fuse into:

```rust
fn launch(&self, cwd: &Path, command: &[String], message: Option<&str>) -> Result<(), String>;
```

The spawn/resume distinction moves into *which argv the record supplies*.
`KittyLauncher` runs `setsid --fork kitty --directory <cwd> -e <command…>
[message]`.

### Message injection: a neutral convention corral keeps

corral appends the initial `message` as the final argv element, space-guarding a
leading `-`/`@`. This is framed as a generic CLI-safety convention, not pi
grammar: it suits pi and most CLIs. It is the one launch detail that stays in
corral, because the message is a runtime value the static record template cannot
carry.

### Spawn command source

- **Board (Shift+Enter / `n` / `+ new`):** the *selected* agent's
  `spawnCommand`, spawned in the selected agent's cwd. Kind follows selection.
  An empty board has no selection, so it cannot spawn — agent #1 is started
  from a terminal. This fits corral's reflector identity and is a documented v1
  limitation.
- **Daemon (directory-target message, no live agent):** the `spawnCommand` of
  any record (live or dormant) for that directory. A directory corral has never
  seen an agent in cannot be spawned into (the kind is unknown); the message
  fails to route. Documented v1 limitation.

### Resume command source

The dormant record's `resumeCommand` (board Enter/compose, daemon session-target
resume of a dead/dormant session).

## Consequences

- pi's CLI grammar (`-e pi`, `--session`, `pi_args`) leaves `launch.rs`
  entirely, into `corral-announce.ts`.
- The `label` default-to-`"pi"` fallback for unknown producers is dropped
  (a missing label renders as unknown, not pi).
- Prune loses its "resume-target file exists" check (it cannot stat an argv);
  it relies on the 14-day staleness sweep alone. A deleted session file now
  fails at resume time instead of being pruned early. Acceptable: rare, and
  the record ages out.
- kitty and sway remain the two real seams; their selection stays fixed in
  `main`. pi is no longer a seam — its command rides in the registry.
- Redundancy: every record of a kind carries an identical `spawnCommand`. A few
  short strings, written once at announce, never synchronized. Deduping into a
  per-kind table is a mechanical change behind the same board-reads-argv
  interface if a cost ever appears (YAGNI now).

## Out of scope

- An opencode adapter (this spec only unblocks it).
- A `$CORRAL_SPAWN_COMMAND` env fallback for empty-board spawn (trivial add if
  the empty-board launcher is wanted).
- Converging the TUI's inline loop onto `core::engine` (pre-existing debt; both
  `prune` sites are updated in parallel here).
