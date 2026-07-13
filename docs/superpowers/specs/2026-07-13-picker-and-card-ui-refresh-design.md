# Picker and Card UI Refresh

Two related presentation changes that share one theme: better overview when
there is a lot on screen. Both live in `crates/board/src/ui.rs`; the picker
also restructures `crates/board/src/picker.rs`. No model, watch, or protocol
change.

## Motivation

- Board cards pack the title and directory onto one line, so a long title (the
  common case) shrinks or drops the directory. The title deserves the whole
  line.
- The Idle column has no age, so its second line is often near-empty: a wasted
  row.
- A Requires Action card is the one that demands the operator, yet the question
  it is blocked on only shares a line with the age.
- The `/` picker is a flat fuzzy list. When many sessions run across many
  projects, a directory-grouped, state-colored view gives a far better
  overview than one undifferentiated column.

## Part 1 — Board Card Relayout (`ui.rs::card`)

Cards become variable height; the existing blank spacer line delimits them, so
varying height still reads as distinct blocks. Card height then tracks how much
the card has to say, which draws the eye to the fuller (more urgent) cards.

Lines, top to bottom, with empty lines dropped:

| Column          | Lines                                          |
| --------------- | ---------------------------------------------- |
| Requires Action | title / basename / question / age-blocked      |
| Running         | title / basename / activity · quiet-age        |
| Idle            | title / basename / activity (if any)           |
| Dormant         | title / basename / record-age (whole dimmed)   |

Rules:

- **Title** owns a full-width line, truncated only when it overflows the column
  width. Dormant titles stay dimmed (as today).
- **Basename** of `cwd` on its own dim line (unchanged content, `basename`,
  just moved off the title line).
- **Requires Action** puts the question on its own line. The question is the
  `activity` value (the extension already leads `SALIENT_ARGS` with
  `"question"`, so a blocked agent's activity is the question text). The age
  (time blocked) goes on the line below it. The question is NOT also repeated
  in a combined meta line.
- **Other columns** keep the combined meta line `activity · age` (Running:
  quiet-age; Dormant: record-age; Idle: no age).
- **Empty lines drop.** A card with no activity and no age (an idle agent that
  has run no tool yet) collapses to title / basename / spacer. No padding to a
  fixed height: the columns are independent stacks that never align row-for-row,
  so fixed height would only reintroduce the wasted Idle row.

`card_meta_line` stays for the combined columns. Requires Action no longer uses
it; instead the card emits the question line (from `agent.activity`) and a
separate age line (from `meta.in_state`).

## Part 2 — `/` Picker Redesign (`picker.rs` + `ui.rs::render_picker`)

A directory-grouped, state-colored fuzzy list. The board is the state view
(columns encode state); the picker earns its keep as the place view (grouped by
directory), with color carrying state since position no longer can.

### Structure

- Agents are grouped under a **basename directory header** (dim, non-selectable
  label row).
- Up/Down move only between agent rows, skipping headers.
- Groups are ordered by their first agent's board attention priority
  (`board.selectable()` order is preserved), so the most urgent directories
  surface first.

### Agent row (one line)

`<glyph> <title>   <dim meta>`

State palette (glyph + title emphasis):

| State           | Glyph | Color      | Title      |
| --------------- | ----- | ---------- | ---------- |
| Requires Action | `●`   | red        | bold       |
| Running         | `●`   | green      | normal     |
| Idle            | `○`   | white      | normal     |
| Dormant         | `·`   | dark-gray  | dim (row)  |

The dim meta is the same short activity/age string the board uses, so a row
carries what the agent is doing without opening the board.

### Query

One box. The query matches the directory path OR the title (case-insensitive
subsequence, as today). An agent row shows if either matches. A group survives
if any of its agents survive; when the query matches the directory itself, all
its agents stay.

### Tab filter

The Tab key cycles a category filter: All → Live → Dormant → All. It narrows
which agents are considered before grouping and fuzzy-matching. Default All.
(Live = `Origin::Live`; Dormant = `Origin::Dormant`.)

### Verbs (unchanged)

- Enter = go: focus a live window, resume a dormant session.
- Shift+Enter = spawn a new agent in the selected agent's directory.

### Internals

`picker.rs` stops storing flat label strings. It carries structured candidates,
each pairing an `Agent` (or an index into the parallel agent list) with its
group key (the basename). The picker computes, from the query and the Tab
filter:

- the visible rows in order: interleaved header rows and agent rows;
- the flat list of selectable agent-row positions (headers excluded);
- `selected` indexes into the selectable agent rows;
- `selected_original` maps the selection back to the original agent index (the
  caller still holds the parallel `Vec<Agent>` to act on).

`render_picker` walks the visible rows, styling headers dim and agent rows per
the palette, and highlights the selected agent row.

## Testing

Pure logic in `picker.rs` is unit-tested (existing pattern):

- fuzzy match against path or title;
- grouping: agents collect under their basename; empty groups drop;
- a directory-matching query keeps all agents in that group;
- Tab filter restricts to Live / Dormant / All;
- Up/Down skip headers and clamp at ends;
- `selected_original` maps a selection back to the right agent index after
  filtering.

`ui.rs` rendering stays exercised by the existing snapshot-free unit tests
where present; the card line-composition helper (which lines a column emits)
is factored to be unit-testable if it is not already.

## Out of Scope

- No per-state color on the board (column position still encodes state).
- No change to discovery, watch, model, or the ACP surface.
- No new agent metadata: everything shown already exists on `Agent`.
