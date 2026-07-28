# Multi-Agent Todo System (MVP)

Status: design, revised 2026-07-25 (first draft 2026-07-24). Not implemented.
Corral's three binaries stay untouched; this system is a client of them.

## What This Is

A directory holding a `todos.md` file, watched, whose items get handed to agents
in other directories. You brain-dump ideas into the file as `#todo`; when the
file changes, `corral-todo watch` wakes one long-lived dispatcher agent living in
that directory, which picks the items it judges ready, sends each to a worker
agent through `corral_message_agent`, and moves those lines to `#progress`. A
worker reports back to the todo directory, and the dispatcher moves the line to
`#done`, or to `#blocked` when the work cannot proceed. Task state lives in the
file as tags, so editing a tag by hand is how you start or stop work, and if the
directory is a git repo the file's history is the task log for free.

Two artifacts implement it. The `corral-todo` crate (folder `todo/` in this
repo) holds the file CLI and the watcher loop. `todo/DISPATCHER.md` holds the
dispatcher's policy, symlinked into the live todo directory as its `AGENTS.md`
so every agent started there reads it, however it was started.

## Why It Lives Outside Corral

`VISION.md` states that corral is a bus, not a container, that it never drives an
agent autonomously, and that it should not grow orchestration features. A watcher
that dispatches tasks into agents is exactly the orchestration that rule
excludes, so it cannot live in the board, in `corrald`, or in `corral-core`'s
model.

It does live in this repository, in its own top-level folder, because corral and
the system on top of it are iterated together and a single checkout is faster to
move. Sharing the workspace is not sharing the design: `corral-todo` is a fifth
crate that *consumes* `corral-core` (scan a registry directory, inject a prompt
into a socket, launch an agent) exactly as an outside program would, and none of
`corral`, `corral-gui` or `corrald` learns that it exists.

Putting the policy in an agent rather than in a program is what keeps the design
this small. An LLM agent already holds `corral_message_agent`,
`list_corral_agents` and `corral_stop_agent`, plus file tools. Judging whether an
item is ready, choosing who gets it, and deciding when it is finished all happen
inside that agent's reasoning. Corral needs no task model, no scheduler, no
kanban surface, and no new column.

The one thing a program does own is the file, because two writers can reach it
(the dispatcher and you, in an editor) and because a garbled `todos.md` is the
system's only state. So every write goes through `corral-todo`, which locks,
rewrites atomically, and enforces the line grammar.

## The Loop

```
todos.md  (edited by you, or by corral-todo on the dispatcher's behalf)
   |
   |  corral-todo watch: every few seconds normalize ids, hash the file
   v
change?  --> is an agent live in the todo dir?  (scan <dir>/.corral/registry)
              |                        |
              | no                     | yes
              v                        v
         launch it hidden          inject the wake into its socket
         with the wake as its      (core::prompt::send_prompt, the same
         first message             ungated path the board's `m` uses)
              |                        |
              +-----------+------------+
                          v
                    dispatcher agent (one, long-lived, hidden)
                      corral-todo list --tag todo
                      pick the ready items, respecting the in-flight cap
                      corral_message_agent(target_dir=..., message=item + id
                                           + "report to <todo dir>")
                      corral-todo set <id> progress --target <dir>
                          |
      worker agents  <----+
            |
            |  corral_message_agent(target_dir=<todo dir>, "item a7f done: ...")
            v
      corrald reuses the live dispatcher (or spawns one), which runs
      corral-todo set a7f done   /   set a7f blocked --note "<reason>"
```

Corral is not in the wake path at all. The watcher reads the todo directory's own
registry record and writes to the socket beside it, which it may do because it
runs as the operator, on the operator's side of the trust boundary, and because
the record's physical location proves the directory it belongs to. Corral is in
the fan-out path (dispatcher to workers) and the report path (workers back to the
todo directory), both of which are ordinary gated agent messaging.

## The File Format

One item per line, state carried by a tag, plus an identity the CLI maintains.
Four tags, and no others:

- `#todo`: captured, not started. The default when you dump an idea.
- `#progress`: handed to an agent, work in flight.
- `#done`: the worker reported completion.
- `#blocked`: the work cannot proceed, and a human has to look. A worker that
  reports failure, and an item too vague to dispatch, both land here.

