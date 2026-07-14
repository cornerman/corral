/**
 * corral-opencode: make this opencode session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running. The pi counterpart is
 * extensions/corral-announce.ts; this file mirrors it closely and deviates only
 * where opencode's plugin API forces it. It is the second worked adapter that
 * proves the corral convention (CONVENTION.md) is harness-neutral: corral needs
 * zero changes, since it reads the launch commands and label straight from the
 * registry record.
 *
 * Binds an ACP socket inside this session's own workdir at
 * <cwd>/.corral/opencode-<pid>.sock (override the dir with $CORRAL_SOCKET_DIR)
 * and writes a registry record at $HOME/.corral/registry/<sessionId>.json
 * (override with $CORRAL_REGISTRY_DIR) pointing at that socket. On clean process
 * exit the socket is unlinked and the record's `socket` is cleared to null,
 * leaving a dormant, resumable record. Served surface:
 *   initialize            identity (agentInfo name "opencode")
 *   session/list          this session: id, title, cwd
 *   session/prompt        inject a user message (opencode queues while busy);
 *                         responds on the next session.idle
 *   session/cancel        abort the current turn (notification)
 * Broadcast to every connected client as session/update notifications:
 *   agent_message_chunk / user_message_chunk (best-effort text from message
 *                         part events; see the UNVERIFIED note below)
 *   tool_call / tool_call_update
 *   session_info_update   session renames (title refreshed at turn boundaries)
 *   state_update          running/idle/requires_action (ACP v2 vocabulary):
 *                         running on the first turn signal, idle on
 *                         session.idle, requires_action while a permission
 *                         prompt is open (permission.updated), cleared back to
 *                         running on permission.replied
 *
 * Registers one tool, corral_message_agent, that submits a cross-session
 * message over corral's control socket $HOME/.corral/corrald.sock (override
 * with $CORRAL_CONTROL_SOCKET) for corral to route; the agent never reaches
 * another session directly. Mirrors pi's tool exactly.
 *
 * UNVERIFIED: no @opencode-ai/plugin types or opencode binary are available in
 * the build sandbox, so the plugin API shapes (client method args, event
 * payload fields, the `tool` registration helper) are coded from opencode's
 * docs and probed defensively at runtime, not typechecked. Every field access
 * on an event payload is guarded, and all bridge work is wrapped so the plugin
 * never throws into opencode.
 *
 * Install: symlink into ~/.config/opencode/plugin/ (global) or
 * .opencode/plugin/ (project).
 *
 * Multiple concurrent clients are fine: every request is answered from current
 * state and updates go to all connections.
 */

import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import { type Plugin, tool } from "@opencode-ai/plugin";

// Longest title corral should receive; an auto-generated title can be long.
const MAX_TITLE = 60;

