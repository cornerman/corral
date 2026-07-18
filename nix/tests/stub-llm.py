#!/usr/bin/env python3
"""Deterministic stub LLM for the corral VM e2e tests.

Speaks two dialects on one port:
  POST /v1/chat/completions  OpenAI Chat Completions (pi, opencode), SSE + JSON
  POST /v1/messages          Anthropic Messages (claude-code), SSE + JSON
  GET  /v1/models            OpenAI model list

Behavior is a rule table: the first rule whose `match` substring occurs in the
last request message wins and yields either a text reply or a tool call. The
test driver adds rules with baked-in dynamic values (session ids) at runtime:
  POST /rules   {"match": str, "reply": str} | {"match": str, "tool": str, "args": {...}}
Runtime rules take priority over the built-ins, newest first (mirrors
mock-llm's last-match-wins). Every request body is kept for assertions:
  GET  /requests   -> JSON array of {path, body}

Replaces mock-llm: its streaming path only chunks message.content, so a canned
tool_calls response cannot be streamed, and pi's openai-completions client
always streams (see the design spec, dated note 2026-07-18).

stdlib only; runs as a systemd service in the test VM.
"""

import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 6556

# Built-in rules, checked after runtime rules. `match` is a substring of the
# stringified last message. Tool rules drive pi's corral tools; the test
# chooses agent behavior by the prompt it sends.
BUILTIN_RULES = [
    {"match": "smoke:ask", "tool": "question",
     "args": {"question": "Proceed?", "options": ["yes", "no"]}},
    {"match": "smoke:msg-b", "tool": "corral_message_agent",
     "args": {"target_dir": "/home/alice/proj-b", "message": "hello-from-a"}},
    {"match": "smoke:list", "tool": "list_corral_agents", "args": {}},
    {"match": "", "reply": "pong"},  # catch-all
]

LOCK = threading.Lock()
RUNTIME_RULES = []
REQUESTS = []


def last_message_text(body, anthropic):
    msgs = body.get("messages", [])
    if not msgs:
        return "", ""
    last = msgs[-1]
    role = last.get("role", "")
    # Tool results: OpenAI uses role "tool"; Anthropic nests tool_result blocks
    # in a user message. Content may be a string or a list of typed parts.
    text = json.dumps(last.get("content", ""))
    return role, text


def pick(body, anthropic):
    role, text = last_message_text(body, anthropic)
    if role == "tool" or '"tool_result"' in text:
        # A finished tool call: close the turn with plain text so the agent
        # goes back to idle instead of looping.
        return {"reply": "done"}
    with LOCK:
        rules = list(RUNTIME_RULES) + BUILTIN_RULES
    for rule in rules:
        if rule["match"] in text:
            return rule
    return {"reply": "pong"}


# ---- OpenAI shapes ---------------------------------------------------------

def openai_message(rule):
    if "tool" in rule:
        return {
            "role": "assistant",
            "content": None,
            "tool_calls": [{
                "id": "call_smoke_1",
                "type": "function",
                "function": {"name": rule["tool"],
                             "arguments": json.dumps(rule["args"])},
            }],
        }, "tool_calls"
    return {"role": "assistant", "content": rule["reply"]}, "stop"


