# corral — Architecture and Setup

This file and README.md MUST always be kept up to date when the setup,
architecture, or interfaces change.

README.md is deliberately short and developer-facing: the logo (the `∴` pen
mark), one-line statement of what corral is, a copy-paste Quick Start, the key
table, and links out. Inverted pyramid, most important first. Keep it that way
— do not grow it into a manual; depth belongs here in AGENTS.md, in
CONVENTION.md, and under docs/. When an interface changes, update the Quick
Start or key table, not the shape.

## TUI/GUI Parity (Hard Rule)

The TUI (`corral`) and GUI (`corral-gui`) are two parallel viewers of the same
registry and MUST stay in sync. Every user-facing feature, key, and card/board
behavior is implemented in BOTH shells, always — never land a feature in one
alone. Shared logic belongs in `corral-core` so both consume it; only the
rendering (ratatui vs iced) differs.

## What This Is

An attention board for locally running ACP agent sessions, plus the discovery
convention it rides on. Design premise: the user launches agents in arbitrary
terminals, never through a manager. Corral shows which agent needs attention
and jumps the user to its real window. It never drives an agent autonomously;
the operator may deliver a message to a selected agent (`m`), which the board
injects over that agent's socket.

Three binaries ship from this workspace, over a shared `corral-core` library.
`corral` is the board as a terminal TUI, and `corral-gui` is the same board as a
desktop (iced) window — two parallel presentation shells, both pure viewers of
the registry, launchable many times over. `corrald` is a headless singleton
daemon that owns inter-agent messaging (the control socket, the whitelist gate,
the approval tray). They share only the filesystem registry and never talk to
each other. The split falls out of two facts: exactly one process can own the
control socket (so messaging must be a singleton), while a registry reflector is
harmless to run many times over. The TUI is the zero-friction path (the terminal
supplies font, theme and flatness, runs over SSH, one tiny binary); the GUI is
the no-terminal launcher window.

Agent-agnostic by design; pi is the current proof of concept, announced via the
`corral-pi` extension. The board reads agent identity generically, so
other ACP agents are a matter of giving them a way to announce (see Future).

The registry record, workdir-local socket, and ACP surface are a harness-neutral
convention specified in `CONVENTION.md` (implement from that alone, no source
reading). This section is the architecture view of the same contract.

Discovery works through a per-session registry on the filesystem, not a
registry service. One record per session names a workdir-local socket:

```
$HOME/.corral/registry/<sessionId>.json   (dir 0700; override $CORRAL_REGISTRY_DIR)
  { sessionId, cwd, title, label, socket, spawnCommand, resumeCommand, lastSeen }
<cwd>/.corral/<label>-<pid>.sock           (dir 0700; override $CORRAL_SOCKET_DIR)
```

A pi extension writes the record and binds the socket on `session_start`, then
on clean shutdown unlinks the socket and clears the record's `socket` to null
(leaving a dormant, resumable entry). The socket lives inside the session's own
working directory, so only that session (and unsandboxed tools like corral) can
reach it; that workdir-local isolation is the primitive a later messaging layer
relies on. Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it. Corral scans
the registry to find each socket (it could never scan the scattered workdirs
directly), then talks plain ACP (JSON-RPC, newline-delimited, as on stdio) to
that agent. A record with `socket == null` is dormant (rendering dormant
sessions is a later stage).

## Data Flow

Two independent processes read the same filesystem registry; they never talk to
each other. The board (`corral`) reflects and drives the operator's own actions;
the daemon (`corrald`) is the singleton that owns inter-agent messaging.

```
your terminal (pi, interactive TUI)              another terminal
  pi -e extensions/corral-pi.ts              corral (attention board, launch many)
    |  writes ~/.corral/registry/<id>.json           |  scans the registry (1s)
    |  binds <cwd>/.corral/pi-<pid>.sock              |  one watch connection per live socket:
    |    on session_start                            |    initialize + session/list (seed)
    |  serves ACP beside the live TUI:               |    streams state_update -> column
    |    initialize, session/list, prompt, cancel    |  Enter -> focus or resume
    |  broadcasts activity + state_update            |  shift+enter -> spawn agent (terminal)
    |  clears socket + unlinks on session_shutdown   |  m -> send prompt DIRECT (ungated,
    |                                                |       operator is trusted)
    |
  corral_message_agent tool -> ~/.corral/corrald.sock ----+  corrald (daemon, ONE singleton)
    (asks to message a target dir or session)            per submission (control.rs):
    <- ack: accepted / approval_needed /                 parse, find recipient, ack, then
       recipient_not_found / directory_not_known         enqueue to the router: authorize
                                                         (whitelist + tray/notify popup),
                                                         resolve dir/session (spawn/resume
                                                         if needed), inject w/ provenance tag
```

The operator's `m` and the agent-initiated path split on trust: the operator is
the trusting authority, so `m` delivers directly and ungated from the board; an
agent's message is gated by the whitelist and the approval popup, which is why
it routes through the daemon. The approval gate maps exactly onto the daemon
boundary. The daemon is a singleton because exactly one process may own the
control socket; the board is a pure registry reflector, so any number may run.

## Crates

Four workspace crates: `core` (shared logic), `board` (TUI `corral`), `gui`
(desktop `corral-gui`), `daemon` (`corrald`). `core` holds everything the three
binaries share, so no UI links another's dependencies (board + gui keep
ratatui / iced, the daemon keeps ksni).

