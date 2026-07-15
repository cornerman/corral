//! Rendering. A clean board: columns separated by dim vertical rules, each with
//! a bold heading over an underline, then cards spaced for air. Three live
//! triage columns in attention priority (Requires Action, Idle, Running) and a
//! dim-gray Dormant column (resumable history). A status/help footer sits at
//! the bottom.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthStr;

/// Left/right padding inside every column, so text never touches a separator.
const PAD: u16 = 1;
/// Rows above a column's cards: the heading and its underline rule.
const HEAD_ROWS: u16 = 2;
/// Rows one card spans: title/age, the pill+badge+activity row, and a blank
/// spacer for air.
const CARD_ROWS: u16 = 3;
/// Rows reserved at the top for the filter: the input line, its underline, and
/// breathing room beneath before the columns.
const FILTER_ROWS: u16 = 4;
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph,
};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use corral_core::model::{Agent, Board, Column, Origin};
use corral_core::palette::{basename, color_index};

/// The four equal column rects, reserving the bottom two rows (a blank spacer
/// above the footer). Shared by `render` and `hit_test` so their geometry
/// cannot drift. Columns are separated by gutters (layout spacing), not borders.
fn column_layout(area: Rect) -> [Rect; 4] {
    // Top: the filter box; middle: the columns; bottom two: spacer + footer.
    let content = Layout::vertical([
        Constraint::Length(FILTER_ROWS),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area)[1];
    let cols = Layout::horizontal([Constraint::Ratio(1, 4); 4])
        .spacing(3)
        .split(content);
    [cols[0], cols[1], cols[2], cols[3]]
}

/// The reserved top strip that holds the filter box (same split as
/// `column_layout`, so their geometry cannot drift).
fn filter_area(area: Rect) -> Rect {
    Layout::vertical([
        Constraint::Length(FILTER_ROWS),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area)[0]
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

/// Draw the inline content filter on the row above the footer: `/` plus the
/// query, with a block cursor while editing. Draws nothing when idle and empty,
/// so the status line shows through.
pub fn render_filter(frame: &mut Frame, filter: &str, filtering: bool) {
    let area = filter_area(frame.area());
    // The corral wordmark, top-left and faint, mirroring the GUI header.
    let mark = Rect {
        x: area.x + PAD,
        y: area.y,
        width: area.width.saturating_sub(PAD).min(6),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "corral",
            Style::default().add_modifier(Modifier::DIM),
        ))),
        mark,
    );
    // A centered field, prominent like a launcher's input line.
    let w = ((area.width as u32 * 7 / 10) as u16).clamp(30.min(area.width), area.width);
    let x = area.x + area.width.saturating_sub(w) / 2;
    // The field is just the input row + its underline; the remaining reserved
    // rows are empty space under it.
    let field = Rect {
        x,
        y: area.y + 1, // a blank row of padding above the filter
        width: w,
        height: 2,
    };
    // Bright border while editing, dim otherwise; the box is always shown.
    let border = if filtering {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    // Just an underline, not a full box (flat, like the GUI's filter).
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(border);
    let inner = block.inner(field);
    frame.render_widget(block, field);
    let line = if filter.is_empty() && !filtering {
        Line::from(Span::styled(
            "type to filter…",
            Style::default().add_modifier(Modifier::DIM),
        ))
    } else {
        let cursor = if filtering { "\u{2588}" } else { "" };
        Line::from(Span::styled(
            format!("{filter}{cursor}"),
            Style::default().add_modifier(Modifier::BOLD),
        ))
    };
    frame.render_widget(Paragraph::new(line), inner);
}

/// Draw the `m` message composer as a centered overlay: the target agent in
/// the title, the typed message on the input line.
pub fn render_compose(frame: &mut Frame, target: &str, buf: &str) {
    let area = centered(frame.area(), 70, 20);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" message {target} — enter send, esc cancel "))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(format!("> {buf}")), inner);
}

/// A footer action the operator can click (the key hints double as buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    Go,
    New,
    Jump,
    Msg,
    Delete,
    Toggle,
    Quit,
}

/// Blank cells between footer entries.
const FOOTER_GAP: u16 = 2;

/// Footer entries left to right: the key(s), the label, and the action a click
/// triggers (`None` for the non-clickable movement hint). Keys are spelled in
/// plain ASCII (no `⏎`/`⇧`/arrow glyphs) so they render in every terminal
/// font; the keycap styling in `footer_layout` supplies the visual polish.
fn footer_items() -> [(Option<FooterAction>, &'static str, &'static str); 8] {
    [
        (None, "arrows", "move"),
        (Some(FooterAction::Go), "enter", "go"),
        (Some(FooterAction::New), "shift+enter", "new"),
        (Some(FooterAction::Jump), "/", "filter"),
        (Some(FooterAction::Msg), "m", "msg"),
        (Some(FooterAction::Delete), "d", "delete"),
        (Some(FooterAction::Toggle), "h", "hide/show"),
        (Some(FooterAction::Quit), "q", "quit"),
    ]
}

