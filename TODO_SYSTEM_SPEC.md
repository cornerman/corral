# Multi-Agent Todo System (MVP)

Status: design, not implemented (2026-07-24). Needs no changes to corral.

## What This Is

A directory holding a `todos.md` file, watched, whose items get handed to agents.
You brain-dump ideas into the file as `#todo`; when the file changes, a
dispatcher agent starts, picks the items it judges ready, sends each to an agent
through corral, and marks it `#progress`, then `#done` when the worker reports
back. Task state lives in the file as tags, so editing a tag by hand is how you
start or stop work. If the directory is a git repo, the file's history becomes
the task log for free.

The whole system is a Markdown file, a poll loop, and an agent with a prompt.
Corral is not modified and is not in the wake path; the dispatcher uses corral's
existing agent tools to reach workers, and the board shows the resulting sessions.

## Why It Lives Outside Corral

`VISION.md` states that corral is a bus, not a container, that it never drives an
agent autonomously, and that it should not grow orchestration features. A watcher
that dispatches tasks into agents is exactly the orchestration that rule
excludes, so it cannot live in corral, in `corrald`, or in the board.

Putting the policy in an agent rather than in a program is what keeps the design
this small. An LLM agent already holds `corral_message_agent`,
`list_corral_agents` and `corral_stop_agent`, plus file tools to read and rewrite
`todos.md`. Judging whether an item is ready, choosing who gets it, and deciding
when it is finished all happen inside that agent's reasoning. Corral needs no
task model, no scheduler, no kanban surface, and no new column.

Waking the dispatcher needs no corral involvement either, because the watcher can
simply run the harness. That is the difference between this design and its first
draft, which routed the wake through a new `corral send` CLI. Calling the agent
directly is smaller in every dimension: no new corral code, no daemon in the
path, and no whitelist pair to approve for the wake itself.

## The Loop

```
todos.md (edited by you, or by the dispatcher)
   |
   |  poll every few seconds, hash the meaningful content
   v
watcher  ---- content changed? ---->  run the harness in <todo-dir>, synchronously:
                                          pi "<todos.md contents> ... dispatch the ready items"
                                          |
                                          v
                                      dispatcher agent:
                                        pick the ready #todo items
                                        corral_message_agent(target_dir=..., message=...)
                                        rewrite those items to #progress
                                        exit
                                          |
                    worker agents  <------+
                          |
                          |  reply on the provenance reply handle
                          v
                      corrald resumes the dormant dispatcher with the reply,
                      which rewrites the item to #done
```

The watcher waits for the dispatcher to exit before polling again, so exactly one
dispatcher ever runs. Serialization is a property of the loop rather than a rule
anyone has to enforce.

## The File Format

One item per line, state carried by a tag. Three tags, and no others:

- `#todo`: captured, not started. The default when you dump an idea.
- `#progress`: handed to an agent, work in flight.
- `#done`: the worker reported completion.

```markdown
# Todos

- #todo add a --dry-run flag to the deploy script, in ~/projects/deploy
- #todo review the auth refactor in ~/projects/api and write findings to REVIEW.md
- #progress port the parser tests to the new fixture format (~/projects/api)
- #done bump the pinned toolchain
```

An item names its own target directory in prose. The dispatcher reads that and
resolves it to a `target_dir`; the format stays free-form deliberately, because a
stricter schema buys nothing the agent cannot infer, and it would make the file
worse to type into.

## The Watcher

A loop that polls `todos.md` every few seconds, hashes its meaningful content,
and on a change runs the harness in the todo directory with a fixed instruction,
waiting for it to finish. A shell script or a systemd timer is enough:

```sh
# sketch, not the implementation
last=""
while sleep 5; do
  now=$(sha256sum todos.md | cut -d' ' -f1)
  [ "$now" = "$last" ] && continue
  last=$now
  pi "$(cat PROMPT.md)"   # blocks until the dispatcher exits
done
```

Polling was chosen over inotify on purpose. A few seconds of latency is
invisible for a todo board, hashing gives idempotence for free, and there is no
watch-descriptor bookkeeping to get wrong when an editor replaces the file rather
than writing into it.

Pass the file's current contents in the prompt, and name its path too, so the
dispatcher can rewrite it. Embedding the contents saves a read; naming the path
keeps the file the single source of truth, which matters because the dispatcher
must write back to exactly the file the watcher hashes.

