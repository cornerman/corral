# Shared prelude for every corral e2e scenario. Provides helpers the scenario
# scripts build on: run-as-alice (with the wayland/sway env), open a kitty
# window under sway, read the vetted state/registry, and poll for records.
import base64
import json
import time

USER = "alice"
UID = 1000
HOME = "/home/alice"
CORRAL = HOME + "/.corral"
STATE = CORRAL + "/state/registry"


def as_user(cmd):
    """Run a shell command as alice with the sway/wayland environment. The
    command is base64-encoded to avoid all quoting pitfalls."""
    enc = base64.b64encode(cmd.encode()).decode()
    return machine.succeed(
        f"su - {USER} -c 'export XDG_RUNTIME_DIR=/run/user/{UID}; "
        "export WAYLAND_DISPLAY=wayland-1; "
        f"export SWAYSOCK=$(ls /run/user/{UID}/sway-ipc.*.sock 2>/dev/null | head -1); "
        f"echo {enc} | base64 -d | bash'"
    )


def try_user(cmd):
    """Like as_user but returns (ok, output) instead of failing the test."""
    enc = base64.b64encode(cmd.encode()).decode()
    status, out = machine.execute(
        f"su - {USER} -c 'export XDG_RUNTIME_DIR=/run/user/{UID}; "
        "export WAYLAND_DISPLAY=wayland-1; "
        f"export SWAYSOCK=$(ls /run/user/{UID}/sway-ipc.*.sock 2>/dev/null | head -1); "
        f"echo {enc} | base64 -d | bash'"
    )
    return status == 0, out


def swaymsg(args):
    return as_user(f"swaymsg {args}")


def open_kitty(cwd, prog):
    """Open a kitty window under sway, cwd rooted at `cwd`, running `prog`."""
    swaymsg(f'exec "kitty --directory {cwd} {prog}"')


def boot():
    """Boot the VM, wait for sway and the stub LLM and corrald."""
    machine.start()
    machine.wait_for_unit("corral-stub-llm.service")
    machine.wait_until_succeeds(
        "curl -s -o /dev/null 127.0.0.1:6556/v1/models", timeout=60)
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_file("/tmp/sway-ready", timeout=120)
    # corrald is an alice user service on default.target.
    machine.wait_until_succeeds(
        f"test -S {CORRAL}/corrald.sock", timeout=60)


ACP_PY = "/etc/corral/acp.py"  # shipped by base.nix via environment.etc


def acp(args):
    """Run the in-VM ACP helper (nix/tests/acp.py) as alice; return stdout."""
    return as_user(f"python3 {ACP_PY} {args}")


def state_records():
    ok, ls = try_user(f"ls {STATE} 2>/dev/null")
    if not ok or not ls.strip():
        return []
    recs = []
    for f in ls.split():
        _, txt = try_user(f"cat {STATE}/{f}")
        try:
            recs.append(json.loads(txt))
        except ValueError:
            pass
    return recs


def dump_diag():
    """Dump why agents may not have announced -- called on a records timeout."""
    machine.log("=== DIAG: processes ===")
    machine.log(machine.execute("ps aux | grep -iE 'pi|nono|corrald|opencode|claude' | grep -v grep")[1])
    machine.log("=== DIAG: workdir .corral/registry ===")
    machine.log(machine.execute(f"ls -la {HOME}/proj-*/.corral/registry/ 2>&1")[1])
    machine.log("=== DIAG: input pointers ===")
    machine.log(machine.execute(f"ls -la {CORRAL}/input/registry/ 2>&1; cat {CORRAL}/input/registry/* 2>&1")[1])
    machine.log("=== DIAG: state/registry ===")
    machine.log(machine.execute(f"ls -la {STATE}/ 2>&1")[1])
    machine.log("=== DIAG: approved-commands ===")
    machine.log(machine.execute(f"cat {CORRAL}/state/approved-commands.json 2>&1")[1])
    machine.log("=== DIAG: corrald journal ===")
    machine.log(try_user("journalctl --user -u corrald -n 60 --no-pager 2>&1")[1])
    machine.log("=== DIAG: nono direct probe ===")
    machine.log(try_user(f"cd {HOME}/proj-a && nono run --profile /etc/corral/agent.jsonc -- pi --version 2>&1")[1])
    machine.log("=== DIAG: pi direct (no nono) ===")
    machine.log(try_user(f"cd {HOME}/proj-a && PI_TELEMETRY=0 $(which pi) --version 2>&1")[1])


def wait_records(pred, timeout=60, desc="records", diag=True):
    """Poll state/registry until pred(records) is true; return the records.
    On timeout, dump diagnostics (unless diag=False, for best-effort probes)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        recs = state_records()
        if pred(recs):
            return recs
        time.sleep(1)
    if diag:
        dump_diag()
    raise Exception(f"timeout waiting for {desc}; last records: {state_records()}")


def records_with_label(recs, label):
    return [r for r in recs if r.get("label") == label]


def stub_post_rule(rule_json):
    machine.succeed(
        f"curl -s -X POST 127.0.0.1:6556/rules -d '{rule_json}'")


def stub_requests():
    out = machine.succeed("curl -s 127.0.0.1:6556/requests")
    return json.loads(out)


def dump_messaging():
    """Diagnostics for the inter-agent messaging path."""
    machine.log("=== DIAG: whitelist ===")
    machine.log(machine.execute(f"cat {CORRAL}/state/whitelist 2>&1")[1])
    machine.log("=== DIAG: audit log ===")
    machine.log(machine.execute(f"cat {CORRAL}/state/audit.log 2>&1")[1])
    machine.log("=== DIAG: notify-send log ===")
    machine.log(machine.execute("cat /tmp/notify-send.log 2>&1")[1])
    machine.log("=== DIAG: corrald journal ===")
    machine.log(try_user("journalctl --user -u corrald -n 60 --no-pager 2>&1")[1])
    machine.log("=== DIAG: stub messages (last content per request) ===")
    for req in stub_requests():
        msgs = req["body"].get("messages", [])
        if msgs:
            machine.log(req["path"] + " :: " + json.dumps(msgs[-1].get("content", ""))[:200])


def window_count():
    """Number of app windows sway currently has mapped."""
    tree = json.loads(swaymsg("-t get_tree"))
    n = 0

    def walk(node):
        nonlocal n
        if node.get("pid") and node.get("type") in ("con", "floating_con"):
            n += 1
        for c in node.get("nodes", []) + node.get("floating_nodes", []):
            walk(c)

    walk(tree)
    return n
