//! corral: an attention board for locally running pi agents.
//!
//! Discovers ACP sockets under $HOME/.corral/sockets/ (override with
//! $CORRAL_ACP_DIR), watches each for its running/idle/requires_action state,
//! and shows them in three columns. Enter or a mouse click focuses an agent's
//! window (sway), `n` spawns a new agent (kitty), `q` quits. Scroll or arrows
//! move the selection. Corral never drives an agent; it
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
use model::{Board, Update};

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

        let count = board.selectable().len();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        terminal.draw(|f| ui::render(f, &board, selected, &status))?;

        if event::poll(POLL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                        selected = (selected + 1).min(count - 1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
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
                    MouseEventKind::ScrollDown if count > 0 => {
                        selected = (selected + 1).min(count - 1);
                    }
                    MouseEventKind::ScrollUp => selected = selected.saturating_sub(1),
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
