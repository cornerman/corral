//! The iced attention board: a flat, crisp presentation shell over the shared
//! `core::engine::Engine`, mirroring the ratatui TUI's look and the full
//! behavior of both boards. Four columns (Requires Action / Idle / Running /
//! Dormant) of flat cards under a centered filter, a thin bottom key-hint bar,
//! base16-themed, following the system light/dark preference.
//!
//! Interaction mirrors the TUI, keeping the egui shell's learnings: `/` focuses
//! the filter (narrows cards by whole content); arrows move, Enter
//! goes, Shift+Enter spawns, `m` messages (compose overlay), `d` dismisses,
//! `Esc` clears the filter then quits, `q` quits; a single click selects a
//! card, a double click goes, and a right-click opens a context menu of the
//! footer actions; selection survives a filter clear.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use corral_core::click::{ClickKind, ClickTracker};
use corral_core::focus::{self, WindowFocuser};
use corral_core::launch::{self, LaunchMode, Launcher, TerminalLauncher};
use corral_core::menu::MenuAction;
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::placement::{apply_placement, kill_pid};
use corral_core::transition::{self, MoveAction};
use corral_core::{engine::Engine, nav, palette::basename, palette::color_index, paths, prompt};

use iced::widget::{
    canvas, column, container, mouse_area, row, scrollable, text, text_input, Space,
};
use iced::{
    keyboard, mouse, Alignment, Background, Border, Color, Element, Font, Length, Point, Rectangle,
    Renderer, Size, Subscription, Task, Theme,
};

use crate::theme::{self, Base16};

/// Fixed card height (points): two text rows (title/age ~18, the
/// pill+badge+activity row ~18) plus the 2px gap and 8px top/bottom padding,
/// rounded up so the second row is never clipped. Compressed from three rows —
/// the cwd pill replaced the full-path line and age moved onto the title row.
const CARD_H: f32 = 56.0;
/// Vertical gap between cards in a column (the list `spacing`).
const CARD_GAP: f32 = 6.0;
/// Context-menu geometry (points): fixed width sized for the longest label
/// ("Toggle hidden"), one row per entry, plus vertical padding. Used to clamp
/// the anchor so the whole menu stays on-screen.
const MENU_W: f32 = 160.0;
const MENU_ROW_H: f32 = 26.0;
fn menu_h() -> f32 {
    MenuAction::ALL.len() as f32 * MENU_ROW_H + 8.0
}

/// A committed card move expires this long after firing if the agent never
/// reaches the target column (fail-quiet; the action was fire-and-forget).
const PENDING_TTL: Duration = Duration::from_secs(5);

/// A column's scrollable id (for programmatic scroll-into-view).
fn col_scroll_id(c: usize) -> scrollable::Id {
    scrollable::Id::new(format!("corral-col-{c}"))
}

/// The filter field's focus id (for programmatic focus/blur).
fn filter_id() -> text_input::Id {
    text_input::Id::new("corral-filter")
}

/// The compose field's focus id, so opening `m` focuses it for typing.
fn compose_id() -> text_input::Id {
    text_input::Id::new("corral-compose")
}

/// Messages the board reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Key(keyboard::Key, keyboard::Modifiers),
    /// Escape, delivered by a dedicated listener that sees it even when the
    /// focused filter field captures it (iced's TextInput eats Escape).
    Escape,
    Scrolled(usize, scrollable::Viewport),
    FilterInput(String),
    FilterSubmit,
    FocusFilter,
    CardClicked(usize),
    /// Right-click a card: select it, then open the context menu at the cursor.
    CardRightClicked(usize),
    /// Latest cursor position, tracked so the context menu opens at the cursor.
    CursorMoved(Point),
    /// Window size, tracked so the context menu clamps on-screen.
    Resized(Size),
    /// Run a context-menu entry on the selected card, then close the menu.
    MenuPick(MenuAction),
    /// Close the context menu without acting (Esc or a click outside).
    MenuDismiss,
    Go,
    Spawn,
    /// Window gained (true) or lost (false) focus. Launcher dismisses on blur.
    Focused(bool),
    OpenCompose,
    Dismiss,
    ToggleHidden,
    Quit,
    ComposeInput(String),
    ComposeSend,
    ComposeCancel,
    /// A key was released (shift-release commits an in-progress card move).
    KeyReleased(keyboard::Key),
    /// The left mouse button was released (drops an in-progress drag).
    MouseReleased,
}

/// An operator message being composed (opened with `m`).
struct Compose {
    target: ComposeTarget,
    label: String,
    buf: String,
}

/// Where a composed message is delivered.
enum ComposeTarget {
    /// A live agent: straight to its socket.
    Live(PathBuf),
    /// A dormant session: resume it with the message as its first prompt.
    Dormant {
        cwd: String,
        resume_command: Vec<String>,
        /// Launch options of the resumed agent (gui + message flag).
        mode: LaunchMode,
    },
}

pub struct Board {
    engine: Engine,
    focuser: Box<dyn WindowFocuser>,
    launcher: TerminalLauncher,
    dark: bool,
    last_theme_check: Instant,
    filter: String,
    filtering: bool,
    /// Launcher mode (--launcher): boot focused on the filter and exit the
    /// process after go / new (ephemeral rofi-style popup; WM respawns it).
    launcher_mode: bool,
    /// Launcher only: set once the window has been focused, so a spurious
    /// pre-focus Unfocused event at boot cannot dismiss it before it appears.
    focused_once: bool,
    /// Flat selection index across the (filtered) columns, TUI-style.
    selected: usize,
    status: String,
    compose: Option<Compose>,
    /// Classifies left clicks into select (single) vs go (double).
    clicks: ClickTracker,
    /// The open right-click context menu, anchored at the cursor (clamped
    /// on-screen). Acts on the current `selected` card.
    menu: Option<Point>,
    /// Latest cursor position and window size, for menu placement + clamping.
    cursor: Point,
    window: Size,
    /// The in-progress card move (shift+arrow, or a mouse drag): the source
    /// column and the ghost target being chosen. The moving agent is the
    /// current `selected`. `None` when not moving.
    move_mode: Option<(Column, Column)>,
    /// Source column of a started mouse drag (set on card press), used to begin
    /// a move once the cursor crosses into another column.
    drag_source: Option<Column>,
    /// Committed-but-unconfirmed moves keyed by `Agent::move_key`: the target
    /// column and when fired. Rendered as an in-flight badge; cleared when the
    /// agent reaches the target or after `PENDING_TTL`.
    pending: HashMap<String, (Column, Instant)>,
    // Snapshot rebuilt each tick / filter change; view and actions read it.
    columns: Vec<Vec<Agent>>,
    in_state: HashMap<PathBuf, String>,
    quiet: HashMap<PathBuf, String>,
    dormant_ages: HashMap<String, String>,
    /// Latest scroll viewport per column, for minimal scroll-into-view.
    viewports: [Option<scrollable::Viewport>; 4],
}

