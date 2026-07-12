//! corral: an attention board for locally running pi agents.
//!
//! Discovers sessions from the registry under $HOME/.corral/registry/
//! (override with $CORRAL_REGISTRY_DIR): each `<sessionId>.json` names a
//! workdir-local ACP socket. Corral watches each live socket for its
//! running/idle/requires_action state,
//! and shows them in four columns. Enter or a mouse click focuses an agent's
//! window (sway), `n` spawns a new agent (kitty) in the selected agent's dir,
//! `c` opens a fuzzy picker to create one in another directory, `q` quits.
//! Up/Down (or scroll) move within a
//! column; Left/Right switch columns. Corral never drives an agent; it
//! routes the operator's attention.
//!
//! Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

mod discovery;
mod focus;
mod launch;
mod mailbox;
mod model;
mod nav;
mod notify;
mod picker;
mod prompt;
mod router;
mod ui;
mod watch;

use discovery::RegistryEntry;
use focus::{SwayFocuser, WindowFocuser};
use launch::{KittyLauncher, Launcher};
use model::{Board, Origin, Update};
use notify::{ApprovalNotifier, NotifySendNotifier};
use picker::Picker;
use router::Router;

const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(250);
/// A dormant record untouched for this long is pruned (its session file is
/// stale or abandoned). Measured from the registry file's mtime, which the
/// extension refreshes on every turn and on clean shutdown.
const DORMANT_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