- `crates/core` — `corral-core` (lib): the shared foundation, UI-free
  (`serde_json` only).
  - `src/discovery.rs` — scan the registry dir, parse each `<sessionId>.json`
    record, and resolve a live record to its socket (parsing the
    `<label>-<pid>.sock` filename). Liveness is read straight from the record
    (`socket` set = live, cleared = dormant); the optional `hidden` field marks
    a session running headless. Pure, unit-tested.
  - `src/launch.rs` — `Launcher` seam. `TerminalLauncher::launch(cwd, command,
    message, mode)` takes a `LaunchMode { gui, message_flag, hidden }` bundling
    the record-derived launch options (built by callers via `Agent::launch_mode`
    / `RegistryEntry::launch_mode`, keeping this crate model-free). A `hidden`
    launch wraps the argv as `env WLR_BACKENDS=headless CORRAL_HIDDEN=1 cage --
    <argv…>`: `cage` is a headless compositor so the window never maps on the
    host (the `WLR_BACKENDS=headless` env is load-bearing — else wlroots opens
    an X11 window on an X11 host), and `CORRAL_HIDDEN` tells the adapter to
    record `hidden`; covers terminal and `gui` agents alike (XWayland). It runs
    `setsid --fork <terminal…> <command…>` rooted at `cwd`
    via the child's working directory (no terminal-specific `--directory`
    flag), where `command` is the argv the registry record carried
    (`spawnCommand` for a fresh session, `resumeCommand` to resume an exact
    one). A GUI agent (`mode.gui`, e.g. quine, from the record's `gui` field)
    is launched directly as `setsid --fork <command…>` — no terminal resolved,
    since the app draws its own window; the pure `setsid_args` builder (which
    branch to take) is unit-tested. corral names
    neither the agent kind (the command rides in the record, so the board
    launches whatever kind the selected card is — pi, opencode, quine, …; pi's
    `--session` grammar lives in the announce extension) nor the terminal:
    `resolve_terminal` picks one from the environment by a ladder —
    `xdg-terminal-exec` (the freedesktop standard) → `$CORRAL_TERMINAL` (an
    explicit argv template, e.g. `"alacritty -e"`) → `$TERMINAL -e`; no
    hardcoded terminal, so if none resolve `launch` errors and the shell shows
    it. `setsid --fork` detaches the window (survives the launcher exiting, no
    zombie, and — since it is not a descendant — the focus parent-walk cannot
    climb into corral's own window; a GUI agent is matched by its own pid, so
    focus never targets its launching terminal — see `focus.rs`
    `match_pids`). An optional initial `message` is appended
    as the final positional arg (space-guarding a leading `-`/`@` as a generic
    CLI-safety convention), or — when the record sets `messageFlag` (e.g.
    `--message` for quine) — as that flag's value (`… --message "<text>"`,
    bound to the flag so unguarded), so a message is delivered atomically at
    launch. The
    pure `resolve_terminal_from` ladder and `with_message` are unit-tested.
    `default_cwd` takes a plain cwd (not an `Agent`) so the crate stays free of
    the board's model.
  - `src/prompt.rs` — `send_prompt`: deliver a user message to a live agent by
    opening a one-shot connection to its socket and writing a `session/prompt`
    request (fire-and-forget). Used by the board (operator `m`) and the daemon
    (agent delivery to a live target). Unit-tested against a throwaway listener.
  - `src/paths.rs` — the well-known on-disk locations (`registry_dir`,
    `control_socket`, `whitelist_file`), each the `env` override or a fixed name
    under `~/.corral`. Shared so all binaries agree on where things live.
  - `src/model.rs` — `Agent`/`Board`/`Column`/`State` (the ACP v2
    `state_update` vocabulary; `Column::ALL` is the single source of column
    order). `Board.set_filter` + `Agent::matches_query` power the inline content
    filter (a fuzzy subsequence match over title / cwd / activity / state /
    harness label), applied inside `column`, which also groups cards by cwd
    with the most-used directories first, so counts, selection, rendering and
    hit-testing all narrow and order together. `Agent`/`RegistryEntry` carry a
    `hidden` flag (from the record); `sync_registry` stamps it — and the
    `resume_command` — onto live agents too, so a live hidden card can be
    revealed/hidden by resume. Pure, unit-tested.
  - `src/watch.rs` — one reader thread per live socket: seeds from
    `initialize` + `session/list`, then streams `state_update`, `tool_call`
    (summarized to a card activity string), and title updates; EOF = gone.
  - `src/engine.rs` — the shared registry-reflect loop: on a ~1s cadence
    scan + prune the registry, spawn a watcher per live socket, fold updates
    into the `Board`, and track per-agent age timers. A shell calls `tick`
    then renders `board()` + the age maps. (The TUI still runs an equivalent
    loop inline; converging it onto the engine, or retiring it, is a TODO.)
  - `src/nav.rs` — pure selection math: move within a column or across columns
    over the per-column counts. `at_vertical_edge` / `column_entry` ring the
    filter input into the vertical cycle (the input sits above the first row
    and below the last), so a shell hands focus to the input at a column edge
    and steps back onto the first/last row when leaving it. Unit-tested.
  - `src/palette.rs` — `color_index`: a stable FNV-1a hash of a cwd path into a
    palette bucket, so both shells color a directory's basename pill the same
    way (same color per path, keyed on the full path). Pure, unit-tested; each
    shell owns its own palette (ratatui ANSI vs base16 accents).
  - `src/focus.rs` — `WindowFocuser` seam, with `focus::detect()` picking an
    implementation by session: EWMH on X11; sway / Hyprland / niri on Wayland.
    `X11Focuser` (via `x11rb`, no libX11) finds the
    window by matching `_NET_WM_PID` against the agent's pid and its ancestors
    (the terminal owning the window is an ancestor of the socket pid; a GUI
    agent's own pid owns its window, so `match_pids` narrows the set to just
    the socket pid for `gui` records — never climbing to the launching
    terminal), then
    activates it with `_NET_ACTIVE_WINDOW` (source indication 2 = pager, and a
    real server timestamp fetched via a property round-trip, so focus-stealing
    prevention does not defer it — this also switches workspaces); `close`
    kills the window's `_NET_WM_PID`. Works on any EWMH WM (i3, bspwm, openbox,
    X11 KWin/Mutter). On Wayland the compositor's own IPC is the only
    path that reports pid (and switches workspaces): `SwayFocuser` correlates
    by a `/proc` parent-walk (socket pid up the PPid chain to the terminal pid
    sway reports) then `swaymsg [con_id=..] focus`; `HyprlandFocuser` matches
    the pid in `hyprctl clients -j` then focuses by window address;
    `NiriFocuser` matches the pid in `niri msg --json windows` then
    `focus-window --id`. `detect()` picks Hyprland/niri/sway by the marker env
    var each exports (`HYPRLAND_INSTANCE_SIGNATURE` / `NIRI_SOCKET` / else
    sway). All `close` by killing the window pid. The sway tree walk and the
    pid-matching are unit-tested. GNOME/KDE on Wayland have no such pid path (a
    Shell extension / KWin script would be needed) and are unsupported; they
    focus fine under X11 via EWMH; `detect()` returns an `UnsupportedFocuser`
    there whose `focus`/`close` fail loud (the shell shows the message). The
    pure `detect_kind` classifier is unit-tested across all sessions.
  - `src/picker.rs` — a directory-grouped fuzzy picker (library module,
    unit-tested). No longer wired to a shell now that both filter inline; kept
    for reuse.
  - `src/placement.rs` — hidden-agent placement: `placement_for(origin, hidden)`
    decides the `h`-toggle (`Reveal` a live hidden agent, `Hide` a visible one,
    `StartHidden` a dormant one), and `apply_placement` executes it as
    kill-and-resume (a live surface cannot migrate between compositors, so
    every transition stops the instance and relaunches on the other side:
    reveal kills the pid then resumes visible, hide closes the window via the
    focuser then resumes hidden, start-hidden just resumes hidden). `kill_pid`
    is the real kill the shells pass; the pure decision and the executor (with
    a stubbed kill) are unit-tested.

