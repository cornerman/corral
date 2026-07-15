# Hidden Agents — Design

## Problem

`corral_message_agent` with `force_new` (and any dir-target that spawns a new
agent) opens a terminal or GUI window without the operator asking. The window
maps on the operator's compositor, steals focus, and clutters the screen. The
operator wants such agents to start and run in the background, invisible, and be
brought into view only on demand.

## Requirements (hard)

- **R1 Never blink.** No window ever maps, even for a single frame, on the
  operator's host compositor.
- **R2 No user WM config.** No requirement that the operator install a
  compositor rule or edit their WM config.
- **R3 No window while hidden.** Nothing visible on the host while an agent is
  hidden — not parked, not tiny, not off-screen.
- **R4 Works for `gui:true`.** Self-windowing GUI agents (quine, cursor) must
  hide too, under the same mechanism.
- **R5 Reveal on demand.** Bring a hidden agent into the foreground when asked.
- **R6 Runs while hidden.** The agent does real background work: binds its
  socket, announces, drives `state_update`, processes its first message.

## Why headless is the only fit (first principles)

A live window lives on exactly one compositor, and neither Wayland nor X can
migrate a running client's surface to a different display server. Two
consequences fix the whole design:

1. **Zero-blink on the host is impossible without a WM rule.** A client maps its
   window the moment it commits its first surface; only a compositor rule can
   suppress that, and a pre-map rule needs a match criterion (`app_id`) known
   before launch — which `gui:true` apps set themselves, so it cannot be forced.
   Every host-side hack (scratchpad park, special workspace, minimize, 1x1,
   off-screen) is therefore either post-map (flashes) or unavailable for
   `gui:true`, and each is a separate per-compositor implementation of uneven
   quality (sway/Hyprland have a stash, niri has none, generic X11/EWMH has
   none). "Make it tiny" is host-park with worse invisibility (a visible dot
   plus a taskbar entry) and the same blink and focus-steal.
2. **Reveal cannot be a live move.** Because a surface cannot cross compositors,
   revealing a hidden agent cannot hand its running window to the host. Reveal
   is a **resume**: stop the hidden instance, relaunch from the persisted
   session in a foreground window.

The window must therefore be born on a compositor that is *not* the host — a
headless one — and reveal is resume. This satisfies R1–R6 uniformly on every
backend, at the cost of a relaunch on reveal (~1–2s, plus any un-persisted
mid-turn state; the transcript is intact).

Options considered and rejected: scratchpad park (blinks, per-compositor,
absent on niri/X11), WM-rule by class (violates R2, still blinks for gui), tmux
detached pty (no GUI, breaks pid focus), terminal `--start-as=hidden` (terminal-
specific, no GUI), per-harness headless-serve mode (not agnostic, GUI apps often
lack one), deferred spawn (fails R6 — no background work).

## Mechanism: per-agent headless cage

A hidden agent runs inside its own headless compositor:

```
setsid --fork env WLR_BACKENDS=headless CORRAL_HIDDEN=1 cage -- <normal launch argv>
```

- **`cage`** is a single-application kiosk compositor (wlroots). It hosts
  exactly one agent and exits when that agent exits — self-cleaning, no
  singleton to own, and any spawner (corrald now, a future board key later) can
  start one with no coordination.
- **`WLR_BACKENDS=headless`** is mandatory and load-bearing. wlroots would
  otherwise auto-pick its **X11 backend on an X11 host**, which renders cage's
  output *into a visible host window* — the blink we are avoiding. Forcing the
  headless backend makes cage render to memory with no host window, identically
  on X11, Wayland, or a bare session. cage touches the host display server not
  at all.
- **XWayland** ships with cage, so both Wayland-native agents (foot, quine) and
  X11 agents (kitty in X11 mode, Electron/cursor) render inside cage. cage sets
  `WAYLAND_DISPLAY` and `DISPLAY` in the child env, so the agent connects to
  cage's compositor, never the host's. Host-agnostic: no Xvfb/Xephyr path is
  needed, and neither would be universal (Xvfb cannot host Wayland-only `foot`;
  Xephyr shows a visible nested window).
- **`CORRAL_HIDDEN=1`** signals the adapter to record the hidden flag (below).

The agent runs fully while hidden: it binds its workdir-local socket, announces,
serves ACP, and drives `state_update`, so it appears as a live card and does
background work.

Dependency: `cage` is shipped via the flake (devShell + wrapped binary PATH);
not installed globally. If `cage` is absent, a hidden spawn fails loud.

