/**
 * corral-claude sidecar: make an interactive Claude Code session discoverable
 * and messageable by ACP clients (corral) while its terminal TUI keeps running.
 *
 * Claude Code has no in-process plugin runtime that can hold a socket or inject
 * a prompt into the live session (its hooks are subprocesses that exit, and its
 * ACP mode is a separate headless stdio server for an IDE). So unlike the pi and
 * opencode adapters — which run *inside* the session and serve ACP directly —
 * this adapter is a resident sidecar the session's SessionStart hook spawns
 * (detached), driven by Claude's hook events arriving over a control socket.
 * See ../corral-pi.ts and ../corral-opencode.ts for the in-process shape;
 * the served ACP surface and registry record are identical (CONVENTION.md).
 *
 * Two unix sockets, both under <cwd>/.corral/ (override $CORRAL_SOCKET_DIR):
 *   claude-<claudePid>.sock         ACP surface corral connects to. The pid is
 *                                   the interactive Claude process (the hook's
 *                                   PPID at SessionStart) so corral's focus
 *                                   parent-walk finds the terminal window.
 *   .claude-ctl-<sessionId>.sock    control channel the hook shim (hook.ts)
 *                                   connects to once per hook event.
 *
 * Registry record at $HOME/.corral/registry/<sessionId>.json (override
 * $CORRAL_REGISTRY_DIR) points at the ACP socket; cleared to socket:null on
 * SessionEnd, leaving a dormant, resumable entry.
 *
 * Served ACP surface (see handleAcp):
 *   initialize      identity (agentInfo name "claude")
 *   session/list    this session: id, title, cwd
 *   session/prompt  queue a user message; always delivered into the LIVE session
 *                   at the next turn boundary by the synchronous Stop hook
 *                   (decision:block reason -> the model, plus systemMessage so
 *                   the text is VISIBLE in the transcript instead of an opaque
 *                   "Stop hook feedback" line). If the session is idle there is no
 *                   upcoming Stop, so the asyncRewake hook is rung as a doorbell
 *                   to wake it; the woken turn ends and its Stop delivers the
 *                   queued message. The request resolves with stopReason once the
 *                   Stop hook takes it (see flushTo). This deferred delivery
 *                   matches the fire-and-forget contract and pi's "queue while
 *                   busy".
 *   session/cancel  no-op: Claude exposes no external turn-abort. Documented
 *                   limitation, answered as a notification.
 * Broadcasts (session/update): state_update (running/idle/requires_action),
 *   session_info_update (title), tool_call/tool_call_update, agent_message_chunk.
 *
 * UNVERIFIED: no Claude Code binary or hook harness is available in this repo's
 * sandbox, so the hook payload field names and the injection semantics
 * (Stop decision:block "reason" as next instruction; asyncRewake exit-2 wake)
 * are coded from the Claude Code hooks reference, not exercised. Every field
 * access is guarded and all work is wrapped so the sidecar never crashes.
 *
 * Run: spawned by hook.ts as `node sidecar.ts` with cwd/sessionId/claudePid in
 * env (CORRAL_CLAUDE_*). Not launched by hand.
 */

import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";

// Longest title corral should receive.
const MAX_TITLE = 60;
// How long an asyncRewake "await" is held before responding empty, so the hook
// re-arms on the next turn rather than blocking forever. Comfortably under the
// hook's own timeout.
const AWAIT_HOLD_MS = 300_000;

// --- session identity, passed by the spawning SessionStart hook ---
const cwd = process.env.CORRAL_CLAUDE_CWD ?? process.cwd();
const sessionId = process.env.CORRAL_CLAUDE_SESSION_ID ?? "";
// The interactive Claude process pid (hook's PPID). Used only in the ACP socket
// filename so corral can correlate the window; the sidecar itself is detached.
const claudePid = process.env.CORRAL_CLAUDE_PID ?? String(process.pid);

if (!sessionId) {
	// Nothing to announce without a session id; fail loud on stderr and exit.
	console.error("corral-claude sidecar: CORRAL_CLAUDE_SESSION_ID missing");
	process.exit(1);
}

const socketDir = process.env.CORRAL_SOCKET_DIR ?? path.join(cwd, ".corral");
const acpSocketPath = path.join(socketDir, `claude-${claudePid}.sock`);
const controlSocketPath = path.join(socketDir, `.claude-ctl-${sessionId}.sock`);

