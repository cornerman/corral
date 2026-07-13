# Corral: The Open Coordination Layer for Many Agent Harnesses

Status: vision + direction (2026-07-13). Captures the "why corral, why now"
that the code-level `AGENTS.md` only implies, plus the reprioritized build
order that follows from it.

## One-Sentence Vision

Corral keeps tabs on your open agent and tool sessions and routes messages
across the boundaries between them (window, directory, harness, sandbox),
without owning how they start or forcing you to live in its interface. It is a
bus, not a container: the open coordination layer for a future of many agent
harnesses that all speak ACP.

## The Core, Stated Plainly

Strip corral to its essence and two jobs remain:

1. **Keep tabs on open sessions.** A live index of every running (and dormant,
   resumable) session, with which one needs your attention surfaced first.
   Think browser tabs, but for agent and tool sessions scattered across real
   OS windows.
2. **Route messages across contexts.** Deliver a message from one session into
   another across whatever boundary separates them.

Everything else (sandbox awareness, the sway/kitty focus and launch, the tray,
the review surface) is machinery in service of those two jobs. The value holds
even with zero sandboxing: the moment you run more than a couple of long-lived
sessions, "which needs me, jump there, message from here into there" earns its
place.

## The Differentiator: Compose, Don't Own

The current tools split into two camps, and corral is in neither.

