# e2e-todo: the todo system's loop, end to end (todo/SPEC.md stage 1).
#
# Not a harness scenario like the other four: `corral-todo` is a client of
# corral-core, so what is under test is the wake chain plus the dispatch fan-out,
# with pi standing in as the dispatcher and worker kind.
#
# What this proves that unit tests cannot: a real `corral-todo watch` process
# launches a real pi session through cage (hidden, no window), that session
# receives the wake text as its first prompt, a later edit reaches the *same*
# session over its socket instead of starting a second one, and a dispatcher's
# corral_spawn_agent lands a worker in another directory through corrald's gate.
#
# What it deliberately does NOT prove: that a real model obeys DISPATCHER.md.
# The stub LLM is a rule table, so policy compliance (above all "write nothing
# when nothing changed") stays a live-run question.
#
# STATUS: has never completed green. Sections 1-4 pass in a real VM; section 5
# first failed with "no terminal found" (see the CORRAL_TERMINAL note there) and
# the fix is UNVERIFIED. Sections 6-10 have never executed. See TODO.md.

TODOS = HOME + "/todos"
PROJ_A = HOME + "/proj-a"
WHITELIST = CORRAL + "/state/whitelist"
# The watcher runs as a transient systemd user unit, not a backgrounded shell
# job: the test driver's `succeed` waits for its command's output stream to
# close, and a daemonized child keeps that open forever. A unit also matches how
# `todo/README.md` says to run it in production, and gives a journal to read.
WATCH_UNIT = "corral-todo-watch"

# Substrings of wake::FIRST_PROMPT and wake::WAKE_MESSAGE. Kept as literals so a
# reworded prompt fails here loudly rather than silently weakening the test.
POLICY_POINTER = "DISPATCHER.md"
FIRST_PROMPT_MARK = "You are the todo dispatcher for this directory"
WAKE_MARK = "todo.txt changed"


def todo_cli(args, dir=TODOS):
    return as_user(f"cd {dir} && corral-todo {args}")


def watch_log():
    ok, out = try_user(
        f"journalctl --user -u {WATCH_UNIT} --no-pager -o cat 2>/dev/null")
    return out if ok else ""


def wake_lines():
    return [l for l in watch_log().splitlines() if "wake " in l]


def stub_saw(substr):
    for req in stub_requests():
        for m in req["body"].get("messages", []):
            if substr in json.dumps(m.get("content", "")):
                return True
    return False


def wait_stub(substr, timeout=90, desc=None):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if stub_saw(substr):
            return
        time.sleep(1)
    machine.log("=== DIAG: watch log ===")
    machine.log(watch_log())
    dump_messaging()
    raise Exception(f"timeout waiting for stub to see {desc or substr!r}")


def todo_file():
    ok, out = try_user(f"cat {TODOS}/todo.txt")
    return out if ok else ""


boot()

# --- 1. init lays out the todo directory -------------------------------
as_user(f"mkdir -p {PROJ_A}")
out = todo_cli(f"init {TODOS}", dir=HOME)
machine.log("init said:\n" + out)
# The policy is a real file in the todo dir, under its role's name.
as_user(f"test -f {TODOS}/DISPATCHER.md")
as_user(f"test -f {TODOS}/todo.txt")
# Never the ambient name: it would govern every agent that runs here.
ok, _ = try_user(f"test -e {TODOS}/AGENTS.md")
assert not ok, "init must not write AGENTS.md"
# init prints the whitelist lines rather than writing them (SECURITY.md).
assert TODOS in out and "->" in out, f"init should print whitelist hints: {out}"
ok, _ = try_user(f"test -e {WHITELIST}")
assert not ok, "init must not create the whitelist"

# --- 2. the CLI owns the file ------------------------------------------
todo_cli('add "(C) middling thing"')
todo_cli('add "(A) urgent thing"')
listed = todo_cli("list")
machine.log("list:\n" + listed)
ids = [l.split()[0] for l in listed.strip().splitlines()]
assert len(ids) == 2, f"expected two items: {listed}"
# Dispatch order: (A) before (C), so the dispatcher takes from the top.
assert "(A)" in listed.splitlines()[0], f"priority must sort first: {listed}"
assert "(C)" in listed.splitlines()[1], f"unexpected order: {listed}"
# Both fields the policy orders by are visible.
assert "-" not in listed.splitlines()[0].split()[2], "priority column missing"

