# corral-cursor Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a corral adapter that makes an interactive Cursor desktop (Electron IDE) window discoverable, focusable, resumable, and best-effort messageable from a corral board.

**Architecture:** A VSIX extension is the resident owner running in Cursor's extension host — it binds a workdir-local ACP socket, writes a `gui: true` registry record, serves the ACP surface, and (best-effort) opens a new pre-filled Composer chat for `session/prompt`. A tiny `state-hook.js`, registered via an auto-merged `~/.cursor/hooks.json`, feeds `running`/`idle` to the extension over a per-window control socket. No corral core (Rust) changes.

**Tech Stack:** Plain JavaScript (no build step, no TypeScript, loaded directly by the extension host), Node built-ins (`net`, `fs`, `path`), `node --test` for the pure helpers, the `vscode` extension API (runtime-only, not available for tests).

## Global Constraints

- Directory: `extensions/corral-cursor/`. Adapter is a pure new sibling of `corral-pi.ts` / `corral-opencode.ts` / `corral-claude/`. **No changes to any Rust crate.**
- Language: **plain JavaScript** (`.js`), no build step, no `tsconfig`, no bundler. The extension `main` is loaded as JS by the extension host.
- Convention contract is `CONVENTION.md` (implement §2–§6 verbatim): registry dir `0700`, record `0600`, atomic write (temp + rename); socket dir `0700`; newline-delimited JSON-RPC 2.0.
- Registry record fixed values: `label: "cursor"`, `gui: true`, `spawnCommand: ["cursor", "<cwd>"]`, `resumeCommand: ["cursor", "<cwd>"]`, no `messageFlag`.
- Socket filename: `cursor-<electronPid>.sock` where `<electronPid>` is the Cursor window-owning (Electron main) pid, so corral's `gui` focus (`focus.rs` `match_pids`, strict socket-pid match) raises the real window.
- Control socket: `.cursor-ctl-<sessionId>.sock` beside the ACP socket.
- `state_update` vocabulary: `running` / `idle` only (no `requires_action`; Cursor exposes no permission hook).
- `session/prompt` opens a **new** Composer chat pre-filled with the text (never intrudes on the open chat).
- Env overrides honored: `$CORRAL_REGISTRY_DIR`, `$CORRAL_SOCKET_DIR`.
- UNVERIFIED (no Cursor runtime here): the Composer command ID(s), Electron-pid resolution, hook payload field names. Every `vscode`/Cursor access is guarded; the extension never throws into the host. Mark these in-file exactly as `corral-claude` does.
- `node` must be on PATH for the hook (document it; same requirement/rationale as `corral-claude`). Tests run with `node --test` (any Node >= 18).
- One-session-per-socket (one card per Cursor window). Multi-session is explicitly out of scope (Future in the spec).

**Spec:** `docs/superpowers/specs/2026-07-15-corral-cursor-adapter-design.md`. Reference implementations: `extensions/corral-claude/sidecar.ts` (resident owner, ACP surface, registry store) and `extensions/corral-claude/hook.ts` (hook shim + control-socket handshake).

---

## File Structure

- Create `extensions/corral-cursor/lib.js` — pure helpers (paths, record, ACP reply dispatch, pid walk, hooks-merge). The functional core; fully unit-tested.
- Create `extensions/corral-cursor/lib.test.js` — `node --test` for `lib.js`.
- Create `extensions/corral-cursor/extension.js` — the resident owner (activation, socket servers, registry write, broadcast, injection, lifecycle). `vscode`-coupled; UNVERIFIED shell.
- Create `extensions/corral-cursor/state-hook.js` — the thin hook shim (event stdin → state ping on control socket).
- Create `extensions/corral-cursor/hooks.template.json` — the hook registration merged into `~/.cursor/hooks.json`.
- Create `extensions/corral-cursor/package.json` — VSIX manifest (`main`, `activationEvents`, `contributes.configuration`).
- Create `extensions/corral-cursor/README.md` — install, node requirement, injection config, limitations.
- Modify `AGENTS.md` and `README.md` — add the `corral-cursor` adapter entry (repo hard rule: keep both current).

---

## Task 1: Pure helpers — paths and registry record

**Files:**
- Create: `extensions/corral-cursor/lib.js`
- Test: `extensions/corral-cursor/lib.test.js`

**Interfaces:**
- Produces:
  - `registryDir(env)` → `string | undefined` (`env.CORRAL_REGISTRY_DIR` or `<env.HOME>/.corral/registry`, else undefined).
  - `socketDir(cwd, env)` → `string` (`env.CORRAL_SOCKET_DIR` or `<cwd>/.corral`).
  - `acpSocketPath(cwd, electronPid, env)` → `string` (`<socketDir>/cursor-<electronPid>.sock`).
  - `controlSocketPath(cwd, sessionId, env)` → `string` (`<socketDir>/.cursor-ctl-<sessionId>.sock`).
  - `buildRecord({ sessionId, cwd, title, socket, nowIso })` → the registry record object (fixed `label`/`gui`/`spawnCommand`/`resumeCommand`, no `messageFlag`).

