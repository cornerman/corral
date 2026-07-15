# corral-cursor: A Corral Adapter for the Cursor Desktop IDE

Design spec — 2026-07-15

## Goal

Make an interactive Cursor desktop (Electron IDE) window discoverable,
triageable, focusable, and — best-effort — messageable from a corral board, the
same way `corral-pi`, `corral-opencode`, and `corral-claude` expose their
harnesses. Cursor is the first GUI editor adapter, so it rides the existing
`gui: true` registry path (as quine does) rather than a terminal-wrapped record.

Target the **Cursor desktop GUI**, not the `cursor-agent` CLI. The CLI is a
clean future fit (it even serves native ACP via `cursor-agent acp`), but the
GUI is what people actually run, so it comes first.

## Constraints That Shape the Design

These are Cursor-platform facts, established from Cursor's docs and community
reports (no Cursor binary is available in this repo — see UNVERIFIED below):

1. **No public API observes or drives Cursor's agent.** The VS Code extension
   API (including the Chat / Language Model APIs) does not expose Cursor's
   proprietary Composer turn state, and offers no supported call to submit a
   prompt into it. Driving Composer means undocumented, reverse-engineered
   command IDs.
2. **Agent lifecycle is observable only via `.cursor/hooks.json` hooks**
   (`beforeSubmitPrompt`, `beforeShellExecution`, `afterShellExecution`,
   `afterFileEdit`, `stop`). There is no session-start/-end hook and no
   permission/notification hook.
3. **Hooks cannot inject.** A hook is a short-lived subprocess spawned beside
   the editor; it can observe and gate, not push text into Composer. Injection
   lives only inside the extension host (`vscode.commands.executeCommand`).
4. **The extension host is a resident in-process runtime.** Unlike Claude Code
   (which has none, forcing the `corral-claude` sidecar), a Cursor VSIX
   extension can hold a unix socket for the window's whole lifetime. So the
   extension *replaces* the sidecar; there is no separate sidecar process.
5. **`gui` focus matches strictly on the socket-filename pid.** `focus.rs`
   `match_pids` does not parent-walk for a `gui: true` record; it matches
   `_NET_WM_PID == agent.pid` where `agent.pid` is parsed from
   `<label>-<pid>.sock`. So the socket MUST be named with Cursor's Electron
   window-owning pid.

These facts force the architecture: **a VSIX extension (resident owner:
socket + registry + ACP + injection) plus a tiny `.cursor/hooks.json` shim that
feeds turn state to the extension.**

## Architecture

```
Cursor desktop window (one workspace)
  corral-cursor VSIX extension  (runs in the extension host, resident)
    | on activate:
    |   resolve Electron window pid (parent-walk from extension host)
    |   bind  <cwd>/.corral/cursor-<electronPid>.sock   (ACP surface)
    |   bind  <cwd>/.corral/.cursor-ctl-<sessionId>.sock (state channel)
    |   write ~/.corral/registry/<sessionId>.json  (gui: true)
    |   merge ~/.cursor/hooks.json to point at bundled state-hook.js
    | serves ACP to corral:
    |   initialize, session/list,
    |   session/prompt  -> executeCommand(<composer inject>, text)  [UNVERIFIED]
    |   session/cancel  -> best-effort no-op
    | broadcasts: state_update (from hooks), session_info_update
    | on deactivate / window close: clear socket:null, unlink (best-effort)
    ^
    | one line per hook event (state only)
  state-hook.js  (bundled; ~/.cursor/hooks.json runs it per event)
    beforeSubmitPrompt -> running ;  stop -> idle
    (reads event JSON on stdin, connects to .cursor-ctl-<id>.sock, exits)

corral (board)  ── gui focus by Electron pid ──> raises the Cursor window
                └─ spawn/resume = `cursor <dir>` (reopens the workspace)
```

### Components (each with one purpose)

- **`extension.ts`** — the resident owner. Everything stateful lives here:
  socket binding, registry record, ACP request handling, injection, state
  broadcast. Depends on: the `vscode` API, Node `net`/`fs`, the registry/socket
  paths (§ convention). Testable in isolation only partially (the `vscode`
  surface is a Cursor runtime); pure helpers (pid resolution, record shape,
  ACP line framing) are factored out and unit-testable with plain Node.
- **`state-hook.js`** — the thin shim. One purpose: translate a hook event on
  stdin into a state ping on the window's control socket, then exit. No
  injection, no spawning (the extension is already resident), no long-poll
  doorbell. Simpler than `corral-claude`'s `hook.ts`.
- **`hooks.json` (template)** — merged into `~/.cursor/hooks.json` by the
  extension on activation, pointing `command` at the bundled `state-hook.js`
  absolute path (from `context.extensionPath`), so no manual edit and no
  hardcoded path. The merge is additive and MUST NOT clobber existing hooks.
- **`package.json`** — the VSIX manifest (activation on startup /
  `onStartupFinished`, so the window announces without waiting for a first
  prompt).
