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

module.exports = { registryDir, socketDir, acpSocketPath, controlSocketPath, buildRecord, acpReply, acpUpdate, resolveWindowPid };
