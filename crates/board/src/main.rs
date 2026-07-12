//! corral: an attention board for locally running pi agents.
//!
//! Discovers ACP sockets under $HOME/.corral/sockets/ (override with
//! $CORRAL_ACP_DIR), watches each for its running/idle/requires_action state,
//! and shows them in three columns. Enter or a mouse click focuses an agent's
//! window (sway), `n` spawns a new agent (kitty), `q` quits. Up/Down (or
//! scroll) move within a column; Left/Right switch columns. Corral never
//! drives an agent; it
//! routes the operator's attention.
//!
//! Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::Rect;

mod discovery;
mod focus;
mod launch;
mod model;
mod ui;
mod watch;

use focus::{SwayFocuser, WindowFocuser};
use launch::{KittyLauncher, Launcher};
use model::{Board, State, Update};

const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(250);

fn main() {
    let Some(dir) = acp_dir() else {
        eprintln!("corral: set $CORRAL_ACP_DIR or $HOME");
        std::process::exit(1);
    };

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal, &dir);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("corral: {e}");
        std::process::exit(1);
    }
}

/// The socket discovery directory: $CORRAL_ACP_DIR, else $HOME/.corral/sockets.
fn acp_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CORRAL_ACP_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corral").join("sockets"))
}

fn run(terminal: &mut ratatui::DefaultTerminal, dir: &std::path::Path) -> std::io::Result<()> {
    let (tx, rx): (Sender<Update>, Receiver<Update>) = mpsc::channel();
    let focuser = SwayFocuser;
    let launcher = KittyLauncher;

    let mut board = Board::default();
    let mut known: HashSet<PathBuf> = HashSet::new();
    let mut selected: usize = 0;
    let mut status = String::new();
    let mut last_scan = Instant::now() - SCAN_INTERVAL * 2;

    loop {
        if last_scan.elapsed() >= SCAN_INTERVAL {
            for entry in discovery::scan(dir) {
                if known.insert(entry.path.clone()) {
                    watch::spawn(entry, tx.clone());
                }
            }
            last_scan = Instant::now();
        }

        // Drain watcher updates. A Gone drops the socket from `known` so a
        // transient failure self-heals on the next scan; a genuinely dead
        // socket just reconnects-and-Gones cheaply once per second.
        while let Ok(update) = rx.try_recv() {
            if let Update::Gone(path) = &update {
                known.remove(path);
            }
            board.apply(update);
        }

        let counts = column_counts(&board);
        let count: usize = counts.iter().sum();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        terminal.draw(|f| ui::render(f, &board, selected, &status))?;

        if event::poll(POLL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = move_row(selected, &counts, true);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = move_row(selected, &counts, false);
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        selected = move_col(selected, &counts, false);
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        selected = move_col(selected, &counts, true);
                    }
                    KeyCode::Enter => focus_selected(&focuser, &board, selected, &mut status),
                    KeyCode::Char('n') => {
                        status.clear();
                        let cwd = launch::default_cwd(board.selectable().get(selected).copied());
                        if let Err(e) = launcher.spawn(&cwd) {
                            status = format!("spawn: {e}");
                        }
                    }
                    _ => {}
                },
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => selected = move_row(selected, &counts, true),
                    MouseEventKind::ScrollUp => selected = move_row(selected, &counts, false),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let s = terminal.size()?;
                        let area = Rect::new(0, 0, s.width, s.height);
                        if let Some(idx) = ui::hit_test(area, &board, m.column, m.row) {
                            selected = idx;
                            focus_selected(&focuser, &board, selected, &mut status);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Ok(())
}

/// Focus the selected agent's window, recording any error in the status line.
/// Shared by the Enter key and a left click.
fn focus_selected(
    focuser: &dyn WindowFocuser,
    board: &Board,
    selected: usize,
    status: &mut String,
) {
    status.clear();
    if let Some(agent) = board.selectable().get(selected) {
        if let Err(e) = focuser.focus(agent) {
            *status = format!("focus: {e}");
        }
    }
}

/// Column agent counts in board order: RequiresAction, Idle, Running. This
/// matches `Board::selectable()`, so a flat index maps cleanly to (column,row).
fn column_counts(board: &Board) -> [usize; 3] {
    [
        board.in_state(State::RequiresAction).len(),
        board.in_state(State::Idle).len(),
        board.in_state(State::Running).len(),
    ]
}

/// Flat selectable index -> (column, row).
fn locate(index: usize, counts: &[usize; 3]) -> (usize, usize) {
    let mut i = index;
    for (c, &n) in counts.iter().enumerate() {
        if i < n {
            return (c, i);
        }
        i -= n;
    }
    (0, 0)
}

/// (column, row) -> flat selectable index.
fn flat(col: usize, row: usize, counts: &[usize; 3]) -> usize {
    counts[..col].iter().sum::<usize>() + row
}

/// Move within the current column (Up/Down), clamped to that column.
fn move_row(index: usize, counts: &[usize; 3], down: bool) -> usize {
    let (c, r) = locate(index, counts);
    if counts[c] == 0 {
        return index;
    }
    let r = if down {
        (r + 1).min(counts[c] - 1)
    } else {
        r.saturating_sub(1)
    };
    flat(c, r, counts)
}

/// Jump to the nearest non-empty column in a direction (Left/Right), keeping
/// the row where possible.
fn move_col(index: usize, counts: &[usize; 3], right: bool) -> usize {
    let (c, r) = locate(index, counts);
    let candidates: Vec<usize> = if right {
        (c + 1..counts.len()).collect()
    } else {
        (0..c).rev().collect()
    };
    for tc in candidates {
        if counts[tc] > 0 {
            return flat(tc, r.min(counts[tc] - 1), counts);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_maps_flat_index_to_columns() {
        // RequiresAction=2, Idle=0, Running=1. selectable order: RA0, RA1, Run0.
        let counts = [2usize, 0, 1];
        assert_eq!(locate(0, &counts), (0, 0));
        assert_eq!(locate(2, &counts), (2, 0));
        // Down within the column, clamped.
        assert_eq!(move_row(0, &counts, true), 1);
        assert_eq!(move_row(1, &counts, true), 1);
        assert_eq!(move_row(1, &counts, false), 0);
        // Right from RA skips the empty Idle column to Running.
        assert_eq!(move_col(1, &counts, true), 2);
        // Left from Running lands back in RA, row clamped.
        assert_eq!(move_col(2, &counts, false), 0);
        // Right from the last column stays put.
        assert_eq!(move_col(2, &counts, true), 2);
    }
}