/// Build the footer's styled spans and, alongside, each clickable item's
/// (action, start column relative to the footer left edge, width). Render and
/// hit-testing both consume this, so their geometry cannot drift. Each item is
/// a keycap (` key ` reversed like a physical key) followed by a dim label.
fn footer_layout() -> (Vec<Span<'static>>, Vec<(FooterAction, u16, u16)>) {
    let keycap = Style::default().fg(Color::Black).bg(Color::Gray);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = 0u16;
    for (i, (action, key, desc)) in footer_items().into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(FOOTER_GAP as usize)));
            x += FOOTER_GAP;
        }
        let cap = format!(" {key} ");
        let label = format!(" {desc}");
        let w = (cap.chars().count() + label.chars().count()) as u16;
        if let Some(action) = action {
            hits.push((action, x, w));
        }
        spans.push(Span::styled(cap, keycap));
        spans.push(Span::styled(label, dim));
        x += w;
    }
    (spans, hits)
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
    let (_, hits) = footer_layout();
    for (action, x, w) in hits {
        if col >= f.x + x && col < f.x + x + w {
            return Some(action);
        }
    }
    None
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

/// Accent colors for cwd pills, indexed by a stable hash of the full cwd path
/// (`core::palette::color_index`) so a directory always reads in the same
/// color across the board — the eye groups cards by color at a glance. These
/// are terminal ANSI accents (cards carry no other color), chosen to stay
/// distinct on both dark and light backgrounds.
const CWD_PALETTE: [Color; 8] = [
    Color::Blue,
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Red,
    Color::LightBlue,
    Color::LightMagenta,
];

/// The colored basename pill for a cwd: ` <leaf> ` on a hash-picked background
/// with dark text for contrast. `dim` fades it for dormant cards. Returns None
/// when the cwd is unknown (no pill drawn).
fn cwd_pill(cwd: Option<&str>, dim: bool) -> Option<Span<'static>> {
    let cwd = cwd?;
    let color = CWD_PALETTE[color_index(cwd, CWD_PALETTE.len())];
    let mut style = Style::default().bg(color).fg(Color::Black);
    if dim {
        style = style.add_modifier(Modifier::DIM);
    }
    Some(Span::styled(format!(" {} ", basename(cwd)), style))
}

/// A muted gray pill (` <text> ` padded like the cwd pill, but a fixed
/// neutral fill rather than a hashed color), used for the kind badge and the
/// `hidden` badge so both read as tags distinct from the plain activity text.
/// `dim` fades it for dormant cards.
fn tag_pill(text: &str, dim: bool) -> Span<'static> {
    let mut style = Style::default().bg(Color::DarkGray).fg(Color::White);
    if dim {
        style = style.add_modifier(Modifier::DIM);
    }
    Span::styled(format!(" {text} "), style)
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
/// The icon shown after the kind badge for a live hidden agent, empty
/// otherwise: the "dotted line face" emoji, Unicode's own glyph for
/// hidden/invisible/disappear. Kept pure so it is unit-tested without a
/// terminal. (The GUI shows the same glyph with a `hidden` hover tooltip; the
/// terminal has no hover, so the icon stands alone.)
pub fn hidden_badge(agent: &Agent) -> &'static str {
    if agent.origin == Origin::Live && agent.hidden {
        "\u{1FAE5}"
    } else {
        ""
    }
}

pub fn focus_label(agent: &Agent) -> String {
    let title = agent.title.as_deref().unwrap_or("(unnamed)");
    let cwd = agent.cwd.as_deref().unwrap_or("?");
    format!("{title} · {}", basename(cwd))
}

/// Per-card timing inputs the columns format into a meta line. `in_state` and
/// `quiet` are keyed by socket path (live agents); `dormant_age` by session id.
/// The meta line means something different per column, because the triage
/// question differs: time-blocked when it needs you, time-since-activity while
/// running, the last action when idle, and record age when dormant.
pub struct CardMeta<'a> {
    /// Time in the current state, by socket path (for Requires Action).
    pub in_state: &'a HashMap<PathBuf, String>,
    /// Time since the last tool activity, by socket path (for Running).
    pub quiet: &'a HashMap<PathBuf, String>,
    /// Age of the session record, by session id (for Dormant).
    pub dormant_age: &'a HashMap<String, String>,
}

