//! corral: an attention board for locally running coding-agent sessions.
//!
//! Discovers sessions from the registry under $HOME/.corral/registry/
//! (override with $CORRAL_REGISTRY_DIR): each `<sessionId>.json` names a
//! workdir-local ACP socket. Corral watches each live socket for its
//! running/idle/requires_action state,
//! and shows them in four columns. Enter or a mouse click focuses an agent's
//! window, Shift+Enter spawns a new agent in the selected agent's dir, `/`
//! focuses the inline filter, `m` composes a message, `d` dismisses, `q`
//! quits (Esc peels one layer per press, then exits). `--launcher` opens an
//! ephemeral popup that exits after a go/spawn. Up/Down (or scroll) move
//! within a column; Left/Right switch columns. Corral never drives an agent;
//! it routes the operator's attention.
//!
//! Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

mod ui;

use corral_core::discovery::{self, RegistryEntry};
use corral_core::focus::{self, WindowFocuser};
use corral_core::launch::{self, Launcher, TerminalLauncher};
use corral_core::model::{Board, Origin, Update};
use corral_core::prompt;
use corral_core::{model, nav, paths, watch};

const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(250);
/// A dormant record untouched for this long is pruned (its session file is
/// stale or abandoned). Measured from the registry file's mtime, which the
/// extension refreshes on every turn and on clean shutdown.
const DORMANT_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

fn main() {
    let Some(dir) = paths::registry_dir() else {
        eprintln!("corral: set $CORRAL_REGISTRY_DIR or $HOME");
        std::process::exit(1);
    };

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // Shift+Enter is only distinguishable from Enter under the kitty keyboard
    // protocol; push the disambiguate flag where the terminal supports it
    // (kitty does). Ordinary keys are unaffected.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    // `--launcher`: open as an ephemeral popup (filter focused, a successful
    // go/spawn exits) to match the GUI launcher.
    let launcher_mode = std::env::args().any(|a| a == "--launcher");
    let result = run(&mut terminal, &dir, launcher_mode);
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("corral: {e}");
        std::process::exit(1);
    }
}

/// Where an operator-composed message (`m`) is delivered.
enum ComposeTarget {
    /// A live agent: deliver straight to its socket.
    Live(PathBuf),
    /// A dormant session: resume it with the message as its first prompt.
    Dormant {
        cwd: String,
        resume_command: Vec<String>,
    },
}

/// The operator composing a message (opened with `m`): the target, a display
/// label for the prompt, and the text so far.
struct Compose {
    target: ComposeTarget,
    label: String,
    buf: String,
}

/// The active input overlay. Exactly one can be open, so the modes are
/// exclusive by construction: no parallel `Option`s to keep consistent.
enum Overlay {
    /// `m`: compose a message to a live agent.
    Compose(Compose),
}