- `crates/board` — the TUI attention board (binary name `corral`), a pure
  viewer of the registry plus the operator's own actions. Holds no messaging
  state; launch it as many times as you like.
  - `src/ui.rs` — ratatui: a prominent centered filter box (underline, top
    padding) over four columns (Requires Action / Idle / Running / Dormant) of
    fixed-height cards, and a clickable key-hint footer. Each card is two rows:
    the session title with the column age dim at the top-right, then a
    hash-colored cwd basename pill (see `core::palette`), the kind badge, and
    the activity hint. Owns card / heading / footer / filter-box / cwd-pill /
    age formatting; `column_layout` and `hit_test` share one geometry (top rows
    reserved for the filter).
  - `src/main.rs` — the imperative shell: scan/prune/watch/draw/dispatch. `/`
    focuses the inline filter (narrows cards by whole content). The filter
    input rings with the board vertically (`core::nav`): Down/Up off the input
    step into the selected column's first/last row (blurring the field, so
    m/d/h act as commands there), and Down/Up at a column's bottom/top edge ring
    back to the input. While filtering
    Enter goes / Shift+Enter spawns directly, arrows navigate. Command keys:
    Up/Down, Left/Right, Enter go, Shift+Enter spawn, `m` message
    (compose overlay), `d` dismiss, `h` toggle hidden (hide a visible session,
    reveal a hidden one, start a dormant one hidden — all via
    `core::placement`), `q` quit; a two-stage left click and a
    clickable footer. Enter on a live hidden card reveals it (resume) rather
    than focusing a non-existent window; Shift+Enter beside a hidden card
    spawns the new agent hidden too (placement follows the selected card).
    A live hidden card shows a dim `hidden` badge. Esc peels one layer per press
    (edit-mode blur -> clear filter) but never exits the normal board — q is the
    sole quit, so a stray Esc cannot nuke the window; matches the GUI.
    `--launcher` opens the TUI as an ephemeral popup: filter focused at boot, a
    successful go/spawn exits the process, as does a single Esc (dismiss the
    throwaway summon at once, no peeling), mirroring the GUI launcher.
    Operator `m` delivers ungated via `core::prompt` / `core::launch`; no
    router.

- `crates/gui` — the desktop attention board (binary name `corral-gui`,
  iced), a second parallel viewer over the same registry via
  `core::engine::Engine`. iced renders text via cosmic-text (crisp, shaped),
  the reason for the toolkit over egui. Flat and base16-themed (dark/light
  polled from the freedesktop appearance portal); an underline-only centered
  filter over the four columns of fixed-height cards; click a card to go
  (two-stage: select then go), Shift+Enter spawns the selected card's kind in
  its dir, a bottom key-hint footer with the canvas-drawn corral mark, the same
  keys as the TUI. Each card is two rows: the title with the column age at the
  top-right, then a hash-colored cwd basename pill (`core::palette`, same color
  per directory) beside the dim kind badge and the activity hint; the Dormant
  column is faded. `src/theme.rs` is the base16 theming
  system: a lenient tinted-theming YAML parser (no YAML dependency), Solarized
  dark/light built-ins, and an env-selected (`CORRAL_THEME_DARK` /
  `CORRAL_THEME_LIGHT`) dark/light preset pair loaded from built-ins plus
  `~/.config/corral/themes` (override `CORRAL_THEME_DIR`); `src/dashboard.rs`
  maps a `Base16` onto iced styling and drives the actions (focus/resume via
  `core::focus`/`core::launch`, message compose, dismiss). Links iced's wgpu
  graphics stack (vulkan-loader/libGL/wayland/X11/xkbcommon); the TUI and daemon do
  not.

