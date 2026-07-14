# iced GUI rewrite + base16 theming — design

Design v1 — 2026-07-14

## Why

The egui/eframe GUI has hit its aesthetic ceiling. egui renders text with
`ab_glyph` (grayscale antialiasing, no hinting, no subpixel), so it can look
tidy but never matches the terminal's font quality, and immediate-mode styling
is utilitarian. The goal is *true beauty* for the no-terminal shell: crisp,
shaped text and a flat, self-owned aesthetic that reads intentionally on any
desktop.

Chosen toolkit: **iced**. It renders text via **cosmic-text** (rustybuzz
shaping + swash rasterization + fontdb system-font matching) — terminal-grade
crispness — and is fully self-themed, so corral's flat look is consistent on
sway / Hyprland / niri / KDE / GNOME rather than looking like a misplaced GNOME
app (the fate of GTK4 + libadwaita off GNOME). iced is also the COSMIC desktop's
toolkit, so it is a durable bet.

GTK4 was rejected: its one advantage (native look) only holds on GNOME, and the
maintainer runs wlroots/X11.

## Scope

Replace **only** `crates/gui` (binary `corral-gui`). `core`, `board` (TUI), and
`daemon` are untouched; the `core::engine::Engine` seam already isolates the
shell. The TUI keeps using the terminal's own palette and is not themed here —
**base16 theming is a GUI-only concern.**

The egui GUI stays on `main` and keeps working until the iced shell is ready to
replace it (no half-broken intermediate on `main`).

## Functional parity (port, do not redesign behavior)

The iced board must reproduce the current GUI's behavior:

- Four columns: Requires Action, Idle, Running, Dormant (`Column::ALL` order).
- Cards: title (truncated) + kind badge, `~`-abbreviated cwd, activity·age line;
  a left accent bar in the state color; selected row faintly tinted. Long text
  never widens a card (truncate with ellipsis).
- A centered filter line ("corral" wordmark left at the filter's height, filter
  centered, status right), narrowing cards by whole content
  (`Agent::matches_query`).
- Bottom key-hint footer as a thin bar; the corral pen mark bottom-right (drawn,
  not a glyph).
- Keys (full TUI parity): arrows / `hjkl` move (Left/Right only in command mode),
  Enter go, Shift+Enter spawn, `m` message (compose overlay), `d` dismiss,
  `/` focus filter, Esc clears filter then quits, `q` quits.
- Two-stage card click: first click selects, click on the selected card goes.
- Selected card always scrolled into view; long columns scroll.
- Follows system light/dark live (freedesktop appearance portal).
- Drives `core::engine::Engine`; actions via `core::focus` / `core::launch` /
  `core::prompt`, exactly as today.

## base16 theming

The palette + loader live **in the iced shell** (only one shell consumes them;
not hoisted into `core` — YAGNI).

- **Palette:** the standard 16-slot base16 (`base00..base07` bg→fg ramp,
  `base08..base0F` accents). Reuse the existing `Base16` shape.
- **File format:** the current tinted-theming spec YAML —
  `system: base16`, `name:`, `author:`, `variant: dark|light`,
  `palette: { base00..base0F }` (hex, lenient on a leading `#`). The whole
  `tinted-theming/schemes` gallery (~250 schemes) drops in unchanged.
  Needs a small YAML-parse dependency.
- **Selection:** a **dark/light preset pair**, auto-switched live by the
  freedesktop appearance portal (reuse the existing `dbus-send` probe). Env:
  `CORRAL_THEME_DARK` / `CORRAL_THEME_LIGHT` name a preset; default pair =
  `solarized-dark` / `solarized-light`.
- **Discovery:** presets = built-ins ∪ every `*.yaml` in the themes dir
  (`~/.config/corral/themes`, override `CORRAL_THEME_DIR`); a file overrides a
  built-in of the same name.
- **Built-ins:** only the Solarized dark/light pair (the zero-config default).
  Everything else comes from the themes dir.
- **No runtime download.** corral stays offline / filesystem-native (no network
  anywhere, per CONVENTION.md). Getting the gallery is the packaging layer's
  job: add `tinted-theming/schemes` as a nix flake input and symlink it into the
  themes dir, or `git clone` it there. This mirrors what `tinty` does (it clones
  the schemes repo) without any runtime network machinery.
- **Mapping:** `Base16 → iced` styling lives in the shell (the analogue of
  today's `Base16 → egui Visuals`). Flat: no rounding, no shadows, thin accents.

## Fonts

iced/cosmic-text loads system fonts via fontdb and rasterizes with swash, so
text is crisp by default. Pick a clean default family; a system font is used
automatically. (This is the concrete payoff over egui.)

## Open items / to decide during implementation

- iced version + renderer backend (wgpu vs tiny-skia software). wgpu needs a
  vulkan/GL loader on the flake's `LD_LIBRARY_PATH` (add `vulkan-loader`);
  tiny-skia avoids the GPU stack but is slower. Decide from build reality on the
  NixOS flake.
- Exact YAML dep (`serde_yaml` is archived; candidates: `serde_yml`,
  `yaml-rust2`, or a tiny hand parse since base16 files are trivial).
- Per-column scroll widget choice in iced.
- Flake: add the graphics libs iced/wgpu needs; keep `wrapProgram` driver path.

## Non-goals

- No change to the TUI, core, or daemon.
- No config file (env + themes dir only).
- No in-app theme switcher in v1 (env-selected; live switch is only light/dark).
- No network / on-demand theme download.