- **`README.md`** — install (VSIX), the node-on-PATH requirement for the hook,
  the injection-command config, and the documented limitations.

## Registry Record

A `gui: true` record, identical shape to the convention (§2), values:

```json
{
  "sessionId": "<uuid persisted in workspaceState>",
  "cwd": "<workspace folder absolute path>",
  "title": "<folder basename, or first prompt text as fallback>",
  "label": "cursor",
  "socket": "<cwd>/.corral/cursor-<electronPid>.sock",
  "gui": true,
  "spawnCommand": ["cursor", "<cwd>"],
  "resumeCommand": ["cursor", "<cwd>"],
  "lastSeen": "<ISO-8601, refreshed on hook events>"
}
```

Decisions:

- **`gui: true`** — corral launches `spawnCommand`/`resumeCommand` directly (no
  terminal wrapper) and focuses by the socket pid, exactly the quine path.
- **`sessionId`** — a UUID generated once and persisted in the extension's
  `context.workspaceState`, so reopening the same folder reactivates to the
  same identity (and the same registry record). Robust across reopens and
  unique per workspace; avoids collisions a path-hash id would risk.
- **`spawnCommand` == `resumeCommand` == `["cursor", "<cwd>"]`** — for a GUI
  editor "resume" is just reopening the workspace folder. There is no distinct
  fresh-vs-resume launch; both open the folder. Cursor's own
  single-instance behavior then focuses/opens that window.
- **No `messageFlag`** — a prompt-carry mechanism does exist (Cursor prompt
  deeplinks, `cursor://…/prompt?text=<urlencoded>`, which pre-fill a chat for
  the user to confirm), but it needs the message URL-encoded *inside a URI*,
  which neither a trailing positional nor `messageFlag`'s plain-text value fits,
  and corral builds no agent-specific launch strings. So dormant
  launch-with-message delivers no text in v1 (see Messaging and Future).

## ACP Surface

Served by the extension over `cursor-<electronPid>.sock` (newline-delimited
JSON-RPC 2.0, multi-client, seed state on connect):

- **`initialize`** — `agentInfo.name = "cursor"`, `agentCapabilities:
  { loadSession: false }`, `authMethods: []`.
- **`session/list`** — one session: `{ sessionId, title, cwd }`.
- **`session/prompt`** — open a **new** Composer chat pre-filled with the
  message text (via `vscode.commands.executeCommand` or the prompt deeplink),
  rather than intruding on whatever chat is currently open. A prompt must land
  in a chat (there is no window-level prompt), and starting a fresh chat removes
  the "which chat?" ambiguity and matches Cursor's own deeplink behavior.
  **UNVERIFIED** (see below). On success, respond `{ stopReason: "end_turn" }`;
  on failure, JSON-RPC error so the board surfaces "delivery not available"
  rather than silently dropping.
- **`session/cancel`** — best-effort no-op (no external turn-abort exposed),
  answered as a notification, documented.
- Any other method — JSON-RPC `-32601`.

Broadcasts:

- **`state_update`** (`running` / `idle`) — driven by hook events. No
  `requires_action` (no permission hook is exposed to us; same class of
  limitation as pi). Seeded to each new client on connect (MUST, §5).
- **`session_info_update`** — on title change (folder name is stable; the
  first-prompt fallback title fires once).

## Focus, Spawn, Resume

- **Focus** — corral's `gui` path matches `_NET_WM_PID == socket pid`. The
  extension resolves the Electron window-owning pid by walking `/proc` up from
  the extension-host pid to the outermost ancestor whose `comm` looks like the
  Cursor/Electron main process, and names the ACP socket with it. This is the
  single load-bearing correlation and is **UNVERIFIED**.
- **Spawn / resume** — `cursor <cwd>` opens the workspace. Because Cursor is
  single-instance, a corral-launched `setsid --fork cursor <dir>` may just open
  a window in the already-running app; the extension re-resolves the real main
  pid on activation, so the socket name stays correct regardless of who
  launched the window.

## Messaging (the "B" goal)

Two best-effort paths, both accepted as UNVERIFIED / half-measures:

1. **Live injection (primary).** The extension's `session/prompt` handler opens
   a new Composer chat pre-filled with the text, via
   `vscode.commands.executeCommand` with a Composer command or the prompt
   deeplink. The exact command ID is undocumented; the extension tries a small
   candidate list and honors a `corral.cursor.injectCommand` setting to
   override. If none succeed, `session/prompt` fails loud (JSON-RPC error).
   Heavily flagged UNVERIFIED; may break on Cursor updates.
2. **Dormant delivery (fallback).** Reopening via `cursor <dir>` raises the
   window but does not carry the message text in v1 (see the `messageFlag`
   decision and Future). So a dormant Cursor card's message reopens the window
   without the text — a documented half-measure (the operator then types it).

This mirrors how `corral-claude` treats its own unverified injection: coded
defensively, guarded, never crashing the host, and clearly marked in-file.

## Corral Core Changes

