# corral TODO

Living list of remaining work. See AGENTS.md for architecture and
docs/superpowers/specs/ for the design. Only open, founded next steps live
here; shipped work is described in AGENTS.md, not tracked here.

## Security Review Follow-Ups (review 2026-08-01)

A line-by-line review of the trust boundary found three exploitable defects and
five smaller ones. **The three highs are fixed** (symlinked `socket` field
defeating T17; a squatted `sessionId` evicting a live session and capturing its
traffic; an uncapped control-socket request line buffering unbounded memory),
each with a proof of concept first and a regression test behind it — see
SECURITY.md T17 / T10 / T15 and its Review History. What remains, most
load-bearing first.

1. [ ] **Cap the approval surfaces (SECURITY.md T21).** `Router.pending` is an
       uncapped `Vec`, and `lib.rs` fires one `notify-send` process in its own
       thread per newly pending id, so a compromised agent can submit thousands
       of well-formed unauthorized submissions and drown a genuine popup in
       noise. **Approval fatigue is the real risk**, not the resource cost: the
       operator who dismisses 200 notifications also dismisses the one that
       mattered. FIX: cap pending per sender directory (drop the excess with one
       audit line), and coalesce the desktop surface to one notification naming a
       count. The T4 quarantine defuses *registration* flood only, so it does not
       answer this.
2. [ ] **Cap pending registrations per label (T21).** `Registrar.denied`
       remembers exact `(label, Template)` pairs, so an agent that mutates its
       `spawnCommand` every tick mints a novel pair forever: the tray head
       rewrites and the sealed `audit.log` grows unbounded (a file the operator
       cannot rotate from inside corral). FIX: cap pending kinds per label and
       rate-limit the audit line. COMPOSES WITH the "multiple approved launch
       sets per label" item below — do that first, since it changes what a
       distinct pair means.
3. [ ] **Cap the pointer store scan (T21).** An agent may create unlimited files
       in the write-only pointer directory; corrald re-reads all of them every
       tick and `prune_orphan_pointers` only acts after a five-minute grace, so a
       peer can burn broker CPU and delay every delivery. FIX: bound the files
       read per scan, and prefer ignoring an unparsable pointer cheaply.
4. [ ] **Reject control characters in a whitelist path (T5).** `representable`
       blocks only the `" -> "` separator, so "allow always" for a sender whose
       directory name contains a newline writes two lines into the sealed
       whitelist. Not currently exploitable — an injected line's `from` is a
       single path component, so it can never match an absolute cwd — but the
       store should stay a clean relation. FIX: refuse any control character in
       `whitelist_add` and never match one in `is_whitelisted` (fail closed,
       exactly as the separator is handled).
5. [ ] **Write the history export `0600` (local disclosure).**
       `core::history::write_and_open` writes a full transcript into `$TMPDIR`
       under the default umask, so on a shared `/tmp` any local user reads it.
       FIX: mode `0600` on the temp file, as `approved_commands::write_approved`
       already does. One line; only unpicked because it is outside the
       cross-agent boundary this review targeted.

## Most Urgent (review 2026-07-24)

Ranked by risk to the project's core claims. The top two are validation gaps:
the code is written and unit-green (210 tests, clippy clean), but the two
headline claims — cross-harness and the security boundary — are not proven at
runtime.

1. **Prove the cross-harness claim at runtime.** VISION item #2 ("two harnesses
   on one board is the demonstration that makes the claim real") is still
   UNVERIFIED: only `e2e-pi` runs green; opencode/claude/cursor have never taken
   a real turn. opencode is the load-bearing second adapter and its runtime
   event field paths are guessed (`properties.sessionID`, message-part text).
   Until one non-pi adapter is validated end-to-end, "the open coordination
   layer for many harnesses" rests on one harness. (Details in the four
   adapter sections below.)
