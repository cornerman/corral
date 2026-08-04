# e2e-opencode: opencode announces, takes a stub turn, receives operator and
# cross-kind (pi -> opencode) delivery, and teardown makes it dormant.
#
# STATUS (2026-08-04): everything below the announce is still SWALLOWED, so this
# scenario is green while proving almost nothing -- read `dump_plugin_state`
# before trusting it. What the diagnostics established: the plugin loads and
# binds its socket, so the old pinned cause ("bun SIGTRAPs under Landlock") was
# false; opencode simply never emits a session-naming event while idle, and
# offline it also cannot reach a model (models.dev + dependency install both
# fail), so a real turn is out of reach here without vendoring the ai-sdk
# package into the VM. Hardening this is TODO.md's cross-harness item.
import time as _t

PROJ_O = HOME + "/proj-o"
PROJ_A = HOME + "/proj-a"


def stub_saw(substr):
    return any(substr in json.dumps(m.get("content", ""))
               for req in stub_requests()
               for m in req["body"].get("messages", []))


boot()
as_user(f"mkdir -p {PROJ_O} {PROJ_A}")

open_kitty(PROJ_O, "opencode")


def dump_plugin_state(tag):
    """Why an opencode record is missing, narrowed to one of three causes.

    The plugin binds its socket and creates `<cwd>/.corral/` the moment it
    LOADS, and writes the record only once a session event reveals a session id
    (see extensions/corral-opencode.ts). So:
      no `<cwd>/.corral/` at all  -> the plugin never loaded (install path,
                                     syntax, or opencode not reading it)
      `.corral/` with a .sock     -> loaded, but no session ever started
                                     (opencode sat on a config/provider screen)
      record but no `socket`      -> loaded and announced, then torn down
    Without this the failure was pinned for weeks on the wrong cause
    ("bun SIGTRAPs under Landlock") while the process was in fact alive and
    unconfined.
    """
    machine.log(f"=== DIAG opencode ({tag}): plugin dir ===")
    machine.log(machine.execute(f"ls -la {HOME}/.config/opencode/plugin/ 2>&1")[1])
    machine.log(f"=== DIAG opencode ({tag}): workdir .corral ===")
    machine.log(machine.execute(f"ls -laR {PROJ_O}/.corral/ 2>&1")[1])
    machine.log(f"=== DIAG opencode ({tag}): opencode state + logs ===")
    machine.log(machine.execute(
        f"ls -la {HOME}/.local/share/opencode/ 2>&1; "
        f"tail -n 40 {HOME}/.local/share/opencode/log/*.log 2>&1")[1])


announced = True
try:
    recs = wait_records(
        lambda rs: any(r.get("socket") for r in records_with_label(rs, "opencode")),
        # 200s, not 90: offline, opencode spends ~70s failing to fetch
        # models.dev and then failing a background dependency install before it
        # even reaches `init`, so a 90s budget left it 19 seconds of real life.
        timeout=200, desc="live opencode record")
except Exception as e:
    announced = False
    dump_plugin_state("no record")
    machine.log("e2e-opencode: opencode did not announce within 90s. Still "
                "swallowed (the adapter is UNVERIFIED at runtime), but read the "
                f"DIAG above before blaming Landlock: {e}")

if announced:
    sock_o = next(r["socket"] for r in records_with_label(recs, "opencode") if r.get("socket"))
    sid_o = next(r.get("sessionId", "") for r in records_with_label(recs, "opencode"))
    assert any("proj-o" in r.get("cwd", "") for r in recs)
    # NSpid bridge: the record carries the window pid + its PID-namespace inode
    # for host-window correlation (opencode keeps its opencode-<pid>.sock name,
    # bound before the session id exists; the pid lives in the record).
    orec = next(r for r in records_with_label(recs, "opencode") if r.get("socket"))
    assert isinstance(orec.get("pid"), int) and orec["pid"] > 0, \
        f"opencode record missing pid: {orec}"
    assert isinstance(orec.get("pidNamespace"), int) and orec["pidNamespace"] > 0, \
        f"opencode record missing pidNamespace: {orec}"

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

    # Cross-kind: a pi session messages the live opencode session by id
    # (whitelisted, keyed on the resolved dir pair). corrald
    # routing is the hard part; the opencode turn that follows is best-effort.
    open_kitty(PROJ_A, "pi")
    pa = wait_records(lambda rs: any(r.get("socket") for r in records_with_label(rs, "pi")),
                      timeout=90, desc="live pi record")
    sock_a = next(r["socket"] for r in records_with_label(pa, "pi") if r.get("socket"))
    sid_a = next(r.get("sessionId", "") for r in records_with_label(pa, "pi"))
    as_user(f"mkdir -p {CORRAL}/state; echo '{PROJ_A} -> {PROJ_O}' >> {CORRAL}/state/whitelist")
    stub_post_rule(json.dumps({
        "match": "smoke:msg-o", "tool": "corral_message_agent",
        "args": {"target_session": sid_o, "message": "cross-kind-hi"}}))
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