impl Board {
    /// Build the board and its boot task. In launcher mode the boot task
    /// focuses the filter so you can type to narrow immediately.
    pub fn new(launcher_mode: bool) -> (Self, Task<Message>) {
        // Viewers read only corrald's vetted registry (never agent-writable records).
        let dir = paths::state_registry_dir()
            .expect("state registry dir (set $HOME or $CORRAL_STATE_DIR)");
        let mut b = Board {
            engine: Engine::new(dir),
            focuser: focus::detect(),
            launcher: TerminalLauncher,
            dark: system_prefers_dark(),
            last_theme_check: Instant::now(),
            filter: String::new(),
            filtering: false,
            launcher_mode,
            focused_once: false,
            selected: 0,
            status: String::new(),
            compose: None,
            clicks: ClickTracker::default(),
            menu: None,
            move_mode: None,
            drag_source: None,
            pending: HashMap::new(),
            cursor: Point::ORIGIN,
            window: Size::new(1024.0, 768.0),
            columns: vec![Vec::new(); Column::ALL.len()],
            in_state: HashMap::new(),
            quiet: HashMap::new(),
            dormant_ages: HashMap::new(),
            viewports: [None; 4],
        };
        // Scan the registry synchronously at boot so cards appear immediately;
        // otherwise the first tick is ~500ms away and the window opens empty
        // (felt on every ephemeral launcher summon). Live state seeds after.
        b.engine.tick();
        b.refresh();
        let boot = if launcher_mode {
            b.filtering = true;
            text_input::focus(filter_id())
        } else {
            Task::none()
        };
        (b, boot)
    }

    pub fn scheme(&self) -> Base16 {
        let (dark, light) = theme::selected_pair();
        if self.dark {
            dark
        } else {
            light
        }
    }

    /// Rebuild the filtered column snapshot + age maps from the engine, then
    /// clamp the selection to the (possibly shrunken) set.
    fn refresh(&mut self) {
        self.in_state = self.engine.in_state_ages();
        self.quiet = self.engine.quiet_ages();
        self.dormant_ages = self.engine.dormant_ages().clone();
        let board = self.engine.board();
        let filter = self.filter.clone();
        self.columns = Column::ALL
            .iter()
            .map(|c| {
                board
                    .column(*c)
                    .into_iter()
                    .filter(|a| a.matches_query(&filter))
                    .cloned()
                    .collect()
            })
            .collect();
        let total: usize = self.columns.iter().map(Vec::len).sum();
        if self.selected >= total {
            self.selected = total.saturating_sub(1);
        }
        // Reconcile pending moves: a move confirms when the agent reaches its
        // target column (the board never fakes the move), else it expires.
        let by_key: HashMap<String, Column> = self
            .columns
            .iter()
            .flatten()
            .map(|a| (a.move_key(), a.column()))
            .collect();
        self.pending.retain(|k, (target, since)| {
            by_key.get(k) != Some(target) && since.elapsed() <= PENDING_TTL
        });
    }

    /// The destination column index under a cursor x (points), from the board
    /// layout (12pt content padding, 14pt row spacing, three 1pt separators,
    /// four equal columns). Snaps gaps to the nearest column so a drop between
    /// columns still targets one. Used for mouse drag/drop.
    fn column_at_x(&self, x: f32) -> Option<usize> {
        let inner = self.window.width - 24.0; // content padding L+R
        let colw = (inner - 3.0 - 6.0 * 14.0) / 4.0; // minus 3 seps + 6 gaps
        if colw <= 0.0 {
            return None;
        }
        let stride = colw + 29.0; // one column + (gap + sep + gap) to the next
        let i = ((x - 12.0) / stride).floor() as i32;
        Some(i.clamp(0, 3) as usize)
    }

    /// Fire the real agent action a card move triggers (see `core::transition`)
    /// and record it pending until confirmed. Shared by the keyboard and mouse
    /// commit paths.
    fn commit_move(&mut self, source: Column, target: Column) {
        let Some(agent) = self.selected_agent().cloned() else {
            return;
        };
        let key = agent.move_key();
        let label = agent.title.clone().unwrap_or_else(|| "agent".into());
        let result: Result<(), String> = match transition::action_for(source, target) {
            MoveAction::Cancel => {
                prompt::send_cancel(&agent.socket_path).map_err(|e| format!("cancel: {e}"))
            }
            MoveAction::Nudge => prompt::send_prompt(&agent.socket_path, "continue")
                .map_err(|e| format!("nudge: {e}")),
            MoveAction::Kill => {
                let close = if agent.hidden {
                    kill_pid(agent.pid)
                } else {
                    self.focuser.close(&agent)
                };
                close.map_err(|e| format!("close: {e}"))
            }
            MoveAction::Resume | MoveAction::ResumeAndNudge => {
                match (&agent.cwd, agent.resume_argv()) {
                    (Some(cwd), Some(cmd)) => {
                        let msg = matches!(
                            transition::action_for(source, target),
                            MoveAction::ResumeAndNudge
                        )
                        .then_some("continue");
                        self.launcher
                            .launch(Path::new(cwd), &cmd, msg, &agent.launch_mode())
                            .map_err(|e| format!("resume: {e}"))
                    }
                    _ => Err("resume: dormant record missing cwd/command".into()),
                }
            }
            MoveAction::NoOp => return,
        };
        match result {
            Ok(()) => {
                self.status = format!("moving {label} → {}", target.title());
                self.pending.insert(key, (target, Instant::now()));
            }
            Err(e) => self.status = e,
        }
    }

    fn counts(&self) -> [usize; 4] {
        std::array::from_fn(|i| self.columns[i].len())
    }

    /// Clamp a menu anchor so the whole box stays inside the window.
    fn clamp_menu(&self, p: Point) -> Point {
        Point {
            x: p.x.min((self.window.width - MENU_W).max(0.0)),
            y: p.y.min((self.window.height - menu_h()).max(0.0)),
        }
    }

