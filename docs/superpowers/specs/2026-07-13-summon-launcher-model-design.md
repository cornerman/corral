# Corral as a WM-Summoned Background Launcher — Design Note

Status: EXPLORATION, not scheduled (2026-07-13). Captures the direction for
running corral as a persistent, hotkey-summoned launcher. Relates to VISION.md
build items 5 (systemd + hide) and 6 (tray), and to the vision's open "Hide
trigger" question. Do not build from this yet.

## The idea

Run corral like an application launcher (rofi/wofi shape): a persistent
background process you summon with a hotkey, act on (focus / resume / spawn /
message), and dismiss. Corral is already close — the `/` jump picker is a
launcher-style fuzzy picker over sessions; what is missing is the
summon/keep-alive/auto-hide lifecycle.

## Decision: delegate the hotkey to the WM; corral does not grab keycodes

Two reasons.

1. **Consistency with the existing split.** VISION.md already decided: corral
   owns behavior; the WM and nixos own lifecycle and visibility. Corral already
   delegates focus (`SwayFocuser`) and launch (`KittyLauncher`) behind seams. A
   global hotkey grabbed inside corral would pull "summon/visibility" back into
   corral, against that split.
2. **Wayland reality.** On X11 an app can grab a global key; on Wayland it
   structurally cannot — input routes through the compositor. The
   `xdg-desktop-portal` `GlobalShortcuts` interface exists but support is uneven
   across the target compositors (KDE/Hyprland implement it; sway does not;
   niri / herbstluftwm / noctalia vary). A WM keybind works on all of them with
   zero corral code and no portal dependency.

Self-listen is therefore rejected. If it is ever revisited, the prerequisite is
verifying the `GlobalShortcuts` portal matrix across sway/niri/herbstluftwm/
noctalia — but the WM-bind path makes that unnecessary.

## What the launcher shape costs corral

Almost nothing in corral code; the rest is nixos/WM deployment glue.

- **Keep-alive (nixos):** a systemd user service running `kitty -e corral`, with
  restart-on-failure. This is vision item 5, and the fix for the earlier
  silent-drop (a mid-session crash stayed dead because `exec_always` only
  re-runs on WM reload).
- **Summon / toggle (WM config, nixos):** one keybind per compositor that shows
  or hides corral's scratchpad window, e.g. `bindsym $mod+c ...` on sway/i3 and
  the niri / herbstluftwm / noctalia equivalents. No corral code.
- **The only new corral code — a `WindowHider` seam** beside `WindowFocuser`, so
  corral can hide *itself* after an action. The WM keybind covers explicit
  toggle, but the "dismiss without picking" and "hide after go" cases need
  corral to move its own window to the scratchpad. A sway impl reuses the
  `/proc` parent-walk `SwayFocuser` already does (corral's pid -> its terminal
  window). Other compositors are future seam impls (YAGNI: build when used).

So corral stays WM-agnostic behind seams. The compositor list reduces to "which
`WindowFocuser` / `WindowHider` impls exist"; the summon binding is nixos glue
that already spans the whole list.

## Open questions (unresolved)

1. **Hide trigger set.** When should corral hide itself (routing keeps running)?
   Candidates: dismiss-only (Esc / no target grabs focus), after-every-action
   (go also hides), or not-corral's-job (WM toggle only). The vision's research
   found vanilla sway/i3 scratchpads do not auto-hide on focus loss, so at least
   the dismiss case needs corral to hide itself; the go case is a judgment call.
2. **Tray interaction (vision item 6).** A `ksni` tray icon (attention/pending
   count, click to open) is a second summon path alongside the hotkey; whether
   the tray click and the keybind share one show/hide code path is unsettled.

## Relation to the pure-reflector invariant

Summon/hide adds no persistent state to corral: it stays a pure reflector over
the filesystem registry. The `WindowHider` seam is another outward action (like
focus/launch/kill), not stored state. Keep-alive and visibility live entirely in
nixos/WM config. The invariant holds.