2. **Validate the security precondition in the VM.** The whole SECURITY.md model
   rests on "whole-process workdir sandbox", but the e2e agents run UNCONFINED
   (`open_kitty` runs plain `pi`, not `nono run`), so every T1-T18 gate is
   exercised only by best-effort probes that no-op when nono cannot run a bare
   command. Land full nono confinement (`nono learn` -> vendored profile) and
   flip the sandbox-negative checks in `scenarios/pi.py` §9 from best-effort to
   hard asserts. Also: the sandbox profile itself is `[designed]`, not
   `[in place]` — it lives in `~/nixos`, so corral ships a security story whose
   enforcement is out-of-repo and untested here.
3. **corrald is unsandboxed, full-authority RCE surface** (SECURITY.md
   "out of scope", the "Confine the broker" item below). It is the one process
   that parses every untrusted record and message. Systemd unit hardening in
   `~/nixos` is a real blast-radius reduction and the highest-value
   defense-in-depth left.

Each of the three is expanded in a section below. Items 1-2 are the land-grab
validation (VISION #1-2); item 3 is hardening.

## VM E2E Smoke Test (follow-ups)

The `nix/tests/` e2e suite landed with `e2e-pi` passing end-to-end (see
`docs/superpowers/specs/2026-07-18-vm-e2e-smoke-test-design.md`). Open items:

- [ ] Full nono confinement in the VM. Agents currently run UNCONFINED in the
      scenarios; running a full agent (or `sh`/`cat`) under nono needs
      per-command path discovery (`nono learn` -> a vendored profile with the
      pi/node/opencode closures granted). Once confined, flip the
      sandbox-negative checks in `scenarios/pi.py` from best-effort to hard
      asserts (cross-workdir read denied, sealed `state/registry` unwritable).
- [ ] Hidden agents in the VM: **cage headless now provably works** under the
      VM's software GL (`e2e-todo` §5, 2026-07-31: `cage -- kitty -e pi` ran and
      the session announced with `hidden: true`). The earlier "did not come up"
      note was wrong, or was really the `no terminal found` bug (see the todo
      section). So flip `scenarios/pi.py` §7-8 from best-effort try/except to
      hard asserts — they were hiding exactly that bug for weeks.
- [ ] Run and harden the other three scenarios; each is wired and evaluates
      but was not run in the authoring sandbox. opencode needs a verified stub
      provider config, and should confirm the bun-under-Landlock outcome once
      confined. claude (unfree) must verify the sidecar announce plus the
      Stop-block / asyncRewake hook delivery paths. cursor (unfree) must verify
      the extension announce and the state hooks. Turn each scenario's
      best-effort logs into hard asserts as it is validated on a real run.
- [ ] CI gating for the unvalidated scenarios. `.github/workflows/ci.yml` runs
      all four e2e checks in a matrix; only `e2e-pi` is proven green. Until
      opencode/claude/cursor are validated on real hardware, decide whether to
      mark those three `continue-on-error` (so only `e2e-pi` gates the merge)
      and promote each to gating as it goes green, versus letting the whole
      matrix gate now (likely red on the three heavy/unfree scenarios).

## Harness Registration

- [ ] Multiple approved launch sets per label (`Approved: label -> SET of
      Template`). TODAY the store is `label -> one Template` and `approve`
      OVERWRITES, so a kind with genuinely different commands per session
      (e.g. `quine --model A` vs `quine --model B`, a real vetted choice, NOT
      identity noise) can hold only one at a time: approving B silently drops
      A, and switching back re-prompts. This is a present bug masked only
      because no kind varies yet, and it contradicts approved_commands.rs's own
      doc ("any change to any field is a new set that needs its own approval").
      FIX: `registered` = the label's set CONTAINS the candidate; `register`
      INSERTS; `approve` inserts the SPECIFIC surfaced template (`current()`),
      so two pending variants approve independently. JSON becomes
      `label -> [template, ...]`; parse the old `label -> {}` shape as a
      one-element set for back-compat. The pending/deny machinery is ALREADY
      keyed on `(label, Template)` pairs (partition dedups on the full pair,
      Registrar.denied is a set of pairs), so the change is local. Keep the
      flood defense (dedup + deny-remembers); a per-label cap is YAGNI for now.
      COMPOSES WITH (does not replace) the {sessionId}/{cwd} placeholder work:
      placeholders remove FAKE per-session variation so the set stays small;
      the set handles REAL variation. Do this AFTER the resume-template branch
      lands. Do NOT placeholder a vetted flag like --model (that would let any
      value ride under one approval, defeating the gate).

