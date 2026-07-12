--- title: How the agent-orchestration ecosystem defines and detects "status" ---

# How the Agent-Orchestration Ecosystem Defines and Detects "Status"

Survey date: 2026-07-12. Scope: kanban and dashboard tools for running multiple coding-agent
sessions, narrowed to one question: what does "status" mean, and how does each tool detect it.

## Executive Summary

Every tool in this survey reduces to the same three-state machine at its core: the agent is
**doing work**, the agent is **waiting on the user**, or the agent is **done or broken**. Tools
differ enormously in how finely they split the middle and end states, and even more in how
reliably they can detect them. That reliability depends on one structural fact more than any
design choice: whether the tool is the **driver** that launched the agent and holds the live
control connection, or a **passive observer** watching a session it did not start.

Drivers can get status for free, because the agent's own protocol hands it to them: Vibe Kanban
receives ACP's `session/request_permission` request directly and models `ApprovalStatus::Pending`
(github.com/BloopAI/vibe-kanban); Crystal parses Claude Code's `stream-json` stdout for a
`"type":"prompt"` message (github.com/stravu/crystal); Codex CLI's own protocol emits
`ExecApprovalRequest` and `ApplyPatchApprovalRequest` events on its JSON-RPC-like channel
(github.com/openai/codex). Observers cannot receive any of this, because these signals are
scoped to whichever client sent the request or holds the driving connection. Tools that only
*watch* a terminal they did not open (Claude Squad, uzi) fall back to grepping literal strings
out of screen scrapes, such as `"esc to interrupt"` or `"No, and tell Claude what to do
differently"` — a fragile mechanism that breaks the moment the target CLI changes its UI copy.

The single most important finding for corral is that the Agent Client Protocol itself is
mid-flight adding a first-class, broadcast-to-every-client status notification. ACP v2's
`state_update` session update (unstable, drafted 2026-07-02, agentclientprotocol/agent-client-
protocol) defines exactly three states — `running`, `idle`, `requires_action` — and is explicitly
designed to reach every attached client, not just the one that sent the prompt. This is the same
three-state model corral's own vendor `_corral/state` notification approximates today, and it is
independent confirmation that a passive observer's honest ceiling is Working / Idle / Needs-Input,
not fine-grained tool-call detail. Corral should track this RFD and plan to consume `state_update`
once pi (or any ACP agent) implements it, rather than treat `_corral/state` as a permanent bespoke
protocol.

Five findings most relevant to corral's design:

1. **`session/request_permission` is architecturally invisible to an observer.** It is a
   client-directed JSON-RPC request, not a `session/update` notification, so nothing in the ACP
   schema broadcasts it to a second, non-driving connection. Corral's inability to see pi's
   approval prompt is not a pi limitation to work around; it is what ACP's request/response
   pattern guarantees for any agent, unless the agent chooses to also mirror the fact of waiting
   (not the content) through a vendor notification.
2. **ACP v2 is standardizing the exact shape corral invented independently.** `state_update`'s
   `running` / `idle` / `requires_action` triad matches corral's Working / Needs-You split plus a
   third bucket corral currently folds into Working. This validates binary-plus-growth as the
   right shape, not just a stopgap.
3. **Every tool that distinguishes "needs approval" from "still thinking" does so only because it
   is the driver.** Vibe Kanban, Crystal, HumanLayer, Codex CLI, and OpenHands's SDK all get this
   for free from a structured channel they control. No passive-observer tool in this survey
   detects approval-wait without either driving the agent or scraping literal terminal text.
4. **Terminal-scraping tools (Claude Squad, uzi) prove the fallback is viable but brittle.** They
   hash tmux pane content to detect "still producing output" and grep known UI strings to detect
   "asking for permission." This works today against Claude Code, Aider, and Gemini CLI's current
   prompt text, and breaks silently on any CLI UI wording change, with no error, just a missed
   detection.
5. **A cheap, high-leverage middle ground already exists in shipped tooling: the terminal bell.**
   Aider rings the ASCII BEL character (or fires an OS notification) the moment it is about to
   block for input (github.com/Aider-AI/aider, `aider/io.py`). Claude Code's own `Notification`
   hook fires a distinct `permission_prompt` event before any approval prompt
   (docs.claude.com/en/docs/claude-code/hooks). Both are opt-in, structured, harness-side signals
   that an external process can consume without scraping pixels, which is the same shape as
   corral's own `_corral/state` extension notification.

## Tool Inventory

### Vibe Kanban (BloopAI)

What it is: an open-source, self-hosted kanban board that launches coding-agent CLIs (Claude
Code, Codex, Gemini CLI, opencode, Cursor CLI, and any ACP agent) itself and tracks each run as a
task. github.com/BloopAI/vibe-kanban.