// --- mutable session state ---
let title: string | null = null;
let lastTitle: string | null | undefined;
let currentState: "running" | "idle" | "requires_action" = "idle";
let registryFile: string | undefined;

// Unix-socket servers (Node net). A connection is our Sock surface.
type Sock = { write: (s: string) => void; end?: () => void };
const acpClients = new Set<Sock>();
// Per-connection read buffer, shared by both socket servers (keyed by the
// distinct socket object) so a partial line survives across data events.
const readBuffers = new Map<unknown, string>();

// Messages queued by session/prompt, each with the ACP request to resolve once
// the message is handed to a hook (delivered into the live session).
type Pending = { text: string; resolve: () => void };
const outbox: Pending[] = [];
// A held asyncRewake connection: a doorbell that wakes an idle session so its
// Stop hook fires. It never carries the message text (delivery is the Stop
// hook's job); respond(WAKE_NOTE) wakes, respond(null) cancels without waking.
let heldAwait: { respond: (text: string | null) => void; timer: ReturnType<typeof setTimeout> } | undefined;
// System reminder shown to Claude on an asyncRewake wake. Neutral on purpose: the
// real message arrives as the immediately following Stop hook's instruction, so
// Claude should not answer this note itself.
const WAKE_NOTE = "A message from corral is about to arrive as your next instruction. Do not respond to this note; wait for the message.";

// --- socket line framing shared by both servers ---
// Node net (not Bun): bun's JavaScriptCore SIGTRAP-crashes under a Landlock
// sandbox (as Claude runs in), so the whole adapter runs on node.
function lineServer(unixPath: string, onLine: (line: string, sock: Sock) => void, onOpen?: (sock: Sock) => void) {
	fs.rmSync(unixPath, { force: true }); // stale leftover from a crashed pid reuse
	const server = net.createServer((conn: net.Socket) => {
		readBuffers.set(conn, "");
		onOpen?.(conn);
		conn.on("data", (chunk: Buffer) => {
			let buf = (readBuffers.get(conn) ?? "") + Buffer.from(chunk).toString("utf8");
			let nl: number;
			while ((nl = buf.indexOf("\n")) >= 0) {
				const line = buf.slice(0, nl).trim();
				buf = buf.slice(nl + 1);
				if (line) onLine(line, conn);
			}
			readBuffers.set(conn, buf);
		});
		const drop = () => {
			acpClients.delete(conn);
			readBuffers.delete(conn);
		};
		conn.on("close", drop);
		conn.on("error", drop);
	});
	server.listen(unixPath);
	return { stop: () => server.close() };
}

// --- ACP surface (corral connects here) ---
function acpUpdateLine(update: Record<string, unknown>): string {
	return JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: { sessionId, update } }) + "\n";
}
function broadcast(update: Record<string, unknown>) {
	if (acpClients.size === 0) return;
	const line = acpUpdateLine(update);
	for (const c of acpClients) {
		try {
			c.write(line);
		} catch {}
	}
}
function setState(next: "running" | "idle" | "requires_action") {
	if (currentState === next) return;
	currentState = next;
	broadcast({ sessionUpdate: "state_update", state: currentState });
}
function setTitleIfChanged(next: string | null) {
	title = next;
	if (title === lastTitle) return;
	lastTitle = title;
	broadcast({ sessionUpdate: "session_info_update", title });
	writeRegistry();
}

function handleAcp(line: string, conn: Sock) {
	let msg: { id?: number | string; method?: string; params?: { prompt?: Array<{ type?: string; text?: string }> } };
	try {
		msg = JSON.parse(line);
	} catch {
		return;
	}
	if (!msg.method) return;
	if (msg.method === "session/cancel") return; // no external abort; documented no-op
	if (msg.id === undefined) return;
	const reply = (result: unknown) => conn.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }) + "\n");
	const fail = (code: number, message: string) =>
		conn.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, error: { code, message } }) + "\n");

	switch (msg.method) {
		case "initialize":
			reply({
				protocolVersion: 1,
				agentCapabilities: { loadSession: false },
				agentInfo: { name: "claude", version: "unknown" },
				authMethods: [],
			});
			break;
		case "session/list":
			reply({ sessions: [{ sessionId, title, cwd }] });
			break;
		case "session/prompt": {
			const text = (msg.params?.prompt ?? [])
				.filter((b) => b.type === "text" && typeof b.text === "string")
				.map((b) => b.text)
				.join("\n");
			if (!text) return fail(-32602, "prompt has no text content");
			// Queue for delivery by the next Stop hook (flushTo), which shows the text
			// via systemMessage. Resolve the ACP request when the Stop hook takes it,
			// not now: corral's send is fire-and-forget, so a deferred stopReason is fine.
			outbox.push({ text, resolve: () => reply({ stopReason: "end_turn" }) });
			// Idle: no upcoming Stop, so ring the parked doorbell to wake the session;
			// the woken turn's Stop then delivers this message visibly. Running: leave it
			// queued for the current turn's Stop (never wake mid-turn).
			if (currentState === "idle" && heldAwait) heldAwait.respond(WAKE_NOTE);
			break;
		}
		default:
			fail(-32601, `method not supported by corral-claude: ${msg.method}`);
	}
}