## Reveal and hide are always kill-and-resume

Because there is no live move, changing an agent's placement is always: kill the
current instance, relaunch on the other side.

- **Kill** targets the agent pid carried in the record's socket filename
  (`<label>-<pid>.sock`). Killing the agent makes cage exit (its only app is
  gone) and the adapter clears the record's `socket` → the record goes dormant.
- **Reveal (hidden → visible):** kill, then run `resumeCommand` in a foreground
  terminal/GUI on the host (the existing dormant-resume path, `LaunchMode`
  without `hidden`), then focus.
- **Hide (visible → hidden):** close the visible window (`focus.rs::close` by
  window pid — the window exists on the host, so this works), then run
  `resumeCommand` with `hidden: true` (into cage).
- **Start hidden (dormant → hidden):** run `resumeCommand` with `hidden: true`.

## How a card learns it is hidden

A new optional boolean `hidden` in the registry record, symmetric with `gui`.
corral sets `CORRAL_HIDDEN=1` in the child env at a hidden spawn; the adapter,
which already writes the record, copies the flag in. A resume-visible sets no
such env var, so the flag clears on reveal — self-correcting. Non-cooperating
adapters simply never advertise `hidden`, so the feature is opt-in per kind,
exactly like `gui`. corral reads `hidden` from the record (the shared truth);
it does not infer hidden from a missing host window (that would conflate hidden
with crashed and violate fail-loud).

## Card rendering (TUI + GUI parity)

A hidden running session stays in its normal state column (Requires Action /
Idle / Running) — the operator watches it work — with a small dim **`hidden`**
tag beside the kind badge. Both shells render it (hard parity rule); only the
ratatui vs iced drawing differs. During a reveal the card briefly flickers
dormant then live as the instance is killed and resumed; acceptable.

## Keys (both shells)

- **`h`** toggles placement of the selected session:
  - visible-running → **hide** (close window, resume into cage);
  - hidden → **unhide** (reveal);
  - dormant → **resume hidden** (start in the background).
- **enter** on a hidden card → **unhide + focus** (reveal). Enter means "take me
  there," which for a hidden agent means reveal it.
- **shift+enter** on a hidden card → spawn a *new* agent of that kind, also
  hidden ("another one in the background"); on a visible card it spawns visible
  as today. The hidden-ness of the spawn follows the selected card.
- **m** on a hidden card → delivers the message to the still-hidden background
  agent; does **not** unhide.
- Nothing but `h` and enter unhides.

## Wiring the original annoyance

`corral_message_agent` `force_new` and dir-spawns route through corrald.
corrald spawns the new agent **hidden by default** (`LaunchMode { hidden: true
}`), so an uninvited agent never pops a window. It appears as a hidden card the
operator reveals on demand. Operator-driven spawns from the board stay visible
(the operator asked for them); the operator hides one explicitly with `h`.

## Code touch-points

- `core/launch.rs`: add `LaunchMode { hidden: bool }`; `setsid_args` wraps the
  argv in `env WLR_BACKENDS=headless CORRAL_HIDDEN=1 cage --` when `hidden`.
  Pure, unit-tested (both branches, gui + terminal).
- `core/model.rs`: add `Agent.hidden`; add a hide/reveal helper that kills the
  record pid and resumes with the right `LaunchMode`.
- `core/discovery.rs`: parse `hidden` from the record (default false).
- `crates/board` + `crates/gui`: `h` key, the `hidden` badge, and enter /
  shift+enter / m branching on `hidden` (parity).
- `crates/daemon/router.rs`: agent-initiated spawns default to `hidden: true`.
- `extensions/corral-pi.ts` (then opencode, claude, cursor): write `hidden` from
  `CORRAL_HIDDEN` into the record in `writeRegistry`.
- `flake.nix`: add `cage`.
- `CONVENTION.md` / `AGENTS.md`: document the `hidden` record field and the
  `CORRAL_HIDDEN` env signal.

## Deliberate v1 limits

- Reveal/hide lose any un-persisted mid-turn state (e.g. an approval open at that
  instant); the persisted transcript is intact.
- A hidden spawn requires `cage` on PATH; absence fails loud (no silent
  fallback to a visible window, which would reintroduce the blink).
- A board-side `shift+m` (compose, then send hidden) is out of scope now; the
  mechanism already supports it and it can be added later without core change.
- One `cage` process per hidden agent. Lightweight; a shared headless compositor
  is a later optimization only if process count ever matters.