Driver or observer: **driver**. Vibe Kanban starts the agent process (`ExecutionProcess`, `run_
reason: CodingAgent`) and, for ACP agents, is itself the ACP client that receives every
`session/update` and `session/request_permission` call
(crates/executors/src/executors/acp/client.rs).

Status vocabulary:
- `TaskStatus`: `Todo`, `InProgress`, `InReview`, `Done`, `Cancelled` — the kanban column, a
  task-level status the user or reviewer sets (crates/db/src/models/task.rs:14).
- `ExecutionProcessStatus`: `Running`, `Completed`, `Failed`, `Killed` — the underlying OS-process
  status for whichever script or agent CLI is running (crates/db/src/models/execution_process.rs:43).
- `ApprovalStatus`: `Pending`, `Approved`, `Denied { reason }`, `TimedOut` — a distinct status just
  for a tool-permission request, separate from the process status, with a 10-hour timeout
  (`APPROVAL_TIMEOUT_SECONDS`) after which a pending approval auto-expires
  (crates/utils/src/approvals.rs).

Detection mechanism: for its ACP executor, Vibe Kanban implements the ACP client trait's
`request_permission` handler directly; the incoming `RequestPermissionRequest` is turned into an
`ApprovalRequest`/`ApprovalStatus::Pending` the instant ACP delivers it
(crates/executors/src/executors/acp/client.rs:68-97). For non-ACP executors (Claude Code, Codex,
opencode) it uses each tool's own structured event stream or SDK client rather than terminal
scraping (crates/executors/src/executors/claude/client.rs, codex/client.rs, opencode/sdk.rs).

Notable limitation: this approval visibility is possible only because Vibe Kanban is the ACP
client of record. A tool watching the same session over a second connection would not receive
`request_permission`, because ACP delivers it to the requesting connection, not by broadcast (see
the ACP protocol-specifics section below).

### Claude Squad (smtg-ai)

What it is: an open-source terminal UI that runs each of several coding-agent CLIs (Claude Code,
Codex, Aider, Gemini CLI, OpenCode, Amp) in its own tmux session and git worktree.
github.com/smtg-ai/claude-squad.

Driver or observer: **driver at the OS-process level** (it starts the tmux session and the CLI
inside it) but has **no semantic channel** into the CLI's own turn lifecycle, so it detects status
the way an observer would: by reading the terminal.

Status vocabulary: `Running`, `Ready`, `Loading`, `Paused` (session/instance.go:17-27). `Running`
means "claude is working"; `Ready` means "waiting for user input"; `Paused` means the worktree was
removed but the branch preserved.

Detection mechanism: every 500 ms, `tickUpdateMetadataCmd` captures the tmux pane
(`tmux capture-pane -p`), computes `HasUpdated()`, and hashes the captured text, comparing it to
the previous hash (session/tmux/tmux.go:194-255). If the hash changed, status becomes `Running`;
if unchanged, `Ready`. In the same pass it also greps the raw pane text for one hard-coded
substring per program to catch a permission prompt: `"No, and tell Claude what to do
differently"` for Claude Code, `"(Y)es/(N)o/(D)on't ask again"` for Aider, `"Yes, allow once"` for
Gemini CLI (session/tmux/tmux.go:242-248). A match sets `hasPrompt`, which the app loop uses to
auto-press Enter rather than to expose a distinct status column (app/app.go:238-248).

Notable limitation: this is literal-string terminal scraping, tied to the exact wording of each
target CLI's approval prompt at the time the code was written. It has no distinct "awaiting
approval" status; a caught prompt collapses into an auto-continue action, not a UI state.

### Crystal / Nimbalyst (stravu)

What it is: a desktop app (now renamed Nimbalyst) that runs multiple Claude Code and Codex
sessions in parallel git worktrees. github.com/stravu/crystal.

Driver or observer: **driver**. It spawns Claude Code with `--output-format stream-json` and
parses the resulting structured JSON events from stdout (main/src/events.ts).

Status vocabulary: `initializing`, `ready`, `running`, `waiting`, `stopped`, `completed_unviewed`,
`error` (main/src/types/session.ts:6).

Detection mechanism: Crystal listens to `claudeCodeManager`'s `output` events. When a parsed JSON
event has `type: "prompt"`, it sets the session to `waiting` (main/src/events.ts:882-889). When a
`type: "system", subtype: "result"` event arrives, that is Claude Code's own end-of-turn signal
(main/src/events.ts:891-893). This is consumption of Claude Code's documented structured
stream-json protocol, not terminal scraping.

Notable limitation: this mechanism exists only because Crystal launched Claude Code itself with a
machine-readable output flag. An external process merely watching Crystal's own terminal window
would see none of this; it is only available on the stdout Crystal itself owns.

### uzi (devflowinc)

What it is: a CLI for running many coding agents in parallel across git worktrees and tmux
sessions, with an `uzi ls` command that lists all running agents and their status.
github.com/devflowinc/uzi.