The fourth tag exists to break a loop the three-tag version had: an item that
failed and went back to `#todo` would be dispatched again on the next wake, and
again after that, burning tokens forever. `#blocked` is the state a dispatcher
never picks up, so returning to work is a human retagging one word.

```markdown
# Todos

- #todo [a7f] add a --dry-run flag to the deploy script, in ~/projects/deploy
- #progress [k2q -> /home/me/projects/api @01H2XABC] review the auth refactor, findings to REVIEW.md
- #blocked [m4z] port the parser tests
  note (2026-07-25): worker reports the fixture format is undecided
- #done [b8c] bump the pinned toolchain
```

The bracket after the tag carries the dispatch facts: a short id the CLI coins,
optionally the resolved target directory after `->`, and optionally the worker's
session id after `@`, learned from the provenance tag of the worker's first
message. The id is what makes a report unambiguous: the dispatcher quotes it in
the task message and asks for it back, so any dispatcher, with no memory of the
dispatch, can close the right line. The target directory and session id are
recorded for you and for a later liveness check, not for matching.

The rest of the line is free-form prose that names its own target directory. The
dispatcher resolves that to a `target_dir`; the format stays loose deliberately,
because a stricter schema buys nothing the agent cannot infer and it would make
the file worse to type into. An indented line under an item is a note, appended
by the CLI (`--note`) or by you, and is never rewritten.

## The CLI: corral-todo

A Rust workspace crate in `todo/`, binary `corral-todo`, with a pure core
(parse, coin ids, retag, render) and a thin shell around it. Rust rather than a
script so the format has one tested implementation, shipped as one binary by the
existing flake, on the same toolchain and under `just test` and `just lint` with
everything else.

```
corral-todo list [--tag <tag>]        # normalize ids, print id\ttag\ttarget\tsession\ttext
corral-todo add "<text>"              # append a #todo item, print its id
corral-todo set <id> <tag> [--target <dir>] [--session <id>] [--note <text>]
corral-todo watch [--dir <dir>] [--interval <secs>] -- <dispatch argv...>
```

Every subcommand that writes takes an exclusive `flock` on `todos.md`, rewrites
through a temporary file plus rename, and releases. A read-modify-write is
therefore atomic against a second dispatcher, against the watcher's own
normalization, and against your editor if the editor honors the lock. `set` with
an unknown id exits nonzero and says so; nothing fails quietly.

`list` normalizing ids is what lets an item be referred to at all: you type prose
with no id, and the first `list` (the watcher runs one before hashing) gives it
one. Normalizing inside `list` rather than in a separate command means there is
no way to read the file and see an unidentified item.

## The Watcher

`corral-todo watch` polls the file every few seconds, normalizes ids, hashes the
result, and on a change ensures exactly one dispatcher is awake in the todo
directory. Normalizing before hashing is what keeps id assignment from counting
as a change, so a fresh brain-dump costs one wake, not two.

Waking has two branches, both from `corral-core`. If a record in
`<dir>/.corral/registry` resolves to a live socket, the watcher writes the wake
message into it with `core::prompt::send_prompt`. If none does, it launches the
configured dispatcher argv with `core::launch::TerminalLauncher`, hidden (a
headless `cage`, so no window appears), passing the wake as the launch message so
delivery is atomic and no announce race exists.

The dispatcher argv is not defaulted: `corral-todo watch --dir ~/todos -- pi`
names the harness explicitly, mirroring corral's own rule that it never names an
agent kind. Any harness that accepts an initial message as a trailing argument
works.

Polling was chosen over inotify on purpose. A few seconds of latency is invisible
for a todo board, hashing gives idempotence for free, and there is no
watch-descriptor bookkeeping to get wrong when an editor replaces the file rather
than writing into it.

The watcher picks whatever agent is live in the todo directory, by convention
that the directory hosts only the dispatcher. That is the same semantics
`corrald` gives a `target_dir` message ("reach whoever works there"), so both
wake paths agree, and there is no dispatcher-identity file to keep in sync.

Lifecycle belongs in `~/nixos`, not here: a systemd user service runs
`corral-todo watch` with restart-on-failure, exactly as one runs `corrald`.

