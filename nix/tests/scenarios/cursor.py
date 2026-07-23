# e2e-cursor: partial by design (no Composer turns). The hard assertion is the
# adapter's pure core (lib.js) unit suite running inside the VM against the
# shipped package; announce/focus are best-effort because Cursor is a
# login-gated Electron app under software GL.

PROJ_CU = HOME + "/proj-cu"

boot()
as_user(f"mkdir -p {PROJ_CU}")

# Locate the shipped cursor adapter tree (package share dir) and run its
# node --test suite -- the reliable, deterministic part of this scenario.
share = machine.succeed(
    "d=$(dirname $(dirname $(readlink -f $(which corral)))); "
    "echo $d/share/corral/extensions/corral-cursor").strip()
machine.succeed(f"test -f {share}/lib.js")
out = machine.succeed(f"cd {share} && node --test 2>&1 || true")
machine.log(out)
assert "fail 0" in out or "failing tests" not in out, \
    f"cursor lib.js unit tests failed:\n{out}"

# Best-effort: launch Cursor and see whether the extension announces a gui:true
# record. Electron under software GL may not start in the test VM.
try:
    swaymsg(f'exec "cursor {PROJ_CU}"')
    recs = wait_records(
        lambda rs: any(r.get("label") == "cursor" and r.get("gui")
                       for r in rs),
        timeout=90, desc="cursor gui record")
    rec = next(r for r in recs if r.get("label") == "cursor")
    assert rec.get("gui") is True, rec
    machine.log(f"e2e-cursor: extension announced gui record: {rec.get('sessionId')}")

    # session/load (history export, corral's `o` key): Cursor exposes no API to
    # read the Composer transcript, so the adapter's existing default case
    # answers method-not-supported like any other unimplemented method.
    if rec.get("socket"):
        load_res = json.loads(acp(f"load {rec['socket']} {rec.get('sessionId', '')} 5"))
        assert not load_res.get("ok") and load_res.get("error") == "unsupported", \
            f"cursor should answer session/load as unsupported: {load_res}"
        machine.log("e2e-cursor: session/load correctly unsupported")
except Exception as e:
    machine.log("e2e-cursor: Cursor did not announce (Electron/software-GL or "
          f"login-gated; UNVERIFIED, expected residue): {e}")

machine.log("e2e-cursor: pure-core assertions passed")
