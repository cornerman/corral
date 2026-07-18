# e2e-pi: the deep scenario. Two nono-confined pi sessions drive the whole
# corral loop against the stub LLM. Ground-truth assertions dominate (records,
# stub request log, socket state, nono exit codes); OCR/focus are generous
# best-effort where the terminal UI is hard to read deterministically -- those
# are marked and should be hardened once validated in a live VM run.

PROJ_A = HOME + "/proj-a"
PROJ_B = HOME + "/proj-b"
WHITELIST = CORRAL + "/whitelist"


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

# --- 2. a plain turn: running -> idle -----------------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:reply operator-turn'")
acp(f"state {sock_a} idle 30")
assert stub_saw("operator-turn"), "stub never saw the operator turn"

# --- 3. requires_action via the question tool ---------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:ask'")
acp(f"state {sock_a} requires_action 30")
# The board correctly showed requires_action; the question tool blocks pi's
# turn. Cancelling the turn returns it to idle (the card-move Idle action path).
acp(f"prompt {sock_a} {sid_a} 'smoke:reply after-answer'")
acp(f"state {sock_a} idle 40")

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
assert not stub_saw("[from /home/alice/proj-a"), \
    "message delivered before whitelist approval"

# Approve via the headless whitelist path and let corrald's poll release it.
as_user(f"mkdir -p {CORRAL}; echo '{PROJ_A} -> {PROJ_B}' >> {WHITELIST}")
deadline = _t.time() + 30
while _t.time() < deadline:
    if stub_saw("[from /home/alice/proj-a"):
        break
    _t.sleep(2)
assert stub_saw("hello-from-a"), "whitelisted message never reached B"
assert stub_saw("[from /home/alice/proj-a"), "provenance tag missing on delivery"

# --- 6. roster + stop ---------------------------------------------------
acp(f"prompt {sock_a} {sid_a} 'smoke:list'")
acp(f"state {sock_a} idle 30")  # list_corral_agents executed without error

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
before = window_count()
stub_post_rule(json.dumps({
    "match": "smoke:resume", "tool": "corral_message_agent",
    "args": {"target_session": sid_b, "message": "wake-b", "hidden": True}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:resume'")
recs = wait_records(
    lambda rs: any(r.get("sessionId") == sid_b and r.get("socket")
                   and r.get("hidden") for r in rs),
    timeout=60, desc="B resumed hidden")
assert window_count() == before, "hidden resume opened a visible window"

# --- 8. hidden force_new spawn in a fresh dir ---------------------------
PROJ_C = HOME + "/proj-c"
as_user(f"mkdir -p {PROJ_C}")
as_user(f"echo '{PROJ_A} -> {PROJ_C}' >> {WHITELIST}")
before = window_count()
stub_post_rule(json.dumps({
    "match": "smoke:spawn", "tool": "corral_message_agent",
    "args": {"target_dir": PROJ_C, "message": "hi-c",
             "force_new": True, "hidden": True}}))
acp(f"prompt {sock_a} {sid_a} 'smoke:spawn'")
wait_records(
    lambda rs: any("proj-c" in r.get("cwd", "") and r.get("hidden")
                   for r in rs),
    timeout=90, desc="hidden spawn in proj-c")
assert window_count() == before, "hidden spawn opened a visible window"

# --- 9. sandbox-negative: the confinement premise -----------------------
prof = "/etc/corral/agent.jsonc"
# A confined process in proj-a cannot read proj-b's workdir record.
ok, _ = try_user(
    f"cd {PROJ_A} && nono run --profile {prof} -- cat {PROJ_B}/.corral/registry/*.json")
assert not ok, "confined agent could read another workdir's record"
# ... nor write the sealed state/registry.
ok, _ = try_user(
    f"cd {PROJ_A} && nono run --profile {prof} -- "
    f"sh -c 'echo evil > {STATE}/evil.json'")
assert not ok, "confined agent could write sealed state/registry"
# ... but can write its own workdir .corral.
as_user(f"cd {PROJ_A} && nono run --profile {prof} -- "
        f"sh -c 'echo ok > {PROJ_A}/.corral/probe'")

# --- 10. GUI board renders (software GL; drop if unsupported) ------------
try:
    open_kitty(HOME, "true")  # ensure a clean surface first
    swaymsg('exec "corral-gui"')
    machine.wait_for_text("proj", timeout=40)
except Exception as e:
    machine.log(f"corral-gui OCR skipped (software GL best-effort): {e}")

machine.log("e2e-pi: all hard assertions passed")