- [ ] **Step 1: Write the failing test**

```js
// extensions/corral-cursor/lib.test.js
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: FAIL — `Cannot find module './lib.js'`.

- [ ] **Step 3: Write minimal implementation**

```js
// extensions/corral-cursor/lib.js
// Pure helpers for the corral-cursor extension. No vscode, no side effects, so
// this module is unit-tested with `node --test` (see lib.test.js). The
// vscode-coupled shell (extension.js) imports these.
"use strict";
const path = require("node:path");

function registryDir(env) {
  if (env.CORRAL_REGISTRY_DIR) return env.CORRAL_REGISTRY_DIR;
  return env.HOME ? path.join(env.HOME, ".corral", "registry") : undefined;
}
function socketDir(cwd, env) {
  return env.CORRAL_SOCKET_DIR || path.join(cwd, ".corral");
}
function acpSocketPath(cwd, electronPid, env) {
  return path.join(socketDir(cwd, env), `cursor-${electronPid}.sock`);
}
function controlSocketPath(cwd, sessionId, env) {
  return path.join(socketDir(cwd, env), `.cursor-ctl-${sessionId}.sock`);
}
// The gui:true record corral runs verbatim. resume == spawn: for a GUI editor
// "resume" is just reopening the workspace folder. No messageFlag: cursor <dir>
// cannot carry prompt text (see spec Messaging/Future).
function buildRecord({ sessionId, cwd, title, socket, nowIso }) {
  return {
    sessionId,
    cwd,
    title: title ?? null,
    label: "cursor",
    socket,
    gui: true,
    spawnCommand: ["cursor", cwd],
    resumeCommand: ["cursor", cwd],
    lastSeen: nowIso,
  };
}

module.exports = { registryDir, socketDir, acpSocketPath, controlSocketPath, buildRecord };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js
git commit -m "feat(corral-cursor): pure path + registry-record helpers"
```

---

## Task 2: Pure helper — ACP request→reply dispatch

**Files:**
- Modify: `extensions/corral-cursor/lib.js`
- Test: `extensions/corral-cursor/lib.test.js`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `acpReply(msg, ctx)` → `object | null`. `msg` is a parsed JSON-RPC request; `ctx = { sessionId, title, cwd, state }`. Returns the JSON-RPC response object to send, or `null` when nothing should be sent (notifications like `session/cancel`, or a request with no `id`). `session/prompt` returns a sentinel `{ jsonrpc, id, __inject: text }` the shell turns into an injection attempt (kept out of pure code). This function performs no I/O.
  - `acpUpdate(sessionId, update)` → the `session/update` notification envelope object.

- [ ] **Step 1: Write the failing test**

```js
// append to lib.test.js
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: FAIL — `lib.acpReply is not a function`.

- [ ] **Step 3: Write minimal implementation**

```js
// add to lib.js (before module.exports)
function acpUpdate(sessionId, update) {
  return { jsonrpc: "2.0", method: "session/update", params: { sessionId, update } };
}

// Pure: compute the JSON-RPC response for one request. No I/O. session/prompt
// returns a sentinel carrying the joined text; the shell (extension.js) attempts
// the Composer injection and, on that result, sends the real reply.
function acpReply(msg, ctx) {
  if (!msg || !msg.method) return null;
  if (msg.method === "session/cancel") return null; // notification, no external abort
  if (msg.id === undefined) return null;
  const ok = (result) => ({ jsonrpc: "2.0", id: msg.id, result });
  const err = (code, message) => ({ jsonrpc: "2.0", id: msg.id, error: { code, message } });
  switch (msg.method) {
    case "initialize":
      return ok({
        protocolVersion: 1,
        agentCapabilities: { loadSession: false },
        agentInfo: { name: "cursor", version: "unknown" },
        authMethods: [],
      });
    case "session/list":
      return ok({ sessions: [{ sessionId: ctx.sessionId, title: ctx.title ?? null, cwd: ctx.cwd }] });
    case "session/prompt": {
      const text = ((msg.params && msg.params.prompt) || [])
        .filter((b) => b && b.type === "text" && typeof b.text === "string")
        .map((b) => b.text)
        .join("\n");
      if (!text) return err(-32602, "prompt has no text content");
      return { jsonrpc: "2.0", id: msg.id, __inject: text };
    }
    default:
      return err(-32601, `method not supported by corral-cursor: ${msg.method}`);
  }
}

// extend module.exports
module.exports = { registryDir, socketDir, acpSocketPath, controlSocketPath, buildRecord, acpReply, acpUpdate };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS (all tests).

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js
git commit -m "feat(corral-cursor): pure ACP request->reply dispatch"
```