- `crates/daemon` — the message-routing daemon (binary name `corrald`), a
  headless singleton. Owns the control socket and every gated (agent-initiated)
  delivery; the approval gate surfaces on a tray and desktop notifications.
  Reads liveness from the registry (no socket watching — that is the board's
  job).
  - `src/mailbox.rs` — cross-session message types: parse a submitted message (a
    `Target` is a directory or an exact session id), `classify` it into an `Ack`
    (accepted / approval_needed / recipient_not_found / directory_not_known)
    from resolved facts, add the `[from agent in <dir> (session <id>)]`
    provenance/reply-handle tag, and read/append the `(sender -> target)`
    whitelist. Pure, unit-tested.
  - `src/control.rs` — the control socket (`~/.corral/corrald.sock`, override
    `$CORRAL_CONTROL_SOCKET`). A background thread accepts one submission per
    connection: read the request line, parse, find the recipient (registry
    scan for a session, dir-exists for a directory), ack the verdict, and (if
    routable) hand the message to the router over a channel. `serve` fails loud
    on a bind error and `is_serving` is the singleton guard (a live listener
    means another corrald owns the socket, so the second refuses to start).
    Ack is synchronous; delivery and the approval gate run later. Unit-tested
    against a throwaway socket.
  - `src/router.rs` — `Router`: routes agent-initiated messages (enqueued from
    the control socket) using a fresh registry scan as its whole view of who is
    live and dormant. A directory target reuses a live agent over its socket or
    spawns one; a session target delivers to the live socket or resumes its
    record. A live socket that fails to connect (crashed session) falls back to
    spawn/resume — the daemon needs no dead-socket tracking. Delivery to a
    not-yet-live target carries the message as the new session's first prompt
    (launch-with-message), atomic with no wait-for-announce; a spawn or resume
    it triggers runs **hidden** (agent-initiated windows must never pop up).
    Holds an in-memory
    queue (no file spool), the authorization decisions, and the one message
    awaiting operator approval; owns `ApprovalAction` and `apply`. Unit-tested
    (gating, spawn-with-message, live + dormant session delivery, allow/deny,
    unknown-session drop).
  - `src/notify.rs` — `ApprovalNotifier` seam. `NotifySendNotifier` mirrors a
    pending approval to a desktop notification with Allow once / Allow always /
    Deny buttons (`notify-send -A`), reporting the choice back on a channel
    tagged with the message id. Best-effort and non-blocking (a thread per
    notification); the tray path always works too. Pure name mapping is
    unit-tested.
  - `src/tray.rs` — the `ksni`/StatusNotifierItem tray, the daemon's
    always-present control surface and the reliable approval path. Shows whether
    a message is waiting and offers Allow once / Allow always / Deny, plus open
    the board and quit. The tray thread cannot touch the router, so each action
    becomes a `TrayCommand` on a channel the main loop drains. Best-effort: no
    StatusNotifierHost means notification-only approval, nothing else changes.
  - `src/main.rs` — the headless loop: refuse to start if another corrald is
    live (`is_serving`), else bind the control socket (fail loud), then each
    tick drain accepted messages, scan the registry, route, reflect a new
    pending approval to the tray + a notification (once), and apply decisions
    from either (guarded on the current pending id).

## Extensions

