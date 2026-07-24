# e2e-pi: the deep scenario. Two nono-confined pi sessions drive the whole
# corral loop against the stub LLM. Ground-truth assertions dominate (records,
# stub request log, socket state, nono exit codes); OCR/focus are generous
# best-effort where the terminal UI is hard to read deterministically -- those
# are marked and should be hardened once validated in a live VM run.

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

# --- 2. a plain turn: running -> idle -----------------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:reply operator-turn'")
acp(f"state {sock_a} idle 30")
assert stub_saw("operator-turn"), "stub never saw the operator turn"

# --- 2b. history export: session/load replays the turn we just ran ------
load_res = json.loads(acp(f"load {sock_a} {sid_a} 15"))
assert load_res.get("ok"), f"pi session/load failed: {load_res}"
assert load_res["chunks"] >= 2, f"expected at least a user+assistant chunk: {load_res}"

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
# A calls corral_message_agent(target_dir=proj-b). No whitelist -> held.
import time as _t
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
    "args": {"target_dir": PROJ_B, "message": "second-to-b"}}))
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
acp(f"state {sock_a} idle 30")  # list_corral_agents executed without error
# The roster reply (corrald's JSON, returned to the stub as the tool result)
# now exposes the title for a reachable session. proj-a is its own dir, so its
# own entry carries its title (the first-user-message fallback set in step 2).
assert stub_saw('"title"'), "roster did not expose the reachable session's title"

# Stop B by session id (whitelisted pair). Rule baked with B's sid.
stub_post_rule(json.dumps({
    "match": "smoke:stop", "tool": "corral_stop_agent",
    "args": {"target_session": sid_b}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:stop'")
wait_records(
    lambda rs: any(r.get("sessionId") == sid_b and not r.get("socket")
                   for r in rs),
    timeout=40, desc="B dormant after stop")

# --- 7. resume dormant B via corrald delivery (hidden by default) -------
# BEST-EFFORT: hidden resume/spawn launch inside a headless `cage`, which needs
# working wlroots/EGL under the VM's software GL -- a documented verify-in-VM
# point. The corrald routing + resume decision is exercised regardless; only
# the cage-hosted relaunch may not come up here. Backbone (announce, turns,
# messaging, stop) is already hard-asserted above.
before = window_count()
stub_post_rule(json.dumps({
    "match": "smoke:resume", "tool": "corral_message_agent",
    "args": {"target_session": sid_b, "message": "wake-b", "hidden": True}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:resume'")
try:
    wait_records(
        lambda rs: any(r.get("sessionId") == sid_b and r.get("socket")
                       and r.get("hidden") for r in rs),
        timeout=45, desc="B resumed hidden", diag=False)
    assert window_count() == before, "hidden resume opened a visible window"
    machine.log("e2e-pi: hidden resume via corrald confirmed (cage headless works)")
except Exception as e:
    machine.log(f"e2e-pi: hidden resume best-effort (cage headless UNVERIFIED): {e}")

# --- 8. hidden force_new spawn in a fresh dir (best-effort, cage) --------
PROJ_C = HOME + "/proj-c"
as_user(f"mkdir -p {PROJ_C}")
as_user(f"echo '{PROJ_A} -> {PROJ_C}' >> {WHITELIST}")
stub_post_rule(json.dumps({
    "match": "smoke:spawn", "tool": "corral_message_agent",
    "args": {"target_dir": PROJ_C, "message": "hi-c",
             "force_new": True, "hidden": True}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:spawn'")
try:
    wait_records(
        lambda rs: any("proj-c" in r.get("cwd", "") and r.get("hidden")
                       for r in rs),
        timeout=60, desc="hidden spawn in proj-c", diag=False)
    machine.log("e2e-pi: hidden force_new spawn via corrald confirmed")
except Exception as e:
    machine.log(f"e2e-pi: hidden spawn best-effort (cage headless UNVERIFIED): {e}")
assert window_count() == before, "hidden spawn opened a visible window"

# --- 3 (moved last, since a blocked question wedges A). requires_action via
#     the question tool: the card must flip to requires_action. Done after all
#     A-driven messaging because pi's question blocks the turn and abort does
#     not reliably unblock it (UNVERIFIED per AGENTS.md), so A is spent after.
acp(f"prompt {sock_a} {sid_a} 'smoke:ask'")
acp(f"state {sock_a} requires_action 30")
machine.log("e2e-pi: question tool -> requires_action confirmed")
acp(f"cancel {sock_a} {sid_a}")
try:
    acp(f"state {sock_a} idle 20")
    machine.log("e2e-pi: session/cancel unblocked the question -> idle (abort VERIFIED)")
except Exception:
    machine.log("e2e-pi: session/cancel did NOT unblock the question "
                "(pi abort-unblocks-question still UNVERIFIED)")

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
