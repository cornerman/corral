/**
 * corral-announce: make this pi session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running.
 *
 * Binds an ACP socket inside this session's own workdir at
 * <cwd>/.corral/pi-<pid>.sock (override the dir with $CORRAL_SOCKET_DIR) and
 * writes a registry record at $HOME/.corral/registry/<sessionId>.json
 * (override with $CORRAL_REGISTRY_DIR) pointing at that socket. The socket is
 * workdir-local so only this session (and unsandboxed tools like corral) can
 * reach it; the registry is corral's single discovery store. On clean
 * shutdown the socket is unlinked and the record's `socket` is cleared to
 * null, leaving a dormant, resumable record. Served surface:
 *   initialize            identity (agentInfo)
 *   session/list          this session: id, title, cwd
 *   session/prompt        inject a user message (queued as follow-up while
 *                         the agent is busy); responds on turn completion
 *   session/cancel        abort the current turn (notification)
 * Broadcast to every connected client as session/update notifications:
 *   user_message_chunk    user messages (TUI-typed or injected)
 *   agent_message_chunk   assistant messages (whole message on message_end;
 *                         token deltas are a later refinement -- the event
 *                         stream shape is not part of pi's documented API)
 *   tool_call / tool_call_update
 *   session_info_update   session renames
 *   state_update          running/idle/requires_action (ACP v2 vocabulary):
 *                         turn_start/turn_end, and requires_action while the
 *                         interactive `question` tool blocks on the user
 *
 * Registers one tool, message_agent, that queues a cross-session message as a
 * mailbox file under $HOME/.corral/outbox/<id>.json (override with
 * $CORRAL_OUTBOX_DIR) for corral to route; the agent never reaches another
 * session directly. A message is addressed by target_dir (whoever works there)
 * or target_session (an exact agent, e.g. to reply), and is stamped with the
 * sender's session id so the receiver can reply to precisely this agent.
 *
 * Install: symlink into ~/.pi/agent/extensions/ or run pi with
 *   pi -e /path/to/corral-announce.ts
 *
 * Multiple concurrent clients are fine: every request is answered from
 * current state and updates go to all connections.
 */