- `extensions/corral-pi.ts` — pi extension announcing an interactive pi
  session: on `session_start` it writes the registry record and binds the
  workdir-local socket; on `session_shutdown` it clears the record's `socket`
  and unlinks. It writes `spawnCommand` (`["pi"]`) and `resumeCommand`
  (`["pi","--session",<sessionId>]`) so corral launches/resumes it without
  naming pi; `resumeCommand` is gated on the session file actually existing, so
  an empty session pi never persisted is not advertised as resumable (else
  resume hits `No session found` and the window closes). Registry writes are
  crash-safe: `stop()` clears the socket by rewriting the known registry file
  with no ctx, and `writeRegistry` is guarded, because on a resume/replacement
  the captured ctx goes stale and touching `ctx.sessionManager` would otherwise
  throw and kill pi. The record's `lastSeen` refreshes on `turn_end` and its
  `title` on rename. The title broadcasts whenever it changes, on rename and on
  `turn_end` (so the first-user-message fallback title reaches clients that
  connected before it existed, not only explicit renames). Serves `initialize`, `session/list` (id, title,
  cwd), `session/prompt` (injects via `pi.sendUserMessage`; queued as follow-up
  while busy; responds with stopReason once the message queue drains, coarse,
  documented in-file), `session/cancel` -> abort. Broadcasts to all connected
  clients: `session/update` message and tool events (whole messages on
  `message_end`; token deltas deferred), `session_info_update` on rename; and
  the standard `state_update` (running/idle/requires_action) on
  `turn_start`/`turn_end` and while the interactive `question` tool blocks on
  the user. A newly connected client is seeded with the current `state_update`.
  Serves multiple concurrent clients. Also registers a `corral_message_agent` tool
  (`target_dir` or `target_session`, `message`, `force_new`, optional `label`)
  that submits a
  cross-session message over `~/.corral/corrald.sock` (stamped with the
  sender's `fromSession` as a reply handle) and reports corral's ack (accepted
  / approval_needed / recipient_not_found / directory_not_known); a connect failure is
  surfaced as "corrald not running" (fail loud, no silent queue). Install:
  symlink into
  `~/.pi/agent/extensions/`.

- `extensions/corral-opencode.ts` — the second worked adapter (an opencode
  plugin), proving the convention is harness-neutral: corral itself needs zero
  changes, since it runs the record's launch commands and reads its `label`
  verbatim. It mirrors `corral-pi` closely and deviates only where
  opencode's API forces it. It binds the same workdir-local socket
  (`<cwd>/.corral/opencode-<pid>.sock`), writes the same registry record with
  `label: "opencode"`, `spawnCommand` (`["opencode"]`) and `resumeCommand`
  (`["opencode","--session",<sessionId>]`), and serves the same ACP surface
  (`initialize`, `session/list`, `session/prompt` injecting via the opencode SDK
  client fire-and-forget and resolving on the next `session.idle`,
  `session/cancel` -> abort). It broadcasts the same `session/update` set:
  message and tool activity, `session_info_update` on rename, and the standard
  `state_update` (running on the first turn signal, idle on `session.idle`,
  `requires_action` while a permission prompt is open via `permission.updated`,
  cleared on `permission.replied`). It tracks a single active session per window
  (multi-session multiplexing is deferred) and, lacking a plugin-unload hook,
  clears the record's socket and unlinks on process exit/SIGINT/SIGTERM;
  best-effort, since corral's dead-socket sweep makes a missed teardown dormant
  anyway. It registers the same `corral_message_agent` tool via opencode's
  `tool` hook. Untypechecked in this repo (no opencode toolchain here), so the
  plugin API shapes are probed defensively at runtime and flagged UNVERIFIED
  in-file. Install: symlink into `~/.config/opencode/plugin/` (global) or
  `.opencode/plugin/` (project).

- `extensions/corral-claude/` — the third adapter (Claude Code), shaped
  differently because Claude Code has no in-process plugin runtime that can hold
  a socket or inject into the live session (its hooks are subprocesses that
  exit; its ACP mode is a separate headless stdio server). So it splits in two:
  a resident `sidecar.ts` (one per session) holds the ACP socket, keeps triage
  state, and queues messages; a thin `hook.ts` shim Claude runs per hook event
  bridges the event to the sidecar over a per-session control socket
  (`<cwd>/.corral/.claude-ctl-<sessionId>.sock`) beside the ACP socket
  (`claude-<claudePid>.sock`, pid = the interactive Claude process so focus
  correlation works). SessionStart spawns the sidecar detached; SessionEnd (or a
  5s Claude-liveness probe, or corral's dead-socket sweep) reaps it. Live-session
  delivery uses Claude's own hook feedback, always via the synchronous `Stop`
  hook so the text is visible: it returns `decision:block` (reason = the queued
  message as the next instruction) plus `systemMessage` (the one hook field shown
  to the user, so the message shows in the transcript, not an opaque "Stop hook
  feedback" line). An `asyncRewake` hook is a doorbell only, armed on both
  `SessionStart` and `Stop`: on an idle session it exits 2 with a neutral wake
  note to make the next `Stop` fire and deliver visibly; it never carries the
  message text. Arming at `SessionStart` (not only `Stop`) is what lets a message
  reach a session that has not taken its first turn — without it no `Stop` has
  fired to arm the doorbell, so the message would wait for the first user prompt. `state_update` is native and richer than pi's
  (`UserPromptSubmit`->running, `Stop`->idle,
  `Notification[permission_prompt]`->requires_action, a real approval gate);
  `session/cancel` is a no-op (no external turn-abort). Runs on `node` (not
  bun: bun's JavaScriptCore SIGTRAP-crashes under a Landlock sandbox, which is
  how Claude runs on hardened setups; node runs the `.ts` directly via native
  type-stripping, >= 22.18 / 24, no build step), external to Claude. So a
  sandboxed harness must also grant its jail read/write to `~/.corral` (the
  registry + `corrald.sock`), or the sidecar cannot register. UNVERIFIED in
  this repo (no Claude harness here): hook payload
  fields and the block/asyncRewake injection semantics are coded from the hooks
  reference and probed defensively. Ships as a Claude Code **plugin**
  (`.claude-plugin/plugin.json` + `hooks/hooks.json` using `${CLAUDE_PLUGIN_ROOT}`,
  so no hardcoded paths and no `settings.json` hand-edit); installable via the
  repo-root `.claude-plugin/marketplace.json` (`claude plugin marketplace add
  cornerman/corral` then `claude plugin install corral-claude@corral`), a
  `~/.claude/skills/` symlink (zero-install skills-dir plugin), or
  `--plugin-dir`. See `extensions/corral-claude/README.md`.

- `extensions/corral-cursor/` — the fourth adapter (Cursor desktop IDE), the
  first GUI-editor kind and the first shipped as a VS Code **extension** (VSIX).
  Cursor exposes no API to observe or drive its Composer agent and its hooks
  cannot inject, but the extension host is a resident in-process runtime, so
  — unlike `corral-claude` — there is no sidecar: the extension itself is the
  resident owner. `extension.js` resolves the Cursor window's Electron pid,
  binds `<cwd>/.corral/cursor-<electronPid>.sock` (that pid so corral's `gui`
  focus by `match_pids` raises the real window), writes a `gui: true` record
  (`label: "cursor"`, `spawnCommand`/`resumeCommand` = `["cursor", <cwd>]`, no
  `messageFlag`), serves the ACP surface, and answers `session/prompt` by
  opening a **new** pre-filled Composer chat (a prompt must land in a chat; a
  fresh one avoids intruding). State is fed by a thin `state-hook.js` the
  extension auto-registers in `~/.cursor/hooks.json` (additive, idempotent),
  mapping `beforeSubmitPrompt`→running / `stop`→idle over a
  `.cursor-ctl-<sessionId>.sock` control channel; coarse (no `requires_action`,
  Cursor exposes no permission hook). One card per window (a chat can be neither
  focused nor resumed independently); dormant delivery reopens the folder
  without the message text. Authored in plain JavaScript (no build step: the
  host loads `main` as JS); the pure core (`lib.js`) is unit-tested with
  `node --test`. UNVERIFIED in this repo (no Cursor here): the Composer inject
  command id(s), the Electron-pid walk / `_NET_WM_PID` equality, and the hook
  payload fields — all guarded so the extension never throws into the host.
  Requires `node` on PATH. Install: `cursor --install-extension`, copy into
  `~/.cursor/extensions/`, or `--extensionDevelopmentPath`. corral needed no
  change. See `extensions/corral-cursor/README.md`.

## Hidden Agents

A session can run **hidden**: fully alive (socket bound, announcing, doing
background work, driving `state_update`) but with no window on the host. corral
launches it inside a per-agent headless `cage` (`env WLR_BACKENDS=headless
CORRAL_HIDDEN=1 cage -- <argv…>`, see `core::launch`), which never touches the
host display server and hosts terminal and `gui:true` agents alike via
XWayland. The `CORRAL_HIDDEN=1` env makes the adapter record `hidden: true`, the
signal the board reads (`core::discovery`) to render a `hidden` badge and to
reveal by resume.

Reveal/hide is never a live move: a Wayland/X surface cannot migrate between
compositors, so `core::placement` does kill-and-resume in every direction —
reveal kills the hidden instance and resumes it visibly, hide closes the visible
window and resumes it into a cage, start-hidden resumes a dormant record hidden.
`h` in either shell toggles placement; Enter (go) reveals a live hidden card;
Shift+Enter beside a hidden card spawns hidden too; `m` delivers to a hidden
agent without unhiding it. The one physics cost: reveal/hide loses any
un-persisted mid-turn state (the transcript survives). `cage` ships via the
flake; a hidden spawn with cage absent fails loud.

The original driver: `corral_message_agent` `force_new` and dir-spawns route
through corrald, which spawns the new agent **hidden by default**, so an
uninvited agent never pops a window — it shows as a hidden card the operator
reveals on demand.

## Inter-Agent Messaging

Sandboxed agents cannot reach each other's sockets (each is workdir-local), so
the `corrald` daemon is the sole trusted cross-workdir router. An agent calls
`corral_message_agent`, which submits the message over `~/.corral/corrald.sock`
(reachable because `~/.corral` is on the sandbox allowlist). corrald parses it,
finds the recipient, and returns a synchronous ack: `recipient_not_found` /
`directory_not_known` if there is nowhere to send, `approval_needed` if the
`(sender-dir -> target-dir)` pair needs approval, else `accepted`. A connect
failure means corrald is down, so submission fails loud instead of queuing
silently. Routable messages are then routed asynchronously: corrald authorizes
the pair against the whitelist (or asks the operator on its tray menu — Allow
once / Allow always / Deny), resolves the target, and injects the message over
that agent's socket with a `[from agent in <dir> (session <id>)]` provenance
tag. The approval gate is not awaited by the sender (a human is unbounded): a
`approval_needed` message is acked at once and delivered after approval, without
a delivery ack. Delivery reuses `core::prompt::send_prompt`, the same path as the
board's operator messaging (`m`). A pending approval is also mirrored to a
desktop notification (`notify-send -A`) whose Allow/Deny buttons resolve it
without opening the tray; best-effort, and the tray menu stays available. The
approval lives in the daemon, not the board — the board is a pure viewer and
never sees these messages.

The operator's own `m` (from the board) is ungated and does not go through
corrald: the operator is the trusting authority, so gating `m` would mean asking
the operator to approve the operator. The board delivers `m` directly (live over
the socket, dormant by resume-with-message). This is why the approval gate and
the daemon boundary coincide.

A message is addressed either by **directory** (`target_dir`: reach whoever
works there, spawning one if none, or a dedicated one for `force_new`) or by
**session id** (`target_session`: reach that exact agent, resuming it from its
dormant record if not live). When a `target_dir` message has to spawn a fresh
agent, the optional `label` picks its kind (matched against a record's `label`,
resolved from any directory so a kind seen anywhere can start here); omitted, it
falls back to that directory's own record kind, and an unknown label fails loud
instead of spawning an arbitrary kind. Session addressing is what makes a reply precise:
the provenance tag carries the sender's session id as a reply handle, so the
receiver answers with `corral_message_agent(target_session = ..)` and it lands on the
agent that actually asked, never a sibling that happens to share the directory.
Authorization is always keyed on the `(sender-dir -> target-dir)` pair (a
session target resolves to its cwd), since directories are the stable, human-
meaningful unit. Fire-and-forget: no reply is auto-routed; the receiver sends a
new message back using the reply handle.

## ACP Conformance

Corral tracks the ACP v2 Prompt Lifecycle RFD
(agentclientprotocol.com/rfds/v2/prompt), which adds a `state_update`
session/update with `running` / `idle` / `requires_action`, broadcast by the
agent to every client (not just the prompt sender). corral-pi emits that
exact shape and vocabulary now, ahead of stabilization, so there is zero
migration when v2 lands and any future ACP agent works unchanged. Tradeoff: a
strict v1-only client that rejects unknown `sessionUpdate` variants would not
recognize `state_update` until v2; acceptable because corral is the consumer
here. The rest of the surface (initialize, session/list, prompt, cancel,
message/tool updates) is ACP v1.

## Interfaces to the Outside World

- CLI `corral` — full-screen TUI, four columns: Requires Action, Idle, Running,
  Dormant. Up/Down (or scroll) move within a column; Left/Right switch
  columns; Enter or left-click goes to the selected agent (focus a
  live window, resume a dormant session by running its `resumeCommand`);
  Shift+Enter spawns a fresh agent of the selected card's kind (its
  `spawnCommand`) in the selected agent's cwd; `/` focuses a prominent
  centered filter box that fuzzily narrows the cards by their content (title /
  cwd / activity / state / harness label), cards grouped by cwd with the
  most-used directories first; while filtering, Enter goes and Shift+Enter spawns
  directly, arrows still navigate, Esc clears (never exits the normal board);
  `m` compose a
  message to any agent — delivered to a live one over its socket, or a dormant
  one by resuming it with the message as its first prompt; `d` close the
  selected live agent (kill its terminal process, closing the window; pi then
  goes dormant and resumable) or forget the selected dormant record; `q` quits
  (the sole quit), and Esc peels one layer per press (edit-mode blur, then clear
  filter) but never exits the normal board. `--launcher` opens the TUI as an
  ephemeral popup (filter focused, a successful go/spawn exits, as does a single
  Esc), the same as `corral-gui --launcher`. A left click
  is two-stage: first click selects, a click on the
  already-selected card goes. Shift+Enter needs the kitty keyboard protocol
  (corral pushes it where supported). Long columns scroll to keep the selection
  visible; live cards show time-in-state. Reads `$HOME` (or
  `$CORRAL_REGISTRY_DIR`) for the registry dir; uses `swaymsg` and `kitty` for
  focus and spawn.
- CLI `corral-gui` — the same attention board as a desktop (iced) window,
  a second parallel viewer for when no terminal is wanted. Flat,
  base16-Solarized, follows the system light/dark (freedesktop appearance
  portal). A centered filter line over the four columns; click a card to go,
  `+ new` to spawn, arrows / Enter / Shift+Enter / `m` / `d` / `/` as in the
  TUI, a bottom key-hint footer. Links the graphics libs (libGL / wayland / X11
  / xkbcommon); on NixOS the flake wraps it with the driver library path. The
  tray's “Open board” launches this.
- CLI `corrald` — the headless message-routing daemon. No TUI; run under a
  systemd user service (see Development Setup). Binds `$HOME/.corral/corrald.sock`
  (override `$CORRAL_CONTROL_SOCKET`); refuses to start if another corrald is
  already live on it. Surfaces the approval gate on a `ksni` tray (Allow once /
  Allow always / Deny, plus open the board and quit) and a `notify-send`
  mirror. Uses the environment-resolved terminal to spawn/resume delivery
  targets. Reads the same registry
  as the board.
- pi extension `corral-pi` — see Extensions above.
- Registry records in `$HOME/.corral/registry/` and unix sockets in each
  `<cwd>/.corral/` (both created 0700; override with `$CORRAL_REGISTRY_DIR` /
  `$CORRAL_SOCKET_DIR`). No TCP ports, no network exposure. Peer authentication
  relies on the directory permissions.
- Inter-agent messaging: `corral_message_agent` submits over
  `$HOME/.corral/corrald.sock` (override `$CORRAL_CONTROL_SOCKET`), the daemon's
  control socket; no TCP, peer auth by directory permissions. corrald authorizes
  `(sender -> target)` dir pairs against `$HOME/.corral/whitelist` (override
  `$CORRAL_WHITELIST`) plus the operator's tray/notification popup. A message
  accepted over the socket lives only in corrald's memory until routed (no
  on-disk spool): a corrald crash before routing loses it, an accepted tradeoff
  under the fire-and-forget contract and the systemd keep-alive.

## Known Limitations (v1, deliberate)

- `requires_action` is emitted today only for the interactive `question` tool
  (the one user-input gate an extension can observe). pi's built-in
  tool-approval prompt is not surfaced to extensions, so an approval-blocked
  agent still shows as Running until pi exposes that gate (see Future).
- Focus correlation assumes the pi process and its terminal window share the
  host PID namespace (true under the current nono/bwrap sandbox). If a sandbox
  unshares PIDs, the `/proc` parent-walk cannot reach the window pid.
  Board-spawned windows are detached (`setsid --fork`) so the walk terminates
  at the agent's own terminal rather than climbing into corral's window.
- A transient watch read error reports the agent gone; the next 1s scan
  reconnects. A genuinely dead socket (crashed pi) reconnects-and-drops cheaply
  once per second until its file disappears.
- corral-pi answers `session/new`/`session/load` with method-not-
  supported: clients can discover, watch, and prompt running pi sessions, but
  attaching with history replay is not yet served.
- corral-pi's `session/prompt` responses resolve for all waiting clients
  at once when the queue drains (no per-message turn attribution).
- Approvals stay in the pi TUI; socket clients never receive
  `session/request_permission`.
- Inter-agent messaging is fire-and-forget (v1): corrald injects the message and
  does not capture a reply back to the sender. A response channel is a clean v2.
- Interactive approval needs either a StatusNotifierHost (tray) or a notification
  daemon. If neither runs (a headless corrald), the headless approval path is the
  whitelist file: add the `(sender-dir -> target-dir)` pair to
  `~/.corral/whitelist` and the next poll releases the pending message and
  delivers it (the file is re-read every tick). No auto-deny; the tray count
  shows what is waiting, and the tray stays the reliable interactive path.
- Delivery policy when the target dir's agent is Running: v1 reuses it and lets
  the extension queue the message as a follow-up (it can intrude on a
  human-driven session; the provenance tag makes that visible). Alternatives
  (never-inject-Running, always-new) are deferred until real use decides.
- `force_new` targets the agent that appears after corral's spawn (a socket not
  present before it); if several agents start in one dir at once the newcomer
  is picked arbitrarily. Adequate for v1.
