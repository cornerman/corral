/**
 * corral-announce: make this pi session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running.
 *
 * Binds an ACP socket at $XDG_RUNTIME_DIR/acp/pi-<pid>.sock. Served surface:
 *   initialize            identity (agentInfo)
 *   session/list          this session: id, title, cwd; working/idle under
 *                         SessionInfo._meta["corral/state"]
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
 * Plus a vendor ExtNotification outside the ACP session/update union:
 *   _corral/state         working/idle transitions (turn_start/turn_end)
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
	// Working/idle triage state, driven by turn_start/turn_end. Reported in
	// session/list and broadcast on every transition so corral can column the
	// agent without polling.
	let currentState: "working" | "idle" = "idle";
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
		const runtimeDir = process.env.XDG_RUNTIME_DIR;
		if (!runtimeDir) return; // no runtime dir, no discovery -- stay silent

		const dir = path.join(runtimeDir, "acp");
		// 0700: the socket grants prompt access to this session; directory
		// permissions are the only peer authentication we rely on.
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		socketPath = path.join(dir, `pi-${process.pid}.sock`);
		fs.rmSync(socketPath, { force: true }); // stale leftover from a crashed pid reuse

		server = net.createServer((conn) => {
			clients.add(conn);
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
		currentState = "working";
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
	});

	pi.on("tool_execution_end", async (event) => {
		broadcast({
			sessionUpdate: "tool_call_update",
			toolCallId: event.toolCallId,
			status: event.isError ? "failed" : "completed",
		});
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

	// Working/idle is not part of ACP's session/update union, so it rides a
	// vendor-namespaced ExtNotification (`_corral/state`). Conformant clients
	// ignore unknown notifications; corral listens for this method.
	function broadcastState() {
		if (clients.size === 0 || !currentCtx) return;
		const line =
			JSON.stringify({
				jsonrpc: "2.0",
				method: "_corral/state",
				params: { sessionId: sessionInfo(currentCtx).sessionId, state: currentState },
			}) + "\n";
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}

	function broadcast(update: Record<string, unknown>) {
		if (clients.size === 0 || !currentCtx) return;
		const notification = JSON.stringify({
			jsonrpc: "2.0",
			method: "session/update",
			params: { sessionId: sessionInfo(currentCtx).sessionId, update },
		});
		for (const c of clients) {
			if (!c.destroyed) c.write(notification + "\n");
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

	function sessionInfo(ctx: ExtensionContext) {
		return {
			sessionId: ctx.sessionManager.getSessionFile() ?? `ephemeral-${process.pid}`,
			title: pi.getSessionName() ?? null,
			cwd: ctx.cwd,
			// Vendor state under _meta; SessionInfo has no standard run-state field.
			_meta: { "corral/state": currentState },
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
