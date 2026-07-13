//! Rendering. A clean board: columns separated by dim vertical rules, each with
//! a bold heading over an underline, then cards spaced for air. Three live
//! triage columns in attention priority (Requires Action, Idle, Running) and a
//! dim-gray Dormant column (resumable history). A status/help footer sits at
//! the bottom.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};

/// Left/right padding inside every column, so text never touches a separator.
const PAD: u16 = 1;
/// Rows above a column's cards: the heading and its underline rule.
const HEAD_ROWS: u16 = 2;
/// Rows one card spans: title, meta, and a blank spacer for air.
const CARD_ROWS: u16 = 4;
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph,
};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::{Agent, Board, Column, Origin, State};
use crate::picker::{Picker, Row};

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

/// Glyph, color, and title emphasis for an agent's state in the picker. Color
/// carries state here because the picker groups by directory, not by state, so
/// position can no longer encode it (as it does on the board).
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

/// Draw the `/` jump picker: a query line and scope label, then agents grouped
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
    // The list index of the selected agent row (headers are interleaved, so it
    // differs from `picker.selected`, which counts only agent rows).
    let mut highlight: Option<usize> = None;
    let mut agent_seen = 0usize;
    for row in picker.rows() {
        match row {
            Row::Header(dir) => {
                let shown = abbrev_cwd(dir, rows[1].width as usize);
                let faint = Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM);
                // Blank line before each group (except the first) sets the
                // directory groups apart without bloating every row.
                let mut lines = Vec::new();
                if !items.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(path_line(&shown, dim, faint));
                items.push(ListItem::new(lines));
            }
            Row::Agent(a) => {
                let selected = agent_seen == picker.selected;
                if selected {
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
                // A colored bar marks the selected entry (the board's motif);
                // others get matching blank width so titles stay aligned.
                let bar = if selected { "▍ " } else { "  " };
                let mut spans = vec![
                    Span::styled(bar.to_string(), Style::default().fg(color)),
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
    // A uniform dark background marks the selection in one clean shade,
    // keeping each span's own color (REVERSED would invert them piecemeal into
    // a patchy two-tone bar).
    let list = List::new(items).highlight_style(Style::default().bg(Color::Indexed(238)));
    let mut state = ListState::default();
    state.select(highlight);
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

pub(crate) fn basename(path: &str) -> &str {
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

/// Middle-ellipsize `s` to at most `width` columns: keep a head and a tail with
/// `…` between, so both ends stay readable. Never overflows.
fn middle_ellipsis(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".repeat(width);
    }
    let keep = width - 1; // one column for the ellipsis
    let tail = keep / 2;
    let head = keep - tail; // head takes the odd extra column
    let chars: Vec<char> = s.chars().collect();
    let head_str: String = chars[..head].iter().collect();
    let tail_str: String = chars[n - tail..].iter().collect();
    format!("{head_str}…{tail_str}")
}

/// Replace a `$HOME` prefix of `path` with `~`.
fn tilde(path: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if !h.is_empty() && path == h => "~".to_string(),
        Some(h) if !h.is_empty() => match path.strip_prefix(&format!("{h}/")) {
            Some(rest) => format!("~/{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    }
}

/// Abbreviate a filesystem path to fit `width` columns, never overflowing.
/// Replaces the home prefix with `~`, then shortens leading components to their
/// first character (leftmost first, keeping the leaf whole) until it fits, and
/// finally middle-ellipsizes as a hard backstop. Keeps the root anchor and the
/// leaf — the most meaningful parts — readable. Pure, unit-tested.
fn abbreviate_path(path: &str, home: Option<&str>, width: usize) -> String {
    let tilded = tilde(path, home);
    if tilded.chars().count() <= width {
        return tilded;
    }
    let segs: Vec<&str> = tilded.split('/').collect();
    // Shorten the middle components (between the anchor at 0 and the leaf) to a
    // single char, adding one more from the left each pass until it fits.
    let leaf = segs.len() - 1;
    let mut best = tilded.clone();
    for depth in 1..leaf {
        let shortened: Vec<String> = segs
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                if (1..=depth).contains(&i) && i < leaf {
                    seg.chars().next().map(String::from).unwrap_or_default()
                } else {
                    (*seg).to_string()
                }
            })
            .collect();
        best = shortened.join("/");
        if best.chars().count() <= width {
            return best;
        }
    }
    // Even fully shortened it overflows (a deep tree or a long leaf): clamp.
    middle_ellipsis(&best, width)
}

/// Abbreviate a working-directory path to `width`, reading `$HOME` for the
/// tilde. Thin wrapper over the pure `abbreviate_path`.
fn abbrev_cwd(path: &str, width: usize) -> String {
    abbreviate_path(path, std::env::var("HOME").ok().as_deref(), width)
}

/// Split a path into its prefix (up to and including the last `/`) and its leaf
/// (the basename). No slash: empty prefix, the whole string is the leaf.
fn split_at_leaf(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Render a path so the basename stays legible while the leading path recedes:
/// the prefix in `prefix` style (dimmer), the leaf in `leaf` style. Neither is
/// bold.
fn path_line(path: &str, leaf: Style, prefix: Style) -> Line<'static> {
    let (p, l) = split_at_leaf(path);
    let mut spans = Vec::new();
    if !p.is_empty() {
        spans.push(Span::styled(p.to_string(), prefix));
    }
    spans.push(Span::styled(l.to_string(), leaf));
    Line::from(spans)
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

/// The card's info line: what the agent is doing (or last did, or is asking)
/// and a column-specific age. The directory has its own line, so it is not
/// repeated here. The age differs per column because the triage question does:
/// time blocked when it needs you, time since the last activity while running,
/// time idle (how long it has been waiting for you) when idle, and record age
/// when dormant.
fn card_meta_line(agent: &Agent, col: Column, meta: &CardMeta) -> String {
    let mut parts: Vec<&str> = Vec::new();
    // What it is doing (Running) or last did (Idle), or asking (Requires
    // Action, where the activity is often the question).
    if let Some(a) = agent.activity.as_deref() {
        parts.push(a);
    }
    let age = match col {
        // Idle reuses in_state (time since entering the state) to show how long
        // the agent has been waiting for the user.
        Column::RequiresAction | Column::Idle => meta.in_state.get(&agent.socket_path),
        Column::Running => meta.quiet.get(&agent.socket_path),
        Column::Dormant => agent
            .session_id
            .as_deref()
            .and_then(|id| meta.dormant_age.get(id)),
    };
    if let Some(age) = age {
        parts.push(age);
    }
    parts.join(" · ")
}

/// The three card lines, top to bottom (spacer excluded): the title, the cwd
/// basename (empty when unknown), and the info line. Fixed at three so cards
/// keep a uniform height and `hit_test` can divide clicks by `CARD_ROWS`.
/// Pure, unit-tested.
fn card_lines(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> [String; 3] {
    let name = agent.title.as_deref().unwrap_or("(unnamed)");
    // The full working directory (~-abbreviated, shortened to fit) on its own
    // line, so same-named leaves under different roots stay distinguishable.
    let dir = agent
        .cwd
        .as_deref()
        .map(|c| abbrev_cwd(c, width))
        .unwrap_or_default();
    let info = card_meta_line(agent, col, meta);
    [truncate(name, width), dir, truncate(&info, width)]
}

fn card(agent: &Agent, col: Column, meta: &CardMeta, width: usize) -> ListItem<'static> {
    // Dormant cards are dimmed whole: they are context, not a call to act.
    let title_style = match agent.origin {
        Origin::Dormant => Style::default().add_modifier(Modifier::DIM),
        Origin::Live => Style::default(),
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    // The leading path recedes (dark gray) so the basename reads first.
    let faint = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let [name, dir, info] = card_lines(agent, col, meta, width);
    ListItem::new(vec![
        Line::from(Span::styled(name, title_style)),
        path_line(&dir, dim, faint),
        Line::from(Span::styled(info, dim)),
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
    let items: Vec<ListItem> = agents.iter().map(|a| card(a, col, meta, card_w)).collect();
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
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = Vec::new();
    for (i, (_, label)) in footer_items().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(FOOTER_GAP as usize)));
        }
        spans.push(Span::styled(*label, dim));
    }
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
            activity: None,
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

        // Left column: HEAD_ROWS=2 (heading+rule), then 4-row cards.
        assert_eq!(hit_test(area, &b, 5, 2, no_scroll), Some(0));
        assert_eq!(hit_test(area, &b, 5, 6, no_scroll), Some(1));
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
    fn idle_info_line_shows_activity_and_time_idle() {
        let (i, q, d) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let a = agent(State::Idle, Some("edit model.rs"));
        // The path is not under the test HOME, so it shows in full (fits 40).
        assert_eq!(
            card_lines(&a, Column::Idle, &m, 40),
            [
                "fix the auth flow",
                "/home/u/projects/corral",
                "edit model.rs · 5m"
            ]
        );
    }

    #[test]
    fn idle_info_line_is_age_only_without_activity() {
        let (i, q, d) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let a = agent(State::Idle, None);
        assert_eq!(card_lines(&a, Column::Idle, &m, 40)[2], "5m");
    }

    #[test]
    fn requires_action_info_line_is_question_then_age() {
        let (i, q, d) = meta(&[("/s/a.sock", "3m")]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let a = agent(State::RequiresAction, Some("Which branch?"));
        assert_eq!(
            card_lines(&a, Column::RequiresAction, &m, 40),
            [
                "fix the auth flow",
                "/home/u/projects/corral",
                "Which branch? · 3m"
            ]
        );
    }

    #[test]
    fn path_fits_shows_tilde_form() {
        assert_eq!(
            abbreviate_path("/home/u/projects/corral", Some("/home/u"), 40),
            "~/projects/corral"
        );
        assert_eq!(abbreviate_path("/home/u", Some("/home/u"), 40), "~");
    }

    #[test]
    fn path_shortens_leading_components_leftmost_first() {
        // "~/projects/corral/crates/board" is 30 cols; at 20 it shortens the two
        // leftmost components to one letter each, keeping the leaf whole.
        assert_eq!(
            abbreviate_path("/home/u/projects/corral/crates/board", Some("/home/u"), 20),
            "~/p/c/crates/board"
        );
    }

    #[test]
    fn path_never_overflows_even_when_fully_shortened() {
        for w in 1..30 {
            let out = abbreviate_path("/home/u/projects/corral/crates/board", Some("/home/u"), w);
            assert!(out.chars().count() <= w, "width {w}: {out:?}");
        }
    }

    #[test]
    fn split_at_leaf_separates_prefix_and_basename() {
        assert_eq!(split_at_leaf("~/p/c/board"), ("~/p/c/", "board"));
        assert_eq!(split_at_leaf("board"), ("", "board"));
        assert_eq!(split_at_leaf("~"), ("", "~"));
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends() {
        assert_eq!(middle_ellipsis("abcdefgh", 5), "ab…gh");
        assert_eq!(middle_ellipsis("short", 10), "short");
    }

    #[test]
    fn basename_is_empty_string_when_cwd_missing() {
        let (i, q, d) = meta(&[]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
        };
        let mut a = agent(State::Idle, None);
        a.cwd = None;
        assert_eq!(card_lines(&a, Column::Idle, &m, 40)[1], "");
    }
}
