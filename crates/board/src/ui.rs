//! Rendering. Three columns of cards in attention priority: Requires Action,
//! Idle, Running (left to right), plus a one-line status/help footer.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::{Agent, Board, Origin, State};
use crate::picker::Picker;

/// The four column rects (Requires Action, Idle, Running, Dormant), reserving
/// the bottom row for the footer. Shared by `render` and `hit_test` so their
/// geometry can never drift apart.
fn column_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(outer[0])
}

/// Map a mouse cell (col,row) to a selectable index, using the same layout as
/// `render`. Returns None for clicks on borders, headings, empty rows, or the
/// footer. Cards are two rows tall; the block's top border occupies one row.
/// `scroll` is each column's first-visible-item index (the persisted
/// `ListState` offset from the last render), so clicks in a scrolled column
/// map to the right agent.
pub fn hit_test(
    area: Rect,
    board: &Board,
    col: u16,
    row: u16,
    scroll: [usize; 4],
) -> Option<usize> {
    let cols = column_layout(area);
    let counts = [
        board.in_state(State::RequiresAction).len(),
        board.in_state(State::Idle).len(),
        board.in_state(State::Running).len(),
        board.dormant().len(),
    ];
    let mut flat_start = 0;
    for (i, rect) in cols.iter().enumerate() {
        let inside = col >= rect.x
            && col < rect.x + rect.width
            && row > rect.y
            && row < rect.y + rect.height;
        if inside {
            let item = scroll[i] + ((row - rect.y - 1) / 2) as usize;
            return (item < counts[i]).then_some(flat_start + item);
        }
        flat_start += counts[i];
    }
    None
}

/// A Rect centered in `area` at the given width/height percentages.
fn centered(area: Rect, pw: u16, ph: u16) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - ph) / 2),
        Constraint::Percentage(ph),
        Constraint::Percentage((100 - ph) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - pw) / 2),
        Constraint::Percentage(pw),
        Constraint::Percentage((100 - pw) / 2),
    ])
    .split(v[1])[1]
}

/// Draw the `c` directory picker as a centered overlay: a query line above
/// the fuzzy-filtered candidate list.
pub fn render_picker(frame: &mut Frame, picker: &Picker, verb: &str) {
    let area = centered(frame.area(), 70, 60);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {verb} — type to filter, ⏎ select, esc cancel "))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", picker.query)), rows[0]);

    let matches = picker.matches();
    let items: Vec<ListItem> = matches.iter().map(|d| ListItem::new(*d)).collect();
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select((!matches.is_empty()).then_some(picker.selected));
    frame.render_stateful_widget(list, rows[1], &mut state);
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn card(agent: &Agent, age: Option<&str>) -> ListItem<'static> {
    let title = agent.title.clone().unwrap_or_else(|| "(unnamed)".into());
    let cwd = agent
        .cwd
        .as_deref()
        .map(basename)
        .unwrap_or("?")
        .to_string();
    // Dormant cards are dimmed whole: they are context, not a call to act.
    let title_style = match agent.origin {
        Origin::Dormant => Style::default().add_modifier(Modifier::DIM),
        Origin::Live => Style::default(),
    };
    // Time in the current state sharpens triage ("blocked for 8m"). Only live
    // agents have a running timer.
    let meta = match age {
        Some(a) => format!("  {} · {} · {}", agent.label, cwd, a),
        None => format!("  {} · {}", agent.label, cwd),
    };
    ListItem::new(vec![
        Line::from(Span::styled(title, title_style)),
        Line::from(Span::styled(
            meta,
            Style::default().add_modifier(Modifier::DIM),
        )),
    ])
}

