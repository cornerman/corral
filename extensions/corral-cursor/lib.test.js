const test = require("node:test");
const assert = require("node:assert");
const lib = require("./lib.js");

test("registryDir honors override then HOME then undefined", () => {
  assert.equal(lib.registryDir({ CORRAL_REGISTRY_DIR: "/x" }), "/x");
  assert.equal(lib.registryDir({ HOME: "/home/u" }), "/home/u/.corral/registry");
  assert.equal(lib.registryDir({}), undefined);
});

test("socket and control paths use socketDir override", () => {
  const env = { CORRAL_SOCKET_DIR: "/s" };
  assert.equal(lib.acpSocketPath("/w", 42, env), "/s/cursor-42.sock");
  assert.equal(lib.controlSocketPath("/w", "sid", env), "/s/.cursor-ctl-sid.sock");
  assert.equal(lib.acpSocketPath("/w", 42, {}), "/w/.corral/cursor-42.sock");
});

test("buildRecord fixes label/gui/commands and omits messageFlag", () => {
  const r = lib.buildRecord({ sessionId: "sid", cwd: "/w", title: "t", socket: "/w/.corral/cursor-42.sock", nowIso: "2026-07-15T00:00:00.000Z" });
  assert.equal(r.label, "cursor");
  assert.equal(r.gui, true);
  assert.deepEqual(r.spawnCommand, ["cursor", "/w"]);
  assert.deepEqual(r.resumeCommand, ["cursor", "/w"]);
  assert.equal("messageFlag" in r, false);
  assert.equal(r.sessionId, "sid");
  assert.equal(r.socket, "/w/.corral/cursor-42.sock");
  assert.equal(r.lastSeen, "2026-07-15T00:00:00.000Z");
});

test("acpReply: initialize returns cursor identity", () => {
  const r = lib.acpReply({ id: 0, method: "initialize", params: {} }, { sessionId: "s", title: "t", cwd: "/w", state: "idle" });
  assert.equal(r.result.agentInfo.name, "cursor");
  assert.equal(r.result.agentCapabilities.loadSession, false);
  assert.equal(r.id, 0);
});

test("acpReply: session/list returns the single session", () => {
  const r = lib.acpReply({ id: 1, method: "session/list", params: {} }, { sessionId: "s", title: "t", cwd: "/w", state: "idle" });
  assert.deepEqual(r.result.sessions, [{ sessionId: "s", title: "t", cwd: "/w" }]);
});

test("acpReply: session/prompt yields an __inject sentinel with joined text", () => {
  const r = lib.acpReply({ id: 2, method: "session/prompt", params: { prompt: [{ type: "text", text: "a" }, { type: "text", text: "b" }, { type: "image" }] } }, {});
  assert.equal(r.__inject, "a\nb");
  assert.equal(r.id, 2);
});

test("acpReply: empty prompt is a JSON-RPC error", () => {
  const r = lib.acpReply({ id: 3, method: "session/prompt", params: { prompt: [] } }, {});
  assert.equal(r.error.code, -32602);
});

test("acpReply: session/cancel and id-less requests return null", () => {
  assert.equal(lib.acpReply({ method: "session/cancel" }, {}), null);
  assert.equal(lib.acpReply({ method: "initialize" }, {}), null);
});

test("acpReply: unknown method is -32601", () => {
  const r = lib.acpReply({ id: 9, method: "session/new", params: {} }, {});
  assert.equal(r.error.code, -32601);
});

test("acpUpdate wraps an update in the session/update envelope", () => {
  const u = lib.acpUpdate("s", { sessionUpdate: "state_update", state: "running" });
  assert.equal(u.method, "session/update");
  assert.equal(u.params.sessionId, "s");
  assert.equal(u.params.update.state, "running");
});

test("resolveWindowPid climbs to the outermost cursor/electron ancestor", () => {
  const table = {
    100: { ppid: 90, comm: "node" },
    90: { ppid: 50, comm: "Cursor Helper (Plugin)" },
    50: { ppid: 1, comm: "cursor" },
    1: { ppid: 0, comm: "systemd" },
  };
  const read = (pid) => table[pid] || null;
  assert.equal(lib.resolveWindowPid(100, read), 50);
});

test("resolveWindowPid falls back to start when no ancestor matches", () => {
  const table = { 7: { ppid: 1, comm: "node" }, 1: { ppid: 0, comm: "systemd" } };
  assert.equal(lib.resolveWindowPid(7, (p) => table[p] || null), 7);
});

test("resolveWindowPid stops safely on a cycle / missing entry", () => {
  assert.equal(lib.resolveWindowPid(5, () => null), 5);
});

test("mergeHooks writes flat {command} entries per stage and preserves others", () => {
  const existing = { version: 1, hooks: { stop: [{ command: "other-hook" }] } };
  let out = lib.mergeHooks(existing, "beforeSubmitPrompt", "node /abs/state-hook.js beforeSubmitPrompt");
  out = lib.mergeHooks(out, "stop", "node /abs/state-hook.js stop");
  assert.ok(out.hooks.stop.some((h) => h.command === "other-hook"));
  assert.ok(out.hooks.stop.some((h) => h.command === "node /abs/state-hook.js stop"));
  assert.ok(out.hooks.beforeSubmitPrompt.some((h) => h.command === "node /abs/state-hook.js beforeSubmitPrompt"));
});

test("mergeHooks is idempotent and self-heals a changed path", () => {
  let out = lib.mergeHooks({}, "stop", "/old/node /a/state-hook.js stop");
  out = lib.mergeHooks(out, "stop", "/new/node /a/state-hook.js stop");
  const ours = out.hooks.stop.filter((h) => h.command.includes("state-hook.js"));
  assert.equal(ours.length, 1);
  assert.equal(ours[0].command, "/new/node /a/state-hook.js stop");
});

test("hookEventToState maps the two observed stages", () => {
  assert.equal(lib.hookEventToState("beforeSubmitPrompt"), "running");
  assert.equal(lib.hookEventToState("stop"), "idle");
  assert.equal(lib.hookEventToState("afterFileEdit"), null);
});

test("isControlSocketFile matches the ctl socket name only", () => {
  assert.equal(lib.isControlSocketFile(".cursor-ctl-e55bb4e8.sock"), true);
  assert.equal(lib.isControlSocketFile("cursor-121602.sock"), false);
  assert.equal(lib.isControlSocketFile("pi-102342.sock"), false);
});
