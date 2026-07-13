# Picker and Card UI Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relayout board cards (title/basename on their own lines, adaptive height, question line in Requires Action) and rebuild the `/` picker as a directory-grouped, state-colored fuzzy list with a Tab scope filter.

**Architecture:** Pure presentation change in `crates/board`. `picker.rs` becomes a structured, directory-grouping candidate model (no ratatui); `ui.rs` owns all styling (glyph/color badges, card lines); `main.rs` wires the simplified `Overlay::Jump(Picker)`. No model, watch, discovery, or ACP change.

**Tech Stack:** Rust, ratatui, crossterm. Tests are plain `#[cfg(test)]` unit tests run with `cargo test`.

## Global Constraints

- Keep `picker.rs` free of ratatui: it holds pure selection/grouping logic; all color and glyph styling lives in `ui.rs`. (Existing separation.)
- Board columns still encode state by position; no per-state color on board cards.
- Verbs unchanged: Enter = go (focus live / resume dormant), Shift+Enter = spawn in the selection's dir.
- Lint clean under `just lint` (clippy `-D warnings`) and formatted with `cargo fmt`.
- Run checks from the repo root; the crate is `corral` at `crates/board`.

---

### Task 1: Board card relayout (fixed height)

Give the title and the basename their own lines, and fill the single info line per column (Idle now shows time-idle). Cards stay fixed height: `CARD_ROWS` goes 3 → 4, so `hit_test` stays a one-line division. Factor a pure `card_lines` helper (always three strings: title, basename, info) so the composition is unit-testable.

**Files:**
- Modify: `crates/board/src/ui.rs` (`CARD_ROWS`, `card_meta_line` idle age, rewrite `card`, add `card_lines`, fix `hit_test` test)
- Test: `crates/board/src/ui.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Agent` (`title`, `cwd`, `activity`, `socket_path`, `session_id`, `origin`), `Column`, `CardMeta` (`in_state`, `quiet`, `dormant_age`), helpers `basename`, `truncate`, `card_meta_line` — all already in `ui.rs`.
- Produces: `fn card_lines(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> [String; 3]` — the title, basename, and info line text, each truncated to `width`.

- [ ] **Step 1: Write the failing test**

Add a new module right after the existing `#[cfg(test)] mod tests { ... }` block in `crates/board/src/ui.rs`:

```rust
#[cfg(test)]
mod card_tests {
    use super::*;
    use crate::model::{Agent, Origin, State};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn agent(state: State, activity: Option<&str>) -> Agent {
        Agent {
            socket_path: PathBuf::from("/s/a.sock"),
            pid: 1,
            label: "pi".into(),
            session_id: Some("sid".into()),
            title: Some("fix the auth flow".into()),
            cwd: Some("/home/u/projects/corral".into()),
            state,
            origin: Origin::Live,
            resume: None,
            activity: activity.map(String::from),
        }
    }

    fn meta(in_state: &[(&str, &str)]) -> (HashMap<PathBuf, String>, HashMap<PathBuf, String>, HashMap<String, String>) {
        let in_state = in_state.iter().map(|(k, v)| (PathBuf::from(*k), v.to_string())).collect();
        (in_state, HashMap::new(), HashMap::new())
    }

    #[test]
    fn idle_info_line_shows_activity_and_time_idle() {
        let (i, q, d) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta { in_state: &i, quiet: &q, dormant_age: &d };
        let a = agent(State::Idle, Some("edit model.rs"));
        assert_eq!(
            card_lines(&a, Column::Idle, &m, 40),
            ["fix the auth flow", "corral", "edit model.rs · 5m"]
        );
    }

    #[test]
    fn idle_info_line_is_age_only_without_activity() {
        let (i, q, d) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta { in_state: &i, quiet: &q, dormant_age: &d };
        let a = agent(State::Idle, None);
        assert_eq!(card_lines(&a, Column::Idle, &m, 40)[2], "5m");
    }

    #[test]
    fn requires_action_info_line_is_question_then_age() {
        let (i, q, d) = meta(&[("/s/a.sock", "3m")]);
        let m = CardMeta { in_state: &i, quiet: &q, dormant_age: &d };
        let a = agent(State::RequiresAction, Some("Which branch?"));
        assert_eq!(
            card_lines(&a, Column::RequiresAction, &m, 40),
            ["fix the auth flow", "corral", "Which branch? · 3m"]
        );
    }

    #[test]
    fn basename_is_empty_string_when_cwd_missing() {
        let (i, q, d) = meta(&[]);
        let m = CardMeta { in_state: &i, quiet: &q, dormant_age: &d };
        let mut a = agent(State::Idle, None);
        a.cwd = None;
        assert_eq!(card_lines(&a, Column::Idle, &m, 40)[1], "");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/cornerman/projects/corral/.worktrees/ui-refresh && cargo test -p corral card_ 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'card_lines'`.