fn column(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    heading: &str,
    agents: &[&Agent],
    selected_row: Option<usize>,
    state: &mut ListState,
    ages: &HashMap<PathBuf, String>,
) {
    let items: Vec<ListItem> = agents
        .iter()
        .map(|a| card(a, ages.get(&a.socket_path).map(String::as_str)))
        .collect();
    let title = format!(" {heading} ({}) ", agents.len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    // The state persists across frames, so ratatui keeps the selected card in
    // view (scrolling long columns) and its offset feeds `hit_test`.
    state.select(selected_row);
    frame.render_stateful_widget(list, area, state);
}

/// Render the whole board. `selected` indexes `board.selectable()`
/// (RequiresAction, then Idle, then Running: attention priority).
pub fn render(
    frame: &mut Frame,
    board: &Board,
    selected: usize,
    status: &str,
    states: &mut [ListState; 4],
    ages: &HashMap<PathBuf, String>,
) {
    let footer_area =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area())[1];
    let cols = column_layout(frame.area());

    let action = board.in_state(State::RequiresAction);
    let idle = board.in_state(State::Idle);
    let running = board.in_state(State::Running);
    let dormant = board.dormant();

    // Map the flat selection index (same order as board.selectable()) onto the
    // four columns.
    let sel_in = |start: usize, len: usize| selected.checked_sub(start).filter(|&r| r < len);
    let action_sel = sel_in(0, action.len());
    let idle_sel = sel_in(action.len(), idle.len());
    let running_sel = sel_in(action.len() + idle.len(), running.len());
    let dormant_sel = sel_in(action.len() + idle.len() + running.len(), dormant.len());

    let [s0, s1, s2, s3] = states;
    column(
        frame,
        cols[0],
        "Requires Action",
        &action,
        action_sel,
        s0,
        ages,
    );
    column(frame, cols[1], "Idle", &idle, idle_sel, s1, ages);
    column(frame, cols[2], "Running", &running, running_sel, s2, ages);
    column(frame, cols[3], "Dormant", &dormant, dormant_sel, s3, ages);

    let help =
        "↑/↓ move   ←/→ col   ⏎ focus/resume   f find   n new   c create   d dismiss   q quit";
    let footer = if status.is_empty() {
        Line::from(help.dim())
    } else {
        Line::from(vec![Span::raw(status), Span::raw("   "), help.dim()])
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Update;
    use std::path::PathBuf;

    fn upsert(board: &mut Board, path: &str, state: State) {
        board.apply(Update::Upsert(Agent {
            socket_path: PathBuf::from(path),
            pid: 1,
            label: "pi".into(),
            session_id: None,
            title: None,
            cwd: None,
            state,
            origin: crate::model::Origin::Live,
            resume: None,
        }));
    }

    #[test]
    fn hit_test_maps_clicks_to_selectable_indices() {
        // Two Requires Action (left column), one Running (right column).
        let mut b = Board::default();
        upsert(&mut b, "/s/a.sock", State::RequiresAction);
        upsert(&mut b, "/s/b.sock", State::RequiresAction);
        upsert(&mut b, "/s/c.sock", State::Running);
        let area = Rect::new(0, 0, 100, 20);
        let no_scroll = [0usize; 4];

        // Left column (x<25): cards are two rows, first content row is 1.
        assert_eq!(hit_test(area, &b, 5, 1, no_scroll), Some(0));
        assert_eq!(hit_test(area, &b, 5, 3, no_scroll), Some(1));
        assert_eq!(hit_test(area, &b, 5, 0, no_scroll), None); // top border
        assert_eq!(hit_test(area, &b, 5, 9, no_scroll), None); // past the two cards

        // Second column (Idle) is empty.
        assert_eq!(hit_test(area, &b, 30, 1, no_scroll), None);

        // Third column (Running), of four equal columns: index continues after
        // the two left cards.
        assert_eq!(hit_test(area, &b, 60, 1, no_scroll), Some(2));

        // Footer row is outside every column.
        assert_eq!(hit_test(area, &b, 5, 19, no_scroll), None);

        // A scrolled left column maps the first visible row to the offset item.
        assert_eq!(hit_test(area, &b, 5, 1, [1, 0, 0, 0]), Some(1));
    }
}
