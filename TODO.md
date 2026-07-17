# corral TODO

Living list of remaining work. See AGENTS.md for architecture and
docs/superpowers/specs/ for the design. Only open, founded next steps live
here; shipped work is described in AGENTS.md, not tracked here.

## Inter-Agent Messaging

- [ ] "Show details" proper window: today the tray's Show details pops a
      `notify-send` notification (from / to / body). Replace it with a small,
      clean native window. This is corral's first pixel surface, so it is gated
      on the bigger "should the board become a GUI app / a launcher" decision.
      Design branches if built standalone: an external dialog
      (`zenity`/`kdialog`/`yad`, zero Rust deps, generic look) vs a tiny spawned
      helper binary (`fltk` small / `egui` nicer, designed look, +dep +crate).
      Do NOT embed a windowing toolkit in the headless `corrald` process.
- [ ] v2: auto response channel — corral captures the target's final message
      and routes it back to the sender's session without the receiver having to
      call `corral_message_agent` itself. (The reply handle makes a manual reply
      already correct; this only automates it.)
- [ ] OPEN: delivery-target policy when the target is Running. v1 reuses +
      queues as follow-up; never-inject-Running and always-new are the
      alternatives. Settle with real use.
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

## Validation

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

- [ ] Hidden-agent icon: a live hidden card now shows a plain-text `hidden`
      pill in both shells (`ui::hidden_badge`, `dashboard.rs`), replacing the
      🫥 U+1FAE5 emoji that rendered as tofu/blank on terminals without that
      2021 glyph. A crossed-out-eye icon reads better but has no basic-Unicode
      codepoint that renders everywhere; the reliable eye-slash `` (U+F070)
      needs a Nerd Font. Revisit if the maintainer's terminals standardize on
      a Nerd Font.

## Extension (corral-pi)

- [ ] `session/prompt` responses resolve for all waiting clients at once when
      the queue drains (no per-message turn attribution). Left as-is: pi does
      not expose which turn consumed which injected message, so precise
      stopReason routing needs a platform change. Correct in aggregate (every
      injected message has had its turn) and fine for fire-and-forget
      messaging.

## Extension (corral-opencode)

- [ ] End-to-end verify at RUNTIME (blocked in the dev sandbox: opencode is a
      Bun-compiled binary that SIGTRAPs under Landlock, so it cannot run here).
      Outside the sandbox: install the plugin, confirm the card appears/updates
      on the board, `m` delivers, tool + message activity render, and clean
      teardown makes the record dormant. Confirm the runtime event payload
      field paths (`properties.sessionID`, message-part text) and the
      `session.list()` title shape, which types cannot pin down.

## Extension (corral-claude)

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

## Future Features

- [ ] Full socket mediation (corrald as the sole socket connector). Today
      viewers connect to each live agent socket themselves to watch live state
      (running/idle/requires_action, activity, title), so a viewer still parses
      one untrusted stream (a compromised agent's socket) — a low-severity
      **display**-spoofing residual on an already-authenticated card (see
      SECURITY.md T18). The principled endpoint of the curator model is to make
      corrald the ONLY process that opens agent sockets, fold live state into
      `state/registry/`, and have viewers read everything from there (zero
      untrusted input in a viewer). Deferred (decision B, 2026-07-16): it
      deepens the corrald dependency and moves operator `m` / card-move
      cancel/nudge through corrald (viewers could no longer reach sockets). The
      inotify watch on `state/registry/` already in place is exactly the viewer
      mechanism this would need, so the migration is mostly: move `core::watch`
      consumption + operator prompt/cancel into corrald, and delete viewer
      socket I/O.

- [ ] Cross-box tasking: grow `corral_message_agent` into the full pi-subagents
      verb set (`spawn`/`send`/`list`/`history`/`kill`/`set_status`), scoped by
      a new **task-group** primitive where same-group agents skip the
      whitelist/approval gate. Design:
      `docs/superpowers/specs/2026-07-15-cross-box-tasking-design.md`. Partial
      code head-start on branch `cross-box-tasking-plan` (commits `f2db1ad`
      group/name registry fields, `c9f9174` same-group auth) — rebase those onto
      `main` to resume, do not merge the stale branch wholesale.

- [ ] Confine the broker (corrald) via **systemd unit hardening** in `~/nixos`
      (deployment glue, defense-in-depth). corrald is unsandboxed same-user
      today (SECURITY.md "out of scope"), so a parsing bug in the one process
      that reads every untrusted record/message is full-authority RCE. It
      cannot be boxed to one dir (it reads every workdir's `.corral/` at its
      real physical location, writes sealed `state/`, connects every agent
      socket, spawns/resumes agents), but a hardened user service still buys
      real blast-radius reduction: no network (`IPAddressDeny=any` /
      `RestrictAddressFamilies=AF_UNIX`), no reading `~/.ssh`/arbitrary home
      (`ProtectHome` relaxed only where reads are needed), `SystemCallFilter`.
      Not a new trust boundary — a compromised corrald still writes `state/`
      and launches agents.
      - **Coupled cost (the only corral-code change): spawn-escape.** systemd
        sandboxing applies to the whole service cgroup, so agents corrald forks
        would inherit its jail (network-deny, mount hiding) and break. Fix: a
        new `core::launch::Launcher` that starts each agent as a fresh
        transient unit (`systemd-run --user …`) outside corrald's
        cgroup/namespaces, with the per-workdir sandbox applied there.
      - REJECTED: a **dedicated OS user** for corrald (own `state/` as a real
        uid boundary). Too invasive — splits `~/.corral` across two uids
        (group/ACL sharing for the index + socket, group-read on operator
        workdirs, a privilege hop to spawn agents back as the operator) to
        defend only the unsandboxed-agent case the model already excludes.
      - If ever pursued to maximum tightness, corrald's **D-Bus** dep (`ksni`
        tray + `notify-send`, the approval surface) could be dropped to remove
        session-bus access from the jail — but only by replacing the surface
        (a Linux tray/notification *is* D-Bus; no non-bus equivalent): move
        approvals into the boards (a sealed `state/pending.json` they render +
        decide over the control socket) plus a `corrald approve <id>` CLI. Not
        worth it on its own — the tray is good UX and fails gracefully to the
        whitelist file already.
