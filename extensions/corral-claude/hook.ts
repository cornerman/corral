#!/usr/bin/env node
/**
 * corral-claude hook shim: the thin bridge Claude Code runs for each configured
 * hook event (see settings.json). It reads the event JSON on stdin, talks to the
 * resident sidecar (sidecar.ts) over that session's control socket, and turns
 * the sidecar's reply into the hook output Claude acts on.
 *
 * Modes (selected by the first CLI arg in settings.json):
 *   (default)   forward one hook event. On SessionStart, spawn the sidecar
 *               detached if it is not already up. For Stop, if the sidecar hands
 *               back queued messages, print {decision:"block",reason,systemMessage}
 *               so Claude continues with them as its next instruction AND the
 *               text shows in the transcript — this is how a corral message
 *               reaches the LIVE session at a turn boundary, visibly.
 *   await       the asyncRewake doorbell (async:true, asyncRewake:true, on Stop):
 *               long-poll the sidecar; when a message is queued on an idle
 *               session it returns a neutral wake note, which we print to stderr
 *               and exit 2 so Claude wakes and its next Stop delivers the message
 *               visibly. The note is never the message text itself.
 *
 * Never throws into Claude and never blocks a tool: any error or missing sidecar
 * exits 0 silently. Only the two intentional paths (Stop block, await wake)
 * produce output.
 *
 * UNVERIFIED against a real Claude Code harness; see sidecar.ts header.
 */

import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";

const MODE = process.argv[2] === "await" ? "await" : "event";
const HERE = path.dirname(new URL(import.meta.url).pathname);

async function main() {
	const raw = await readStdin();
	let ev: Record<string, unknown> = {};
	try {
		ev = JSON.parse(raw);
	} catch {
		return; // no parseable event: nothing to do
	}
	const cwd = typeof ev.cwd === "string" ? ev.cwd : process.cwd();
	const sessionId = typeof ev.session_id === "string" ? ev.session_id : "";
	if (!sessionId) return;

	const socketDir = process.env.CORRAL_SOCKET_DIR ?? path.join(cwd, ".corral");
	const controlSocket = path.join(socketDir, `.claude-ctl-${sessionId}.sock`);

	if (MODE === "await") {
		// The doorbell is armed at SessionStart too (so a message reaches a session
		// that has not taken its first turn), where the sidecar is still being
		// spawned by the concurrent event hook. Wait for its socket before polling;
		// on Stop it is already up so this returns at once. Do not spawn here — the
		// event hook owns spawning, avoiding a double-bind race.
		await waitUp(controlSocket, 5000);
		const note = await requestAwait(controlSocket);
		if (note) {
			// asyncRewake doorbell: exit 2 wakes an idle session so its Stop hook fires
			// and delivers the queued message visibly (systemMessage). We only wake
			// here; `note` is a neutral reminder (stderr shown to Claude), never the
			// message text itself.
			process.stderr.write(note);
			process.exit(2);
		}
		return; // nothing queued within the hold window; re-armed next turn
	}

	// Normal event. Ensure the sidecar exists on SessionStart, then forward.
	const name = String(ev.hook_event_name ?? "");
	if (name === "SessionStart" && !(await isUp(controlSocket))) {
		spawnSidecar(cwd, sessionId);
		await waitUp(controlSocket, 3000);
	}
	const inject = await requestEvent(controlSocket, ev);
	if (name === "Stop" && inject) {
		// Deliver queued corral messages as the next instruction (reason -> the
		// model). reason is fed to Claude only and renders as an opaque "Stop hook
		// feedback" line, so also emit systemMessage: the one hook field shown to the
		// user, making the incoming message text visible in the transcript.
		process.stdout.write(JSON.stringify({ decision: "block", reason: inject, systemMessage: inject }));
	}
}

function spawnSidecar(cwd: string, sessionId: string) {
	try {
		// Detached so it outlives this hook and is NOT an ancestor of the terminal
		// window (corral's focus walk must not climb into it). PPID here is the
		// interactive Claude process; pass it so the ACP socket filename carries the
		// pid corral correlates the window by.
		const child = spawn("node", [path.join(HERE, "sidecar.ts")], {
			cwd,
			detached: true,
			stdio: "ignore",
			env: {
				...process.env,
				CORRAL_CLAUDE_CWD: cwd,
				CORRAL_CLAUDE_SESSION_ID: sessionId,
				CORRAL_CLAUDE_PID: String(process.ppid),
			},
		});
		child.unref();
	} catch {}
}

// One-shot control request: send a line, read one reply line, return inject.
function talk(socketPath: string, payload: unknown, timeoutMs: number): Promise<string | null> {
	return new Promise((resolve) => {
		const conn = net.createConnection(socketPath);
		let buf = "";
		let done = false;
		const finish = (v: string | null) => {
			if (done) return;
			done = true;
			try {
				conn.destroy();
			} catch {}
			resolve(v);
		};
		conn.setTimeout(timeoutMs, () => finish(null));
		conn.on("connect", () => conn.write(`${JSON.stringify(payload)}\n`));
		conn.on("data", (chunk) => {
			buf += chunk.toString("utf8");
			const nl = buf.indexOf("\n");
			if (nl < 0) return;
			try {
				finish((JSON.parse(buf.slice(0, nl)) as { inject?: string | null }).inject ?? null);
			} catch {
				finish(null);
			}
		});
		conn.on("error", () => finish(null));
		conn.on("close", () => finish(null));
	});
}

const requestEvent = (s: string, ev: unknown) => talk(s, { kind: "event", event: ev }, 10_000);
// Long hold: the sidecar rings this doorbell early when a message is queued on
// an idle session, or lets its own hold elapse; keep the socket timeout above
// that so we do not cut it short. The returned value is a neutral wake note.
const requestAwait = (s: string) => talk(s, { kind: "await" }, 360_000);

function isUp(socketPath: string): Promise<boolean> {
	return new Promise((resolve) => {
		if (!fs.existsSync(socketPath)) return resolve(false);
		const conn = net.createConnection(socketPath);
		const done = (v: boolean) => {
			try {
				conn.destroy();
			} catch {}
			resolve(v);
		};
		conn.setTimeout(500, () => done(false));
		conn.on("connect", () => done(true));
		conn.on("error", () => done(false));
	});
}
async function waitUp(socketPath: string, ms: number) {
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		if (await isUp(socketPath)) return;
		await new Promise((r) => setTimeout(r, 100));
	}
}

function readStdin(): Promise<string> {
	return new Promise((resolve) => {
		let data = "";
		process.stdin.setEncoding("utf8");
		process.stdin.on("data", (c) => (data += c));
		process.stdin.on("end", () => resolve(data));
		process.stdin.on("error", () => resolve(data));
		// Some hook events may deliver no stdin; do not hang.
		setTimeout(() => resolve(data), 2000);
	});
}

main().catch(() => {}); // never throw into Claude