Driver or observer: **driver at the OS-process level**, same pattern as Claude Squad: it starts
each tmux session, but reads status back out of the terminal rather than a structured channel.

Status vocabulary: `running`, `ready` (cmd/ls/ls.go:106-113); anything it cannot classify reports
`unknown`.

Detection mechanism: `getAgentStatus` runs `tmux capture-pane -t <session>:agent -p` and checks
whether the captured text contains the literal substring `"esc to interrupt"` or `"Thinking"`; if
either is present, status is `running`, otherwise `ready` (cmd/ls/ls.go:94-103). `"esc to
interrupt"` is Claude Code's own status-line text while it is generating.

Notable limitation: identical class of fragility to Claude Squad. There is no distinct status for
"waiting on a permission prompt" at all; a permission prompt does not contain either matched
string, so uzi reports such a session as `ready`, indistinguishable from a session that is truly
idle.

### HumanLayer (humanlayer.dev)

What it is: an approval and observability layer for agent tool calls, with a daemon (`hld`) that
tracks Claude Code sessions and exposes a "waiting for input" status distinct from running.
github.com/humanlayer/humanlayer.

Driver or observer: **driver**, and additionally the approval mechanism itself: HumanLayer
inserts itself as the approval authority the agent's own tool-call path must go through.

Status vocabulary: `draft`, `starting`, `running`, `completed`, `failed`, `interrupting`,
`interrupted`, `waiting_input`, `discarded` (hld/session/types.go:20-28). `waiting_input` is
explicitly documented in-code as "Session is waiting for tool approval input."

Detection mechanism: when a tool call requires approval, the daemon's approval manager creates an
`Approval` record with `ApprovalStatusLocalPending` and, in the same code path, updates the
session's status to `waiting_input` (hld/approval/manager.go:96-101). The approval object is
correlated to the specific tool call so the UI can show which call is blocked
(hld/approval/manager.go:83-90).

Notable limitation: this depends on the agent's tool calls routing through HumanLayer's own
approval path (an MCP-style interception the daemon controls), so it is not available to a tool
that merely watches an agent's terminal from outside.

### OpenHands SDK (OpenHands, formerly All Hands AI)

What it is: the agent-execution SDK behind OpenHands V1 (`software-agent-sdk`), which exposes a
`ConversationState` with an explicit execution-status enum that its own frontend consumes over a
WebSocket. github.com/OpenHands/software-agent-sdk.

Driver or observer: **driver**; the SDK's frontend is a first-party consumer of its own internal
state.

Status vocabulary: `ConversationExecutionStatus`: `IDLE`, `RUNNING`, `PAUSED`,
`WAITING_FOR_CONFIRMATION`, `FINISHED`, `ERROR`, `STUCK`, `DELETING`
(openhands-sdk/openhands/sdk/conversation/state.py:48-59). The SDK defines `is_terminal()` to
cover exactly `FINISHED`, `ERROR`, `STUCK` — explicitly excluding `IDLE`, because `IDLE` is also
the initial pre-run state, and treating it as terminal would produce false completion signals on
first connect (same file, docstring at line 66).

Detection mechanism: `STUCK` implies OpenHands runs its own loop-detection heuristic distinct from
a simple busy/idle read (not independently verified in this pass beyond the enum's own comment;
the detector itself lives elsewhere in the SDK and was not traced). `WAITING_FOR_CONFIRMATION`
corresponds to OpenHands's confirmation-mode tool approval.

Notable limitation: like HumanLayer, this state lives inside the process that is running the
agent loop and is pushed to its own UI; it is not a wire format designed for a third-party,
non-driving observer.

### Codex CLI (OpenAI)

What it is: OpenAI's open-source terminal coding agent, with its own structured JSON-RPC-like
protocol (`codex proto`, and the `codex-rs/protocol` crate) in addition to an ACP-compatible mode.
github.com/openai/codex.

Driver or observer: **driver's protocol**, consumed by whatever process launched `codex` (its own
TUI, or a wrapping tool like Conductor or Vibe Kanban).

Status vocabulary (from `EventMsg`, codex-rs/protocol/src/protocol.rs): turn-level —
`TurnStarted`, `TurnComplete`, `TurnAborted`; approval-specific — `ExecApprovalRequest`,
`ApplyPatchApprovalRequest`, `RequestPermissions`, `RequestUserInput`, `ElicitationRequest`;
terminal/error — `StreamError`, `Error`, `ShutdownComplete`. This is a much finer-grained
vocabulary than any other tool surveyed, distinguishing shell-exec approval from patch-apply
approval from generic user-input requests.

