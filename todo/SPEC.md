# Multi-Agent Todo System (MVP)

Status: design, revised 2026-07-25 against the four-verb tool surface
(CONVENTION.md v3). Not implemented. Corral's three binaries stay untouched; this
system is a client of them.

## What This Is

A directory holding a `todo.txt` file, watched, whose items get handed to fresh
agents in other directories. You brain-dump ideas into the file as ordinary
todo.txt lines; when the file changes, `corral-todo watch` wakes one long-lived
dispatcher agent living in that directory, which picks the items it judges ready,
starts a worker per item with `corral_spawn_agent`, and marks those lines
`status:progress`. The worker confirms the task, works, and reports back to the
dispatcher's session, which completes the line, or marks it `status:blocked` when
the work cannot proceed. Task state lives in the file, so editing a line by hand
is how you start or stop work, and if the directory is a git repo the file's
history is the task log for free.

Two artifacts implement it. The `corral-todo` crate (folder `todo/` in this repo)
holds the file CLI and the watcher loop. `todo/DISPATCHER.md` holds the
dispatcher's policy, symlinked into the live todo directory as its `AGENTS.md` so
every agent started there reads it, however it was started.

## Why It Lives in the Workspace but Integrates Seamlessly

`VISION.md` states that corral is a bus, not a container, that it never drives an
agent autonomously, and that it should not grow orchestration features. A watcher
that dispatches tasks into agents is exactly the orchestration that rule
excludes, so it cannot live in `corrald` or be driven by corral's background core.

We integrate the *visualization* and *triggers* directly into corral's boards.
The `corral-todo` parser and locking logic are built as a highly cohesive library
in the workspace. `crates/core` uses this library to read the global `todo.txt`
file and render it on your card board!

This gives you a unified workspace without violating our principles:
1. **Separation of Concerns:** The board has zero scheduling logic. It merely
   renders static `todo.txt` open/blocked cards, and delegates scheduling to the
   dispatcher agent.
2. **Direct Operator Triggers:** When you drag/move a task card from TODO to
   Progress on the board, the board performs a safe, locked edit on the file
   to set `status:progress` and sends a wake-up prompt to the dispatcher. The
   dispatcher remains the sole entity responsible for spawning and managing the
   handshake.
3. **No Duplication:** In-progress tasks are automatically hidden from the TODO
   column, appearing only as they live in the registry columns.

The file remains the absolute source of truth. Every edit—whether by you in an
editor, the dispatcher, or the board—goes through `corral-todo` crate helpers
which lock, write atomically, and enforce the todo.txt grammar.

## The Loop

```
todo.txt  (edited by you, the board, or the dispatcher under lock)
   |
   +-- (corral-todo watch: normalize, hash, poll ~5s) --> change?
                                                             |
   +-- wake live socket / resume dormant / launch hidden <---+
   v
 dispatcher agent (one, long-lived, hidden)
   corral-todo list --open
   pick ready items (priority sorting, cap, no-two-workers-per-dir rule)
   corral_spawn_agent(cwd=<target>, task=<item id + instructions>)
   corral-todo set a7f progress --target <dir> --worker <session id>
       |
  worker agent <-- (fresh, charter-prefixed, hidden)
       |
       | 1. handshake: task in own words + questions ---> dispatcher (Provenance tag)
       | 2. dispatcher answers with a go-ahead
       | 3. result: "item a7f done/blocked: ..." ---> dispatcher
       v
  corrald delivers to dispatcher's session.
  Dispatcher runs corral-todo set a7f done / blocked.
```

The board is a peer actor: dragging a card on your board writes the state update
directly to `todo.txt` under lock and wakes the dispatcher, which handles the
spawn. The worker session's active progress is then dynamically reflected back.

## Addressing: Spawn Down, Report Up

The four-verb surface (`corral_spawn_agent`, `corral_message_agent`,
`corral_stop_agent`, `corral_list_agents`) has no directory-addressed message, so
the shape of this system follows from it. Work goes **down** as a spawn: one fresh
worker per item, in the item's target directory, with the task as its first
prompt. Reports come **up** as a message to an exact session, named by the reply
handle in the provenance tag, which is the dispatcher's own session id.

