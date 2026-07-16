# corral TODO

Living list of remaining work. See AGENTS.md for architecture and
docs/superpowers/specs/ for the design.

## Built (on `main`)

The attention board is shipped and working:
- `crates/board`: 4 columns Requires Action / Idle / Running / Dormant (ACP v2
  `state_update` vocabulary), a live watch connection per socket, window focus
  (EWMH on X11; sway / Hyprland / niri on Wayland) and terminal spawn behind
  the `WindowFocuser` / `Launcher` seams. Keys: Up/Down (or scroll) within a
  column, Left/Right across columns, Enter/left-click go (focus a live window
  or resume a dormant session), Shift+Enter spawn in the selected card's dir,
  `/` inline fuzzy filter, `m` message, `d` dismiss, `q` quit (Esc peels one
  layer per press). `--launcher` opens an ephemeral popup.
- Discovery: per-session registry `~/.corral/registry/<sessionId>.json`
  (override dir `$CORRAL_REGISTRY_DIR`) naming a workdir-local socket
  `<cwd>/.corral/<label>-<pid>.sock` (override `$CORRAL_SOCKET_DIR`). The board
  watches live sockets and renders dormant records (socket cleared on clean
  shutdown) in the Dormant column.
- `corral-pi`: serves initialize / session/list / session/prompt /
  session/cancel; broadcasts message + tool events, `state_update`
  (running/idle from turn events; requires_action while the `question` tool
  blocks), `session_info_update` on rename; title falls back to the first user
  message (capped) when unnamed, and updates live.

## Unified Session Registry (designed, not built — see spec)

One per-session file drives discovery, isolation, and resume.

- [x] corral-pi: bind the socket at `<cwd>/.corral/pi-<pid>.sock`
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
- [x] Label override: a `target_dir` message may set an optional `label` to
      choose the agent kind spawned when none is live. Resolved from any
      registry record of that kind (`router.rs::spawn_command_for_label`), so a
      kind seen anywhere can start in any dir; an unknown label fails loud (no
      arbitrary kind). Omitted keeps the prior behavior
      (`spawn_command_for_dir`, first record for the dir). Wired through the
      `label` field on the mailbox `Message` and both extensions' tool schema.
      Plan: `docs/superpowers/plans/2026-07-14-label-override-via-tool-call.md`.
- [ ] OPEN: smarter default when `label` is omitted. TODAY the router still
      picks the FIRST registry record whose cwd matches the dir
      (`spawn_command_for_dir`, arbitrary when a dir hosted several kinds).
      DIRECTION: default deterministically to the dir's MOST-USED label
      (occurrence), then the global most-used. NOTE: the target dir need NOT be
      previously announced — any existing directory works, used directly as the
      new agent's cwd. Such a dir has no local label history, so its kind comes
      from the caller's `label` else the global most-used default. (So
      `directory_not_known` should mean only "path does not exist", never
      "no record here yet".)
- [x] Expose available sessions to agents (discovery): `list_corral_agents`, a
      read-only `{"op":"list"}` roster query on the control socket, served
      synchronously by corrald (`mailbox::build_roster`). SHIPPED SHAPE (differs
      from the earlier invisible-sessions plan): EVERY session yields a
      per-session entry {kind, sessionId, live} addressable by
      `target_session` (any session is messageable; the operator gates an
      unwhitelisted pair on delivery). The whitelist scopes only the detail
      fields: a reachable directory's entry (caller's own dir, or a whitelisted
      `(sender-dir -> target-dir)` pair) adds `cwd` + `description`; an
      unreachable one hides both. A roster never carries a title or activity —
      messaging is not reading. The board stays a pure viewer (discovery lives
      in corrald).

## Validation

- [x] `$HOME/.corral` is in the pi sandbox allow list (sandboxed sessions can
      announce there). Run corral itself with `cargo run` / `just board`.
- [ ] Live end-to-end run: real sandboxed pi sessions appear, focus jumps to
      the right window, the `question` tool flips the card to Requires Action.
      (Needs a fresh pi session; ones started before `.corral` was allowed
      still bind the old path.)

## Platform (pi) — the requires_action follow-up (C)

- [ ] Full `requires_action` coverage. Today corral-pi only detects the
      `question` tool. pi's built-in tool-approval confirms and other
      `ctx.ui.*` prompts (select, input, elicitation) are invisible to
      extensions. Wanted: pi emits a signal whenever any blocking UI prompt
      opens/closes, or pi speaks ACP v2 `state_update` natively.
- [ ] Track the ACP v2 Prompt Lifecycle RFD
      (agentclientprotocol.com/rfds/v2/prompt). When `state_update` stabilizes,
      corral already speaks it; retire any interim shim.

