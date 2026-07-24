# corral CLI: `send` and `list`, a Shell Entry Point Into the Bus

Status: **shelved by decision, 2026-07-24.** Design complete, not implemented,
no code written. Written against `404d627`, i.e. after `03664dc` "gate messages
on the whitelist alone"; the security rules it states still hold at `3bdc44c`.

The operator resolved D0 as "ship nothing now, keep the spec as the artifact":
the CLI has **no committed consumer**, and its one candidate (the multi-agent
todo system, `TODO_SYSTEM_SPEC.md`) evaluated it and chose a path that bypasses
it. Implement this the day a real consumer appears, which is roughly an
afternoon's work; until then the value here is that the design and especially
the security decision are settled, so neither gets re-litigated later.

The subordinate decisions were answered before shelving, and D2's answer changed
the contract: **D1** yes, add `--help` / `-h` / `help`. **D2** omit `createdAt`
entirely, because nothing in the workspace reads it (zero references to
`createdAt` or `created_at` in `crates/`); `CONVENTION.md` now marks the field
optional (`3bdc44c`), so omitting it is conformant and no `iso8601_utc` helper
is needed. **D3** put the e2e asserts inside `nix/tests/scenarios/pi.py`.

## Problem

Two callers can inject a message into corral's bus, and no third one can. An
agent submits through its `corral_message_agent` tool; the operator presses `m`
on a board. Every other local program has no way in, even though the daemon it
would talk to is a plain unix socket in the operator's own home directory: a git
hook, a CI step, a systemd unit, a status bar, a file watcher, a shell one-liner.

That is a hole in the interface surface, not a missing feature of any one project.
corral already exposes discovery as files the operator may read and submission as
a control socket any process of that uid may connect to; what is missing is a way
for a local program to speak the protocol without embedding a JSON client of its
own. The
four harness adapters each reimplement that client in TypeScript, so the protocol
has four implementations and the shell has none.

One concrete near-use exists, for `corral list`. `TODO_SYSTEM_SPEC.md` starts a
fresh dispatcher agent on every change to its todo file: stateless, but it pays to
re-read the whole list each time. A long-lived dispatcher would instead have to
ask whether one is already alive before starting a second, since two dispatchers
on one file race and the file has no lock. That liveness question is what `corral
list` answers from a shell.

One qualification, because it bounds how strong that argument is: a script running
**as the operator** can already answer the same question by globbing
`~/.corral/state/registry/*.json`, the vetted records both boards read. `corral
list` differs in giving the *reachability-filtered* roster (a caller sees titles,
cwds and descriptions only for directories it may reach) and in working from
inside a sandbox, where the sealed `state/` is unreadable by construction. So the
liveness case is decisive for a confined caller and merely convenient for an
unconfined one.

## Non-Goals

- `corral watch`, `corral spawn`, `corral stop`, `corral history`. Each was
  considered and dropped: an agent already reaches those through its own tools,
  and the missing capability is only submission from a shell. Adding them would
  grow corral toward the orchestrator VISION.md forbids.
- Any task state, queue, or scheduling inside corral ("a bus, not a container").
- Waking the multi-agent todo system. That was this spec's original driver and it
  is no longer a consumer; see "Honest Status".
- A new binary. The subcommands ride the existing `corral`.
- TUI/GUI parity. This is a non-interactive entry point, not a board feature, so
  the parity hard rule does not apply.

## Honest Status: No Committed Consumer

This spec began as the dependency of a multi-agent todo MVP, whose file watcher
was to wake a dispatcher agent with `corral send --dir <todo-dir> "todos.md
changed"`. That consumer evaluated the CLI and chose a simpler path: the watcher
now runs the harness directly (`pi "<todo list> … dispatch the ready items"`,
blocking until it exits), so the wake never touches corral or corrald.
`TODO_SYSTEM_SPEC.md` (`404d627`) names the four costs that decision removed:

1. a new subcommand plus the first Rust control-socket client inside corral;
2. every wake routed through the daemon;
3. a `(todo -> todo)` whitelist approval for the wake itself;
4. an ack that can only ever say "accepted for routing", never "delivered".

That reasoning is sound and it generalizes: **when the caller knows which agent it
wants, and can block, running the harness directly beats going through the bus.**
The CLI earns its place only where that is false, which is three situations:

- The target is **already running** and must be reached rather than duplicated.
  Running the harness again starts a second session; only a message reaches the
  live one.