- Each project dir where pi runs gains a `<cwd>/.corral/` holding the session
  socket. Deliberate: workdir-local is the sandbox-isolation primitive. Add it
  to a global gitignore if the stray dir bothers you.
- Dormant sessions render as dormant (one card per resumable session, newest
  first), resume on Enter, dismiss
  on `d`, and are pruned when their session file is gone or the record is >14
  days stale. A crashed session (no clean shutdown, so its registry `socket`
  stays set) is caught by a staleness sweep: the board records sockets whose
  watcher fails to connect (`dead_sockets`) and treats a dead-socketed record
  as dormant, so a crashed agent stays resumable instead of vanishing. A
  freshly starting session (socket set, not yet proven dead) stays on the live
  path and never flickers through the Dormant column.

## Future

- More than pi. The board core is agent-agnostic; the only pi-specific piece is
  the `corral-pi` adapter. The stable contract any ACP agent joins by:
  (1) write `~/.corral/registry/<sessionId>.json` with `label` set to the agent
  kind, `socket` pointing at (2) a workdir-local `<label>-<pid>.sock` speaking
  ACP (initialize, session/list, session/prompt), a `spawnCommand`/
  `resumeCommand` argv corral runs verbatim to launch/resume the kind, and (3)
  broadcast `state_update`. A non-cooperating agent can be wrapped by a generic
  stdio-to-socket-plus-registry shim instead of a bespoke extension. Missing
  `state_update` just defaults the card to Idle; a missing `label` renders as
  `agent`; a missing `spawnCommand`/`resumeCommand` leaves the kind
  discoverable and drivable but not launchable by corral.
