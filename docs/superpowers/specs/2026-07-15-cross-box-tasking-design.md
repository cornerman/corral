# Cross-Box Tasking: Mirroring the Subagents API Over Corral

Status: design proposal (2026-07-15). Reconciles with `VISION.md` "Boundary
With Subagents". Decisions with the operator are resolved (see "Decisions
Reached").

Resume note: a partial implementation lives on the branch
`cross-box-tasking-plan` — the `group`/`name` registry fields as the task-group
foundation (commit `f2db1ad`, core + `CONVENTION.md`) and same-group implicit
authorization in `corrald` (commit `c9f9174`). That branch is behind current
`main` and its UI commits were superseded by independent work on main, so
resume by rebasing those two commits onto `main` (or reimplementing them),
not by merging the branch wholesale. This doc is the durable design; the branch
is the code head-start.

## One-Sentence Goal

Grow corral's single `corral_message_agent` tool into the full, familiar
`spawn_agent` / `send_message` / `list_agents` / `agent_history` /
`kill_agent` / `set_status` verb set of pi's in-process `subagents` extension,
so tasking a swarm that spans real windows, directories, and harnesses uses the
same mental model as tasking one inside a single session — without letting that
richer surface leak one session's context into another.

## Why This Is Worth Doing

pi's `subagents` extension is a proven, well-worded multi-agent tasking API
(the source lives at `~/.pi/agent/extensions/subagents/`). Its strength is a
tight conceptual model the model actually follows: spawn a named agent with a
role, exchange fire-and-forget messages, run event-driven (act, end your turn,
get auto-re-woken on reply), never poll, and open every subtask with a
task-confirmation handshake (understanding + clarification questions, wait for
go-ahead). corral already owns the *harder* tier — reaching across boxes — but
exposes only one blunt verb over it. Adopting the subagents surface makes
cross-box tasking feel identical to in-box tasking: one API, one vocabulary,
two scopes.

Corral's cross-box agents are strictly more capable than in-process subagents:
each is a real windowed, persistent, resumable session the human can watch and
take over, of any harness (pi, opencode, Claude Code, quine), surviving the
spawner. A swarm on the board is a swarm you can see and grab.

## The Tension With VISION, and Its Resolution

`VISION.md` states the decision rule "does the work stay in this box? Use a
subagent. Does it need another box? Use corral" and warns "corral should not
grow orchestration features: pi's subagents already own intra-task fan-out
inside the box." This proposal does not overturn that rule; it sharpens it.

Corral is not becoming an orchestrator that *owns* agents. It stays a bus that
composes them: `spawn` still launches an independent windowed session via the
registry's `spawnCommand` (setsid-detached, no parent link), `send` still routes
through the gated `corrald` daemon, `kill` still just closes a window. What
grows is only the *agent-facing verb set over the bus*, converging it with the
subagents wording so a model that learned one already knows the other. "Compose,
don't own" is unchanged; the API surface is what mirrors, not the ownership
model.

Action item: once accepted, update `VISION.md` "Boundary With Subagents" to
state the two tiers now share an interface, and that the split is scope
(same-box vs different-box), not vocabulary.

## Concept Mapping: In-Process → Cross-Box

| subagents (one process) | corral (across boxes) | status |
|---|---|---|
| agent = SDK session in the same process | agent = real windowed session in the registry | exists |
| `name` (process-local, unique) | group-local **name** alias over the global `sessionId`, resolved by corrald | NEW |
| `spawn_agent(name, systemPrompt, model?, message?)` | launch a session in a dir with a task charter as its first user message | partial (launch+message exists) |
| `send_message(to[], content)` | `corral_message_agent` (+ multicast, + name/group addressing) | exists (single target) |
| `list_agents()` | list **group** members only | NEW, leak-sensitive |
| `agent_history(name)` | read a **group** member's transcript (opt-in) | NEW, highest leak risk |
| `kill_agent(name[])` | close/kill windowed sessions | exists for operator (`d`); agent-initiated is new |
| `set_status(status, etaMinutes?)` | agent-set status line on the card | NEW |
| task-confirmation handshake, reporting guidance, uncertainty-flows-up | injected charter preamble on the first user message | PORT VERBATIM |
| spawn tree, `depth`, `maxAgents`, `maxSpawnDepth` | group membership + caps | PORT |
| event-driven wake (`deliverAs: steer`) | socket inject wakes an idle session at its turn boundary | maps onto existing prompt delivery |
| turn budget, `halt` / `resume_agents`, frozen inbox | — | DROPPED (see Decisions) |