- The caller must **not block**: a git hook, a CI step, a keybind, a status-bar
  action. Submission returns in milliseconds with an ack.
- The caller is **confined**, so it cannot read the sealed registry and cannot
  exec a harness with a workdir of its choosing, but it can reach
  `corrald.sock` (which the shipped sandbox profile already grants).

Against that, the cost: roughly 200 lines of Rust in two new modules, a permanent
public surface on the `corral` binary, and edits to three documents. None of it is
speculative in *design* (every mechanism below reuses a shipped, tested path), but
all of it is speculative in *demand*, and an unused interface still has to be
maintained, tested and explained. The repo's own principle applies: the best
weapon against the complexity daemon is the word no.

The operator decides, and I would rank the options this way:

1. **Ship neither now, keep the spec.** No consumer exists today. The design is
   the durable artifact: it can be implemented in an afternoon the day one
   appears, and this document already records the security decision that would
   otherwise be re-litigated then.
2. **Ship both**, on the interface-hole argument: a bus reachable only by its own
   agents and by a keypress has no public entrance, and the three situations
   above will each arrive eventually.
3. **Ship `list` alone.** Tempting (read-only, ungated, no security argument to
   get wrong) but it saves less than it looks: `list` rides the same outbox
   submission envelope, so `core::control::submit` is needed either way, and only
   the `send` argv surface and its gate documentation fall away. A confined
   consumer is also the only caller for whom it beats globbing the registry.

## Surface

Two subcommands plus a help form on the existing `corral` binary. Bare `corral`
and `corral --launcher` behave exactly as today.

```
corral send (--dir <path> | --session <id>) [--label <kind>] [--force-new] [--visible] [--] <message>
corral list
corral help | --help | -h
```

`send` prints the daemon's ack line verbatim on stdout (`{"status":"accepted"}`),
so a script can read the word and a human can read the same line. `list` prints
corrald's roster JSON line verbatim. Errors go to stderr with the binary's
existing `corral: ` prefix.

There is no `--json` flag on `list`: the reply is always one JSON line, so a flag
that changes nothing would be interface noise.

Flags on `send`, at full parity with the agent tool surface:

| Flag | Meaning |
|------|---------|
| `--dir <path>` | Reach whoever works in that directory, spawning one if none. Exactly one of `--dir` / `--session`. |
| `--session <id>` | Reach that exact session, resuming it from its dormant record if needed. |
| `--label <kind>` | Which agent kind to spawn if a `--dir` target has to be started (matched against a record's `label`, resolved from a record of that kind in **any** directory). |
| `--force-new` | With `--dir`: spawn a dedicated fresh agent instead of reusing a live one. |
| `--visible` | The spawn/resume this message triggers opens a real window (`hidden:false`). Default is hidden. |
| `--` | End of flags, so a message may start with `-`. |

The message is one trailing positional argument. No stdin form (nothing needs
it, and quoting one argument is the smaller interface).

`--label` is load-bearing, not decoration. `router.rs` resolves a labelled spawn
command from a record of that kind found anywhere, and the comment says it
"works even where the kind never ran", so `--label pi` starts an agent in a
directory corral has never seen. Without a label the router falls back to that
directory's own record and fails with "no known agent kind for `<dir>` (never
announced there)". Any scripted sender into a fresh directory must therefore pass
`--label`.

### Exit Codes

| Code | When |
|------|------|
| 0 | Routable ack: `accepted` or `approval_needed`. |
| 1 | Non-routable ack (`recipient_not_found`, `directory_not_known`, `malformed`, or any unrecognized status), or a local failure (no `$HOME`, cannot create the outbox, cannot connect, timeout, no ack line). |
| 2 | Usage error: unknown flag, missing message, both or neither target, message over the 256 KiB submission cap. |

Exit 0 means **accepted for routing**, never delivered. Two reasons, both worth
stating because they bound what a caller may conclude:

1. The gate runs later. An `approval_needed` message waits for the operator, and
   the sender is told at once rather than being made to wait on a human.
2. The spawn runs later, and its failures are asynchronous. A wrong or missing
   `--label` produces "unknown label" or "no known agent kind" inside corrald
   **after** the ack was already sent, so it surfaces only in
   `~/.corral/state/audit.log`, never in the exit code.

A connect failure is a loud non-zero, never a silent local queue: if corrald is
down, the caller must know.