def openai_json(model, rule):
    message, finish = openai_message(rule)
    return {
        "id": "chatcmpl-smoke", "object": "chat.completion", "created": 0,
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


def openai_sse(model, rule):
    """Chunks: role first, then one delta carrying the whole content/tool
    call, then the finish chunk. Whole-in-one-delta is accepted by OpenAI
    clients (arguments arrive as a single string fragment)."""
    def chunk(delta, finish=None):
        return {"id": "chatcmpl-smoke", "object": "chat.completion.chunk",
                "created": 0, "model": model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
    message, finish = openai_message(rule)
    out = [chunk({"role": "assistant"})]
    if "tool" in rule:
        tc = message["tool_calls"][0]
        out.append(chunk({"tool_calls": [{
            "index": 0, "id": tc["id"], "type": "function",
            "function": {"name": tc["function"]["name"],
                         "arguments": tc["function"]["arguments"]}}]}))
    else:
        out.append(chunk({"content": message["content"]}))
    out.append(chunk({}, finish))
    return out


# ---- Anthropic shapes ------------------------------------------------------

def anthropic_json(model, rule):
    if "tool" in rule:
        content = [{"type": "tool_use", "id": "toolu_smoke_1",
                    "name": rule["tool"], "input": rule["args"]}]
        stop = "tool_use"
    else:
        content = [{"type": "text", "text": rule["reply"]}]
        stop = "end_turn"
    return {
        "id": "msg_smoke", "type": "message", "role": "assistant",
        "model": model, "content": content,
        "stop_reason": stop, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }


def anthropic_sse(model, rule):
    msg = anthropic_json(model, rule)
    block = msg["content"][0]
    events = [
        ("message_start", {"type": "message_start", "message": {**msg, "content": [], "stop_reason": None}}),
    ]
    if block["type"] == "text":
        events += [
            ("content_block_start", {"type": "content_block_start", "index": 0,
                                     "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                     "delta": {"type": "text_delta", "text": block["text"]}}),
        ]
    else:
        events += [
            ("content_block_start", {"type": "content_block_start", "index": 0,
                                     "content_block": {"type": "tool_use", "id": block["id"],
                                                       "name": block["name"], "input": {}}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                     "delta": {"type": "input_json_delta",
                                               "partial_json": json.dumps(block["input"])}}),
        ]
    events += [
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        ("message_delta", {"type": "message_delta",
                           "delta": {"stop_reason": msg["stop_reason"], "stop_sequence": None},
                           "usage": {"output_tokens": 1}}),
        ("message_stop", {"type": "message_stop"}),
    ]
    return events


# ---- HTTP ------------------------------------------------------------------

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("stub-llm: " + fmt % args + "\n")

    def _json(self, obj, status=200):
        data = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _sse_headers(self):
        # SSE has no Content-Length; close the connection when the stream ends
        # so clients (and curl) see EOF instead of a hung keep-alive socket.
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True

    def _sse_openai(self, chunks):
        self._sse_headers()
        for c in chunks:
            self.wfile.write(b"data: " + json.dumps(c).encode() + b"\n\n")
        self.wfile.write(b"data: [DONE]\n\n")

    def _sse_anthropic(self, events):
        self._sse_headers()
        for name, payload in events:
            self.wfile.write(("event: %s\ndata: %s\n\n" % (name, json.dumps(payload))).encode())

    def do_GET(self):
        if self.path.startswith("/v1/models"):
            self._json({"object": "list",
                        "data": [{"id": "smoke", "object": "model", "owned_by": "stub"}]})
        elif self.path == "/requests":
            with LOCK:
                self._json(REQUESTS)
        else:
            self._json({"error": "not found"}, 404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw or b"{}")
        except ValueError:
            self._json({"error": "bad json"}, 400)
            return
        if self.path == "/rules":
            with LOCK:
                RUNTIME_RULES.insert(0, body)
            self._json({"ok": True})
            return
        with LOCK:
            REQUESTS.append({"path": self.path, "body": body})
        anthropic = self.path.startswith("/v1/messages")
        openai = re.search(r"/chat/completions$", self.path)
        if not (anthropic or openai):
            self._json({"error": "not found"}, 404)
            return
        rule = pick(body, anthropic)
        model = body.get("model", "smoke")
        stream = bool(body.get("stream"))
        if anthropic:
            if stream:
                self._sse_anthropic(anthropic_sse(model, rule))
            else:
                self._json(anthropic_json(model, rule))
        else:
            if stream:
                self._sse_openai(openai_sse(model, rule))
            else:
                self._json(openai_json(model, rule))


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else PORT
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    sys.stderr.write("stub-llm: listening on %d\n" % port)
    server.serve_forever()