## Desktop GUI (corral-gui, iced)

- [x] Spike + packaging: themed eframe window (base16 Solarized, follows system
      light/dark), flake graphics deps (`libGL`/`libxkbcommon`/`wayland`/X11)
      and a NixOS `wrapProgram` LD_LIBRARY_PATH on the binary. `just gui`.
- [x] `core::engine::Engine`: the shared registry-reflect loop (scan/prune/
      watch/drain/timers), so both shells stay thin. `model`/`watch`/`nav`/
      `picker`/`focus` lifted into `corral-core`.
- [x] Dashboard v1: four columns of cards over the engine, state-colored dot,
      `~`-path, activity·age; click a card to go (focus/resume); `+ new agent`.
- [x] Parity with the TUI: `m` message compose, `/` inline fuzzy filter, `d`
      dismiss, arrow navigation + selection highlight, two-stage click (select
      then go), clickable footer, staged Escape, launcher mode. Column names
      come from the shared `Column::title()` in both shells (no drift); each
      shell keeps its own rendering idiom (ratatui uppercase headings + `▍`
      selection bar vs iced title-case headings + accent-tinted card).
- [ ] DEBT: the ratatui board still runs its own inline copy of the reflect
      loop and `age_label`/`prune`; converge it onto `core::engine`, or retire
      the TUI once the GUI is the daily driver. Duplication is temporary and
      deliberate (kept the working TUI untouched during the GUI build).
- [x] Launcher mode (`corral-gui --launcher` and `corral --launcher`): ephemeral
      rofi-style popup, both shells at parity. Boots focused on the filter; go
      (Enter/focus) and new (Shift+Enter/spawn) exit the process on success (m/d
      keep it open, q exits); the GUI also dismisses on focus loss (window
      Unfocused, guarded on a prior Focus so boot cannot self-close). Escape
      peels one layer per press and quits at the last (compose -> blur -> clear
      -> exit), in both launcher and normal mode, in the TUI and GUI alike. The
      GUI sets window `app_id`/X11 class `corral-launcher`. Placement is a WM float/center rule
      keyed on that app_id (owns-behavior / WM-owns-visibility split), NOT
      self-positioned. Chosen deliberately (option D) to test whether a WM rule
      is good enough before investing in a real overlay.
