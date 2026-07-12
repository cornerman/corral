# Corral Board: Unified Selection and Honest Footer

## Problem

The board grew four action keys that hide only two verbs. `n` "new" and `c`
"create" both spawn an agent; they differ only in where the target directory
comes from (the selected card's cwd vs. a fuzzy picker). `⏎`/click "focus" and
`f` "find" both go to an agent; they differ only in how the target is picked
(the board cursor vs. a fuzzy picker). The duplicate English names are a
symptom: each verb was split into a spatial key and a picker key, and the
picker key needed a fresh word.

The root cause: the board cursor can select an *agent* but never a *directory*.
So spawning had to grow its own selectors (`c` and `n`), and `f` is a second,
redundant agent-selector bolted beside the cursor.

Two smaller footer defects share the theme of leaking detail the operator
should not carry:

- The footer advertises only `↑/↓ move`. Left/Right switch columns
  (`nav::move_col`) but go undocumented.
- `⏎ focus/resume` exposes the live-vs-dormant mechanism. To the operator both
  are "go there"; whether it focuses a window or resumes a session is an
  implementation detail.

## Design

### One omni-picker: "jump to anything" (`/`)

A single fuzzy overlay lists every destination and infers the verb from the
destination's type on Enter:

- a live agent → focus its window
- a dormant agent → resume its session
- a known directory → spawn a new agent there

This dissolves `find` and `create` into one act — a fuzzy jump to a
destination — that was only ever forked by target type. There is no "find" vs
"create"; there is one door, and whether the target already runs an agent is a
detail the tool infers. This mirrors dropping `resume` from the footer:
live-vs-dormant and exists-vs-does-not are map details, not verbs.

`/` opens it (the universal search/jump idiom). Both `f` and `c` are removed.

**Entries.** Every known directory (the cwd of any board session, live or
dormant, via `picker::gather_dirs`) appears as a spawnable `(new)` entry, *in
addition to* any agent running there. A directory with a live agent therefore
appears twice: once as the agent (go to it) and once as `(new)` (spawn another
there). The picker is thus the search-path superset of both `n` and "go".

**Order.** Agents first, in board order (`Board::selectable`: Requires Action,
Idle, Running, Dormant), then the `(new)` directory entries sorted by path.
Grouping keeps the two intents legible while fuzzy-typing narrows either.

**Labels.** Agents reuse `goto_label` (`title · basename`, with `(dormant)`
when dormant). Directory entries read `basename (new)`.

### Keep `n` as the only spawn shortcut

`n` still spawns an agent in the selected card's directory — the no-typing
"another agent in this same project" path, common enough to keep as a
dedicated key. It is now the *only* spawn shortcut; bulk/elsewhere spawning
goes through `/`.

### Two-stage mouse click

A left click no longer teleports the operator's window on a stray click. The
rule:

- click a card that is **not** the current selection → select it only
- click the card that **is** already selected → activate it (focus/resume)

So reaching a new agent by mouse is select-then-confirm (two clicks); clicking
the already-highlighted card acts immediately. Scroll and `hit_test` geometry
are unchanged.

### Honest footer

New help line (verbs with one name each, all four arrows documented, no
mechanism leak):

```
↑↓←→ move   ⏎ go   / jump   m msg   n new   d close/forget   q quit
```

## Implementation Notes

- **Typed picker entries.** Replace the `Overlay::Spawn(Picker)` and
  `Overlay::Goto(Picker, Vec<Agent>)` variants with a single
  `Overlay::Jump(Picker, Vec<Destination>)`, where

  ```rust
  enum Destination {
      Agent(model::Agent), // focus if live, resume if dormant
      NewDir(String),      // spawn a new agent in this cwd
  }
  ```

  The `Picker` stays string-based over the entry labels; `selected_original()`
  indexes back into the parallel `Vec<Destination>` (the pattern `Goto` already
  used). On Submit, match the `Destination`: `Agent` → `activate`, `NewDir` →
  `launcher.spawn`.

- **Building the list.** From `board.selectable()` build the `Agent` entries
  (label via `goto_label`); from `picker::gather_dirs(&board)` build one
  `NewDir` entry per directory (label `basename (new)`). Concatenate:
  agents, then dirs.

- **Picker overlay title.** One title (drop the `verb` parameter of
  `render_picker`), e.g. `" jump — type to filter, ⏎ select, esc cancel "`.

- **Mouse.** In the `MouseEventKind::Down(MouseButton::Left)` arm, after
  `hit_test` yields `idx`, extract a pure decision so it is unit-testable:

  ```rust
  enum Click { Select, Activate }
  fn click_action(clicked: usize, selected: usize) -> Click {
      if clicked == selected { Click::Activate } else { Click::Select }
  }
  ```

  `Select` sets `selected = idx`; `Activate` sets `selected = idx` then calls
  `activate_selected`.

- **Removals.** Delete the `f` and `c` key arms, the `Overlay::Spawn`/`Goto`
  variants, and the now-unused `verb` argument threading. `gather_dirs`,
  `goto_label`, `activate`, and `Picker` all survive.

## Testing

- `click_action` returns `Activate` only when `clicked == selected`, else
  `Select`.
- Jump-list construction: N selectable agents + M distinct dirs yields N agent
  entries followed by M `(new)` entries; a dir with a live agent yields both
  its agent entry and a `(new)` entry (appears twice).
- Existing `nav` and `hit_test` tests stay green (no geometry change).

## Non-Goals

- No directory column on the board (keeps "attention" and "launchpad"
  separate; the board still routes attention, the picker launches).
- No filesystem scan for directories; candidates remain the cwds agents have
  actually run in.