- quine is the third worked kind and the first that serves the convention
  *natively*: rather than a bespoke adapter file (like `corral-pi.ts` /
  `corral-opencode.ts`), quine compiles the surface in as a `--corral`
  interface in its own repo. It is also the first GUI agent, so its record
  carries `gui: true`: corral launches it directly (no terminal wrapper, see
  `launch.rs`) and focuses it by its own pid (`focus.rs` `match_pids`), since a
  self-windowing app owns its window rather than living inside a terminal. It
  also declares `messageFlag: "--message"`, so corral delivers a launch message
  as `--message "<text>"` rather than a trailing positional (the flag form pi
  and opencode do not need).
- Kind badges become load-bearing once a second agent kind ships: the card
  already shows the `label`, so mixed pi/opencode boards read at a glance.
- Full requires_action coverage. pi core (or a native ACP `state_update`
  implementation in pi) emitting a signal whenever any `ctx.ui.*` prompt opens
  (approvals, select, input, elicitation), so the board catches every
  user-input gate, not just the `question` tool. This is the platform-side
  companion to corral's display, and the standard end-state per the ACP v2
  RFD.
- More than sway and kitty. `SwayFocuser` and `KittyLauncher` are the PoC
  implementations for the maintainer's setup; other compositors and terminals
  drop in as new `WindowFocuser` / `Launcher` implementations behind the same
  seams, with no change to the triage core.