Detection mechanism: Codex's core emits these as discrete protocol events on its JSON channel as
work proceeds; a driving client reads them directly. Separately, Codex also writes a session
"rollout" file under a `sessions` subdirectory of `$CODEX_HOME` for resume and debugging purposes
(codex-rs/core/src/session_rollout_init_error.rs; codex-rs/rollout-trace/README.md documents an
opt-in, more detailed trace bundle written only when `CODEX_ROLLOUT_TRACE_ROOT` is set). Whether
the rollout file is a stable, documented format suitable for a third-party file-watching observer
is **unverified**; the rollout-trace README explicitly frames trace bundles as a diagnostic
artifact, not an integration surface.

Notable limitation: the rich approval vocabulary is visible only to the driving client. A
file-watch fallback on the rollout log exists in principle but its stability as an external
contract was not confirmed.

### Claude Code hooks (Anthropic)

What it is: not a kanban tool, but the structured extension point every observer-oriented tool in
this space eventually reaches for when it wants status without scraping the terminal.
docs.claude.com/en/docs/claude-code/hooks.

Driver or observer: hooks run as local shell commands, HTTP calls, or LLM prompts that Claude Code
itself invokes at lifecycle points; the hook script is trusted local configuration, effectively
part of the driver's own machine, but a hook can forward events to any external process, including
one that did not launch the session — which makes hooks the one mechanism in this survey that lets
a non-driving observer receive turn-lifecycle events, provided the hook is configured to forward
them.

Status vocabulary exposed via hook events:
- `SessionStart` / `SessionEnd` — once per session.
- `UserPromptSubmit`, `Stop`, `StopFailure` — once per turn. `Stop` fires when the main agent
  finishes responding, unless the turn ended by a user interrupt; `StopFailure` fires instead when
  it ended in an API error such as a rate limit or auth failure — this is the harness's own split
  between "turn ended normally" and "turn ended by error," matching this survey's cross-cutting
  taxonomy entry for "crashed/error" as distinct from "done."
- `PreToolUse` / `PostToolUse` — once per tool call inside the agentic loop.
- `Notification`, with a `notification_type` matcher including `permission_prompt` ("Claude needs
  you to approve a tool use"), `idle_prompt` ("Claude is done and waiting for your next prompt"),
  `agent_needs_input` and `agent_completed` (background-session equivalents, gated to Claude Code
  v2.1.198+, and only fire "while agent view is open in a terminal").
- `SubagentStart` / `SubagentStop` for the Task-tool subagent lifecycle.

Detection mechanism: Claude Code calls the configured hook handler at each event with JSON on
stdin (for command hooks) carrying `session_id`, `hook_event_name`, and event-specific fields. The
`Stop` event's input additionally carries `background_tasks` (each with its own `status` field,
covering task types `shell`, `subagent`, `monitor`, `workflow`, `teammate`, `cloud session`, `MCP
task`) and `session_crons`, specifically so a `Stop` hook can distinguish "the session is fully
done" from "the session is paused, waiting on background work to wake it" — a fourth taxonomy
value (paused-pending-background-work) that most tools in this survey do not model separately.

Notable limitation: `Notification` hooks cannot block or modify the notification; they exist
purely for side effects such as forwarding to an external service. This is exactly the shape
corral's own `_corral/state` extension notification takes: an opt-in, additive signal a harness
chooses to emit, rather than a mandatory protocol field. It requires the user to install the hook
configuration; a stock Claude Code install run under a passive observer that did not configure
hooks emits none of this.

### Backlog.md (MrLesk)

What it is: a markdown-file-based task tracker meant to be shared between a human and coding
agents inside a git repository, with a CLI and web kanban UI. github.com/MrLesk/Backlog.md.

Driver or observer: **neither**, structurally. Backlog.md's "status" is a task-lifecycle field
(`To Do`, `In Progress`, `Done`, and any custom statuses defined in project config) that the agent
or human edits directly in a task's markdown frontmatter (README.md: `backlog task list -s "To
Do"`; `backlog config` documents `definition_of_done` and status configuration).

Status vocabulary: user-configurable; ships with `To Do` / `In Progress` / `Done` by default.

Detection mechanism: **self-reported**, not derived from any process signal. A task's status
changes only when something (agent or human) runs `backlog task edit` or writes the frontmatter
field directly. There is no liveness check: a crashed or hung agent that forgot to update the
field leaves the task showing whatever status it last wrote.

Notable limitation: this is the weakest category of "status" in this survey precisely because it
carries no crash detection or timeout at all. It is included to make the taxonomy complete: it
shows a status meaning ("what phase is this piece of work in") that is orthogonal to "is the
process that's doing it still alive and what does it need." Corral's Working/Needs-You question
does not apply to Backlog.md at all; the two tools solve different problems that are easy to
conflate because both call the field "status."

### Aider

What it is: an open-source terminal coding-agent CLI, notable here for a specific, well-scoped
notification mechanism rather than any dashboard. github.com/Aider-AI/aider.

