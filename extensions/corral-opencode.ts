/**
 * corral-opencode: make this opencode session discoverable and drivable by ACP
 * clients while the interactive TUI keeps running. The pi counterpart is
 * extensions/corral-pi.ts; this file mirrors it closely and deviates only
 * where opencode's plugin API forces it. It is the second worked adapter that
 * proves the corral convention (CONVENTION.md) is harness-neutral: corral needs
 * zero changes, since it reads the launch commands and label straight from the
 * registry record.
 *
 * Binds an ACP socket inside this session's own workdir at
 * <cwd>/.corral/opencode-<pid>.sock (override the dir with $CORRAL_SOCKET_DIR)
 * and writes its registry record inside this workdir at
 * <cwd>/.corral/registry/<sessionId>.json, appending this cwd to the
 * $HOME/.corral/input/registry/<sessionId> pointer file (content = this cwd;
 * override $CORRAL_INPUT_REGISTRY), so
 * corrald authenticates identity by where the record physically lives. On clean process
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
 * VERIFICATION: the plugin API surface is typechecked against
 * @opencode-ai/plugin@1.16.2 (matching the installed opencode) — the `Plugin`
 * signature, the `client.session.list/prompt/abort` calls, and every
 * `event.type` string against the SDK `Event` union. The `corral_message_agent`
 * tool is defined with a plain JSON-schema `args` (not the zod-based `tool()`
 * helper) so the plugin needs no runtime import from @opencode-ai/plugin, which
 * is unresolvable from the nix store path this plugin loads from. That check found
 * tool activity arrives as the dedicated `tool.execute.before/after` plugin
 * hooks (there is no `tool.*` event), so it is handled as hooks here, not in the
 * `event` switch. Still UNVERIFIED at runtime: opencode is a Bun-compiled
 * binary that SIGTRAPs under the Landlock sandbox, so a live load test (Bun
 * socket bind, real event payload field paths, message-part text extraction)
 * must run outside the sandbox. Every event field access stays guarded and all
 * bridge work is wrapped so the plugin never throws into opencode.
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
// Type-only import: erased at compile time. A runtime value import (e.g. the
// `tool` helper) would fail to resolve from the immutable nix store path this
// plugin loads from (no node_modules up-tree), and opencode drops the whole
// plugin on an unresolved import. So the tool below uses a plain JSON-schema
// definition instead of opencode's zod-based `tool()`/`tool.schema` builder.
import type { Plugin } from "@opencode-ai/plugin";

// Longest title corral should receive; an auto-generated title can be long.
const MAX_TITLE = 60;

export const CorralOpencode: Plugin = async ({ client, directory }) => {
	// A Bun.listen server (unix socket) plus the registry file it announces.
	let server: { stop: () => void } | undefined;
	let socketPath: string | undefined;
	// The REAL record file, in the session's own workdir
	// (`<cwd>/.corral/registry/<sessionId>.json`); corrald authenticates identity
	// by the record's physical location.
	let recordFile: string | undefined;
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
	// UNVERIFIED (no opencode toolchain here): the model is probed from the
	// assistant message metadata. Shape guarded so a miss just leaves the model
	// unreported (corral shows nothing), never throwing into the plugin host.
	// Format "<provider>/<id>", shown verbatim by corral.
	let currentModel: string | undefined;
	function setModelFrom(obj: unknown) {
		const o = (obj ?? {}) as {
			providerID?: string;
			modelID?: string;
			provider?: string;
			model?: string;
			info?: unknown;
		};
		const provider = o.providerID ?? o.provider;
		const model = o.modelID ?? o.model;
		if (provider && model) {
			const next = `${provider}/${model}`;
			if (next !== currentModel) {
				currentModel = next;
				broadcastModel();
			}
			return;
		}
		// The metadata often nests under `info` (message.updated); probe once.
		if (o.info && o.info !== obj) setModelFrom(o.info);
	}
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
							const line = modelConfigLine();
							if (line) sock.write(line);
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

	// Broadcast the current model as an ACP Session Config Option (category
	// "model"). corral reads currentValue for display only; options[] is omitted
	// (corral never selects a model). Mirrors the pi adapter.
	function modelConfigLine(): string | undefined {
		if (!activeSessionId || !currentModel) return undefined;
		return sessionUpdateLine({
			sessionUpdate: "config_options_update",
			configOptions: [
				{ id: "model", name: "Model", category: "model", type: "select", currentValue: currentModel },
			],
		});
	}

	function broadcastModel() {
		const line = modelConfigLine();
		if (!line) return;
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

	// Drop our pointer (write our own <sessionId> file, content = cwd). corrald
	// pre-creates the dir; we overwrite in place (write-only, no read). The
	// pointer persists across a clean shutdown; only the board's `d` removes it.
	// Best-effort.
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

	// Mark the session dormant by clearing the live socket: rewrite the record
	// file with `socket: null` (leaving a dormant, resumable entry). Used by
	// stop() on process exit. Best-effort.
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

	// Announce: write the record into this workdir's `.corral/registry/` and drop
	// our pointer in the store. corrald authenticates by physical location, so
	// the record carries no `cwd` field. No-op until the active session id is known.
	function writeRegistry() {
		try {
			if (!activeSessionId) return;
			const corral = process.env.CORRAL_SOCKET_DIR ?? path.join(activeCwd, ".corral");
			const recordDir = path.join(corral, "registry");
			fs.mkdirSync(recordDir, { recursive: true, mode: 0o700 });
			recordFile = path.join(recordDir, `${activeSessionId}.json`);
			// Launch commands: spawn a fresh opencode, and resume this exact session.
			// resumeCommand is a stable TEMPLATE whose literal `{sessionId}` token
			// corral substitutes at launch (see CONVENTION.md), so the record shape is
			// identical across sessions and the approved launch set never flaps.
			const record = {
				sessionId: activeSessionId,
				// No `cwd`: identity is the record's physical location (corrald derives it).
				title: activeTitle,
				// Agent kind, so corral can label a dormant card (no socket to parse).
				label: "opencode",
				// One-line kind description for the list_corral_agents roster.
				// Adapter-authored, not model output.
				description: "opencode: terminal coding agent",
				socket: socketPath ?? null,
				spawnCommand: ["opencode"],
				// opencode's TUI reads a trailing positional as a project path and
				// exits; its initial prompt rides a dedicated flag instead, so corral
				// delivers a launch message as `--prompt "<text>"` (opencode.ai/docs/cli).
				messageFlag: "--prompt",
				resumeCommand: ["opencode", "--session", "{sessionId}"],
				// A hidden spawn runs inside a headless cage; corral sets
				// CORRAL_HIDDEN=1 there. Record it so the board reveals by resume.
				hidden: process.env.CORRAL_HIDDEN === "1",
				// Last-known model as "<provider>/<id>" (undefined dropped by
				// JSON.stringify), so a dormant card shows it.
				model: currentModel,
				lastSeen: new Date().toISOString(),
			};
			const tmp = `${recordFile}.${process.pid}.tmp`;
			fs.writeFileSync(tmp, JSON.stringify(record, null, 2), { mode: 0o600 });
			fs.renameSync(tmp, recordFile);
			writePointer(activeCwd, activeSessionId);
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
						// Assistant message metadata carries the model; probe defensively.
						setModelFrom(props);
						break;
					case "message.part.updated":
						markRunning();
						setModelFrom(props);
						broadcastMessageActivity(props as never);
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

		// Tool activity is delivered by opencode as dedicated plugin hooks, NOT as
		// event-bus events (there is no `tool.*` entry in the SDK `Event` union),
		// so it is caught here, not in the `event` switch. `before` fires when a
		// tool call starts (name + callID + args), `after` when it finishes.
		"tool.execute.before": async (
			input: { tool: string; sessionID: string; callID: string },
			output: { args: unknown },
		) => {
			try {
				if (input.sessionID && input.sessionID !== activeSessionId) {
					activeSessionId = input.sessionID;
					writeRegistry();
					void refreshTitle();
				}
				markRunning();
				broadcast({
					sessionUpdate: "tool_call",
					toolCallId: input.callID,
					title: input.tool,
					status: "in_progress",
					rawInput: output.args,
				});
			} catch {}
		},
		"tool.execute.after": async (
			input: { tool: string; sessionID: string; callID: string },
			_output: { title: string; output: string; metadata: unknown },
		) => {
			// The after hook exposes no error flag (output is title/output/metadata),
			// so completion is reported without a failed variant.
			try {
				broadcast({ sessionUpdate: "tool_call_update", toolCallId: input.callID, status: "completed" });
			} catch {}
		},

		// corral_message_agent: hand a message to another session. It only submits
		// over ~/.corral/corrald.sock; corral (the unsandboxed board) is the trusted
		// cross-workdir router that authorizes, resolves the target, spawns/resumes
		// an agent, and injects with a provenance tag. Sandboxed agents cannot reach
		// each other directly, so this indirection is the only cross-session path.
		//
		// The tool is defined with a plain JSON-schema `args` (no zod / no `tool()`
		// wrapper) so the plugin needs no runtime import from @opencode-ai/plugin,
		// which is unresolvable from the nix store path (see the import note above).
		// opencode's plugin-tool loader accepts plain-object arg schemas directly.
		// Caveat: that path advertises every arg as `required`; the descriptions
		// tell the model to leave the unused addressing field empty, and execute()
		// treats an empty string as absent, so exactly-one-of still holds.
		tool: {
			corral_message_agent: {
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
				args: {
					target_session: {
						type: "string",
						description:
							"Reach this exact session id (resuming it if dormant). Use it to REPLY: it is the " +
							"<id> from the '[from <dir> (session <id>)]' tag on a message you received. " +
							"Give EITHER this OR target_dir; leave the other one empty (\"\").",
					},
					target_dir: {
						type: "string",
						description:
							"Absolute path: reach whoever works in this directory, starting a new agent there " +
							"if none is live. Use it to start a conversation when you have no session id yet. " +
							"Give EITHER this OR target_session; leave the other one empty (\"\").",
					},
					message: {
						type: "string",
						description: "The message text to deliver to the other agent.",
					},
					force_new: {
						type: "boolean",
						description:
							"With target_dir only: always start a dedicated fresh agent instead of reusing the " +
							"one already working there. Pass false when not needed.",
					},
					label: {
						type: "string",
						description:
							'With target_dir only: which agent kind to start if a fresh agent is spawned ' +
							'(e.g. "pi", "opencode"). Defaults to the kind already used in that directory; ' +
							'leave empty ("") to use the default.',
					},
					hidden: {
						type: "boolean",
						description:
							"Whether a newly spawned agent runs hidden (alive and working, but no window). " +
							"Defaults true, so an agent you summon never pops a window on the operator; they " +
							"reveal it from the board. Set false to request a visible window, which the operator " +
							"must approve. Ignored when the target agent is already running.",
					},
				},
				async execute(args: {
					target_dir?: string;
					target_session?: string;
					message: string;
					force_new?: boolean;
					label?: string;
					hidden?: boolean;
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
					// Optional: which agent kind to spawn if target_dir has no live agent.
					if (args.label) record.label = args.label;
					// Spawn visibility (default hidden); a visible spawn is operator-gated.
					if (typeof args.hidden === "boolean") record.hidden = args.hidden;
					const dest = hasDir ? args.target_dir : `session ${args.target_session}`;
					// A connect failure means corral is not running: fail loud rather
					// than silently queue undelivered.
					let status: string;
					try {
						status = await submitToCorral(controlSocket, activeCwd, record);
					} catch {
						return `corral is not running (cannot reach ${controlSocket}); message not sent.`;
					}
					return describeAck(status, String(dest));
				},
			},
			// list_corral_agents: read-only capability roster. Ungated — any session
			// is messageable (operator approval may be asked). Every session is a
			// per-session entry by sessionId; corral hides an unreachable directory's
			// cwd and description, and never a session's title or activity.
			// corral_stop_agent: kill a peer session's process (it goes dormant and
			// resumable). Gated exactly like corral_message_agent; an already-dormant
			// target is a no-op success. target_session only (stopping is precise).
			corral_stop_agent: {
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
				args: {
					target_session: {
						type: "string",
						description:
							"The exact session id to stop (kill its process). Take it from a message's " +
							"'[from <dir> (session <id>)]' tag or from list_corral_agents.",
					},
				},
				async execute(args: { target_session?: string }) {
					const home = process.env.HOME;
					const controlSocket =
						process.env.CORRAL_CONTROL_SOCKET ??
						(home ? path.join(home, ".corral", "corrald.sock") : undefined);
					if (!controlSocket) return "corral: no HOME; cannot stop agent";
					if (typeof args.target_session !== "string" || args.target_session.length === 0) {
						return "corral_stop_agent: target_session is required";
					}
					const record: Record<string, unknown> = {
						op: "stop",
						id: randomUUID(),
						fromCwd: activeCwd,
						fromSession: activeSessionId ?? "",
						targetSession: args.target_session,
						createdAt: new Date().toISOString(),
					};
					const dest = `session ${args.target_session}`;
					let status: string;
					try {
						status = await submitToCorral(controlSocket, activeCwd, record);
					} catch {
						return `corral is not running (cannot reach ${controlSocket}); stop not sent.`;
					}
					return describeStopAck(status, dest);
				},
			},
			list_corral_agents: {
				description:
					"List the coding-agent sessions corral knows about, so you can choose whom to message " +
					"or which kind to spawn. You can message any of them via target_session (the operator may " +
					"be asked to approve if the directory pair is not whitelisted). Every session is an entry " +
					"with kind, sessionId and live; a session in a directory you may reach also carries its " +
					"cwd and description, an unreachable one hides both. Use an entry's sessionId as " +
					"target_session for corral_message_agent. Never reveals a session's title or activity.",
				args: {},
				async execute() {
					const home = process.env.HOME;
					const controlSocket =
						process.env.CORRAL_CONTROL_SOCKET ??
						(home ? path.join(home, ".corral", "corrald.sock") : undefined);
					if (!controlSocket) return "corral: no HOME; cannot list agents";
					try {
						return await submitRawToCorral(controlSocket, activeCwd, { op: "list", fromCwd: activeCwd });
					} catch {
						return `corral is not running (cannot reach ${controlSocket}).`;
					}
				},
			},
		},
	};
};

export default CorralOpencode;

// Submit a record over corral's control socket and resolve with the raw first
// reply line (a JSON document). Rejects on connect failure (corral down) or if
// no reply arrives within a short window (so the tool never hangs). The message
// tool and the roster query share it; each parses the line it expects.
// Where this session drops outbox request files: `<cwd>/.corral/outbox/`.
// corrald derives the trusted `fromCwd` from a request file's location.
function outboxDir(cwd: string): string {
	const corral = process.env.CORRAL_SOCKET_DIR ?? path.join(cwd, ".corral");
	return path.join(corral, "outbox");
}

// Submit a control request the authenticated way: write it to our outbox and
// send only `{"submit":"<path>"}`. corrald derives `fromCwd` from the file's
// location, reads and deletes it, and replies with the ack line.
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

// Turn corral's ack for a stop into a message for the sending agent. Adds the
// `already_stopped` no-op to the shared message-ack vocabulary.
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
