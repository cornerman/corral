/**
 * corral-announce: make this pi session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running.
 *
 * Binds an ACP socket at $HOME/.corral/sockets/pi-<pid>.sock (override the
 * directory with $CORRAL_ACP_DIR). Served surface:
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
 * Install: symlink into ~/.pi/agent/extensions/ or run pi with
 *   pi -e /path/to/corral-announce.ts
 *
 * Multiple concurrent clients are fine: every request is answered from
 * current state and updates go to all connections.
 */

import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	let server: net.Server | undefined;
	let socketPath: string | undefined;
	let currentCtx: ExtensionContext | undefined;
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

	const stop = () => {
		// Idempotent: session_shutdown can fire more than once across
		// session replacement flows (/resume, /fork).
		for (const c of clients) c.destroy();
		clients.clear();
		server?.close();
		server = undefined;
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
		// Discovery dir: $CORRAL_ACP_DIR, else $HOME/.corral. Not
		// $XDG_RUNTIME_DIR: sandboxed pi sessions cannot reach it.
		const home = process.env.HOME;
		const dir = process.env.CORRAL_ACP_DIR ?? (home ? path.join(home, ".corral", "sockets") : undefined);
		if (!dir) return; // nowhere to announce -- stay silent

		// 0700: the socket grants prompt access to this session; directory
		// permissions are the only peer authentication we rely on.
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		socketPath = path.join(dir, `pi-${process.pid}.sock`);
		fs.rmSync(socketPath, { force: true }); // stale leftover from a crashed pid reuse

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
		broadcast({ sessionUpdate: "session_info_update", title: pi.getSessionName() ?? null });
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
					agentInfo: { name: "pi", version: piVersion() },
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
		let raw = pi.getSessionName() ?? undefined;
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

	function sessionInfo(ctx: ExtensionContext) {
		return {
			sessionId: ctx.sessionManager.getSessionFile() ?? `ephemeral-${process.pid}`,
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

/** pi does not expose its own version to extensions; best-effort lookup. */
function piVersion(): string {
	try {
		// require() is available to extensions (they run in pi's Node process).
		// eslint-disable-next-line @typescript-eslint/no-require-imports
		return require("@earendil-works/pi-coding-agent/package.json").version;
	} catch {
		return "?";
	}
}
