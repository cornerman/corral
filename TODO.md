# corral TODO

Living list of remaining work. See AGENTS.md for architecture and
docs/superpowers/specs/ for the design.

## Built (on `main`)

The attention board is shipped and working:
- `crates/board`: 3 columns Requires Action / Idle / Running (ACP v2
  `state_update` vocabulary), a live watch connection per socket, sway focus
  (`/proc` parent-walk) and kitty spawn behind `WindowFocuser`/`Launcher`
  seams. Keys: Up/Down (or scroll) within a column, Left/Right (h/l) across
  columns, Enter/left-click focus, `n` spawn in selected cwd, `N` fuzzy dir
  picker (cwds of sessions on the board only, no filesystem scan), `q`
  quit. 20 tests, clippy clean.
- Discovery: per-session registry `~/.corral/registry/<sessionId>.json`
  (override dir `$CORRAL_REGISTRY_DIR`) naming a workdir-local socket
  `<cwd>/.corral/pi-<pid>.sock` (override `$CORRAL_SOCKET_DIR`). The board
  watches live sockets; dormant records (socket cleared on clean shutdown) are
  written but not yet rendered.
- `corral-announce`: serves initialize / session/list / session/prompt /
  session/cancel; broadcasts message + tool events, `state_update`
  (running/idle from turn events; requires_action while the `question` tool
  blocks), `session_info_update` on rename; title falls back to the first user
  message (capped) when unnamed, and updates live.

## Unified Session Registry (designed, not built — see spec)

One per-session file drives discovery, isolation, and resume.

- [x] corral-announce: bind the socket at `<cwd>/.corral/pi-<pid>.sock`
      (workdir-local = sandbox-isolated); write
      `~/.corral/registry/<sessionId>.json`
      `{ sessionId, cwd, title, socket, resume, lastSeen }` on session_start;
      refresh `lastSeen` on turn_end + `title` on rename; on clean shutdown
      unlink socket + clear `socket`. `resume` = session-file path (null for
      ephemeral).
- [x] board discovery: scan `~/.corral/registry/*.json`; a record with a
      socket → live (persistent watch), else skipped for now.
- [x] board: dormant sessions in a dedicated dimmed Dormant column
      (latest-per-cwd); `Agent.origin` Live|Dormant, dormant is a derived view
      of the registry (no socket-path re-keying needed); Enter/click resumes
      via the Launcher seam (`pi --session <resume>` in `kitty --directory
      cwd`); `d` dismisses; prune dead-target / >14-day (by registry-file
      mtime).
- [x] Staleness sweep: a crashed session leaves `socket` set but dead. The
      board records sockets whose watcher fails to connect (`dead_sockets`) and
      treats a dead-socketed record as dormant, so it stays resumable instead
      of vanishing; a still-connecting socket never flickers through Dormant.

## Inter-Agent Messaging (built — see spec)

- [x] Operator-initiated: `m` composes a message to the selected live agent,
      delivered over its socket via `session/prompt` (`prompt::send_prompt`,
      fire-and-forget). Spawn-in-dir-then-send is folded into the
      agent-initiated routing below.
- [x] Agent-initiated transport: `corral_message_agent` submits over the
      `~/.corral/corrald.sock` control socket (`crates/daemon/src/control.rs`);
      the `corrald` daemon parses, finds the recipient, acks synchronously
      (accepted / approval_needed / recipient_not_found / directory_not_known), and
      enqueues routable messages into `router.rs`. A connect failure fails loud
      (corrald down); accepted messages are in-memory only until routed (no
      on-disk spool).
- [x] Daemon split: messaging moved out of the board into a singleton `corrald`
      binary (`crates/daemon`) owning the control socket, whitelist gate, and
      router; the approval gate surfaces on a `ksni` tray + `notify-send`. The
      board (`crates/board`) is now a pure registry viewer, launchable many
      times; operator `m` delivers directly and ungated. Shared code lives in
      `crates/core` (`corral-core`). Singleton guard: a second corrald refuses
      to start.
- [ ] nixos: a systemd user service to keep `corrald` alive
      (restart-on-failure), and a WM keybind to summon the board window.
- [ ] "Show details" proper window: today the tray's Show details pops a
      `notify-send` notification (from / to / body). Replace it with a small,
      clean native window. This is corral's first pixel surface, so it is gated
      on the bigger "should the board become a GUI app / a launcher" decision.
      Design branches if built standalone: an external dialog
      (`zenity`/`kdialog`/`yad`, zero Rust deps, generic look) vs a tiny spawned
      helper binary (`fltk` small / `egui` nicer, designed look, +dep +crate).
      Do NOT embed a windowing toolkit in the headless `corrald` process.