## The Dispatcher Agent

One long-lived agent session per todo directory, hidden, whose policy is
`todo/DISPATCHER.md` symlinked as the directory's `AGENTS.md`. It tells the
agent to:

1. Read the list with `corral-todo list`, never by hand-parsing the file.
2. For each `#todo` item, judge readiness: enough detail to act on, target
   directory exists, no unmet dependency on an item still in flight, and the
   in-flight cap (`#progress` count, default 3) not yet reached.
3. Send each ready item with `corral_message_agent`, whose message states the
   item id, the task, the absolute path of the todo directory, and the
   instruction to report completion or blockage back to that directory quoting
   the id. Then `corral-todo set <id> progress --target <dir>`.
4. On a worker report, `corral-todo set <id> done`, or `set <id> blocked --note
   "<reason>"`, recording the reporter's session id with `--session` when the
   provenance tag supplies one.
5. Write nothing when nothing needs changing.

Readiness is a judgment, not a rule, which is the point of using an agent. An
item too vague to act on stays `#todo` with a note asking the question, or moves
to `#blocked` if it cannot be acted on at all.

The policy lives in an auto-loaded `AGENTS.md` rather than in a prompt the
watcher passes, because the dispatcher is not always started by the watcher: a
worker's report can spawn one through `corrald`, which passes only the report and
the swarm charter. An agent that finds itself in the todo directory must be able
to learn its job from the directory. The wake message the watcher sends is
therefore one line, not a prompt document, and there is exactly one copy of the
policy.

The dispatcher being long-lived rather than one-shot is what makes reports safe.
A short-lived print-mode dispatcher still has to load the corral adapter (that is
where `corral_message_agent` comes from), so it announces itself as live, and a
report arriving during its single turn is injected into a process about to exit
and is lost. A persistent session queues that report as a follow-up turn
instead. It also stops re-reading the whole list from cold on every wake.

Statelessness survives as a recovery property rather than a per-wake cost: the
file holds all the state, so killing the dispatcher (board `d`, or when its
context grows unwieldy) loses nothing, and the next wake launches a fresh one
that reads the file and carries on.

## Serialization and Loop Safety

Writes serialize twice over. One dispatcher per directory means one turn at a
time, since the harness queues an injected prompt as a follow-up rather than
running it concurrently, and every write goes through `corral-todo`'s lock, which
covers the case that a second agent, or your editor, touches the file anyway.

Convergence needs one condition: **the dispatcher must not write to `todos.md`
when nothing needs changing.** Given that, a dispatch costs exactly one extra
wake. The dispatcher retags an item to `#progress`, the hash changes, the watcher
rings once more, the dispatcher finds no ready `#todo` and no new report, writes
nothing, and the system settles. No debounce beyond the poll interval, no pausing
the watcher, and no state shared between the watcher and the agent.

## What Corral Provides

Corral contributes the fan-out and the view, both of which already exist. The
dispatcher reaches workers in other directories with `corral_message_agent`,
which is the one path a sandboxed agent has across a workdir boundary, and it can
survey who is available with `list_corral_agents` before choosing. Every session
the system starts shows up on the board as an ordinary card, so the operator
watches and intervenes with the keys they already use. A worker's report rides
the same messaging path back, addressed to the todo directory.

Authorization keys on the `(sender dir -> target dir)` pair alone, and it is
directional, so a working pair needs two approvals: `(todo -> worker)` for the
dispatch and `(worker -> todo)` for the report. The `hidden` flag is window
placement and has no bearing on approval.

## Prerequisites and First Run

The live todo directory (say `~/todos`) exists outside this repository, holds
`todos.md` and an `AGENTS.md` symlink to `todo/DISPATCHER.md`, and is ideally its
own git repo so the task log accumulates. Outside, because pi concatenates every
`AGENTS.md` up the tree: a todo directory nested in this repo would feed corral's
own 9.6k-word architecture document into every dispatcher, at a cost in tokens
and in confusion about what the agent is supposed to be working on.

`corrald` runs (worker reports need it; the wake path does not). `corral-todo` and
the harness are on PATH, including inside the workers' sandboxes for anything they
run. The operator approves the two pairs per worker directory on first use.