// Hand all queued messages to the Stop-hook reply, resolving each ACP request.
// Returns the joined text, or null if the outbox was empty. Only the Stop hook
// consumes (the asyncRewake doorbell never drains), so delivery always carries a
// systemMessage and shows in the transcript.
function flushTo(consume: (text: string | null) => void): void {
	if (outbox.length === 0) return consume(null);
	const batch = outbox.splice(0, outbox.length);
	const text = batch.map((m) => m.text).join("\n\n");
	consume(text);
	for (const m of batch) {
		try {
			m.resolve();
		} catch {}
	}
}

// --- control channel (hook.ts connects here, one line per hook event) ---
// Request:  { kind: "event", event: <hook payload> }  |  { kind: "await", timeoutMs?: n }
// Response: { inject: string|null }  (for "event" only Stop uses inject, the
//           delivered message; for "await" inject is a neutral wake note, never
//           the message text — the doorbell only wakes, the Stop hook delivers)
function handleControl(line: string, conn: Sock) {
	let req: { kind?: string; event?: Record<string, unknown>; timeoutMs?: number };
	try {
		req = JSON.parse(line);
	} catch {
		return;
	}
	const respond = (inject: string | null) => {
		try {
			conn.write(JSON.stringify({ inject }) + "\n");
			conn.end?.();
		} catch {}
	};

	if (req.kind === "await") {
		// asyncRewake doorbell. If a message is already queued on an idle session,
		// wake at once; otherwise park until a message rings it or the hold elapses
		// (then the hook re-arms). Never drains the outbox: the Stop hook delivers.
		if (currentState === "idle" && outbox.length > 0) return respond(WAKE_NOTE);
		if (heldAwait) heldAwait.respond(null); // only one parked waiter
		const timer = setTimeout(() => {
			heldAwait = undefined;
			respond(null);
		}, Math.min(req.timeoutMs ?? AWAIT_HOLD_MS, AWAIT_HOLD_MS));
		heldAwait = {
			respond: (t) => {
				clearTimeout(timer);
				heldAwait = undefined;
				respond(t);
			},
			timer,
		};
		return;
	}

	// A normal hook event: update triage state / activity, and for Stop drain the
	// outbox into the reply so the hook injects it via decision:block.
	const ev = req.event ?? {};
	const name = String(ev.hook_event_name ?? "");
	touchRegistry();
	switch (name) {
		case "SessionStart":
			applyTitle(ev.session_title);
			setState("idle");
			return respond(null);
		case "UserPromptSubmit": {
			// Fallback title from the first real user prompt. Skip corral's own
			// injected deliveries: Claude wraps a Stop-hook block message in a
			// <task-notification> envelope and re-fires UserPromptSubmit with it, so
			// an unguarded fallback would title the session with our own re-wake text.
			const p = typeof ev.prompt === "string" ? ev.prompt : "";
			if (!title && !p.trimStart().startsWith("<task-notification>")) applyTitle(ev.prompt);
			setState("running");
			return respond(null);
		}
		case "PreToolUse":
			setState("running");
			broadcast({
				sessionUpdate: "tool_call",
				toolCallId: String(ev.tool_use_id ?? "tool"),
				title: String(ev.tool_name ?? "tool"),
				status: "in_progress",
				rawInput: ev.tool_input,
			});
			return respond(null);
		case "PostToolUse":
			broadcast({
				sessionUpdate: "tool_call_update",
				toolCallId: String(ev.tool_use_id ?? "tool"),
				status: "completed",
			});
			return respond(null);
		case "Notification": {
			const t = String(ev.notification_type ?? "");
			if (t === "permission_prompt") setState("requires_action");
			else if (t === "idle_prompt") setState("idle");
			return respond(null);
		}
		case "Stop": {
			// End of turn. Emit the final assistant text as an activity chunk, then
			// drain any queued messages as the block reason so Claude continues with
			// them as its next instruction (delivery into the live session).
			const last = ev.last_assistant_message;
			if (typeof last === "string" && last.trim()) {
				broadcast({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: last } });
			}
			return flushTo((text) => {
				if (text) setState("running");
				else setState("idle");
				respond(text);
			});
		}
		case "SessionEnd":
			respond(null);
			return shutdown();
		default:
			return respond(null);
	}
}