Driver or observer: n/a; Aider is the target being observed by whatever terminal or multiplexer
it runs inside.

Detection mechanism it exposes to the outside: `io.py` sets `bell_on_next_input = True` the moment
Aider marks that "the LLM has started processing" (`aider/io.py:1051`). The next time Aider is
about to block for user input, `ring_bell()` either shells out to a configured OS notifier
(`terminal-notifier` or `osascript` on macOS; a Linux notifier is chosen similarly) or, with no
notifier configured, prints the raw ASCII BEL character `\a` to the terminal (`aider/io.py:1088-
1103`). This is the terminal-bell mechanism in its cleanest documented form: a one-byte signal any
terminal emulator, tmux status bar, or `xterm`-style "silence" watcher can detect without parsing
any text at all.

Notable limitation: a bell says only "something happened, go look," not what happened. It cannot
distinguish "task finished successfully" from "task is asking for approval" from "task hit an
error." It is a wake-up signal, not a status.

### Agent Client Protocol (ACP) — Zed / agentclientprotocol.com

What it is: the wire protocol corral already speaks, developed by Zed and now a
multi-implementer open project (Anthropic's Claude Code, Google's Gemini CLI, OpenAI's Codex, and
others ship ACP adapters). agentclientprotocol.com;
github.com/agentclientprotocol/agent-client-protocol.

Driver or observer: ACP has exactly one **client** role per session (the driver — the editor or
tool that sent `session/prompt` and can call `session/cancel`) and one **agent** role. It has no
first-class notion of a second, non-driving client in the currently-shipped v1 protocol. corral's
whole design is a workaround for this: it opens its own socket connection to the same agent
process (via `corral-announce`) and receives the *broadcast* portion of ACP traffic, without ever
being the client that sent the prompt.

Status vocabulary in the stable (v1) schema: **none, directly.** The closed `SessionUpdate` union
(agentclientprotocol/agent-client-protocol, `schema/v1/schema.json`) carries only message and
tool-call content notifications; the only turn-boundary signal is `StopReason` (`end_turn`,
`max_tokens`, `max_turn_requests`, `refusal`, `cancelled`, or an implementation-specific value
prefixed with `_`), and `StopReason` is returned **only in the `session/prompt` response**, which
goes only to the client that sent that specific prompt — this is why corral's AGENTS.md correctly
states "ACP signals turn end only to the prompt sender via `stopReason`."

Approval vocabulary: `session/request_permission` is a request the agent sends **to the client**
(schema/v2/schema.json, `RequestPermissionRequest`), with responses `Selected` (user chose one of
`PermissionOption`s) or `Cancelled` (the client cancelled the turn while the prompt was pending).
It is a JSON-RPC request/response pair over the single client connection, not a `session/update`
notification, so nothing in the schema broadcasts it to a second observing connection.

**What is changing in ACP v2 (unstable, drafted 2026-07-02):** the `v2 Prompt Lifecycle` RFD
(docs/rfds/v2/prompt.mdx, agentclientprotocol/agent-client-protocol) proposes a new `state_update`
`session/update` variant, explicitly because "if an agent finishes its turn, wants to wait for the
next user action, but has a background subagent or task running, can it only submit updates about
that status after the user prompts again?" It defines three states:

- `running` — "a turn has begun," important once turns are no longer strictly tied to a single
  prompt request.
- `idle` — carries an optional `stopReason`, sent "whenever the agent is done."
- `requires_action` — "the agent is trying to run, but needs to wait on user input to continue, it
  isn't just idle." The RFD explicitly floats, but does not commit to, adding "which permission or
  elicitation it is waiting on" as a future refinement.

Because `state_update` is a `session/update` notification like any other, and ACP notifications are
delivered to every connection subscribed to the session's updates (not scoped to whoever sent the
last prompt), this is, as of this RFD, the first protocol-level, multi-client-visible signal for
"the agent needs you" in ACP's history. As of 2026-07-12 this lives only in `schema/v2` at package
version `2.0.0-alpha.0` (schema/v2/Cargo.toml) and the RFD itself is dated 2026-07-02 — it is very
new and unstable, not yet something corral or pi can depend on in production.

Permission-request v2 changes: a separate RFD (docs/rfds/v2/permission-requests.mdx) makes
`RequestPermissionRequest` carry a required `title`, optional `description`, and an optional
tagged `subject` (currently only a `tool_call` variant is defined), decoupling the human-readable
prompt copy from the tool-call's persisted title/content. This does not change who receives the
request; it remains a request to the client, not a broadcast.

Notable limitation: even after `state_update` ships, `requires_action` alone does not tell an
observer *what* is being requested (a permission? an elicitation? a plain question?) — the RFD
says this explicitly is future work, if it happens at all. An observer would gain "needs you," not
"needs you because of X."