/// The column-specific age shown at the card's top-right. It differs per column
/// because the triage question does: time blocked when it needs you, time since
/// the last activity while running, time idle (how long it has been waiting for
/// you) when idle, and record age when dormant.
fn card_age(agent: &Agent, col: Column, meta: &CardMeta) -> Option<String> {
    match col {
        // Idle reuses in_state (time since entering the state) to show how long
        // the agent has been waiting for the user.
        Column::RequiresAction | Column::Idle => meta.in_state.get(&agent.socket_path),
        Column::Running => meta.quiet.get(&agent.socket_path),
        Column::Dormant => agent
            .session_id
            .as_deref()
            .and_then(|id| meta.dormant_age.get(id)),
    }
    .cloned()
}

/// The title line: the session name on the left, the column-specific age dim
/// and right-aligned in the card width. Age moved here (off the info line) to
/// keep the second row free for the cwd pill, badge, and activity.
fn title_line(
    name: &str,
    age: Option<&str>,
    width: usize,
    title_style: Style,
    age_style: Style,
) -> Line<'static> {
    let age = age.unwrap_or("");
    let age_w = age.chars().count();
    // Reserve the age plus one separating space; the name takes the rest.
    let name = truncate(name, width.saturating_sub(age_w + 1));
    let pad = width.saturating_sub(name.chars().count() + age_w);
    Line::from(vec![
        Span::styled(name, title_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(age.to_string(), age_style),
    ])
}

fn card(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> ListItem<'static> {
    let dormant = matches!(agent.origin, Origin::Dormant);
    // Dormant cards are dimmed whole: they are context, not a call to act.
    let title_style = if dormant {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let name = agent.title.as_deref().unwrap_or("(unnamed)");
    let age = card_age(agent, col, meta);

    // Second row: colored basename pill, kind badge, then the activity hint
    // filling the rest. Widths are counted so the activity truncates to fit.
    let mut row2 = Vec::new();
    let mut used = 0usize;
    if let Some(pill) = cwd_pill(agent.cwd.as_deref(), dormant) {
        used += pill.content.chars().count() + 1; // pill + a separating space
        row2.push(pill);
        row2.push(Span::raw(" "));
    }
    used += agent.label.chars().count() + 2; // pill padding
    row2.push(tag_pill(&agent.label, dormant));
    // A live hidden agent shows the hidden icon (it runs in a headless cage, so
    // Enter reveals it by resume rather than focusing a window). A bare glyph,
    // not a pill: it reads as an icon. Count display width (the emoji spans two
    // cells) so the activity text below still truncates to fit.
    let hidden = hidden_badge(agent);
    if !hidden.is_empty() {
        used += 1 + UnicodeWidthStr::width(hidden); // separating space + glyph cells
        row2.push(Span::raw(" "));
        row2.push(Span::styled(hidden, dim));
    }
    if let Some(a) = agent.activity.as_deref() {
        let act_w = width.saturating_sub(used + 2); // two spaces before activity
        row2.push(Span::raw("  "));
        row2.push(Span::styled(truncate(a, act_w), dim));
    }

    ListItem::new(vec![
        title_line(name, age.as_deref(), width, title_style, dim),
        Line::from(row2),
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
    meta: &CardMeta,
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
            // Column name from the shared `Column::title()` (one source for
            // both shells), uppercased as this shell's rendering idiom.
            Span::styled(col.title().to_uppercase(), name_style),
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
    let items: Vec<ListItem> = agents.iter().map(|a| card(a, col, meta, card_w)).collect();
    let list = List::new(items)
        .highlight_symbol("▍")
        // Always reserve the bar column so selecting never shifts the text
        // (each column is its own list; the default only reserves it when that
        // list has the selection).
        .highlight_spacing(HighlightSpacing::Always)
        // Selection is the `▍` bar plus bold. No background fill: a fixed shade
        // looked wrong in one terminal mode and the 16-color palette has no
        // faint-enough adaptive gray, so the bar (theme foreground) is the mark.
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
    meta: &CardMeta,
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
        column(frame, cols[i], col, &agents, sel, &mut states[i], meta);
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
    let (spans, _) = footer_layout();
    frame.render_widget(Paragraph::new(Line::from(spans)), footer_area);

    // The corral mark, bottom-right and faint: "the pen" — a bracketed
    // enclosure holding three dots (the board's own agent glyph). Right-aligned
    // on the footer row, clear of the left-aligned key hints.
    frame.render_widget(
        Paragraph::new(Line::from(LOGO.fg(Color::DarkGray)).alignment(Alignment::Right)),
        footer_area,
    );
}

/// The minimal corral mark shown in the board's bottom-right corner: a pen
/// (`⟦ ⟧`) enclosing three dots (`∴`), matching the tray icon. Glyph only.
const LOGO: &str = "⟦∴⟧";

#[cfg(test)]
mod tests {
    use super::*;
    use corral_core::model::{State, Update};
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
            origin: corral_core::model::Origin::Live,
            spawn_command: None,
            resume_command: None,
            activity: None,
            gui: false,
            message_flag: None,
            hidden: false,
        }));
    }

    #[test]
    fn hit_test_maps_clicks_to_selectable_indices() {
        // Two Requires Action (left column), one Running (right column).
        let mut b = Board::default();
        upsert(&mut b, "/s/a.sock", State::RequiresAction);
        upsert(&mut b, "/s/b.sock", State::RequiresAction);
        upsert(&mut b, "/s/c.sock", State::Running);
        let area = Rect::new(0, 0, 100, 28);
        let no_scroll = [0usize; 4];

        // Columns start below FILTER_ROWS=4; then HEAD_ROWS=2 (heading+rule),
        // so the first card's top row is 4 + 2 = 6, and cards are 3 rows tall.
        assert_eq!(hit_test(area, &b, 5, 6, no_scroll), Some(0));
        assert_eq!(hit_test(area, &b, 5, 9, no_scroll), Some(1));
        assert_eq!(hit_test(area, &b, 5, 5, no_scroll), None); // heading/rule row
        assert_eq!(hit_test(area, &b, 5, 12, no_scroll), None); // past the two cards

        // Second column (Idle) is empty.
        assert_eq!(hit_test(area, &b, 30, 6, no_scroll), None);

        // Third live column (Running): index continues after the two left cards.
        assert_eq!(hit_test(area, &b, 60, 6, no_scroll), Some(2));

        // Footer row is outside every column.
        assert_eq!(hit_test(area, &b, 5, 27, no_scroll), None);

        // A scrolled left column maps the first visible row to the offset item.
        assert_eq!(hit_test(area, &b, 5, 6, [1, 0, 0, 0]), Some(1));
    }
}