**Owner-tools that swallow the agents.** Superset, Abralo, herdr, cmux, Orca,
Conductor. They create the worktrees, start the agents, run a persistent
daemon, embed terminals in their own window (Tauri or Electron), and add diff
review and editor integration. They make you *start through them* and *live in
their UI*. That is a walled garden: high switching cost, and each only works
for the agents it chose to embed. The Abralo author's own motivation is nearly
verbatim corral's premise: he ran split terminals but found it hard to see
which agents needed his attention, so he built a GUI that shows them all
(https://news.ycombinator.com/item?id=48832797).

**Terminal-native minimalists.** tmux driving sessions; Claude Code's built-in
`claude agents` view with left-arrow to jump back; plain split panes.
Lightweight, but single-multiplexer or single-vendor.

**Corral is a third thing.** It does not own launch and does not embed
terminals. You start agents however you like, in native terminals; corral
indexes them and points you at the *real* window (focus under sway, resume via
kitty). This is a philosophy, not a feature, and it is the thing the
owner-tools structurally cannot copy without abandoning their model.

## Why Now: Endless Harnesses and the Gap Above ACP

The bet is that we get *many* agent harnesses (pi, Claude Code, Codex,
OpenCode, Gemini CLI, Aider, and more), with none winning outright. History
rhymes: before LSP, every editor-times-language integration was bespoke
(M-times-N); LSP collapsed it to M-plus-N. ACP (the Agent Client Protocol) is
trying to be the LSP for a client talking to an agent.

The opening is that **ACP standardizes one client talking to one agent. It does
not standardize the layer above:**

- **Discovery / registry.** How do I enumerate every running agent session on
  this machine?
- **Cross-session attention.** Which of them needs me, aggregated into one
  view?
- **Cross-context routing.** How does a message get from this session into that
  one, across a boundary neither can cross alone?

Those three are exactly corral's pieces, and they sit on top of ACP, not
against it. Corral does not compete with ACP v2; it is the missing coordination
layer for a world where every agent already speaks it. "A better socket
registry solution" is the unclaimed slot. If the endless-harnesses bet is
right, something has to fill it.

Corral already tracks the ACP v2 Prompt Lifecycle RFD (the `state_update`
running / idle / requires_action vocabulary), so it is aligned with where the
protocol is going rather than betting against it.

## On Sandboxing: A Footnote, Not the Flag

Sandboxing per agent went mainstream in late 2025. Anthropic shipped native
sandboxing for Claude Code on the same primitives this system uses (Linux
bubblewrap, macOS Seatbelt, plus a network proxy), released as the open-source
`sandbox-runtime` (https://github.com/anthropic-experimental/sandbox-runtime,
https://www.anthropic.com/engineering/claude-code-sandboxing). So
"start every agent boxed" is now a first-party default, not an eccentric habit.

Two calibrated claims follow:

- **For positioning, do not lead with the sandbox.** It is a narrow motivation.
  Most people who "isolate multiple agents" today mean git worktrees
  (merge-safety on a shared host, not security) or a per-agent security box
  that protects the *host* from *one* agent. The model corral relies on, agents
  sealed so completely from *each other* that coordination needs a trusted
  broker, is still rare, because most setups keep a shared host so a daemon can
  reach everything. Corral answers a question most of the field has not asked
  yet.
- **For architecture, keep the sandbox-aware design anyway.** The shape it
  forced (workdir-local sockets, a non-owning broker, directory-keyed identity)
  is also the correct shape for the general multi-context case. It cost nothing
  extra and it is a correctness property. Demote the sandbox from headline to
  footnote; do not remove the code it inspired.

Note the dependency: corral's isolation primitive assumes the *whole* agent
process is boxed to its workdir (the nono/bwrap model, or a per-agent
container), which is what makes a sibling's socket unreachable. Claude Code's
built-in sandbox boxes the *bash tool*, not the whole process, so corral's
model maps to whole-agent boxing, not to a tool-level sandbox.

## Boundary With Subagents

Two tiers of multi-agent work, split by the box:

- **Inside one session: subagents.** pi's `spawn_agent` fans out helpers that
  share the sandbox and workdir, die with the parent, and have no window of
  their own. No escape from the box, no corral. Cheap and contained.
- **Across sessions: corral.** Reaching a *different* box (another workdir,
  another human-driven session) needs the broker. Persistent, its own window
  and history, survives the sender.

Decision rule: **does the work stay in this box? Use a subagent. Does it need
another box? Use corral.** The two stack; they do not compete. This is a second
reason corral should not grow orchestration features: pi's subagents already
own intra-task fan-out inside the box.

## Multiple Harnesses in One Repo: Same Dir or Different Dir

Give each harness its own directory (worktree or box) for anything concurrent;
reserve same-directory for advisory or turn-taking use.

- Two agents editing one working tree clobber each other (the failure every
  parallel-agent guide warns about, and the reason worktrees exist). Concurrent
  plus same directory equals corruption.
- A separate directory is simultaneously a worktree (merge-safe), a sandbox
  cell (nono boxing is per-directory), and the unit corral authorizes on
  (directory-keyed whitelist). One choice satisfies all three layers, which is
  why the pattern feels consistent.
- Session addressing (`target_session`) still routes a reply to the exact
  agent, so cross-directory back-and-forth stays precise.

So the pattern is pi and Claude Code each in their own directory, coordinating
through corral. Same-directory is sane only when the second agent is read-only
or takes turns, which is really a subagent-shaped need.

## Architecture Decisions Reached (2026-07-13)

- **One binary, one process, terminal TUI.** No board/daemon split and no
  `crates/core` reorg. The routing loop lives in the same process as the board,
  as it does today. Rationale: routing cannot function without the graphical
  session anyway (delivery spawns kitty, focus uses sway), so a headless daemon
  that "survives without the GUI" buys nothing. The two share one lifecycle by
  nature. The terminal TUI is deliberate: simple, and it fits terminal-native
  developers.
- **Corral owns behavior; the WM and nixos own lifecycle and visibility.**
  Keeping the process alive (systemd, `exec_always`) and showing or hiding its
  window (scratchpad) are deployment concerns in `~/nixos`, not corral code.
  The earlier silent-drop (messages queued undelivered) was a reliability
  failure: the scratchpad was not running, and `exec_always` only re-runs on WM
  reload, so a mid-session crash stayed dead. A systemd user service with
  restart-on-failure is the fix, and it lives in nixos.
- **The kitty and sway hardcodes are acceptable.** Corral already hardcodes
  `kitty -e pi` to spawn agents, so a systemd service running `kitty -e corral`
  for the board is the same hardcode, not new coupling. WM-agnosticism is a
  goal for the *core seams* (`WindowFocuser`, `Launcher`), not for the
  deployment glue.
- **Launch-with-message replaces the wait-for-announce dance.** pi accepts an
  initial message as a positional argument and submits it in interactive mode
  (verified in pi 0.80 `interactive-mode.js`: `initialMessage` and
  `initialMessages` are passed to `session.prompt`). So delivery to a
  not-yet-live target becomes `pi [--session <path>] "<message>"` at launch,
  atomic and race-free. This removes `RouteState`, the pre-existing-socket
  diffing, and `OpDelivery`/`pump_operator` from the router. Caveat: pi's
  parser has no `--` end-of-options marker and treats a leading `-` or `@` as a
  flag or file, so a launched message starting with those is space-guarded (pi
  trims the space before submitting).
- **Message submission moves from the outbox to a socket** at
  `~/.corral/corrald.sock` (reachable by the sandboxed extension because
  `~/.corral` is on its allowlist, the same reason the outbox file works). This
  gives an immediate ack ("accepted for routing", not "delivered") and turns
  "daemon down" from a silent queue into a visible connect failure. TCP is
  rejected: it breaks the "no network exposure, peer-auth via directory
  permissions" property. The registry stays filesystem-based, not a socket
  command, because it is broadcast state read by many consumers (board and
  router alike) and must survive restarts and work with no daemon running.

## Reprioritized Build Order

The land-grab (capturing the coordination-layer space) reorders the work. The
first two items are what turn "my private tool" into "the space"; the rest is
daily-use ergonomics.

1. **Publish the contract as a neutral convention.** (done) Specified in
   [CONVENTION.md](CONVENTION.md): the registry record, the workdir-local
   socket, the ACP surface, and the `state_update` broadcast, independently of
   pi *and* of corral, with a MUST/SHOULD/MAY conformance checklist so any
   harness or tool can implement it without reading corral's source.
2. **Prove cross-harness with a second adapter.** A Claude Code (or Codex)
   announce shim beside `corral-announce`. Two harnesses on one board is the
   demonstration that makes the claim real; one harness is just a tool.
3. **launch-with-message** (done): the launcher takes an optional initial
   message submitted as the new session's first prompt, so delivery to a
   not-yet-live target is atomic. Deleted `RouteState`, the pre-existing-socket
   diffing, and `OpDelivery`/`pump_operator` from the router.
4. **outbox to socket**: fail-loud ack on submit.
5. **systemd unit for the scratchpad** plus a hide behavior (nixos side for the
   unit; corral side for hide). The real reliability fix.
6. **Tray via `ksni`**: glanceable attention/pending count, click to open.
7. **Review surface**: a cohesive approval-review view showing the full prompt
   text, kept separate in code from the triage columns even though it is the
   same binary.

Items 3 through 7 improve corral for daily use; items 1 and 2 capture the space.

## Open Questions

- **Hide trigger.** When should corral hide its own board window without
  exiting (routing keeps running)? Research finding: vanilla sway/i3
  scratchpads do not auto-hide on focus loss (people bolt that on with scripts
  like `swayhide` or an `auto_scratchpad` watcher). So focusing the target does
  not hide the board for free; even the go case may need an explicit hide, and
  the dismiss-without-picking case (Esc, no target grabs focus) can only be
  resolved by corral hiding itself. Candidate mechanism: a `WindowHider` seam
  beside `WindowFocuser`, with a sway implementation that moves corral's own
  window to the scratchpad by walking `/proc` from corral's pid to its terminal
  window (the same parent-walk `SwayFocuser` already uses for other agents).
  Trigger set (dismiss-only vs after-every-action vs not-corral's-job) is
  undecided.
- **Non-agent contexts.** The generic contract already permits any long-running
  process (a build, a REPL, a remote session) to announce as a tab. Do not
  build for this; do not foreclose it. Add nothing until a real use appears.
