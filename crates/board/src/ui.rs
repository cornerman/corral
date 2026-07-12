//! Rendering. A clean board: columns separated by dim vertical rules, each with
//! a bold heading over an underline, then cards spaced for air. Three live
//! triage columns in attention priority (Requires Action, Idle, Running) and a
//! dim-gray Dormant column (resumable history). A status/help footer sits at
//! the bottom.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};

/// Left/right padding inside every column, so text never touches a separator.
const PAD: u16 = 1;
/// Rows above a column's cards: the heading and its underline rule.
const HEAD_ROWS: u16 = 2;
/// Rows one card spans: title, meta, and a blank spacer for air.
const CARD_ROWS: u16 = 3;
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::{Agent, Board, Column, Origin};
use crate::picker::Picker;

/// The heading shown above each column. Bound to the column identity (not a
/// parallel array), so it cannot drift from `Column::ALL`.
fn heading(column: Column) -> &'static str {
    match column {
        Column::RequiresAction => "Requires Action",
        Column::Idle => "Idle",
        Column::Running => "Running",
        Column::Dormant => "Dormant",
    }
}

/// The four equal column rects, reserving the bottom two rows (a blank spacer
/// above the footer). Shared by `render` and `hit_test` so their geometry
/// cannot drift. Columns are separated by gutters (layout spacing), not borders.
fn column_layout(area: Rect) -> [Rect; 4] {
    let content = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(area)[0];
    let cols = Layout::horizontal([Constraint::Ratio(1, 4); 4])
        .spacing(3)
        .split(content);
    [cols[0], cols[1], cols[2], cols[3]]
}

/// Map a mouse cell (col,row) to a selectable index, using the same layout as
/// `render`. Returns None for clicks on the heading/rule rows, gutters, empty
/// rows, or the footer. A column reserves `HEAD_ROWS` at the top; each card is
/// `CARD_ROWS` tall. `scroll` is each column's first-visible-item index (the
/// persisted `ListState` offset), so clicks in a scrolled column map right.
pub fn hit_test(
    area: Rect,
    board: &Board,
    col: u16,
    row: u16,
    scroll: [usize; 4],
) -> Option<usize> {
    let cols = column_layout(area);
    let counts = board.column_counts();
    let mut flat_start = 0;
    for (i, rect) in cols.iter().enumerate() {
        let cards_top = rect.y + HEAD_ROWS;
        let inside = col >= rect.x
            && col < rect.x + rect.width
            && row >= cards_top
            && row < rect.y + rect.height;
        if inside {
            let item = scroll[i] + ((row - cards_top) / CARD_ROWS) as usize;
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

/// Draw the `/` jump picker as a centered overlay: a query line above the
/// fuzzy-filtered agent list (Enter goes, Shift+Enter spawns).
pub fn render_picker(frame: &mut Frame, picker: &Picker) {
    let area = centered(frame.area(), 70, 60);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" jump — type to filter, ⏎ go, ⇧⏎ new, esc cancel ")
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

/// Draw the `m` message composer as a centered overlay: the target agent in
/// the title, the typed message on the input line.
pub fn render_compose(frame: &mut Frame, target: &str, buf: &str) {
    let area = centered(frame.area(), 70, 20);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" message {target} — ⏎ send, esc cancel "))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(format!("> {buf}")), inner);
}

/// An operator decision on a pending inter-agent message. Returned by both the
/// keys and `approval_hit_test` (a mouse click), so the two input paths share
/// one set of actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// The clickable buttons, left to right, with their chip labels. The `(key)`
/// hint doubles as the keyboard shortcut.
fn approval_buttons() -> [(ApprovalAction, &'static str); 3] {
    [
        (ApprovalAction::AllowOnce, " allow once (⏎) "),
        (ApprovalAction::AllowAlways, " allow always (a) "),
        (ApprovalAction::Deny, " deny (esc) "),
    ]
}

/// Blank cells between adjacent button chips.
const APPROVAL_GAP: u16 = 2;

/// The row and x-ranges of the approval buttons, from the same geometry
/// `render_approval` draws, so clicks map to the chips exactly.
fn approval_footer(area: Rect) -> Rect {
    let overlay = centered(area, 70, 40);
    let inner = Block::default().borders(Borders::ALL).inner(overlay);
    Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner)[1]
}