fn main() {
    let Some(dir) = registry_dir() else {
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
    let result = run(&mut terminal, &dir);
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

/// A corral path: the `env` override if set, else `$HOME/.corral/<name>`.
/// `None` only when neither is available. All of corral's on-disk locations
/// share this shape (a well-known name under `~/.corral`, overridable for
/// tests and non-standard setups).
fn corral_path(env: &str, name: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(env) {
        return Some(PathBuf::from(v));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corral").join(name))
}

/// The registry directory: the session records corral discovers.
fn registry_dir() -> Option<PathBuf> {
    corral_path("CORRAL_REGISTRY_DIR", "registry")
}

/// The outbox directory the corral-announce `message_agent` tool writes to.
fn outbox_dir() -> Option<PathBuf> {
    corral_path("CORRAL_OUTBOX_DIR", "outbox")
}

/// The whitelist file of pre-authorized `(sender -> target)` dir pairs.
fn whitelist_file() -> Option<PathBuf> {
    corral_path("CORRAL_WHITELIST", "whitelist")
}

/// Where an operator-composed message (`m`) is delivered.
enum ComposeTarget {
    /// A live agent: deliver straight to its socket.
    Live(PathBuf),
    /// A dormant session (by id): resume it, then deliver.
    Dormant(String),
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
    /// `/`: fuzzy-pick any agent to go to (Enter) or spawn beside (Shift+Enter).
    Jump(Picker, Vec<model::Agent>),
    /// `m`: compose a message to a live agent.
    Compose(Compose),
}

/// The outcome of a key press inside a picker overlay.
enum PickerInput {
    /// The query or selection changed; keep the picker open.
    Continue,
    /// Esc: close without acting.
    Cancel,
    /// Enter: go to the current selection, then close.
    Submit,
    /// Shift+Enter: spawn a new agent in the selection's dir, then close.
    SubmitSpawn,
}

fn picker_input(p: &mut Picker, key: KeyEvent) -> PickerInput {
    match key.code {
        KeyCode::Esc => PickerInput::Cancel,
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => PickerInput::SubmitSpawn,
        KeyCode::Enter => PickerInput::Submit,
        KeyCode::Up => {
            p.up();
            PickerInput::Continue
        }
        KeyCode::Down => {
            p.down();
            PickerInput::Continue
        }
        KeyCode::Backspace => {
            p.backspace();
            PickerInput::Continue
        }
        KeyCode::Char(c) => {
            p.push(c);
            PickerInput::Continue
        }
        _ => PickerInput::Continue,
    }
}

/// Feed one event to the open overlay. Returns the overlay to keep it open, or
/// `None` once it has closed (cancelled or acted).
fn handle_overlay(
    mut ov: Overlay,
    ev: Event,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    router: Option<&mut Router>,
    status: &mut String,
) -> Option<Overlay> {
    let Event::Key(key) = ev else {
        return Some(ov);
    };
    if key.kind != KeyEventKind::Press {
        return Some(ov);
    }
    match &mut ov {
        Overlay::Jump(p, targets) => match picker_input(p, key) {
            PickerInput::Continue => Some(ov),
            PickerInput::Cancel => None,
            PickerInput::Submit => {
                if let Some(a) = p.selected_original().and_then(|i| targets.get(i)) {
                    if let Err(e) = activate(a, focuser, launcher) {
                        *status = e;
                    }
                }
                None
            }
            PickerInput::SubmitSpawn => {
                if let Some(a) = p.selected_original().and_then(|i| targets.get(i)) {
                    let cwd = launch::default_cwd(Some(a));
                    if let Err(e) = launcher.spawn(&cwd) {
                        *status = format!("spawn: {e}");
                    }
                }
                None
            }
        },
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
                        // Dormant: hand it to the router, which resumes the
                        // session and delivers once it announces.
                        ComposeTarget::Dormant(session_id) => match router {
                            Some(r) => {
                                r.operator_send(session_id.clone(), text.to_string());
                                *status = format!("resuming {} to deliver", c.label);
                            }
                            None => *status = "send: unavailable (no HOME)".into(),
                        },
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

fn run(terminal: &mut ratatui::DefaultTerminal, dir: &std::path::Path) -> std::io::Result<()> {
    let (tx, rx): (Sender<Update>, Receiver<Update>) = mpsc::channel();
    let focuser = SwayFocuser;
    let launcher = KittyLauncher;

    let mut board = Board::default();
    let mut known: HashSet<PathBuf> = HashSet::new();
    // Sockets whose watcher failed to connect. A record whose socket is set
    // but dead is a crashed session: surfaced as dormant (resumable) rather
    // than vanishing. Cleared when the socket comes alive (an Upsert).
    let mut dead_sockets: HashSet<PathBuf> = HashSet::new();
    let mut selected: usize = 0;
    let mut status = String::new();
    // The active input overlay, if any (`c` spawn dir, `f` focus agent, `m`
    // compose a message). One at a time by construction.
    let mut overlay: Option<Overlay> = None;
    // Agent-initiated message routing (the outbox). The router owns its state;
    // None when there is no home to resolve the outbox/whitelist under.
    let mut router = outbox_dir()
        .zip(whitelist_file())
        .map(|(o, w)| Router::new(o, w));
    // A pending approval is mirrored to a desktop notification; its buttons
    // report back here. `notified` tracks which message id already has one, so
    // a notification fires once per pending message.
    let notifier = NotifySendNotifier;
    let (napp_tx, napp_rx) = mpsc::channel::<(String, ui::ApprovalAction)>();
    let mut notified: Option<String> = None;
    // Scroll offset for the pending approval's (possibly long) message body.
    let mut approval_scroll: u16 = 0;
    // One persistent ListState per column so ratatui scrolls long columns and
    // hit_test can read each column's scroll offset.
    let mut list_states: [ListState; 4] = std::array::from_fn(|_| ListState::default());
    // When each live agent entered its current state, keyed by socket path, so
    // the cards can show time-in-state.
    let mut state_since: HashMap<PathBuf, Instant> = HashMap::new();
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
                    dead_sockets.insert(path.clone());
                }
                // Each SetState is a real transition (the extension only
                // broadcasts on change): restart the timer.
                Update::SetState(path, _) => {
                    state_since.insert(path.clone(), Instant::now());
                }
                Update::Upsert(a) => {
                    state_since
                        .entry(a.socket_path.clone())
                        .or_insert_with(Instant::now);
                    dead_sockets.remove(&a.socket_path);
                }
                Update::SetTitle(..) => {}
            }
            board.apply(update);
        }

        // Route agent-initiated messages when no overlay is capturing input.
        // Cheap (a readdir), so delivery is prompt once a spawned target
        // announces.
        if overlay.is_none() {
            if let Some(r) = router.as_mut() {
                if let Some(s) = r.poll(&board, &launcher) {
                    status = s;
                }
            }
        }

        // Mirror a newly pending approval to a desktop notification, and apply
        // any decision its buttons send back (ignoring stale ids).
        match router.as_ref().and_then(Router::pending) {
            Some(msg) if notified.as_deref() != Some(&msg.id) => {
                notifier.notify(
                    msg.id.clone(),
                    &msg.from_cwd,
                    &msg.target_label(),
                    &msg.message,
                    napp_tx.clone(),
                );
                notified = Some(msg.id.clone());
                approval_scroll = 0; // fresh message starts at the top
            }
            None => notified = None,
            _ => {}
        }
        while let Ok((id, action)) = napp_rx.try_recv() {
            if let Some(r) = router.as_mut() {
                if r.pending().map(|m| m.id.as_str()) == Some(id.as_str()) {
                    apply_approval(r, action, &mut status);
                }
            }
        }

        let counts = board.column_counts();
        let count: usize = counts.iter().sum();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        let ages: HashMap<PathBuf, String> = state_since
            .iter()
            .map(|(p, t)| (p.clone(), ui::age_label(t.elapsed())))
            .collect();
        terminal.draw(|f| {
            ui::render(f, &board, selected, &status, &mut list_states, &ages);
            match &overlay {
                Some(Overlay::Jump(p, _)) => ui::render_picker(f, p),
                Some(Overlay::Compose(c)) => ui::render_compose(f, &c.label, &c.buf),
                None => {}
            }
            if let Some(msg) = router.as_ref().and_then(Router::pending) {
                ui::render_approval(f, msg, approval_scroll);
            }
        })?;

        if event::poll(POLL)? {
            let ev = event::read()?;
            // The approval overlay captures all input until the operator
            // decides on the pending inter-agent message. Enter = allow once,
            // a = allow always (persist), Esc = deny; or click a button.
            if let Some(r) = router.as_mut().filter(|r| r.pending().is_some()) {
                let action = match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Enter => Some(ui::ApprovalAction::AllowOnce),
                        KeyCode::Char('a') => Some(ui::ApprovalAction::AllowAlways),
                        KeyCode::Esc => Some(ui::ApprovalAction::Deny),
                        // Up/Down scroll the message body, not a decision.
                        KeyCode::Up | KeyCode::Char('k') => {
                            approval_scroll = approval_scroll.saturating_sub(1);
                            None
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            approval_scroll = approval_scroll.saturating_add(1);
                            None
                        }
                        _ => None,
                    },
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let s = terminal.size()?;
                            let area = Rect::new(0, 0, s.width, s.height);
                            ui::approval_hit_test(area, m.column, m.row)
                        }
                        MouseEventKind::ScrollUp => {
                            approval_scroll = approval_scroll.saturating_sub(1);
                            None
                        }
                        MouseEventKind::ScrollDown => {
                            approval_scroll = approval_scroll.saturating_add(1);
                            None
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(a) = action {
                    apply_approval(r, a, &mut status);
                }
                continue;
            }
            // Any open overlay captures all input until it closes.
            if let Some(ov) = overlay.take() {
                overlay = handle_overlay(ov, ev, &focuser, &launcher, router.as_mut(), &mut status);
                continue;
            }
            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
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
                        // Shift+Enter: spawn a new agent in the selected dir.
                        status.clear();
                        let cwd = launch::default_cwd(board.selectable().get(selected).copied());
                        if let Err(e) = launcher.spawn(&cwd) {
                            status = format!("spawn: {e}");
                        }
                    }
                    KeyCode::Enter => {
                        activate_selected(&focuser, &launcher, &board, selected, &mut status)
                    }
                    KeyCode::Char('d') => {
                        dismiss_selected(dir, &focuser, &board, selected, &mut status);
                    }
                    KeyCode::Char('/') => {
                        // Fuzzy-pick any agent: Enter goes to it, Shift+Enter
                        // spawns a fresh agent in its dir.
                        status.clear();
                        let targets: Vec<model::Agent> =
                            board.selectable().into_iter().cloned().collect();
                        if !targets.is_empty() {
                            let labels = targets.iter().map(goto_label).collect();
                            overlay = Some(Overlay::Jump(Picker::new(labels), targets));
                        }
                    }
                    KeyCode::Char('m') => {
                        // Message any agent: a live one over its socket, a
                        // dormant one by resuming it first (via the router).
                        status.clear();
                        if let Some(a) = board.selectable().get(selected).copied() {
                            let target = match a.origin {
                                Origin::Live => Some(ComposeTarget::Live(a.socket_path.clone())),
                                Origin::Dormant => a.session_id.clone().map(ComposeTarget::Dormant),
                            };
                            if let Some(target) = target {
                                overlay = Some(Overlay::Compose(Compose {
                                    target,
                                    label: ui::focus_label(a),
                                    buf: String::new(),
                                }));
                            }
                        }
                    }
                    _ => {}
                },
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollDown => selected = nav::move_row(selected, &counts, true),
                    MouseEventKind::ScrollUp => selected = nav::move_row(selected, &counts, false),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let s = terminal.size()?;
                        let area = Rect::new(0, 0, s.width, s.height);
                        let scroll = std::array::from_fn(|i| list_states[i].offset());
                        if let Some(idx) = ui::hit_test(area, &board, m.column, m.row, scroll) {
                            match click_action(idx, selected) {
                                Click::Select => selected = idx,
                                Click::Go => {
                                    selected = idx;
                                    activate_selected(
                                        &focuser, &launcher, &board, selected, &mut status,
                                    );
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
/// session. Errors land in the status line.
fn activate_selected(
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    board: &Board,
    selected: usize,
    status: &mut String,
) {
    status.clear();
    if let Some(agent) = board.selectable().get(selected).copied() {
        if let Err(e) = activate(agent, focuser, launcher) {
            *status = e;
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
        Origin::Dormant => match (&agent.cwd, &agent.resume) {
            (Some(cwd), Some(resume)) => launcher
                .resume(Path::new(cwd), resume)
                .map_err(|e| format!("resume: {e}")),
            _ => Err("resume: dormant record missing cwd/resume".into()),
        },
    }
}

/// Picker label for the `f` go-to list: the focus label, marked when dormant
/// so the operator knows Enter will resume rather than focus.
fn goto_label(agent: &model::Agent) -> String {
    match agent.origin {
        Origin::Live => ui::focus_label(agent),
        Origin::Dormant => format!("{} (dormant)", ui::focus_label(agent)),
    }
}

/// Apply an approval decision to the router. Shared by the in-board dialog
/// (keys/click) and the desktop notification's buttons.
fn apply_approval(router: &mut Router, action: ui::ApprovalAction, status: &mut String) {
    match action {
        ui::ApprovalAction::AllowOnce => router.allow_once(),
        ui::ApprovalAction::AllowAlways => {
            if let Err(e) = router.allow_always() {
                *status = format!("whitelist: {e}");
            }
        }
        ui::ApprovalAction::Deny => router.deny(),
    }
}

/// `d`: dismiss the selected agent. A live agent is closed by terminating its
/// pi process (which closes its `kitty -e pi` window and, via pi's clean
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

/// Prune dormant records whose resume target is gone or that have not been
/// touched in `DORMANT_MAX_AGE`. Live records (socket set) are never pruned.
/// Returns the surviving entries.
fn prune(dir: &Path, entries: Vec<RegistryEntry>) -> Vec<RegistryEntry> {
    entries
        .into_iter()
        .filter(|e| {
            if e.socket.is_some() {
                return true; // live: not ours to prune
            }
            let dead = e.resume.as_deref().is_none_or(|r| !Path::new(r).exists());
            let file = dir.join(format!("{}.json", e.session_id));
            let stale = std::fs::metadata(&file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > DORMANT_MAX_AGE);
            if dead || stale {
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

    #[test]
    fn shift_enter_in_picker_is_spawn() {
        let mut p = Picker::new(vec!["a".into()]);
        let plain = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let shift = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert!(matches!(picker_input(&mut p, plain), PickerInput::Submit));
        assert!(matches!(
            picker_input(&mut p, shift),
            PickerInput::SubmitSpawn
        ));
    }
}
