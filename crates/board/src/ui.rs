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

use corral_core::menu::MenuAction;
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

/// The column whose rect contains the mouse cell `col` (x only; during a move
/// the drop-boxes span the full column height). Used to pick a drag/drop target
/// column. None for the gutters or outside the columns.
pub fn column_at(area: Rect, col: u16) -> Option<Column> {
    let cols = column_layout(area);
    cols.iter()
        .position(|r| col >= r.x && col < r.x + r.width)
        .map(|i| Column::ALL[i])
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

/// The right-click context menu's on-screen rect, anchored at the cursor
/// `(col,row)` and clamped so it stays fully inside `area`. Width fits the
/// widest entry label plus padding and borders; height is one row per entry
/// plus the top/bottom border. Shared by `render_menu` and `menu_hit_test` so
/// their geometry cannot drift.
pub fn menu_rect(area: Rect, anchor: (u16, u16)) -> Rect {
    let label_w = MenuAction::ALL
        .iter()
        .map(|a| a.label().chars().count())
        .max()
        .unwrap_or(0) as u16;
    // borders (2) + one space of padding on each side (2).
    let width = label_w + 4;
    let height = MenuAction::ALL.len() as u16 + 2;
    let (cx, cy) = anchor;
    // Clamp the top-left so the whole box fits on screen.
    let x = cx.min(area.x + area.width.saturating_sub(width));
    let y = cy.min(area.y + area.height.saturating_sub(height));
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Map a click inside the menu to an entry index, or None for the border rows
/// or a click outside the menu entirely. Uses the same rect `render_menu`
/// draws.
pub fn menu_hit_test(rect: Rect, col: u16, row: u16) -> Option<usize> {
    let inside = col >= rect.x
        && col < rect.x + rect.width
        && row > rect.y // skip the top border
        && row < rect.y + rect.height - 1; // skip the bottom border
    inside.then(|| (row - rect.y - 1) as usize)
}

/// Draw the right-click context menu: a bordered box of the five actions, the
/// highlighted entry reversed. `selected` is the highlighted entry index.
pub fn render_menu(frame: &mut Frame, anchor: (u16, u16), selected: usize) {
    let rect = menu_rect(frame.area(), anchor);
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let lines: Vec<Line> = MenuAction::ALL
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let style = if i == selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            // Pad the label to the inner width so the reversed highlight spans
            // the whole row, not just the text.
            let text = format!(
                " {:<w$}",
                a.label(),
                w = inner.width.saturating_sub(1) as usize
            );
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
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
/// font; the keycap styling in `footer_layout` supplies the visual polish. The
/// verbs shared with the context menu come from `MenuAction::label` so the two
/// cannot drift (footer is the source of truth for those strings).
fn footer_items() -> [(Option<FooterAction>, &'static str, &'static str); 8] {
    [
        (None, "arrows", "move"),
        (Some(FooterAction::Go), "enter", MenuAction::Go.label()),
        (
            Some(FooterAction::New),
            "shift+enter",
            MenuAction::Spawn.label(),
        ),
        (Some(FooterAction::Jump), "/", "filter"),
        (Some(FooterAction::Msg), "m", MenuAction::Message.label()),
        (Some(FooterAction::Delete), "d", MenuAction::Dismiss.label()),
        (
            Some(FooterAction::Toggle),
            "h",
            MenuAction::ToggleHidden.label(),
        ),
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

/// Label for the compose target and the `f` focus picker: the title and the
/// cwd's last path segment.
/// The marker shown after the kind badge for a live hidden agent, empty
/// otherwise. A plain-text `hidden` word (rendered as a muted pill), not an
/// emoji: it must render in any terminal font, so it does not depend on
/// emoji-glyph coverage. Kept pure so it is unit-tested without a terminal.
/// (The GUI renders the same word.)
pub fn hidden_badge(agent: &Agent) -> &'static str {
    if agent.origin == Origin::Live && agent.hidden {
        "hidden"
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
    /// Cards with a pending card-move, keyed by agent id (session id, else
    /// socket path), mapped to the destination column title. Such a card stays
    /// in its real column with a `→ <target> ⋯` in-flight badge until the
    /// agent's own state reaches the target (the board never fakes state).
    pub pending: &'a HashMap<String, String>,
}

/// The stable id a card-move is keyed on (delegates to `Agent::move_key`, the
/// single cross-shell definition). Shared by the shell (recording a pending
/// move) and the card renderer (showing the badge).
pub fn agent_key(agent: &Agent) -> String {
    agent.move_key()
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

    // A pending card-move shows a bright in-flight badge; the card has not
    // moved yet (it waits for the agent's real state to reach the target).
    let pending = meta.pending.get(&agent_key(agent)).cloned();

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
    // A live hidden agent shows a `hidden` pill (it runs in a headless cage, so
    // Enter reveals it by resume rather than focusing a window). A muted tag
    // pill of plain text, so it renders in any terminal font. Count its width
    // (text + a space + the pill's two padding cells) so the activity text
    // below still truncates to fit.
    let hidden = hidden_badge(agent);
    if !hidden.is_empty() {
        used += 1 + hidden.chars().count() + 2; // separating space + pill
        row2.push(Span::raw(" "));
        row2.push(tag_pill(hidden, dormant));
    }
    if let Some(target) = &pending {
        // The in-flight badge takes precedence over the activity hint: it is
        // the operator's own pending action and the more urgent thing to see.
        let badge = format!("  → {target} ⋯");
        row2.push(Span::styled(
            truncate(&badge, width.saturating_sub(used)),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else if let Some(a) = agent.activity.as_deref() {
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

/// Draw move mode: the columns become labeled drop-boxes with their cards
/// hidden (the boxes cover them), the target box highlighted, Requires Action
/// greyed as a non-destination. The moving card's label sits inside the target
/// box, and a hint line shows the controls. Drawn as an overlay over the board
/// so `render` stays untouched.
pub fn render_move(frame: &mut Frame, source: Column, target: Column, moving_label: &str) {
    use corral_core::transition::{action_for, MoveAction, DESTINATIONS};
    let cols = column_layout(frame.area());
    // Whether committing on the target does anything: a no-op target (the source
    // column, or Requires Action) means "drop to cancel".
    let cancels = matches!(action_for(source, target), MoveAction::NoOp);
    for (i, col) in Column::ALL.into_iter().enumerate() {
        let rect = cols[i];
        frame.render_widget(Clear, rect);
        let is_target = col == target;
        let is_dest = DESTINATIONS.contains(&col) || col == source;
        let border_style = if is_target && cancels {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if is_target {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if is_dest {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            // Requires Action (not the source): never a destination.
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        };
        let title = if is_dest {
            format!(" {} ", col.title())
        } else {
            format!(" {} (not a target) ", col.title())
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if is_target {
            // Show the moving card's label centered in the target box, plus a
            // "drop to cancel" note when the target is a no-op.
            let text = if cancels {
                format!("{moving_label} (drop to cancel)")
            } else {
                moving_label.to_string()
            };
            let label = truncate(&text, inner.width.saturating_sub(2) as usize);
            let mid = Rect {
                y: inner.y + inner.height / 2,
                height: 1,
                ..inner
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    Style::default().add_modifier(Modifier::BOLD),
                )))
                .alignment(Alignment::Center),
                mid,
            );
        }
    }
    // Control hint on the footer row.
    let footer = footer_rect(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "moving: shift+←/→ choose column, release shift to drop, esc cancel",
            Style::default().add_modifier(Modifier::DIM),
        ))),
        footer,
    );
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

    fn upsert(board: &mut Board, path: &str, state: State) {
        board.apply(Update::Upsert(Box::new(Agent {
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
            state_since: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        })));
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

    #[test]
    fn menu_rect_clamps_to_stay_on_screen() {
        let area = Rect::new(0, 0, 100, 28);
        // Anchored near the bottom-right, the box shifts left/up to fit.
        let r = menu_rect(area, (98, 26));
        assert!(r.x + r.width <= area.width);
        assert!(r.y + r.height <= area.height);
        assert_eq!(r.height as usize, MenuAction::ALL.len() + 2);
    }

    #[test]
    fn menu_hit_test_maps_rows_to_entries_and_skips_borders() {
        let area = Rect::new(0, 0, 100, 28);
        let r = menu_rect(area, (10, 5));
        // Top border row: no entry.
        assert_eq!(menu_hit_test(r, r.x + 1, r.y), None);
        // First entry row (just below the top border).
        assert_eq!(menu_hit_test(r, r.x + 1, r.y + 1), Some(0));
        assert_eq!(menu_hit_test(r, r.x + 1, r.y + 5), Some(4));
        // Bottom border row: no entry.
        assert_eq!(menu_hit_test(r, r.x + 1, r.y + r.height - 1), None);
        // A column outside the box: no entry.
        assert_eq!(menu_hit_test(r, r.x + r.width + 1, r.y + 1), None);
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
            state_since: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        }
    }

    #[test]
    fn hidden_badge_only_for_live_hidden() {
        let mut a = agent(State::Idle, None);
        a.hidden = true;
        assert_eq!(hidden_badge(&a), "hidden");
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
        HashMap<String, String>,
    ) {
        let in_state = in_state
            .iter()
            .map(|(k, v)| (PathBuf::from(*k), v.to_string()))
            .collect();
        (in_state, HashMap::new(), HashMap::new(), HashMap::new())
    }

    #[test]
    fn idle_age_is_time_in_state() {
        let (i, q, d, p) = meta(&[("/s/a.sock", "5m")]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
            pending: &p,
        };
        let a = agent(State::Idle, Some("edit model.rs"));
        assert_eq!(card_age(&a, Column::Idle, &m).as_deref(), Some("5m"));
    }

    #[test]
    fn age_is_none_when_unknown() {
        let (i, q, d, p) = meta(&[]);
        let m = CardMeta {
            in_state: &i,
            quiet: &q,
            dormant_age: &d,
            pending: &p,
        };
        let a = agent(State::Idle, None);
        assert_eq!(card_age(&a, Column::Idle, &m), None);
    }

    #[test]
    fn title_line_puts_age_right_aligned() {
        let line = title_line(
            "fix the auth flow",
            Some("5m"),
            30,
            Style::default(),
            Style::default(),
        );
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
