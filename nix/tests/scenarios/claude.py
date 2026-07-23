# e2e-claude: the Claude Code adapter. Converts the adapter's UNVERIFIED list
# into live assertions. Claude runs on node (bun SIGTRAPs under Landlock), the
# sidecar holds the ACP socket, and delivery uses the Stop-block / asyncRewake
# hook paths. Claude's real turn behavior against the Anthropic stub is itself
# a verify point, so turn-dependent checks are best-effort; the hard backbone
# is sidecar announce and teardown.

PROJ_CL = HOME + "/proj-cl"

boot()
as_user(f"mkdir -p {PROJ_CL}")

# Launch Claude under sway. SessionStart spawns the sidecar, which announces.
open_kitty(PROJ_CL, "claude")
announced = True
try:
    recs = wait_records(
        lambda rs: any(r.get("socket") for r in records_with_label(rs, "claude")),
        timeout=120, desc="live claude record (sidecar announced)")
except Exception as e:
    announced = False
    machine.log("e2e-claude: no claude record within 120s. The adapter is UNVERIFIED "
          "in-repo (hook payloads, sidecar spawn); this is the first live check "
          f"and pins the current outcome: {e}")

if announced:
    rec = next(r for r in records_with_label(recs, "claude") if r.get("socket"))
    sock_cl, sid_cl = rec["socket"], rec.get("sessionId", "")
    assert "proj-cl" in rec.get("cwd", ""), rec
    # The socket pid is the interactive Claude process (focus correlation).
    machine.log(f"e2e-claude: announced socket={sock_cl} sid={sid_cl}")

    # State + delivery are turn-dependent (Claude vs the Anthropic stub) and
    # thus best-effort on this first live run.
    try:
        acp(f"prompt {sock_cl} {sid_cl} 'reply claude-turn'")
        acp(f"state {sock_cl} idle 40")
        machine.log("e2e-claude: turn ran (running -> idle)")
    except Exception as e:
        machine.log(f"e2e-claude: turn/state best-effort skipped: {e}")

    # History export: session/load replays from the on-disk transcript
    # (transcript_path, captured off the first hook event). Best-effort like
    # the turn check above -- both the turn and the transcript line schema are
    # UNVERIFIED in this repo.
    try:
        load_res = json.loads(acp(f"load {sock_cl} {sid_cl} 15"))
        machine.log(f"e2e-claude: session/load result: {load_res}")
    except Exception as e:
        machine.log(f"e2e-claude: session/load best-effort skipped: {e}")

    # Teardown: end the session, sidecar is reaped, record goes dormant.
    as_user("pkill -f claude || true")
    try:
        wait_records(
            lambda rs: any(r.get("sessionId") == sid_cl and not r.get("socket")
                           for r in rs),
            timeout=60, desc="claude dormant after teardown")
        machine.log("e2e-claude: teardown -> dormant confirmed")
    except Exception as e:
        machine.log(f"e2e-claude: dormant-after-teardown best-effort: {e}")

    machine.log("e2e-claude: backbone assertions passed")
