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
  try { ev = JSON.parse(raw); } catch { ev = {}; }
  // Stage: prefer the arg we pass in hooks.json (reliable), fall back to payload.
  const eventName = process.argv[2] || ev.hook_event_name || ev.hookEventName || "";
  const state = lib.hookEventToState(eventName);
  if (!state) return;
  // beforeSubmitPrompt carries the model; forward it so the extension can
  // broadcast + persist it (undefined on other stages, dropped below).
  const model = lib.modelFromPayload(ev);
  // cwd: Cursor's payload carries `workspace_roots` (array), not `cwd`; hooks also
  // run from the project root, so process.cwd() is a final fallback.
  const cwd =
    (Array.isArray(ev.workspace_roots) && ev.workspace_roots[0]) ||
    ev.cwd || ev.workspace_root || process.cwd();
  const dir = process.env.CORRAL_SOCKET_DIR || path.join(cwd, ".corral");
  // Find the extension's control socket by name (it is keyed on OUR session id,
  // which the hook payload does not contain), not by reconstructing the path.
  let name;
  try { name = fs.readdirSync(dir).find(lib.isControlSocketFile); } catch { return; }
  if (!name) return;
  const ctl = path.join(dir, name);
  await new Promise((resolve) => {
    const conn = net.createConnection(ctl);
    const done = () => { try { conn.destroy(); } catch {} resolve(); };
    conn.setTimeout(1000, done);
    // Write, then close only AFTER the write flushes (the callback). Destroying
    // immediately after write() races the flush and drops the message.
    conn.on("connect", () => {
      try { conn.write(JSON.stringify({ kind: "state", state, model }) + "\n", () => conn.end()); }
      catch { done(); }
    });
    conn.on("close", done);
    conn.on("error", done);
  });
}

main().catch(() => {}); // never throw into Cursor