---

## Task 3: Pure helper — Electron window pid resolution

**Files:**
- Modify: `extensions/corral-cursor/lib.js`
- Test: `extensions/corral-cursor/lib.test.js`

**Interfaces:**
- Produces:
  - `resolveWindowPid(startPid, readProc)` → `number`. Walks the process tree upward from `startPid` (the extension host) and returns the outermost ancestor whose `comm` matches Cursor/Electron (case-insensitive contains `cursor` or equals `electron`); falls back to `startPid` if none matches or the walk is exhausted. `readProc(pid)` → `{ ppid: number, comm: string } | null` is injected so the walk is testable without a real `/proc`.

- [ ] **Step 1: Write the failing test**

```js
// append to lib.test.js
test("resolveWindowPid climbs to the outermost cursor/electron ancestor", () => {
  // 100 extension-host node -> 90 (Cursor Helper) -> 50 cursor (main) -> 1 init
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: FAIL — `lib.resolveWindowPid is not a function`.

- [ ] **Step 3: Write minimal implementation**

```js
// add to lib.js
// Walk up the process tree to the outermost Cursor/Electron ancestor: its pid is
// what the WM reports as _NET_WM_PID for the window, so naming the socket with it
// lets corral's gui focus (strict socket-pid match) raise the real window.
// UNVERIFIED: that the extension host's ancestor chain reaches the main process
// and that _NET_WM_PID equals it. readProc is injected for testing.
function resolveWindowPid(startPid, readProc) {
  const isCursor = (comm) => {
    const c = String(comm || "").toLowerCase();
    return c.includes("cursor") || c === "electron";
  };
  let best = startPid;
  let pid = startPid;
  const seen = new Set();
  for (let i = 0; i < 32; i++) {
    if (seen.has(pid)) break;
    seen.add(pid);
    const info = readProc(pid);
    if (!info) break;
    if (isCursor(info.comm)) best = pid; // keep climbing to the OUTERMOST match
    if (!info.ppid || info.ppid <= 1) break;
    pid = info.ppid;
  }
  return best;
}

// extend module.exports with resolveWindowPid
```

Also update the `module.exports` line to include `resolveWindowPid`.

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js
git commit -m "feat(corral-cursor): electron window-pid resolver (injectable proc reader)"
```

---

## Task 4: Pure helper — additive `~/.cursor/hooks.json` merge

**Files:**
- Modify: `extensions/corral-cursor/lib.js`
- Test: `extensions/corral-cursor/lib.test.js`

**Interfaces:**
- Produces:
  - `mergeHooks(existing, hookCommand)` → new hooks-config object. `existing` is the parsed `~/.cursor/hooks.json` (or `{}`). `hookCommand` is `{ command: "node", args: [absPath] }`. Adds a `beforeSubmitPrompt` and a `stop` entry pointing at our command **without duplicating** (idempotent: keyed on the args[0] absolute path) and **without dropping** any pre-existing hooks. Returns `{ version: 1, hooks: { ... } }`.

- [ ] **Step 1: Write the failing test**

```js
// append to lib.test.js
test("mergeHooks adds our beforeSubmitPrompt+stop and preserves others", () => {
  const existing = { version: 1, hooks: { stop: [{ hooks: [{ command: "other", args: ["/x"] }] }] } };
  const out = lib.mergeHooks(existing, { command: "node", args: ["/abs/state-hook.js"] });
  // preserves the pre-existing stop hook
  assert.ok(out.hooks.stop.some((g) => g.hooks.some((h) => h.command === "other")));
  // adds ours to stop and beforeSubmitPrompt
  assert.ok(out.hooks.stop.some((g) => g.hooks.some((h) => h.args && h.args[0] === "/abs/state-hook.js")));
  assert.ok(out.hooks.beforeSubmitPrompt.some((g) => g.hooks.some((h) => h.args && h.args[0] === "/abs/state-hook.js")));
});

test("mergeHooks is idempotent (no duplicate of our command)", () => {
  const once = lib.mergeHooks({}, { command: "node", args: ["/abs/state-hook.js"] });
  const twice = lib.mergeHooks(once, { command: "node", args: ["/abs/state-hook.js"] });
  const count = twice.hooks.stop.filter((g) => g.hooks.some((h) => h.args && h.args[0] === "/abs/state-hook.js")).length;
  assert.equal(count, 1);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: FAIL — `lib.mergeHooks is not a function`.

- [ ] **Step 3: Write minimal implementation**

```js
// add to lib.js
// Additively register our state-hook for beforeSubmitPrompt + stop, keyed on our
// absolute args[0] so re-activation never duplicates and never drops a user's own
// hooks. Pure: caller reads/writes the file.
function mergeHooks(existing, hookCommand) {
  const out = { version: 1, hooks: {} };
  const src = (existing && existing.hooks) || {};
  for (const k of Object.keys(src)) out.hooks[k] = Array.isArray(src[k]) ? src[k].slice() : src[k];
  const ourPath = hookCommand.args && hookCommand.args[0];
  const group = { hooks: [{ type: "command", command: hookCommand.command, args: hookCommand.args.slice() }] };
  for (const stage of ["beforeSubmitPrompt", "stop"]) {
    const arr = Array.isArray(out.hooks[stage]) ? out.hooks[stage].slice() : [];
    const present = arr.some((g) => (g.hooks || []).some((h) => h.args && h.args[0] === ourPath));
    if (!present) arr.push(group);
    out.hooks[stage] = arr;
  }
  return out;
}

