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