    fn selected_agent(&self) -> Option<&Agent> {
        self.columns.iter().flatten().nth(self.selected)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                self.engine.tick();
                if self.last_theme_check.elapsed() >= Duration::from_secs(2) {
                    self.dark = system_prefers_dark();
                    self.last_theme_check = Instant::now();
                }
                self.refresh();
            }
            Message::FilterInput(s) => {
                self.filter = s;
                self.filtering = true;
                self.refresh();
                return self.scroll_to_selection();
            }
            Message::FocusFilter => {
                self.filtering = true;
                return text_input::focus(filter_id());
            }
            Message::FilterSubmit => {
                // Enter while filtering goes to the selection (TUI parity).
                return self.act_go();
            }
            Message::CardClicked(idx) => {
                // A click blurs the filter field, so leave filter mode too.
                self.filtering = false;
                self.selected = idx;
                // Arm a possible drag from this card's column; a drag into
                // another column begins a move (see CursorMoved), a plain
                // release is just the click below.
                self.drag_source = self.columns.iter().flatten().nth(idx).map(|a| a.column());
                // Single click selects; a double click on the same card goes
                // (focus/reveal/resume).
                match self.clicks.press(idx, Instant::now()) {
                    ClickKind::Go => return self.act_go(),
                    ClickKind::Select => return self.scroll_to_selection(),
                }
            }
            Message::CardRightClicked(idx) => {
                self.filtering = false;
                self.selected = idx;
                // Anchor the menu at the cursor, clamped so it stays on-screen.
                self.menu = Some(self.clamp_menu(self.cursor));
                return self.scroll_to_selection();
            }
            Message::CursorMoved(p) => {
                self.cursor = p;
                // While a card drag is armed, moving into another valid
                // destination column begins/retargets the move (the drop-boxes
                // then show where it will land).
                if let Some(source) = self.drag_source {
                    if let Some(c) = self.column_at_x(p.x).map(|i| Column::ALL[i]) {
                        match self.move_mode {
                            // Once moving, retarget to a valid stop under the
                            // cursor (a destination, or the source = drop to
                            // cancel); an invalid column (Requires Action) is
                            // ignored, keeping the last target.
                            Some((src, _)) if transition::stops(src).contains(&c) => {
                                self.move_mode = Some((src, c));
                            }
                            Some(_) => {}
                            // Crossing into a different valid destination begins
                            // the move; jitter or an invalid column does not (a
                            // plain click stays a click).
                            None if c != source && transition::DESTINATIONS.contains(&c) => {
                                self.move_mode = Some((source, c));
                            }
                            None => {}
                        }
                    }
                }
            }
            Message::Resized(sz) => self.window = sz,
            Message::KeyReleased(key) => {
                // Shift-release drops an in-progress keyboard move.
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::Shift)) {
                    if let Some((source, target)) = self.move_mode.take() {
                        self.commit_move(source, target);
                    }
                }
            }
            Message::MouseReleased => {
                // A drop commits an in-progress drag; a release with no move was
                // just a click.
                if let Some((source, target)) = self.move_mode.take() {
                    self.commit_move(source, target);
                }
                self.drag_source = None;
            }
            Message::MenuPick(action) => {
                self.menu = None;
                return match action {
                    MenuAction::Go => self.act_go(),
                    MenuAction::Message => self.update(Message::OpenCompose),
                    MenuAction::Spawn => self.act_spawn(),
                    MenuAction::ToggleHidden => self.act_toggle_hidden(),
                    MenuAction::Dismiss => self.update(Message::Dismiss),
                };
            }
            Message::MenuDismiss => self.menu = None,
            Message::Go => return self.act_go(),
            Message::Spawn => return self.act_spawn(),
            Message::Focused(focused) => {
                if focused {
                    self.focused_once = true;
                } else if self.launcher_mode && self.focused_once {
                    // rofi-style: the ephemeral launcher closes on focus loss.
                    return iced::exit();
                }
            }
            Message::OpenCompose => {
                if let Some(a) = self.selected_agent() {
                    self.compose = compose_for(a);
                    if self.compose.is_none() {
                        self.status = "cannot message: no target".into();
                    } else {
                        // Focus the field so the operator types straight away.
                        return text_input::focus(compose_id());
                    }
                }
            }
            Message::Dismiss => {
                if let Some(a) = self.selected_agent().cloned() {
                    self.status = self.dismiss(&a);
                }
            }
            Message::ToggleHidden => return self.act_toggle_hidden(),
            Message::Quit => return iced::exit(),
            Message::ComposeInput(s) => {
                if let Some(c) = &mut self.compose {
                    c.buf = s;
                }
            }
            Message::ComposeSend => {
                if let Some(c) = self.compose.take() {
                    let text = c.buf.trim().to_string();
                    if !text.is_empty() {
                        self.status = self.deliver(&c.target, &c.label, &text);
                    }
                }
            }
            Message::ComposeCancel => self.compose = None,
            Message::Escape => return self.escape(),
            Message::Key(key, mods) => return self.on_key(key, mods),
            Message::Scrolled(c, vp) => {
                if let Some(slot) = self.viewports.get_mut(c) {
                    *slot = Some(vp);
                }
            }
        }
        Task::none()
    }

    /// Escape. Owns all Escape handling so it is reliable even when the focused
    /// filter field captures the key event.
    ///   - Launcher: dismiss the popup at once (a throwaway summon, single
    ///     press to bail), regardless of filter/edit state.
    ///   - Normal board, in stages (never quits — only `q` does, so a stray
    ///     Esc can never nuke the persistent window):
    ///       1. compose overlay open -> cancel it;
    ///       2. filter field focused -> blur it, keeping the filter and selection;
    ///       3. filter non-empty -> clear it, keeping the current selection (its
    ///          index stays in range once the full board returns);
    ///       4. otherwise -> nothing.
    fn escape(&mut self) -> Task<Message> {
        // A card move in progress cancels first (before the launcher exit), so
        // Esc backs out of a move without dropping it.
        if self.move_mode.is_some() {
            self.move_mode = None;
            self.drag_source = None;
            self.status.clear();
            return Task::none();
        }
        if self.launcher_mode {
            return iced::exit();
        }
        if self.menu.is_some() {
            self.menu = None;
            return Task::none();
        }
        if self.compose.is_some() {
            self.compose = None;
            return Task::none();
        }
        if self.filtering {
            // Unfocus, leaving the filter text and cursor.
            self.filtering = false;
            // Focus a nonexistent id to blur the filter field.
            return text_input::focus(text_input::Id::new("corral-blur"));
        }
        if !self.filter.is_empty() {
            // Field already blurred: clear the filter, keeping the selection.
            self.filter.clear();
            self.refresh();
            return self.scroll_to_selection();
        }
        // Nothing left to peel: do nothing. The normal board never exits on
        // Escape (q is the sole quit), so an accidental Escape never kills it.
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        let counts = self.counts();
        // An open context menu captures the keyboard: only Escape acts (owned
        // by `escape()` via the dedicated listener, closing the menu), so keys
        // never leak to the board behind it. Mirrors the TUI's capture.
        if self.menu.is_some() {
            return Task::none();
        }
        // In the compose overlay, Enter sends; Esc (cancel) is owned by
        // `escape()` via the dedicated listener; nothing else.
        if self.compose.is_some() {
            if let keyboard::Key::Named(Named::Enter) = key {
                return self.update(Message::ComposeSend);
            }
            return Task::none();
        }
        // A card move in progress captures the keyboard: Left/Right slide the
        // ghost target, Enter commits (shift-release also commits, via the
        // release listener), Esc cancels (via `escape()`).
        if let Some((source, target)) = self.move_mode {
            match key {
                keyboard::Key::Named(Named::ArrowLeft) => {
                    self.move_mode =
                        Some((source, transition::slide_target(source, target, false)));
                }
                keyboard::Key::Named(Named::ArrowRight) => {
                    self.move_mode = Some((source, transition::slide_target(source, target, true)));
                }
                keyboard::Key::Named(Named::Enter) => {
                    self.move_mode = None;
                    self.commit_move(source, target);
                }
                _ => {}
            }
            return Task::none();
        }
        match key {
            // The filter input rings with the board vertically: while it is
            // focused, Down/Up step off it into the board (Down -> the column's
            // first row, Up -> its last), blurring the field so m/d/h act as
            // commands; while a card is selected, Down at the bottom edge and Up
            // at the top edge ring back to the filter input; otherwise move
            // within the column.
            keyboard::Key::Named(Named::ArrowDown) => {
                return self.ring_vertical(&counts, true);
            }
            keyboard::Key::Named(Named::ArrowUp) => {
                return self.ring_vertical(&counts, false);
            }
            // No `filtering` guard on these: a focused filter field captures
            // Left/Right (caret) and letters (typing), so `on_key_press` never
            // delivers them here while focused; when the field is unfocused,
            // they are commands regardless of any applied filter.
            // Shift+Left/Right grab the selected card into move mode (drive its
            // state by moving it between columns); plain Left/Right switch the
            // selected column.
            keyboard::Key::Named(Named::ArrowRight) => {
                if mods.shift() {
                    if let Some(src) = self.selected_agent().map(|a| a.column()) {
                        self.move_mode = Some((src, transition::initial_target(src, true)));
                    }
                } else {
                    self.selected = nav::move_col(self.selected, &counts, true);
                }
            }
            keyboard::Key::Named(Named::ArrowLeft) => {
                if mods.shift() {
                    if let Some(src) = self.selected_agent().map(|a| a.column()) {
                        self.move_mode = Some((src, transition::initial_target(src, false)));
                    }
                } else {
                    self.selected = nav::move_col(self.selected, &counts, false);
                }
            }
            keyboard::Key::Named(Named::Enter) => {
                return if mods.shift() {
                    self.act_spawn()
                } else {
                    self.act_go()
                };
            }
            keyboard::Key::Character(c) => match c.as_str() {
                "m" => return self.update(Message::OpenCompose),
                "d" => return self.update(Message::Dismiss),
                "h" => return self.update(Message::ToggleHidden),
                "q" => std::process::exit(0),
                "/" => {
                    self.filtering = true;
                    return text_input::focus(filter_id());
                }
                _ => {}
            },
            _ => {}
        }
        // Any nav arm falls through to here; keep the selection visible.
        self.scroll_to_selection()
    }

    /// One vertical step of the input<->board ring (see `on_key`). Returns the
    /// focus/scroll task so the filter field gains or loses focus as the
    /// selection crosses the input node. No-op direction on an empty board.
    fn ring_vertical(&mut self, counts: &[usize; 4], down: bool) -> Task<Message> {
        if self.filtering {
            if counts.iter().sum::<usize>() == 0 {
                return Task::none();
            }
            // Step off the input into the board and blur the field.
            self.filtering = false;
            self.selected = nav::board_entry(self.selected, counts, down);
            return Task::batch([
                text_input::focus(text_input::Id::new("corral-blur")),
                self.scroll_to_selection(),
            ]);
        }
        if nav::at_board_edge(self.selected, counts, down) {
            // Ring back to the filter input.
            self.filtering = true;
            return text_input::focus(filter_id());
        }
        self.selected = nav::move_selection(self.selected, counts, down);
        self.scroll_to_selection()
    }

    /// Scroll the selected card's column just enough to keep it visible (a no-op
    /// when it is already in view), mirroring the TUI/egui behavior.
    fn scroll_to_selection(&self) -> Task<Message> {
        let mut idx = self.selected;
        for (c, col) in self.columns.iter().enumerate() {
            if idx < col.len() {
                let card_top = idx as f32 * (CARD_H + CARD_GAP);
                let card_bottom = card_top + CARD_H;
                let (off, h) = match &self.viewports[c] {
                    Some(v) => (v.absolute_offset().y, v.bounds().height),
                    None => return Task::none(),
                };
                let new_off = if card_top < off {
                    card_top
                } else if h > 0.0 && card_bottom > off + h {
                    card_bottom - h
                } else {
                    return Task::none();
                };
                return scrollable::scroll_to(
                    col_scroll_id(c),
                    scrollable::AbsoluteOffset {
                        x: 0.0,
                        y: new_off.max(0.0),
                    },
                );
            }
            idx -= col.len();
        }
        Task::none()
    }

    fn act_go(&mut self) -> Task<Message> {
        let mut ok = false;
        if let Some(a) = self.selected_agent().cloned() {
            self.status = match activate(&a, self.focuser.as_ref(), &self.launcher) {
                Ok(()) => {
                    ok = true;
                    format!("→ {}", a.title.as_deref().unwrap_or("agent"))
                }
                Err(e) => e,
            };
        }
        // Launcher mode: a successful go dismisses the popup (the WM respawns
        // it on the next summon). An error stays open so its status is seen.
        if ok && self.launcher_mode {
            return iced::exit();
        }
        Task::none()
    }

    fn act_spawn(&mut self) -> Task<Message> {
        let agent = self.selected_agent().cloned();
        let mut ok = false;
        self.status = match agent
            .as_ref()
            .and_then(|a| a.spawn_argv().map(|c| (a, c)))
        {
            Some((a, command)) => {
                let cwd = launch::default_cwd(a.cwd.as_deref());
                // a.launch_mode() carries the selected card's `hidden`, so a
                // spawn beside a hidden card is hidden too (same placement).
                match self.launcher.launch(&cwd, &command, None, &a.launch_mode()) {
                    Ok(()) => {
                        ok = true;
                        format!("spawned in {}", tilde(&cwd.to_string_lossy()))
                    }
                    Err(e) => format!("spawn: {e}"),
                }
            }
            None => "spawn: no launchable agent selected".into(),
        };
        if ok && self.launcher_mode {
            return iced::exit();
        }
        Task::none()
    }

    /// `h`: toggle the selected agent's placement (hide a visible one, reveal a
    /// hidden one, start a dormant one hidden). Kill-and-resume in every case,
    /// mirroring the TUI.
    fn act_toggle_hidden(&mut self) -> Task<Message> {
        if let Some(a) = self.selected_agent().cloned() {
            self.status =
                match apply_placement(&a, self.focuser.as_ref(), &self.launcher, &kill_pid) {
                    Ok(()) => "toggling".into(),
                    Err(e) => e,
                };
        }
        Task::none()
    }

    fn dismiss(&self, agent: &Agent) -> String {
        match agent.origin {
            // A hidden agent has no host window; kill its pid directly (as
            // reveal does). A visible one is closed by killing its window pid
            // via the focuser. Either way the agent shuts down cleanly and
            // leaves a dormant record.
            Origin::Live => {
                let close = if agent.hidden {
                    kill_pid(agent.pid)
                } else {
                    self.focuser.close(agent)
                };
                match close {
                    Ok(()) => format!("closing {}", agent.title.as_deref().unwrap_or("agent")),
                    Err(e) => format!("close: {e}"),
                }
            }
            // Forget a dormant session: delete its SOURCE record in the agent's
            // own workdir AND its home input pointer; corrald reflects the
            // removal out of state/registry (deleting the copy directly would
            // be re-curated).
            Origin::Dormant => match (&agent.cwd, &agent.session_id) {
                (Some(cwd), Some(id)) => match corral_core::curation::forget_dormant(cwd, id) {
                    Ok(()) => "forgot dormant record".into(),
                    Err(e) => format!("dismiss: {e}"),
                },
                _ => "dismiss: no session id/cwd".into(),
            },
        }
    }

    fn deliver(&self, target: &ComposeTarget, label: &str, text: &str) -> String {
        match target {
            ComposeTarget::Live(socket) => match prompt::send_prompt(socket, text) {
                Ok(()) => format!("sent to {label}"),
                Err(e) => format!("send: {e}"),
            },
            ComposeTarget::Dormant {
                cwd,
                resume_command,
                mode,
            } => match self
                .launcher
                .launch(Path::new(cwd), resume_command, Some(text), mode)
            {
                Ok(()) => format!("resuming {label} to deliver"),
                Err(e) => format!("resume: {e}"),
            },
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
            keyboard::on_key_press(|key, mods| Some(Message::Key(key, mods))),
            // Shift-release commits an in-progress keyboard move.
            keyboard::on_key_release(|key, _mods| Some(Message::KeyReleased(key))),
            // A focused TextInput captures Escape (to blur itself), so
            // `on_key_press` never sees it; listen at any status for Escape.
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape),
                iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focused(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => {
                    Some(Message::Focused(false))
                }
                // Track the cursor so the context menu opens at it, and the
                // window size so it clamps on-screen.
                iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(position))
                }
                iced::Event::Window(iced::window::Event::Resized(size)) => {
                    Some(Message::Resized(size))
                }
                // A left-button release drops an in-progress card drag.
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::MouseReleased)
                }
                _ => None,
            }),
        ])
    }

    pub fn theme(&self) -> Theme {
        let s = self.scheme();
        Theme::custom(
            s.name.clone(),
            iced::theme::Palette {
                background: s.base[0],
                text: s.base[5],
                primary: s.accent[Base16::BLUE],
                success: s.accent[Base16::GREEN],
                danger: s.accent[Base16::RED],
            },
        )
    }

    pub fn view(&self) -> Element<'_, Message> {
        let s = self.scheme();
        let board = self.board_view(&s);
        if let Some(compose) = &self.compose {
            // Overlay the compose card centered over a dimmed board.
            iced::widget::stack![board, compose_overlay(compose, &s)].into()
        } else if let Some(anchor) = self.menu {
            // A full-window dismiss layer under the menu catches outside clicks.
            iced::widget::stack![board, menu_overlay(anchor, &s)].into()
        } else {
            board
        }
    }

    fn board_view(&self, s: &Base16) -> Element<'_, Message> {
        let dim = s.base[3];
        let fg = s.base[5];

        // Top row: "corral" wordmark left, filter centered, status right.
        let wordmark = container(text("corral").size(15).color(dim))
            .width(Length::Fill)
            .align_y(Alignment::Center);
        // Modern, minimal: no box, just a thin underline (like the TUI and the
        // old GUI). iced's text_input border is uniform, so the field is drawn
        // frameless and a 1px line sits beneath it.
        let underline = s.base[2];
        let filter_field = text_input("type to filter…", &self.filter)
            .id(filter_id())
            .on_input(Message::FilterInput)
            .on_submit(Message::FilterSubmit)
            .size(18)
            .padding(4)
            .style(bare_input(s));
        let filter = column![
            filter_field,
            container(Space::new(Length::Fill, Length::Fixed(1.0))).style(move |_t| {
                container::Style {
                    background: Some(Background::Color(underline)),
                    ..container::Style::default()
                }
            }),
        ]
        .width(Length::Fixed(380.0))
        .spacing(2);
        let status = container(text(&self.status).size(13).color(dim))
            .width(Length::Fill)
            .align_x(Alignment::End);
        let top = row![wordmark, filter, status]
            .align_y(Alignment::Center)
            .spacing(8);

        // Four columns.
        let mut base = 0usize;
        let mut cols = row![].spacing(14).height(Length::Fill);
        for (i, col) in Column::ALL.into_iter().enumerate() {
            let count = self.columns[i].len();
            // The Dormant column is faded: it holds inactive, resumable records.
            let fade = if matches!(col, Column::Dormant) {
                0.55
            } else {
                1.0
            };
            let header = row![
                text(col.title())
                    .size(15)
                    .color(Color { a: fade, ..fg })
                    .font(semibold()),
                text(format!("{count}"))
                    .size(15)
                    .color(Color { a: fade, ..dim }),
            ]
            .spacing(8);
            // Right padding reserves the gutter so the scrollbar never
            // overlays the card's right-aligned kind label.
            let mut list = column![]
                .spacing(6)
                .padding(iced::Padding::ZERO.right(12.0));
            for (j, agent) in self.columns[i].iter().enumerate() {
                let idx = base + j;
                let age = card_age(agent, col, &self.in_state, &self.quiet, &self.dormant_ages);
                list = list.push(self.card(agent, col, s, age, idx));
            }
            base += count;
            let body = scrollable(list)
                .id(col_scroll_id(i))
                .on_scroll(move |vp| Message::Scrolled(i, vp))
                .height(Length::Fill)
                .width(Length::Fill);
            cols = cols.push(
                column![header, body]
                    .spacing(8)
                    .width(Length::Fill)
                    .height(Length::Fill),
            );
            // A faint hairline in the gutter separates the columns (the GUI
            // analogue of the TUI's vertical dividers), reusing the filter's
            // underline color so the UI has one structural-line tone.
            if i + 1 < Column::ALL.len() {
                cols = cols.push(
                    container(Space::new(Length::Fixed(1.0), Length::Fill)).style(move |_t| {
                        container::Style {
                            background: Some(Background::Color(underline)),
                            ..container::Style::default()
                        }
                    }),
                );
            }
        }

        // In move mode the columns become labeled drop-boxes (cards hidden),
        // the target highlighted, Requires Action greyed as a non-destination.
        let mid: Element<'_, Message> = match self.move_mode {
            Some((src, target)) => self.move_columns(s, src, target),
            None => cols.into(),
        };

        let footer = self.footer(s);

        let content = column![top, mid, footer].spacing(14).padding(12);
        let bg0 = s.base[0];
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_t| container::Style {
                background: Some(Background::Color(bg0)),
                ..container::Style::default()
            })
            .into()
    }

    /// The four columns rendered as drop-boxes for move mode: each a bordered
    /// box titled with the column name, the `target` box highlighted with the
    /// moving card's label, Requires Action greyed as a non-destination.
    /// Mirrors the TUI's `render_move`.
    fn move_columns(&self, s: &Base16, source: Column, target: Column) -> Element<'_, Message> {
        let label = self
            .selected_agent()
            .and_then(|a| a.title.clone())
            .unwrap_or_else(|| "agent".into());
        // A no-op target (the source column, or Requires Action) = drop to
        // cancel.
        let cancels = matches!(transition::action_for(source, target), MoveAction::NoOp);
        let mut cols = row![].spacing(14).height(Length::Fill);
        for col in Column::ALL {
            let is_target = col == target;
            let is_dest = transition::DESTINATIONS.contains(&col) || col == source;
            let (border_col, fg) = if is_target && cancels {
                (s.accent[Base16::YELLOW], s.base[5])
            } else if is_target {
                (s.accent[Base16::GREEN], s.base[5])
            } else if is_dest {
                (s.base[2], s.base[4])
            } else {
                (s.base[1], s.base[3]) // Requires Action: never a destination
            };
            let title = if is_dest {
                col.title().to_string()
            } else {
                format!("{} (not a target)", col.title())
            };
            let mut body = column![text(title).size(15).color(fg).font(semibold())].spacing(10);
            if is_target {
                let note = if cancels {
                    format!("{label} (drop to cancel)")
                } else {
                    label.clone()
                };
                body = body.push(text(note).size(14).color(fg).font(semibold()));
            }
            let bw = if is_target { 2.0 } else { 1.0 };
            let boxed = container(body)
                .padding(12)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |_t| container::Style {
                    border: Border {
                        color: border_col,
                        width: bw,
                        radius: 6.0.into(),
                    },
                    ..container::Style::default()
                });
            cols = cols.push(boxed);
        }
        cols.into()
    }

    fn card<'a>(
        &self,
        agent: &'a Agent,
        col: Column,
        s: &Base16,
        age: String,
        idx: usize,
    ) -> Element<'a, Message> {
        let selected = idx == self.selected;
        // Fade the Dormant column's cards (inactive, resumable records).
        let a = if matches!(col, Column::Dormant) {
            0.55
        } else {
            1.0
        };
        let dim = Color { a, ..s.base[3] };
        let fg = Color { a, ..s.base[5] };
        let accent = Color {
            a,
            ..state_color(agent, s)
        };

        // Title row: title (fill, clipped) on the left, the column age dim on
        // the right (moved off the info line to free the second row).
        let title = container(
            text(agent.title.clone().unwrap_or_else(|| "(unnamed)".into()))
                .size(14)
                .color(fg)
                .font(semibold())
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        // Pin the title to one line: a fixed-height card must always leave the
        // second row for the cwd pill + badge, so a long title clips instead of
        // wrapping and stealing that row (belt-and-braces with Wrapping::None).
        .height(Length::Fixed(18.0))
        .clip(true);
        let mut title_row = row![title].spacing(6).align_y(Alignment::Center);
        if !age.is_empty() {
            title_row = title_row.push(text(age).size(11).color(dim));
        }

        let mut body = column![title_row].spacing(2);

        // Second row: the colored basename pill, the kind badge, then the
        // activity hint filling the rest. The pill's color is a stable hash of
        // the full cwd, so same-directory cards read in the same color.
        let mut meta_row = row![].spacing(6).align_y(Alignment::Center);
        if let Some(cwd) = &agent.cwd {
            // The pill stays fully opaque even when dormant (alpha-fading it
            // muddied the chip against the card); dormancy mutes it instead.
            let (pc, ptext) = cwd_pill_colors(cwd, s, matches!(col, Column::Dormant));
            let pill = container(
                text(basename(cwd).to_string())
                    .size(11)
                    .color(ptext)
                    .wrapping(text::Wrapping::None),
            )
            .padding([1, 6])
            .style(move |_t| container::Style {
                background: Some(Background::Color(pc)),
                border: Border {
                    radius: 6.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            });
            meta_row = meta_row.push(pill);
        }
        meta_row = meta_row.push(tag_pill(&agent.label, s));
        // A live hidden agent shows a `hidden` pill (it runs in a headless
        // cage, so going to it reveals it by resume rather than focusing). A
        // plain-text tag pill, matching the TUI, so it needs no emoji font.
        if agent.origin == Origin::Live && agent.hidden {
            meta_row = meta_row.push(tag_pill("hidden", s));
        }
        // A pending card-move shows a bright in-flight badge, taking precedence
        // over the activity hint (it is the operator's own pending action). The
        // card has not moved yet: it waits for the agent's real state.
        if let Some((target, _)) = self.pending.get(&agent.move_key()) {
            meta_row = meta_row.push(
                text(format!("→ {} ⋯", target.title()))
                    .size(12)
                    .color(fg)
                    .font(semibold()),
            );
        } else if let Some(info) = agent.activity.as_deref().filter(|s| !s.is_empty()) {
            meta_row = meta_row.push(
                container(
                    text(info.to_string())
                        .size(12)
                        .color(dim)
                        .wrapping(text::Wrapping::None),
                )
                .width(Length::Fill)
                .clip(true),
            );
        }
        body = body.push(meta_row);

        // Thin state-colored left bar + faint accent tint when selected. Cards
        // are a fixed height (like the TUI): variable content stays aligned,
        // and a fixed bar height sidesteps `Length::Fill` (illegal inside a
        // scrollable — it makes the content report a fill height and panics).
        // Constant width so selecting a card does not shift its content right
        // (the bar is a layout element here, unlike the egui overlay).
        let bar = container(Space::new(Length::Fixed(3.0), 0.0))
            .height(Length::Fixed(CARD_H))
            .style(move |_t| container::Style {
                background: Some(Background::Color(accent)),
                ..container::Style::default()
            });
        // Every card sits on the elevated surface (base01) so it reads as a
        // card against the window background (base00); the selected card mixes
        // in its state accent, so selection is obvious without shifting layout.
        let fill = if selected {
            mix(s.base[1], accent, 0.22)
        } else {
            s.base[1]
        };
        let inner = container(body).padding([8, 10]).width(Length::Fill);
        let card = container(row![bar, inner].spacing(0))
            .width(Length::Fill)
            .height(Length::Fixed(CARD_H))
            .clip(true)
            .style(move |_t| container::Style {
                background: Some(Background::Color(fill)),
                ..container::Style::default()
            });
        mouse_area(card)
            .on_press(Message::CardClicked(idx))
            .on_right_press(Message::CardRightClicked(idx))
            .interaction(mouse::Interaction::Pointer)
            .into()
    }

    fn footer(&self, s: &Base16) -> Element<'_, Message> {
        let dim = s.base[3];
        let fg = s.base[5];
        let cap_bg = s.base[2];
        // A keycap pill (` key ` on a subtle fill) beside a dim label, matching
        // the TUI footer. Keys are plain ASCII so they render everywhere.
        let hint =
            |key: &'static str, desc: &'static str, msg: Option<Message>| -> Element<'_, Message> {
                let cap = container(text(key).size(13).color(fg))
                    .padding([1, 6])
                    .style(move |_t| container::Style {
                        background: Some(cap_bg.into()),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                let item = row![cap, text(desc).size(13).color(dim)]
                    .spacing(5)
                    .align_y(Alignment::Center);
                match msg {
                    Some(msg) => mouse_area(item)
                        .on_press(msg)
                        .interaction(mouse::Interaction::Pointer)
                        .into(),
                    None => item.into(),
                }
            };
        // Same keys, order and keycap styling as the TUI footer (ui.rs
        // footer_items / footer_layout). Verbs shared with the context menu
        // come from MenuAction::label so footer and menu cannot drift.
        row![
            hint("arrows", "move", None),
            hint("enter", MenuAction::Go.label(), Some(Message::Go)),
            hint(
                "shift+enter",
                MenuAction::Spawn.label(),
                Some(Message::Spawn)
            ),
            hint("/", "filter", Some(Message::FocusFilter)),
            hint("m", MenuAction::Message.label(), Some(Message::OpenCompose)),
            hint("d", MenuAction::Dismiss.label(), Some(Message::Dismiss)),
            hint(
                "h",
                MenuAction::ToggleHidden.label(),
                Some(Message::ToggleHidden)
            ),
            hint("q", "quit", Some(Message::Quit)),
            Space::new(Length::Fill, 0.0),
            canvas(Mark { color: dim })
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(14.0)),
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into()
    }
}