# --- 3. refuse to watch a directory with no policy ---------------------
ok, out = try_user(f"cd {HOME} && rm -f /tmp/np.log; mkdir -p {HOME}/nopolicy && "
                   f"echo idea > {HOME}/nopolicy/todo.txt && "
                   f"timeout 6 corral-todo watch --dir {HOME}/nopolicy --interval 1 -- pi "
                   f"> /tmp/np.log 2>&1; cat /tmp/np.log")
assert POLICY_POINTER in out and "corral-todo init" in out, \
    f"watch must refuse a dir with no policy, naming init: {out}"
# And it must not have launched anything.
recs = state_records()
assert not any("nopolicy" in r.get("cwd", "") for r in recs), \
    "a policy-less dir must never get an agent"

# --- 4. the whitelist is seeded for the dispatch pair ------------------
# Both directions: (todo -> worker) for the spawn, (worker -> todo) for the
# report. Authorization is directional (SECURITY.md), so one line is not enough.
as_user(f"mkdir -p {CORRAL}/state && "
        f"printf '%s -> %s\\n%s -> %s\\n' "
        f"'{TODOS}' '{PROJ_A}' '{PROJ_A}' '{TODOS}' > {WHITELIST}")

# --- 5. an edit spawns a hidden dispatcher ----------------------------
# Two env vars must be passed explicitly, because a user unit does not inherit
# the login shell's environment:
#   WAYLAND_DISPLAY -- cage nests under the test's sway.
#   CORRAL_TERMINAL -- a hidden *terminal* agent is cage hosting a terminal
#     hosting pi, so `launch` still resolves one. Without it the first VM run of
#     this scenario failed every wake with "no terminal found" (correctly, and
#     it retried every tick). Anyone running `watch` as a systemd service hits
#     the same wall; see todo/README.md.
as_user(
    f"export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{UID}/bus; "
    f"systemd-run --user --unit={WATCH_UNIT} "
    f"--setenv=WAYLAND_DISPLAY=wayland-1 "
    f"--setenv=CORRAL_TERMINAL='kitty -e' "
    f"corral-todo watch --dir {TODOS} --interval 2 -- pi")
machine.wait_until_succeeds(
    f"systemctl --user --machine={USER}@ is-active {WATCH_UNIT}", timeout=30)
todo_cli('add "first real task"')

recs = wait_records(
    lambda rs: any(TODOS in r.get("cwd", "") and r.get("label") == "pi" for r in rs),
    timeout=180, desc="the dispatcher's record")
disp = [r for r in recs if TODOS in r.get("cwd", "")][0]
machine.log("dispatcher record: " + json.dumps(disp))
# Hidden: the watcher always launches into a headless cage, so no window maps.
assert disp.get("hidden") is True, f"the dispatcher must run hidden: {disp}"

# It was told what it is and where its policy lives, and the policy was not
# inlined into the prompt.
wait_stub(FIRST_PROMPT_MARK, desc="the first-run dispatcher prompt")
assert stub_saw(POLICY_POINTER), "the wake must name DISPATCHER.md"
assert not stub_saw("Core Operational Loop"), \
    "the prompt must point at the policy, not inline it"

log = watch_log()
machine.log("watch log after first wake:\n" + log)
assert "via spawn" in log, f"first wake should spawn: {log}"

# --- 6. a second edit reaches the same session over its socket --------
before = len(wake_lines())
sessions_before = {r.get("sessionId") for r in state_records()
                   if TODOS in r.get("cwd", "")}
# The startup content and the first add are two changes inside pi's boot
# window; the spawn grace must hold the second one rather than stack a
# sibling dispatcher (watch.rs SPAWN_GRACE).
assert len(sessions_before) == 1, \
    f"exactly one dispatcher may exist: {sessions_before}"
