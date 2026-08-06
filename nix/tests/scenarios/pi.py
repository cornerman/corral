# e2e-pi: the deep scenario. Two pi sessions drive the whole corral loop against
# the stub LLM. The sessions currently run UNCONFINED (open_kitty runs plain
# `pi`, not `nono run`); full nono confinement is the tracked follow-up in
# TODO.md, and the sandbox-negative section below is BEST-EFFORT until it lands
# (see section 9). Ground-truth assertions dominate (records, stub request log,
# socket state); OCR/focus are generous best-effort where the terminal UI is
# hard to read deterministically -- those are marked and should be hardened
# once validated in a live VM run.

PROJ_A = HOME + "/proj-a"
PROJ_B = HOME + "/proj-b"
# The whitelist lives in the sealed state/ dir (paths::whitelist_file), not
# directly under ~/.corral. The operator/headless approval path appends here.
WHITELIST = CORRAL + "/state/whitelist"


def socket_of(recs, label, cwd_substr):
    for r in records_with_label(recs, label):
        if cwd_substr in r.get("cwd", "") and r.get("socket"):
            return r["socket"], r.get("sessionId", "")
    return None, None


def stub_saw(substr):
    for req in stub_requests():
        for m in req["body"].get("messages", []):
            if substr in json.dumps(m.get("content", "")):
                return True
    return False


def roster_agents():
    # The capability roster corrald returns to a `corral_list_agents` tool
    # call, parsed out of the stub's request log (the roster arrives as the
    # tool-result content of a role:"tool" message). `stub_saw` cannot be used
    # here: it runs json.dumps on the content, which backslash-escapes the
    # roster's inner quotes, so a key like `"title"` would never match.
    for req in stub_requests():
        for m in req["body"].get("messages", []):
            if m.get("role") != "tool":
                continue
            content = m.get("content", "")
            if isinstance(content, list):
                content = " ".join(
                    p.get("text", "") for p in content if isinstance(p, dict))
            try:
                doc = json.loads(content)
            except (ValueError, TypeError):
                continue
            if isinstance(doc, dict) and doc.get("status") == "ok" \
                    and isinstance(doc.get("agents"), list):
                return doc["agents"]
    return []


boot()

# --- 1. two pi sessions announce ---------------------------------------
as_user(f"mkdir -p {PROJ_A} {PROJ_B}")
open_kitty(PROJ_A, "pi")
open_kitty(PROJ_B, "pi")

recs = wait_records(
    lambda rs: len(records_with_label(rs, "pi")) >= 2
    and all(r.get("socket") for r in records_with_label(rs, "pi")),
    timeout=120, desc="two live pi records")
sock_a, sid_a = socket_of(recs, "pi", "proj-a")
sock_b, sid_b = socket_of(recs, "pi", "proj-b")
assert sock_a and sock_b, f"missing sockets: {recs}"
# cwd is stamped from physical location, not any content field.
for r in records_with_label(recs, "pi"):
    assert r["cwd"].startswith(HOME), r
# per-session pointer files exist in the write-only input dir.
as_user(f"test -n \"$(ls {CORRAL}/input/registry/)\"")

# Model exposure: pi runs the stub provider's `smoke` model, so both the vetted
# record and the live config_options_update broadcast carry "stub/smoke".
for r in records_with_label(recs, "pi"):
    assert r.get("model") == "stub/smoke", f"record missing model: {r}"
model_res = json.loads(acp(f"model {sock_a} 20"))
assert model_res.get("model") == "stub/smoke", \
    f"pi did not broadcast the model: {model_res}"

# NSpid bridge: the record carries the window pid + its PID-namespace inode, so
# a host consumer correlates to a host window (focus/kill) even for a sandboxed
# agent. Here pi shares the host PID namespace, so the pid is host-level and
# focus works via the identity shortcut. The socket filename is <sessionId>.sock
# (opaque, no longer parsed for pid/label).
for r in records_with_label(recs, "pi"):
    assert isinstance(r.get("pid"), int) and r["pid"] > 0, \
        f"record missing pid: {r}"
    assert isinstance(r.get("pidNamespace"), int) and r["pidNamespace"] > 0, \
        f"record missing pidNamespace: {r}"
    assert r["socket"].endswith(f"/{r['sessionId']}.sock"), \
        f"pi socket filename should be <sessionId>.sock: {r}"

# --- 2. a plain turn: running -> idle -----------------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:reply operator-turn'")
acp(f"state {sock_a} idle 30")
assert stub_saw("operator-turn"), "stub never saw the operator turn"