## Cross-Cutting Taxonomy of "Status"

Collecting every distinct meaning of "status" found across the tools above:

1. **Task/work-item status** — what phase of human/PM workflow a unit of work is in: `Todo`,
   `InProgress`, `InReview`, `Done` (Vibe Kanban `TaskStatus`; Backlog.md). This is about the
   *task*, not the *process*; a task can sit `InProgress` for days while its agent process starts,
   stops, crashes, and restarts several times underneath it.
2. **Process/turn liveness** — is the agent actively computing right now: `Running` /
   `Working` vs `Ready` / `Idle`. This is the one every tool has some version of, and the one a
   busy/idle heuristic (hash comparison, CPU check, or a `running`/`idle` protocol field) answers
   directly.
3. **Awaiting user input to continue, not yet done** — ACP's `requires_action`, HumanLayer's
   `waiting_input`, OpenHands's `WAITING_FOR_CONFIRMATION`, Crystal's `waiting`, Vibe Kanban's
   `ApprovalStatus::Pending`, Codex's `ExecApprovalRequest`/`RequestUserInput`. This is
   semantically distinct from idle: the agent has more work queued behind a decision only the
   human can make. Tools disagree on whether this needs its own bucket (most driver tools give it
   one) or folds into "working" (every PTY-scraping observer in this survey, corral today).
4. **Turn/session complete** — ACP's `StopReason: end_turn`, Codex's `TurnComplete`, Crystal's
   `type: "system", subtype: "result"`, Claude Code's `Stop` hook. All distinguish a *clean* finish
   from other ways a turn can end.
5. **Cancelled** — ACP's `StopReason: cancelled`, Vibe Kanban's `ExecutionProcessStatus::Killed`,
   Codex's `TurnAborted`. The user (or a client) actively stopped the turn, as opposed to it
   running to completion or erroring.
6. **Error/crashed/refused** — ACP's `StopReason: refusal` or `max_tokens`/`max_turn_requests`,
   Codex's `Error`/`StreamError`, Claude Code's `StopFailure` (explicitly distinguished from `Stop`
   because it fires "when the turn ends due to an API error" such as rate limits or auth
   failures), OpenHands's `ERROR`, Vibe Kanban's `ExecutionProcessStatus::Failed`. Several tools
   further split "the model refused" from "we hit a token/turn budget" from "the underlying API
   call failed" — three different failure semantics that a single "error" bucket would blur.
7. **Stuck/loop-detected** — OpenHands's `STUCK`, its own heuristic category distinct from a plain
   error, implying pattern-based loop detection rather than a hard failure signal.
8. **Paused / waiting on background work, not asking for input** — Claude Code's `Stop` hook
   `background_tasks`/`session_crons` fields exist specifically to let a hook tell "fully done"
   apart from "done for now, but a background shell/subagent/cron will wake this session later."
   Claude Squad's `Paused` (worktree removed, branch kept) is a different, storage-level meaning of
   the same word.
9. **Self-reported task-field status** — Backlog.md's kanban column. Carries no liveness guarantee
   at all; it is metadata the agent or human writes, not a signal derived from the agent's
   execution.

The disagreement worth surfacing explicitly: tools that can see a structured turn-lifecycle
protocol split states 3 through 7 into as many as eight or nine distinct values (Codex, Claude
Code hooks). Tools limited to terminal scraping (Claude Squad, uzi) collapse everything except
"still producing new output" into a single `Ready`/`ready` bucket, because that is all a hash
comparison or substring match can support without either brittle, tool-specific string matching
(which they also do, partially, for approval prompts) or actually parsing the target CLI's screen
layout.

## Detection Mechanisms, Ranked by Robustness

From most to least robust, and marked for observer-viability:

**(a) Protocol/structured events — most robust, but scoped to the driver by default.**
Examples: ACP `session/update` notifications and `stopReason`; ACP `session/request_permission`;
Claude Code's `stream-json` output format (consumed by Crystal); Codex's `EventMsg` stream; MCP
tool-call interception (HumanLayer). Precise, low-latency, immune to UI text changes. **Observer-
viable only if the protocol explicitly broadcasts to multiple connections** (ACP's forthcoming
`state_update` would qualify; `session/request_permission` and `stopReason` today do not, because
both are scoped to one connection by the spec).

**(b) Harness-side opt-in notifications — robust, and the one mechanism genuinely built for
observers.** Examples: Claude Code's `Notification` hook (`permission_prompt`, `idle_prompt`,
`agent_needs_input`, `agent_completed`); corral's own `_corral/state` vendor `ExtNotification`
emitted by the `corral-announce` pi extension on `turn_start`/`turn_end`. These are additive
signals a harness chooses to emit specifically so an external process can watch without driving.
Their only weakness is that they require the harness (or an extension to it) to be configured to
emit them at all; a vanilla install of the same tool, run without the extension/hook, emits
nothing.