todo_cli('add "second real task"')
deadline = time.time() + 90
while time.time() < deadline and len(wake_lines()) <= before:
    time.sleep(1)
log = watch_log()
machine.log("watch log after second wake:\n" + log)
assert "via inject" in log, f"a live dispatcher must be injected into: {log}"
# Injecting means no second session: the session id is the address the whole
# system converges on, so it must survive an edit.
sessions_after = {r.get("sessionId") for r in state_records()
                  if TODOS in r.get("cwd", "")}
assert sessions_after == sessions_before, \
    f"inject must reuse the session: {sessions_before} -> {sessions_after}"

# Each wake carries its own fingerprint, so two real edits are two distinct ones.
prints = [l.split("wake ")[1].split()[0] for l in wake_lines()]
assert len(set(prints)) == len(prints), \
    f"a repeated fingerprint means a retried wake, not an edit: {prints}"

# --- 7. the dispatcher dispatches: spawn a worker in another dir -------
# The stub is a rule table, so it cannot read todo.txt. Drive the tool call
# directly: the next wake makes the dispatcher spawn a worker in proj-a, which
# is the fan-out todo/SPEC.md specifies (spawn down, report up).
#
# WAKE_MARK is a substring of BOTH wake::WAKE_MESSAGE and wake::FIRST_PROMPT,
# and the stub matches its rules against the LAST message only. So this rule
# fires for an injected wake AND for any dispatcher spawned from here on. That
# is benign today (the assertion below wants one worker in proj-a and takes the
# first), but it means this section cannot tell WHICH dispatcher dispatched.
# If that ever matters, give the rule a marker the wake text does not carry.
stub_post_rule(json.dumps({
    "match": WAKE_MARK,
    "tool": "corral_spawn_agent",
    "args": {"cwd": PROJ_A, "task": "smoke: the dispatched worker task",
             "label": "pi", "window": "hidden"},
}))
todo_cli('add "third task, this one dispatches"')

recs = wait_records(
    lambda rs: any(PROJ_A in r.get("cwd", "") and r.get("label") == "pi" for r in rs),
    timeout=180, desc="the worker's record in proj-a")
worker = [r for r in recs if PROJ_A in r.get("cwd", "")][0]
machine.log("worker record: " + json.dumps(worker))
# corrald spawns hidden by default, so an uninvited agent never pops a window.
assert worker.get("hidden") is True, f"the worker must run hidden: {worker}"
# The task rode along as the first prompt, atomically with the launch.
wait_stub("the dispatched worker task", desc="the worker's task prompt")
# A freshly spawned agent is charter-prefixed by corrald.
assert stub_saw("reached through corral"), "the worker should get the charter"

# --- 8. no windows appeared -------------------------------------------
# Everything the todo system starts is hidden, so sway maps nothing new beyond
# whatever was already there at boot.
machine.log(f"sway windows: {window_count()}")
assert window_count() == 0, "the todo system must never map a window"

# --- 9. the file still parses and the CLI still owns it ---------------
final = todo_file()
machine.log("final todo.txt:\n" + final)
assert final.count("\n") >= 4, f"expected four items: {final}"
# Every line got an id and a creation date on the way through.
for line in final.strip().splitlines():
    assert "id:" in line, f"unnormalized line survived: {line}"
# And the dispatcher path never corrupted it: the CLI can still round-trip.
listed = todo_cli("list --open")
assert len(listed.strip().splitlines()) == 4, f"expected four open: {listed}"

# --- 10. settling: no wake without a change ---------------------------
# The watcher polls every 2s; after the dust settles a quiet interval must add
# no wake lines. (A dispatcher that rewrites the file pointlessly would show up
# here as extra wakes -- but the stub never writes, so this checks the
# *watcher's* idempotence, not the policy's.)
time.sleep(8)
quiet = len(wake_lines())
time.sleep(8)
assert len(wake_lines()) == quiet, \
    f"the watcher woke with no change: {watch_log()}"

machine.log("e2e-todo: OK")