/// The right-click context menu overlay: a full-window transparent layer that
/// dismisses on a click (outside the menu), with the menu itself positioned at
/// the cursor anchor on top. Each entry runs its action via `MenuPick`.
fn menu_overlay<'a>(anchor: Point, s: &Base16) -> Element<'a, Message> {
    let (bg, border, fg) = (s.base[1], s.base[2], s.base[5]);
    let mut items = column![].width(Length::Fixed(MENU_W));
    for action in MenuAction::ALL {
        let entry = container(text(action.label()).size(14).color(fg))
            .width(Length::Fill)
            .padding([4, 10]);
        items = items.push(
            mouse_area(entry)
                .on_press(Message::MenuPick(action))
                .interaction(mouse::Interaction::Pointer),
        );
    }
    let menu = container(items)
        .padding(4)
        .style(move |_t| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: border,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });
    // Position the menu at the anchor by padding the top-left of a full-window
    // container (iced has no absolute placement; padding offsets it).
    let positioned = container(menu)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding::ZERO.top(anchor.y).left(anchor.x));
    // The dismiss layer sits under the menu (earlier in the stack), so a click
    // on an entry hits the entry and a click anywhere else closes the menu.
    let dismiss = mouse_area(container(Space::new(Length::Fill, Length::Fill)))
        .on_press(Message::MenuDismiss)
        .on_right_press(Message::MenuDismiss);
    iced::widget::stack![dismiss, positioned].into()
}