function applyTitle(raw: unknown) {
	if (typeof raw !== "string") return;
	const clean = raw.replace(/\s+/g, " ").trim();
	if (!clean) return;
	setTitleIfChanged(clean.length > MAX_TITLE ? `${clean.slice(0, MAX_TITLE - 1)}…` : clean);
}

// --- registry store (identical record shape to the other adapters) ---
function registryDir(): string | undefined {
	if (process.env.CORRAL_REGISTRY_DIR) return process.env.CORRAL_REGISTRY_DIR;
	const home = process.env.HOME;
	return home ? path.join(home, ".corral", "registry") : undefined;
}
function writeRegistry() {
	try {
		const dir = registryDir();
		if (!dir) return;
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		registryFile = path.join(dir, `${sessionId}.json`);
		const record = {
			sessionId,
			cwd,
			title,
			label: "claude",
			socket: acpSocketPath,
			// Run verbatim by corral. resumeCommand resumes this exact session; a
			// trailing message (CONVENTION §2a) is appended by corral for
			// launch-with-delivery. UNVERIFIED that `claude --resume <id> "msg"`
			// accepts the trailing prompt in interactive mode.
			spawnCommand: ["claude"],
			resumeCommand: ["claude", "--resume", sessionId],
			// A hidden spawn runs inside a headless cage; corral sets
			// CORRAL_HIDDEN=1 there. Record it so the board reveals by resume.
			hidden: process.env.CORRAL_HIDDEN === "1",
			lastSeen: new Date().toISOString(),
		};
		const tmp = `${registryFile}.${process.pid}.tmp`;
		fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
		fs.renameSync(tmp, registryFile);
	} catch {}
}
// Refresh lastSeen at each hook event without a full rewrite race.
let lastTouch = 0;
function touchRegistry() {
	const now = Date.now();
	if (now - lastTouch < 2000) return; // coalesce bursts
	lastTouch = now;
	writeRegistry();
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

// --- lifecycle ---
let acpServer: { stop: () => void } | undefined;
let controlServer: { stop: () => void } | undefined;

function shutdown() {
	try {
		for (const c of acpClients) c.end?.();
	} catch {}
	acpClients.clear();
	clearSocketInRegistry();
	try {
		acpServer?.stop();
	} catch {}
	try {
		controlServer?.stop();
	} catch {}
	for (const p of [acpSocketPath, controlSocketPath]) {
		try {
			fs.rmSync(p, { force: true });
		} catch {}
	}
	process.exit(0);
}

try {
	fs.mkdirSync(socketDir, { recursive: true, mode: 0o700 });
	acpServer = lineServer(acpSocketPath, handleAcp, (sock) => {
		acpClients.add(sock);
		// Seed the new client with current state so corral columns us at once.
		try {
			sock.write(acpUpdateLine({ sessionUpdate: "state_update", state: currentState }));
		} catch {}
	});
	controlServer = lineServer(controlSocketPath, handleControl);
	writeRegistry();
} catch (e) {
	console.error("corral-claude sidecar: failed to bind sockets:", e);
	process.exit(1);
}

// A crashed Claude leaves no SessionEnd; self-exit when the interactive process
// disappears so we do not leak a socket-holder. corral's dead-socket sweep is
// the final backstop, but this keeps the record honest promptly.
setInterval(() => {
	try {
		process.kill(Number(claudePid), 0); // liveness probe, does not signal
	} catch {
		shutdown();
	}
}, 5000);

for (const sig of ["SIGINT", "SIGTERM"] as const) process.once(sig, shutdown);