Corral itself needs nothing: no new binary, no new flag, no configuration.

## Known Limits (MVP, Deliberate)

Nothing supervises a worker that stops without reporting. Its item sits at
`#progress` until you notice and retag it. The recorded target directory and
session id make a liveness check cheap for whenever a sweep is added, and the
board still shows the session.

The watcher learns nothing about whether a dispatch succeeded. It wakes the
dispatcher and returns to polling; whether a worker was actually reached shows up
in the file (a tag that moved) and on the board (a card that appeared), not in an
exit code.

A `#blocked` item needs a human. That is the point of the tag, but it does mean
the system stalls silently on ambiguity unless you read the file.

Delivery to a Running worker queues the message as a follow-up, so a dispatch can
intrude on a human-driven session. The provenance tag makes that visible.

The dispatcher's context grows for as long as it lives, and nothing compacts or
restarts it. Kill it when it gets fat; the file is authoritative.

One watcher per todo directory, and one dispatcher per directory by convention.
Two watchers on one directory would wake two dispatchers, which the file lock
keeps from garbling the file but which would still double-dispatch. If some other
agent happens to be live in the todo directory, the watcher will wake that one
instead, since it addresses the directory rather than a session.

## Rejected Alternatives

A fresh short-lived dispatcher per change, the first draft's shape. It is
appealingly stateless, but it must load the corral adapter for the messaging
tool, so it announces as live and can swallow a report that arrives during its
one turn; and it pays to re-read the list from cold every time.

Reports addressed to the dispatcher's exact session (`target_session` from the
provenance reply handle) rather than to the directory. It works, but it makes
completion depend on resuming one specific dormant session, and with ids in the
file the transcript is not needed to match a report to a line anyway. Addressing
the directory lets any dispatcher, fresh or live, close any item.

A `corral send` CLI as the wake path, specified in the shelved
`docs/superpowers/specs/2026-07-24-corral-cli-design.md`. It adds a subcommand
plus the first Rust control-socket client to corral, routes every wake through
`corrald`, needs a `(todo -> todo)` whitelist approval, and can only ever report
"accepted for routing". Injecting into the dispatcher's socket from the watcher
removes all four costs, and is legitimate precisely because the watcher is
operator-side, like the board's `m`.

Free-form agent edits to `todos.md`, with the write race and format drift as
documented limits. Cheaper to build and slightly nicer prose, but the file is the
only state the system has, and two possible writers plus a hand-maintained id
grammar is exactly where a small tested program earns its place.

A sidecar `state.json` mapping items to sessions, keeping `todos.md` clean prose.
Rejected because two files fall out of sync, and because the bracket is
short enough to read past.

Putting the watcher in `corrald` would make the trusted singleton broker also the
orchestrator, contradicting `VISION.md` and growing the one process whose parsing
bugs carry full authority.

Putting task state and a kanban surface in the board would break the board's
definition as a pure viewer of the registry, since task state is not registry
state, and it would double the work under the TUI/GUI parity rule.

Suppressing wakes while the dispatcher is Running, instead of relying on the
no-write-when-nothing-changed condition. It needs the watcher to read board
state, and it would drop a real edit made mid-turn.

## Open Questions

Whether the watcher should also wake on a timer, so in-flight items get swept for
stalled workers. The long-lived dispatcher makes this nearly free (one more
injected message, "sweep the in-flight items"), which is an argument for adding
it as soon as the first worker dies quietly. The MVP still says no, so every
dispatch traces back to an edit.

Whether `#done` items should move to a separate file or a `done/` archive once
the list grows unwieldy. Deferred until it actually does.

Whether one dispatcher per todo directory scales to several todo directories, or
whether that wants a different shape. Not a question the MVP has to answer.

Whether the dispatcher should self-manage its context (compact, or ask to be
restarted) rather than waiting to be killed.

## When Implementing

Update the repository's `AGENTS.md` and `README.md` at the same time, per their
own hard rule, and add the crate to the workspace, the flake package, and
`just test`. The VM end-to-end rule in `AGENTS.md` covers corral's adapters,
board and daemon; this system's tests are the crate's unit tests over the pure
format core, plus a manual run against a real todo directory.
