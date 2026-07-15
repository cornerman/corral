# Cross-Session Tasking Over Corral

Status: design (2026-07-15, revised). Supersedes the earlier "task group"
draft, whose central primitive (a `group` that auto-authorizes intra-swarm
traffic) is removed here as a privacy leak. This revision keeps corral's single
approval gate as the sole cross-directory boundary and adds one read-only
discovery tool.

## One-Sentence Goal

Let a coding agent in one directory task an agent in another the way pi's
in-process `subagents` extension tasks agents in one process: same mental model
(spawn, message, reply, event-driven), but every cross-directory reach stays
behind corral's existing operator-gated approval, and no agent can read another
session's content.

## What We Learned From pi's Subagents (first-hand, `~/.pi/agent/extensions/subagents/`)

The subagents extension is a proven multi-agent tasking API worth mirroring in
*wording*, but its safety model does not transfer, and knowing exactly why is
what shapes this design.

- **Verb set** (`index.ts`): `spawn_agent({name, systemPrompt, overrideModel?,
  message?})`, `send_message({to[], content})`, `list_agents()`,
  `kill_agent({name[]})`, `agent_history({name, offset?, limit?})`,
  `set_status({status, etaMinutes?})`. Names are process-local aliases;
  received messages are prefixed `[message from <sender>]`.
- **No sandboxing whatsoever.** All agents are SDK sessions in one pi process,
  sharing credentials, cwd, and filesystem. Isolation is purely
  conversation-level: separate sessions plus a message-only channel.
  `agent_history` is an explicit pull that hands a peer another agent's whole
  transcript. This is safe *only* because the process is a single trust domain
  owned by one human on one task.
- **Caps** (`CAPS = { maxAgents: 8, maxSpawnDepth: 3, turnBudget: 200 }`) meter
  runaway token spend inside that one process; a budget halt escalates to
  `main` once and `resume_agents()` re-arms.
- **Event-driven discipline.** An agent runs only when messaged, then ends its
  turn and goes idle, auto-rewoken on the next message; polling is forbidden.
  Messages deliver with `deliverAs: "steer"` (at the recipient's next turn
  boundary), so even a looping agent receives them.
- **Wording worth porting verbatim** (`agentSystemPrompt`): the
  task-confirmation handshake (first turn returns the task in the agent's own
  words plus a generous block of clarification questions, then waits for a
  go-ahead); "the only channel is the message tool, a turn ending without a send
  communicates nothing"; reporting guidance (keep routine traffic
  lateral/downward, escalate up for the handshake, blockers, parent-only
  decisions, and final results, bias to fewer higher-signal upward messages);
  uncertainty flows up the chain until someone who can decide (ultimately the
  user) is reached; event-driven, do-not-poll.
- **Shape** (`engine.ts` / `spawner.ts` SDK-free and unit-tested, real adapters
  injected from `index.ts`): a functional-core / imperative-shell split, the
  same discipline corral already keeps (`corral-core` pure, `corrald` the
  shell).

**The load-bearing lesson.** Subagents' unrestricted introspection
(`list_agents`, `agent_history`) is safe *because the trust domain is the
process*. Corral spans the union of every session the user has open across every
project, some human-driven and private. It therefore cannot copy the
introspection; it must copy the *vocabulary and discipline* and supply the
boundary subagents never needed. The caps likewise do not transfer: cross-box
each agent is a visible, independently rate-limited window, so the human
watching the board is the governor, not a turn budget.

## The Single Boundary: corral's Existing Approval Gate

`corrald` already gates every agent-initiated reach on the `(sender-dir →
target-dir)` pair: whitelist hit, else an operator approval popup (Allow once /
Allow always / Deny), Allow-always persisting the pair to `~/.corral/whitelist`.
This design changes nothing about that gate and adds no bypass. In particular
the abandoned draft's `group` field and same-group auto-authorization are
removed entirely (revert branch commits `8f9d0e7` and `8e28e70`): a self-
declared label must never grant reach. Swarm identity, if the operator wants to
see it on the board, is a session-name convention (a derived prefix), not a
registry field.