**(c) Process/OS signals — robust for liveness, blind to semantics.** Not deeply used by any
surveyed tool for turn-level status (none of the sources found relied on `/proc` CPU-time deltas
or `waitpid` to infer "thinking" vs "idle" — CPU activity does not reliably distinguish "streaming
a response" from "waiting on a slow tool call" from "idle but the process happens to be polling").
**Fully observer-viable** (any process on the same host can read `/proc/<pid>/stat`), but only
answers "is the process alive," not "what is it doing."

**(d) PTY/terminal output scraping, including the bell — moderate robustness, fully observer-
viable, but brittle to UI changes.** Hash-diffing a captured pane (Claude Squad) or grepping for a
literal known-good string (Claude Squad's `"esc to interrupt"`; uzi's identical string; Aider's
programmatic bell). **Fully observer-viable**, since it needs only read access to the terminal or
tmux pane, no cooperation from the target process beyond it printing normal output. Its ceiling is
low: it can reliably answer only "did new text appear" and, for a closed, enumerable set of known
CLIs, "does this known substring appear right now." It cannot generalize to an arbitrary or
future CLI without adding a new hard-coded string per release.

**(e) File/log watching — moderate, observer-viable, but contract stability varies.** Codex's
rollout/session-log files under `$CODEX_HOME` exist and are read on resume, but their README frames
them as diagnostic, not an integration surface (unverified whether they carry a stable public
schema). Backlog.md's markdown frontmatter is the opposite case: explicitly a public, documented,
user/agent-editable file format, but it answers question 9 in the taxonomy (task-field status),
not question 2 (process liveness) — a file can say `In Progress` while the process behind it is
long dead.

**(f) Timeout/inactivity heuristics — a backstop, not a primary signal, and observer-viable by
definition.** Vibe Kanban's `ApprovalStatus::TimedOut` after `APPROVAL_TIMEOUT_SECONDS` (10 hours)
converts a stuck `Pending` into a terminal state so the UI does not wait forever. Corral's own
"transient watch read error reports the agent gone; the next 1s scan reconnects" is the same
category: a liveness backstop layered under whatever primary signal exists, needed precisely
because every signal above (a)-(e) can silently stop arriving without an explicit "I am gone"
message.

## Protocol-Level Specifics: What an ACP Observer Can and Cannot Learn

Corral is exactly the case this section is written for: a socket client connected to an agent's
ACP surface that did **not** send the `session/prompt` for the turn it is watching.

What it **can** learn today (v1, stable), if the agent chooses to emit it:
- Every `session/update` notification the agent sends is a broadcast in the sense that any
  connected client receives it; that includes `agent_message_chunk`, `tool_call`/`tool_call_
  update`, `plan`, and `session_info_update`. This is exactly what corral's `watch.rs` already
  consumes.
- A vendor `_meta`/`ExtNotification` the agent chooses to emit alongside the standard union, which
  is exactly what `corral-announce`'s `_corral/state` is: an `ExtNotification`, ignored by
  conformant clients that do not recognize it, consumed by corral because it knows the vendor key.

What it **cannot** learn, structurally, from v1 ACP alone:
- `stopReason`, because it travels only in the `session/prompt` **response**, addressed to the
  connection that sent that specific prompt.
- `session/request_permission`, because it is a request the agent sends **to** a client, and the
  spec does not define a mechanism for a second, non-requesting client to see it, or to answer it.
  corral's AGENTS.md is precisely correct on this point: "Approvals stay in the pi TUI; socket
  clients never receive `session/request_permission`."

What v2's draft `state_update` would change, if and when it stabilizes and pi implements it: since
it is a `session/update` notification (not a request/response pair scoped to one connection), it
would reach corral the same way `agent_message_chunk` does today. Corral would then be able to
show `requires_action` as its own bucket **without knowing why** the agent is blocked — it would
still not receive the permission's `title`, `description`, or `options`, only the fact that the
agent is waiting. That is a meaningfully richer signal than today's binary Working/Needs-You (it
would let corral rename "Needs You" to mean specifically "blocked pending your decision" rather
than "not producing new output"), but it does not close the gap all the way to showing what the
agent is asking for.

## Recommendations for Corral

**Can "awaiting approval" be detected by a passive observer today, without changing pi?** No,
not from ACP alone, and this survey found no other tool that manages it either without either (a)
driving the agent (Vibe Kanban, Crystal, Codex, HumanLayer) or (b) scraping a hard-coded, version-
fragile string out of a terminal it can read (Claude Squad, uzi). Given corral's design premise
(never drive, watch arbitrary terminals the user opened themselves, be agent-agnostic), option (a)
is out by design, and option (b) is a poor fit for a tool whose value proposition is being a small,
correct, protocol-conformant observer rather than a terminal scraper tied to one CLI's exact
prompt text.

