// The resident corral-cursor owner: runs in Cursor's extension host, binds the
// ACP socket + control socket, writes the gui:true registry record, serves the
// ACP surface to corral, broadcasts state (fed by state-hook.js), and opens a new
// Composer chat for session/prompt. No sidecar: the extension host IS the
// resident runtime. Plain JS: the host loads main as JS, no build step.
//
// UNVERIFIED (no Cursor here): Electron pid resolution, hook payload fields, and
// the Composer inject command. Every vscode/Cursor access is guarded so the
// extension never throws into the host.
"use strict";
const fs = require("node:fs");
const net = require("node:net");
const path = require("node:path");
const crypto = require("node:crypto");
let vscode; try { vscode = require("vscode"); } catch {}
const lib = require("./lib.js");

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

let servers = [];
let registryFile;
let state = "idle";
let title = null;
const clients = new Set();
const ctx = { sessionId: "", cwd: "", get title() { return title; }, get state() { return state; } };

function activate(context) {
  try { start(context); } catch (e) { try { console.error("corral-cursor:", e); } catch {} }
}

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
  }));
  // Control channel the state-hook pings.
  servers.push(lineServer(ctlPath, (line) => onControl(line)));

  writeRegistry(acpPath);
  mergeHooksFile(context);

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
    // session/prompt: attempt to open a new pre-filled Composer chat, then answer.
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

// Stub; Task 7 replaces this with the real injection.
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

// Placeholder; Task 8 implements the ~/.cursor/hooks.json auto-merge.
function mergeHooksFile(_context) {}

function shutdown(acpPath, ctlPath) {
  clearSocketInRegistry();
  for (const s of servers) { try { s.close(); } catch {} }
  servers = [];
  for (const p of [acpPath, ctlPath]) { try { fs.rmSync(p, { force: true }); } catch {} }
}

function deactivate() { /* subscriptions.dispose handles shutdown */ }

module.exports = { activate, deactivate };