/// Draw the inter-agent message approval dialog: who wants to message whom, the
/// message body, and the operator's choices (clickable, or by key).
pub fn render_approval(frame: &mut Frame, msg: &crate::mailbox::Message, scroll: u16) {
    let area = centered(frame.area(), 70, 40);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" inter-agent message — approve? ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Reserve the bottom row for the action buttons so a long message can never
    // push them out of view.
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let body = vec![
        Line::from(vec![
            Span::raw("from: "),
            Span::styled(msg.from_cwd.clone(), bold),
        ]),
        Line::from(vec![
            Span::raw("to:   "),
            Span::styled(msg.target_label(), bold),
        ]),
        Line::raw(""),
        Line::raw(msg.message.clone()),
    ];
    // Scrollable: a long message is read by scrolling (Up/Down / wheel), not
    // clipped. The action row below stays fixed.
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0)),
        rows[0],
    );

    // Highlighted chips, click or press the key. Gaps match APPROVAL_GAP so
    // `approval_hit_test` lines up with what is drawn.
    let chip = Style::default().add_modifier(Modifier::REVERSED);
    let mut spans = Vec::new();
    for (i, (_, label)) in approval_buttons().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(APPROVAL_GAP as usize)));
        }
        spans.push(Span::styled(*label, chip));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

/// Map a mouse click to an approval button, using the same geometry as
/// `render_approval`. `None` for clicks off the buttons.
pub fn approval_hit_test(area: Rect, col: u16, row: u16) -> Option<ApprovalAction> {
    let footer = approval_footer(area);
    if row != footer.y {
        return None;
    }
    let mut x = footer.x;
    for (action, label) in approval_buttons() {
        let w = label.chars().count() as u16;
        if col >= x && col < x + w {
            return Some(action);
        }
        x += w + APPROVAL_GAP;
    }
    None
}

/// A footer action the operator can click (the key hints double as buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    Go,
    New,
    Jump,
    Msg,
    Delete,
    Quit,
}

/// Blank cells between footer entries.
const FOOTER_GAP: u16 = 3;