// extend module.exports with mergeHooks
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js
git commit -m "feat(corral-cursor): additive idempotent hooks.json merge"
```

---

## Task 5: The hook shim — `state-hook.js`

**Files:**
- Create: `extensions/corral-cursor/state-hook.js`
- Modify: `extensions/corral-cursor/lib.js` (add `hookEventToState`)
- Test: `extensions/corral-cursor/lib.test.js`

**Interfaces:**
- Produces (pure, in lib.js):
  - `hookEventToState(eventName)` → `"running" | "idle" | null`. `beforeSubmitPrompt` → `running`; `stop` → `idle`; anything else → `null`.
- `state-hook.js` is executable I/O glue (UNVERIFIED payload fields): read stdin JSON, derive cwd + sessionId + event name, map to a state, connect to the control socket, send one line `{ kind: "state", state }`, exit. Never throws into Cursor.

- [ ] **Step 1: Write the failing test (pure part)**

```js
// append to lib.test.js
test("hookEventToState maps the two observed stages", () => {
  assert.equal(lib.hookEventToState("beforeSubmitPrompt"), "running");
  assert.equal(lib.hookEventToState("stop"), "idle");
  assert.equal(lib.hookEventToState("afterFileEdit"), null);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: FAIL — `lib.hookEventToState is not a function`.

- [ ] **Step 3: Implement `hookEventToState` in lib.js, then write `state-hook.js`**

```js
// add to lib.js + export
function hookEventToState(eventName) {
  if (eventName === "beforeSubmitPrompt") return "running";
  if (eventName === "stop") return "idle";
  return null;
}
```

```js
#!/usr/bin/env node
// extensions/corral-cursor/state-hook.js
// The thin bridge Cursor runs for each configured hook event (see
// hooks.template.json). Reads the event JSON on stdin, maps it to a running/idle
// state, and pings the resident extension over that window's control socket. No
// injection, no spawning (the extension is already resident). Never throws into
// Cursor: any error exits 0 silently.
//
// UNVERIFIED: hook payload field names (cwd, session id, event name) are coded
// from the Cursor hooks reference, not exercised against a real harness.
"use strict";
const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const lib = require("./lib.js");

function readStdin() {
  return new Promise((resolve) => {
    let data = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (c) => (data += c));
    process.stdin.on("end", () => resolve(data));
    process.stdin.on("error", () => resolve(data));
    setTimeout(() => resolve(data), 2000);
  });
}

async function main() {
  const raw = await readStdin();
  let ev = {};
  try { ev = JSON.parse(raw); } catch { return; }
  // UNVERIFIED field names: accept several shapes defensively.
  const cwd = ev.cwd || ev.workspace_root || ev.workspaceRoot || process.cwd();
  const sessionId = ev.session_id || ev.sessionId || ev.conversation_id || "";
  const eventName = ev.hook_event_name || ev.hookEventName || process.argv[2] || "";
  if (!sessionId) return;
  const state = lib.hookEventToState(eventName);
  if (!state) return;
  const ctl = lib.controlSocketPath(cwd, sessionId, process.env);
  if (!fs.existsSync(ctl)) return;
  await new Promise((resolve) => {
    const conn = net.createConnection(ctl);
    const done = () => { try { conn.destroy(); } catch {} resolve(); };
    conn.setTimeout(1000, done);
    conn.on("connect", () => { try { conn.write(JSON.stringify({ kind: "state", state }) + "\n"); } catch {} done(); });
    conn.on("error", done);
  });
}

main().catch(() => {}); // never throw into Cursor
```

Note: the hook stage is also passed as `argv[2]` in `hooks.template.json` (Task 8) as a fallback when the payload omits the event name.

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js extensions/corral-cursor/state-hook.js
git commit -m "feat(corral-cursor): state-hook shim + event->state mapping"
```

---

## Task 6: The resident extension shell — `extension.js` (sockets, registry, state)

**Files:**
- Create: `extensions/corral-cursor/extension.js`

**Interfaces:**
- Consumes: all of `lib.js` (`acpSocketPath`, `controlSocketPath`, `registryDir`, `buildRecord`, `acpReply`, `acpUpdate`, `resolveWindowPid`, `mergeHooks`).
- Produces: `activate(context)` / `deactivate()` (the VSIX entry points named in `package.json`).
- Injection is a **stub** here (`tryInject(text)` returns `false`); Task 7 fills it. This task's deliverable is discover/watch/focus/state (the "A" landing), independently reviewable.

- [ ] **Step 1: Implement the shell**

```js
// extensions/corral-cursor/extension.js
// The resident corral-cursor owner: runs in Cursor's extension host, binds the
// ACP socket + control socket, writes the gui:true registry record, serves the
// ACP surface to corral, broadcasts state (fed by state-hook.js), and (Task 7)
// opens a new Composer chat for session/prompt. No sidecar: the extension host
// IS the resident runtime. Plain JS: the host loads main as JS, no build step.
//
// UNVERIFIED (no Cursor here): Electron pid resolution, hook payload fields, and
// the Composer inject command (Task 7). Every vscode/Cursor access is guarded so
// the extension never throws into the host.
"use strict";
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
let vscode; try { vscode = require("vscode"); } catch {}
const lib = require("./lib.js");

const MAX_TITLE = 60;

function readProc(pid) {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, "utf8");
    // comm is in parens and may contain spaces/parens: take between first '(' and last ')'.
    const l = stat.indexOf("("), r = stat.lastIndexOf(")");
    const comm = stat.slice(l + 1, r);
    const after = stat.slice(r + 2).split(" "); // state, ppid, ...
    return { comm, ppid: Number(after[1]) };
  } catch { return null; }
}

