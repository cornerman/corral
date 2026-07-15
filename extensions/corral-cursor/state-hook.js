#!/usr/bin/env node
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
  // Prefer the payload's event name; fall back to the stage passed as argv[2]
  // (hooks.template.json passes the stage) when the payload omits it.
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