#[cfg(test)]
mod card_tests {
    use super::*;
    use corral_core::model::{Agent, Origin, State};
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
            spawn_command: None,
            resume_command: None,
            activity: activity.map(String::from),
            gui: false,
            message_flag: None,
            hidden: false,
        }
    }

    #[test]
    fn hidden_badge_only_for_live_hidden() {
        let mut a = agent(State::Idle, None);
        a.hidden = true;
        assert_eq!(hidden_badge(&a), "\u{1FAE5}");
        a.hidden = false;
        assert_eq!(hidden_badge(&a), "");
        a.hidden = true;
        a.origin = Origin::Dormant;
        assert_eq!(hidden_badge(&a), "", "dormant cards never show the badge");
    }

    #[allow(clippy::type_complexity)]
    fn meta(
        in_state: &[(&str, &str)],
    ) -> (
        HashMap<PathBuf, String>,
        HashMap<PathBuf, String>,
        HashMap<String, String>,
    ) {
        let in_state = in_state
            .iter()
            .map(|(k, v)| (PathBuf::from(*k), v.to_string()))
            .collect();
        (in_state, HashMap::new(), HashMap::new())
    }

    #[test]
    fn idle_age_is_time_in_state() {
        let (i, q, d) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let a = agent(State::Idle, Some("edit model.rs"));
        assert_eq!(card_age(&a, Column::Idle, &m).as_deref(), Some("5m"));
    }

    #[test]
    fn age_is_none_when_unknown() {
        let (i, q, d) = meta(&[]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let a = agent(State::Idle, None);
        assert_eq!(card_age(&a, Column::Idle, &m), None);
    }

    #[test]
    fn title_line_puts_age_right_aligned() {
        let line =
            title_line("fix the auth flow", Some("5m"), 30, Style::default(), Style::default());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 30, "fills the card width exactly");
        assert!(text.starts_with("fix the auth flow"));
        assert!(text.ends_with("5m"), "age is right-aligned");
    }

    #[test]
    fn cwd_pill_is_the_padded_basename() {
        let pill = cwd_pill(Some("/home/u/projects/corral"), false).unwrap();
        assert_eq!(pill.content.as_ref(), " corral ");
    }

    #[test]
    fn cwd_pill_color_is_stable_per_path() {
        // Same path -> same color; the whole grouping premise.
        let a = cwd_pill(Some("/a/corral"), false).unwrap().style.bg;
        let a2 = cwd_pill(Some("/a/corral"), false).unwrap().style.bg;
        assert_eq!(a, a2);
    }

    #[test]
    fn cwd_pill_is_none_when_cwd_missing() {
        assert!(cwd_pill(None, false).is_none());
    }
}