## Security: the CLI Is a Submitter, Not the Operator

`corral send` is gated exactly like a harness adapter. It carries no operator
trust, and this is the load-bearing decision of the whole design.

The rule: **operator trust cannot be established over a socket that untrusted
code can reach.** The board's `m` is ungated because it is a keypress inside the
operator's own process, which is proof of the operator's presence. A CLI
invocation has no such proof. `nix/tests/profiles/agent.jsonc` grants every
sandboxed agent both `~/.corral/corrald.sock` and (through `default`) read access
to the nix store, so a confined agent can exec the `corral` binary. An ungated
CLI would hand every confined agent a whitelist bypass, dissolving the boundary
SECURITY.md T2 exists to hold.

So the CLI submits the same way an adapter does, and its identity comes from the
physical location of its outbox file:

- Identity is `std::env::current_dir()`. The outbox file lands in
  `<cwd>/.corral/outbox/`, and corrald derives the authenticated `fromCwd` from
  where that file physically lives (`discovery::cwd_from_outbox_path`). The
  directory you invoke `corral send` from is the left side of the whitelist pair.
- There is no `--from` flag and never will be. A `--from` flag is exactly the
  forgery that location-derived identity exists to prevent.

Authorization keys on the `(sender-dir -> target-dir)` pair against
`~/.corral/state/whitelist`, plus the operator's tray or notification popup. Two
consequences to plan around:

- The whitelist is the **single** authorization axis since `03664dc`. `hidden` is
  window placement only, feeding `mode.hidden` on a launch path. A whitelisted
  pair therefore spawns visibly with no prompt, and an unwhitelisted pair prompts
  even for a hidden spawn. `--visible` costs no approval; it exists because the
  operator prefers seeing agents.
- `is_whitelisted` has no self-pair exemption, so a send from directory X to
  directory X still prompts once. The operator has no whitelist file at all
  today, so **every** pair prompts on first use. The price of the security
  decision is one "allow always" per pair, and nothing in this design may assume
  an existing whitelist. (The roster query does exempt self, in `handle`'s
  `visible` closure, so `corral list` needs no approval.)

The CLI is not a session, so it sends no `fromSession`. The recipient's
provenance tag reads `[from <sender-dir-basename>]` with no reply handle, which
means **a recipient cannot reply to a CLI-sent message**. That is the right
default: a shell command has exited by the time an answer could arrive, and there
is no session to route one to. A scripted sender that wants an answer must name a
real session as the reply target inside its own message text.

SECURITY.md gains a Trust Boundaries bullet stating that the CLI is a submitter
with adapter-level trust, not an operator action, together with the "trust cannot
be established over a reachable socket" rule.

## Mechanism: `core::control::submit`

The control-socket client exists today only in TypeScript, duplicated across the
adapters. This adds the first Rust one, in `corral-core` beside `prompt.rs`,
because the protocol is shared property of the daemon and every submitter, and
`corral-gui` may want it later.

```rust
/// Submit one control request the authenticated way and return corrald's ack line.
pub fn submit(socket: &Path, cwd: &Path, record: &serde_json::Value) -> std::io::Result<String>
```

Steps, mirroring `submitRawToCorral` in `extensions/corral-pi.ts` and the reader
side in `crates/daemon/src/control.rs`:

1. Serialize the record. Fail loud if it exceeds the submission cap, before
   touching the filesystem.
2. Create `<corral>/outbox/` with mode 0700, where `<corral>` is
   `$CORRAL_SOCKET_DIR` if set, else `<cwd>/.corral` (the same resolution the
   adapters use).
3. Write the JSON to `<corral>/outbox/<id>.json` with mode 0600, where `<id>` is
   the record's own id. CONVENTION.md already calls the id "also the outbox
   filename stem", so one value names both.
4. Connect `paths::control_socket()` (`~/.corral/corrald.sock`, override
   `$CORRAL_CONTROL_SOCKET`), with a 5 s read and write timeout.
5. Write one line, `{"submit":"<absolute path>"}\n`.
6. Read exactly one line back and return it trimmed.
7. On **every** failure path, unlink the outbox file (best effort). corrald
   consumes it on success, so a successful submit leaves nothing behind.

That path shape is the authentication: corrald requires exactly
`<cwd>/.corral/outbox/<file>` and rejects anything else, so the file's location
is what proves the sender's directory.

