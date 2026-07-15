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
