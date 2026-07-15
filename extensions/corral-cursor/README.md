# corral-cursor

A corral adapter for the **Cursor desktop IDE** (Electron). It makes an open
Cursor window discoverable, focusable, resumable, and best-effort messageable
from a corral board. Cursor is the first GUI-editor adapter and the first that
ships as a **VS Code extension (VSIX)** rather than an in-session plugin or a
sidecar.

## How It Works

Cursor exposes no API to observe or drive its Composer agent, and its hooks
cannot inject into a running session. But the extension host is a resident,
in-process runtime that can hold a socket. So corral-cursor splits its work by
what each layer can reach:

- **`extension.js`** — the resident owner, running in the extension host. On
  activation it resolves the Cursor window's Electron pid, binds a workdir-local
  ACP socket named with that pid, writes the `gui: true` registry record, serves
  the ACP surface, and (best-effort) opens a new pre-filled Composer chat for
  `session/prompt`. Because the host is resident, there is **no sidecar** (unlike
  `corral-claude`).
- **`state-hook.js`** — a thin shim that Cursor runs per hook event. It maps the
  event to `running`/`idle` and pings the extension over the window's control
  socket. Registered via an **auto-merged `~/.cursor/hooks.json`** (the extension
  writes it on activation, additively, never clobbering your existing hooks).
- **`lib.js`** — the pure, unit-tested core (path/record builders, ACP reply
  dispatch, Electron-pid walk, hooks merge, event→state). Run `node --test
  extensions/corral-cursor/lib.test.js`.

### Sockets and Record

```
<cwd>/.corral/cursor-<electronPid>.sock     ACP surface corral connects to.
                                            The pid is the Cursor window-owning
                                            (Electron main) process, so corral's
                                            gui focus raises the real window.
<cwd>/.corral/.cursor-ctl-<sessionId>.sock  control channel the state-hook pings.
$HOME/.corral/registry/<sessionId>.json     gui:true record; socket cleared to
                                            null on deactivate (dormant, resumable).
```

The record carries `label: "cursor"`, `gui: true`, and
`spawnCommand`/`resumeCommand` = `["cursor", "<cwd>"]`. For a GUI editor,
"resume" is just reopening the workspace folder; Cursor restores its chat
history for that folder.

### ACP Surface

- `initialize` — identity (`agentInfo.name` = `"cursor"`).
- `session/list` — this window's single session (id, title, cwd).
- `session/prompt` — opens a **new** Composer chat pre-filled with the message
  (via `vscode.commands.executeCommand` candidates or the prompt deeplink),
  rather than intruding on the open chat. **UNVERIFIED** (see Status).
- `session/cancel` — no-op (no external turn-abort).
- Broadcasts `state_update` (`running`/`idle`) and `session_info_update`.

## Install

Requires **`node` on PATH** (the hook runs `node state-hook.js`; same
requirement and rationale as `corral-claude`).

- Package a VSIX and install it: `cursor --install-extension corral-cursor.vsix`
  (build the `.vsix` with `vsce package` from this directory), or
- copy this folder into `~/.cursor/extensions/corral-cursor/`, or
- for development, launch Cursor with
  `--extensionDevelopmentPath=<path to this folder>`.

On first activation the extension merges its state-hook into
`~/.cursor/hooks.json`. No manual hooks setup.

### Injection command override

The Composer prompt-submit command id is undocumented and may change across
Cursor versions. If messaging does not open a chat, set
`corral.cursor.injectCommand` (Settings → corral) to the correct command id.

## Known Limitations

- **Multiple Cursor windows share one Electron pid**, so focus raises one Cursor
  window, not guaranteed the exact one. Precise per-window focus needs
  title/class matching in corral's `focus.rs` (deferred).
- **One card per window (workspace)**, not per Composer chat — a chat can be
  neither focused nor resumed independently of its window.
- **State is coarse** (`running`/`idle` only; Cursor exposes no permission hook,
  so no `requires_action`).
- **Messaging is best-effort and UNVERIFIED**; if no injection path works, the
  card degrades to discover/watch/focus/resume.
- **Dormant delivery drops the message text** (reopen only): `cursor <dir>`
  cannot carry a prompt. A future generic `messageUriTemplate` (prompt deeplink)
  could pre-fill it.

## TODO / Future

- **Agent-initiated messaging (send side) is not implemented.** This adapter only
  receives (corral -> Cursor). Unlike `corral-pi` / `corral-opencode` /
  `corral-claude`, it does not yet register a `corral_message_agent` tool
  (Appendix A) letting the Cursor agent message other agents. Two candidate
  routes when picked up: an MCP stdio server auto-registered in
  `~/.cursor/mcp.json` (the robust, Cursor-documented path), or a small
  `corral-msg` CLI plus a Cursor rule that invokes it via the agent's shell tool
  (no MCP, but no clean global rule-install and prose-driven invocation). The
  `vscode.lm.registerTool` API is not a route — Cursor's Composer does not
  consume VS Code LM tools.

## Status: UNVERIFIED

No Cursor runtime is available in this repo, so the Cursor-specific pieces are
coded defensively from the docs and not exercised: the Composer inject command
id(s), the Electron-pid resolution (extension-host → main-process walk and that
`_NET_WM_PID` equals it), and the hook payload field names. Every `vscode` and
Cursor access is guarded; the extension never throws into the host. The pure
`lib.js` core is fully unit-tested.