The harness must be reachable from the watcher's environment and must accept an
initial message, which pi does as a trailing positional argument. Any harness
with that shape works; nothing here is pi-specific beyond the invocation.

## The Dispatcher Agent

An ordinary agent session in the todo directory, started fresh by the watcher on
each change. Its prompt tells it to:

1. Read the todo list (given in the prompt, and on disk at the named path).
2. For each `#todo` item, judge whether it is ready to start: enough detail, its
   target directory exists, no unmet dependency on an item still in flight.
3. Send each ready item to its target with `corral_message_agent`, then rewrite
   that line's tag to `#progress`.
4. When a worker reports completion on the reply handle, rewrite that line to
   `#done`.
5. Write nothing if there is nothing to change.

Readiness is a judgment, not a rule, which is the point of using an agent. An
item too vague to act on stays `#todo`, and the dispatcher may append a question
to it for you to answer.

A short-lived dispatcher loses no state, because the file holds all of it. Step 4
happens in a later session: the dispatcher exits after dispatching, its record
goes dormant, and a worker's reply resumes it through corrald with that reply as
its first prompt. So completion handling costs nothing extra to build.

## Loop Safety

The dispatcher writes to the file the watcher watches, so the design has to
converge rather than oscillate. It does, under one condition: **the dispatcher
must not write to `todos.md` when nothing needs changing.**

Given that, a dispatch costs exactly one extra wake. The dispatcher rewrites an
item to `#progress`, the content hash changes, the watcher rings once more, the
dispatcher reads the list, finds no ready `#todo` and no newly finished item,
writes nothing, and the system settles. No debounce beyond the poll interval, no
pausing the watcher, and no state shared between the watcher and the agent.

Making the watcher suppress wakes while the dispatcher runs is unnecessary here,
since the watcher blocks on the dispatcher anyway.

## What Corral Provides

Corral contributes the fan-out and the view, both of which already exist. The
dispatcher reaches workers in other directories with `corral_message_agent`,
which is the one path a sandboxed agent has across a workdir boundary, and it can
survey who is available with `list_corral_agents` before choosing. Every session
the system starts shows up on the board as an ordinary card, so the operator
watches and intervenes with the keys they already use.

Authorization keys on the `(sender dir -> target dir)` pair alone. A whitelisted
pair goes straight through, anything else asks the operator once. The `hidden`
flag is window placement and has no bearing on approval.

## Prerequisites and First Run

The todo directory exists, holds `todos.md` and the dispatcher prompt, and has a
harness on PATH. The operator approves one pair per worker directory the
dispatcher fans out to, `(todo -> <worker dir>)`, on first use. The wake path
needs no approval at all, since it never touches corrald.

Corral itself needs nothing: no new binary, no new flag, no configuration.

## Known Limits (MVP, Deliberate)

Nothing supervises a worker that stops without reporting. Its item sits at
`#progress` until you notice and retag it; the board still shows the session, so
you can look.

The watcher learns nothing about whether a dispatch succeeded. It starts the
dispatcher and waits; whether the dispatcher actually reached a worker shows up in
the file (a tag that moved) and on the board (a card that appeared), not in an
exit code.

Delivery to a Running worker queues the message as a follow-up, so a dispatch can
intrude on a human-driven session. The provenance tag makes that visible.

Two watchers on one directory would run two dispatchers and race on the file,
which has no lock. Run one.

A fresh dispatcher per change spends tokens re-reading the list every time. That
is the price of statelessness, and it is what makes the file authoritative.

## Rejected Alternatives

A `corral send` CLI as the wake path, specified before this revision. It works,
but it adds a subcommand plus the first Rust control-socket client to corral,
routes every wake through corrald, needs a `(todo -> todo)` whitelist approval,
and can only ever report "accepted for routing" rather than delivery. Calling the
harness directly removes all four costs.

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

Whether the dispatcher should ever start work unprompted, on a timer, rather than
only when the file changes. The MVP says no, so every dispatch traces back to an
edit.

Whether `#done` items should move to a separate file or a `done/` archive once
the list grows unwieldy. Deferred until it actually does.

Whether one dispatcher per todo directory scales to several todo directories, or
whether that wants a different shape. Not a question the MVP has to answer.