**None expected.** `corral-cursor` is a pure new adapter, like
`corral-opencode`. It relies only on the existing `gui: true` machinery
(`launch.rs` direct launch, `focus.rs` `match_pids` strict-pid) and the
existing ACP watch path. If implementation uncovers a gap, that is flagged
back, not silently patched.

## Distribution

- Ship as a **VSIX extension** installed into Cursor (`cursor
  --install-extension`, OpenVSX, or a `--extensionDevelopmentPath` for dev).
- On activation the extension writes/merges `~/.cursor/hooks.json` to register
  `state-hook.js` by absolute path, so there is no manual hooks setup.
- The hook runs `node <abs>/state-hook.js`; **`node` must be on PATH** (same
  requirement and rationale as `corral-claude`). Documented in the README.

## Testing

- **Pure helpers** (unit-tested with plain Node, no Cursor): registry record
  construction, ACP line framing/parsing, the `/proc` pid parent-walk (against
  synthetic `/proc`-like input), the `hooks.json` additive merge.
- **The `vscode`-coupled surface** (activation, `executeCommand` injection,
  window pid resolution against a real Electron tree) cannot be exercised
  without a Cursor runtime and is marked UNVERIFIED, matching the
  `corral-claude` precedent. Every field access is guarded; the extension never
  throws into Cursor.
- **Convention conformance** is checked against `CONVENTION.md` §8 by
  inspection (the record + ACP surface match the other adapters).

## UNVERIFIED (no Cursor in this repo)

- The Composer prompt-submit command ID(s) for injection.
- The Electron window pid resolution (extension-host → main-process walk) and
  that `_NET_WM_PID` reports that pid.
- Hook payload field names (`beforeSubmitPrompt` / `stop` stdin JSON) and the
  cwd/session correlation carried in them.
- Whether merging `~/.cursor/hooks.json` at the user level is honored for the
  workspace, and hook trust prompts.

## Known Limitations (v1, deliberate)

- **Multiple Cursor windows share one Electron pid**, so `_NET_WM_PID` is the
  same for all of them; focus raises one Cursor window, not guaranteed the
  exact one. Precise per-window focus needs title/class matching in `focus.rs`
  (a core change), deferred.
- **One card per window (workspace)**, not per Composer chat. Not because chats
  are invisible (hook payloads likely carry a conversation id) but because they
  are **not actionable**: corral's focus raises an OS window and its resume
  reopens a folder, so a chat can be neither focused nor resumed independently
  of its window. The convention's `session/list` is plural, but corral is
  one-session-per-socket today (`watch.rs` indexes `sessions[0]`; a card is keyed
  by socket path), so per-chat cards would need a cross-cutting core change and
  would still all focus/resume the same window/folder. Per-chat state, if the
  hooks expose it, enriches the window card as activity lines. See Future.
- **State is coarse** (`running`/`idle` only, no `requires_action`), bounded by
  the hook lifecycle Cursor exposes.
- **Messaging is best-effort and UNVERIFIED**; it may not work at all against a
  given Cursor version, in which case the card degrades to
  discover/watch/focus/resume (the "A" landing).
- **Dormant delivery drops the message text** (reopen only).

## Future

- **Agent-initiated messaging (`corral_message_agent`, Appendix A) — TODO.** v1
  is receive-only (corral -> Cursor); the Cursor agent cannot yet message other
  agents as pi/opencode/claude can. Routes when picked up: an MCP stdio server
  auto-registered in `~/.cursor/mcp.json` (robust, Cursor-documented), or a
  `corral-msg` CLI + a Cursor rule invoking it via the agent's shell tool (no
  MCP, but no clean global rule-install and prose-driven invocation).
  `vscode.lm.registerTool` is not a route (Cursor's Composer ignores VS Code LM
  tools). Left out of v1 by choice.

- When the `cursor-agent` CLI is in play, a second, cleaner adapter can serve
  the convention over `cursor-agent acp` (native ACP), with real injection and
  a terminal window to focus — the terminal-agent shape (twin of the other CLI
  adapters).
- Title/class-based focus in `focus.rs` to disambiguate multiple Electron
  windows of the same app (benefits any GUI editor adapter, not just Cursor).
- Multi-session-per-socket in core: finish the plural-`sessions` affordance by
  re-keying a card from socket path to `(socket, session_id)` and fanning the
  `session/list` array into multiple cards. Cross-cutting (touches
  `watch.rs`/`model.rs`/`nav.rs`/`focus.rs`) and also unlocks opencode's
  deferred multi-session. Deferred; for Cursor it gains per-chat state only,
  since focus/resume stay window/folder-scoped.
- A generic `messageUriTemplate` record field: an agent declares a prompt-URI
  template (e.g. `cursor://…/prompt?text={msg}`), and corral URL-encodes the
  message into it and launches it (via `cursor --open-url` / `xdg-open`), giving
  dormant Cursor delivery a pre-filled prompt (user confirms). Generic across
  any agent with a prompt-URI scheme; a `CONVENTION.md` + `launch.rs` change,
  deferred out of v1.
