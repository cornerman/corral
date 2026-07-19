//! corral: an attention board for locally running coding-agent sessions.
//!
//! Discovers sessions from the registry under $HOME/.corral/registry/
//! (override with $CORRAL_REGISTRY_DIR): each `<sessionId>.json` names a
//! workdir-local ACP socket. Corral watches each live socket for its
//! running/idle/requires_action state,
//! and shows them in four columns. Enter or a double-click focuses an agent's
//! window (a single click just selects; right-click opens a context menu of the
//! footer actions), Shift+Enter spawns a new agent in the selected agent's dir, `/`
//! focuses the inline filter, `m` composes a message, `d` dismisses, `q`
//! quits (Esc peels one layer per press but never exits — q is the sole quit).
//! `--launcher` opens an ephemeral popup that exits after a go/spawn, or on a
//! single Esc. Up/Down (or scroll) move
//! within a column; Left/Right switch columns. Corral never drives an agent;
//! it routes the operator's attention.
//!
//! Not $XDG_RUNTIME_DIR: sandboxed agents cannot reach it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, ModifierKeyCode, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

mod ui;

use corral_core::click::{ClickKind, ClickTracker};
use corral_core::engine::Engine;
use corral_core::focus::{self, WindowFocuser};
use corral_core::launch::{self, LaunchMode, Launcher, TerminalLauncher};
use corral_core::menu::MenuAction;
use corral_core::model::{Board, Column, Origin};
use corral_core::placement::{apply_placement, kill_pid};
use corral_core::prompt;
use corral_core::transition::{self, MoveAction};
use corral_core::{model, nav, paths};

const POLL: Duration = Duration::from_millis(250);