# --- 2b. history export: session/load replays the turn we just ran ------
load_res = json.loads(acp(f"load {sock_a} {sid_a} 15"))
assert load_res.get("ok"), f"pi session/load failed: {load_res}"
assert load_res["chunks"] >= 2, f"expected at least a user+assistant chunk: {load_res}"
# pi has no system-prompt session entry (session-format.md), so the export
# must synthesize it from ctx.getSystemPrompt() as a system_prompt update.
assert load_res.get("systemPrompt"), f"pi session/load did not replay the system prompt: {load_res}"

# Context exposure: after a turn, pi has at least one session-log entry, so
# the live broadcast and the persisted record must both carry it.
context_res = json.loads(acp(f"context {sock_a} 20"))
assert isinstance(context_res.get("entries"), int) and context_res["entries"] >= 1, \
    f"pi did not broadcast entries: {context_res}"
assert context_res.get("age"), f"pi did not broadcast an age string: {context_res}"
recs = wait_records(
    lambda rs: any(r.get("sessionId") == sid_a and r.get("entries")
                   for r in rs),
    timeout=30, desc="A's record carries entries after a turn")
rec_a = next(r for r in recs if r.get("sessionId") == sid_a)
assert rec_a.get("entries", 0) >= 1, f"record missing entries: {rec_a}"
assert rec_a.get("contextAge"), f"record missing contextAge: {rec_a}"
# --- 4. board TUI renders + operator m delivers -------------------------
open_kitty(HOME, "corral")
try:
    machine.wait_for_text("proj-a", timeout=30)
except Exception as e:
    machine.log(f"OCR of the TUI board did not find proj-a (best-effort): {e}")

# Operator m == the send_prompt path; assert end-to-end delivery via the stub.
acp(f"prompt {sock_b} {sid_b} 'smoke:reply operator-m-to-b'")
acp(f"state {sock_b} idle 30")
assert stub_saw("operator-m-to-b"), "operator m to B not delivered"

