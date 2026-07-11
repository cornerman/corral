//! Rendering. Two columns of cards, Needs You on the left (where attention
//! goes first) and Working on the right, plus a one-line status/help footer.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::model::{Agent, Board};

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
/// (Needs You first, then Working).
pub fn render(frame: &mut Frame, board: &Board, selected: usize, status: &str) {
    let outer = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());
    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[0]);

    let needs = board.needs_you();
    let working = board.working();

    // Map the flat selection index onto the two columns.
    let (needs_sel, working_sel) = if selected < needs.len() {
        (Some(selected), None)
    } else {
        (None, Some(selected - needs.len()))
    };

    column(frame, cols[0], "Needs You", &needs, needs_sel);
    column(frame, cols[1], "Working", &working, working_sel);

    let help = "↑/↓ select   ⏎ focus   n new   q quit";
    let footer = if status.is_empty() {
        Line::from(help.dim())
    } else {
        Line::from(vec![Span::raw(status), Span::raw("   "), help.dim()])
    };
    frame.render_widget(Paragraph::new(footer), outer[1]);
}