/// Footer entries left to right: label, and the action a click triggers
/// (`None` for the non-clickable movement hint).
fn footer_items() -> [(Option<FooterAction>, &'static str); 7] {
    [
        (None, "↑↓←→ move"),
        (Some(FooterAction::Go), "⏎ go"),
        (Some(FooterAction::New), "⇧⏎ new"),
        (Some(FooterAction::Jump), "/ jump"),
        (Some(FooterAction::Msg), "m msg"),
        (Some(FooterAction::Delete), "d delete"),
        (Some(FooterAction::Quit), "q quit"),
    ]
}

/// The footer row: the bottom-most row, inset by PAD to align with the columns
/// (the row above is a blank spacer, or the status line). Shared by `render`
/// and `footer_hit_test` so their geometry cannot drift.
fn footer_rect(area: Rect) -> Rect {
    let bottom = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(area)[1];
    Rect {
        x: bottom.x + PAD,
        y: bottom.y + 1,
        width: bottom.width.saturating_sub(2 * PAD),
        height: 1,
    }
}

/// Map a click to a footer action, using the same geometry `render` draws.
pub fn footer_hit_test(area: Rect, col: u16, row: u16) -> Option<FooterAction> {
    let f = footer_rect(area);
    if row != f.y {
        return None;
    }
    let mut x = f.x;
    for (action, label) in footer_items() {
        let w = label.chars().count() as u16;
        if col >= x && col < x + w {
            return action;
        }
        x += w + FOOTER_GAP;
    }
    None
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Truncate to `width` columns, adding an ellipsis when it does not fit, so a
/// long title is cut cleanly rather than hard-clipped mid-word by the renderer.
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    match width {
        0 => String::new(),
        _ => s.chars().take(width - 1).collect::<String>() + "…",
    }
}

/// Compact age like `8s`, `5m`, `2h`, `3d` for time-in-state display.
pub fn age_label(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// Label for the compose target and the `f` focus picker: the title and the
/// cwd's last path segment.
pub fn focus_label(agent: &Agent) -> String {
    let title = agent.title.as_deref().unwrap_or("(unnamed)");
    let cwd = agent.cwd.as_deref().unwrap_or("?");
    format!("{title} · {}", basename(cwd))
}

fn card(agent: &Agent, age: Option<&str>, width: usize) -> ListItem<'static> {
    let title = truncate(agent.title.as_deref().unwrap_or("(unnamed)"), width);
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
    let meta = truncate(
        &match age {
            Some(a) => format!("{} · {} · {}", agent.label, cwd, a),
            None => format!("{} · {}", agent.label, cwd),
        },
        width,
    );
    ListItem::new(vec![
        Line::from(Span::styled(title, title_style)),
        Line::from(Span::styled(
            meta,
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(""), // blank spacer: air between cards
    ])
}

fn column(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    col: Column,
    agents: &[&Agent],
    selected_row: Option<usize>,
    state: &mut ListState,
    ages: &HashMap<PathBuf, String>,
) {
    let secondary = matches!(col, Column::Dormant);
    let dim_gray = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    // Pad the column so nothing touches the separators.
    let inner = Rect {
        x: area.x + PAD,
        y: area.y,
        width: area.width.saturating_sub(2 * PAD),
        height: area.height,
    };
    let rows = Layout::vertical([
        Constraint::Length(1), // heading
        Constraint::Length(1), // underline rule
        Constraint::Min(0),    // cards
    ])
    .split(inner);

    // Heading: bold uppercase name + dim count; the dormant column is dim gray.
    let (name_style, count_style) = if secondary {
        (dim_gray, dim_gray)
    } else {
        (
            Style::default().add_modifier(Modifier::BOLD),
            Style::default().add_modifier(Modifier::DIM),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(heading(col).to_uppercase(), name_style),
            Span::styled(format!("  {}", agents.len()), count_style),
        ])),
        rows[0],
    );

    // Underline rule anchoring the heading.
    let rule_style = if secondary {
        dim_gray
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(rows[1].width as usize),
            rule_style,
        ))),
        rows[1],
    );

    // Cards. The persisted state scrolls long columns and its offset feeds
    // `hit_test`; the left bar marks the selection.
    // Card text width, minus the 1-col selection bar the list reserves.
    let card_w = rows[2].width.saturating_sub(1) as usize;
    let items: Vec<ListItem> = agents
        .iter()
        .map(|a| card(a, ages.get(&a.socket_path).map(String::as_str), card_w))
        .collect();
    let list = List::new(items)
        .highlight_symbol("▍")
        // Always reserve the bar column so selecting never shifts the text
        // (each column is its own list; the default only reserves it when that
        // list has the selection).
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    state.select(selected_row);
    frame.render_stateful_widget(list, rows[2], state);
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
    let footer_area = footer_rect(frame.area());
    let cols = column_layout(frame.area());

    // One flat column per `Column::ALL` entry (matching navigation and
    // hit-testing). `start` accumulates the flat selection offset so the
    // highlighted card lands in the right column.
    let mut start = 0;
    for (i, col) in Column::ALL.into_iter().enumerate() {
        let agents = board.column(col);
        let sel = selected.checked_sub(start).filter(|&r| r < agents.len());
        column(frame, cols[i], col, &agents, sel, &mut states[i], ages);
        start += agents.len();
    }

    // Dim vertical rules between columns, in the middle of each gutter.
    for pair in cols.windows(2) {
        let sep = Rect {
            x: pair[0].x + pair[0].width + 1,
            y: pair[0].y,
            width: 1,
            height: pair[0].height,
        };
        let bar = Text::from(vec![Line::from("│"); sep.height as usize]);
        frame.render_widget(
            Paragraph::new(bar).style(Style::default().add_modifier(Modifier::DIM)),
            sep,
        );
    }

    // Status (if any) on the spacer row above; the footer row holds the
    // clickable key hints at a fixed position (so clicks map, and a status
    // message never shifts them).
    if !status.is_empty() {
        let spacer = Rect {
            y: footer_area.y.saturating_sub(1),
            ..footer_area
        };
        frame.render_widget(Paragraph::new(Line::from(status.dim())), spacer);
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = Vec::new();
    for (i, (_, label)) in footer_items().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(FOOTER_GAP as usize)));
        }
        spans.push(Span::styled(*label, dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{State, Update};
    use std::path::PathBuf;

    #[test]
    fn age_label_scales_units() {
        assert_eq!(age_label(Duration::from_secs(8)), "8s");
        assert_eq!(age_label(Duration::from_secs(5 * 60)), "5m");
        assert_eq!(age_label(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(age_label(Duration::from_secs(3 * 86400)), "3d");
    }

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

        // Left column: HEAD_ROWS=2 (heading+rule), then 3-row cards.
        assert_eq!(hit_test(area, &b, 5, 2, no_scroll), Some(0));
        assert_eq!(hit_test(area, &b, 5, 5, no_scroll), Some(1));
        assert_eq!(hit_test(area, &b, 5, 1, no_scroll), None); // heading/rule row
        assert_eq!(hit_test(area, &b, 5, 12, no_scroll), None); // past the two cards

        // Second column (Idle) is empty.
        assert_eq!(hit_test(area, &b, 30, 2, no_scroll), None);

        // Third live column (Running): index continues after the two left cards.
        assert_eq!(hit_test(area, &b, 60, 2, no_scroll), Some(2));

        // Footer row is outside every column.
        assert_eq!(hit_test(area, &b, 5, 19, no_scroll), None);

        // A scrolled left column maps the first visible row to the offset item.
        assert_eq!(hit_test(area, &b, 5, 2, [1, 0, 0, 0]), Some(1));
    }
}