export const CorralOpencode: Plugin = async ({ client, directory }) => {
	// A Bun.listen server (unix socket) plus the registry file it announces.
	let server: { stop: () => void } | undefined;
	let socketPath: string | undefined;
	let registryFile: string | undefined;
	// The single active opencode session this window drives. Multi-session
	// multiplexing is deferred (YAGNI): we track only the latest active id.
	let activeSessionId: string | undefined;
	// The session cwd is the plugin's project directory; the socket lives under
	// it so only this session (and unsandboxed tools like corral) can reach it.
	const activeCwd = directory;
	let activeTitle: string | null = null;
	// Last title pushed to clients, so we broadcast session_info_update only when
	// it actually changes. `undefined` forces the first comparison.
	let lastTitle: string | null | undefined;
	// Triage state in ACP v2 vocabulary (running/idle/requires_action), broadcast
	// as the standard state_update session/update so corral can column the agent
	// without polling.
	let currentState: "running" | "idle" | "requires_action" = "idle";
	// Bun sockets are the client connections; a per-connection read buffer lives
	// beside each so a partial line survives across data events.
	const clients = new Set<{ write: (s: string) => void; end?: () => void }>();
	const buffers = new Map<unknown, string>();
	// session/prompt requests waiting for the turn that consumes them to end.
	const pendingPrompts: Array<{ conn: { write: (s: string) => void }; id: number | string }> = [];

	// Bind the socket and announce as soon as the plugin loads: the socket path
	// needs only cwd + pid, not the session id, so it can serve immediately. The
	// registry record is written lazily once the first session event reveals the
	// active session id (see the event hook). Everything is best-effort and
	// guarded; announcing must never crash opencode.
	try {
		const socketDir = process.env.CORRAL_SOCKET_DIR ?? path.join(activeCwd, ".corral");
		// 0700: the socket grants prompt access to this session; directory
		// permissions are the only peer authentication we rely on.
		fs.mkdirSync(socketDir, { recursive: true, mode: 0o700 });
		socketPath = path.join(socketDir, `opencode-${process.pid}.sock`);
		fs.rmSync(socketPath, { force: true }); // stale leftover from a crashed pid reuse

		server = (globalThis as { Bun?: { listen: (opts: unknown) => { stop: () => void } } }).Bun!.listen({
			unix: socketPath,
			socket: {
				open(sock: { write: (s: string) => void }) {
					clients.add(sock);
					buffers.set(sock, "");
					// Seed the new client with the current state so it can column us
					// at once (only once a session exists to name in the envelope).
					if (activeSessionId) {
						try {
							sock.write(sessionUpdateLine({ sessionUpdate: "state_update", state: currentState }));
						} catch {}
					}
				},
				data(sock: { write: (s: string) => void }, chunk: Uint8Array) {
					let buf = (buffers.get(sock) ?? "") + Buffer.from(chunk).toString("utf8");
					let nl: number;
					while ((nl = buf.indexOf("\n")) >= 0) {
						const line = buf.slice(0, nl).trim();
						buf = buf.slice(nl + 1);
						if (line) handle(line, sock);
					}
					buffers.set(sock, buf);
				},
				close(sock: unknown) {
					clients.delete(sock as { write: (s: string) => void });
					buffers.delete(sock);
				},
				error(sock: unknown) {
					clients.delete(sock as { write: (s: string) => void });
					buffers.delete(sock);
				},
			},
		});
	} catch {
		// e.g. another announcer won the socket, or Bun.listen is unavailable.
		stop();
	}

	// Clear the socket in the registry before unlinking it, leaving a dormant,
	// resumable entry, then stop the server. Idempotent and synchronous so it is
	// safe from a process-exit handler. opencode has no plugin-unload hook, so
	// this runs from exit/SIGINT/SIGTERM (below); if teardown is missed, corral's
	// dead-socket sweep makes the record dormant anyway.
	function stop() {
		try {
			for (const c of clients) c.end?.();
		} catch {}
		clients.clear();
		buffers.clear();
		try {
			server?.stop();
		} catch {}
		server = undefined;
		clearSocketInRegistry();
		if (socketPath) {
			try {
				fs.rmSync(socketPath, { force: true });
			} catch {}
			socketPath = undefined;
		}
	}

	// opencode has no unload hook, so hang teardown off the process. "exit" is
	// synchronous best-effort for normal shutdown; the signal handlers clean up
	// then re-raise the signal so opencode's own default termination still runs
	// (process.once removed our handler, so the re-raise hits the default).
	process.on("exit", stop);
	for (const sig of ["SIGINT", "SIGTERM"] as const) {
		process.once(sig, () => {
			stop();
			try {
				process.kill(process.pid, sig);
			} catch {}
		});
	}

	// --- outgoing: session/update broadcasts ---

	// Every outgoing session/update shares this envelope.
	function sessionUpdateLine(update: Record<string, unknown>): string {
		return (
			JSON.stringify({
				jsonrpc: "2.0",
				method: "session/update",
				params: { sessionId: activeSessionId, update },
			}) + "\n"
		);
	}

	// State transitions ride the standard ACP v2 state_update session/update
	// (agentclientprotocol.com/rfds/v2/prompt): running / idle / requires_action.
	function broadcastState() {
		if (clients.size === 0 || !activeSessionId) return;
		const line = sessionUpdateLine({ sessionUpdate: "state_update", state: currentState });
		for (const c of clients) {
			try {
				c.write(line);
			} catch {}
		}
	}

	// A turn is starting: transition idle -> running. Do not override
	// requires_action here; only permission.replied clears that (the turn
	// continues), mirroring pi's `question`-tool handling.
	function markRunning() {
		if (currentState !== "idle") return;
		currentState = "running";
		broadcastState();
	}

	// Push a session_info_update only when the title changed, so an already-
	// connected client sees a rename without reconnecting. New clients get the
	// current title from their session/list seed.
	function broadcastTitleIfChanged() {
		if (activeTitle === lastTitle) return;
		lastTitle = activeTitle;
		broadcast({ sessionUpdate: "session_info_update", title: activeTitle });
	}

	function broadcast(update: Record<string, unknown>) {
		if (clients.size === 0 || !activeSessionId) return;
		const line = sessionUpdateLine(update);
		for (const c of clients) {
			try {
				c.write(line);
			} catch {}
		}
	}

	// Resolve waiting session/prompt requests once the turn drains (session.idle).
	// Coarser than per-message attribution: all waiting clients resolve at once,
	// documented like pi's agent_end drain.
	function drainPrompts() {
		while (pendingPrompts.length > 0) {
			const p = pendingPrompts.shift();
			try {
				p?.conn.write(JSON.stringify({ jsonrpc: "2.0", id: p.id, result: { stopReason: "end_turn" } }) + "\n");
			} catch {}
		}
	}

	// Fetch the session title from opencode and, if it changed, broadcast the
	// rename and persist it. opencode auto-names sessions, so unlike pi there is
	// no first-user-message fallback to compute. Uses session.list() (the
	// confirmed client method) and matches by id. Best-effort: the client shape
	// is unverified, so tolerate either { data } or a bare array, and any failure.
	async function refreshTitle() {
		if (!activeSessionId) return;
		try {
			const res = (await client.session.list()) as {
				data?: Array<{ id?: string; title?: unknown }>;
			};
			const list = Array.isArray(res?.data) ? res.data : Array.isArray(res) ? (res as never[]) : [];
			const s = list.find((x: { id?: string }) => x?.id === activeSessionId);
			const raw = s && typeof s.title === "string" ? s.title : "";
			const clean = raw.replace(/\s+/g, " ").trim();
			activeTitle = clean ? (clean.length > MAX_TITLE ? `${clean.slice(0, MAX_TITLE - 1)}…` : clean) : null;
			broadcastTitleIfChanged();
			writeRegistry();
		} catch {}
	}

	// --- incoming: client requests ---

	function handle(line: string, conn: { write: (s: string) => void }) {
		let msg: {
			id?: number | string;
			method?: string;
			params?: { prompt?: Array<{ type?: string; text?: string }> };
		};
		try {
			msg = JSON.parse(line);
		} catch {
			return;
		}
		if (!msg.method) return;

		// session/cancel is a notification (no id) per ACP.
		if (msg.method === "session/cancel") {
			if (activeSessionId) {
				try {
					// Fire-and-forget: aborting must not block the socket handler.
					void client.session.abort({ path: { id: activeSessionId } });
				} catch {}
			}
			return;
		}
		if (msg.id === undefined) return;

		const reply = (result: unknown) =>
			conn.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }) + "\n");
		const fail = (code: number, message: string) =>
			conn.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, error: { code, message } }) + "\n");

		switch (msg.method) {
			case "initialize":
				reply({
					protocolVersion: 1,
					agentCapabilities: { loadSession: false },
					// Version is unknown without a typed constant from opencode; the
					// name is what corral labels the card with.
					agentInfo: { name: "opencode", version: "unknown" },
					authMethods: [],
				});
				break;
			case "session/list":
				if (!activeSessionId) return fail(-32603, "no active session");
				reply({ sessions: [{ sessionId: activeSessionId, title: activeTitle, cwd: activeCwd }] });
				break;
			case "session/prompt": {
				if (!activeSessionId) return fail(-32603, "no active session");
				const text = (msg.params?.prompt ?? [])
					.filter((b) => b.type === "text" && typeof b.text === "string")
					.map((b) => b.text)
					.join("\n");
				if (!text) return fail(-32602, "prompt has no text content");
				// The message is injected verbatim, matching pi's session/prompt: the
				// provenance tag is added by corral before it reaches this socket, not
				// by the adapter. Fire-and-forget: opencode's prompt call waits for the
				// full turn, so we do NOT await it (that would block the socket
				// handler); opencode queues it while a turn is active. The request stays
				// open and resolves on the next session.idle (see drainPrompts).
				pendingPrompts.push({ conn, id: msg.id });
				try {
					void client.session.prompt({
						path: { id: activeSessionId },
						body: { parts: [{ type: "text", text }] },
					});
				} catch {
					// Dispatch failed outright: resolve now so the client is not stuck.
					pendingPrompts.pop();
					return fail(-32603, "failed to inject prompt");
				}
				break;
			}
			default:
				fail(-32601, `method not supported by corral-opencode: ${msg.method}`);
		}
	}

	// --- registry store ---

	// $CORRAL_REGISTRY_DIR, else $HOME/.corral/registry.
	function registryDir(): string | undefined {
		if (process.env.CORRAL_REGISTRY_DIR) return process.env.CORRAL_REGISTRY_DIR;
		const home = process.env.HOME;
		return home ? path.join(home, ".corral", "registry") : undefined;
	}

	// Mark the session dormant by clearing the live socket, without any session
	// lookup: read the known registry file and rewrite `socket: null`. Used by
	// stop() on process exit. Best-effort.
	function clearSocketInRegistry() {
		if (!registryFile) return;
		try {
			const rec = JSON.parse(fs.readFileSync(registryFile, "utf8"));
			rec.socket = null;
			const tmp = `${registryFile}.${process.pid}.tmp`;
			fs.writeFileSync(tmp, JSON.stringify(rec, null, 2), { mode: 0o600 });
			fs.renameSync(tmp, registryFile);
		} catch {
			// Record already gone or unreadable: nothing to clear.
		}
	}

	// corral's single discovery store: `<sessionId>.json` names the live socket
	// (or null when dormant) plus enough to resume. Written atomically
	// (tmp + rename) so a scanning corral never reads a half-written file. No-op
	// until the active session id is known.
	function writeRegistry() {
		try {
			const dir = registryDir();
			if (!dir || !activeSessionId) return;
			fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
			registryFile = path.join(dir, `${activeSessionId}.json`);
			// Launch commands corral runs verbatim (it never parses them): the argv
			// to spawn a fresh opencode and to resume this exact session. opencode
			// auto-persists sessions, so resumeCommand is always set once the id is
			// known (unlike pi, which gates on the session file existing).
			const record = {
				sessionId: activeSessionId,
				cwd: activeCwd,
				title: activeTitle,
				// Agent kind, so corral can label a dormant card (no socket to parse).
				label: "opencode",
				socket: socketPath ?? null,
				spawnCommand: ["opencode"],
				resumeCommand: ["opencode", "--session", activeSessionId],
				lastSeen: new Date().toISOString(),
			};
			const tmp = `${registryFile}.${process.pid}.tmp`;
			fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
			fs.renameSync(tmp, registryFile);
		} catch {
			// Announcing is best-effort and must never crash opencode.
		}
	}

	// --- event bus mapping (opencode events -> ACP broadcasts) ---

	// The sessionID lives on the event payload but its exact path varies across
	// opencode versions; probe defensively.
	function eventSessionId(ev: {
		properties?: { sessionID?: string; session_id?: string; info?: { sessionID?: string; id?: string } };
		sessionID?: string;
		session_id?: string;
	}): string | undefined {
		const p = ev?.properties ?? {};
		return p.sessionID ?? p.session_id ?? p.info?.sessionID ?? p.info?.id ?? ev?.sessionID ?? ev?.session_id;
	}

	// tool.execute.before/after payload fields are unverified; probe defensively.
	function toolId(p: Record<string, unknown>): string {
		return String(p.callID ?? p.toolCallId ?? p.id ?? "tool");
	}
	function toolName(p: Record<string, unknown>): string {
		return String(p.tool ?? p.toolName ?? p.name ?? "tool");
	}

	// Best-effort message text from a message.part.updated event. UNVERIFIED
	// payload shape: extract a text part and a role if present, skip otherwise.
	function broadcastMessageActivity(p: {
		part?: { type?: string; text?: string };
		info?: { role?: string };
		role?: string;
	}) {
		try {
			const part = p.part ?? {};
			const text = part.type === "text" && typeof part.text === "string" ? part.text : "";
			if (!text) return;
			const role = p.info?.role === "user" || p.role === "user" ? "user" : "assistant";
			broadcast({
				sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
				content: { type: "text", text },
			});
		} catch {}
	}

	return {
		event: async ({ event }: { event: { type?: string; properties?: Record<string, unknown> } }) => {
			try {
				const type = event?.type ?? "";
				const props = (event?.properties ?? {}) as Record<string, unknown>;
				const sid = eventSessionId(event as never);
				// Learn (or switch to) the active session and announce it.
				if (sid && sid !== activeSessionId) {
					activeSessionId = sid;
					writeRegistry();
					void refreshTitle();
				}

				switch (type) {
					case "session.created":
						if (sid) {
							activeSessionId = sid;
							writeRegistry();
							void refreshTitle();
						}
						break;
					case "session.idle":
						currentState = "idle";
						broadcastState();
						// Refresh title at the turn boundary (a rename shows up here) and
						// bump lastSeen for age-based pruning of dormant records.
						void refreshTitle();
						drainPrompts();
						break;
					case "message.updated":
						markRunning();
						break;
					case "message.part.updated":
						markRunning();
						broadcastMessageActivity(props as never);
						break;
					case "tool.execute.before":
						markRunning();
						broadcast({
							sessionUpdate: "tool_call",
							toolCallId: toolId(props),
							title: toolName(props),
							status: "in_progress",
							rawInput: props.args ?? props.input,
						});
						break;
					case "tool.execute.after":
						broadcast({
							sessionUpdate: "tool_call_update",
							toolCallId: toolId(props),
							status: props.error ? "failed" : "completed",
						});
						break;
					case "permission.updated":
						// A permission prompt is open: the session is blocked on the user.
						currentState = "requires_action";
						broadcastState();
						break;
					case "permission.replied":
						// Answered; the turn continues.
						currentState = "running";
						broadcastState();
						break;
					default:
						break;
				}
			} catch {
				// Never throw into opencode from the event bus.
			}
		},

		// corral_message_agent: hand a message to another session. It only submits
		// over ~/.corral/corrald.sock; corral (the unsandboxed board) is the trusted
		// cross-workdir router that authorizes, resolves the target, spawns/resumes
		// an agent, and injects with a provenance tag. Sandboxed agents cannot reach
		// each other directly, so this indirection is the only cross-session path.
		//
		// UNVERIFIED: the `tool()` registration helper and its schema/ctx shape are
		// coded from opencode's docs, not typechecked here.
		tool: {
			corral_message_agent: tool({
				description:
					"Send a message to another coding-agent session. Address it EITHER by " +
					"target_dir (reach whoever works in that directory, spawning one if none) " +
					"OR by target_session (reach that exact agent, resuming it if dormant) — " +
					"give exactly one. To reply to a message you received, pass its session id " +
					"(shown as 'session <id>' in the incoming tag) as target_session. corral " +
					"routes it and tags it as coming from you. Fire-and-forget: no reply is awaited.",
				args: {
					target_dir: tool.schema
						.string()
						.optional()
						.describe("Absolute path of the target agent's working directory."),
					target_session: tool.schema
						.string()
						.optional()
						.describe("Exact session id of the target agent (e.g. to reply to a sender)."),
					message: tool.schema.string().describe("The message to deliver."),
					force_new: tool.schema
						.boolean()
						.optional()
						.describe("With target_dir: spawn a dedicated fresh agent instead of reusing one."),
				},
				async execute(args: {
					target_dir?: string;
					target_session?: string;
					message: string;
					force_new?: boolean;
				}) {
					const home = process.env.HOME;
					const controlSocket =
						process.env.CORRAL_CONTROL_SOCKET ??
						(home ? path.join(home, ".corral", "corrald.sock") : undefined);
					if (!controlSocket) return "corral: no HOME; cannot submit message";

					const hasDir = typeof args.target_dir === "string" && args.target_dir.length > 0;
					const hasSession = typeof args.target_session === "string" && args.target_session.length > 0;
					if (hasDir === hasSession) {
						return "corral_message_agent: give exactly one of target_dir or target_session";
					}
					const record: Record<string, unknown> = {
						id: randomUUID(),
						fromCwd: activeCwd,
						// The sender's session id, so the recipient can reply to this exact agent.
						fromSession: activeSessionId ?? "",
						message: args.message,
						forceNew: args.force_new ?? false,
						createdAt: new Date().toISOString(),
					};
					if (hasDir) record.targetDir = args.target_dir;
					else record.targetSession = args.target_session;
					const dest = hasDir ? args.target_dir : `session ${args.target_session}`;
					// A connect failure means corral is not running: fail loud rather
					// than silently queue undelivered.
					let status: string;
					try {
						status = await submitToCorral(controlSocket, record);
					} catch {
						return `corral is not running (cannot reach ${controlSocket}); message not sent.`;
					}
					return describeAck(status, String(dest));
				},
			}),
		},
	};
};