- [ ] **Step 3: Show idle its age in `card_meta_line`**

In `crates/board/src/ui.rs`, in `card_meta_line`, change the age match so Idle uses the same `in_state` map as Requires Action:

```rust
    let age = match col {
        Column::RequiresAction | Column::Idle => meta.in_state.get(&agent.socket_path),
        Column::Running => meta.quiet.get(&agent.socket_path),
        Column::Dormant => agent
            .session_id
            .as_deref()
            .and_then(|id| meta.dormant_age.get(id)),
    };
```

- [ ] **Step 4: Add `card_lines` and rewrite `card`**

Replace the whole `fn card(...)` (from `fn card(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> ListItem<'static> {` through its closing brace) with:

```rust
/// The three card lines, top to bottom (spacer excluded): the title, the cwd
/// basename (empty when unknown), and the info line (`card_meta_line`). Fixed
/// at three so cards keep a uniform height and `hit_test` can divide by
/// `CARD_ROWS`. Pure, unit-tested.
fn card_lines(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> [String; 3] {
    let name = agent.title.as_deref().unwrap_or("(unnamed)");
    let dir = agent.cwd.as_deref().map(basename).unwrap_or("");
    let info = card_meta_line(agent, col, meta);
    [truncate(name, width), truncate(dir, width), truncate(&info, width)]
}

fn card(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> ListItem<'static> {
    // Dormant cards are dimmed whole: they are context, not a call to act.
    let title_style = match agent.origin {
        Origin::Dormant => Style::default().add_modifier(Modifier::DIM),
        Origin::Live => Style::default(),
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let [name, dir, info] = card_lines(agent, col, meta, width);
    ListItem::new(vec![
        Line::from(Span::styled(name, title_style)),
        Line::from(Span::styled(dir, dim)),
        Line::from(Span::styled(info, dim)),
        Line::from(""), // blank spacer: air between cards
    ])
}
```

- [ ] **Step 5: Bump `CARD_ROWS` and fix the `hit_test` test**

Change the constant near the top of `crates/board/src/ui.rs` from `const CARD_ROWS: u16 = 3;` to `const CARD_ROWS: u16 = 4;`.

In the `hit_test_maps_clicks_to_selectable_indices` test, the second card now starts one row lower (cards are 4 rows tall). Update the comment and the second-card assertion:

```rust
        // Left column: HEAD_ROWS=2 (heading+rule), then 4-row cards.
        assert_eq!(hit_test(area, &b, 5, 2, no_scroll), Some(0));
        assert_eq!(hit_test(area, &b, 5, 6, no_scroll), Some(1));
```

