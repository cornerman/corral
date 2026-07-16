# corral_stop_agent — Design

## Summary

A new agent-facing tool, `corral_stop_agent`, lets one agent stop another
agent's process over the `corrald` control socket. Stopping kills the target's
process, so its card goes Dormant and stays resumable — the same effect as the
operator's `d` on the board, reached through the daemon instead of the board.

It is gated exactly like `corral_message_agent`: the `(sender-dir →
target-dir)` whitelist authorizes it, and an unwhitelisted pair prompts the
operator on the tray or a desktop notification. corrald tracks no parentage
(none exists today), so the messaging gate is the whole authorization story.

## Goals

- An agent can stop a peer it knows by session id.
- Destruction stays governed: an unwhitelisted stop needs the operator, and the
  operator sees it is a kill, not a message.
- No new authorization machinery: reuse the message whitelist + approval gate.
- Idempotent: stopping an already-stopped target succeeds silently.

## Non-Goals

- No `target_dir` stop (kill-whoever-works-there is ambiguous; killing is
  precise).
- No cancel-the-turn variant (`session/cancel`); stop means kill the process.
- No parentage tracking (rejected as too much machinery for this).
- No board/GUI change: this is a daemon + adapter feature.

## Behavior

**Addressing.** `target_session` only — one exact agent by its session id (the
reply handle from a provenance tag, or from `list_corral_agents`).

**Authorization.** The `(sender-dir → target-dir)` whitelist authorizes a stop,
where the target dir is the target session's cwd (same resolution as a
session-addressed message). A whitelisted pair stops straight through; an
unwhitelisted pair is held for the operator (tray Allow once / Allow always /
Deny, plus the notification mirror), acked `approval_needed` at once and killed
after approval. The whitelist authorizes kills too (a whitelisted pair already
trusts each other); it is not force-gated like a `hidden:false` visible spawn.

**Idempotence.** Stopping a target that is already dormant (registry `socket`
cleared), or whose process is gone by routing time, is a no-op success, not an
error.

**Effect.** corrald kills the target's process by the pid parsed from its
socket filename (`<label>-<pid>.sock`). The adapter's clean-shutdown path (or
corral's dead-socket sweep) then clears the record's `socket`, leaving a
dormant, resumable entry — identical to the board's `d`.

## Ack Vocabulary

Reuses the existing acks plus one new variant:

- `recipient_not_found` — no registry record for that session id.
- `already_stopped` — record exists but is dormant, or the process is gone.
  Synchronous, not routed. New `Ack` variant, `routable = false`.
- `approval_needed` — live target, pair not whitelisted; held for the operator.
- `accepted` — live target, pair whitelisted; enqueued for the kill.

`directory_not_known` cannot occur (no `target_dir`).

## Wire Protocol

A stop submission is one JSON line over `corrald.sock`, distinguished by
`op`:

```json
{"op":"stop","id":"<uuid>","fromCwd":"<abs>","fromSession":"<sid>","targetSession":"<sid>"}
```

`op:"list"` (roster) and a message (no `op`) are the existing shapes;
`op:"stop"` is the third. The tool reports corrald's ack string verbatim.

## Components

### Adapters (`extensions/`)

Register `corral_stop_agent({ target_session })` beside `corral_message_agent`
in all four adapters — `corral-pi.ts`, `corral-opencode.ts`,
`corral-claude/`, `corral-cursor/`. It submits the `op:"stop"` line and reports
the ack. `corral-pi.ts` is authored first; the other three mirror it
mechanically (they already mirror the message tool and stay UNVERIFIED in this
repo). The tool description states it stops a peer by session id, that stopping
kills the process (leaving it resumable), and that an unwhitelisted stop needs
operator approval.

### `crates/daemon/src/mailbox.rs`

- The routed item gains `action: Action` where `Action = Deliver | Stop`. The
  gate, queue, pending, and whitelist logic are action-agnostic, so `poll` /
  `apply` / `is_whitelisted` are untouched. A Stop item carries `target =
  Session`, an empty message body, and never a charter.
- `parse_stop(text) -> Option<Message>` (or the shared item type) builds a Stop
  item from the `op:"stop"` line.
- `Ack::AlreadyStopped` (wire `already_stopped`, `routable = false`).
- `classify` gains no `hidden` force-gate for stops (a stop is authorized like a
  hidden message: whitelisted → `accepted`, else `approval_needed`). The
  already-stopped / recipient-not-found decisions are made in `control.rs` from
  liveness, before classify.

### `crates/daemon/src/control.rs`

On an `op:"stop"` submission: parse it, resolve the target session in a fresh
registry scan.

- No record → `recipient_not_found`.
- Record present but dormant (`socket` null), or its pid no longer alive →
  `already_stopped`, synchronous, not routed.
- Live → `classify` against the whitelist → `accepted` (enqueue) or
  `approval_needed` (enqueue; the router holds it pending).

### `crates/daemon/src/router.rs`

`deliver` branches on `action`:

- `Deliver` → the existing message path (unchanged).
- `Stop` → resolve the target session's live pid from the registry and call
  `core::placement::kill_pid`. Never spawn, resume, or prepend the charter. If
  the pid is gone by routing time, no-op (the sender was already acked).

### Approval surface (`tray.rs`, `notify.rs`, `Pending`)

`Pending` carries the action so the pending-approval text reads **"Stop agent
<label>"**, never "Message to …". The tray menu label, the notification body,
and any status line distinguish a pending kill from a pending message, so the
operator knows they are approving destruction.

### `crates/core`

`placement::kill_pid` already exists. Add a small helper (in `discovery` or the
daemon) that resolves a session id to its live pid from the registry, so the
router and `control.rs` liveness check share one path.

## Testing

- `mailbox`: `parse_stop` (valid, malformed, missing fields); `classify` /
  ack mapping for a stop (whitelisted → accepted, unwhitelisted → approval,
  new `already_stopped` wire word and `routable = false`).
- `control`: an `op:"stop"` submission acks `recipient_not_found`,
  `already_stopped` (dormant), `accepted` (live whitelisted),
  `approval_needed` (live unwhitelisted) against a throwaway socket + registry.
- `router`: a Stop item kills the resolved pid (stubbed kill), does not spawn or
  resume, no-ops on a vanished pid, and honors the approval gate (allow → kill,
  deny → drop).
- The existing message tests stay green (action defaults to Deliver where they
  build items).

## Docs

- `CONVENTION.md` — the `corral_stop_agent` tool and the `op:"stop"` wire shape.
- `AGENTS.md` — rewrite the "Kill of a peer is deferred" limitation and extend
  the Inter-Agent Messaging section to cover stop (gate, addressing,
  idempotence).
- `README.md` — mention the tool if the tool list is surfaced there.
- Adapter READMEs — note the added tool.

## Rejected Alternatives

- **Parentage-scoped kills** (only stop what you spawned): needs an
  env-stamped `parentSession` through every adapter and the convention; too
  much machinery. Reuse the messaging gate instead.
- **Kill always force-gated** regardless of whitelist (like `hidden:false`):
  rejected — a whitelisted pair already trusts each other.
- **`session/cancel` (cancel-turn) semantics**: rejected — stop means kill.
- **Separate StopRequest type + parallel queue**: duplicates the whole
  gate/approval machinery; the `action` field reuses it.