export default CorralOpencode;

// Submit a message record over corral's control socket and resolve with the
// one-word ack status corral returns. Rejects on connect failure (corral down)
// or if no ack arrives within a short window (so the tool never hangs).
function submitToCorral(socketPath: string, record: Record<string, unknown>): Promise<string> {
	return new Promise((resolve, reject) => {
		const conn = net.createConnection(socketPath);
		let buf = "";
		let done = false;
		const finish = (fn: () => void) => {
			if (done) return;
			done = true;
			conn.destroy();
			fn();
		};
		conn.setTimeout(5000, () => finish(() => reject(new Error("timeout"))));
		conn.on("connect", () => conn.write(`${JSON.stringify(record)}\n`));
		conn.on("data", (chunk) => {
			buf += chunk.toString("utf8");
			const nl = buf.indexOf("\n");
			if (nl < 0) return;
			let status = "unknown";
			try {
				status = String((JSON.parse(buf.slice(0, nl)) as { status?: unknown }).status ?? "unknown");
			} catch {}
			finish(() => resolve(status));
		});
		conn.on("error", (e) => finish(() => reject(e)));
		conn.on("close", () => finish(() => reject(new Error("closed before ack"))));
	});
}

// Turn corral's ack status into a message for the sending agent.
function describeAck(status: string, dest: string): string {
	switch (status) {
		case "accepted":
			return `Accepted for routing by corral (to ${dest}).`;
		case "approval_needed":
			return `Submitted to ${dest}; approval needed, delivered once approved.`;
		case "recipient_not_found":
			return `Not sent: recipient not found (${dest}).`;
		case "directory_not_known":
			return `Not sent: directory not known (${dest}).`;
		case "malformed":
			return "Not sent: corral rejected the message as malformed.";
		default:
			return `corral responded: ${status}.`;
	}
}