## Inter-Agent Messaging

- [ ] "Show details" -> full approval dialog (Mechanism B, needs its own
      plan). ONE shared dialog seam serving BOTH approval kinds: a **message**
      (full body + provenance: from-dir, reply-handle session, target) and a
      **harness registration** (the exact launch argvs: spawn / resume /
      messageFlag / gui). Trigger: a "Show details" button in BOTH the tray menu
      AND the desktop notification, which spawns the dialog; the dialog carries
      the Approve/Deny (message: Allow once / always / Deny) buttons, so the
      operator reads the full content and decides in one surface. Fixes today's
      fragmentation: the actionable notification clips the message to 140 chars
      while the button-less "Show details" notification shows the full body, and
      registration has NO details view at all. Fallbacks: keep `notify-send`
      buttons and the tray menu buttons when no dialog resolves.
      Design branches: an external dialog (`zenity`/`kdialog`/`yad`, resolved by
      a ladder + `$CORRAL_DIALOG` override, shipped on corrald's PATH via the
      flake, zero Rust deps, generic look) vs a tiny spawned helper binary
      (`fltk` small / `egui` nicer, designed look, +dep +crate). Do NOT embed a
      windowing toolkit in the headless `corrald` process; run the dialog in its
      own thread and return the choice on the existing approval channel.
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

## Todo System (`todo/`, stage 1 shipped 2026-07-30)

Stage 1 is implemented and unit-green (73 crate tests, clippy clean, in the flake
package). Design `todo/SPEC.md`, policy `todo/DISPATCHER.md`, setup
`todo/README.md`. What remains, most load-bearing first.

1. **No real model has ever run the dispatcher loop.** Every test uses either a
   fake `Launcher` or the scripted stub LLM, so the one property the system's
   convergence depends on is unverified: **the dispatcher must write nothing to
   `todo.txt` when nothing needs changing.** If it writes anyway, the fingerprint
   advances, the watcher wakes it again, and the loop burns tokens indefinitely.
   Nothing in the code can prevent this — it is `DISPATCHER.md`'s job — so it
   needs a live run: `corral-todo init ~/todos`, one deliberately undispatchable
   line, `corral-todo watch --dir ~/todos --interval 5 -- pi`, then read the wake
   log. Different fingerprints in a row = not converging. Silence = settled.
   The wake log exists precisely to make this countable.