function activate(context) {
  const guard = (fn) => { try { return fn(); } catch (e) { try { console.error("corral-cursor:", e); } catch {} } };
  guard(() => start(context));
}

let servers = [];
let registryFile;
let state = "idle";
let title = null;
const clients = new Set();
let ctx = { sessionId: "", cwd: "", get title() { return title; }, get state() { return state; } };

function start(context) {
  if (!vscode) return; // not in an editor host
  const folders = vscode.workspace.workspaceFolders;
  const cwd = folders && folders[0] ? folders[0].uri.fsPath : process.cwd();
  // Stable per-workspace identity, persisted so reopening maps to the same record.
  let sessionId = context.workspaceState.get("corralSessionId");
  if (!sessionId) { sessionId = crypto.randomUUID(); context.workspaceState.update("corralSessionId", sessionId); }
  const electronPid = lib.resolveWindowPid(process.pid, readProc);
  const env = process.env;
  const sockDir = lib.socketDir(cwd, env);
  const acpPath = lib.acpSocketPath(cwd, electronPid, env);
  const ctlPath = lib.controlSocketPath(cwd, sessionId, env);
  title = path.basename(cwd);
  ctx.sessionId = sessionId; ctx.cwd = cwd;

  fs.mkdirSync(sockDir, { recursive: true, mode: 0o700 });

  // ACP surface corral connects to.
  servers.push(lineServer(acpPath, (line, sock) => onAcp(line, sock), (sock) => {
    clients.add(sock);
    try { sock.write(JSON.stringify(lib.acpUpdate(sessionId, { sessionUpdate: "state_update", state })) + "\n"); } catch {}
  }, () => clients));
  // Control channel the state-hook pings.
  servers.push(lineServer(ctlPath, (line) => onControl(line)));

  writeRegistry(acpPath);
  // Hook auto-registration is Task 8 (mergeHooksFile).

  context.subscriptions.push({ dispose: () => shutdown(acpPath, ctlPath) });
}

function lineServer(unixPath, onLine, onOpen) {
  try { fs.rmSync(unixPath, { force: true }); } catch {}
  const buffers = new Map();
  const server = net.createServer((conn) => {
    buffers.set(conn, "");
    if (onOpen) onOpen(conn);
    conn.on("data", (chunk) => {
      let buf = (buffers.get(conn) || "") + chunk.toString("utf8");
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (line) onLine(line, conn);
      }
      buffers.set(conn, buf);
    });
    const drop = () => { clients.delete(conn); buffers.delete(conn); };
    conn.on("close", drop);
    conn.on("error", drop);
  });
  server.listen(unixPath);
  return server;
}

function onAcp(line, sock) {
  let msg; try { msg = JSON.parse(line); } catch { return; }
  const reply = lib.acpReply(msg, ctx);
  if (!reply) return;
  if (reply.__inject !== undefined) {
    // session/prompt: attempt to open a new pre-filled Composer chat (Task 7),
    // then answer. fire-and-forget stopReason on success, error on failure.
    tryInject(reply.__inject).then((okDelivered) => {
      const out = okDelivered
        ? { jsonrpc: "2.0", id: reply.id, result: { stopReason: "end_turn" } }
        : { jsonrpc: "2.0", id: reply.id, error: { code: -32011, message: "cursor: could not deliver to Composer" } };
      try { sock.write(JSON.stringify(out) + "\n"); } catch {}
    });
    return;
  }
  try { sock.write(JSON.stringify(reply) + "\n"); } catch {}
}

