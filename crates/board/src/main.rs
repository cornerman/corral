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
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

mod discovery;
mod focus;
mod launch;
mod mailbox;
mod model;
mod picker;
mod prompt;
mod ui;
mod watch;

use discovery::RegistryEntry;
use focus::{SwayFocuser, WindowFocuser};
use launch::{KittyLauncher, Launcher};
use mailbox::Message;
use model::{Board, Origin, State, Update};
use picker::Picker;

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
    let result = run(&mut terminal, &dir);
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

/// A message being routed to a directory that had no (or, with `force_new`, no
/// dedicated) agent yet: corral spawned one and waits for it to announce.
struct RouteState {
    spawned: bool,
    /// Sockets already live in the target dir before the spawn, so the newly
    /// spawned agent can be told apart and receive the message.
    pre: HashSet<PathBuf>,
}

/// The operator composing a message to a live agent (opened with `m`): the
/// target socket, a display label for the prompt, and the text so far.
struct Compose {
    socket: PathBuf,
    target: String,
    buf: String,
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
    // Some(_) while a picker overlay is open (`c` spawn dir, or `f`
    // focus). `picker_focus` holds the live agents behind the labels when the
    // picker is a focus picker; None means it is the spawn-dir picker.
    let mut picker: Option<Picker> = None;
    let mut picker_focus: Option<Vec<model::Agent>> = None;
    // Some(_) while the operator is composing a message (`m`) to a live agent.
    let mut compose: Option<Compose> = None;
    // Agent-initiated message routing (the outbox). `approval` holds the one
    // non-whitelisted message awaiting an operator decision; `approved` and
    // `deferred` record per-session allow-once / decide-later choices;
    // `routing` tracks spawns in flight to a target dir.
    let mut approval: Option<(PathBuf, Message)> = None;
    let mut approved: HashSet<String> = HashSet::new();
    let mut deferred: HashSet<String> = HashSet::new();
    let mut routing: HashMap<String, RouteState> = HashMap::new();
    let outbox = outbox_dir();
    let whitelist = whitelist_file();
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

        // Route agent-initiated messages: deliver whitelisted/approved ones,
        // raise the approval overlay for anything new. Cheap (a readdir), so
        // delivery is prompt once a spawned target announces.
        if picker.is_none() && compose.is_none() {
            if let (Some(ob), Some(wl)) = (&outbox, &whitelist) {
                route_outbox(
                    ob,
                    wl,
                    &board,
                    &launcher,
                    &mut routing,
                    &mut approval,
                    &approved,
                    &deferred,
                    &mut status,
                );
            }
        }

        let counts = column_counts(&board);
        let count: usize = counts.iter().sum();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        let ages: HashMap<PathBuf, String> = state_since
            .iter()
            .map(|(p, t)| (p.clone(), age_label(t.elapsed())))
            .collect();
        terminal.draw(|f| {
            ui::render(f, &board, selected, &status, &mut list_states, &ages);
            if let Some(p) = &picker {
                let verb = if picker_focus.is_some() {
                    "focus agent"
                } else {
                    "spawn agent"
                };
                ui::render_picker(f, p, verb);
            }
            if let Some(c) = &compose {
                ui::render_compose(f, &c.target, &c.buf);
            }
            if let Some((_, msg)) = &approval {
                ui::render_approval(f, msg);
            }
        })?;

