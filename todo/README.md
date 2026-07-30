# corral-todo

A watched `todo.txt` whose items get handed to fresh corral agents. You write a
line, a dispatcher agent reads it, picks a worker directory, and starts an agent
there; the worker reports back and the line closes.

Design: [SPEC.md](SPEC.md). Dispatcher policy: [DISPATCHER.md](DISPATCHER.md).

## Quick Start

```sh
corral-todo init ~/todos              # writes DISPATCHER.md + todo.txt, prints
                                      # the whitelist lines you need to add
cd ~/todos && git init                # optional: the task log accumulates
echo "add a --dry-run flag to the deploy script in ~/projects/deploy" >> todo.txt
corral-todo watch --dir ~/todos -- pi # names the harness, always
```

The watcher polls every 5 seconds (`--interval`), and on a change wakes exactly
one dispatcher in that directory: injecting into its live socket, else resuming
its dormant session, else starting one hidden. Nothing pops a window.

## The Policy Is `DISPATCHER.md`, and Nothing Auto-Loads It

`init` writes the dispatcher's operating policy to `~/todos/DISPATCHER.md`, from a
copy embedded in the binary. Three consequences worth knowing:

- **Not `AGENTS.md`.** That name is ambient: it would govern every agent that ever
  runs in the todo directory, including your own interactive session. The policy
  belongs to one role, so it carries that role's name.
- **The prompt is what loads it.** Every wake message names the file, so a
  dispatcher reads it on its first turn and again after a context compaction. This
  is also why the todo system works with any harness: it relies on an agent being
  able to read a file, not on a harness-specific config name (`AGENTS.md` for pi
  and opencode, `CLAUDE.md` for Claude Code, `GEMINI.md` for Gemini CLI).
- **The copy in your directory wins, and it is yours.** Tune it as you learn what
  the dispatcher gets wrong; the change takes effect on its next turn, with no
  rebuild. `init` refuses to overwrite it without `--force`. No symlink points back
  into the corral checkout, so a `git pull` there cannot silently change how your
  dispatcher behaves.

`watch` refuses to start when the file is missing, rather than waking a generic
agent that cannot interpret "run your dispatcher loop".

## The Todo Directory Lives Outside This Repository

`~/todos`, not `corral/todo/live`. pi concatenates every `AGENTS.md` up the
directory tree, so a todo directory nested in this repository would feed corral's
own architecture document (about 10k words) into every dispatcher, at a cost in
tokens and in confusion about what the agent is supposed to be working on. The
dispatcher's own policy avoids that name for the same reason, one level down.

## Prerequisites

- **`corrald` runs.** Spawning workers and receiving their reports go through it.
  The wake path does not, so a todo directory still normalizes and wakes without it.
- **`corral-todo` and your harness are on `PATH`**, including inside a worker's
  sandbox for anything the worker itself runs.
- **Both whitelist directions per worker directory**, in `~/.corral/whitelist`
  (`init` prints these with your todo directory filled in):

  ```
  /home/me/todos -> /home/me/projects/deploy
  /home/me/projects/deploy -> /home/me/todos
  ```

  Authorization is directional and keyed on the directory pair, so a working pair
  needs two lines: one for the spawn, one for the handshake and the report.
  Clicking "Allow always" twice on corrald's tray does the same thing. The file is
  re-read every tick, so no restart is needed. `corral-todo` prints rather than
  writes it: the whitelist grants cross-directory authorization and stays
  operator-owned (see `SECURITY.md`).
- **The worker directory is known to corrald** (some session ran there once).
  A spawn into a directory corral has never seen is acked `directory_not_known`.

## The CLI

```
corral-todo init <dir> [--force]     # DISPATCHER.md + todo.txt + whitelist hints
corral-todo list [--open|--status <open|progress|blocked|done>]   # dispatch order
corral-todo add "<text>"
corral-todo set <id> <state> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive                  # completed lines move to done.txt
corral-todo watch [--dir <dir>] [--interval <secs>] -- <harness argv...>
```

`list` prints `<id> <state> <priority> <created> [target:] [worker:]  <text>`,
sorted into dispatch order: priority `(A)` before `(Z)`, then oldest first, with
unprioritized items last. The tool owns that rule so the dispatcher does not have
to reconstruct it on every wake.

No subcommand picks a harness. `watch`'s argv after `--` names the dispatcher's
kind and has no default; a **resume** instead uses the record's own
`resumeCommand`, so the kind that ran there before wins and the session id
survives. Worker kinds are the dispatcher's choice, via `corral_spawn_agent`'s
optional `label`.

The file is `--file`, else `$CORRAL_TODO_FILE`, else `./todo.txt`. Every write
takes an exclusive `flock` on `<file>.lock` and rewrites through a temp file plus
rename, so your editor, the dispatcher and the watcher cannot corrupt each other.

Reading coins an `id:` for any line lacking one and stamps a creation date, so
there is no way to see an unidentified item. Ids are what match a worker's report
back to a line, which is why the dispatcher quotes them at workers.

## State Lives in the Line

```
2026-07-25 add a --dry-run flag to the deploy script id:a7f +deploy
(A) 2026-07-25 review the auth refactor id:k2q status:progress target:/home/me/projects/api worker:01H2XABC
2026-07-24 port the parser tests -- blocked: which fixture format? id:m4z status:blocked
x 2026-07-25 2026-07-23 bump the pinned toolchain id:b8c
```

No `status:` key means open, and open is the only state the dispatcher picks up.
`status:blocked` exists to break a loop: an item that failed and returned to open
would be dispatched again forever, so returning it to work is a human deleting
one word. Everything else in a line is yours, including `+projects`, `@contexts`
and a `(A)`-`(Z)` priority the dispatcher reads as an ordering hint.

## Watching What It Does

The watcher logs one line per wake and nothing at all while the system is settled:

```
corral-todo watch: wake 69fd22429f88cda6 via inject (1 item, 1 open)
corral-todo watch: wake 28fe559c13967b0f via inject (2 items, 2 open)
```

The fingerprint is the hash of the normalized file, which makes the property that
matters legible. Wakes with **different** fingerprints in a row mean the dispatcher
keeps changing the file, so it is not converging — the one failure mode that burns
tokens indefinitely. The **same** fingerprint twice means a wake failed and was
retried. Silence means settled.

To see the dispatcher's reasoning rather than its effects, open `corral`, select its
card, and press `o`: that fetches the session's full transcript and opens it.

## Running It As A Service

Lifecycle is deployment glue, not code here. Run `corral-todo watch` from a
systemd user service with restart-on-failure, the same way `corrald` runs. The
watcher is a separate process from `corrald` on purpose: a todo.txt parse failure
or a stuck lock must not take down messaging for every agent on the host.
