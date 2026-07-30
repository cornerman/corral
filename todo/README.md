# corral-todo

A watched `todo.txt` whose items get handed to fresh corral agents. You write a
line, a dispatcher agent reads it, picks a worker directory, and starts an agent
there; the worker reports back and the line closes.

Design: [SPEC.md](SPEC.md). Dispatcher policy: [DISPATCHER.md](DISPATCHER.md).

## Quick Start

```sh
mkdir ~/todos && cd ~/todos
git init                                             # the task log accumulates
ln -s ~/projects/corral/todo/DISPATCHER.md AGENTS.md # the dispatcher's policy
echo "add a --dry-run flag to the deploy script in ~/projects/deploy" > todo.txt
corral-todo watch --dir ~/todos -- pi                # names the harness, always
```

The watcher polls every 5 seconds (`--interval`), and on a change wakes exactly
one dispatcher in that directory: injecting into its live socket, else resuming
its dormant session, else starting one hidden. Nothing pops a window.

## The Todo Directory Lives Outside This Repository

`~/todos`, not `corral/todo/live`. pi concatenates every `AGENTS.md` up the
directory tree, so a todo directory nested in this repository would feed corral's
own architecture document (about 10k words) into every dispatcher, at a cost in
tokens and in confusion about what the agent is supposed to be working on.

## Prerequisites

- **`corrald` runs.** Spawning workers and receiving their reports go through it.
  The wake path does not, so a todo directory still normalizes and wakes without it.
- **`corral-todo` and your harness are on `PATH`**, including inside a worker's
  sandbox for anything the worker itself runs.
- **Both whitelist directions per worker directory**, in `~/.corral/whitelist`:

  ```
  /home/me/todos -> /home/me/projects/deploy
  /home/me/projects/deploy -> /home/me/todos
  ```

  Authorization is directional and keyed on the directory pair, so a working pair
  needs two lines: one for the spawn, one for the handshake and the report.
  Clicking "Allow always" twice on corrald's tray does the same thing. The file is
  re-read every tick, so no restart is needed.
- **The worker directory is known to corrald** (some session ran there once).
  A spawn into a directory corral has never seen is acked `directory_not_known`.

## The CLI

```
corral-todo list [--open|--status <open|progress|blocked|done>]
corral-todo add "<text>"
corral-todo set <id> <state> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive                  # completed lines move to done.txt
corral-todo watch [--dir <dir>] [--interval <secs>] -- <harness argv...>
```

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

## Running It As A Service

Lifecycle is deployment glue, not code here. Run `corral-todo watch` from a
systemd user service with restart-on-failure, the same way `corrald` runs. The
watcher is a separate process from `corrald` on purpose: a todo.txt parse failure
or a stuck lock must not take down messaging for every agent on the host.