import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import { VERSION } from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export default function (pi: ExtensionAPI) {
	let server: net.Server | undefined;
	let socketPath: string | undefined;
	let registryFile: string | undefined;
	let currentCtx: ExtensionContext | undefined;
	// Last title pushed to clients, so we broadcast a session_info_update only
	// when it actually changes. pi fires session_info_changed on an explicit
	// rename, but not when the first user message becomes the fallback title, so
	// we also re-check on turn_end. `undefined` forces the first comparison.
	let lastTitle: string | null | undefined;
	// First non-empty user message this run, from message_end (authoritative,
	// unlike scanning getEntries mid-turn where a fresh session has no entry
	// yet). The fallback title when the session is unnamed.
	let firstUserText: string | undefined;
	// Triage state in ACP v2 vocabulary (running/idle/requires_action), broadcast
	// as the standard state_update session/update so corral (and any ACP client)
	// can column the agent without polling.
	let currentState: "running" | "idle" | "requires_action" = "idle";
	// toolCallId of an in-flight `question` tool: while it runs, the agent is
	// blocked on the user, which is requires_action.
	let questionCallId: string | undefined;
	const clients = new Set<net.Socket>();
	// session/prompt requests waiting for the turn that consumes them to end.
	const pendingPrompts: Array<{ conn: net.Socket; id: number | string }> = [];

	// message_agent: let this agent hand a message to another session, addressed
	// by working directory. It only writes a mailbox file under ~/.corral/outbox;
	// corral (the unsandboxed board) is the trusted cross-workdir router that
	// authorizes, resolves the target, spawns an agent if none runs there, and
	// injects with a provenance tag. Sandboxed agents cannot reach each other
	// directly, so this indirection is the only cross-session path.
	pi.registerTool({
		name: "message_agent",
		label: "Message agent",
		description:
			"Send a message to another coding-agent session. Address it EITHER by " +
			"target_dir (reach whoever works in that directory, spawning one if none) " +
			"OR by target_session (reach that exact agent, resuming it if dormant) — " +
			"give exactly one. To reply to a message you received, pass its session id " +
			"(shown as 'session <id>' in the incoming tag) as target_session. corral " +
			"routes it and tags it as coming from you. Fire-and-forget: no reply is awaited.",
		parameters: Type.Object({
			target_dir: Type.Optional(
				Type.String({
					description: "Absolute path of the target agent's working directory.",
				}),
			),
			target_session: Type.Optional(
				Type.String({
					description: "Exact session id of the target agent (e.g. to reply to a sender).",
				}),
			),
			message: Type.String({ description: "The message to deliver." }),
			force_new: Type.Optional(
				Type.Boolean({
					description: "With target_dir: spawn a dedicated fresh agent instead of reusing one.",
				}),
			),
		}),
		async execute(_id, params, _signal, _onUpdate, ctx) {
			const home = process.env.HOME;
			const outbox = process.env.CORRAL_OUTBOX_DIR ?? (home ? path.join(home, ".corral", "outbox") : undefined);
			if (!outbox) {
				return { content: [{ type: "text", text: "corral: no HOME; cannot queue message" }] };
			}
			const hasDir = typeof params.target_dir === "string" && params.target_dir.length > 0;
			const hasSession = typeof params.target_session === "string" && params.target_session.length > 0;
			if (hasDir === hasSession) {
				return {
					content: [{ type: "text", text: "message_agent: give exactly one of target_dir or target_session" }],
				};
			}
			fs.mkdirSync(outbox, { recursive: true, mode: 0o700 });
			const id = randomUUID();
			const record: Record<string, unknown> = {
				id,
				fromCwd: ctx.cwd,
				// The sender's session id, so the recipient can reply to this exact agent.
				fromSession: ctx.sessionManager.getSessionId(),
				message: params.message,
				forceNew: params.force_new ?? false,
				createdAt: new Date().toISOString(),
			};
			if (hasDir) record.targetDir = params.target_dir;
			else record.targetSession = params.target_session;
			// Atomic write so a scanning corral never reads a half-written mailbox.
			const file = path.join(outbox, `${id}.json`);
			const tmp = `${file}.${process.pid}.tmp`;
			fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
			fs.renameSync(tmp, file);
			const dest = hasDir ? `the agent in ${params.target_dir}` : `session ${params.target_session}`;
			return {
				content: [{ type: "text", text: `Queued message to ${dest} (corral will deliver it).` }],
			};
		},
	});

	const stop = () => {
		// Idempotent: session_shutdown can fire more than once across
		// session replacement flows (/resume, /fork).
		for (const c of clients) c.destroy();
		clients.clear();
		server?.close();
		server = undefined;
		// Clear the socket in the registry before unlinking it: the record
		// stays as a dormant, resumable entry.
		if (currentCtx && registryFile) writeRegistry(currentCtx, null);
		if (socketPath) {
			fs.rmSync(socketPath, { force: true });
			socketPath = undefined;
		}
	};

	pi.on("session_start", async (_event, ctx) => {
		stop();
		currentCtx = ctx;
		currentState = "idle";
		questionCallId = undefined;
		lastTitle = undefined;
		firstUserText = undefined;
		// Socket lives inside this session's own workdir: only this session
		// (and unsandboxed tools like corral) can reach it. Not
		// $XDG_RUNTIME_DIR: sandboxed pi sessions cannot reach that.
		const socketDir = process.env.CORRAL_SOCKET_DIR ?? path.join(ctx.cwd, ".corral");

		// 0700: the socket grants prompt access to this session; directory
		// permissions are the only peer authentication we rely on.
		fs.mkdirSync(socketDir, { recursive: true, mode: 0o700 });
		socketPath = path.join(socketDir, `pi-${process.pid}.sock`);
		fs.rmSync(socketPath, { force: true }); // stale leftover from a crashed pid reuse
		writeRegistry(ctx, socketPath); // announce in the registry store

		server = net.createServer((conn) => {
			clients.add(conn);
			// Seed the new client with the current state so it can column us at once.
			if (currentCtx && !conn.destroyed) {
				conn.write(sessionUpdateLine({ sessionUpdate: "state_update", state: currentState }));
			}
			let buf = "";
			conn.on("data", (chunk) => {
				buf += chunk.toString("utf8");
				let nl: number;
				while ((nl = buf.indexOf("\n")) >= 0) {
					const line = buf.slice(0, nl).trim();
					buf = buf.slice(nl + 1);
					if (line) handle(line, conn);
				}
			});
			const drop = () => {
				clients.delete(conn);
				conn.destroy();
			};
			conn.on("error", drop);
			conn.on("close", drop);
		});
		server.on("error", () => stop()); // e.g. EADDRINUSE: another announcer won
		server.listen(socketPath);
	});

	pi.on("session_shutdown", async () => stop());

	// --- outgoing: agent activity -> session/update broadcasts ---

	pi.on("turn_start", async () => {
		currentState = "running";
		broadcastState();
	});

	pi.on("turn_end", async () => {
		currentState = "idle";
		broadcastState();
		// The title may have become available this turn (the first user message
		// becomes the fallback title); push it to clients if it changed.
		broadcastTitleIfChanged();
		// Refresh lastSeen so age-based pruning of dormant records is accurate.
		if (currentCtx) writeRegistry(currentCtx, socketPath ?? null);
	});

	pi.on("message_end", async (event) => {
		const role = event.message.role;
		if (role !== "user" && role !== "assistant") return;
		const text = messageText(event.message);
		if (!text) return;
		broadcast({
			sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
			content: { type: "text", text },
		});
		// The first user message is the fallback title: capture it and push the
		// title as soon as it arrives, so a card stops showing "(unnamed)".
		if (role === "user" && firstUserText === undefined) {
			firstUserText = text;
			broadcastTitleIfChanged();
		}
	});

	pi.on("tool_execution_start", async (event) => {
		broadcast({
			sessionUpdate: "tool_call",
			toolCallId: event.toolCallId,
			title: event.toolName,
			status: "in_progress",
			rawInput: event.args,
		});
		// The `question` tool blocks on the user: that is requires_action. This is
		// the only user-input gate an extension can observe today; pi's built-in
		// tool-approval prompt is not surfaced (see AGENTS.md Future).
		if (event.toolName === "question") {
			questionCallId = event.toolCallId;
			currentState = "requires_action";
			broadcastState();
		}
	});

	pi.on("tool_execution_end", async (event) => {
		broadcast({
			sessionUpdate: "tool_call_update",
			toolCallId: event.toolCallId,
			status: event.isError ? "failed" : "completed",
		});
		if (event.toolCallId === questionCallId) {
			questionCallId = undefined;
			currentState = "running"; // answered; the turn continues
			broadcastState();
		}
	});

	pi.on("session_info_changed", async () => {
		broadcastTitleIfChanged();
		// Persist the new title so dormant records show the current name.
		if (currentCtx) writeRegistry(currentCtx, socketPath ?? null);
	});

	pi.on("agent_end", async (_event, ctx) => {
		// Resolve waiting session/prompt requests once the queue is drained:
		// a prompt delivered as follow-up is processed before this condition
		// holds, so "idle with nothing pending" means every injected message
		// has had its turn. Coarser than per-message tracking, documented so.
		if (pendingPrompts.length === 0 || ctx.hasPendingMessages()) return;
		while (pendingPrompts.length > 0) {
			const p = pendingPrompts.shift();
			if (p && !p.conn.destroyed) {
				p.conn.write(
					JSON.stringify({ jsonrpc: "2.0", id: p.id, result: { stopReason: "end_turn" } }) +
						"\n",
				);
			}
		}
	});

	// Every outgoing session/update shares this envelope.
	function sessionUpdateLine(update: Record<string, unknown>): string {
		return (
			JSON.stringify({
				jsonrpc: "2.0",
				method: "session/update",
				params: { sessionId: sessionInfo(currentCtx!).sessionId, update },
			}) + "\n"
		);
	}

	// State transitions ride the standard ACP v2 state_update session/update
	// (agentclientprotocol.com/rfds/v2/prompt): running / idle / requires_action.
	function broadcastState() {
		if (clients.size === 0 || !currentCtx) return;
		const line = sessionUpdateLine({ sessionUpdate: "state_update", state: currentState });
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}

	// Push a session_info_update only when the computed title changed, so an
	// already-connected client sees the name appear without a reconnect. New
	// clients still get the current title from their session/list seed.
	function broadcastTitleIfChanged() {
		if (!currentCtx) return;
		const title = sessionTitle(currentCtx);
		if (title === lastTitle) return;
		lastTitle = title;
		broadcast({ sessionUpdate: "session_info_update", title });
	}

	function broadcast(update: Record<string, unknown>) {
		if (clients.size === 0 || !currentCtx) return;
		const line = sessionUpdateLine(update);
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}

	// --- incoming: client requests ---

	function handle(line: string, conn: net.Socket) {
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
			currentCtx?.abort();
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
					agentInfo: { name: "pi", version: VERSION },
					authMethods: [],
				});
				break;
			case "session/list":
				if (!currentCtx) return fail(-32603, "no active session");
				reply({ sessions: [sessionInfo(currentCtx)] });
				break;
			case "session/prompt": {
				if (!currentCtx) return fail(-32603, "no active session");
				const text = (msg.params?.prompt ?? [])
					.filter((b) => b.type === "text" && typeof b.text === "string")
					.map((b) => b.text)
					.join("\n");
				if (!text) return fail(-32602, "prompt has no text content");
				// Busy sessions get the message queued as a follow-up; the
				// request stays open until the queue drains (see agent_end).
				pendingPrompts.push({ conn, id: msg.id });
				if (currentCtx.isIdle()) {
					pi.sendUserMessage(text);
				} else {
					pi.sendUserMessage(text, { deliverAs: "followUp" });
				}
				break;
			}
			default:
				fail(-32601, `method not supported by corral-announce: ${msg.method}`);
		}
	}

	// Longest title corral should receive; a raw first user message can be huge.
	const MAX_TITLE = 60;

	// Title precedence mirrors window-title.ts so corral and the window agree:
	// the session name if the model set it, else the first user message. pi never
	// auto-names sessions, so without this corral would show "(unnamed)".
	function sessionTitle(ctx: ExtensionContext): string | null {
		// Name if set, else the first user message this run, else scan history
		// (for a resumed session, whose first message predates this run and so
		// never fired message_end here).
		let raw = pi.getSessionName() ?? firstUserText ?? undefined;
		if (!raw) {
			for (const e of ctx.sessionManager.getEntries() as Array<{ type?: string; message?: unknown }>) {
				if (e.type === "message" && (e.message as { role?: string })?.role === "user") {
					const t = messageText(e.message as { content?: unknown });
					if (t) {
						raw = t;
						break;
					}
				}
			}
		}
		if (!raw) return null;
		const clean = raw.replace(/\s+/g, " ").trim();
		return clean.length > MAX_TITLE ? `${clean.slice(0, MAX_TITLE - 1)}…` : clean;
	}

	// Registry store: $CORRAL_REGISTRY_DIR, else $HOME/.corral/registry.
	function registryDir(): string | undefined {
		if (process.env.CORRAL_REGISTRY_DIR) return process.env.CORRAL_REGISTRY_DIR;
		const home = process.env.HOME;
		return home ? path.join(home, ".corral", "registry") : undefined;
	}

	// corral's single discovery store: `<sessionId>.json` names the live socket
	// (or null when dormant) plus enough to resume. Written atomically
	// (tmp + rename) so a scanning corral never reads a half-written file.
	function writeRegistry(ctx: ExtensionContext, socket: string | null) {
		const dir = registryDir();
		if (!dir) return;
		const sessionId = ctx.sessionManager.getSessionId();
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		registryFile = path.join(dir, `${sessionId}.json`);
		const record = {
			sessionId,
			cwd: ctx.cwd,
			title: sessionTitle(ctx),
			// Agent kind, so corral can label a dormant card (no socket to parse).
			label: "pi",
			socket,
			// The session file path is what `pi --session <path>` resumes; null
			// for an ephemeral (--no-session) session, which is not resumable.
			resume: ctx.sessionManager.getSessionFile() ?? null,
			lastSeen: new Date().toISOString(),
		};
		const tmp = `${registryFile}.${process.pid}.tmp`;
		fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
		fs.renameSync(tmp, registryFile);
	}

	function sessionInfo(ctx: ExtensionContext) {
		return {
			// The stable session UUID, matching the registry filename and the
			// reply handle stamped by message_agent. Must NOT be the session
			// file path, or session-addressed routing (live_by_session) would
			// never match a live agent.
			sessionId: ctx.sessionManager.getSessionId(),
			title: sessionTitle(ctx),
			cwd: ctx.cwd,
		};
	}
}

/** Plain text of a session message whose content may be a string or blocks. */
function messageText(message: { content?: unknown }): string {
	const content = message.content;
	if (typeof content === "string") return content;
	if (Array.isArray(content)) {
		return content
			.filter((b) => b && b.type === "text" && typeof b.text === "string")
			.map((b) => b.text)
			.join("\n");
	}
	return "";
}