- [ ] OPEN — self-floating popup like fuzzel/rofi. iced cannot do it: on
      Wayland a normal `xdg-toplevel` may not request float/center/popup
      (placement is the compositor's job by protocol), and fuzzel/wofi are not
      normal windows — they use `wlr-layer-shell`, which iced/winit does not
      speak. On X11 a window-type hint (DIALOG) would auto-float, but iced 0.13
      exposes only `application_id` + `override_redirect`. If the WM-rule path
      proves insufficient, the real options are: (C) the `iced_layershell`
      crate (bolt layer-shell onto iced; different app entry point;
      wlroots-only), or switch toolkit to **gtk4 + gtk4-layer-shell** (proper
      native overlay, Pango text, system-provided deps instead of the compiled
      wgpu/vulkan stack, does launcher + dashboard). REJECTED: delegating the
      launcher to `fuzzel --dmenu` (zero toolkit) — we want our own UI. This
      also reopens the bigger question of whether iced's GPU stack earns its
      weight versus a native toolkit.

## Board Polish

- [x] Column scrolling: each column keeps a persistent `ListState`, so ratatui
      scrolls long columns to keep the selection visible and `hit_test` reads
      the real scroll offset per column.
- [x] Time-in-state: live cards show a compact age (`8s`/`5m`/`2h`/`3d`) since
      the last state transition, restarted on each `SetState`.
- [x] `f` fuzzy-focus: picker over live agents (filter by title/cwd), Enter
      focuses the chosen window. Reuses the Picker via `selected_original`.
- [ ] Hidden-agent icon: a live hidden card now shows a plain-text `hidden`
      pill in both shells (`ui::hidden_badge`, `dashboard.rs`), replacing the
      🫥 U+1FAE5 emoji that rendered as tofu/blank on terminals without that
      2021 glyph. A crossed-out-eye icon reads better but has no basic-Unicode
      codepoint that renders everywhere; the reliable eye-slash `` (U+F070)
      needs a Nerd Font. Revisit if the maintainer's terminals standardize on
      a Nerd Font.

## Extension (corral-pi)

- [x] `agentInfo.version`: now imports the exported `VERSION` constant from
      `@earendil-works/pi-coding-agent` (the old `require(package.json)` did
      not resolve).
- [ ] `session/prompt` responses resolve for all waiting clients at once when
      the queue drains (no per-message turn attribution). Left as-is: pi does
      not expose which turn consumed which injected message, so precise
      stopReason routing needs a platform change. Correct in aggregate (every
      injected message has had its turn) and fine for fire-and-forget
      messaging.

## Extension (corral-opencode)

- [x] Second adapter: an opencode plugin mirroring `corral-pi` (registry
      record, workdir-local ACP socket, `state_update` broadcast,
      `corral_message_agent` tool). Single active session per window;
      multi-session multiplexing deferred. Teardown clears the socket and
      unlinks on process exit/SIGINT/SIGTERM (no plugin-unload hook).
- [x] Static verification against `@opencode-ai/plugin@1.16.2` (the installed
      opencode's version): the `Plugin` signature, `client.session.list/prompt/
      abort` calls, the `tool()` helper, and every `event.type` string all
      typecheck against the real SDK. FOUND + FIXED a bug: tool activity is
      delivered as the dedicated `tool.execute.before/after` plugin HOOKS, not
      as event-bus events (no `tool.*` in the `Event` union), so the old
      event-switch cases never fired and opencode cards showed no tool activity.
      Now registered as real hooks. Permission handling was already correct
      (`permission.updated`/`permission.replied` are real events).
- [ ] End-to-end verify at RUNTIME (blocked in the dev sandbox: opencode is a
      Bun-compiled binary that SIGTRAPs under Landlock, so it cannot run here).
      Outside the sandbox: install the plugin, confirm the card appears/updates
      on the board, `m` delivers, tool + message activity render, and clean
      teardown makes the record dormant. Confirm the runtime event payload
      field paths (`properties.sessionID`, message-part text) and the
      `session.list()` title shape, which types cannot pin down.

## Extension (corral-claude)

- [x] Third adapter (Claude Code), branch `feat/corral-claude`: since Claude
      Code has no in-process plugin runtime, a resident `sidecar.ts` (spawned
      detached by the SessionStart hook) holds the ACP socket and a
      per-session control socket; a thin `hook.ts` shim bridges each hook event
      to it. Live delivery via `Stop` decision:block (turn boundary) +
      `asyncRewake` exit-2 (idle wake); `state_update` native (incl.
      `Notification[permission_prompt]` -> requires_action). Packaged as a
      Claude Code plugin (`.claude-plugin/plugin.json` + `hooks/hooks.json`
      using `${CLAUDE_PLUGIN_ROOT}`, repo-root `marketplace.json`).
- [ ] End-to-end verify (needs `bun` on PATH + a real Claude Code): install the
      plugin, start `claude`, confirm the card appears with correct pid/focus,
      `state_update` tracks running/idle/requires_action, `m` and inter-agent
      delivery land in the LIVE session (Stop-block and idle asyncRewake paths
      both), tool activity renders, and SessionEnd + the liveness probe reap
      the sidecar and make the record dormant. UNVERIFIED in-repo (no bun, no
      Claude harness): hook payload field names and the block/asyncRewake
      injection semantics are coded from the hooks reference only.
- [ ] Confirm the open unknowns in the adapter README: `claude --resume <id>
      "msg"` accepting a trailing prompt interactively (dormant delivery);
      exact `Notification` matcher values and `last_assistant_message` on
      `Stop`; and that `asyncRewake` exit-2 wakes a fully idle terminal TUI.
- [x] Claude adapter is on `main` (the `feat/corral-claude` work is merged);
      what remains is the runtime verification above, not a merge.

## Future Features

- [ ] Cross-box tasking: grow `corral_message_agent` into the full pi-subagents
      verb set (`spawn`/`send`/`list`/`history`/`kill`/`set_status`), scoped by
      a new **task-group** primitive where same-group agents skip the
      whitelist/approval gate. Design:
      `docs/superpowers/specs/2026-07-15-cross-box-tasking-design.md`. Partial
      code head-start on branch `cross-box-tasking-plan` (commits `f2db1ad`
      group/name registry fields, `c9f9174` same-group auth) — rebase those onto
      `main` to resume, do not merge the stale branch wholesale.

- [x] Multi-agent: a second harness announces. `extensions/corral-opencode.ts`
      (an opencode plugin) mirrors `corral-pi`, binding
      `<cwd>/.corral/opencode-<pid>.sock` and writing a record with
      `label: "opencode"`; corral needed zero changes (it runs the record's
      launch commands verbatim and reads `label` generically). Further non-pi
      agents drop in the same way (their own extension or a stdio-to-socket
      wrapper binding `<label>-<pid>.sock`).
- [ ] More compositors/terminals: new `WindowFocuser` / `Launcher`
      implementations behind the existing seams (sway/kitty are PoC).
