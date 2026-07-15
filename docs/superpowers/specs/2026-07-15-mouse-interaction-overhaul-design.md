# Mouse Interaction Overhaul (TUI + GUI)

Date: 2026-07-15
Status: approved, ready for implementation planning

## Summary

Replace the corral board's two-stage single click with a desktop-standard click
model and add a right-click context menu, in both shells (`corral` TUI and
`corral-gui`), per the TUI/GUI parity hard rule. No middle-click gesture and no
on-card delete button: deletion is reachable only through the menu.

## Motivation

Today a single left click is two-stage: the first click selects a card, a second
click on the already-selected card goes to it (`click_action`/`Click` in
`crates/board/src/main.rs`; `Message::CardClicked` in
`crates/gui/src/dashboard.rs`). This is unusual and easy to trigger
accidentally. Standard desktop behavior (single = select, double = open) is more
predictable, and a right-click menu makes every footer action reachable by mouse
without memorizing keys.

## Decisions

Resolved during brainstorming:

- Single left click = select only; it never navigates. It selects the card under
  the cursor for later keyboard actions.
- Double left click = go, the same action as Enter (focus a live window / reveal
  a hidden card / resume a dormant one).
- Enter still goes; Shift+Enter still spawns; scroll still moves the selection.
- Right-click a card selects it first, then opens a context menu anchored at the
  cursor.
- Menu entries use fixed generic labels (not state-adapted), in footer order,
  with the destructive entry last: `Go`, `Message`, `Spawn`, `Toggle hidden`,
  `Dismiss`.
- Picking an entry runs the same code path as its key on the selected card, then
  closes the menu.
- Esc or a click outside the menu closes it without acting.
- Right-click on empty space (no card): do nothing.
- No middle-click delete and no X-on-card button. Deletion is only the menu's
  `Dismiss`.
- Double-click threshold: ~400ms.

Rejected: middle-click delete and X button (accidental-delete surface, TUI has
no hover); state-adapted menu labels (more work, chosen against for simplicity);
keeping the two-stage model alongside double-click (a slow double-click and a
two-stage click become indistinguishable); an empty-space/global menu (spawn
needs a card to know kind and dir).

## Architecture

Shared, UI-free decision logic lives in `corral-core`; only rendering differs
between ratatui and iced.

### corral-core (new pure, unit-tested helpers)

- Double-click classifier. Given the last click's `(index, Instant)`, the new
  click's `(index, Instant)`, and the threshold (~400ms), return `Select` or
  `Go`. Same index within threshold = `Go`, else `Select`. A `Go` also resets
  the tracked state so a triple click is not two gos.
- Context-menu model. An enum of the five actions in display order
  (`Go`, `Message`, `Spawn`, `ToggleHidden`, `Dismiss`) with their fixed labels,
  plus a mapping from a chosen action to the existing action dispatch. This
  keeps entry order/labels single-sourced for both shells.

Both are pure functions/enums with no toolkit types.

### corral (TUI, `crates/board`)

- Mouse handling in `src/main.rs` already reads crossterm `Mouse` events.
  - `Down(Left)`: feed the core double-click classifier (tracking last click
    `(idx, Instant)`); `Select` sets the selection, `Go` runs the existing go
    path. Remove `click_action`/`Click` (replaced by the core classifier). The
    footer hit-test path stays.
  - `Down(Right)`: hit-test to a card; if hit, select it and open the menu
    anchored near `(m.column, m.row)`; if not, ignore.
  - `Down(Middle)`: ignored.
- A context-menu popup rendered in `src/ui.rs`: a bordered box near the cursor
  listing the five entries, the first highlighted. While open it captures input:
  Up/Down move the highlight, Enter/left-click-on-entry runs it, Esc or a click
  outside closes. Reuse the same dispatch functions the keys call.
- Menu geometry (position, size, per-entry hit-test) is a pure helper in
  `ui.rs`, unit-tested like `hit_test`/`footer_hit_test`.

### corral-gui (`crates/gui`)

- `src/dashboard.rs`:
  - Replace the two-stage `Message::CardClicked(idx)` logic. On
    `mouse_area::on_press`, feed the core classifier with a stored last-click
    `(idx, Instant)`; `Select` sets `self.selected`, `Go` runs the go path.
  - Add `mouse_area::on_right_press` per card emitting a new
    `Message::CardRightClicked(idx)`, which selects the card and sets menu state
    (open, anchor point).
  - Render the menu as a floating overlay via iced `stack`, positioned at the
    anchor; each entry is a `mouse_area` row emitting a `Message::MenuPick`.
    A full-window transparent `mouse_area` behind the menu emits
    `Message::MenuDismiss` on press (click-outside to close); Esc also dismisses.
  - `MenuPick(action)` maps through the core menu model to the existing action
    methods (go / message-compose / spawn / toggle-hidden / dismiss), then
    closes the menu.

## Data Flow

```
left click  -> core::classify_click(last, now, 400ms) -> Select | Go
right click -> select card + open menu at cursor
menu pick   -> core menu action -> existing dispatch (go/message/spawn/hide/dismiss) -> close
Esc / click-outside -> close menu (no action)
```

## Error Handling

No new failure modes. The dispatched actions keep their current fail-loud
behavior (focus/resume/launch errors surface in the status line as today). An
out-of-range menu anchor is clamped to stay on screen (pure geometry helper).

## Testing

- Core: unit tests for the double-click classifier (same index in-threshold =
  Go; different index = Select; over-threshold = Select; Go resets state) and
  for the menu model (entry order, labels, action mapping).
- TUI: unit tests for the menu geometry/hit-test helper and the anchor clamp,
  alongside existing `hit_test` tests.
- GUI: logic exercised through `Dashboard::update` where testable; rendering is
  visual.
- Manual: verify double-click go, single-click select-only, right-click menu and
  each entry, Esc/outside dismissal, empty-space no-op, in both shells.

## Out of Scope

Middle-click, on-card delete affordance, drag-and-drop, empty-space/global menu,
state-adapted menu labels, confirmation dialog for Dismiss.

## Docs to Update on Landing

`AGENTS.md` (board key/click descriptions in the TUI and GUI crate sections and
the "Interfaces to the Outside World" CLI entries) and `README.md` if the key
table mentions click behavior.