fn main() {
    // Viewers read only corrald's vetted registry (security design: viewers
    // never touch agent-writable records).
    let Some(dir) = paths::state_registry_dir() else {
        eprintln!("corral: set $CORRAL_STATE_DIR or $HOME");
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
    let result = run(&mut terminal, &dir, launcher_mode, enhanced);
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
        /// Launch options of the resumed agent (gui + message flag).
        mode: LaunchMode,
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

/// The open right-click context menu: where it is anchored and which entry is
/// highlighted. The menu always acts on the board's current `selected` (a
/// right-click selects the card under the cursor before opening).
struct Menu {
    anchor: (u16, u16),
    highlight: usize,
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
                            mode,
                        } => {
                            *status = match launcher.launch(
                                Path::new(cwd),
                                resume_command,
                                Some(text),
                                mode,
                            ) {
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

/// A committed-but-unconfirmed card move: the destination column and when it
/// was fired. The card stays in its real column with an in-flight badge until
/// the agent's own state reaches `target` (or this times out).
const PENDING_TTL: Duration = Duration::from_secs(5);

/// Push the extra keyboard flags move mode needs: report event *types* (so we
/// see the shift-key release that commits) and all keys as escape codes (so the
/// modifier key itself is reported). Scoped to move mode via push/pop so normal
/// input is untouched. Only meaningful where the terminal supports enhancement.
fn push_move_flags(enhanced: bool) {
    if enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
    }
}

fn pop_move_flags(enhanced: bool) {
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Fire the real agent action a card move triggers (see `core::transition`).
/// Returns the (agent key, target) to record as pending on success, or None for
/// a no-op / failure (the status line carries any error).
fn commit_move(
    source: Column,
    target: Column,
    agent: &model::Agent,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    status: &mut String,
) -> Option<(String, Column)> {
    let key = ui::agent_key(agent);
    let label = ui::focus_label(agent);
    let result: Result<(), String> = match transition::action_for(source, target) {
        // Stop the turn (also unblocks a pending question).
        MoveAction::Cancel => {
            prompt::send_cancel(&agent.socket_path).map_err(|e| format!("cancel: {e}"))
        }
        // Start a turn with the literal nudge.
        MoveAction::Nudge => {
            prompt::send_prompt(&agent.socket_path, "continue").map_err(|e| format!("nudge: {e}"))
        }
        // Kill the agent: a hidden one by pid, a visible one via its window
        // (same as `d`); pi goes dormant and resumable.
        MoveAction::Kill => {
            if agent.hidden {
                kill_pid(agent.pid).map_err(|e| format!("close: {e}"))
            } else {
                focuser.close(agent).map_err(|e| format!("close: {e}"))
            }
        }
        // Resume the dormant session; ResumeAndNudge also delivers the nudge as
        // its first prompt so it lands running rather than idle.
        MoveAction::Resume | MoveAction::ResumeAndNudge => {
            match (&agent.cwd, agent.resume_argv()) {
                (Some(cwd), Some(cmd)) => {
                    let msg = matches!(
                        transition::action_for(source, target),
                        MoveAction::ResumeAndNudge
                    )
                    .then_some("continue");
                    launcher
                        .launch(Path::new(cwd), &cmd, msg, &agent.launch_mode())
                        .map_err(|e| format!("resume: {e}"))
                }
                _ => Err("resume: dormant record missing cwd/command".into()),
            }
        }
        MoveAction::NoOp => return None,
    };
    match result {
        Ok(()) => {
            *status = format!("moving {label} → {}", target.title());
            Some((key, target))
        }
        Err(e) => {
            *status = e;
            None
        }
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    dir: &std::path::Path,
    launcher_mode: bool,
    enhanced: bool,
) -> std::io::Result<()> {
    // EWMH on X11, sway on Wayland (until other Wayland focusers land).
    let focuser = focus::detect();
    let launcher = TerminalLauncher;

    // The shared registry-reflect loop (scan/prune/watch/drain/age timers),
    // the same engine the GUI runs on, so the two shells cannot drift.
    let mut engine = Engine::new(dir.to_path_buf());
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
    // The open right-click context menu, if any: its cursor anchor and the
    // highlighted entry. Captures input while open.
    let mut menu: Option<Menu> = None;
    // Classifies left clicks into select (single) vs go (double) on the same
    // card within the double-click window.
    let mut clicks = ClickTracker::default();
    // The in-progress card move (shift+arrow, or a mouse drag): the source
    // column and the ghost target the operator is choosing. The moving agent is
    // the current `selected`. `None` when not moving.
    let mut move_mode: Option<(Column, Column)> = None;
    // Source column of a started mouse drag (set on left-press over a card),
    // used to begin a move once the drag crosses into another column.
    let mut drag_source: Option<Column> = None;
    // Committed-but-unconfirmed moves, keyed by agent key (see `ui::agent_key`):
    // the target column and when fired. Rendered as an in-flight badge; cleared
    // when the agent reaches the target or after `PENDING_TTL`.
    let mut pending: HashMap<String, (Column, Instant)> = HashMap::new();
    // Inter-agent messaging lives entirely in the corrald daemon now; the board
    // is a pure viewer of the registry plus the operator's own actions (focus,
    // spawn, resume, and the ungated `m` message to a selected agent).
    // One persistent ListState per column so ratatui scrolls long columns and
    // hit_test can read each column's scroll offset.
    let mut list_states: [ListState; 4] = std::array::from_fn(|_| ListState::default());

    loop {
        // Scan/prune/watch on the engine's cadence, then drain watcher updates
        // into the board and its age timers (shared with the GUI shell).
        engine.tick();
        engine.set_filter(filter.clone());
        let board = engine.board();
        // Reconcile pending moves against the live board: a move confirms when
        // the agent reaches its target column (the board never fakes the move);
        // otherwise it expires after PENDING_TTL. Keyed by agent key so a
        // resume (new socket) still matches by session id.
        let by_key: HashMap<String, Column> = board
            .selectable()
            .iter()
            .map(|a| (ui::agent_key(a), a.column()))
            .collect();
        pending.retain(|k, (target, since)| {
            by_key.get(k) != Some(target) && since.elapsed() <= PENDING_TTL
        });
        let pending_labels: HashMap<String, String> = pending
            .iter()
            .map(|(k, (t, _))| (k.clone(), t.title().to_string()))
            .collect();

        let counts = board.column_counts();
        let count: usize = counts.iter().sum();
        if selected >= count {
            selected = count.saturating_sub(1);
        }

        let in_state = engine.in_state_ages();
        let quiet = engine.quiet_ages();
        let meta = ui::CardMeta {
            in_state: &in_state,
            quiet: &quiet,
            dormant_age: engine.dormant_ages(),
            pending: &pending_labels,
        };
        let move_label =
            move_mode.and_then(|_| board.selectable().get(selected).map(|a| ui::focus_label(a)));
        terminal.draw(|f| {
            ui::render(f, board, selected, &status, &mut list_states, &meta);
            // Move mode owns the screen (drop-boxes over the columns); the
            // filter/overlay/menu are all closed while moving.
            if let Some((src, target)) = &move_mode {
                ui::render_move(f, *src, *target, move_label.as_deref().unwrap_or(""));
            } else {
                ui::render_filter(f, &filter, filtering);
                match &overlay {
                    Some(Overlay::Compose(c)) => ui::render_compose(f, &c.label, &c.buf),
                    None => {}
                }
                if let Some(m) = &menu {
                    ui::render_menu(f, m.anchor, m.highlight);
                }
            }
        })?;

        if event::poll(POLL)? {
            let ev = event::read()?;
            // A card move in progress captures all input: shift+arrow (or a
            // mouse drag) slides the ghost target; shift-release, Enter, or a
            // mouse drop commits; Esc or a right-click cancels. Committing fires
            // the real agent action and records it pending until confirmed.
            if let Some((source, mut target)) = move_mode {
                let mut commit = false;
                let mut cancel = false;
                match ev {
                    Event::Key(key) => match (key.code, key.kind) {
                        // The shift key released: drop where the ghost rests.
                        (
                            KeyCode::Modifier(
                                ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift,
                            ),
                            KeyEventKind::Release,
                        ) => commit = true,
                        (KeyCode::Enter, KeyEventKind::Press) => commit = true,
                        (KeyCode::Esc, KeyEventKind::Press) => cancel = true,
                        (KeyCode::Left, KeyEventKind::Press) => {
                            target = transition::slide_target(source, target, false)
                        }
                        (KeyCode::Right, KeyEventKind::Press) => {
                            target = transition::slide_target(source, target, true)
                        }
                        _ => {}
                    },
                    Event::Mouse(m) => match m.kind {
                        // Drag retargets to the column under the cursor when it
                        // is a valid stop (a destination, or the source itself =
                        // drop to cancel); an invalid column (Requires Action
                        // that is not the source) is ignored, keeping the last
                        // target.
                        MouseEventKind::Drag(MouseButton::Left) => {
                            let s = terminal.size()?;
                            let area = Rect::new(0, 0, s.width, s.height);
                            if let Some(c) = ui::column_at(area, m.column) {
                                if transition::stops(source).contains(&c) {
                                    target = c;
                                }
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => commit = true,
                        MouseEventKind::Down(MouseButton::Right) => cancel = true,
                        _ => {}
                    },
                    _ => {}
                }
                if commit {
                    if let Some(agent) = board.selectable().get(selected).copied() {
                        if let Some((key, tgt)) = commit_move(
                            source,
                            target,
                            agent,
                            focuser.as_ref(),
                            &launcher,
                            &mut status,
                        ) {
                            pending.insert(key, (tgt, Instant::now()));
                        }
                    }
                    move_mode = None;
                    drag_source = None;
                    pop_move_flags(enhanced);
                } else if cancel {
                    move_mode = None;
                    drag_source = None;
                    status.clear();
                    pop_move_flags(enhanced);
                } else {
                    move_mode = Some((source, target));
                }
                continue;
            }
            // An open context menu captures all input: arrows move the
            // highlight, Enter/left-click runs an entry, Esc/outside-click
            // closes. The chosen action runs on the board's current selection
            // (a right-click selected the card before opening).
            if let Some(mut m) = menu.take() {
                let mut chosen: Option<MenuAction> = None;
                let mut keep = true;
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Esc => keep = false,
                        KeyCode::Up => {
                            m.highlight =
                                (m.highlight + MenuAction::ALL.len() - 1) % MenuAction::ALL.len();
                        }
                        KeyCode::Down => {
                            m.highlight = (m.highlight + 1) % MenuAction::ALL.len();
                        }
                        KeyCode::Enter => chosen = Some(MenuAction::ALL[m.highlight]),
                        _ => {}
                    },
                    Event::Mouse(me) => match me.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let s = terminal.size()?;
                            let area = Rect::new(0, 0, s.width, s.height);
                            let rect = ui::menu_rect(area, m.anchor);
                            match ui::menu_hit_test(rect, me.column, me.row) {
                                Some(i) => chosen = Some(MenuAction::ALL[i]),
                                None => keep = false, // click outside dismisses
                            }
                        }
                        MouseEventKind::Down(MouseButton::Right) => keep = false,
                        _ => {}
                    },
                    _ => {}
                }
                if let Some(action) = chosen {
                    // Menu closed; run the same path as the footer/key.
                    match action {
                        MenuAction::Go => {
                            if activate_selected(
                                focuser.as_ref(),
                                &launcher,
                                board,
                                selected,
                                &mut status,
                            ) && launcher_mode
                            {
                                break;
                            }
                        }
                        MenuAction::Spawn => {
                            if spawn_new(&launcher, board, selected, &mut status) && launcher_mode {
                                break;
                            }
                        }
                        MenuAction::Message => {
                            status.clear();
                            overlay = open_compose(board, selected);
                        }
                        MenuAction::ToggleHidden => toggle_selected(
                            focuser.as_ref(),
                            &launcher,
                            board,
                            selected,
                            &mut status,
                        ),
                        MenuAction::Dismiss => {
                            dismiss_selected(focuser.as_ref(), board, selected, &mut status);
                        }
                    }
                } else if keep {
                    menu = Some(m);
                }
                continue;
            }
            // Filter edit mode: printable keys edit the query, arrows still
            // navigate, Enter keeps it, Esc leaves filter mode (never quits).
            if filtering {
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            // Launcher: Esc dismisses the popup at once (a
                            // throwaway summon, single press to bail). Normal:
                            // Esc peels one layer, leaving edit mode but keeping
                            // the query (a second Esc then clears it).
                            KeyCode::Esc if launcher_mode => break,
                            KeyCode::Esc => filtering = false,
                            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                if spawn_new(&launcher, board, selected, &mut status)
                                    && launcher_mode
                                {
                                    break;
                                }
                            }
                            KeyCode::Enter => {
                                if activate_selected(
                                    focuser.as_ref(),
                                    &launcher,
                                    board,
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
                            // Down/Up step off the input into the board (the
                            // input is the single ring node above the board's
                            // first card and below its last): Down lands on the
                            // first card, Up on the last. Leaving edit mode makes
                            // m/d/h reachable as commands. No-op on empty board.
                            KeyCode::Down if counts.iter().sum::<usize>() > 0 => {
                                filtering = false;
                                selected = nav::board_entry(selected, &counts, true);
                            }
                            KeyCode::Up if counts.iter().sum::<usize>() > 0 => {
                                filtering = false;
                                selected = nav::board_entry(selected, &counts, false);
                            }
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
                    // Launcher: Esc dismisses the popup at once. Normal: Esc
                    // only clears a non-empty filter and otherwise does nothing
                    // — it never exits (q is the sole quit), so a stray Esc can
                    // never nuke the persistent board. The selection stays put
                    // (a smaller filtered set means its index is still in range
                    // once the full board returns). Matches the GUI so both
                    // shells behave alike.
                    KeyCode::Esc if launcher_mode => break,
                    KeyCode::Esc => {
                        filter.clear();
                    }
                    // Down/Up flow across the whole board (a column's last card
                    // into the next column's first); only at the very last card
                    // (Down) or first card (Up) of the board ring to the input.
                    KeyCode::Down => {
                        if nav::at_board_edge(selected, &counts, true) {
                            filtering = true;
                        } else {
                            selected = nav::move_selection(selected, &counts, true);
                        }
                    }
                    KeyCode::Up => {
                        if nav::at_board_edge(selected, &counts, false) {
                            filtering = true;
                        } else {
                            selected = nav::move_selection(selected, &counts, false);
                        }
                    }
                    // Shift+Left/Right grab the selected card into move mode
                    // (drive its state by moving it between columns); plain
                    // Left/Right just switch the selected column.
                    KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(a) = board.selectable().get(selected).copied() {
                            let source = a.column();
                            move_mode = Some((source, transition::initial_target(source, false)));
                            push_move_flags(enhanced);
                        }
                    }
                    KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(a) = board.selectable().get(selected).copied() {
                            let source = a.column();
                            move_mode = Some((source, transition::initial_target(source, true)));
                            push_move_flags(enhanced);
                        }
                    }
                    KeyCode::Left => {
                        selected = nav::move_col(selected, &counts, false);
                    }
                    KeyCode::Right => {
                        selected = nav::move_col(selected, &counts, true);
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if spawn_new(&launcher, board, selected, &mut status) && launcher_mode {
                            break;
                        }
                    }
                    KeyCode::Enter => {
                        if activate_selected(
                            focuser.as_ref(),
                            &launcher,
                            board,
                            selected,
                            &mut status,
                        ) && launcher_mode
                        {
                            break;
                        }
                    }
                    KeyCode::Char('d') => {
                        dismiss_selected(focuser.as_ref(), board, selected, &mut status);
                    }
                    KeyCode::Char('h') => {
                        toggle_selected(focuser.as_ref(), &launcher, board, selected, &mut status);
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
                        overlay = open_compose(board, selected);
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
                                        board,
                                        selected,
                                        &mut status,
                                    ) && launcher_mode
                                    {
                                        break;
                                    }
                                }
                                ui::FooterAction::New => {
                                    if spawn_new(&launcher, board, selected, &mut status)
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
                                    overlay = open_compose(board, selected);
                                }
                                ui::FooterAction::Delete => {
                                    dismiss_selected(focuser.as_ref(), board, selected, &mut status)
                                }
                                ui::FooterAction::Toggle => toggle_selected(
                                    focuser.as_ref(),
                                    &launcher,
                                    board,
                                    selected,
                                    &mut status,
                                ),
                                ui::FooterAction::Quit => break,
                            }
                        } else if ui::filter_hit_test(area, m.column, m.row) {
                            // Click on the filter field focuses it (GUI parity).
                            status.clear();
                            filtering = true;
                        } else {
                            let scroll = std::array::from_fn(|i| list_states[i].offset());
                            if let Some(idx) = ui::hit_test(area, board, m.column, m.row, scroll) {
                                // Single click selects; a double click on the
                                // same card goes (focus/reveal/resume).
                                selected = idx;
                                // Arm a possible drag: remember the card's
                                // column so a drag into another column begins a
                                // move (see the Drag arm).
                                drag_source = board.selectable().get(idx).map(|a| a.column());
                                if let ClickKind::Go = clicks.press(idx, Instant::now()) {
                                    if activate_selected(
                                        focuser.as_ref(),
                                        &launcher,
                                        board,
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
                    // Dragging an armed card into a different, valid
                    // destination column begins a move; the move-mode handler
                    // above then owns the drag until the drop.
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(source) = drag_source {
                            let s = terminal.size()?;
                            let area = Rect::new(0, 0, s.width, s.height);
                            // Crossing into a different valid destination begins
                            // the move; intra-column jitter or a drag onto an
                            // invalid column (Requires Action) does not (so a
                            // plain click is never a move).
                            if let Some(c) = ui::column_at(area, m.column) {
                                if c != source && transition::DESTINATIONS.contains(&c) {
                                    move_mode = Some((source, c));
                                }
                            }
                        }
                    }
                    // A left-release with no move started was just a click; drop
                    // the drag arming.
                    MouseEventKind::Up(MouseButton::Left) => drag_source = None,
                    // Right click a card: select it, then open the context menu
                    // anchored at the cursor. Empty space does nothing.
                    MouseEventKind::Down(MouseButton::Right) => {
                        let s = terminal.size()?;
                        let area = Rect::new(0, 0, s.width, s.height);
                        let scroll = std::array::from_fn(|i| list_states[i].offset());
                        if let Some(idx) = ui::hit_test(area, board, m.column, m.row, scroll) {
                            selected = idx;
                            menu = Some(Menu {
                                anchor: (m.column, m.row),
                                highlight: 0,
                            });
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

/// Enter/double-click on the selected agent: focus a live window, or resume a dormant
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

/// `h`/footer click on the selected agent: toggle placement (hide a visible
/// agent, reveal a hidden one, or start a dormant one hidden). Always
/// kill-and-resume (no live surface migration). Shared by the key and the
/// clickable footer hint.
fn toggle_selected(
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
    board: &Board,
    selected: usize,
    status: &mut String,
) {
    status.clear();
    if let Some(agent) = board.selectable().get(selected).copied() {
        *status = match apply_placement(agent, focuser, launcher, &kill_pid) {
            Ok(()) => format!("toggling {}", ui::focus_label(agent)),
            Err(e) => e,
        };
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
        // A live hidden agent has no host window to focus; going to it reveals
        // it (kill + resume visible), the same kill-and-resume as `h`.
        Origin::Live if agent.hidden => {
            apply_placement(agent, focuser, launcher, &kill_pid).map_err(|e| format!("reveal: {e}"))
        }
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, agent.resume_argv()) {
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), &command, None, &agent.launch_mode())
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
        Origin::Dormant => match (&a.cwd, a.resume_argv()) {
            (Some(cwd), Some(command)) => Some(ComposeTarget::Dormant {
                cwd: cwd.clone(),
                resume_command: command,
                mode: a.launch_mode(),
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
    let Some(command) = agent.spawn_argv() else {
        *status = format!("spawn: {} announced no spawn command", agent.label);
        return false;
    };
    let cwd = launch::default_cwd(agent.cwd.as_deref());
    // agent.launch_mode() carries the selected card's `hidden`, so Shift+Enter
    // beside a hidden card spawns the new agent hidden too (same placement).
    match launcher.launch(&cwd, &command, None, &agent.launch_mode()) {
        Ok(()) => true,
        Err(e) => {
            *status = format!("spawn: {e}");
            false
        }
    }
}

/// `d`: dismiss the selected agent. A live agent is closed by terminating its
/// agent process (which closes its terminal window and, via the agent's clean
/// shutdown, leaves a dormant resumable record). A hidden live agent has no
/// host window, so its pid is killed directly. A dormant record is forgotten
/// by deleting its registry file. So `d` twice fully removes a session: close,
/// then forget.
fn dismiss_selected(
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
        // A hidden agent has no host window; kill its pid directly (as reveal
        // does). A visible one is closed by killing its window pid via the
        // focuser. Either way pi shuts down cleanly and leaves a dormant record.
        Origin::Live => {
            *status = if agent.hidden {
                match kill_pid(agent.pid) {
                    Ok(()) => format!("closing {}", ui::focus_label(agent)),
                    Err(e) => format!("close: {e}"),
                }
            } else {
                match focuser.close(agent) {
                    Ok(()) => format!("closing {}", ui::focus_label(agent)),
                    Err(e) => format!("close: {e}"),
                }
            };
        }
        Origin::Dormant => {
            // Forget a dormant session: delete its SOURCE record in the agent's
            // own workdir (`<cwd>/.corral/registry/`) AND its home input pointer
            // (`~/.corral/input/registry/<id>`); corrald's curator reflects the
            // removal out of state/registry next tick. Deleting the
            // state/registry copy directly would be futile (re-curated).
            if let (Some(cwd), Some(id)) = (&agent.cwd, &agent.session_id) {
                if let Err(e) = corral_core::curation::forget_dormant(cwd, id) {
                    *status = format!("dismiss: {e}");
                }
            }
        }
    }
}
