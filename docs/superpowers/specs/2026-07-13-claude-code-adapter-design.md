# Claude Code Announce Adapter — Design Note

Status: DRAFT, paused mid-design (2026-07-13). Captures the analysis for
build-order item 2 (prove cross-harness with a second adapter). Open questions
at the end are unresolved; do not implement until they are settled.

## Goal

A second adapter beside `corral-announce.ts` that makes an interactive Claude
Code session a first-class corral citizen, implementing `CONVENTION.md`. Two
harnesses on one board is the demonstration that turns the convention from
"documented" into "adopted", and the first real test that CONVENTION.md is
implementable from the doc alone.

## Rejected: stdio-to-socket wrapper

The vision floated a generic stdio-to-socket wrapper for non-cooperating
agents. It is a dead end for an *interactive* harness:

- Piping stdio strips the pty (raw mode, SIGWINCH/resize, colors) and breaks the
  TUI; full pty passthrough (`script`/`tmux`-style) preserves the terminal but
  only yields a raw ANSI byte stream.
- The convention needs *semantic* events (turn start/end, `requires_action`, a
  clean `session/prompt`). Those cannot be reconstructed from screen bytes
  without brittle ANSI screen-scraping.

So the wrapper buys terminal capture at the cost of interactivity and still
cannot produce the signals that matter.

## Approach: Claude Code hooks as the side channel

Claude Code hooks are the side-channel equivalent of pi's in-process extension:
they fire out-of-band on lifecycle events while the TUI keeps its own stdio
(interactivity preserved), and they deliver semantic events, not screen bytes.
Every hook receives JSON on stdin with `session_id`, `cwd`, `hook_event_name`,
`permission_mode`, plus event-specific fields (`prompt`, `tool_name`, …).

Hooks are fire-and-exit shell commands, so they cannot themselves hold a socket
open. The adapter is therefore two pieces:

1. **Detached sidecar, one per session.** `SessionStart` launches it
   (`setsid --fork`, the same detach trick corral uses for spawned windows). It
   binds `<cwd>/.corral/claude-<pid>.sock`, serves `initialize` and
   `session/list`, holds client connections, and broadcasts `state_update`
   (seeding each new client on connect). It is the long-lived ACP server the
   hooks feed.
2. **Thin hook shims.** Each reads the stdin JSON, derives a state / title /
   activity, and pushes it to the sidecar over a control line. Installed in
   Claude Code `settings.json` under the `hooks` key.

This splits what pi does in one in-process extension into fire-and-exit hooks
plus a separately-spawned sidecar — more moving parts, same contract.

## Event mapping (Claude Code hook -> convention action)

| Claude hook                         | Convention action |
|-------------------------------------|-------------------|
| `SessionStart`                      | write registry record; spawn sidecar; bind socket |
| `UserPromptSubmit`                  | `state_update: running`; capture first prompt as fallback `title` |
| `Stop`                              | `state_update: idle` |
| `Notification` (permission / input) | `state_update: requires_action` |
| `PostToolUse`                       | activity line (MAY) |
| `SessionEnd`                        | set record `socket: null`; unlink socket; sidecar exits |

A crash (no `SessionEnd`) leaves a stale, non-null `socket`; corral's existing
dead-socket detection treats that record as dormant (CONVENTION.md §6,
consumer-side). No extra work needed.

Registry record (§2) writes trivially since every hook carries `session_id` and
`cwd`. Fallback title mirrors pi: first user prompt, capped.

## The one real gap: `session/prompt` to a live session

No documented way exists for an external process to inject a user turn into an
already-running interactive Claude Code TUI. This splits cleanly:

- **Cold delivery works.** Claude's CLI takes an initial prompt
  (`claude "message"`), so spawn/resume-with-message (the launch-with-message
  path already built) delivers to a not-yet-live Claude target via a
  `ClaudeLauncher` behind the existing `Launcher` seam. Harness-specific,
  supported.
- **Live delivery does not.** Messaging an already-open Claude window over the
  socket would need TUI injection we do not have.

`session/prompt` is only SHOULD in CONVENTION.md, so a Claude adapter with the
full MUST set (discover, triage, focus, resume, cold-deliver) is conformant and
a real cross-harness proof. Live-injection is a documented limitation, not a
blocker.

## Bonus insight: the generic announce sidecar

The sidecar can be harness-agnostic — a generic announce sidecar fed by any
harness's hook/event API. It is the event-based successor to the rejected stdio
wrapper: side-channel instead of stdio, semantic events instead of
screen-scraping. Claude Code is just the first hook source; another harness with
a hook/event API drops in by writing shims that feed the same sidecar.

## Open questions (unresolved — resume here)

1. **v1 scope.** Full MUST set + cold delivery + documented live-injection gap
   (leaning this way) vs. read-only citizen (no delivery at all) vs. investigate
   live-injection harder first (tty write / undocumented API).
2. **Sidecar implementation.** New Rust crate in this repo (typed, tested,
   reusable as the generic sidecar; hooks are tiny shims) vs. standalone
   TypeScript mirroring `corral-announce.ts` vs. minimal shell + socat glue.
3. **Sidecar control channel.** How a fire-and-exit hook feeds the running
   sidecar: a second control line on the same socket, a separate control
   socket, or a state file the sidecar watches. (Not yet designed.)
4. **Verification without a live Claude Code.** How to test the adapter end to
   end if Claude Code is not installed in the dev environment.

## References (non-normative)

- `CONVENTION.md` — the contract being implemented.
- `extensions/corral-announce.ts` — the pi reference adapter.
- Claude Code hooks reference: code.claude.com/docs/en/hooks (event list,
  stdin JSON schema, exit-code control contract).
