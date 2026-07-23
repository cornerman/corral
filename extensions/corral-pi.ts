/**
 * corral-pi: make this pi session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running.
 *
 * Binds an ACP socket inside this session's own workdir at
 * <cwd>/.corral/pi-<pid>.sock (override the dir with $CORRAL_SOCKET_DIR) and
 * writes its registry record inside this workdir at
 * <cwd>/.corral/registry/<sessionId>.json and drops a pointer file at
 * $HOME/.corral/input/registry/<sessionId> (content = this cwd; override
 * $CORRAL_INPUT_REGISTRY), so
 * corrald authenticates identity by where the record physically lives. The
 * record points at that socket. The socket is
 * workdir-local so only this session (and unsandboxed tools like corral) can
 * reach it; the registry is corral's single discovery store. On clean
 * shutdown the socket is unlinked and the record's `socket` is cleared to
 * null, leaving a dormant, resumable record. Served surface:
 *   initialize            identity (agentInfo)
 *   session/list          this session: id, title, cwd
 *   session/prompt        inject a user message (queued as follow-up while
 *                         the agent is busy); responds on turn completion
 *   session/load           replay the full message history (user/assistant
 *                          text only, not tool calls) as session/update
 *                          notifications, then respond (ACP v1 session/load;
 *                          agentclientprotocol.com/protocol/session-setup
 *                          #loading-sessions)
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
 * Registers one tool, corral_message_agent, that submits a cross-session
 * message over corral's control socket $HOME/.corral/corrald.sock (override
 * with $CORRAL_CONTROL_SOCKET) for corral to route; the agent never reaches
 * another session directly. Submission gets a synchronous ack (accepted /
 * approval_needed / recipient_not_found / directory_not_known), and a connect failure
 * means corral is down (fail loud, no silent queue). A message is addressed by
 * target_dir (whoever works there) or target_session (an exact agent, e.g. to
 * reply), and is stamped with the sender's session id so the receiver can
 * reply to precisely this agent.
 *
 * Install: symlink into ~/.pi/agent/extensions/ or run pi with
 *   pi -e /path/to/corral-pi.ts
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
	// The record file, in the session's own workdir
	// (`<cwd>/.corral/registry/<sessionId>.json`). corrald authenticates identity
	// by the record's physical location (only this workdir's sandboxed agent can
	// write there). See the corral security model (physical-location identity).
	let recordFile: string | undefined;
	// Whether this session ever persisted its session file. A session that never
	// did has nothing to resume, so on shutdown its record is dropped rather than
	// left dormant (see writeRegistry / stop). Set once, never reset.
	let everPersisted = false;
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
	// Current model as "<provider>/<id>" (e.g. anthropic/claude-opus-4), or
	// undefined before pi resolves one. Broadcast as an ACP config option and
	// persisted in the record so a dormant card shows its last-known model.
	let currentModel: string | undefined;
	// Last-known context info (entries/percent/age), refreshed at turn_start
	// and turn_end (see below) and persisted to the registry record so a
	// dormant card still shows its last reading.
	let currentContext: { entries: number; percent: number | null; age: string } | undefined;
	// toolCallId of an in-flight `question` tool: while it runs, the agent is
	// blocked on the user, which is requires_action.
	let questionCallId: string | undefined;
	const clients = new Set<net.Socket>();
	// session/prompt requests waiting for the turn that consumes them to end.
	const pendingPrompts: Array<{ conn: net.Socket; id: number | string }> = [];

	// corral_message_agent: let this agent hand a message to another session. It
	// only submits over ~/.corral/corrald.sock; corral (the unsandboxed board) is
	// the trusted cross-workdir router that authorizes, resolves the target,
	// spawns an agent if none runs there, and injects with a provenance tag.
	// Sandboxed agents cannot reach each other directly, so this indirection is
	// the only cross-session path.
	pi.registerTool({
		name: "corral_message_agent",
		label: "Message agent",
		description:
			"Message another coding-agent session and hold a back-and-forth with it. Use it to " +
			"ask a peer agent a question, hand off a subtask, or answer a message you received.\n\n" +
			"Addressing — give EXACTLY ONE of:\n" +
			"• target_session: reach one specific session by its id. This is how you REPLY: an " +
			"incoming message is tagged '[from <dir> (session <id>)]', so pass that <id> " +
			"as target_session and your answer lands on the exact agent that wrote to you (never a " +
			"sibling in the same directory). A dormant session is resumed to receive it.\n" +
			"• target_dir: reach whoever works in that directory (absolute path), starting a new " +
			"agent there if none is running.\n\n" +
			"Replying is expected: delivery is one-way and fire-and-forget — corral does NOT route " +
			"a response back automatically. If you asked something, wait for the other agent to " +
			"message you back; if you received a message and a reply would help, send one to its " +
			"session id. Every message is tagged with your identity so the recipient can reply to you.",
		parameters: Type.Object({
			target_session: Type.Optional(
				Type.String({
					description:
						"Reach this exact session id (resuming it if dormant). Use it to REPLY: it is the " +
						"<id> from the '[from <dir> (session <id>)]' tag on a message you received.",
				}),
			),
			target_dir: Type.Optional(
				Type.String({
					description:
						"Absolute path: reach whoever works in this directory, starting a new agent there " +
						"if none is live. Use it to start a conversation when you have no session id yet.",
				}),
			),
			message: Type.String({ description: "The message text to deliver to the other agent." }),
			force_new: Type.Optional(
				Type.Boolean({
					description:
						"With target_dir only: always start a dedicated fresh agent instead of reusing the " +
						"one already working there.",
				}),
			),
			label: Type.Optional(
				Type.String({
					description:
						"With target_dir only: which agent kind to start if a fresh agent is spawned " +
						'(e.g. "pi", "opencode"). Defaults to the kind already used in that directory.',
				}),
			),
			hidden: Type.Optional(
				Type.Boolean({
					description:
						"Whether a newly spawned agent runs hidden (alive and working, but no window). " +
						"Defaults true, so an agent you summon never pops a window on the operator; they " +
						"reveal it from the board with h or Enter. Set false to request a visible window, " +
						"which the operator must approve. Ignored when the target agent is already running.",
				}),
			),
		}),
		async execute(_id, params, _signal, _onUpdate, ctx) {
			const home = process.env.HOME;
			const socketPath =
				process.env.CORRAL_CONTROL_SOCKET ?? (home ? path.join(home, ".corral", "corrald.sock") : undefined);
			if (!socketPath) {
				return { content: [{ type: "text", text: "corral: no HOME; cannot submit message" }] };
			}
			const hasDir = typeof params.target_dir === "string" && params.target_dir.length > 0;
			const hasSession = typeof params.target_session === "string" && params.target_session.length > 0;
			if (hasDir === hasSession) {
				return {
					content: [{ type: "text", text: "corral_message_agent: give exactly one of target_dir or target_session" }],
				};
			}
			const record: Record<string, unknown> = {
				id: randomUUID(),
				fromCwd: ctx.cwd,
				// The sender's session id, so the recipient can reply to this exact agent.
				fromSession: ctx.sessionManager.getSessionId(),
				message: params.message,
				forceNew: params.force_new ?? false,
				createdAt: new Date().toISOString(),
			};
			if (hasDir) record.targetDir = params.target_dir;
			else record.targetSession = params.target_session;
			// Optional: which agent kind to spawn if target_dir has no live agent.
			if (params.label) record.label = params.label;
			// Spawn visibility (default hidden); a visible spawn is operator-gated.
			if (typeof params.hidden === "boolean") record.hidden = params.hidden;
			const dest = hasDir ? params.target_dir : `session ${params.target_session}`;
			// Submit over corral's control socket. A connect failure means corral is
			// not running: fail loud here rather than silently queue undelivered.
			let status: string;
			try {
				status = await submitToCorral(socketPath, ctx.cwd, record);
			} catch {
				return {
					content: [
						{
							type: "text",
							text: `corral is not running (cannot reach ${socketPath}); message not sent.`,
						},
					],
				};
			}
			return { content: [{ type: "text", text: describeAck(status, String(dest)) }] };
		},
	});

	// corral_stop_agent: stop (kill) a peer session's process, leaving it dormant
	// and resumable — the same effect as the operator's `d` on the board, reached
	// through corrald. Gated exactly like corral_message_agent: the whitelisted
	// (sender-dir → target-dir) pair stops straight through, an unwhitelisted pair
	// prompts the operator. Stopping an already-dormant target is a no-op success.
	pi.registerTool({
		name: "corral_stop_agent",
		label: "Stop agent",
		description:
			"Stop another coding-agent session: kill its process so it goes dormant (its " +
			"transcript survives and it can be resumed later). Use it to shut down an agent you " +
			"spawned once its work is done, or to stop a runaway peer.\n\n" +
			"Addressing: give target_session, the exact session id to stop (the <id> from a " +
			"'[from <dir> (session <id>)]' provenance tag, or from list_corral_agents). There is no " +
			"target_dir form — stopping is precise.\n\n" +
			"Gating: if the (your dir → target dir) pair is not whitelisted the operator must " +
			"approve the stop. Stopping a session that is already dormant or gone succeeds as a " +
			"no-op. Fire-and-forget: corral does not report back once the kill lands.",
		parameters: Type.Object({
			target_session: Type.String({
				description:
					"The exact session id to stop (kill its process). Take it from a message's " +
					"'[from <dir> (session <id>)]' tag or from list_corral_agents.",
			}),
		}),
		async execute(_id, params, _signal, _onUpdate, ctx) {
			const home = process.env.HOME;
			const socketPath =
				process.env.CORRAL_CONTROL_SOCKET ?? (home ? path.join(home, ".corral", "corrald.sock") : undefined);
			if (!socketPath) {
				return { content: [{ type: "text", text: "corral: no HOME; cannot stop agent" }] };
			}
			if (typeof params.target_session !== "string" || params.target_session.length === 0) {
				return { content: [{ type: "text", text: "corral_stop_agent: target_session is required" }] };
			}
			const record: Record<string, unknown> = {
				op: "stop",
				id: randomUUID(),
				fromCwd: ctx.cwd,
				fromSession: ctx.sessionManager.getSessionId(),
				targetSession: params.target_session,
				createdAt: new Date().toISOString(),
			};
			const dest = `session ${params.target_session}`;
			let status: string;
			try {
				status = await submitToCorral(socketPath, ctx.cwd, record);
			} catch {
				return {
					content: [{ type: "text", text: `corral is not running (cannot reach ${socketPath}); stop not sent.` }],
				};
			}
			return { content: [{ type: "text", text: describeStopAck(status, dest) }] };
		},
	});

	// list_corral_agents: read-only capability roster. Ungated — any session is
	// messageable (operator approval may be asked). Every session is a
	// per-session entry addressable by sessionId; corral hides an unreachable
	// directory's cwd and description, and never a title or activity.
	pi.registerTool({
		name: "list_corral_agents",
		label: "List agents",
		description:
			"List the coding-agent sessions corral knows about, so you can choose whom to message " +
			"or which kind to spawn. You can message any of them via target_session (the operator " +
			"may be asked to approve if the directory pair is not whitelisted). Every session is an " +
			"entry with kind, sessionId and live; a session in a directory you may reach also carries " +
			"its cwd and description, an unreachable one hides both (so you learn a session exists and " +
			"can message it, without learning where it runs). Use an entry's sessionId as the " +
			"target_session for corral_message_agent. It never reveals a session's title or activity.",
		parameters: Type.Object({}),
		async execute(_id, _params, _signal, _onUpdate, ctx) {
			const home = process.env.HOME;
			const socketPath =
				process.env.CORRAL_CONTROL_SOCKET ?? (home ? path.join(home, ".corral", "corrald.sock") : undefined);
			if (!socketPath) {
				return { content: [{ type: "text", text: "corral: no HOME; cannot list agents" }] };
			}
			let reply: string;
			try {
				reply = await submitRawToCorral(socketPath, ctx.cwd, { op: "list", fromCwd: ctx.cwd });
			} catch {
				return {
					content: [{ type: "text", text: `corral is not running (cannot reach ${socketPath}).` }],
				};
			}
			return { content: [{ type: "text", text: reply }] };
		},
	});

	const stop = () => {
		// Idempotent: session_shutdown can fire more than once across
		// session replacement flows (/resume, /fork).
		for (const c of clients) c.destroy();
		clients.clear();
		server?.close();
		server = undefined;
		// A persisted session goes dormant (clear its socket, keep it resumable); a
		// session that never wrote its file has nothing to resume, so drop its whole
		// record instead of leaving a dormant card that resumes to `No session
		// found`. Done WITHOUT ctx: stop() can run during a session replacement
		// (resume/fork/reload), when the captured currentCtx is stale and touching
		// ctx.sessionManager throws — which would crash pi.
		if (everPersisted) {
			clearSocketInRegistry();
		} else {
			forgetRecordAndPointer();
		}
		if (socketPath) {
			fs.rmSync(socketPath, { force: true });
			socketPath = undefined;
		}
	};

	pi.on("session_start", async (_event, ctx) => {
		stop();
		currentCtx = ctx;
		currentModel = modelString(ctx);
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
			// Seed the model too, so a card shows it before the first change.
			if (currentModel) {
				const line = modelConfigLine();
				if (line && !conn.destroyed) conn.write(line);
			}
			// Seed the context info too, so a card shows it before the first turn.
			if (currentContext) {
				const line = contextUpdateLine();
				if (line && !conn.destroyed) conn.write(line);
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

	// The model changed (/model, cycle, restore): refresh clients and persist so
	// a dormant card shows the last-known model.
	pi.on("model_select", async (event, ctx) => {
		currentCtx = ctx;
		currentModel = `${event.model.provider}/${event.model.id}`;
		broadcastModel();
		if (currentCtx) writeRegistry(currentCtx, socketPath ?? null);
	});

	// --- outgoing: agent activity -> session/update broadcasts ---

	pi.on("turn_start", async (_event, ctx) => {
		currentState = "running";
		broadcastState();
		currentContext = contextInfo(ctx);
		broadcastContext();
	});

	pi.on("turn_end", async (_event, ctx) => {
		currentState = "idle";
		broadcastState();
		// The title may have become available this turn (the first user message
		// becomes the fallback title); push it to clients if it changed.
		broadcastTitleIfChanged();
		currentContext = contextInfo(ctx);
		broadcastContext();
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

	// The current model as "<provider>/<id>" from ctx.model, or undefined when pi
	// has not resolved one. corral shows it verbatim (never prettified).
	function modelString(ctx: ExtensionContext): string | undefined {
		const m = ctx.model;
		if (!m?.provider || !m?.id) return undefined;
		return `${m.provider}/${m.id}`;
	}

	// Broadcast the current model as an ACP Session Config Option (category
	// "model"). corral reads currentValue for display only; it never selects a
	// model, so the selectable options[] is omitted. Sent on model_select and as
	// a per-connection seed.
	function modelConfigLine(): string | undefined {
		if (!currentCtx || !currentModel) return undefined;
		return sessionUpdateLine({
			sessionUpdate: "config_options_update",
			configOptions: [
				{
					id: "model",
					name: "Model",
					category: "model",
					type: "select",
					currentValue: currentModel,
				},
			],
		});
	}

	function broadcastModel() {
		const line = modelConfigLine();
		if (!line) return;
		for (const c of clients) {
			if (!c.destroyed) c.write(line);
		}
	}

	// A compact age string: "8s" / "5m" / "2h" / "3d" — mirrors
	// core::engine::age_label's unit scale exactly (kept independent since the
	// two live in different languages and the arithmetic is trivial).
	function ageLabel(ms: number): string {
		const s = Math.floor(ms / 1000);
		if (s < 60) return `${s}s`;
		if (s < 3600) return `${Math.floor(s / 60)}m`;
		if (s < 86400) return `${Math.floor(s / 3600)}h`;
		return `${Math.floor(s / 86400)}d`;
	}

	// Entries count, context-window percent, and age, from pi's own
	// introspection APIs (docs/extensions.md: ctx.sessionManager.getEntries(),
	// ctx.getContextUsage()). undefined until the session has at least one
	// entry (a session_start-only context has nothing to size yet). age is
	// derived from the session file's own creation entry (session-format.md:
	// the first logged entry carries the session's creation timestamp), so it
	// stays correct across a resume without corral persisting its own
	// start-time field.
	function contextInfo(
		ctx: ExtensionContext,
	): { entries: number; percent: number | null; age: string } | undefined {
		const entries = ctx.sessionManager.getEntries() as Array<{ timestamp?: string }>;
		if (entries.length === 0) return undefined;
		const createdAt = Date.parse(entries[0]?.timestamp ?? "");
		if (Number.isNaN(createdAt)) return undefined;
		const usage = ctx.getContextUsage();
		return {
			entries: entries.length,
			percent: usage?.percent ?? null,
			age: ageLabel(Date.now() - createdAt),
		};
	}

	// Broadcast the current context info as corral-pi's own context_update
	// session/update (not an ACP-standard shape, same footing as state_update).
	// Sent on turn_start/turn_end and as a per-connection seed.
	function contextUpdateLine(): string | undefined {
		if (!currentCtx || !currentContext) return undefined;
		return sessionUpdateLine({
			sessionUpdate: "context_update",
			entries: currentContext.entries,
			...(currentContext.percent !== null ? { percent: currentContext.percent } : {}),
			age: currentContext.age,
		});
	}

	function broadcastContext() {
		const line = contextUpdateLine();
		if (!line) return;
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
					agentCapabilities: { loadSession: true },
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
			case "session/load": {
				if (!currentCtx) return fail(-32603, "no active session");
				const ctxAtRequest = currentCtx;
				// cwd/mcpServers from msg.params are intentionally ignored: this is
				// the one already-running in-process session replaying itself, not
				// a fresh session restore, so there is nothing to reconnect.
				(async () => {
					try {
						const entries = await ctxAtRequest.sessionManager.getEntries();
						for (const e of entries as Array<{
							type?: string;
							message?: { role?: string; content?: unknown };
						}>) {
							// Deliberate scope cut: only `type: "message"` entries
							// (role user/assistant) are replayed, not tool_call/other
							// SessionTreeEntry types (thinking_level_change,
							// model_change, compaction, branch_summary, custom, label,
							// session_info) -- no established ACP mapping for those, and
							// the feature's ask is message history, not a tool-call log
							// (YAGNI). Mirrors message_end's own role filter above.
							if (e.type !== "message") continue;
							const role = e.message?.role;
							if (role !== "user" && role !== "assistant") continue;
							const text = messageText(e.message as { content?: unknown });
							if (!text) continue;
							conn.write(
								sessionUpdateLine({
									sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk",
									content: { type: "text", text },
								}),
							);
						}
						reply(sessionInfo(ctxAtRequest));
					} catch (e) {
						fail(-32603, `session/load failed: ${e}`);
					}
				})();
				break;
			}
			default:
				fail(-32601, `method not supported by corral-pi: ${msg.method}`);
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

	// The raw pointer store ($CORRAL_INPUT_REGISTRY, else
	// $HOME/.corral/input/registry): a directory corrald scans, one file per
	// session named <sessionId> whose content is that session's cwd. The sandbox
	// grants write-only on ~/.corral/input, so we create/overwrite our own file
	// and never read the directory.
	function pointerDir(): string | undefined {
		if (process.env.CORRAL_INPUT_REGISTRY) return process.env.CORRAL_INPUT_REGISTRY;
		const home = process.env.HOME;
		return home ? path.join(home, ".corral", "input", "registry") : undefined;
	}

	// The per-project record store `<cwd>/.corral/registry/` (socket dir +
	// `/registry`), where this session writes `<sessionId>.json`. corrald reads
	// it and authenticates the record by this physical location.
	function recordDirFor(ctx: ExtensionContext): string {
		const corral = process.env.CORRAL_SOCKET_DIR ?? path.join(ctx.cwd, ".corral");
		return path.join(corral, "registry");
	}

	// Drop our pointer (write our own <sessionId> file, content = cwd). corrald
	// pre-creates the dir; we overwrite in place (write-only, no read). The
	// pointer persists across a clean shutdown so a dormant session stays
	// discoverable; only the board's `d` removes it. Best-effort.
	function writePointer(cwd: string, sessionId: string) {
		const dir = pointerDir();
		if (!dir) return;
		try {
			fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
			fs.writeFileSync(path.join(dir, sessionId), `${cwd}\n`);
		} catch {
			// Pointer store unreachable: announcing is best-effort.
		}
	}

	// Mark the session dormant by clearing the live socket, without any ctx:
	// rewrites the record file with `socket: null` (leaving a dormant, resumable
	// entry). Used by stop(), which may run when the captured ctx
	// is stale (session replacement), where touching ctx.sessionManager would
	// throw and crash pi. Best-effort.
	function clearSocketInRegistry() {
		if (!recordFile) return;
		try {
			const rec = JSON.parse(fs.readFileSync(recordFile, "utf8"));
			rec.socket = null;
			const tmp = `${recordFile}.${process.pid}.tmp`;
			fs.writeFileSync(tmp, JSON.stringify(rec, null, 2), { mode: 0o600 });
			fs.renameSync(tmp, recordFile);
		} catch {
			// Record already gone or unreadable: nothing to clear.
		}
	}

	// Drop a never-persisted session's record and pointer entirely (see stop):
	// with resumeCommand always set, an empty session must not linger as a
	// resumable dormant card. Derives the sessionId from the record filename, so
	// it needs no ctx (which may be stale). Best-effort.
	function forgetRecordAndPointer() {
		if (!recordFile) return;
		try {
			const sessionId = path.basename(recordFile, ".json");
			fs.rmSync(recordFile, { force: true });
			const dir = pointerDir();
			if (dir) fs.rmSync(path.join(dir, sessionId), { force: true });
		} catch {
			// Already gone or unreachable: nothing to forget.
		}
	}

	// Announce: write the record into this workdir's `.corral/registry/` and drop
	// our pointer in the store. corrald authenticates the record by where it
	// physically lives, so the record carries no `cwd` field (it cannot be
	// trusted to name its own directory).
	function writeRegistry(ctx: ExtensionContext, socket: string | null) {
		try {
		const sessionId = ctx.sessionManager.getSessionId();
		const recordDir = recordDirFor(ctx);
		fs.mkdirSync(recordDir, { recursive: true, mode: 0o700 });
		recordFile = path.join(recordDir, `${sessionId}.json`);
		// Launch commands corral runs verbatim (it never parses them): the argv
		// to spawn a fresh pi and to resume this exact session. This is where
		// pi's CLI grammar lives, so corral stays agent-neutral. A second agent
		// kind ships its own adapter writing its own argv.
		//
		// resumeCommand is a stable TEMPLATE: the literal `{sessionId}` token is
		// substituted by corral at launch (see CONVENTION.md). Writing the template
		// (not the concrete id) keeps the record shape identical for every session,
		// so the approved launch set never flaps and corrald stops re-prompting to
		// verify the pi harness. corral always resumes in the session's own cwd, so
		// pi's per-project `--session <id>` lookup resolves it.
		//
		// A never-persisted (empty) session must not leave a resumable dormant card
		// (it would resume to `No session found`). Since resumeCommand is now always
		// set, that concern moves to stop(): a session that never wrote its file has
		// its whole record dropped rather than left dormant. Track persistence here.
		const sessionFile = ctx.sessionManager.getSessionFile();
		const resumable = sessionFile != null && fs.existsSync(sessionFile);
		if (resumable) everPersisted = true;
		// A hidden spawn runs the agent inside a headless cage; corral sets
		// CORRAL_HIDDEN=1 in that environment. Record it so the board reveals
		// this session by resume instead of focusing a (non-existent) window.
		const hidden = process.env.CORRAL_HIDDEN === "1";
		const record = {
			sessionId,
			// No `cwd`: identity is the record's physical location, which corrald
			// derives; a self-reported cwd would be untrusted and is omitted.
			title: sessionTitle(ctx),
			// Agent kind, so corral can label a dormant card (no socket to parse).
			label: "pi",
			// One-line, human-readable kind description for the capability roster
			// a peer agent reads via list_corral_agents. Adapter-authored, not
			// model output.
			description: "pi: terminal TUI coding agent",
			socket,
			spawnCommand: ["pi"],
			resumeCommand: ["pi", "--session", "{sessionId}"],
			hidden,
			// Last-known model as "<provider>/<id>", so a dormant card shows it.
			// An undefined value is dropped by JSON.stringify, so no `model` key
			// is written when unknown (matching corral's Option<String> parse).
			model: currentModel,
			// Last-known context size/age, so a dormant card still shows it.
			// undefined fields are dropped by JSON.stringify (matching corral's
			// Option parse); percent is only included when known.
			entries: currentContext?.entries,
			contextPercent: currentContext?.percent ?? undefined,
			contextAge: currentContext?.age,
			lastSeen: new Date().toISOString(),
		};
		const tmp = `${recordFile}.${process.pid}.tmp`;
		fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
		fs.renameSync(tmp, recordFile);
		writePointer(ctx.cwd, sessionId);
		} catch {
			// A stale ctx (after session replacement/reload) throws on
			// sessionManager access; announcing is best-effort and must never
			// crash pi. Skip this update.
		}
	}

	function sessionInfo(ctx: ExtensionContext) {
		return {
			// The stable session UUID, matching the registry filename and the
			// reply handle stamped by corral_message_agent. Must NOT be the session
			// file path, or session-addressed routing (live_by_session) would
			// never match a live agent.
			sessionId: ctx.sessionManager.getSessionId(),
			title: sessionTitle(ctx),
			cwd: ctx.cwd,
		};
	}
}

// Submit a record over corral's control socket and resolve with the raw first
// reply line (a JSON document). Rejects on connect failure (corral down) or if
// no reply arrives within a short window (so the tool never hangs). Both the
// message tool and the roster query use it; each parses the line it expects.
// Where this session drops outbox request files: `<cwd>/.corral/outbox/`.
// corrald derives the trusted `fromCwd` from a request file's location, so the
// request need not (and cannot be trusted to) name its own directory.
function outboxDir(cwd: string): string {
	const corral = process.env.CORRAL_SOCKET_DIR ?? path.join(cwd, ".corral");
	return path.join(corral, "outbox");
}

// Submit a control request the authenticated way: write it to our outbox and
// send only `{"submit":"<path>"}` over the socket. corrald opens the file,
// derives `fromCwd` from its physical location, reads and deletes it, and
// replies with the ack line. `cwd` is this session's working directory.
function submitRawToCorral(
	socketPath: string,
	cwd: string,
	record: Record<string, unknown>,
): Promise<string> {
	return new Promise((resolve, reject) => {
		let file: string;
		try {
			const dir = outboxDir(cwd);
			fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
			file = path.join(dir, `${Date.now()}-${process.pid}-${Math.random().toString(36).slice(2)}.json`);
			fs.writeFileSync(file, JSON.stringify(record), { mode: 0o600 });
		} catch (e) {
			reject(e as Error);
			return;
		}
		const conn = net.createConnection(socketPath);
		let buf = "";
		let done = false;
		const finish = (fn: () => void) => {
			if (done) return;
			done = true;
			conn.destroy();
			fn();
		};
		conn.setTimeout(5000, () => finish(() => { try { fs.rmSync(file, { force: true }); } catch {} reject(new Error("timeout")); }));
		conn.on("connect", () => conn.write(`${JSON.stringify({ submit: file })}\n`));
		conn.on("data", (chunk) => {
			buf += chunk.toString("utf8");
			const nl = buf.indexOf("\n");
			if (nl < 0) return;
			const line = buf.slice(0, nl);
			finish(() => resolve(line));
		});
		conn.on("error", (e) => finish(() => { try { fs.rmSync(file, { force: true }); } catch {} reject(e); }));
		conn.on("close", () => finish(() => { try { fs.rmSync(file, { force: true }); } catch {} reject(new Error("closed before ack")); }));
	});
}

// Submit a message record and resolve with the one-word ack status corral
// returns (parsed from the raw reply line).
async function submitToCorral(socketPath: string, cwd: string, record: Record<string, unknown>): Promise<string> {
	const line = await submitRawToCorral(socketPath, cwd, record);
	try {
		return String((JSON.parse(line) as { status?: unknown }).status ?? "unknown");
	} catch {
		return "unknown";
	}
}

// Turn corral's ack for a stop into a message for the sending agent. Shares the
// wire vocabulary with a message ack, plus `already_stopped` (the idempotent
// no-op when the target was already dormant or gone).
function describeStopAck(status: string, dest: string): string {
	switch (status) {
		case "accepted":
			return `Stop accepted by corral (${dest}); the agent is being killed.`;
		case "approval_needed":
			return `Stop submitted (${dest}); operator approval needed, killed once approved.`;
		case "already_stopped":
			return `Already stopped: ${dest} was dormant or gone (no-op).`;
		case "recipient_not_found":
			return `Not stopped: no such session (${dest}).`;
		case "malformed":
			return "Not stopped: corral rejected the request as malformed.";
		default:
			return `corral responded: ${status}.`;
	}
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