Two consequences are worth stating plainly.

First, the dispatcher's session id is the address the whole system converges on,
so the watcher **resumes** the existing session rather than starting a new one
whenever a record is there. A stale handle is not fatal (corrald resumes a dormant
record, so an old dispatcher session wakes, reads the file, and updates it under
the lock), but session churn costs an extra card, an extra context, and a
confusing board.

Second, a spawned worker arrives with corral's charter, which tells it to open
with a task-confirmation handshake and wait for a go-ahead. So dispatching an item
is a short conversation, not a fire-and-forget push: spawn, answer the questions,
then let it work. That is a feature here, since the questions are exactly what a
brain-dumped one-liner is missing, and the dispatcher can push a question it cannot
answer back into the file as `status:blocked` with the question appended.

## The File Format: todo.txt

The file is a [todo.txt](https://github.com/todotxt/todo.txt) file, not a bespoke
dialect, so anything that already edits todo.txt (a text editor, `todo.sh`, a
phone app for capture on the move) works on it unchanged. The format gives four
things this system needs and would otherwise have invented: one task per line,
arbitrary `key:value` metadata, a completion marker, and the `done.txt` archive
convention.

State is read from the line, never from a separate index:

- **todo**, the default: an open line with no `status:` key. What you get by
  typing an idea.
- **`status:progress`**: handed to a worker, work in flight.
- **`status:blocked`**: the work cannot proceed and a human has to look. A worker
  reporting failure, a question only you can answer, and an item too vague to
  dispatch all land here.
- **done**: the standard todo.txt completion form, `x` plus the completion date at
  the start of the line, so ordinary todo.txt tools see it as complete.

`status:blocked` exists to break a loop: an item that failed and went back to open
would be dispatched again on the next wake, and again after that, burning tokens
forever. Blocked is the state the dispatcher never picks up, so returning to work
is a human deleting one word.

```
2026-07-25 add a --dry-run flag to the deploy script id:a7f +deploy
(A) 2026-07-25 review the auth refactor, findings to REVIEW.md id:k2q status:progress target:/home/me/projects/api worker:01H2XABC
2026-07-24 port the parser tests status:blocked id:m4z -- blocked: which fixture format? worker asked
x 2026-07-25 2026-07-23 bump the pinned toolchain id:b8c
```

Metadata keys the system owns: `id:` (a short id `corral-todo` coins for every
item), `target:` (the absolute worker directory, recorded at dispatch), and
`worker:` (the worker's session id, learned from the reply handle on its
handshake, which the charter guarantees arrives). Everything else in the line is
yours: prose, `+projects`, `@contexts`, and a `(A)`-`(Z)` priority the dispatcher
reads as an ordering hint.

The id is what makes a report unambiguous. The dispatcher puts it in the task text
and asks for it back, so a dispatcher with no memory of the dispatch, resumed
purely to receive a report, can still close the right line. `target:` and
`worker:` are recorded for you and for a later liveness check, not for matching.

Prose still carries the intent, including which directory an item is about;
`target:` is the dispatcher's resolution of that prose, not a substitute for it.
Two consequences of todo.txt's grammar are worth stating: a `key:value` value
cannot contain a space, so a target path with a space in it is refused loudly
rather than mangled; and a task is exactly one line, so a blocked reason is
appended to the task text (after ` -- `) rather than living on a note line.

## The CLI: corral-todo

A Rust workspace crate in `todo/`, binary `corral-todo`, with a pure core (parse a
todo.txt line, coin ids, set state, render) and a thin shell around it. Rust
rather than a script so the format has one tested implementation, shipped as one
binary by the existing flake, on the same toolchain and under `just test` and
`just lint` with everything else.

```
corral-todo list [--open|--status <s>]   # normalize, print id, state, target, worker, text
corral-todo add "<text>"                 # append an item, print its id
corral-todo set <id> <state> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive                      # move completed lines to done.txt
corral-todo watch [--dir <dir>] [--interval <secs>] -- <dispatch argv...>
```

Every subcommand that writes takes an exclusive `flock` on `todo.txt`, rewrites
through a temporary file plus rename, and releases. A read-modify-write is
therefore atomic against a second dispatcher, against the watcher's own
normalization, and against your editor if the editor honors the lock. `set` with
an unknown id exits nonzero and says so; nothing fails quietly.

Normalization means coining an `id:` for any item that lacks one and stamping the
todo.txt creation date. It happens inside `list` rather than in a separate
command, so there is no way to read the file and see an unidentified item.

## The Watcher

`corral-todo watch` polls the file every few seconds, normalizes, hashes the
result, and on a change ensures exactly one dispatcher is awake in the todo
directory. Normalizing before hashing keeps id and date stamping from counting as
a change, so a fresh brain-dump costs one wake, not two.

Waking has three branches, all built on `corral-core`. A record in
`<dir>/.corral/registry` that resolves to a live socket gets the wake message
written into it (`core::prompt::send_prompt`). A dormant record (socket cleared)
is resumed through its own `resumeCommand` with the wake as the launch message, so
the session id survives and every reply handle a worker holds stays valid. With no
record at all, the configured dispatcher argv is launched fresh, hidden (a headless
`cage`, so no window appears), again with the wake as the launch message so
delivery is atomic and no announce race exists. If several records exist, the most
recently seen one wins.

The dispatcher argv is not defaulted: `corral-todo watch --dir ~/todos -- pi`
names the harness explicitly, mirroring corral's own rule that it never names an
agent kind. Any harness that accepts an initial message as a trailing argument
works, and a resume uses whatever the record itself declares.

Polling was chosen over inotify on purpose. A few seconds of latency is invisible
for a todo board, hashing gives idempotence for free, and there is no
watch-descriptor bookkeeping to get wrong when an editor replaces the file rather
than writing into it.

Watching from a separate process, rather than from inside the dispatcher, is the
load-bearing choice here. The thing that must not fail is *noticing*: wake logic
inside the dispatcher dies with the dispatcher, so a crashed or never-started
dispatcher would let edits rot with nothing able to say so. A supervisor belongs
outside the process whose liveness is in question, which is also why `corrald` runs
under systemd rather than inside a board. Lifecycle stays in `~/nixos`, not here: a
systemd user service runs `corral-todo watch` with restart-on-failure, exactly as
one runs `corrald`.

## The Dispatcher Agent

One long-lived agent session per todo directory, hidden, whose policy is
`todo/DISPATCHER.md` symlinked as the directory's `AGENTS.md`. It tells the agent
to:

1. Read the list with `corral-todo list --open`, never by hand-parsing the file.
2. For each open item, judge readiness: enough detail to act on, its directory
   resolvable and existing, no unmet dependency on an item still in flight, no
   other in-flight item in that same directory, and the in-flight cap
   (`status:progress` count, default 3) not yet reached. Priority `(A)`-`(Z)`
   orders the candidates.
3. Start a worker with `corral_spawn_agent(cwd = <target dir>, task = <the item,
   its id, and the instruction to report the id back>)`, optionally naming a
   `label` (harness kind) and leaving `window` hidden. Then `corral-todo set <id>
   progress --target <dir>`.
4. Answer the worker's handshake: give the go-ahead, or answer what the item text
   already implies. A question only the human can settle goes into the file as
   `set <id> blocked --reason "<the question>"`, and the worker is stopped with
   `corral_stop_agent`.
5. On a result message, `corral-todo set <id> done`, or `set <id> blocked --reason
   "<why>"`, recording the reporter's session id with `--worker` when the
   provenance handle supplies one.
6. Write nothing when nothing needs changing.

Never two workers in one directory: `VISION.md` is explicit that two agents in one
working tree clobber each other, so an item whose target already has an in-flight
sibling waits, and parallelism comes from items in different directories (or from
you pointing an item at a worktree of its own).

Readiness is a judgment, not a rule, which is the point of using an agent. An item
too vague to act on stays open with a question appended, or goes to
`status:blocked` if it cannot be acted on at all.

The policy lives in an auto-loaded `AGENTS.md` rather than in a prompt the watcher
passes, because the dispatcher is not always started by the watcher: a worker's
report can resume it through `corrald`, which passes only the report. An agent that
finds itself in the todo directory must be able to learn its job from the
directory. The wake message the watcher sends is therefore one line, not a prompt
document, and there is exactly one copy of the policy.

The dispatcher being long-lived rather than one-shot is what makes reports cheap
and its address stable. A short-lived print-mode dispatcher must still load the
corral adapter (that is where the tools come from), so it announces as live, and a
report arriving during its single turn is injected into a process about to exit and
is lost. A persistent session queues that report as a follow-up turn instead, and
keeps one session id for every worker's reply handle.

Statelessness survives as a recovery property rather than a per-wake cost: the file
holds all the state, so killing the dispatcher (board `d`, or when its context
grows unwieldy) loses nothing, and the next wake resumes or relaunches one that
reads the file and carries on.

## Serialization and Loop Safety

Writes serialize twice over. One dispatcher per directory means one turn at a time,
since the harness queues an injected prompt as a follow-up rather than running it
concurrently, and every write goes through `corral-todo`'s lock, which covers the
case that a second dispatcher session (an old one resumed by a stale reply handle)
or your editor touches the file anyway.

Convergence needs one condition: **the dispatcher must not write to `todo.txt`
when nothing needs changing.** Given that, a dispatch costs exactly one extra
wake. The dispatcher marks an item `status:progress`, the hash changes, the watcher
rings once more, the dispatcher finds no ready item and no new report, writes
nothing, and the system settles. No debounce beyond the poll interval, no pausing
the watcher, and no state shared between the watcher and the agent.

## What Corral Provides, and How It Authorizes

Corral contributes the fan-out and the view, both of which already exist. The
dispatcher starts workers in other directories with `corral_spawn_agent`, reaches
them by session with `corral_message_agent`, stops them with `corral_stop_agent`,
and can survey what exists with `corral_list_agents`. That is the one path a
sandboxed agent has across a workdir boundary. Every session the system starts
shows up on the board as an ordinary card, so the operator watches and intervenes
with the keys they already use.

The dispatcher's spawns and messages are gated by the whitelist and the approval
popup, the same as any agent's, while the watcher's wake is ungated like the
board's `m`. That asymmetry is deliberate: `m` is a human pressing a key, whereas
the dispatcher is an LLM reading a file you brain-dump into plus reports written by
other agents, which is untrusted input and therefore a prompt-injection surface.
The whitelist is what bounds where an injected dispatcher could fan out to, so the
gate earns its keep exactly here.

Authorization keys on the `(sender dir -> target dir)` pair alone, and it is
directional, so a working pair needs two grants: `(todo -> worker)` for the spawn
and `(worker -> todo)` for the handshake and the result. Message, stop, hidden
spawn and visible spawn all authorize identically, so one grant per direction
covers everything the pair does. Since `~/.corral/whitelist` is operator-owned and
re-read every tick, setup pre-seeds both lines per worker directory instead of
waiting for a tray click; clicking "Allow always" once does the same thing.

A program can send on that same gated path, which is worth recording even though
the MVP does not use it. Write the submission JSON to `<dir>/.corral/outbox/<name>`
(`id` plus one verb: `{"op":"message","targetSession":..,"message":..}`,
`{"op":"spawn","cwd":..,"task":..,"label"?,"hidden"?}`, or
`{"op":"stop","targetSession":..}`), then send one line
`{"submit":"<abs path>"}` to `~/.corral/corrald.sock` and read the ack
(`accepted`, `approval_needed`, `recipient_not_found`, `directory_not_known`,
`malformed`). corrald resolves the file through `/proc/self/fd`, requires the real
path to be exactly `<cwd>/.corral/outbox/<name>`, derives the sender's directory
from that location, overwrites any `fromCwd` in the content, and consumes the file.
Identity is therefore the directory the outbox file sits in, not a claim, so
`corral-todo` submitting from the todo directory would carry the dispatcher's own
identity and pass the same whitelist and approval popup. That is the sanctioned way
to add a mechanical sender (a sweep, a re-dispatch) if one is ever wanted; the MVP
leaves all sending to the dispatcher's own tools, since a second sender is a second
interface with no use yet.

## Board Integration and Column Mapping

To keep your Screen clean and provide a beautiful, unified workspace, the todo system integrates directly into the horizontal column layout of `corral` and `corral-gui`.

Instead of spreading agent states across five horizontal columns, we consolidate our attention board into an elegant, three-column pipeline:

1. **`TODO` (Column 0 - Left):** Shows open (`id:`) or blocked tasks parsed directly from your global `todo.txt` file. Blocked tasks representing unresolved questions or failures are pinned here with a prominent visual treatment.
2. **`PROGRESS` (Column 1 - Middle):** Shows active worker agent sessions (tasks currently marked with `status:progress`). Internally, these active cards are vertically grouped and sorted:
   - *Requires Action* at the very top (agents blocked on an interactive prompt), ensuring questions appear right in your center of attention.
   - *Idle* in the middle (ready/paused agents).
   - *Running* at the bottom (agents actively executing commands).
3. **`DONE / DORMANT` (Column 2 - Right):** Shows reaped, offline, or completed sessions for historical review and logs.

This 3-column layout fits standard terminal widths seamlessly, avoids card duplication (in-progress items dissolve out of Column 0 and migrate dynamically into Column 1), and makes titles highly readable.

### Drag and Drop Card-Move Transitions

Moving cards across the three columns triggers actual work scheduling:

* **Todo ➔ Progress:** Moving a task card to Column 1 commits an atomic write to `todo.txt` under lock to set `status:progress` on the item, then wakes the dispatcher. The dispatcher parses the target directory, spawns the worker session, and writes its id to `worker:`. Once live, the agent's card dynamically appears on the Progress column.
* **Progress ➔ Todo:** Moving an active card back to Column 0 cancels the task. The board updates `todo.txt` under lock to remove `status:progress` and `worker:` tags, and sends a clean `corral_stop_agent` to halt the worker. The card instantly reverts back into a static thought in the TODO column.
* **Progress ➔ Done/Dormant:** Halts the worker session cleanly (moving its representation to Column 2).
* **Todo ➔ Done/Dormant:** Invalid (ideas cannot be put directly to offline history).

### In-Place Quick-Add and Completion

The board supports rapid, keyboard-centric inbox capture:

* **Add Thought (`a` key):** Hitting `a` in the TUI (or clicking an add block in the GUI) opens an inline text box at the bottom of the TODO column. Type your thought, hit Enter, and the board performs an atomic lock-and-append to `todo.txt`, drawing the new card instantly.
* **Mark Complete (`d` key):** Hitting the dismiss key `d` on any card in the TODO column performs a safe lock-and-edit to prefix the task with standard `x` completion markers in `todo.txt`, removing it from the board.

## Prerequisites and First Run

The live todo directory (say `~/todos`) exists outside this repository, holds
`todo.txt` and an `AGENTS.md` symlink to `todo/DISPATCHER.md`, and is ideally its
own git repo so the task log accumulates. Outside, because pi concatenates every
`AGENTS.md` up the tree: a todo directory nested in this repo would feed corral's
own architecture document (about 10k words) into every dispatcher, at a cost in
tokens and in confusion about what the agent is supposed to be working on.

`corrald` runs (spawns and reports need it; the wake path does not). `corral-todo`
and the harness are on PATH, including inside the workers' sandboxes for anything
they run. The two whitelist pairs per worker directory are seeded or approved. A
target directory must already be known to corrald as a spawnable place (a record
from any session that ran there), since a spawn into a directory corral has never
seen acks `directory_not_known`.

Corral itself needs nothing: no new binary, no new flag, no configuration.

## Known Limits (MVP, Deliberate)

Nothing supervises a worker that stops without reporting. Its item sits at
`status:progress` until you notice and edit the line. The recorded `target:` and
`worker:` make a liveness check cheap for whenever a sweep is added, and the board
still shows the session.

The watcher learns nothing about whether a dispatch succeeded. It wakes the
dispatcher and returns to polling; whether a worker was actually started shows up
in the file (a state that moved) and on the board (a card that appeared), not in an
exit code.

Every item costs a handshake round trip before work starts, since the charter tells
a fresh worker to confirm the task first. Good for a vague one-liner, pure latency
for an obvious one.

A `status:blocked` item needs a human. That is the point of the state, but it does
mean the system stalls silently on ambiguity unless you read the file.

todo.txt has no quoting in `key:value`, so a worker directory whose path contains a
space cannot be recorded and is refused loudly. One line per task also means a
blocked reason rides in the task text rather than on a note line, so lines grow.

The dispatcher's context grows for as long as it lives, and nothing compacts or
restarts it. Kill it when it gets fat; the file is authoritative and the next wake
resumes the same session.

A stale reply handle (a worker holding the id of a dispatcher session that has
since been replaced) still resolves, because corrald resumes the dormant record, so
an old dispatcher session can wake alongside the current one. The file lock keeps
that safe, and both read the same authoritative file, but the board shows two
cards until you dismiss one.

One watcher per todo directory. Two watchers on one directory would wake two
dispatchers, which the lock keeps from garbling the file but which would still
double-dispatch.

## Rejected Alternatives

**Reports addressed to the todo directory** rather than to the dispatcher's
session, which an earlier draft of this spec assumed. CONVENTION.md v3 removed
directory-addressed messaging entirely (a message names an exact session; a
directory is only a spawn target), so this is no longer expressible. It also
turned out unnecessary: `id:` in the file, not a transcript, is what matches a
report to a line, so a resumed dispatcher closes the right item regardless of
which session receives the report.

**A bespoke markdown line format** (`- #progress [a7f @sess] text`), this spec's
first draft. Slightly nicer to read and marginally smaller to parse, but it invents
a dialect where todo.txt already specifies one, and it gives up every existing
editor and capture app.

**todo.md kanban columns** ([todomd.org](https://github.com/todomd/todo.md)):
states as `### Column` headings with GFM checkboxes, which renders on GitHub and
has a VS Code kanban editor. Rejected because per-item metadata is still an
invention there, and because a state change rewrites two sections instead of one
field, which is worse for both diffs and a program holding a lock.

**A deterministic dispatcher with no LLM.** `corral-todo` could announce its own
record and ACP socket (`CONVENTION.md` is harness-neutral, and `VISION.md`
explicitly permits a non-agent participant), receive reports itself, and submit
spawns over `corrald.sock` gated exactly like an agent. It is genuinely smaller,
and it is rejected because the value is the judgment: inferring a target from
prose, deciding readiness, answering a worker's handshake questions, and reading a
free-form report. A fixed report grammar and explicit per-item targets would push
that work back onto the human who is trying to brain-dump.

**An ungated dispatch helper** (`corral-todo dispatch <id> --to <dir>`, injecting
operator-side on the LLM's instruction) to avoid the whitelist grants. Rejected as
a confused deputy: corral's authority acting on an LLM's decision. Bounding it with
an allowlist in the todo directory would only duplicate the whitelist that already
exists. The objection is specific to the *ungated* path; a program submitting over
`corrald.sock` from the todo directory is gated like the agent that lives there and
escalates nothing.

**Bundling the watcher into `corrald`.** It saves one systemd unit and no code,
since the registry scan, prompt injection and launch already live in `corral-core`.
It costs the property that the todo system is deletable without touching corral,
and it puts task policy inside the sealed trusted singleton whose parsing bugs
carry full authority (`SECURITY.md`), which `VISION.md` forbids.

**Watching the file from inside the dispatcher** (a pi extension calling `fs.watch`
and nudging its own session with `pi.sendUserMessage`, about 30 lines). It removes
the watcher process entirely, and it is rejected because it cannot notice anything
while the dispatcher is dead, which is exactly when noticing matters. It is also
per-harness where the watcher is neutral.

**A fresh short-lived dispatcher per change**, the first draft's shape. Appealingly
stateless, but it must load the corral adapter for the tools, so it announces as
live and can swallow a report that arrives during its one turn; it pays to re-read
the list from cold every time; and its session id churns, stranding reply handles.

**Reusing one long-lived worker per directory** instead of spawning per item. A
spawn is the verb that carries a task as a first prompt and arrives with the
charter, and a fresh worker starts with a clean context scoped to one item.
Reusing a session would need `corral_message_agent` plus a roster lookup to find
it, and would mix unrelated tasks in one transcript.

**A `corral send` CLI as the wake path**, specified in the shelved
`docs/superpowers/specs/2026-07-24-corral-cli-design.md`. It adds a subcommand plus
the first Rust control-socket client to corral, routes every wake through
`corrald`, needs a `(todo -> todo)` whitelist grant, and can only ever report
"accepted for routing". Injecting into the dispatcher's socket from the watcher
removes all four costs, and is legitimate precisely because the watcher is
operator-side, like the board's `m`.

**Free-form agent edits to the file**, with the write race and format drift as
documented limits. Cheaper to build, but the file is the only state the system has,
and two possible writers plus a hand-maintained grammar is exactly where a small
tested program earns its place.

**A sidecar `state.json`** mapping items to sessions, keeping the task file clean
prose. Rejected because two files fall out of sync, and because todo.txt
`key:value` metadata is what the sidecar would have held.

**A wide five-column board grid.** Spreading tasks and agents across separate columns for TODO, Action, Idle, Running, and Dormant is rejected because it crowds laptop screen widths and splits focus. The consolidated 3-column stacked pipeline (Todo ➔ Progress ➔ Done) keeps active and inactive states gathered, fits terminal cells perfectly, and increases read space.

**A board with zero todo integration.** Stamping corral as purely a passive viewer of running registry processes is rejected because it misses an incredible UX synergy. Letting the board parse todo.txt under lock elevates the board into a planning command-center, while keeping scheduling boundaries strictly in the dispatcher agent policies.

**Suppressing wakes while the dispatcher is Running**, instead of relying on the
no-write-when-nothing-changed condition. It needs the watcher to read board state,
and it would drop a real edit made mid-turn.

## Open Questions

Whether the watcher should also wake on a timer, so in-flight items get swept for
stalled workers. The long-lived dispatcher makes this nearly free (one more
injected message, "sweep the in-flight items"), which is an argument for adding it
as soon as the first worker dies quietly. The MVP still says no, so every dispatch
traces back to an edit. A sweep is also the first plausible use for a gated
program-side submit, since asking a recorded `worker:` session for its status needs
no judgment.

Whether the dispatcher should stop a worker once its item is done, or leave the
session dormant for its transcript. Stopping keeps the board clean; leaving it
keeps the work reviewable, and `d` on the board does either.

When `corral-todo archive` should run: on the dispatcher's judgment, on a size
threshold, or only when you ask. todo.txt's `done.txt` convention settles where
completed items go; it does not settle when.

Whether one dispatcher per todo directory scales to several todo directories, or
whether that wants a different shape. Not a question the MVP has to answer.

Whether the dispatcher should self-manage its context (compact, or ask to be
restarted) rather than waiting to be killed.

## When Implementing

Update the repository's `AGENTS.md` and `README.md` at the same time, per their own
hard rule, and add the crate to the workspace, the flake package, and `just test`.
The VM end-to-end rule in `AGENTS.md` covers corral's adapters, board and daemon;
this system's tests are the crate's unit tests over the pure todo.txt core, plus a
manual run against a real todo directory.