## Development Setup

- Nix flake (nixpkgs-unstable) + direnv; Rust pinned via rust-toolchain.toml
  through rust-overlay. Four workspace crates: `corral-core` (lib), `corral`
  (TUI board bin), `corral-gui` (desktop board bin), `corral-daemon` (`corrald`
  bin). The flake's devShell + package carry the GUI graphics libs (`libGL`,
  `libxkbcommon`, `wayland`, X11) and `wrapProgram` the `corral-gui` binary with
  the driver library path for NixOS. It also ships `cage` (+ `xwayland`) on the
  runtime PATH of all three binaries (and in the devShell), the headless
  compositor hidden agents run inside. `just` commands: `test`, `lint`, `board`,
  `gui`, `daemon`, `watch` (cargo-watch tests), and `watch-board` / `watch-gui`
  / `watch-daemon` (rebuild + rerun on change), `nix-build`. GUI builds need the
  devShell (its `LD_LIBRARY_PATH`), so run them via `nix develop`.
- Lifecycle is deployment glue in `~/nixos`, not corral code: a systemd user
  service runs `corrald` (restart-on-failure) so messaging survives a crash;
  a WM keybind summons a board window — either a floating/borderless `kitty -e
  corral` scratchpad (the TUI as a launcher popup) or `corral-gui`. corrald owns
  behavior; nixos/WM own keep-alive and visibility.
- CI: GitHub Action runs `nix flake check` (build + tests via nix).
