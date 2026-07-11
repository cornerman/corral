/**
 * corral-announce: make this pi session discoverable by corral.
 *
 * Binds an ACP socket at $XDG_RUNTIME_DIR/acp/pi-<pid>.sock while the
 * interactive session runs, so `corral` lists it next to agentwrap-hosted
 * sessions. Stage 1 scope: discovery + identity only (initialize and
 * session/list); prompting and update streaming come later.
 *
 * Install: symlink into ~/.pi/agent/extensions/ or run pi with
 *   pi -e /path/to/corral-announce.ts
 *
 * Multiple concurrent clients are fine: each connection gets its own
 * JSON-RPC read loop and every request is answered from current state.
 */

import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	let server: net.Server | undefined;
	let socketPath: string | undefined;

	const stop = () => {
		// Idempotent: session_shutdown can fire more than once across
		// session replacement flows (/resume, /fork).
		server?.close();
		server = undefined;
		if (socketPath) {
			fs.rmSync(socketPath, { force: true });
			socketPath = undefined;
		}
	};

	pi.on("session_start", async (_event, ctx) => {
		stop();
		const runtimeDir = process.env.XDG_RUNTIME_DIR;
		if (!runtimeDir) return; // no runtime dir, no discovery -- stay silent

		const dir = path.join(runtimeDir, "acp");
		// 0700: the socket grants prompt access to this session; directory
		// permissions are the only peer authentication we rely on.
		fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
		socketPath = path.join(dir, `pi-${process.pid}.sock`);
		fs.rmSync(socketPath, { force: true }); // stale leftover from a crashed pid reuse

		server = net.createServer((conn) => {
			let buf = "";
			conn.on("data", (chunk) => {
				buf += chunk.toString("utf8");
				let nl: number;
				while ((nl = buf.indexOf("\n")) >= 0) {
					const line = buf.slice(0, nl).trim();
					buf = buf.slice(nl + 1);
					if (line) handle(line, conn, ctx);
				}
			});
			conn.on("error", () => conn.destroy());
		});
		server.on("error", () => stop()); // e.g. EADDRINUSE: another announcer won
		server.listen(socketPath);
	});

	pi.on("session_shutdown", async () => stop());

	function handle(line: string, conn: net.Socket, ctx: ExtensionContext) {
		let msg: { id?: number | string; method?: string };
		try {
			msg = JSON.parse(line);
		} catch {
			return;
		}
		if (msg.id === undefined || !msg.method) return; // ignore notifications

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
				reply({ sessions: [sessionInfo(ctx)] });
				break;
			default:
				fail(-32601, `method not supported by corral-announce: ${msg.method}`);
		}
	}

	function sessionInfo(ctx: ExtensionContext) {
		return {
			sessionId: ctx.sessionManager.getSessionFile() ?? `ephemeral-${process.pid}`,
			title: pi.getSessionName() ?? null,
			cwd: ctx.cwd,
		};
	}
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
