# corral-claude

Make an interactive Claude Code session a first-class corral board citizen:
discoverable, focusable, resumable, live-state, and messageable — without
Claude Code having any in-process plugin runtime.

## Why this is shaped differently

The pi (`../corral-announce.ts`) and opencode (`../corral-opencode.ts`) adapters
run *inside* the session and serve ACP directly, because those harnesses load a
long-lived plugin that can hold a socket and inject a prompt. Claude Code cannot:
its hooks are subprocesses that exit, and its ACP mode is a separate headless
stdio server for an IDE, not the live terminal session. So corral-claude splits
in two:

- **`sidecar.ts`** — a resident process (one per session) that holds the ACP
  socket, keeps triage state, and queues messages. Spawned detached by the
  SessionStart hook; self-exits on SessionEnd or when the Claude process dies.
- **`hook.ts`** — a thin shim Claude runs for each hook event, bridging the
  event to the sidecar over a per-session control socket, and turning the
  sidecar's reply into hook output.

Two sockets under `<cwd>/.corral/`: `claude-<pid>.sock` (ACP, corral connects
here; pid is the interactive Claude process so focus correlation works) and
`.claude-ctl-<sessionId>.sock` (control, hook.ts connects here).

It ships as a Claude Code **plugin**: `.claude-plugin/plugin.json` plus
`hooks/hooks.json`, whose commands reference `${CLAUDE_PLUGIN_ROOT}` so nothing
is hardcoded and no `settings.json` is hand-edited.

## Live-session messaging: the two injection vectors

corral's `m` and inter-agent delivery reach the *live* session through Claude's
own hook feedback, deferred to a hook boundary (matching the fire-and-forget
contract):

- **Turn boundary** — the `Stop` hook returns `{"decision":"block","reason":…}`,
  which continues the conversation with the queued message as Claude's next
  instruction. Delivered when the current turn ends.
- **Idle** — an `asyncRewake` hook (async, on `Stop`) long-polls the sidecar; a
  message arriving while the session is idle makes it exit 2, which wakes Claude
  immediately with the message on stderr.

`state_update` is driven natively and is richer than pi's: `UserPromptSubmit` →
running, `Stop` → idle, and `Notification[permission_prompt]` →
**requires_action** (a real approval gate). `session/cancel` is a no-op: Claude
exposes no external turn-abort.

## Install

First install `bun` (the sidecar and shim run on it) and put it on PATH. Then
pick one path:

**Marketplace (updatable).** From a clone of the corral repo, or by URL:

```
claude plugin marketplace add cornerman/corral
claude plugin install corral-claude@corral
```

**Zero-install (skills-dir plugin).** Symlink this directory under your personal
skills dir; it loads as `corral-claude@skills-dir` on the next session with no
install step and no marketplace:

```
ln -s "$PWD/extensions/corral-claude" ~/.claude/skills/corral-claude
```

**Session-only.** `claude --plugin-dir extensions/corral-claude`.

Then start `claude` in any project; it appears on the corral board within ~1s.
Verify the plugin loaded with `claude plugin list` or `/plugin`, and inspect its
hooks with `claude plugin validate extensions/corral-claude`.

## Status: UNVERIFIED

No Claude Code binary or hook harness runs in corral's build sandbox, so the
hook payload field names and the injection semantics (Stop `decision:block`
reason as next instruction; `asyncRewake` exit-2 wake) are implemented from the
Claude Code hooks reference and probed defensively, not exercised end to end.
Known unknowns, to confirm against a live Claude:

- Whether `claude --resume <id> "message"` accepts a trailing prompt in
  interactive mode (used for launch-with-delivery to a dormant session).
- Exact `Notification` matcher values and `last_assistant_message` presence on
  `Stop`.
- That `asyncRewake` exit-2 reliably wakes a fully idle session in the terminal
  TUI (documented for background/agent views).

## Known limitations

- One session per working directory at a time (the control socket is keyed by
  session id, but focus/resume assume the common single-session case).
- Alternative lifecycle not taken: Claude's plugin **monitor** component could
  host the sidecar (lifetime = session, auto-reaped, no detach or liveness
  probe), but monitors are experimental, skipped on some hosts, and receive no
  session id, so the hook-spawned sidecar is the robust choice for now.
- The sidecar is a detached process; it is reaped on SessionEnd, on a Claude
  process-death probe (5s), and finally by corral's dead-socket sweep.
- Title updates after a `/rename` mid-session are not caught (no rename hook);
  the title comes from SessionStart and a first-prompt fallback.
