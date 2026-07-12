//! Rendering. Three columns of cards in attention priority: Requires Action,
//! Idle, Running (left to right), plus a one-line status/help footer.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::model::{Agent, Board, State};

/// Map a mouse cell (col,row) to a selectable index, using the same layout as
/// `render`. Returns None for clicks on borders, headings, empty rows, or the
/// footer. Cards are two rows tall; the block's top border occupies one row.
pub fn hit_test(area: Rect, board: &Board, col: u16, row: u16) -> Option<usize> {
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let cols = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(33),
        Constraint::Percentage(33),
    ])
    .split(outer[0]);
    let counts = [
        board.in_state(State::RequiresAction).len(),
        board.in_state(State::Idle).len(),
        board.in_state(State::Running).len(),
    ];
    let mut offset = 0;
    for (i, rect) in cols.iter().enumerate() {
        let inside = col >= rect.x
            && col < rect.x + rect.width
            && row > rect.y
            && row < rect.y + rect.height;
        if inside {
            let item = ((row - rect.y - 1) / 2) as usize;
            return (item < counts[i]).then_some(offset + item);
        }
        offset += counts[i];
    }
    None
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn card(agent: &Agent) -> ListItem<'static> {
    let title = agent.title.clone().unwrap_or_else(|| "(unnamed)".into());
    let cwd = agent
        .cwd
        .as_deref()
        .map(basename)
        .unwrap_or("?")
        .to_string();
    ListItem::new(vec![
        Line::from(Span::raw(title)),
        Line::from(Span::styled(
            format!("  {} · {}", agent.label, cwd),
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
) {
    let items: Vec<ListItem> = agents.iter().map(|a| card(a)).collect();
    let title = format!(" {heading} ({}) ", agents.len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(selected_row);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Render the whole board. `selected` indexes `board.selectable()`
/// (RequiresAction, then Idle, then Running: attention priority).
pub fn render(frame: &mut Frame, board: &Board, selected: usize, status: &str) {
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());
    let cols = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(33),
        Constraint::Percentage(33),
    ])
    .split(outer[0]);

    let action = board.in_state(State::RequiresAction);
    let idle = board.in_state(State::Idle);
    let running = board.in_state(State::Running);

    // Map the flat selection index (same order as board.selectable()) onto the
    // three columns.
    let sel_in = |start: usize, len: usize| selected.checked_sub(start).filter(|&r| r < len);
    let action_sel = sel_in(0, action.len());
    let idle_sel = sel_in(action.len(), idle.len());
    let running_sel = sel_in(action.len() + idle.len(), running.len());

    column(frame, cols[0], "Requires Action", &action, action_sel);
    column(frame, cols[1], "Idle", &idle, idle_sel);
    column(frame, cols[2], "Running", &running, running_sel);

    let help = "↑/↓ move   ←/→ column   ⏎/click focus   n new   q quit";
    let footer = if status.is_empty() {
        Line::from(help.dim())
    } else {
        Line::from(vec![Span::raw(status), Span::raw("   "), help.dim()])
    };
    frame.render_widget(Paragraph::new(footer), outer[1]);
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

        // Left column (x<34): cards are two rows, first content row is 1.
        assert_eq!(hit_test(area, &b, 5, 1), Some(0));
        assert_eq!(hit_test(area, &b, 5, 3), Some(1));
        assert_eq!(hit_test(area, &b, 5, 0), None); // top border
        assert_eq!(hit_test(area, &b, 5, 9), None); // past the two cards

        // Middle column (Idle) is empty.
        assert_eq!(hit_test(area, &b, 45, 1), None);

        // Right column (Running): index continues after the two left cards.
        assert_eq!(hit_test(area, &b, 80, 1), Some(2));

        // Footer row is outside every column.
        assert_eq!(hit_test(area, &b, 5, 19), None);
    }
}