        if event::poll(POLL)? {
            let ev = event::read()?;
            // The picker, when open, captures all input until it closes.
            if picker.is_some() {
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Esc => {
                                picker = None;
                                picker_focus = None;
                            }
                            KeyCode::Enter => {
                                if let Some(targets) = picker_focus.take() {
                                    // Focus picker: map the selected match back
                                    // to its agent and focus its window.
                                    let idx = picker.as_ref().and_then(Picker::selected_original);
                                    picker = None;
                                    if let Some(a) = idx.and_then(|i| targets.get(i)) {
                                        if let Err(e) = focuser.focus(a) {
                                            status = format!("focus: {e}");
                                        }
                                    }
                                } else {
                                    let dir = picker.as_ref().and_then(Picker::selected_dir);
                                    picker = None;
                                    if let Err(e) = dir.map_or(Ok(()), |d| {
                                        launcher.spawn(std::path::Path::new(&d))
                                    }) {
                                        status = format!("spawn: {e}");
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                if let Some(p) = picker.as_mut() {
                                    p.backspace();
                                }
                            }
                            KeyCode::Up => {
                                if let Some(p) = picker.as_mut() {
                                    p.up();
                                }
                            }
                            KeyCode::Down => {
                                if let Some(p) = picker.as_mut() {
                                    p.down();
                                }
                            }
                            KeyCode::Char(c) => {
                                if let Some(p) = picker.as_mut() {
                                    p.push(c);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                continue;
            }
            // The approval overlay captures all input until the operator
            // decides on the pending inter-agent message.
            if approval.is_some() {
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        let (file, msg) = {
                            let (f, m) = approval.as_ref().unwrap();
                            (f.clone(), m.clone())
                        };
                        match key.code {
                            // allow once (this session): route it now.
                            KeyCode::Char('a') => {
                                approved.insert(msg.id);
                                approval = None;
                            }
                            // allow always: persist the pair, then route.
                            KeyCode::Char('A') => {
                                if let Some(wl) = &whitelist {
                                    if let Err(e) =
                                        mailbox::whitelist_add(wl, &msg.from_cwd, &msg.target_dir)
                                    {
                                        status = format!("whitelist: {e}");
                                    }
                                }
                                approved.insert(msg.id);
                                approval = None;
                            }
                            // deny: drop the message.
                            KeyCode::Char('d') => {
                                let _ = std::fs::remove_file(&file);
                                routing.remove(&msg.id);
                                approval = None;
                            }
                            // later: leave it queued, stop asking this session.
                            KeyCode::Esc => {
                                deferred.insert(msg.id);
                                approval = None;
                            }
                            _ => {}
                        }
                    }
                }
                continue;
            }
            // The compose overlay captures all input until it closes.
            if let Some(c) = compose.as_mut() {
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Esc => compose = None,
                            KeyCode::Enter => {
                                let c = compose.take().unwrap();
                                let text = c.buf.trim();
                                if !text.is_empty() {
                                    status = match prompt::send_prompt(&c.socket, text) {
                                        Ok(()) => format!("sent to {}", c.target),
                                        Err(e) => format!("send: {e}"),
                                    };
                                }
                            }
                            KeyCode::Backspace => {
                                c.buf.pop();
                            }
                            KeyCode::Char(ch) => c.buf.push(ch),
                            _ => {}
                        }
                    }
                }
                continue;
            }
            match ev {
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
                    KeyCode::Enter => {
                        activate_selected(&focuser, &launcher, &board, selected, &mut status)
                    }
                    KeyCode::Char('d') => {
                        dismiss_selected(dir, &board, selected, &mut status);
                    }
                    KeyCode::Char('n') => {
                        status.clear();
                        let cwd = launch::default_cwd(board.selectable().get(selected).copied());
                        if let Err(e) = launcher.spawn(&cwd) {
                            status = format!("spawn: {e}");
                        }
                    }
                    KeyCode::Char('c') => {
                        status.clear();
                        picker = Some(Picker::new(crate::picker::gather_dirs(&board)));
                    }
                    KeyCode::Char('m') => {
                        // Message a live agent: deliver a prompt over its
                        // socket. Dormant agents have no socket to receive on.
                        status.clear();
                        if let Some(a) = board.selectable().get(selected).copied() {
                            if a.origin == Origin::Live {
                                compose = Some(Compose {
                                    socket: a.socket_path.clone(),
                                    target: focus_label(a),
                                    buf: String::new(),
                                });
                            }
                        }
                    }
                    KeyCode::Char('f') => {
                        // Fuzzy-focus: pick among live agents by title/cwd,
                        // faster than arrow nav when many are running.
                        status.clear();
                        let live: Vec<model::Agent> = board
                            .selectable()
                            .into_iter()
                            .filter(|a| a.origin == Origin::Live)
                            .cloned()
                            .collect();
                        if !live.is_empty() {
                            let labels = live.iter().map(focus_label).collect();
                            picker = Some(Picker::new(labels));
                            picker_focus = Some(live);
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
                        let scroll = std::array::from_fn(|i| list_states[i].offset());
                        if let Some(idx) = ui::hit_test(area, &board, m.column, m.row, scroll) {
                            selected = idx;
                            activate_selected(&focuser, &launcher, &board, selected, &mut status);
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
    let Some(agent) = board.selectable().get(selected).copied() else {
        return;
    };
    let result = match agent.origin {
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, &agent.resume) {
            (Some(cwd), Some(resume)) => launcher
                .resume(Path::new(cwd), resume)
                .map_err(|e| format!("resume: {e}")),
            _ => Err("resume: dormant record missing cwd/resume".into()),
        },
    };
    if let Err(e) = result {
        *status = e;
    }
}

/// Compact age like `8s`, `5m`, `2h`, `3d` for time-in-state display.
fn age_label(d: Duration) -> String {
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

/// Label for the `f` focus picker: the title and the cwd's last path segment.
fn focus_label(agent: &model::Agent) -> String {
    let title = agent.title.as_deref().unwrap_or("(unnamed)");
    let cwd = agent.cwd.as_deref().unwrap_or("?");
    let base = cwd.rsplit('/').next().unwrap_or(cwd);
    format!("{title} · {base}")
}

/// `d`: dismiss the selected dormant session by deleting its registry record.
/// A no-op on live agents (they are not the operator's to forget).
fn dismiss_selected(dir: &Path, board: &Board, selected: usize, status: &mut String) {
    status.clear();
    let Some(agent) = board.selectable().get(selected).copied() else {
        return;
    };
    if agent.origin != Origin::Dormant {
        return;
    }
    if let Some(id) = &agent.session_id {
        let file = dir.join(format!("{id}.json"));
        if let Err(e) = std::fs::remove_file(&file) {
            *status = format!("dismiss: {e}");
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

/// Route pending outbox messages. Deliver whitelisted or already-approved ones
/// to their target directory (spawning an agent there if needed); raise the
/// approval overlay for the first message that is neither. `deferred` messages
/// wait untouched until the next board run.
#[allow(clippy::too_many_arguments)]
fn route_outbox(
    outbox: &Path,
    whitelist: &Path,
    board: &Board,
    launcher: &dyn Launcher,
    routing: &mut HashMap<String, RouteState>,
    approval: &mut Option<(PathBuf, Message)>,
    approved: &HashSet<String>,
    deferred: &HashSet<String>,
    status: &mut String,
) {
    if approval.is_some() {
        return; // one decision at a time
    }
    let pending = mailbox::scan_outbox(outbox);
    // Forget spawn-tracking for messages that are no longer queued.
    let ids: HashSet<&str> = pending.iter().map(|(_, m)| m.id.as_str()).collect();
    routing.retain(|k, _| ids.contains(k.as_str()));

    for (file, msg) in pending {
        if deferred.contains(&msg.id) {
            continue;
        }
        let ok = approved.contains(&msg.id)
            || mailbox::is_whitelisted(whitelist, &msg.from_cwd, &msg.target_dir);
        if !ok {
            *approval = Some((file, msg));
            return; // ask the operator; the rest wait for the next tick
        }
        deliver(&file, &msg, board, launcher, routing, status);
    }
}

/// Deliver one authorized message: reuse the live agent in the target dir, or
/// spawn one (and, for `force_new`, always a dedicated agent) and wait for it
/// to announce before injecting.
fn deliver(
    file: &Path,
    msg: &Message,
    board: &Board,
    launcher: &dyn Launcher,
    routing: &mut HashMap<String, RouteState>,
    status: &mut String,
) {
    let in_dir = board.live_in_dir(&msg.target_dir);
    if !msg.force_new {
        if let Some(agent) = in_dir.first() {
            finish(file, &agent.socket_path, msg, routing, status);
            return;
        }
    }
    let r = routing.entry(msg.id.clone()).or_insert_with(|| RouteState {
        spawned: false,
        pre: in_dir.iter().map(|a| a.socket_path.clone()).collect(),
    });
    if !r.spawned {
        if let Err(e) = launcher.spawn(Path::new(&msg.target_dir)) {
            *status = format!("route spawn: {e}");
        }
        r.spawned = true;
        return; // the fresh agent will announce on a later tick
    }
    // Deliver to the agent that appeared after our spawn.
    if let Some(agent) = in_dir.iter().find(|a| !r.pre.contains(&a.socket_path)) {
        let sock = agent.socket_path.clone();
        finish(file, &sock, msg, routing, status);
    }
}

/// Inject a delivered message over the target socket and drop the mailbox file.
fn finish(
    file: &Path,
    socket: &Path,
    msg: &Message,
    routing: &mut HashMap<String, RouteState>,
    status: &mut String,
) {
    *status = match prompt::send_prompt(socket, &msg.tagged()) {
        Ok(()) => format!("routed to {}", msg.target_dir),
        Err(e) => format!("route send: {e}"),
    };
    let _ = std::fs::remove_file(file);
    routing.remove(&msg.id);
}

/// Column agent counts in board order: RequiresAction, Idle, Running, Dormant.
/// This matches `Board::selectable()`, so a flat index maps cleanly to
/// (column, row).
fn column_counts(board: &Board) -> [usize; 4] {
    [
        board.in_state(State::RequiresAction).len(),
        board.in_state(State::Idle).len(),
        board.in_state(State::Running).len(),
        board.dormant().len(),
    ]
}

/// Flat selectable index -> (column, row).
fn locate(index: usize, counts: &[usize; 4]) -> (usize, usize) {
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
fn flat(col: usize, row: usize, counts: &[usize; 4]) -> usize {
    counts[..col].iter().sum::<usize>() + row
}

/// Move within the current column (Up/Down), clamped to that column.
fn move_row(index: usize, counts: &[usize; 4], down: bool) -> usize {
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
fn move_col(index: usize, counts: &[usize; 4], right: bool) -> usize {
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
    fn age_label_scales_units() {
        assert_eq!(age_label(Duration::from_secs(8)), "8s");
        assert_eq!(age_label(Duration::from_secs(5 * 60)), "5m");
        assert_eq!(age_label(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(age_label(Duration::from_secs(3 * 86400)), "3d");
    }

    #[test]
    fn navigation_maps_flat_index_to_columns() {
        // RequiresAction=2, Idle=0, Running=1, Dormant=0. order: RA0, RA1, Run0.
        let counts = [2usize, 0, 1, 0];
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