**Is binary Working/Idle the honest ceiling without an extra signal?** Yes, and this survey
supports treating that as a considered design position rather than a compromise: ACP v2's own
`state_update` RFD, written independently by the protocol's maintainer, converges on the same
three buckets (`running`/`idle`/`requires_action`) that corral's Working/Needs-You/(folded-in)
already approximates. The gap between corral today and ACP v2's target state is exactly one
bucket: distinguishing `idle` (nothing to do) from `requires_action` (blocked on you) within what
corral currently lumps into Working. That is a precise, small, well-justified next step, not a
sign that the whole model is under-designed.

**What minimal signal would a harness need to expose to close that one gap?** Following the same
shape pi's `corral-announce` extension already uses for `turn_start`/`turn_end`, a pi extension
would need one more vendor notification — call it `_corral/state: "blocked"` — emitted whenever
pi's own tool-approval prompt opens, and cleared on approval, denial, or cancellation. This
requires no protocol change and no ACP spec work: it is the same mechanism as the existing
`_corral/state` `ExtNotification`, extended from two states to three, and it is exactly the shape
Claude Code's own `Notification` hook already uses in production (`permission_prompt` as a
distinct event from `idle_prompt`). The dependency is entirely on pi exposing a hook or event at
its own tool-approval boundary for the extension to observe; nothing in ACP blocks pi from doing
this, since it would be pi's own vendor addition, not a modification to the ACP union.

Longer term, corral should track ACP v2's `state_update` RFD
(agentclientprotocol/agent-client-protocol, `docs/rfds/v2/prompt.mdx`) rather than invest further
in the bespoke `_corral/state` protocol. If `state_update` stabilizes with a `requires_action`
variant, that is the point to retire the vendor notification and consume the standard one instead,
gaining forward-compatibility with any future ACP agent (not just pi) that implements v2 without
requiring each one to also adopt a corral-specific extension.

## Sources

- ACP schema (v2, unstable): raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/schema/v2/schema.json
- ACP v2 Prompt Lifecycle RFD: raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/docs/rfds/v2/prompt.mdx
- ACP v2 Permission Requests RFD: raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/docs/rfds/v2/permission-requests.mdx
- ACP v2 schema package version / commit history: github.com/agentclientprotocol/agent-client-protocol (schema/v2/Cargo.toml, commit a34b8965 2026-07-06, commit history on schema/v2/schema.json)
- Vibe Kanban: github.com/BloopAI/vibe-kanban (crates/db/src/models/task.rs, crates/db/src/models/execution_process.rs, crates/utils/src/approvals.rs, crates/executors/src/executors/acp/client.rs)
- Claude Squad: github.com/smtg-ai/claude-squad (session/instance.go, session/tmux/tmux.go, app/app.go)
- Crystal: github.com/stravu/crystal (main/src/types/session.ts, main/src/events.ts)
- uzi: github.com/devflowinc/uzi (cmd/ls/ls.go)
- HumanLayer: github.com/humanlayer/humanlayer (hld/session/types.go, hld/approval/manager.go)
- OpenHands SDK: github.com/OpenHands/software-agent-sdk (openhands-sdk/openhands/sdk/conversation/state.py)
- Codex CLI: github.com/openai/codex (codex-rs/protocol/src/protocol.rs, codex-rs/rollout-trace/README.md, codex-rs/core/src/session_rollout_init_error.rs)
- Claude Code hooks reference: docs.claude.com/en/docs/claude-code/hooks
- Backlog.md: github.com/MrLesk/Backlog.md (README.md)
- Aider: github.com/Aider-AI/aider (aider/io.py)
- Conductor: conductor.build/docs (concepts/agent-modes, concepts/parallel-agents, concepts/workflow) — general workflow model confirmed; exact internal status vocabulary and detection mechanism not published and not independently verified in this pass; Conductor is closed-source.
- corral (this repo): AGENTS.md, crates/board, extensions/corral-announce.ts

## Tools Considered but Dropped for Lack of Verifiable Primary Sources

Sculptor (Imbue), Terragon/Terry, sketch.dev, Cursor Background Agents, Devin/Cognition: each has
a public marketing or docs site, but in this pass none yielded server-rendered text describing an
internal status vocabulary or detection mechanism that could be cited to a specific page or source
file (Cursor's and Sculptor's docs are client-rendered single-page apps that did not return usable
text via a plain HTTP fetch; Devin, Terragon, and sketch.dev are closed products with no public
source and no docs page found describing status internals). Rather than restate marketing copy or
recalled impressions as fact, they are omitted per this report's citation discipline. Zed's agent
panel is covered indirectly: Zed is the maintaining organization of ACP itself, and the ACP
protocol section above is the primary, verifiable source for how Zed's client models handle agent
turn state.
