//! Key-event capture + decision trace, mirroring the board's EXACT key
//! evaluation. Same terminal setup as the board (`ratatui::init`, mouse
//! capture, the board's kitty flag), the same `poll`/`read` loop, and the same
//! `match` arms with the same guards — but each arm prints a label of which
//! board action it WOULD have fired, instead of firing it. So if Neo layer-4
//! arrows work here but not in the real board, the difference is outside this
//! loop; if they fall to `_ => {}` here, we see exactly why.
//!
//! Run:  cargo run -p corral --example keycap
//! Optional env: CORRAL_KEYCAP_FLAGS = normal (default) | move | none
//!   normal -> DISAMBIGUATE only (the real board's navigation mode)
//!   move   -> DISAMBIGUATE + REPORT_ALL + REPORT_EVENT_TYPES (move mode)
//!   none   -> no enhancement
//! Quit with q or Ctrl+C. The trace re-prints to the restored terminal after.

use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, ModifierKeyCode, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;

const POLL: Duration = Duration::from_millis(250);

/// Decide which board action a key resolves to, using the EXACT same arms and
/// guards as `crates/board/src/main.rs`'s normal-mode match (the block starting
/// `Event::Key(key) if key.kind == KeyEventKind::Press => match key.code`).
/// Returns a short label; "(ignored)" means it hit `_ => {}`.
fn classify_normal(key: &crossterm::event::KeyEvent) -> &'static str {
    // Guard: identical to the board's `if key.kind == KeyEventKind::Press`.
    if key.kind != KeyEventKind::Press {
        return "(ignored: not a Press)";
    }
    match key.code {
        KeyCode::Char('q') => "quit",
        KeyCode::Esc => "esc: clear filter",
        KeyCode::Down => "nav: down (move_selection, or ring to filter)",
        KeyCode::Up => "nav: up (move_selection, or ring to filter)",
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
            "move-mode: grab left (shift+left)"
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
            "move-mode: grab right (shift+right)"
        }
        KeyCode::Left => "nav: col left",
        KeyCode::Right => "nav: col right",
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => "spawn new",
        KeyCode::Enter => "go: activate selected",
        KeyCode::Char('d') => "dismiss",
        KeyCode::Char('h') => "toggle hidden",
        KeyCode::Char('o') => "history",
        KeyCode::Char('/') => "focus filter",
        KeyCode::Char('m') => "message (compose)",
        _ => "(ignored: no arm)",
    }
}

/// Same decision shape as the board's filter-edit-mode match.
fn classify_filter(key: &crossterm::event::KeyEvent) -> &'static str {
    if key.kind != KeyEventKind::Press {
        return "(ignored: not a Press)";
    }
    match key.code {
        KeyCode::Esc => "filter: leave edit mode",
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => "filter: spawn new",
        KeyCode::Enter => "filter: go (activate)",
        KeyCode::Backspace => "filter: pop",
        KeyCode::Down => "filter: step off into board (first card)",
        KeyCode::Up => "filter: step off into board (last card)",
        KeyCode::Left => "filter: col left",
        KeyCode::Right => "filter: col right",
        KeyCode::Char(c) => match c {
            'q' => "filter: quit",
            _ => "filter: push char",
        },
        _ => "(ignored: no arm)",
    }
}

/// Same decision shape as the board's move-mode match (uses event-type guards).
fn classify_move(key: &crossterm::event::KeyEvent) -> &'static str {
    match (key.code, key.kind) {
        (
            KeyCode::Modifier(ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift),
            KeyEventKind::Release,
        ) => "move: commit (shift released)",
        (KeyCode::Enter, KeyEventKind::Press) => "move: commit (enter)",
        (KeyCode::Esc, KeyEventKind::Press) => "move: cancel (esc)",
        (KeyCode::Left, KeyEventKind::Press) => "move: slide target left",
        (KeyCode::Right, KeyEventKind::Press) => "move: slide target right",
        _ => "(ignored: no arm)",
    }
}

fn main() {
    // Match the board's setup exactly: alternate screen + raw mode, mouse
    // capture, then the kitty enhancement flag the board pushes.
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

    let mode = std::env::var("CORRAL_KEYCAP_FLAGS").unwrap_or_else(|_| "normal".into());
    let flags = match mode.as_str() {
        "move" => Some(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        ),
        "none" => None,
        _ => Some(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    };
    if let Some(f) = flags {
        let _ = execute!(std::io::stdout(), PushKeyboardEnhancementFlags(f));
    }

    // Three modes the board cycles through; cycle with Tab.
    let mut which: u8 = 0; // 0=normal, 1=filter, 2=move
    let mode_label = |w: u8| match w {
        0 => "NORMAL",
        1 => "FILTER",
        _ => "MOVE",
    };
    let mut lines: Vec<String> = Vec::new();

    let render = |lines: &Vec<String>, which: u8, terminal: &mut ratatui::DefaultTerminal| {
        use ratatui::widgets::Paragraph;
        let body = format!(
            "enhanced={enhanced} flags={mode}  mode={}  (Tab cycles; q/Ctrl+C quits)\n\n{}",
            mode_label(which),
            lines.join("\n")
        );
        let _ = terminal.draw(|f| {
            f.render_widget(Paragraph::new(body), f.area());
        });
    };
    render(&lines, which, &mut terminal);

    loop {
        if event::poll(POLL).unwrap() {
            let ev = event::read().unwrap();
            match ev {
                Event::Key(key) => {
                    // Cycle the decision mode on Tab (board has no Tab binding,
                    // so this is a tool-only control).
                    if key.code == KeyCode::Tab && key.kind == KeyEventKind::Press {
                        which = (which + 1) % 3;
                        lines.clear();
                        render(&lines, which, &mut terminal);
                        continue;
                    }
                    let label = match which {
                        0 => classify_normal(&key),
                        1 => classify_filter(&key),
                        _ => classify_move(&key),
                    };
                    let m = mode_label(which);
                    lines.push(format!("{key:?}  ->  [{m}] {label}"));
                    render(&lines, which, &mut terminal);
                    if key.code == KeyCode::Char('q')
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        break;
                    }
                }
                other => {
                    lines.push(format!("{other:?}  ->  (non-key)"));
                    render(&lines, which, &mut terminal);
                }
            }
        }
    }

    if flags.is_some() {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    for l in &lines {
        println!("{l}");
    }
}