## The New Load-Bearing Primitive: The Task Group

A **group** (task group / swarm) is the explicit trust-and-visibility boundary
that in-process subagents get for free from "same process". It is the whole key
to both the feature and the leak defense.

- The registry record (`~/.corral/registry/<sessionId>.json`, spec in
  `CONVENTION.md`) gains a `group` field and a per-group `name` alias.
- A human-started session has no group (or a singleton group of itself). It is
  private by default.
- When an agent spawns a child through corral, the child inherits the spawner's
  group id (creating one on first spawn, rooted at the spawner). The spawn act
  *is* the authorization: by letting its root agent spawn, the operator
  implicitly authorized intra-group traffic, exactly as launching a pi session
  authorizes its in-process subagents.
- Group membership is a dynamic, stronger form of the existing
  `(sender-dir → target-dir)` whitelist: same-group ⇒ implicitly whitelisted for
  messaging and scoped introspection; cross-group ⇒ the existing explicit
  whitelist + operator approval popup, unchanged. Groups and the whitelist
  coexist: the group auto-authorizes a swarm, the whitelist handles explicit
  cross-group pairs the operator wants standing.

Implementation note (mirror subagents' shape): keep the group/registry policy as
pure, headless-testable logic in `corral-core` (as subagents keeps `engine.ts` +
`spawner.ts` SDK-free and injects the real adapters from `index.ts` — a
functional-core/imperative-shell split). `corrald` is the imperative shell that
owns the sockets and gating.

`corrald` (the trusted singleton that already scans the registry and owns
gating) is the natural home for group tracking, name resolution, and the scoped
queries. The board stays a pure viewer; it may render group membership (e.g. a
swarm badge / a grouped column) but never authorizes.

## Leak Analysis (the concern, worked through)

Subagents can freely `list_agents` and read any peer's `agent_history` because
all agents live in one process serving one human's one task — a single, total
trust domain. Corral spans the *union of every session the user has open across
every project*, some human-driven and private (a client-A repo, a personal
repo, a secrets-handling session). Porting the introspection verbs naively would
turn every agent into an observer of all of it. Vectors:

1. **Discovery leak — `list_agents` over the global registry.** A coding agent
   in client-A's repo would learn client-B's repo exists, its title, cwd, and
   live activity. Fix: `list_agents` returns only same-group members. Never the
   global registry.

2. **Transcript leak — `agent_history` across boxes.** Reading another box's
   conversation defeats the workdir-isolation primitive the whole design rests
   on (a sandboxed agent cannot reach a sibling's socket; `agent_history` would
   hand it the contents anyway). Fix: same-group only, and off unless the swarm
   root opted in at group creation — it is the sharpest verb.

3. **Injection-into-a-private-session leak — `target_dir` reuse.** Reusing a
   human's existing live session pulls that private context into agent-driven
   work, and its reply may carry private content back to the requester. Fix:
   agent-initiated spawning prefers `force_new` (a fresh box) over reusing a
   human's session; reuse is allowed only for same-group members. Cross-group
   reach keeps the operator approval gate.

4. **Reply-handle chaining.** The provenance tag carries the sender's session as
   a reply handle, letting an agent reach one session repeatedly. Bounded today
   by the whitelist; group scoping bounds it further (a handle to a non-group,
   non-whitelisted session still hits the approval gate).

5. **Prompt/context exfiltration via a crafted message.** A cross-group message
   could try to get a target to dump its context. Mitigations already present:
   the `[from agent in <dir> (session <id>)]` provenance tag makes agent-origin
   visible to model and human, and cross-group delivery is operator-approved.
   Group isolation keeps the blast radius inside one task.

Confirmed against the subagents source (report from the nixos agent,
2026-07-15): it applies **no VM/bwrap/process/fs sandboxing** — all agents run
in one pi process sharing creds, cwd, and filesystem; isolation is purely at the
conversation level (separate sessions, message-only channel), and `agent_history`
is an explicit pull that hands a peer another agent's whole transcript. So the
unrestricted introspection is safe *only* because the process is one trust
domain; corral, spanning real boxes, must supply the boundary subagents never
needed.

**Unifying principle.** Subagents' safe, unrestricted introspection rests on the
trust domain being *the process*. Corral must make that domain explicit and
bounded — *the task group* — and confine every rich verb (`list`, `history`,
`status`) to it. Everything inter-group rides the existing gated bus; the
human's own sessions form their own private domain, invisible to agents by
default. The leak fix and the feature are the same mechanism: the group.

## Wording to Reuse (verbatim from subagents)

The charter preamble is the part worth copying word-for-word, prepended to the
first user message of every corral-spawned session (adapting only "process" →
"swarm" and the tool list to corral's verbs):

- The task-confirmation handshake: first turn sends the spawner (1) the task in
  the agent's own words and (2) a generous block of clarification questions,
  then ends the turn and waits for a go-ahead before any work.
- The "CRITICAL — how communication works" note: the only channel between agents
  is the message tool; a turn ending without a send communicates nothing.
- Reporting guidance: keep routine updates lateral/downward, escalate up for the
  handshake, blockers, parent-only decisions, and final results; bias to fewer,
  higher-signal upward messages.
- Handling uncertainty: resolve it yourself first; escalate judgment calls up the
  chain; uncertainty flows up until it reaches someone (ultimately the user) who
  can decide; you cannot reach the user directly (only the root can).
- Event-driven discipline: do not poll or busy-wait; act, end the turn, get
  auto-re-woken on a message; track outstanding replies; inspect only on
  suspicion of a problem.

The tool `description` strings (`spawn_agent`, `send_message`, `list_agents`,
`kill_agent`, `agent_history`, `set_status`) port with minimal edits: swap
"agent name" for "group-local name (or session id)" and note that reach outside
the group is operator-gated.

## New Contract Surface (CONVENTION.md additions)

1. **`group` + `name` on the registry record.** `group` groups a swarm; `name`
   is the group-local alias corral resolves to a `sessionId` for addressing.
2. **Task charter at spawn = the first user message.** No separate persistent
   system-prompt channel is built: a corral-spawned agent is a full harness
   session that already loads AGENTS.md, project context, and its own system
   prompt, so it needs only the task, which the existing launch-with-message
   path delivers as the first user message (the charter preamble is prepended).
   This works on every harness with zero new per-adapter code and is visible to
   the human on the card.
3. **Scoped queries in `corrald`.** `list` and `history` answered by the daemon
   from a group-filtered registry view, never the raw scan. Names resolved by
   the daemon.

## What Not To Build (YAGNI / KISS)

- **Turn budget + `halt` / `resume_agents`.** In-process these cap runaway token
  spend the human cannot see. Cross-box, each agent is a visible window,
  independently rate-limited by its own harness, and there is no single process
  to meter. The human watching the board is the governor. Dropped.
- **A separate system-prompt-at-spawn mechanism.** The first user message is the
  charter (see above). Dropped as over-engineering.
- **A corral-owned scheduler / lifecycle manager.** Would violate "compose, don't
  own". Spawned sessions are detached and independent; corral tracks the group,
  it does not run a supervision loop.
- **Global `list_agents`.** Explicitly rejected: it is the discovery leak.

## Decisions Reached (with the operator, 2026-07-15)

- **Groups are the trust/visibility unit** — a new `group` registry field,
  auto-authorizing a spawned swarm — AND the per-directory whitelist stays for
  explicit standing cross-group pairs. The two coexist.
- **`agent_history` is ported**, same-group only, off unless the swarm root
  opted in.
- **The task charter is the first user message**; no separate system-prompt
  infrastructure is built.
- **The turn budget / `halt` / `resume_agents` are dropped** cross-box.
- **Addressing within a group uses group-local names** resolved by `corrald`.

## Rollout Sketch (thin slices, each shippable)

1. Add `group` + `name` to the registry record + `CONVENTION.md`; corral-pi
   writes them; board renders a swarm badge. (No behavior change yet.)
2. `corrald` tracks groups + resolves names; generalize the whitelist so
   same-group is implicitly authorized; keep cross-group gated.
3. Grow the agent-facing tool from `corral_message_agent` to the full verb set,
   porting the subagents wording; `list`/`history`/`status` scoped to the group;
   the charter preamble prepended to the spawn's first user message.
4. Update `VISION.md` and `AGENTS.md`; TUI/GUI parity for any board rendering
   (hard rule).
