//! Rendering. Two columns of cards, Needs You on the left (where attention
//! goes first) and Working on the right, plus a one-line status/help footer.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::model::{Agent, Board, State};

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
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▌");
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

    let help = "↑/↓ select   ⏎ focus   n new   q quit";
    let footer = if status.is_empty() {
        Line::from(help.dim())
    } else {
        Line::from(vec![Span::raw(status), Span::raw("   "), help.dim()])
    };
    frame.render_widget(Paragraph::new(footer), outer[1]);
}