function onControl(line) {
  let req; try { req = JSON.parse(line); } catch { return; }
  if (req.kind === "state" && (req.state === "running" || req.state === "idle")) setState(req.state);
}

function setState(next) {
  if (state === next) return;
  state = next;
  broadcast({ sessionUpdate: "state_update", state });
  touchRegistry();
}
function broadcast(update) {
  const l = JSON.stringify(lib.acpUpdate(ctx.sessionId, update)) + "\n";
  for (const c of clients) { try { c.write(l); } catch {} }
}

// Task 7 replaces this stub.
async function tryInject(_text) { return false; }

function writeRegistry(acpPath) {
  try {
    const dir = lib.registryDir(process.env);
    if (!dir) return;
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    registryFile = path.join(dir, `${ctx.sessionId}.json`);
    const record = lib.buildRecord({ sessionId: ctx.sessionId, cwd: ctx.cwd, title, socket: acpPath, nowIso: new Date().toISOString() });
    const tmp = `${registryFile}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
    fs.renameSync(tmp, registryFile);
  } catch {}
}
let lastTouch = 0;
function touchRegistry() {
  const now = Date.now();
  if (now - lastTouch < 2000 || !registryFile) return;
  lastTouch = now;
  try {
    const rec = JSON.parse(fs.readFileSync(registryFile, "utf8"));
    rec.lastSeen = new Date().toISOString();
    rec.title = title;
    const tmp = `${registryFile}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(rec, null, 2), { mode: 0o600 });
    fs.renameSync(tmp, registryFile);
  } catch {}
}
function clearSocketInRegistry() {
  if (!registryFile) return;
  try {
    const rec = JSON.parse(fs.readFileSync(registryFile, "utf8"));
    rec.socket = null;
    const tmp = `${registryFile}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(rec, null, 2), { mode: 0o600 });
    fs.renameSync(tmp, registryFile);
  } catch {}
}

function shutdown(acpPath, ctlPath) {
  clearSocketInRegistry();
  for (const s of servers) { try { s.close(); } catch {} }
  servers = [];
  for (const p of [acpPath, ctlPath]) { try { fs.rmSync(p, { force: true }); } catch {} }
}

function deactivate() { /* subscriptions.dispose handles shutdown */ }

module.exports = { activate, deactivate };
```

- [ ] **Step 2: Sanity-check it loads without a vscode host**

Run: `node -e "const e=require('./extensions/corral-cursor/extension.js'); if(typeof e.activate!=='function'||typeof e.deactivate!=='function') throw new Error('bad exports'); console.log('ok: exports present, no vscode host needed to require')"`
Expected: prints `ok: ...` (the `require("vscode")` is guarded, so loading without a host does not throw).

- [ ] **Step 3: Re-run the pure suite to confirm nothing regressed**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add extensions/corral-cursor/extension.js
git commit -m "feat(corral-cursor): resident extension shell (sockets, registry, state, ACP)"
```

---

## Task 7: Composer injection — open a new pre-filled chat

**Files:**
- Modify: `extensions/corral-cursor/extension.js` (replace the `tryInject` stub)

**Interfaces:**
- Consumes: `vscode.commands`, `vscode.workspace.getConfiguration`.
- Produces: `async tryInject(text)` → `Promise<boolean>` — true if a candidate delivery path succeeded.

- [ ] **Step 1: Replace the stub with the candidate-list + config injection**

```js
// replace `async function tryInject(_text) { return false; }` with:

// Open a NEW Composer chat pre-filled with `text`. A prompt must land in a chat
// (no window-level prompt); a fresh chat avoids intruding on the open one and
// mirrors Cursor's prompt-deeplink behavior. UNVERIFIED: the Composer command
// ID(s) are undocumented, so try a config override, then a candidate list, then
// the prompt deeplink. Returns true on the first path that does not throw.
async function tryInject(text) {
  if (!vscode) return false;
  const cfg = (() => { try { return vscode.workspace.getConfiguration("corral.cursor"); } catch { return null; } })();
  const override = cfg && cfg.get ? cfg.get("injectCommand") : null;
  // Each candidate: run a command that opens/focuses Composer and submits text.
  // The exact arg shape is unknown; pass text as a plain string and as {text}.
  const commandCandidates = [override, "composer.newAgentChat", "aichat.newchat", "composer.startComposerPrompt", "workbench.action.chat.open"].filter(Boolean);
  for (const cmd of commandCandidates) {
    const okStr = await runCommand(cmd, text);
    if (okStr) return true;
    const okObj = await runCommand(cmd, { text, query: text });
    if (okObj) return true;
  }
  // Fallback: the prompt deeplink pre-fills a chat for the user to confirm.
  try {
    const uri = vscode.Uri.parse(`cursor://anysphere.cursor-deeplink/prompt?text=${encodeURIComponent(text)}`);
    const ok = await vscode.env.openExternal(uri);
    if (ok) return true;
  } catch {}
  return false;
}

async function runCommand(cmd, arg) {
  try { await vscode.commands.executeCommand(cmd, arg); return true; } catch { return false; }
}
```

- [ ] **Step 2: Sanity-check it still loads without a host**

Run: `node -e "require('./extensions/corral-cursor/extension.js'); console.log('ok: loads')"`
Expected: prints `ok: loads`.

- [ ] **Step 3: Commit**

```bash
git add extensions/corral-cursor/extension.js
git commit -m "feat(corral-cursor): session/prompt opens a new pre-filled Composer chat (UNVERIFIED)"
```

---

## Task 8: VSIX manifest, hook auto-registration, template

**Files:**
- Create: `extensions/corral-cursor/package.json`
- Create: `extensions/corral-cursor/hooks.template.json`
- Modify: `extensions/corral-cursor/extension.js` (call `mergeHooksFile` on activate)

**Interfaces:**
- Consumes: `lib.mergeHooks`, `context.extensionPath`.
- Produces: activation writes/merges `~/.cursor/hooks.json` to run `node <extensionPath>/state-hook.js <stage>`.

- [ ] **Step 1: Write the manifest**

```json
{
  "name": "corral-cursor",
  "displayName": "corral for Cursor",
  "description": "Announce this Cursor window to a corral board (discover, focus, resume, message).",
  "version": "0.1.0",
  "publisher": "corral",
  "engines": { "vscode": "^1.90.0" },
  "main": "./extension.js",
  "activationEvents": ["onStartupFinished"],
  "contributes": {
    "configuration": {
      "title": "corral",
      "properties": {
        "corral.cursor.injectCommand": {
          "type": "string",
          "default": "",
          "description": "Override the Cursor command id corral uses to open a pre-filled Composer chat when delivering a message. Leave empty to try built-in candidates."
        }
      }
    }
  }
}
```

- [ ] **Step 2: Write the hooks template (documentation/reference; the extension writes the real file)**

```json
{
  "version": 1,
  "hooks": {
    "beforeSubmitPrompt": [
      { "hooks": [{ "type": "command", "command": "node", "args": ["<ABSOLUTE>/state-hook.js", "beforeSubmitPrompt"] }] }
    ],
    "stop": [
      { "hooks": [{ "type": "command", "command": "node", "args": ["<ABSOLUTE>/state-hook.js", "stop"] }] }
    ]
  }
}
```

- [ ] **Step 3: Wire auto-merge into activate()**

Add, and call `mergeHooksFile(context)` at the end of `start(context)`:

```js
// Register our state-hook in ~/.cursor/hooks.json additively and idempotently,
// so there is no manual hooks setup. Absolute path from extensionPath; node must
// be on PATH (documented). Stage passed as argv fallback for state-hook.js.
function mergeHooksFile(context) {
  try {
    const home = process.env.HOME;
    if (!home) return;
    const hooksFile = path.join(home, ".cursor", "hooks.json");
    let existing = {};
    try { existing = JSON.parse(fs.readFileSync(hooksFile, "utf8")); } catch {}
    const script = path.join(context.extensionPath, "state-hook.js");
    // Two stages need distinct argv (the stage fallback), so merge twice with the
    // per-stage command shape. mergeHooks keys on args[0]; include the stage arg.
    let merged = lib.mergeHooks(existing, { command: "node", args: [script, "beforeSubmitPrompt"] });
    merged = lib.mergeHooks(merged, { command: "node", args: [script, "stop"] });
    fs.mkdirSync(path.dirname(hooksFile), { recursive: true });
    const tmp = `${hooksFile}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(merged, null, 2));
    fs.renameSync(tmp, hooksFile);
  } catch {}
}
```

Note: `mergeHooks` currently keys idempotency on `args[0]` only. Because both stages share `args[0]` (the script path), update the `mergeHooks` de-dupe check in `lib.js` to key on the **full args array** so both the `beforeSubmitPrompt` and `stop` entries persist. Adjust the test in Task 4 accordingly (assert both entries exist and re-merge does not duplicate either).

- [ ] **Step 4: Update `mergeHooks` de-dupe to full-args, fix its tests, re-run**

Change the `present` check in `lib.js` to compare the whole args array:

```js
const sameArgs = (a, b) => a && b && a.length === b.length && a.every((x, i) => x === b[i]);
// ...
const present = arr.some((g) => (g.hooks || []).some((h) => sameArgs(h.args, hookCommand.args)));
```

Update the Task 4 tests to pass stage-specific args (`["/abs/state-hook.js","stop"]`) and assert idempotency per full-args.

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS.

- [ ] **Step 5: Confirm the extension still loads**

Run: `node -e "require('./extensions/corral-cursor/extension.js'); console.log('ok')"`
Expected: `ok`.

- [ ] **Step 6: Commit**

```bash
git add extensions/corral-cursor/package.json extensions/corral-cursor/hooks.template.json extensions/corral-cursor/lib.js extensions/corral-cursor/lib.test.js extensions/corral-cursor/extension.js
git commit -m "feat(corral-cursor): VSIX manifest + auto-merged hooks registration"
```

---

## Task 9: README and repo docs

**Files:**
- Create: `extensions/corral-cursor/README.md`
- Modify: `AGENTS.md` (Extensions section — add a `corral-cursor` entry)
- Modify: `README.md` (adapters list — add Cursor)

**Interfaces:** none (docs).

- [ ] **Step 1: Write `extensions/corral-cursor/README.md`**

Cover: what it is (GUI editor adapter, first VSIX), architecture (extension host resident owner + state-hook, no sidecar), install (`cursor --install-extension` from a packaged `.vsix`, or copy the folder into `~/.cursor/extensions/`, or `--extensionDevelopmentPath`), the **node-on-PATH** requirement for the hook, the `corral.cursor.injectCommand` setting, and the Known Limitations (multi-window shared pid focus, coarse state, UNVERIFIED injection, dormant drops text). Mirror the tone/sections of `extensions/corral-claude/README.md`, ending with a `## Status: UNVERIFIED` section.

- [ ] **Step 2: Add the `corral-cursor` entry to `AGENTS.md`**

In the `## Extensions` section, after the `corral-claude/` paragraph, add a paragraph describing `corral-cursor/`: the first GUI editor adapter and first VSIX; a resident extension (not a sidecar, since the extension host is the resident runtime) that binds `<cwd>/.corral/cursor-<electronPid>.sock` (Electron pid so `gui` focus works), writes a `gui: true` record (`label: "cursor"`, spawn/resume `["cursor", <cwd>]`), serves the ACP surface, opens a new pre-filled Composer chat for `session/prompt` (UNVERIFIED), and feeds `running`/`idle` from an auto-merged `~/.cursor/hooks.json` state-hook. One card per window; UNVERIFIED (no Cursor here).

- [ ] **Step 3: Add Cursor to the adapters list in `README.md`**

Find where `corral-pi` / `corral-opencode` / `corral-claude` are listed and add a Cursor bullet in the same style (GUI editor, VSIX, best-effort messaging).

- [ ] **Step 4: Final full test run**

Run: `node --test extensions/corral-cursor/lib.test.js`
Expected: PASS (all pure helpers).

- [ ] **Step 5: Commit**

```bash
git add extensions/corral-cursor/README.md AGENTS.md README.md
git commit -m "docs(corral-cursor): adapter README + AGENTS/README entries"
```

---

## Self-Review

**Spec coverage:**
- Extension resident owner (socket, registry, ACP, injection, state) → Tasks 1,2,6,7,8. ✓
- `gui: true` record, `label: "cursor"`, spawn/resume `cursor <dir>`, no messageFlag → Task 1. ✓
- Socket named with Electron pid for focus → Tasks 3,6. ✓
- ACP surface (initialize/list/prompt/cancel, -32601) → Task 2,6. ✓
- Message opens a new pre-filled chat (UNVERIFIED, candidate list + config + deeplink) → Task 7. ✓
- State via hooks (running/idle), auto-merged hooks.json → Tasks 4,5,8. ✓
- No corral core changes → Global Constraints; nothing in the plan touches `crates/`. ✓
- UNVERIFIED / defensive coding, node-on-PATH → Global Constraints, Tasks 5,6,7, README (Task 9). ✓
- Known limitations documented → Task 9 README. ✓
- Multi-session / messageUriTemplate / title-focus explicitly deferred → not built (spec Future). ✓

**Placeholder scan:** hooks.template.json uses `<ABSOLUTE>` as an intentional documentation placeholder (the extension writes the real path in Task 8); every executable step has concrete code. No TODO/TBD in code.

**Type/name consistency:** `tryInject` (Task 6 stub → Task 7 real) same signature `async (text) => Promise<boolean>`. `mergeHooks` de-dupe key changes from `args[0]` (Task 4) to full-args (Task 8) — Task 8 explicitly updates both the code and the Task 4 tests, called out to avoid a stale-test bug. `acpReply`/`acpUpdate`/`resolveWindowPid`/`buildRecord`/`controlSocketPath`/`acpSocketPath`/`hookEventToState` names are consistent across tasks and the exports line.

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute in this session with checkpoints.
