//! corral: an attention board for locally running pi agents.
//!
//! Discovers ACP sockets under $XDG_RUNTIME_DIR/acp/, watches each for its
//! Working/Needs-You state, and shows them in two columns. Enter focuses an
//! agent's window (sway), `n` spawns a new agent (kitty), `q` quits. Corral
//! never drives an agent; it routes the operator's attention.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};

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
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        eprintln!("corral: XDG_RUNTIME_DIR is not set");
        std::process::exit(1);
    };
    let dir = runtime_dir.join("acp");

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &dir);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("corral: {e}");
        std::process::exit(1);
    }
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
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        if count > 0 {
                            selected = (selected + 1).min(count - 1);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        status.clear();
                        if let Some(agent) = board.selectable().get(selected) {
                            if let Err(e) = focuser.focus(agent) {
                                status = format!("focus: {e}");
                            }
                        }
                    }
                    KeyCode::Char('n') => {
                        status.clear();
                        let cwd = launch::default_cwd(board.selectable().get(selected).copied());
                        if let Err(e) = launcher.spawn(&cwd) {
                            status = format!("spawn: {e}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