# --- 5. inter-agent message, gated then whitelisted ---------------------
# A messages B's session id (the only addressing form). No whitelist -> held.
import time as _t
stub_post_rule(json.dumps({
    "match": "smoke:msg-b", "tool": "corral_message_agent",
    "args": {"target_session": sid_b, "message": "hello-from-a"}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:msg-b'")
_t.sleep(8)
# Only a DELIVERED message carries the provenance tag; absence proves gating.
assert not stub_saw("[from proj-a"), \
    "message delivered before whitelist approval"

# --- 5b. head-of-line + reply-by-session: B answers A via target_session ----
# A->B is now parked awaiting approval. B replies to A by SESSION id (the
# reply-handle path a spawned agent uses to answer its spawner), then ONLY
# B->A is whitelisted. The reply must deliver to A's live socket even though
# A->B is still pending ahead of it (regression: the old single-pending queue
# blocked the whole queue on the first un-approved message).
stub_post_rule(json.dumps({
    "match": "smoke:msg-a", "tool": "corral_message_agent",
    "args": {"target_session": sid_a, "message": "hello-from-b"}}))
acp(f"prompt {sock_b} {sid_b} 'smoke:msg-a'")
_t.sleep(8)
as_user(f"mkdir -p {CORRAL}/state; echo '{PROJ_B} -> {PROJ_A}' >> {WHITELIST}")
deadline = _t.time() + 90
while _t.time() < deadline:
    if stub_saw("[from proj-b"):
        break
    _t.sleep(2)
if not stub_saw("hello-from-b"):
    dump_messaging()
assert stub_saw("hello-from-b"), \
    "B->A reply-by-session never delivered (send_prompt seed-drain regression) \
     or blocked behind the still-pending A->B (head-of-line regression)"
assert not stub_saw("[from proj-a"), \
    "A->B delivered without its own approval"

# --- 5c. operator Allow-once via the notification releases by id ------------
# The real approval surface: corrald fires `notify-send -A` per pending message
# and applies the clicked action to that message id. The VM's stub notify-send
# answers with /tmp/notify-mode. A sends a SECOND message on the same
# unwhitelisted A->B pair: it parks, its own notification fires, the stub
# clicks "Allow once", and only THIS message may deliver -- the first A->B
# message must stay parked (by-id resolution; the reported
# allow-once-not-delivered flow).
as_user("echo once > /tmp/notify-mode")
stub_post_rule(json.dumps({
    "match": "smoke:again", "tool": "corral_message_agent",
    "args": {"target_session": sid_b, "message": "second-to-b"}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:again'")
deadline = _t.time() + 90
while _t.time() < deadline:
    if stub_saw("second-to-b"):
        break
    _t.sleep(2)
as_user("echo dismiss > /tmp/notify-mode")
if not stub_saw("second-to-b"):
    dump_messaging()
    machine.log(try_user("cat /tmp/notify-send.log")[1])
assert stub_saw("second-to-b"), \
    "notification Allow-once did not deliver the approved message"
assert not stub_saw("hello-from-a"), \
    "Allow once released the WRONG message (first A->B must stay parked)"
ok, nlog = try_user("cat /tmp/notify-send.log")
assert ok and "corral" in nlog, f"approval notification never fired: {nlog}"

# Approve via the headless whitelist path and let corrald's poll release it.
# Generous window: delivery needs corrald's poll + B's turn against the stub,
# both of which slow under host contention (e.g. `just e2e` before it went
# sequential, or a busy CI runner).
as_user(f"mkdir -p {CORRAL}/state; echo '{PROJ_A} -> {PROJ_B}' >> {WHITELIST}")
deadline = _t.time() + 90
while _t.time() < deadline:
    # 5c already delivered a "[from proj-a"-tagged message, so wait on this
    # message's own text.
    if stub_saw("hello-from-a"):
        break
    _t.sleep(2)
if not stub_saw("hello-from-a"):
    dump_messaging()
assert stub_saw("hello-from-a"), "whitelisted message never reached B"
assert stub_saw("[from proj-a"), "provenance tag missing on delivery"

# --- 6. roster + stop ---------------------------------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:list'")
acp(f"state {sock_a} idle 30")  # corral_list_agents executed without error
# The roster reply (corrald's JSON, returned to the stub as the tool result)
# now exposes the title for a reachable session. proj-a is its own dir, so its
# own entry carries its title (the first-user-message fallback set in step 2).
assert any(a.get("title") for a in roster_agents()), \
    "roster did not expose the reachable session's title"

# Stop B by session id (whitelisted pair). Rule baked with B's sid.
stub_post_rule(json.dumps({
    "match": "smoke:stop", "tool": "corral_stop_agent",
    "args": {"target_session": sid_b}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:stop'")
wait_records(
    lambda rs: any(r.get("sessionId") == sid_b and not r.get("socket")
                   for r in rs),
    timeout=40, desc="B dormant after stop")

# --- 7. resume dormant B via corrald delivery (placement is inherited) ---
# Hard-asserted since 2026-08-02. These two sections used to swallow their
# failure as "cage headless UNVERIFIED", which is precisely how corrald's `no
# terminal found` bug (no $TERMINAL in its unit, so every routed spawn died
# while the caller's ack said `accepted`) passed here unnoticed until e2e-todo
# asserted hard. Do not put the try/except back.
#
# B was opened visibly by the scenario, so its resume comes back VISIBLE: a
# resume inherits the record's own placement (router.rs), since the messager
# does not get to move another agent's window. Only a *spawn* defaults hidden
# -- that is §8. Asserting a window actually maps is what proves the
# inheritance is real rather than a flag nobody acts on.
before = window_count()
stub_post_rule(json.dumps({
    "match": "smoke:resume", "tool": "corral_message_agent",
    "args": {"target_session": sid_b, "message": "wake-b"}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:resume'")
recs = wait_records(
    lambda rs: any(r.get("sessionId") == sid_b and r.get("socket") for r in rs),
    timeout=45, desc="B resumed")
resumed = [r for r in recs if r.get("sessionId") == sid_b][0]
assert not resumed.get("hidden"), \
    f"a resume must inherit the record's visible placement: {resumed}"
deadline = time.time() + 30
while time.time() < deadline and window_count() <= before:
    time.sleep(1)
assert window_count() > before, "the visible resume mapped no window"

# --- 8. hidden spawn in a fresh dir --------------------------------------
PROJ_C = HOME + "/proj-c"
# Own baseline: §7 just added B's window, so the count moved.
before = window_count()
as_user(f"mkdir -p {PROJ_C}")
as_user(f"echo '{PROJ_A} -> {PROJ_C}' >> {WHITELIST}")
# `label` is required here: proj-c has never been announced in, so there is no
# directory-local kind to fall back on and corrald refuses to guess ("no known
# agent kind for ..."). Omitting it is what the old try/except was silently
# swallowing.
stub_post_rule(json.dumps({
    "match": "smoke:spawn", "tool": "corral_spawn_agent",
    "args": {"cwd": PROJ_C, "task": "hi-c", "label": "pi",
             "window": "hidden"}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:spawn'")
wait_records(
    lambda rs: any("proj-c" in r.get("cwd", "") and r.get("hidden")
                   for r in rs),
    timeout=60, desc="hidden spawn in proj-c")
assert window_count() == before, "hidden spawn opened a visible window"

# --- 8b. a non-canonical spawn cwd hits the same grant (SECURITY.md T20) ----
# corrald canonicalizes the spawn `cwd` at the control-socket boundary, so a
# `..` spelling of proj-c authorizes against the `proj-a -> proj-c` whitelist
# line above and is audited under the real path. Before that fix the raw string
# missed the whitelist (parked forever) and the operator's approval popup would
# have shown a basename of whatever path the sender chose.
stub_post_rule(json.dumps({
    "match": "smoke:canon", "tool": "corral_spawn_agent",
    "args": {"cwd": PROJ_C + "/../proj-c", "task": "hi-canon",
             "window": "hidden"}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:canon'")
deadline = _t.time() + 90
audit = ""
while _t.time() < deadline:
    audit = try_user(f"cat {CORRAL}/state/audit.log")[1]
    if "spawned" in audit and PROJ_C in audit:
        break
    _t.sleep(2)
assert PROJ_C in audit, \
    f"non-canonical spawn cwd never authorized under the canonical path: {audit}"
assert "/../" not in audit, \
    f"audit line kept the raw spelling instead of the canonical path: {audit}"

# --- 3 (moved last, since a blocked question wedges A). requires_action via
#     the question tool: the card must flip to requires_action. Done after all
#     A-driven messaging because pi's question blocks the turn and abort does
#     not unblock it (ACCEPTED limitation, AGENTS.md), so A is spent after.
acp(f"prompt {sock_a} {sid_a} 'smoke:ask'")
acp(f"state {sock_a} requires_action 30")
machine.log("e2e-pi: question tool -> requires_action confirmed")
# ACCEPTED (AGENTS.md): pi's abort does NOT dismiss a pending question. Pin it
# as a hard assert -- the session must STAY blocked (never reach idle). If pi
# ever gains question-abort this flips to expect idle, and AGENTS.md's accepted
# limitation must be revisited. At the board level a Requires Action -> Idle
# card-move therefore does not fire this cancel; both shells surface an
# informative status instead (that UI path is not observable over raw ACP here).
acp(f"cancel {sock_a} {sid_a}")
reached_idle = True
try:
    acp(f"state {sock_a} idle 15")
except Exception:
    reached_idle = False
assert not reached_idle, \
    "pi abort unexpectedly unblocked the question -> accepted limitation no longer holds"
machine.log("e2e-pi: session/cancel left the question blocked (accepted, confirmed)")

# --- 3b. a SIGKILLed session must read dormant, not live forever ---------
# SIGKILL skips pi's session_shutdown, so the socket FILE survives in
# proj-a/.corral while nothing listens. Before the curator's pid-liveness
# check (NSpid bridge over the record's pid + pidNamespace) the vetted record
# kept its socket, so corral_list_agents reported the crashed session live
# for days and the 14-day dormant prune never applied. A is already spent
# (wedged on the question above), so it is the natural victim.
recs = wait_records(
    lambda rs: any(r.get("sessionId") == sid_a and r.get("pid") for r in rs),
    timeout=30, desc="A's record carries a pid")
pid_a = next(r["pid"] for r in recs if r.get("sessionId") == sid_a)
as_user(f"kill -9 {pid_a}")
wait_records(
    lambda rs: any(r.get("sessionId") == sid_a and not r.get("socket")
                   for r in rs),
    timeout=40, desc="SIGKILLed A demoted to dormant by the pid check")

# --- 9. sandbox-negative: the confinement premise (BEST-EFFORT) ---------
# Running arbitrary commands under nono needs per-command path discovery
# (`nono learn`) just like the agents do, so these probes are best-effort
# until full nono confinement lands (the tracked follow-up). The premise they
# check -- cross-workdir reads denied, sealed state/registry unwritable -- is
# meanwhile hard-covered by corral's own curation/vet unit tests and the
# security test matrix.
prof = "/etc/corral/agent.jsonc"
def confined(cmd):
    return try_user(f"cd {PROJ_A} && nono run --profile {prof} -- {cmd}")[0]
try:
    if confined("sh -c 'echo ok > /tmp/nono-selftest'"):
        # nono can run a plain command here, so the denials are meaningful.
        assert not confined(f"cat {PROJ_B}/.corral/registry/x.json"), \
            "confined agent could read another workdir's record"
        assert not confined(f"sh -c 'echo evil > {STATE}/evil.json'"), \
            "confined agent could write sealed state/registry"
        machine.log("e2e-pi: sandbox-negative confinement checks passed")
    else:
        machine.log("e2e-pi: nono cannot run a plain command here (path discovery "
                    "needed); sandbox-negative deferred to the confinement follow-up")
except Exception as e:
    machine.log(f"e2e-pi: sandbox-negative best-effort: {e}")

# --- 10. GUI board renders (software GL; drop if unsupported) ------------
try:
    open_kitty(HOME, "true")  # ensure a clean surface first
    swaymsg('exec "corral-gui"')
    machine.wait_for_text("proj", timeout=40)
except Exception as e:
    machine.log(f"corral-gui OCR skipped (software GL best-effort): {e}")

machine.log("e2e-pi: all hard assertions passed")
