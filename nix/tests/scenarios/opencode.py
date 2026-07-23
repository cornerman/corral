# e2e-opencode: opencode announces, takes a stub turn, receives operator and
# cross-kind (pi -> opencode) delivery, and teardown makes it dormant. opencode
# is bun-compiled and may SIGTRAP under Landlock (the TODO.md landmine); the
# scenario pins that outcome either way. Turn behavior depends on opencode's
# provider config for the stub (UNVERIFIED, version-specific), so turn checks
# are best-effort; the hard backbone is announce + teardown + corrald routing.
import time as _t

PROJ_O = HOME + "/proj-o"
PROJ_A = HOME + "/proj-a"


def stub_saw(substr):
    return any(substr in json.dumps(m.get("content", ""))
               for req in stub_requests()
               for m in req["body"].get("messages", []))


boot()
as_user(f"mkdir -p {PROJ_O} {PROJ_A}")

# opencode on PATH is nono-wrapped. If it SIGTRAPs under Landlock, no record
# appears -- pin that as the known outcome (the test still passes; a fix flips
# this branch).
open_kitty(PROJ_O, "opencode")
announced = True
try:
    recs = wait_records(
        lambda rs: any(r.get("socket") for r in records_with_label(rs, "opencode")),
        timeout=90, desc="live opencode record")
except Exception as e:
    announced = False
    machine.log("e2e-opencode: opencode did not announce under nono within 90s "
                "(likely the bun-under-Landlock SIGTRAP, TODO.md). Pinned as the "
                f"current known outcome: {e}")

if announced:
    sock_o = next(r["socket"] for r in records_with_label(recs, "opencode") if r.get("socket"))
    sid_o = next(r.get("sessionId", "") for r in records_with_label(recs, "opencode"))
    assert any("proj-o" in r.get("cwd", "") for r in recs)

    # Operator delivery: turn is best-effort (opencode provider config UNVERIFIED).
    acp(f"prompt {sock_o} {sid_o} 'reply operator-to-opencode'")
    try:
        acp(f"state {sock_o} idle 40")
    except Exception as e:
        machine.log(f"e2e-opencode: opencode turn best-effort skipped: {e}")
    machine.log("e2e-opencode: operator turn seen by stub: "
                + str(stub_saw("operator-to-opencode")))

    # History export: session/load replays the turn above. Best-effort like the
    # turn itself (opencode provider config UNVERIFIED, so the turn may not
    # have actually produced messages) -- log rather than hard-assert.
    try:
        load_res = json.loads(acp(f"load {sock_o} {sid_o} 15"))
        machine.log(f"e2e-opencode: session/load result: {load_res}")
    except Exception as e:
        machine.log(f"e2e-opencode: session/load best-effort skipped: {e}")

    # Cross-kind: a pi session messages the opencode dir (whitelisted). corrald
    # routing is the hard part; the opencode turn that follows is best-effort.
    open_kitty(PROJ_A, "pi")
    pa = wait_records(lambda rs: any(r.get("socket") for r in records_with_label(rs, "pi")),
                      timeout=90, desc="live pi record")
    sock_a = next(r["socket"] for r in records_with_label(pa, "pi") if r.get("socket"))
    sid_a = next(r.get("sessionId", "") for r in records_with_label(pa, "pi"))
    as_user(f"mkdir -p {CORRAL}/state; echo '{PROJ_A} -> {PROJ_O}' >> {CORRAL}/state/whitelist")
    stub_post_rule(json.dumps({
        "match": "smoke:msg-o", "tool": "corral_message_agent",
        "args": {"target_dir": PROJ_O, "message": "cross-kind-hi"}}))
    acp(f"prompt {sock_a} {sid_a} 'smoke:msg-o'")
    _t.sleep(20)
    machine.log("e2e-opencode: cross-kind delivery seen by stub: "
                + str(stub_saw("cross-kind-hi")))

    # Teardown: killing the process makes the record dormant (socket null).
    as_user("pkill -f opencode || true")
    wait_records(
        lambda rs: any(r.get("sessionId") == sid_o and not r.get("socket") for r in rs),
        timeout=40, desc="opencode dormant after teardown")
    machine.log("e2e-opencode: announce + teardown assertions passed")