- [x] Addressing by target dir (spawn if absent; `force_new` for a dedicated
      agent) OR by exact `target_session` (deliver if live, else resume its
      dormant record). Session addressing makes replies land on the precise
      sender, not a sibling sharing the dir. An unknown session is dropped.
- [x] Provenance + reply handle: injects `[from agent in <dir> (session <id>)]`
      — the sender's session id is the handle the receiver replies to via
      `corral_message_agent(target_session=..)`. Auth is the `(sender-dir ->
      target-dir)` whitelist (a session target resolves to its cwd) in
      `~/.corral/whitelist` plus the operator popup (a/A/d/esc). `_meta` not
      added (send path is plain text).
- [ ] v2: auto response channel — corral captures the target's final message
      and routes it back to the sender's session without the receiver having to
      call `corral_message_agent` itself. (The reply handle above makes a manual reply
      already correct; this only automates it.)
- [ ] OPEN: delivery-target policy when the target is Running. v1 reuses +
      queues as follow-up; never-inject-Running and always-new are the
      alternatives. Settle with real use.

## Validation

- [x] `$HOME/.corral` is in the pi sandbox allow list (sandboxed sessions can
      announce there). Run corral itself with `cargo run` / `just board`.
- [ ] Live end-to-end run: real sandboxed pi sessions appear, focus jumps to
      the right window, the `question` tool flips the card to Requires Action.
      (Needs a fresh pi session; ones started before `.corral` was allowed
      still bind the old path.)

## Platform (pi) — the requires_action follow-up (C)

- [ ] Full `requires_action` coverage. Today corral-announce only detects the
      `question` tool. pi's built-in tool-approval confirms and other
      `ctx.ui.*` prompts (select, input, elicitation) are invisible to
      extensions. Wanted: pi emits a signal whenever any blocking UI prompt
      opens/closes, or pi speaks ACP v2 `state_update` natively.
- [ ] Track the ACP v2 Prompt Lifecycle RFD
      (agentclientprotocol.com/rfds/v2/prompt). When `state_update` stabilizes,
      corral already speaks it; retire any interim shim.

## Desktop GUI (corral-gui, egui/eframe)

- [x] Spike + packaging: themed eframe window (base16 Solarized, follows system
      light/dark), flake graphics deps (`libGL`/`libxkbcommon`/`wayland`/X11)
      and a NixOS `wrapProgram` LD_LIBRARY_PATH on the binary. `just gui`.
- [x] `core::engine::Engine`: the shared registry-reflect loop (scan/prune/
      watch/drain/timers), so both shells stay thin. `model`/`watch`/`nav`/
      `picker`/`focus` lifted into `corral-core`.
- [x] Dashboard v1: four columns of cards over the engine, state-colored dot,
      `~`-path, activity·age; click a card to go (focus/resume); `+ new agent`.
- [ ] Parity with the TUI: `m` message compose, `/` fuzzy jump (reuse
      `core::picker`), `d` dismiss, keyboard navigation + selection highlight,
      two-stage click (select then go), path abbreviation polish.
- [ ] DEBT: the ratatui board still runs its own inline copy of the reflect
      loop and `age_label`/`prune`; converge it onto `core::engine`, or retire
      the TUI once the GUI is the daily driver. Duplication is temporary and
      deliberate (kept the working TUI untouched during the GUI build).

## Board Polish

- [x] Column scrolling: each column keeps a persistent `ListState`, so ratatui
      scrolls long columns to keep the selection visible and `hit_test` reads
      the real scroll offset per column.
- [x] Time-in-state: live cards show a compact age (`8s`/`5m`/`2h`/`3d`) since
      the last state transition, restarted on each `SetState`.
- [x] `f` fuzzy-focus: picker over live agents (filter by title/cwd), Enter
      focuses the chosen window. Reuses the Picker via `selected_original`.

## Extension (corral-announce)

- [x] `agentInfo.version`: now imports the exported `VERSION` constant from
      `@earendil-works/pi-coding-agent` (the old `require(package.json)` did
      not resolve).
- [ ] `session/prompt` responses resolve for all waiting clients at once when
      the queue drains (no per-message turn attribution). Left as-is: pi does
      not expose which turn consumed which injected message, so precise
      stopReason routing needs a platform change. Correct in aggregate (every
      injected message has had its turn) and fine for fire-and-forget
      messaging.

## Future Features

- [ ] Multi-agent: let non-pi ACP agents announce (their own extension or a
      stdio-to-socket wrapper binding `<label>-<pid>.sock`). The board already
      discovers any socket and reads agentInfo generically.
- [ ] More compositors/terminals: new `WindowFocuser` / `Launcher`
      implementations behind the existing seams (sway/kitty are PoC).