/// The compose overlay: a centered card with a message field. Enter sends
/// (handled in `on_key`), Esc cancels.
fn compose_overlay<'a>(compose: &'a Compose, s: &Base16) -> Element<'a, Message> {
    let fg = s.base[5];
    let card = container(
        column![
            text(format!("message {}", compose.label))
                .size(15)
                .color(fg)
                .font(semibold()),
            text_input("message…", &compose.buf)
                .id(compose_id())
                .on_input(Message::ComposeInput)
                .on_submit(Message::ComposeSend)
                .size(15)
                .padding(8)
                .width(Length::Fixed(440.0))
                .style(flat_input(s)),
            row![
                mouse_area(text("send").size(14).color(s.accent[Base16::BLUE]))
                    .on_press(Message::ComposeSend)
                    .interaction(mouse::Interaction::Pointer),
                mouse_area(text("cancel").size(14).color(s.base[3]))
                    .on_press(Message::ComposeCancel)
                    .interaction(mouse::Interaction::Pointer),
            ]
            .spacing(16),
        ]
        .spacing(12),
    )
    .padding(18)
    .style({
        let (bg1, border) = (s.base[1], s.base[2]);
        move |_t| container::Style {
            background: Some(Background::Color(bg1)),
            border: Border {
                color: border,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        }
    });
    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

/// Frameless text-input style: transparent background, no border (the filter's
/// underline is drawn separately). Returns a closure capturing owned colors.
fn bare_input(s: &Base16) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    let (dim, fg) = (s.base[3], s.base[5]);
    let sel = Color {
        a: 0.35,
        ..s.accent[Base16::BLUE]
    };
    move |_t, _st| text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        icon: dim,
        placeholder: dim,
        value: fg,
        selection: sel,
    }
}