Capability tiers, stated plainly. Each more-intrusive step is its own gate;
none is implied by a lesser one (a message permission never confers a window
pop or a kill).

| capability | who has it |
|---|---|
| harness **kind catalog** (anonymous: "pi/opencode/quine exist and are running") | any agent, ungated |
| **roster** of a directory (session id, kind, live/dormant) | only directories you are whitelisted to message |
| **message** an existing agent (inject a prompt) | whitelisted `(from → to)` dir pairs, directional, plus a one-shot reply to whoever just messaged you |
| **spawn hidden** (background, no window) | whitelisted dir pair, or approval for a new dir |
| **spawn visible** (`hidden: false`, pops a window) | always operator approval, even for a whitelisted pair |
| **kill** (close a window → dormant) | only spawn parentage `corrald` itself observed |
| **transcript** (another session's conversation content) | nobody, ever |

- **Messaging is not reading.** A whitelist entry `A → B` authorizes A to
  *inject* into B. It grants no ability to read B (no transcript, no title, no
  activity), and it does not imply `B → A` (authorization is directional; the
  operator approves each direction). Reading another session's transcript is a
  capability corral offers to no agent, whitelisted or not (this is why the
  draft's `agent_history` is dropped).
- **The one-shot reply.** When A messages B, B holding A's reply handle may
  answer that exact A session once, ungated, to complete the round-trip A
  started. It does not let B initiate to A later, nor reach A's siblings.
- **The directory is not a leak; the session name is.** An agent earns a
  whitelist entry by *naming* the target directory, so showing it back its own
  authorized paths reveals nothing new. The session name/title is authored by
  the *other* occupant and can describe work A was never told about, so it is
  never shown.
- **Spawn visibility is its own gate.** Agent-initiated spawns default hidden
  (alive, working, no window on the host) so an uninvited agent never pops a
  window. `hidden: false` is a *request*, not a right: popping a window is more
  intrusive than messaging, so it always passes the operator approval popup
  ("spawn a visible agent in /foo?"), even for a whitelisted pair. The agent
  states why it wants visibility; the operator decides.
- **Kill is bounded by observed parentage, not the whitelist.** Being
  whitelisted to *message* a directory must not confer the power to *terminate*
  what is there. An agent may kill only a session `corrald` itself spawned on
  its behalf — a fact `corrald` observed at spawn time (an ephemeral in-memory
  `spawned-by` map), never a self-declared label (this is the crucial
  difference from the rejected `group`: parentage cannot be forged because only
  `corrald` writes it). Killing closes the window and leaves the session
  dormant/resumable, so it is resource hygiene over one's own fan-out, not
  destruction; the only cost is un-persisted mid-turn state.

## New Tool: `list_corral_agents`

A read-only discovery tool so a source agent can *choose* a harness and *reuse*
an existing agent instead of blindly spawning a duplicate. It takes no target
and needs no approval (it names no directory to reach). It submits to `corrald`
(the sandboxed agent must not read the global registry directly); `corrald`
computes the answer from `whitelist ∩ registry` and returns a capability picture,
not just a listing — each entry annotated with what *this caller* may do with
it, so the agent decides without bouncing off the gate:

- **Every entry, always (anonymous):** the harness kind, with its adapter-
  authored one-line `description` (see below). For directories the caller is
  *not* whitelisted to, entries collapse to distinct kinds only (so the total
  count / scale of unrelated work does not leak).
- **Whitelisted directories only:** additionally the directory, the session id,
  and live/dormant state. Never the session name/title or activity.
- **Per-entry affordances:** `can_message` (the caller is whitelisted to this
  dir, so a message lands without an approval popup) and `can_kill` (`corrald`
  observed the caller spawn this one). These leak nothing new — they only report
  what the caller itself may do, which it would discover by trying anyway.

**The `description` is adapter-authored, in the record.** Each adapter writes a
one-line description of its harness kind into a new `description` field on the
registry record (`CONVENTION.md §1`): `corral-pi` → "terminal TUI coding agent",
quine → "GUI coding app, self-windowing", etc. This keeps corral harness-
agnostic (a new kind self-describes on announce, no `corral-core` edit) and the
trust cost is nil: the string is adapter code, not LLM output, and strictly less
dangerous than `spawnCommand`, which corral already runs verbatim. Per-label
conflicts are only possible across adapter versions; latest-seen wins.

Decision flow it enables: an empty target directory, the caller picks a kind
matched to the task (GUI review → `quine`, terminal coding → `pi`/`opencode`)
and spawns; an authorized directory with a reusable `can_message` live agent,
the caller delegates to it by `target_session`. (`corral_message_agent(target_dir=…)`
already reuses-or-spawns; the roster only lets the caller *decide* before
acting.)

## The Existing Tool: `corral_message_agent` (unchanged)

The single cross-session reach tool, registered by every adapter. Give exactly
one of `target_session` (reach an exact agent by id, resuming if dormant — the
reply path) or `target_dir` (reach whoever works there, spawning if none);
`message` is the text; `force_new` forces a fresh agent; `label` picks the kind
to spawn. Sender identity (`fromCwd`, `fromSession` reply handle) is stamped
automatically. Fire-and-forget: it returns an ack (`accepted` /
`approval_needed` / `recipient_not_found` / `directory_not_known`, or a loud
"corral is not running"); no reply is auto-routed.

One new parameter: **`hidden` (boolean, default `true`)**, documented in the
tool description. Agent-initiated spawns default hidden (alive, working, no
window), so an uninvited agent never pops a window and the safe behavior is the
default. `hidden: false` requests a visible window and always passes the
operator approval popup (see the spawn-visibility gate above). The description
tells the agent its spawn is hidden by default and that the operator reveals it
with `h`/Enter.

The charter wording ported verbatim from subagents (task-confirmation
handshake, comms-only-via-tool, reporting guidance, uncertainty-flows-up, event-
driven discipline) is prepended to the first user message of any corral-spawned
agent, adapting only "process" → "swarm" and the tool list to corral's two
verbs.

## Deferred (YAGNI, stated as decisions not omissions)

- **Kill (agent-initiated).** The rule is settled — an agent may kill only a
  session `corrald` observed it spawn (an in-memory `spawned-by` map) — but the
  implementation waits until real fan-out swarms exist and self-cleanup matters.
  Until then the operator kills any agent from the board (`d`) and idle hidden
  agents sit dormant at near-zero cost. Recorded so it drops in without touching
  the message gate.
- **Model selection at spawn.** No `model` field exists in the registry and not
  every harness takes a model flag. Add a `model` field (adapter-written, shown
  in the catalog) and a `modelFlag` launch mechanism (mirroring the existing
  `messageFlag`) only when a concrete task must pin a model.
- **The rest of the subagents verb set** (`set_status`, transcript reading, the
  turn budget / `halt` / `resume_agents`): dropped. The human watching the
  board is the governor; a status line and history are content-leak surface with
  no cross-box payoff yet.

## Net Change

1. Revert branch commits `8f9d0e7` (same-group auth) and `8e28e70`
   (`group`/`name` fields, env transport, `CONVENTION.md` §2b).
2. Add a `description` field to the registry record (`CONVENTION.md §1`),
   adapter-authored, and a `list` query served by `corrald` from
   `whitelist ∩ registry` that returns the capability picture (kind +
   description always; dir/session/liveness for whitelisted dirs; `can_message`
   / `can_kill` affordances per entry).
3. Add the `list_corral_agents` tool, the `hidden` parameter (default true,
   visible spawn gated) on `corral_message_agent`, and the charter preamble to
   all four adapters (pi/opencode/claude/cursor), keeping the agent tool surface
   in parity as board features are.
4. Gate a visible agent-initiated spawn (`hidden: false`) through the operator
   approval popup in `corrald`.

Kill (observed-parentage) and model selection are recorded but deferred. The
trust model stays exactly today's: one gate, no bypass, no cross-session reads.