2. **`e2e-todo` is green** (`nix/tests/scenarios/todo.py`, `checks.e2e-todo`),
   first time on 2026-08-02, and now runs in `just e2e` and the CI matrix beside
   the four harness scenarios. All ten sections pass in a real VM: `init`, the
   CLI's ordering, the policy-less refusal, a hidden dispatcher spawned under
   cage with `FIRST_PROMPT`, an injected second wake into the *same* session, a
   dispatched worker landing in `proj-a` through corrald's gate with the charter,
   no window mapped anywhere, the file still parsing, and a quiet interval adding
   no wake. The run logs exactly three wakes with distinct fingerprints —
   `via spawn (2 items)`, `via inject (3 items)`, `via inject (5 items)` — which
   is the convergence property `todo/SPEC.md` asks for, made countable.

   **Three fixes got it there. Two were real product bugs, found by this scenario
   alone** (the third was the scenario's own stale count: §9 asserted four open
   items while five `add` calls reach it and the stub never completes one):

   - **corrald could not spawn any agent from its unit.** `corrald` resolves a
     terminal at launch time, but a systemd user service inherits no `$TERMINAL`
     and the VM has no `xdg-terminal-exec`, so every routed spawn died with
     `corrald: route spawn: no terminal found` while the *caller's* ack still
     said `accepted` (fire-and-forget hides it from the agent; only corrald's
     journal shows it). This is a **product bug, not a test bug**: the shipped
     `nix/hm-module.nix` unit had no `Environment=`, so every home-manager user
     with `daemon.enable` had a corrald that could route to live agents but never
     start one. FIX: new `programs.corral.daemon.terminal` option (null default,
     e.g. `"kitty -e"`) rendered as a **quoted** `Environment=` entry, since
     systemd splits unquoted spaces; `nix/tests/base.nix` sets it.
   - **The watcher stacked dispatchers.** An agent needs seconds between process
     start and announcing its record, which is longer than a poll interval, so a
     change arriving in that gap found no record, fell to the end of the wake
     chain, and spawned a *second* dispatcher (observed: two sessions in
     `~/todos`, 4s apart, both holding `FIRST_PROMPT`). Section 6 still passed
     because both existed before its snapshot — the assertion compared
     before/after sets, so it could not see a herd that predated it. FIX:
     `watch.rs` `SPAWN_GRACE` (60s) holds further spawns after one succeeds; the
     pending change waits and lands via `inject` once the record appears, and
     after the grace a silent spawn is presumed dead so spawning resumes.
     `spawned_at` clears on any inject/resume (proof a session exists). The
     scenario now asserts `len(sessions_before) == 1` so a herd fails loudly.

   **`e2e-pi` §8 was masking the corrald bug.** It wraps the hidden-spawn check
   in try/except as "best-effort (cage headless UNVERIFIED)", so the same
   `no terminal found` failure passed silently there. e2e-todo caught it only
   because its §7 asserts hard. Now that a hidden cage launch is **proven** to
   work under the VM's software GL (pixman, via `environment.sessionVariables`,
   which reaches user units through PAM), flip `scenarios/pi.py` §8 (and the
   §7 hidden-resume probe) from best-effort to hard asserts, and drop the
   corresponding "hidden agents in the VM" caveat from the e2e follow-ups above.
3. **The scenario cannot test policy, only plumbing.** The stub LLM is a rule
   table, so `e2e-todo` drives `corral_spawn_agent` directly rather than letting
   a dispatcher decide. It therefore proves the wake chain and the fan-out, and
   can never prove item 1. Do not let a green `e2e-todo` be mistaken for a
   validated dispatcher.
4. **No systemd user service ships.** Lifecycle is glue in `~/nixos` per
   AGENTS.md. Blocked on item 1 proving the command line, and it must carry the
   two env vars in `todo/README.md` ("Running It As A Service") or every wake
   fails.
5. **Nothing supervises a worker that stops without reporting** (accepted MVP
   limit, `todo/SPEC.md`). Its item sits at `status:progress` until a human looks.
   `target:` and `worker:` are recorded so a liveness sweep is cheap when wanted:
   compare `worker:` against the registry and return the item to open.
6. **The dispatcher's own failure modes are unmapped.** Predicted, in order of
   suspicion: it never records `worker:` because it does not recognise a charter
   handshake as one; it answers the handshake but the worker never reports (silence
   looks like success under fire-and-forget); it closes the item on the handshake
   rather than on the report. Each is a `DISPATCHER.md` wording fix, findable only
   by item 1.
7. **Stage 2 (the board TODO column) is unstarted and has two open design
   questions** — the drop granularity inside a stacked `PROGRESS`, and whether the
   third column holds done tasks or only dormant records. Both in
   `todo/SPEC.md`'s Open Questions. It changes `core::model::Column::ALL` and so
   `core::nav`, `core::transition` and both shells, for every agent, and is a
   deliberate amendment to the "board is a pure viewer" premise. Resolve the two
   questions before writing a plan.