/// Flat text-input style: a faint filled box, thin border, no rounding (used by
/// the compose overlay). Returns a closure capturing owned colors.
fn flat_input(s: &Base16) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    let (bg, border, dim, fg) = (s.base[1], s.base[2], s.base[3], s.base[5]);
    let sel = Color {
        a: 0.35,
        ..s.accent[Base16::BLUE]
    };
    move |_t, _st| text_input::Style {
        background: Background::Color(bg),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        icon: dim,
        placeholder: dim,
        value: fg,
        selection: sel,
    }
}

fn semibold() -> Font {
    Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    }
}

/// The corral "pen" mark: a square frame enclosing three dots (`∴`), the same
/// geometry as the tray icon (daemon `icon.rs`). Drawn on a canvas.
struct Mark {
    color: Color,
}

impl canvas::Program<Message> for Mark {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let n = bounds.width.min(bounds.height);
        let lo = n * 0.14;
        let side = n * 0.72;
        let rect = canvas::Path::rectangle(Point::new(lo, lo), Size::new(side, side));
        frame.stroke(
            &rect,
            canvas::Stroke::default()
                .with_width((n / 13.0).max(1.0))
                .with_color(self.color),
        );
        let dr = n * 0.09;
        for (fx, fy) in [(0.5, 0.40), (0.375, 0.61), (0.625, 0.61)] {
            let dot = canvas::Path::circle(Point::new(n * fx, n * fy), dr);
            frame.fill(&dot, self.color);
        }
        vec![frame.into_geometry()]
    }
}

