#!/usr/bin/env python3
"""In-VM ACP assertion helper for the corral e2e tests.

Connects to a workdir-local agent socket, speaks newline-delimited JSON-RPC
(the corral ACP surface), and prints JSON for the test driver to parse.

Usage:
  acp.py list <socket>                 -> session/list result
  acp.py state <socket> <want> [secs]  -> wait until a state_update reports
                                          <want> (running|idle|requires_action);
                                          prints {"ok":true,"state":...}
  acp.py prompt <socket> <sid> <text>  -> send session/prompt (fire-and-forget)

Ground truth for the rest (records, focus, windows) is read directly by the
driver from state/registry, swaymsg, etc.; this helper only covers the live
socket surface.
"""
import json
import socket
import sys
import time


def connect(path, timeout=10):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(path)
    return s


def send(s, obj):
    s.sendall((json.dumps(obj) + "\n").encode())


def lines(s):
    buf = b""
    while True:
        chunk = s.recv(4096)
        if not chunk:
            return
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if line.strip():
                yield json.loads(line)


def rpc(s, method, params=None, rid=1):
    send(s, {"jsonrpc": "2.0", "id": rid, "method": method,
             "params": params or {}})
    for msg in lines(s):
        if msg.get("id") == rid:
            return msg


def cmd_list(path):
    s = connect(path)
    rpc(s, "initialize", {}, 1)
    res = rpc(s, "session/list", {}, 2)
    print(json.dumps(res.get("result", {})))


def cmd_state(path, want, secs):
    # Poll the socket until a state_update reports `want`, up to `secs`. Send
    # initialize but do NOT consume replies with rpc(): the extension seeds a
    # state_update notification right after connect, and rpc() would discard it
    # while waiting for the init reply. Read every line and inspect it.
    deadline = time.time() + secs
    s = connect(path, timeout=secs + 2)
    send(s, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    buf = b""
    while time.time() < deadline:
        try:
            chunk = s.recv(4096)
        except socket.timeout:
            break
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            msg = json.loads(line)
            st = None
            if msg.get("method") in ("session/update", "state_update"):
                p = msg.get("params", {})
                st = p.get("state") or p.get("update", {}).get("state")
            if st == want:
                print(json.dumps({"ok": True, "state": st}))
                return
    print(json.dumps({"ok": False}))
    sys.exit(1)


def cmd_prompt(path, sid, text):
    s = connect(path)
    rpc(s, "initialize", {}, 1)
    send(s, {"jsonrpc": "2.0", "id": 2, "method": "session/prompt",
             "params": {"sessionId": sid,
                        "prompt": [{"type": "text", "text": text}]}})
    time.sleep(1)
    print(json.dumps({"ok": True}))


def cmd_cancel(path, sid):
    s = connect(path)
    rpc(s, "initialize", {}, 1)
    send(s, {"jsonrpc": "2.0", "method": "session/cancel",
             "params": {"sessionId": sid}})
    time.sleep(1)
    print(json.dumps({"ok": True}))


if __name__ == "__main__":
    op = sys.argv[1]
    if op == "list":
        cmd_list(sys.argv[2])
    elif op == "state":
        cmd_state(sys.argv[2], sys.argv[3],
                  int(sys.argv[4]) if len(sys.argv) > 4 else 15)
    elif op == "prompt":
        cmd_prompt(sys.argv[2], sys.argv[3], sys.argv[4])
    elif op == "cancel":
        cmd_cancel(sys.argv[2], sys.argv[3])
    else:
        sys.exit("unknown op: " + op)