The 256 KiB cap becomes shared instead of duplicated: `curation::MAX_SUBMISSION`
turns `pub` and `control.rs` enforces the same constant that corrald enforces on
the reading side, so the client-side check cannot drift from the server's.

`submit` is generic over the record so `list` reuses it: `list` submits
`{"op":"list","fromCwd":<cwd>}` (corrald overrides `fromCwd` with the
authenticated value anyway) and prints the roster line.

## Mechanism: Argv Dispatch

The gotcha to design around: `crates/board/src/main.rs` calls `ratatui::init()`
before it looks at argv, greping only for `--launcher`. A subcommand dispatched
after that would scribble escape sequences over its own stdout. So parsing moves
to the very top of `main`, before any terminal setup, in a new
`crates/board/src/cli.rs`:

```rust
pub enum Command { Board { launcher: bool }, Send(Send), List, Help }
pub fn parse(args: &[String]) -> Result<Command, String>   // pure, unit-tested
```

`main` matches: `Send`/`List`/`Help` run and exit before `ratatui::init()`;
`Board` falls through to today's code path unchanged.

Only the exact first words `send`, `list`, `help`, `--help` and `-h` divert;
anything else keeps today's behavior of opening the board. One existing
invocation therefore changes meaning: `corral --help` prints usage instead of
opening the TUI (D1, decided). A binary that answers a help request with a
full-screen TUI is a trap once it has subcommands.

Parsing stays hand-rolled: the workspace has no argument parser today, and two
subcommands with five flags do not earn `clap`'s dependency weight.

Record building is a second pure function so it can be tested without a socket:

```rust
pub fn record(send: &Send, id: &str) -> serde_json::Value
```

