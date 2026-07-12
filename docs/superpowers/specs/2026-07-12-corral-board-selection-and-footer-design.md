# Corral Board: One Selection, Two Verbs

## Problem

The board grew four action keys that hide only two verbs. `n` "new" and `c`
"create" both spawn an agent; `⏎`/click "focus" and `f` "find" both go to an
agent. Each verb was split into a *spatial* key (acting on the board cursor)
and a *picker* key (a fuzzy overlay), and the picker key needed a fresh English
word. The duplicate names — new/create, focus/find — are the symptom.

The root cause: the board cursor can select an *agent* but never a *directory*,
so spawning grew its own selectors (`c`, `n`), and `f` is a second, redundant
agent-selector beside the cursor.

Two smaller footer defects leak detail the operator should not carry:

- The footer advertises only `↑/↓ move`; Left/Right switch columns
  (`nav::move_col`) but go undocumented.
- `⏎ focus/resume` exposes the live-vs-dormant mechanism. To the operator both
  are just "go there".

## Design

One selection, two verbs, one rule everywhere.

### Two verbs, chosen by a modifier — on both the board and the picker

- **Enter** = go to the selected agent (focus if live, resume if dormant).
- **Shift+Enter** = spawn a new agent in the selected agent's directory.

The same two-key rule holds whether the selection is the board cursor or a row
in the picker. A *modifier*, not a second English word, chooses go-vs-spawn, so
there is no "new" vs "create" and no "focus" vs "find". `n`, `c`, and `f` are
all removed.

### `/` opens the picker: a fuzzy view of the same agents

`/` opens a fuzzy overlay listing every selectable agent (`board.selectable()`:
Requires Action, Idle, Running, then Dormant), one row per agent, labelled with
`goto_label` (`title · basename`, `(dormant)` when dormant). It is the search
path to the same selection the cursor gives spatially; Enter and Shift+Enter
act identically there. No directory entries, no `(new)` rows, no duplication —
each project appears once, and Shift+Enter on its agent starts a fresh one in
its directory. This subsumes the old `c` (spawn in a known project) without a
separate list.

### Two-stage mouse click

A left click no longer teleports the operator's window on a stray click:

- click a card that is **not** the current selection → select it only
- click the card that **is** already selected → go to it (focus/resume)

Reaching a new agent by mouse is select-then-confirm (two clicks); clicking the
already-highlighted card acts at once. Scroll and `hit_test` geometry are
unchanged. The mouse does the "go" verb only; spawning stays on the keyboard
(Shift+Enter).

### Honest footer

```
↑↓←→ move   ⏎ go   ⇧⏎ new   / jump   m msg   d close/forget   q quit
```

All four arrows documented, one name per verb, no mechanism leak.

## The Shift+Enter Constraint (kitty keyboard protocol)

In a plain terminal, Shift+Enter is indistinguishable from Enter — both arrive
as `KeyCode::Enter` with no modifier. To tell them apart, corral pushes
`KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` at startup and pops it on
exit. kitty supports this; corral already hard-depends on kitty
(`KittyLauncher`) and sway, so requiring it for corral's own window is
consistent.

- Push the flag only when `crossterm::event::supports_keyboard_enhancement()`
  reports true; pop it in the same teardown path as `DisableMouseCapture`.
- Detect spawn with `key.modifiers.contains(KeyModifiers::SHIFT)` on
  `KeyCode::Enter`.
- Use only `DISAMBIGUATE_ESCAPE_CODES` (not `REPORT_EVENT_TYPES` or
  `REPORT_ALL_KEYS_AS_ESCAPE_CODES`), so ordinary keys and the existing
  `KeyEventKind::Press` filter are unaffected.
- Fallback: where the protocol is unsupported, Shift+Enter is not distinguished
  and spawning is unavailable. Documented; acceptable for a kitty-centric tool.

## Implementation Notes

- **Overlays collapse.** Remove `Overlay::Spawn(Picker)` and
  `Overlay::Goto(Picker, Vec<Agent>)`; add one
  `Overlay::Jump(Picker, Vec<model::Agent>)`. `Compose` is unchanged. The
  picker payload is exactly the old `Goto` (agents only), so no new entry type
  is introduced.

- **Picker input gains a spawn outcome.** `picker_input` (or its caller) must
  see the modifier. Add `PickerInput::SubmitSpawn` returned for Shift+Enter;
  `Submit` (plain Enter) → `activate(agent)`, `SubmitSpawn` → spawn in
  `agent.cwd`. Pass `KeyEvent` (not just `KeyCode`) into `picker_input`.

- **Board keys.** In the main match, `KeyCode::Enter` branches on the SHIFT
  modifier: no shift → `activate_selected`; shift → spawn in the selected
  agent's dir (`launch::default_cwd(board.selectable().get(selected))`, keeping
  the `$HOME` fallback). Delete the `n`, `c`, and `f` arms.

- **Mouse.** After `hit_test` yields `idx`, use a pure decision so it is
  unit-testable:

  ```rust
  enum Click { Select, Go }
  fn click_action(clicked: usize, selected: usize) -> Click {
      if clicked == selected { Click::Go } else { Click::Select }
  }
  ```

  `Select` sets `selected = idx`; `Go` sets `selected = idx` then calls
  `activate_selected`.

- **Dead code removed.** `picker::gather_dirs` and `Picker::selected_dir`
  (only the spawn picker used them) go. `Picker::selected_original`,
  `goto_label`, `activate`, and `default_cwd` all survive.

- **Enhancement flags.** Push/pop around the run loop in `main`, guarded by
  `supports_keyboard_enhancement()`.

## Testing

- `click_action` returns `Go` only when `clicked == selected`, else `Select`.
- Jump-list is exactly `board.selectable()` in order (one row per agent, no dir
  rows): assert length and order against a seeded board.
- Existing `nav`, `hit_test`, and `model` tests stay green (no geometry or
  model change).

## Non-Goals

- No directory column on the board (attention vs. launchpad stay separate).
- No filesystem scan; the picker lists agents, and spawning targets an existing
  agent's directory. Starting an agent in a never-before-used directory remains
  a manual terminal launch (corral's founding premise).
- No mouse spawn (no Shift+click); spawning is a keyboard verb.