/// Build a compose target for an agent, or `None` if it cannot be messaged.
fn compose_for(agent: &Agent) -> Option<Compose> {
    // Title + cwd basename, matching the TUI's compose label (ui::focus_label).
    let title = agent.title.as_deref().unwrap_or("(unnamed)");
    let cwd = agent.cwd.as_deref().unwrap_or("?");
    let label = format!("{title} · {}", basename(cwd));
    let target = match agent.origin {
        Origin::Live => ComposeTarget::Live(agent.socket_path.clone()),
        Origin::Dormant => ComposeTarget::Dormant {
            cwd: agent.cwd.clone()?,
            resume_command: agent.resume_argv()?,
            mode: agent.launch_mode(),
        },
    };
    Some(Compose {
        target,
        label,
        buf: String::new(),
    })
}

/// Go to an agent: focus a live window, or resume a dormant session.
fn activate(
    agent: &Agent,
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

/// The age string a column shows.
fn card_age(
    agent: &Agent,
    column: Column,
    in_state: &HashMap<PathBuf, String>,
    quiet: &HashMap<PathBuf, String>,
    dormant_ages: &HashMap<String, String>,
) -> String {
    match column {
        Column::Running => quiet.get(&agent.socket_path).cloned().unwrap_or_default(),
        Column::RequiresAction | Column::Idle => in_state
            .get(&agent.socket_path)
            .cloned()
            .unwrap_or_default(),
        Column::Dormant => agent
            .session_id
            .as_deref()
            .and_then(|s| dormant_ages.get(s))
            .cloned()
            .unwrap_or_default(),
    }
}

/// The accent color for an agent's state, from the active base16 scheme.
/// Linear blend of two colors (t=0 -> a, t=1 -> b); result is opaque.
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

fn state_color(agent: &Agent, s: &Base16) -> Color {
    match agent.origin {
        Origin::Dormant => s.base[3],
        Origin::Live => match agent.state {
            State::RequiresAction => s.accent[Base16::RED],
            State::Running => s.accent[Base16::GREEN],
            State::Idle => s.base[5],
        },
    }
}

/// Accent color for a cwd pill: a stable hash of the full path into the eight
/// base16 accents (`core::palette`), so a directory reads in the same color
/// across the board and the eye groups cards by color.
fn cwd_color(cwd: &str, s: &Base16) -> Color {
    s.accent[color_index(cwd, s.accent.len())]
}

/// Rec. 601 luma, used to pick a contrasting text color for a pill.
fn luma(c: Color) -> f32 {
    0.299 * c.r + 0.587 * c.g + 0.114 * c.b
}

/// True when the theme's background is darker than its foreground: a dark
/// scheme, where pills get brightened for punch (a light theme is left alone).
fn is_dark(s: &Base16) -> bool {
    luma(s.base[0]) < luma(s.base[5])
}

/// A cwd pill's (background, text) colors. The background is the directory's
/// stable accent, brightened toward white on a dark theme so the chip reads as
/// a bright, saturated tag against the dark card (Solarized's mid-tone accents
/// look dull raw). The text is near-black or near-white by the pill's own luma,
/// so it stays legible whatever accent lands. A dormant card mutes the chip
/// toward the card surface — opaque, not translucent, which looked washed out.
fn cwd_pill_colors(cwd: &str, s: &Base16, dormant: bool) -> (Color, Color) {
    let mut bg = cwd_color(cwd, s);
    if is_dark(s) {
        bg = mix(bg, Color::WHITE, 0.22);
    }
    if dormant {
        bg = mix(bg, s.base[1], 0.5);
    }
    let text = if luma(bg) > 0.5 { s.base[0] } else { s.base[7] };
    (bg, text)
}

/// A muted gray pill (matching the footer's keycap fill) for a plain tag —
/// the kind badge and the `hidden` badge — so both read as tags distinct from
/// the colored cwd pill and the plain activity text.
fn tag_pill<'a>(text_str: &'a str, s: &Base16) -> Element<'a, Message> {
    let (bg, fg) = (s.base[2], s.base[4]);
    container(text(text_str).size(11).color(fg))
        .padding([1, 6])
        .style(move |_t| container::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                radius: 6.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// Replace a leading `$HOME` with `~` for a compact path.
fn tilde(path: &str) -> String {
    match std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()) {
        Some(home) if path == home => "~".into(),
        Some(home) if path.starts_with(&format!("{home}/")) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

/// Whether the desktop prefers a dark appearance (freedesktop settings portal;
/// 1 = dark, 2 = light). Defaults to dark if the portal is absent.
fn system_prefers_dark() -> bool {
    let out = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply=literal",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ])
        .output();
    match out {
        Ok(o) => !String::from_utf8_lossy(&o.stdout).contains("uint32 2"),
        Err(_) => true,
    }
}