Fields per CONVENTION.md: `id`, `message`, `targetDir` xor `targetSession`,
`forceNew`, plus `label` when given and `hidden` always (explicitly
`true` unless `--visible`, so the record states its own intent rather than
leaning on the daemon's default). No `fromSession`, as above. `fromCwd` is
omitted entirely: corrald overwrites it with the authenticated value, so writing
it would only invite a reader to trust it.

The id is `<unix_millis>-<pid>-<counter>`, with the counter a process-local
`AtomicUsize`. Uniqueness among in-flight submissions is the only requirement, so
this needs no `uuid` dependency.

Ack handling is the third pure function, `exit_code(ack_line: &str) -> i32`: parse
the `status` field, map `accepted` and `approval_needed` to 0 and everything else
(including an unparseable line) to 1. The raw line is printed either way.

## Testing

Unit tests, following `prompt.rs` and `history.rs`:

- `cli::parse`: every accepted shape; both targets; neither target; missing
  message; unknown flag; `--` guarding a leading dash; `send` with an
  over-cap message; each of `help` / `--help` / `-h` yielding `Help`; bare
  `corral` and `--launcher` still yielding `Board`.
- `cli::record`: field presence and exclusivity, `hidden` true by default and
  false under `--visible`, `label` present only when given, no `fromSession`.
- `cli::exit_code`: each ack word, plus garbage.
- `control::submit` against a throwaway `UnixListener` in a `tempfile::tempdir`:
  it writes the envelope line, the referenced path satisfies
  `discovery::cwd_from_outbox_path` and yields the temp cwd, the file content
  round-trips the record, and the ack line comes back. Plus two failure tests:
  no listener leaves no outbox file behind, and a listener that closes without
  acking is an error that also leaves no file behind.

VM e2e (AGENTS.md hard rule). The CLI is a new user-facing entry point, so it
earns assertions, but not a new VM: it needs a live agent to deliver to, which
`nix/tests/scenarios/pi.py` already has. Proposal is a new section in that
scenario, placed after section 4 (before B is stopped and before A wedges on the
question tool), using a dedicated sender directory `~/cli-sender` and message
text found nowhere else:

1. **Negative (the security argument).** From `~/cli-sender`, whose pair is not
   whitelisted, `corral send --dir <proj-b> 'cli-gated'`. Assert the exit status
   is 0 and the printed status is `approval_needed`, then assert after a wait
   that the stub LLM never saw `cli-gated`. Cheap, because it only has to prove
   that nothing was delivered, and it is the entire justification for gating the
   CLI.
2. **Positive.** Whitelist `~/cli-sender -> <proj-b>`, send `cli-to-b`, and wait
   for the stub to see both `cli-to-b` and the `[from cli-sender` tag. This also
   pins the no-reply-handle tag shape.
3. **`list`.** Run `corral list` from `~/cli-sender`, parse the stdout as JSON,
   and assert it carries an `agents` array. Ungated, so it needs no whitelist
   entry.
4. **Non-routable exit code.** `corral send --dir /nonexistent x` exits 1 and
   prints `directory_not_known`.

## Documentation Updates

- **AGENTS.md**, the "Interfaces to the Outside World" entry for CLI `corral`:
  add the two subcommands, the exit-code contract, and the sentence that the CLI
  is gated like an adapter rather than trusted like the operator.
- **README.md**, three lines under Messaging: how to send from a script, and the
  one-time approval per pair.
- **SECURITY.md**: the Trust Boundaries bullet described above.
- **CONVENTION.md**: no change. The CLI is a client of the already-specified
  control-socket protocol, not a new contract.

## Rejected Alternatives

- **An ungated CLI carrying operator trust.** Rejected on the security ground
  above: every sandboxed agent can exec the binary, so it would be a whitelist
  bypass, not a convenience.
- **A `--from` flag** (or any self-reported sender). That is the forgery
  location-derived identity prevents.
- **A separate `corral-cli` binary.** A second binary is a bigger interface than
  two subcommands on the one that already exists, and bare `corral` keeps its
  behavior either way.
- **`clap`.** Two subcommands, five flags, no dependency needed.
- **The wire client in `crates/board`.** The protocol is shared with the daemon
  and the adapters; `corral-core` is where shared logic belongs, and the GUI can
  reuse it.
- **`corral watch`** (a corral-side file watcher). A watcher belongs to whoever
  owns the watched file, not to the bus.
- **Routing the todo system's wake through `corral send`.** Rejected by that
  project itself, correctly: it knows exactly which agent it wants and can block,
  so it runs the harness directly (`TODO_SYSTEM_SPEC.md`, and "Honest Status"
  above). Recorded here so the next reader does not mistake this spec for that
  project's dependency.

## Decisions (All Resolved)

Kept as written for the record; see the Status header for the answers.

- **D0: implement at all, and how much.** The three ranked options in "Honest
  Status" (neither / both / `list` alone). **Resolved: neither, spec shelved.**
- **D1: `--help`.** Recommended yes: `corral --help`, `-h`, or `help` prints the
  two-subcommand usage and exits, which is a strict improvement over today
  (where `--help` opens the board). It does change the behavior of one existing
  invocation, so it is the operator's call.
- **D2: `createdAt` without a date dependency.** CONVENTION.md listed the field,
  corrald ignores it, and Rust's std has no ISO-8601 formatter. This spec
  recommended a ~12-line pure `iso8601_utc(unix_secs)` helper. **Resolved the
  other way:** nothing in the workspace reads the field at all, so `3bdc44c`
  marked it optional in CONVENTION.md and the CLI omits it. Removing code beat
  adding a date converter for a value no consumer reads.
- **D3: e2e placement.** Recommended inside `nix/tests/scenarios/pi.py` as
  above, rather than a fifth flake check: the CLI needs a live agent anyway, and
  a new VM would cost minutes of CI for four asserts.

## Observation Outside This Scope (Since Fixed)

Writing this spec turned up a doc/code divergence in the security model:
SECURITY.md T2 claimed `fromSession` "is verified against the curated registry"
while no such verification existed in `crates/daemon` (`from_session` was parsed
and used only to build the provenance tag), so any agent could forge a peer's
reply handle and misdirect a recipient's reply.

Calibrated: this was a confused deputy on the *reply* path, not a bypass of the
sender's own gate. The tag's directory half stayed authenticated from the outbox
location, so a forger could not send as another directory; it could only make a
recipient's answer land on some session in a third directory. The giveaway that
the check was intended is T2's next paragraph, which accepts *same-directory*
forgery explicitly, a narrowing that only makes sense if the cross-directory case
were blocked.

**Fixed in `3bdc44c`**, independently of this spec:
`mailbox::session_claims_other_dir` is a pure predicate, and `control.rs`
refuses a message as `malformed` when the registry pins the claimed handle to a
directory other than the authenticated sender's. An id absent from the registry
still routes, deliberately, because an adapter may message before the next
curation tick publishes its record; same-directory siblings remain mutually
forgeable, which T2 accepts explicitly. Three tests cover it. The CLI was never
affected either way, since it sends no `fromSession`.