(Replace the existing `assert_eq!(hit_test(area, &b, 5, 5, no_scroll), Some(1));` line and its preceding comment. Leave the other assertions unchanged — they still hold: row 12 is past the two 4-row cards, row 1 is the heading, etc.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd /home/cornerman/projects/corral/.worktrees/ui-refresh && cargo test -p corral 2>&1 | tail -20`
Expected: PASS (all tests, including the new `card_tests`).

- [ ] **Step 7: Lint and format**

Run: `cd /home/cornerman/projects/corral/.worktrees/ui-refresh && cargo fmt && just lint 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
cd /home/cornerman/projects/corral/.worktrees/ui-refresh && git add crates/board/src/ui.rs docs/superpowers && git commit -m "ui: relayout board cards (title/basename own lines, idle shows time-idle)"
```

---

### Task 2: Directory-grouped, state-colored picker

Rebuild `picker.rs` to hold the agents, group them by basename, fuzzy-match on path-or-title, and expose a Tab scope filter; move all styling into `ui.rs::render_picker`; simplify `Overlay::Jump` to carry only the `Picker`.

**Files:**
- Rewrite: `crates/board/src/picker.rs`
- Modify: `crates/board/src/ui.rs` (`render_picker`, make `basename` `pub(crate)`, add `badge`)
- Modify: `crates/board/src/main.rs` (`Overlay::Jump`, `picker_input`, `handle_overlay` Jump arm, render match, `open_jump`, remove `goto_label`, fix `shift_enter_in_picker_is_spawn` test)
- Test: `crates/board/src/picker.rs` (rewrite `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::model::{Agent, Origin, State}`, `crate::ui::basename`.
- Produces:
  - `pub enum Filter { All, Live, Dormant }` with `pub fn label(self) -> &'static str`.
  - `pub enum Row<'a> { Header(&'a str), Agent(&'a Agent) }`.
  - `pub struct Picker { pub query: String, pub filter: Filter, pub selected: usize, agents: Vec<Agent> }`.
  - `impl Picker`: `new(agents: Vec<Agent>) -> Self`, `rows(&self) -> Vec<Row>`, `selected_agent(&self) -> Option<&Agent>`, `filter_label(&self) -> &'static str`, `push(char)`, `backspace()`, `up()`, `down()`, `cycle_filter()`.

- [ ] **Step 1: Write the failing tests (rewrite `picker.rs` test module)**

Replace the entire `#[cfg(test)] mod tests { ... }` block at the end of `crates/board/src/picker.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::State;
    use std::path::PathBuf;

    fn agent(title: &str, cwd: &str, origin: Origin) -> Agent {
        Agent {
            socket_path: PathBuf::from(format!("/s/{title}.sock")),
            pid: 1,
            label: "pi".into(),
            session_id: Some(title.into()),
            title: Some(title.into()),
            cwd: Some(cwd.into()),
            state: State::Idle,
            origin,
            resume: None,
            activity: None,
        }
    }

    fn sample() -> Picker {
        Picker::new(vec![
            agent("alpha", "/home/u/projects/corral", Origin::Live),
            agent("beta", "/home/u/projects/nixos", Origin::Live),
            agent("gamma", "/home/u/projects/corral", Origin::Dormant),
        ])
    }

    #[test]
    fn groups_by_basename_in_first_appearance_order() {
        let p = sample();
        let rows = p.rows();
        // corral (alpha, gamma) appears before nixos (beta).
        assert!(matches!(rows[0], Row::Header("corral")));
        assert!(matches!(rows[1], Row::Agent(a) if a.title.as_deref() == Some("alpha")));
        assert!(matches!(rows[2], Row::Agent(a) if a.title.as_deref() == Some("gamma")));
        assert!(matches!(rows[3], Row::Header("nixos")));
        assert!(matches!(rows[4], Row::Agent(a) if a.title.as_deref() == Some("beta")));
    }

    #[test]
    fn query_matches_title_or_path_and_drops_empty_groups() {
        let mut p = sample();
        p.push('b');
        p.push('e');
        p.push('t'); // "bet" matches title beta only
        let rows = p.rows();
        assert_eq!(rows.len(), 2); // nixos header + beta
        assert!(matches!(rows[0], Row::Header("nixos")));
    }

    #[test]
    fn dir_query_keeps_all_agents_in_group() {
        let mut p = sample();
        for c in "corral".chars() {
            p.push(c);
        }
        let rows = p.rows();
        // both corral agents survive; nixos group is gone.
        assert!(matches!(rows[0], Row::Header("corral")));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn tab_filter_restricts_origin() {
        let mut p = sample();
        p.cycle_filter(); // All -> Live
        assert_eq!(p.filter, Filter::Live);
        let live: Vec<_> = p
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Agent(a) => a.title.clone(),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(live, vec!["alpha", "beta"]);
        p.cycle_filter(); // Live -> Dormant
        let dormant: Vec<_> = p
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Agent(a) => a.title.clone(),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(dormant, vec!["gamma"]);
    }

    #[test]
    fn down_skips_headers_and_selected_agent_maps_back() {
        let mut p = sample();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("alpha"));
        p.down();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("gamma"));
        p.down();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("beta"));
        p.down(); // clamps at end
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("beta"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/cornerman/projects/corral && cargo test -p corral 2>&1 | tail -25`
Expected: FAIL — compile errors (`Row`, `Filter`, `cycle_filter`, `selected_agent` not found; `Picker::new` signature mismatch). This is expected before Step 3.

- [ ] **Step 3: Rewrite the `picker.rs` module body**

Replace everything in `crates/board/src/picker.rs` above the `#[cfg(test)]` line with:

```rust
//! Fuzzy picker for the `/` jump overlay. Holds the board's agents, groups them
//! by their directory basename, and fuzzy-filters on path or title as the
//! operator types. A Tab scope filter narrows to Live or Dormant. Pure logic:
//! all glyph and color styling lives in `ui.rs`.

use crate::model::{Agent, Origin};
use crate::ui::basename;

/// Scope filter cycled by Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Live,
    Dormant,
}

impl Filter {
    fn accepts(self, origin: Origin) -> bool {
        match self {
            Filter::All => true,
            Filter::Live => origin == Origin::Live,
            Filter::Dormant => origin == Origin::Dormant,
        }
    }

    fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Live,
            Filter::Live => Filter::Dormant,
            Filter::Dormant => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Live => "live",
            Filter::Dormant => "dormant",
        }
    }
}

/// A visible row: a directory group header (a pure label) or an agent under it.
pub enum Row<'a> {
    Header(&'a str),
    Agent(&'a Agent),
}

pub struct Picker {
    pub query: String,
    pub filter: Filter,
    /// Index into the flat list of visible agent rows (headers excluded).
    pub selected: usize,
    /// Agents in board attention-priority order (from `board.selectable()`).
    agents: Vec<Agent>,
}

/// The directory group an agent belongs to: its cwd basename, or a fallback.
fn group_of(a: &Agent) -> &str {
    a.cwd.as_deref().map(basename).unwrap_or("(no dir)")
}

impl Picker {
    pub fn new(agents: Vec<Agent>) -> Self {
        Self {
            query: String::new(),
            filter: Filter::All,
            selected: 0,
            agents,
        }
    }

    /// Whether an agent survives the current filter and query. The query is a
    /// subsequence match against the title, the full path, or the basename, so
    /// typing a directory name keeps every agent under it.
    fn survives(&self, a: &Agent) -> bool {
        if !self.filter.accepts(a.origin) {
            return false;
        }
        let title = a.title.as_deref().unwrap_or("(unnamed)");
        let path = a.cwd.as_deref().unwrap_or("");
        fuzzy(&self.query, title) || fuzzy(&self.query, path) || fuzzy(&self.query, group_of(a))
    }

    /// Surviving agents grouped by basename, groups in first-appearance order,
    /// agents within a group in original (attention) order. Each entry is the
    /// group name and the indices into `self.agents`.
    fn grouped(&self) -> Vec<(&str, Vec<usize>)> {
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, a) in self.agents.iter().enumerate() {
            if !self.survives(a) {
                continue;
            }
            let g = group_of(a);
            match groups.iter_mut().find(|(k, _)| *k == g) {
                Some((_, v)) => v.push(i),
                None => groups.push((g, vec![i])),
            }
        }
        groups
    }

    /// The visible rows in order: each group's header followed by its agents.
    pub fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for (g, idxs) in self.grouped() {
            out.push(Row::Header(g));
            for i in idxs {
                out.push(Row::Agent(&self.agents[i]));
            }
        }
        out
    }

    /// Flat indices of the visible agent rows, in row order (headers excluded).
    fn agent_order(&self) -> Vec<usize> {
        self.grouped().into_iter().flat_map(|(_, v)| v).collect()
    }

    /// The agent under the current selection, if any.
    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agent_order().get(self.selected).map(|&i| &self.agents[i])
    }

    pub fn filter_label(&self) -> &'static str {
        self.filter.label()
    }

    pub fn push(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.selected = 0;
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        let n = self.agent_order().len();
        if n > 0 {
            self.selected = (self.selected + 1).min(n - 1);
        }
    }
}

/// Case-insensitive subsequence match: every query char appears in order. An
/// empty query matches everything.
fn fuzzy(query: &str, cand: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    for ch in cand.chars().flat_map(char::to_lowercase) {
        match q.peek() {
            Some(&qc) if qc == ch => {
                q.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    q.peek().is_none()
}
```

- [ ] **Step 4: Make `basename` shareable and rewrite `render_picker` in `ui.rs`**

In `crates/board/src/ui.rs`, change `fn basename(path: &str) -> &str {` to `pub(crate) fn basename(path: &str) -> &str {`.

Then replace the whole `pub fn render_picker(...)` function with:

```rust
/// Glyph, color, and title emphasis for an agent's state in the picker. Color
/// carries state here because the picker groups by directory, not by state.
fn badge(a: &Agent) -> (&'static str, Color, bool) {
    match a.origin {
        Origin::Dormant => ("·", Color::DarkGray, false),
        Origin::Live => match a.state {
            State::RequiresAction => ("●", Color::Red, true),
            State::Running => ("●", Color::Green, false),
            State::Idle => ("○", Color::White, false),
        },
    }
}

/// Draw the `/` jump picker: a query line, a scope label, then agents grouped
/// under dim directory headers, each row a colored state glyph + title + dim
/// activity. Enter goes, Shift+Enter spawns, Tab cycles the scope.
pub fn render_picker(frame: &mut Frame, picker: &Picker) {
    let area = centered(frame.area(), 70, 60);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            " jump [{}] — filter, tab scope, ⏎ go, ⇧⏎ new, esc cancel ",
            picker.filter_label()
        ))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", picker.query)), rows[0]);

    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut items: Vec<ListItem> = Vec::new();
    let mut highlight: Option<usize> = None;
    let mut agent_seen = 0usize;
    for row in picker.rows() {
        match row {
            Row::Header(dir) => {
                items.push(ListItem::new(Line::from(Span::styled(dir.to_string(), dim))));
            }
            Row::Agent(a) => {
                if agent_seen == picker.selected {
                    highlight = Some(items.len());
                }
                agent_seen += 1;
                let (glyph, color, bold) = badge(a);
                let mut title_style = Style::default().fg(color);
                if bold {
                    title_style = title_style.add_modifier(Modifier::BOLD);
                }
                if a.origin == Origin::Dormant {
                    title_style = title_style.add_modifier(Modifier::DIM);
                }
                let name = a.title.as_deref().unwrap_or("(unnamed)");
                let mut spans = vec![
                    Span::styled(format!("{glyph} "), Style::default().fg(color)),
                    Span::styled(name.to_string(), title_style),
                ];
                if let Some(act) = a.activity.as_deref() {
                    spans.push(Span::styled(format!("   {act}"), dim));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
        }
    }
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(highlight);
    frame.render_stateful_widget(list, rows[1], &mut state);
}
```

The above uses `Row`, `State`, and `Origin`. At the top of `ui.rs`, update the picker import line `use crate::picker::Picker;` to `use crate::picker::{Picker, Row};`, and confirm `State` and `Origin` are in scope on the `use crate::model::{...};` line (add them if missing).

- [ ] **Step 4b: Verify it compiles**

Run: `cd /home/cornerman/projects/corral && cargo build -p corral 2>&1 | tail -15`
Expected: `ui.rs` compiles (errors may remain in `main.rs` until Step 5).

- [ ] **Step 5: Wire `main.rs` to the new `Overlay::Jump(Picker)`**

In `crates/board/src/main.rs`:

1. Change the `Overlay` variant `Jump(Picker, Vec<model::Agent>)` to `Jump(Picker)`.

2. In `picker_input`, add a Tab arm (before the `_ =>` fallthrough):

```rust
        KeyCode::Tab => {
            p.cycle_filter();
            PickerInput::Continue
        }
```

3. Replace the `Overlay::Jump(p, targets) => match picker_input(p, key) { ... }` arm (the whole arm, through its closing brace before `Overlay::Compose`) with:

```rust
        Overlay::Jump(p) => match picker_input(p, key) {
            PickerInput::Continue => Some(ov),
            PickerInput::Cancel => None,
            PickerInput::Submit => {
                if let Some(a) = p.selected_agent() {
                    if let Err(e) = activate(a, focuser, launcher) {
                        *status = e;
                    }
                }
                None
            }
            PickerInput::SubmitSpawn => {
                if let Some(a) = p.selected_agent() {
                    let cwd = launch::default_cwd(Some(a));
                    if let Err(e) = launcher.spawn(&cwd, None) {
                        *status = format!("spawn: {e}");
                    }
                }
                None
            }
        },
```

4. Change the render match arm `Some(Overlay::Jump(p, _)) => ui::render_picker(f, p),` to `Some(Overlay::Jump(p)) => ui::render_picker(f, p),`.

5. Replace `open_jump` with:

```rust
/// Open the `/` jump picker over all agents (Enter goes, Shift+Enter spawns).
fn open_jump(board: &Board) -> Option<Overlay> {
    let agents: Vec<model::Agent> = board.selectable().into_iter().cloned().collect();
    (!agents.is_empty()).then(|| Overlay::Jump(Picker::new(agents)))
}
```

6. Delete the now-unused `goto_label` function (the `fn goto_label(agent: &model::Agent) -> String { ... }` block and its doc comment).

7. Fix the `shift_enter_in_picker_is_spawn` test: replace `let mut p = Picker::new(vec!["a".into()]);` with a real agent:

```rust
        let mut p = Picker::new(vec![model::Agent {
            socket_path: std::path::PathBuf::from("/s/a.sock"),
            pid: 1,
            label: "pi".into(),
            session_id: Some("a".into()),
            title: Some("a".into()),
            cwd: Some("/tmp".into()),
            state: model::State::Idle,
            origin: model::Origin::Live,
            resume: None,
            activity: None,
        }]);
```

- [ ] **Step 6: Run the full test suite**

Run: `cd /home/cornerman/projects/corral && cargo test -p corral 2>&1 | tail -25`
Expected: PASS — all picker tests and the existing suite.

- [ ] **Step 7: Lint and format**

Run: `cd /home/cornerman/projects/corral && cargo fmt && just lint 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
cd /home/cornerman/projects/corral && git add crates/board/src/picker.rs crates/board/src/ui.rs crates/board/src/main.rs && git commit -m "picker: directory-grouped, state-colored fuzzy list with scope filter"
```

---

## Notes for the implementer

- The board's `card` change (Task 1) and the picker (Task 2) are independent; either can land first.
- `board.selectable()` already returns agents in attention-priority order, so group first-appearance ordering surfaces urgent directories first for free.
- Do not thread `CardMeta` into the picker; picker rows intentionally show `activity` only (see Global Constraints).
- After both tasks, update `crates/board/AGENTS.md`'s `src/ui.rs` and `src/picker.rs` descriptions to match (card lines are adaptive; picker is directory-grouped with a scope filter). Fold that doc edit into the Task 2 commit or a small follow-up commit.