/// Feed one event to the open overlay. Returns the overlay to keep it open, or
/// `None` once it has closed (cancelled or acted).
fn handle_overlay(
    mut ov: Overlay,
    ev: Event,
    launcher: &dyn Launcher,
    status: &mut String,
) -> Option<Overlay> {
    let Event::Key(key) = ev else {
        return Some(ov);
    };
    if key.kind != KeyEventKind::Press {
        return Some(ov);
    }
    match &mut ov {
        Overlay::Compose(c) => match key.code {
            KeyCode::Esc => None,
            KeyCode::Enter => {
                let text = c.buf.trim();
                if !text.is_empty() {
                    match &c.target {
                        ComposeTarget::Live(socket) => {
                            *status = match prompt::send_prompt(socket, text) {
                                Ok(()) => format!("sent to {}", c.label),
                                Err(e) => format!("send: {e}"),
                            };
                        }
                        // Dormant: resume the session with the message as its
                        // first prompt (atomic, no wait-for-announce).
                        ComposeTarget::Dormant {
                            cwd,
                            resume_command,
                        } => {
                            *status =
                                match launcher.launch(Path::new(cwd), resume_command, Some(text)) {
                                    Ok(()) => format!("resuming {} to deliver", c.label),
                                    Err(e) => format!("resume: {e}"),
                                };
                        }
                    }
                }
                None
            }
            KeyCode::Backspace => {
                c.buf.pop();
                Some(ov)
            }
            KeyCode::Char(ch) => {
                c.buf.push(ch);
                Some(ov)
            }
            _ => Some(ov),
        },
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    dir: &std::path::Path,
    launcher_mode: bool,
) -> std::io::Result<()> {
    let (tx, rx): (Sender<Update>, Receiver<Update>) = mpsc::channel();
    // EWMH on X11, sway on Wayland (until other Wayland focusers land).
    let focuser = focus::detect();
    let launcher = TerminalLauncher;

    let mut board = Board::default();
    let mut known: HashSet<PathBuf> = HashSet::new();
    // Sockets whose watcher failed to connect. A record whose socket is set
    // but dead is a crashed session: surfaced as dormant (resumable) rather
    // than vanishing. Cleared when the socket comes alive (an Upsert).
    let mut dead_sockets: HashSet<PathBuf> = HashSet::new();
    let mut selected: usize = 0;
    let mut status = String::new();
    // Inline content filter (`/` focuses it). When non-empty the board shows
    // only matching cards; `filtering` is the text-edit mode.
    let mut filter = String::new();
    // The launcher popup opens straight into filter-edit mode (type to narrow,
    // Enter to go), matching the GUI launcher.
    let mut filtering = launcher_mode;
    // The active input overlay, if any (`m` compose a message).
    let mut overlay: Option<Overlay> = None;
    // Inter-agent messaging lives entirely in the corrald daemon now; the board
    // is a pure viewer of the registry plus the operator's own actions (focus,
    // spawn, resume, and the ungated `m` message to a selected agent).
    // One persistent ListState per column so ratatui scrolls long columns and
    // hit_test can read each column's scroll offset.
    let mut list_states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
    // When each live agent entered its current state, keyed by socket path, so
    // Requires Action cards can show how long it has been blocked.
    let mut state_since: HashMap<PathBuf, Instant> = HashMap::new();
    // When each live agent last produced a tool activity or transition, so
    // Running cards show time-since-activity (a stuck hint) not raw run time.
    let mut last_event: HashMap<PathBuf, Instant> = HashMap::new();
    // Age of each dormant session's registry record (by session id), from its
    // file mtime; refreshed each scan.
    let mut dormant_ages: HashMap<String, String> = HashMap::new();
    let mut last_scan = Instant::now() - SCAN_INTERVAL * 2;

    loop {
        if last_scan.elapsed() >= SCAN_INTERVAL {
            // The registry is the single store. Prune stale dormant records,
            // watch each live socket, and hand the survivors to the board so it
            // can rebuild the dormant column.
            let entries = prune(dir, discovery::scan_registry(dir));
            // Forget dead sockets for records that no longer exist, so the set
            // cannot grow without bound.
            dead_sockets.retain(|p| {
                entries
                    .iter()
                    .any(|e| e.socket.as_deref() == Some(p.as_path()))
            });
            for entry in &entries {
                if let Some(sock) = discovery::live_socket(entry) {
                    if known.insert(sock.path.clone()) {
                        watch::spawn(sock, tx.clone());
                    }
                }
            }
            board.sync_registry(&entries, &dead_sockets);
            // Record age (by session id) for the dormant column, from each
            // record's file mtime.
            dormant_ages.clear();
            for e in &entries {
                let file = dir.join(format!("{}.json", e.session_id));
                if let Some(age) = std::fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(ui::age_label)
                {
                    dormant_ages.insert(e.session_id.clone(), age);
                }
            }
            last_scan = Instant::now();
        }

        // Drain watcher updates. A Gone drops the socket from `known` so a
        // transient failure self-heals on the next scan; a genuinely dead
        // socket just reconnects-and-Gones cheaply once per second.
        while let Ok(update) = rx.try_recv() {
            match &update {
                Update::Gone(path) => {
                    known.remove(path);
                    state_since.remove(path);
                    last_event.remove(path);
                    dead_sockets.insert(path.clone());
                }
                // Each SetState is a real transition (the extension only
                // broadcasts on change): restart the state timer, and count it
                // as activity for the stuck-hint timer.
                Update::SetState(path, _) => {
                    let now = Instant::now();
                    state_since.insert(path.clone(), now);
                    last_event.insert(path.clone(), now);
                }
                // A tool call is fresh activity: reset the stuck-hint timer.
                Update::SetActivity(path, _) => {
                    last_event.insert(path.clone(), Instant::now());
                }
                Update::Upsert(a) => {
                    let now = Instant::now();
                    state_since.entry(a.socket_path.clone()).or_insert(now);
                    last_event.entry(a.socket_path.clone()).or_insert(now);
                    dead_sockets.remove(&a.socket_path);
                }
                Update::SetTitle(..) => {}
            }
            board.apply(update);
        }

        board.set_filter(filter.clone());
        let counts = board.column_counts();
        let count: usize = counts.iter().sum();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        let in_state: HashMap<PathBuf, String> = state_since
            .iter()
            .map(|(p, t)| (p.clone(), ui::age_label(t.elapsed())))
            .collect();
        let quiet: HashMap<PathBuf, String> = last_event
            .iter()
            .map(|(p, t)| (p.clone(), ui::age_label(t.elapsed())))
            .collect();
        let meta = ui::CardMeta {
            in_state: &in_state,
            quiet: &quiet,
            dormant_age: &dormant_ages,
        };
        terminal.draw(|f| {
            ui::render(f, &board, selected, &status, &mut list_states, &meta);
            ui::render_filter(f, &filter, filtering);
            match &overlay {
                Some(Overlay::Compose(c)) => ui::render_compose(f, &c.label, &c.buf),
                None => {}
            }
        })?;

        if event::poll(POLL)? {
            let ev = event::read()?;
            // Filter edit mode: printable keys edit the query, arrows still
            // navigate, Enter keeps it, Esc leaves filter mode (never quits).
            if filtering {
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            // Esc peels one layer: leave edit mode but keep
                            // the query (a second Esc then clears it).
                            KeyCode::Esc => filtering = false,
                            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                if spawn_new(&launcher, &board, selected, &mut status)
                                    && launcher_mode
                                {
                                    break;
                                }
                            }
                            KeyCode::Enter => {
                                if activate_selected(
                                    focuser.as_ref(),
                                    &launcher,
                                    &board,
                                    selected,
                                    &mut status,
                                ) && launcher_mode
                                {
                                    break;
                                }
                            }
                            KeyCode::Backspace => {
                                filter.pop();
                            }
                            KeyCode::Down => selected = nav::move_row(selected, &counts, true),
                            KeyCode::Up => selected = nav::move_row(selected, &counts, false),
                            KeyCode::Left => selected = nav::move_col(selected, &counts, false),
                            KeyCode::Right => selected = nav::move_col(selected, &counts, true),
                            KeyCode::Char(c) => filter.push(c),
                            _ => {}
                        }
                    }
                }
                continue;
            }
            // Any open overlay captures all input until it closes.
            if let Some(ov) = overlay.take() {
                overlay = handle_overlay(ov, ev, &launcher, &mut status);
                continue;
            }
            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') => break,
                    // Esc peels layers, quitting at the last: clear a non-empty
                    // filter (reset the cursor), else exit. Matches the GUI so
                    // both shells behave alike.
                    KeyCode::Esc => {
                        if filter.is_empty() {
                            break;
                        }
                        filter.clear();
                        selected = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = nav::move_row(selected, &counts, true);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = nav::move_row(selected, &counts, false);
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        selected = nav::move_col(selected, &counts, false);
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        selected = nav::move_col(selected, &counts, true);
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if spawn_new(&launcher, &board, selected, &mut status) && launcher_mode {
                            break;
                        }
                    }
                    KeyCode::Enter => {
                        if activate_selected(
                            focuser.as_ref(),
                            &launcher,
                            &board,
                            selected,
                            &mut status,
                        ) && launcher_mode
                        {
                            break;
                        }
                    }
                    KeyCode::Char('d') => {
                        dismiss_selected(dir, focuser.as_ref(), &board, selected, &mut status);
                    }
                    KeyCode::Char('/') => {
                        // Focus the inline filter; typing narrows the cards.
                        status.clear();
                        filtering = true;
                    }
                    KeyCode::Char('m') => {
                        // Message any agent: a live one over its socket, a
                        // dormant one by resuming it first (via the router).
                        status.clear();
                        overlay = open_compose(&board, selected);
                    }
                    _ => {}
                },
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => selected = nav::move_row(selected, &counts, true),
                    MouseEventKind::ScrollUp => selected = nav::move_row(selected, &counts, false),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let s = terminal.size()?;
                        let area = Rect::new(0, 0, s.width, s.height);
                        // Footer buttons first (their own row), else a card.
                        if let Some(fa) = ui::footer_hit_test(area, m.column, m.row) {
                            match fa {
                                ui::FooterAction::Go => {
                                    if activate_selected(
                                        focuser.as_ref(),
                                        &launcher,
                                        &board,
                                        selected,
                                        &mut status,
                                    ) && launcher_mode
                                    {
                                        break;
                                    }
                                }
                                ui::FooterAction::New => {
                                    if spawn_new(&launcher, &board, selected, &mut status)
                                        && launcher_mode
                                    {
                                        break;
                                    }
                                }
                                ui::FooterAction::Jump => {
                                    status.clear();
                                    filtering = true;
                                }
                                ui::FooterAction::Msg => {
                                    status.clear();
                                    overlay = open_compose(&board, selected);
                                }
                                ui::FooterAction::Delete => dismiss_selected(
                                    dir,
                                    focuser.as_ref(),
                                    &board,
                                    selected,
                                    &mut status,
                                ),
                                ui::FooterAction::Quit => break,
                            }
                        } else {
                            let scroll = std::array::from_fn(|i| list_states[i].offset());
                            if let Some(idx) = ui::hit_test(area, &board, m.column, m.row, scroll) {
                                match click_action(idx, selected) {
                                    Click::Select => selected = idx,
                                    Click::Go => {
                                        selected = idx;
                                        if activate_selected(
                                            focuser.as_ref(),
                                            &launcher,
                                            &board,
                                            selected,
                                            &mut status,
                                        ) && launcher_mode
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
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

/// A left click first selects a card; only a click on the already-selected
/// card goes to it. Keeps a stray click from teleporting the operator's
/// window. Extracted pure so the rule is unit-tested.
enum Click {
    Select,
    Go,
}

fn click_action(clicked: usize, selected: usize) -> Click {
    if clicked == selected {
        Click::Go
    } else {
        Click::Select
    }
}

/// Enter/click on the selected agent: focus a live window, or resume a dormant
/// session. Errors land in the status line. Returns whether an agent was
/// activated (so the launcher can exit on success).
fn activate_selected(
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    board: &Board,
    selected: usize,
    status: &mut String,
) -> bool {
    status.clear();
    let Some(agent) = board.selectable().get(selected).copied() else {
        return false;
    };
    match activate(agent, focuser, launcher) {
        Ok(()) => true,
        Err(e) => {
            *status = e;
            false
        }
    }
}

/// Go to an agent: focus a live window, or resume a dormant session. Shared by
/// the Enter key, a left click, and the `f` go-to picker.
fn activate(
    agent: &model::Agent,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
) -> Result<(), String> {
    match agent.origin {
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, &agent.resume_command) {
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), command, None)
                .map_err(|e| format!("resume: {e}")),
            _ => Err("resume: dormant record missing cwd/resume command".into()),
        },
    }
}

/// Open the compose overlay to message the selected agent, if any.
fn open_compose(board: &Board, selected: usize) -> Option<Overlay> {
    let a = board.selectable().get(selected).copied()?;
    let target = match a.origin {
        Origin::Live => Some(ComposeTarget::Live(a.socket_path.clone())),
        Origin::Dormant => match (&a.cwd, &a.resume_command) {
            (Some(cwd), Some(command)) => Some(ComposeTarget::Dormant {
                cwd: cwd.clone(),
                resume_command: command.clone(),
            }),
            _ => None,
        },
    }?;
    Some(Overlay::Compose(Compose {
        target,
        label: ui::focus_label(a),
        buf: String::new(),
    }))
}

/// Spawn a fresh agent of the selected card's kind, in its dir. The launch
/// command rides in the record (`spawn_command`), so the board spawns whatever
/// kind the selection is without naming any agent. An empty board has no
/// selection and so cannot spawn (agent #1 is started from a terminal).
fn spawn_new(launcher: &dyn Launcher, board: &Board, selected: usize, status: &mut String) -> bool {
    status.clear();
    let Some(agent) = board.selectable().get(selected).copied() else {
        *status = "spawn: no agent selected (start the first agent from a terminal)".into();
        return false;
    };
    let Some(command) = &agent.spawn_command else {
        *status = format!("spawn: {} announced no spawn command", agent.label);
        return false;
    };
    let cwd = launch::default_cwd(agent.cwd.as_deref());
    match launcher.launch(&cwd, command, None) {
        Ok(()) => true,
        Err(e) => {
            *status = format!("spawn: {e}");
            false
        }
    }
}

/// `d`: dismiss the selected agent. A live agent is closed by terminating its
/// agent process (which closes its terminal window and, via the agent's clean
/// shutdown, leaves a dormant resumable record). A dormant record is forgotten
/// by deleting its registry file. So `d` twice fully removes a session: close,
/// then forget.
fn dismiss_selected(
    dir: &Path,
    focuser: &dyn WindowFocuser,
    board: &Board,
    selected: usize,
    status: &mut String,
) {
    status.clear();
    let Some(agent) = board.selectable().get(selected).copied() else {
        return;
    };
    match agent.origin {
        Origin::Live => {
            *status = match focuser.close(agent) {
                Ok(()) => format!("closing {}", ui::focus_label(agent)),
                Err(e) => format!("close: {e}"),
            };
        }
        Origin::Dormant => {
            if let Some(id) = &agent.session_id {
                let file = dir.join(format!("{id}.json"));
                if let Err(e) = std::fs::remove_file(&file) {
                    *status = format!("dismiss: {e}");
                }
            }
        }
    }
}

/// Prune dormant records only when they have gone untouched past
/// `DORMANT_MAX_AGE`. Deletion is deliberately conservative: a record is
/// removed solely on age, never because it looks unresumable or fails to
/// parse. An unreadable or unfamiliar record is ignored (skipped from the
/// view), not deleted, so a schema change or a newer producer can never
/// destroy history. Live records (socket set) are never pruned. Returns the
/// surviving entries.
fn prune(dir: &Path, entries: Vec<RegistryEntry>) -> Vec<RegistryEntry> {
    entries
        .into_iter()
        .filter(|e| {
            if e.socket.is_some() {
                return true; // live: not ours to prune
            }
            let file = dir.join(format!("{}.json", e.session_id));
            let stale = std::fs::metadata(&file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > DORMANT_MAX_AGE);
            if stale {
                let _ = std::fs::remove_file(&file);
                return false;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_goes_only_on_the_already_selected_card() {
        assert!(matches!(click_action(3, 3), Click::Go));
        assert!(matches!(click_action(3, 1), Click::Select));
        assert!(matches!(click_action(0, 5), Click::Select));
    }
}