8. **Smaller, deferred deliberately.** A mechanical sender in `corral-todo` (the
   outbox-submit path is specified in `todo/SPEC.md` if ever wanted; the MVP
   leaves all sending to the dispatcher's tools). Whitespace and blank lines are
   lost on first write (`todo/SPEC.md` known limits). `list` has no `--json`, so
   the dispatcher parses columns.

### Working on e2e-todo (read this first)

```sh
just e2e-one e2e-todo 2>&1 | tee ~/e2e-todo.log   # ~10 min, needs KVM, NOT /tmp
grep -nE "e2e-todo: OK|Exception|DIAG" ~/e2e-todo.log
```

On a failure the scenario dumps diagnostics before raising (`nix/tests/prelude.py`
`dump_diag` / `dump_messaging`, plus the todo scenario's own `watch_log`). The
four that answered every question so far, in order of usefulness: corrald's
journal (it alone shows a routed spawn failing while the caller's ack said
`accepted`), `ps aux` (shows the real `cage -- kitty -e pi` argv and how many
dispatchers exist), `state/registry` (what the boards would see), and the watch
unit's journal (`journalctl --user -u corral-todo-watch`, one line per wake).

Two scenario mechanics worth knowing before editing it. The stub LLM is a rule
table matching a substring against the **last message only**, runtime rules
before built-ins, so a rule keyed on wake text also fires for a freshly spawned
dispatcher's first prompt (§7 carries a comment on this). And the watcher's first
tick fingerprints whatever `todo.txt` already holds, so `watch` wakes once at
startup **before** the scenario's first `add` — the run therefore starts with two
changes inside pi's boot window, which is exactly what `SPAWN_GRACE` now absorbs.

A thread from the failing runs, now closed: corrald's journal showed only **one**
`route spawn: no terminal found` line though several wakes reached a dispatcher.
The green run explains it — the scenario's own §7 rule is what makes a dispatcher
call the tool, and it is posted late, so only the wakes after it dispatch at all.
The wake count and the route count are not meant to match.

### Sandbox gotchas (cost real time, 2026-07-31)

- **`/tmp` does not survive between tool calls** in the agent sandbox. Two
  `just e2e-one` runs were backgrounded with their logs in `/tmp`; the first was
  readable across several calls, then both vanished mid-session, losing the
  second run's failure detail entirely. Write long-running job logs somewhere
  persistent (the repo dir, ignored, or `~/`) and grep them as they grow.
- A VM check takes **~8-10 min** wall clock (build + boot + scenario), so
  background it and do useful reading meanwhile rather than blocking.
- `nix flake check --no-build` catches a broken module option or a bad
  `Environment=` render in seconds, and is worth running before any 10-minute
  VM round trip.

### Verification state at hand-off (2026-07-30)

Green and re-run after every change: `just test` (290 workspace tests, 73 of them
`corral-todo`), `just lint` (fmt + clippy `-D warnings`). `nix build` last
completed green at `95c9d1f`; the final commit `16878a2` touches only `.md`,
`.py`, `justfile` and `nix/tests/` — no Rust source — so the binaries are
unchanged, but the package build was **not** re-run to confirm it. Do that first
(`nix build`) before trusting the tree.

Verified by hand against real processes, not just tests: the `init` output and its
refusal to clobber; `list` ordering; a live ACP socket receiving exactly one
`session/prompt` per edit with the right wake text; the wake log's one-line-per-
change behaviour with distinct fingerprints; and sections 1-4 of `e2e-todo` inside
a real VM.

### Verification state at hand-off (2026-08-02)

`just e2e-one e2e-todo` ends in `e2e-todo: OK` (~9 min, run 4 of 4 that day), so
the corrald-terminal fix and the watcher grace are both confirmed against real
processes, not only unit tests. Also green: `cargo test --workspace`, `cargo
clippy --workspace --all-targets -D warnings`, `nix flake check --no-build`.
**Not** re-run since: `nix build`, and the four harness scenarios (`just e2e`
now includes e2e-todo, so the whole set is one command and ~45 min).

Still unproven, and the reason item 1 above stays open: no real model has driven
the loop. The green scenario proves the plumbing, never the policy.
