//! The iced attention board: a flat, crisp presentation shell over the shared
//! `core::engine::Engine`, mirroring the ratatui TUI's look and the full
//! behavior of both boards. Four columns (Requires Action / Idle / Running /
//! Dormant) of flat cards under a centered filter, a thin bottom key-hint bar,
//! base16-themed, following the system light/dark preference.
//!
//! Interaction mirrors the TUI, keeping the egui shell's learnings: `/` focuses
//! the filter (narrows cards by whole content); arrows / `hjkl` move, Enter
//! goes, Shift+Enter spawns, `m` messages (compose overlay), `d` dismisses,
//! `Esc` clears the filter then quits, `q` quits; a two-stage card click (first
//! click selects, click the selected card goes); selection survives a filter
//! clear.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use corral_core::focus::{self, WindowFocuser};
use corral_core::launch::{self, LaunchMode, Launcher, TerminalLauncher};
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::{engine::Engine, nav, palette::color_index, paths, prompt};

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
    Go,
    Spawn,
    /// Window gained (true) or lost (false) focus. Launcher dismisses on blur.
    Focused(bool),
    OpenCompose,
    Dismiss,
    Quit,
    ComposeInput(String),
    ComposeSend,
    ComposeCancel,
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
    dir: PathBuf,
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
        let dir = paths::registry_dir().expect("registry dir (set $HOME or $CORRAL_REGISTRY_DIR)");
        let mut b = Board {
            engine: Engine::new(dir.clone()),
            focuser: focus::detect(),
            launcher: TerminalLauncher,
            dir,
            dark: system_prefers_dark(),
            last_theme_check: Instant::now(),
            filter: String::new(),
            filtering: false,
            launcher_mode,
            focused_once: false,
            selected: 0,
            status: String::new(),
            compose: None,
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
    }

    fn counts(&self) -> [usize; 4] {
        std::array::from_fn(|i| self.columns[i].len())
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
                // Two-stage: first click selects, click on the selected goes.
                if idx == self.selected {
                    return self.act_go();
                }
                self.selected = idx;
                return self.scroll_to_selection();
            }
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

    /// Escape, in stages (never quits — only `q` does). Owns all Escape
    /// handling so it is reliable even when the focused filter field captures
    /// the key event:
    ///   1. compose overlay open -> cancel it;
    ///   2. filter field focused -> blur it, keeping the filter and selection;
    ///   3. otherwise -> clear the filter, keeping the current selection (its
    ///      index stays in range once the full board returns).
    fn escape(&mut self) -> Task<Message> {
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
        // Nothing left to dismiss: exit. Escape peels layers (compose, filter
        // focus, filter text) and only quits at this final stage, so an
        // accidental Escape mid-filter never kills the window.
        iced::exit()
    }

    fn on_key(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        let counts = self.counts();
        // In the compose overlay, Enter sends; Esc (cancel) is owned by
        // `escape()` via the dedicated listener; nothing else.
        if self.compose.is_some() {
            if let keyboard::Key::Named(Named::Enter) = key {
                return self.update(Message::ComposeSend);
            }
            return Task::none();
        }
        match key {
            keyboard::Key::Named(Named::ArrowDown) => {
                self.selected = nav::move_row(self.selected, &counts, true);
            }
            keyboard::Key::Named(Named::ArrowUp) => {
                self.selected = nav::move_row(self.selected, &counts, false);
            }
            // No `filtering` guard on these: a focused filter field captures
            // Left/Right (caret) and letters (typing), so `on_key_press` never
            // delivers them here while focused; when the field is unfocused,
            // they are commands regardless of any applied filter.
            keyboard::Key::Named(Named::ArrowRight) => {
                self.selected = nav::move_col(self.selected, &counts, true);
            }
            keyboard::Key::Named(Named::ArrowLeft) => {
                self.selected = nav::move_col(self.selected, &counts, false);
            }
            keyboard::Key::Named(Named::Enter) => {
                return if mods.shift() {
                    self.act_spawn()
                } else {
                    self.act_go()
                };
            }
            keyboard::Key::Character(c) => match c.as_str() {
                "j" => self.selected = nav::move_row(self.selected, &counts, true),
                "k" => self.selected = nav::move_row(self.selected, &counts, false),
                "l" => self.selected = nav::move_col(self.selected, &counts, true),
                "h" => self.selected = nav::move_col(self.selected, &counts, false),
                "m" => return self.update(Message::OpenCompose),
                "d" => return self.update(Message::Dismiss),
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
            .and_then(|a| a.spawn_command.as_ref().map(|c| (a, c)))
        {
            Some((a, command)) => {
                let cwd = launch::default_cwd(a.cwd.as_deref());
                match self.launcher.launch(&cwd, command, None, &a.launch_mode()) {
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

    fn dismiss(&self, agent: &Agent) -> String {
        match agent.origin {
            Origin::Live => match self.focuser.close(agent) {
                Ok(()) => format!("closing {}", agent.title.as_deref().unwrap_or("agent")),
                Err(e) => format!("close: {e}"),
            },
            Origin::Dormant => match &agent.session_id {
                Some(id) => match std::fs::remove_file(self.dir.join(format!("{id}.json"))) {
                    Ok(()) => "forgot dormant record".into(),
                    Err(e) => format!("dismiss: {e}"),
                },
                None => "dismiss: no session id".into(),
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

        let footer = self.footer(s);

        let content = column![top, cols, footer].spacing(14).padding(12);
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
            let pc = Color {
                a,
                ..cwd_color(cwd, s)
            };
            let ptext = Color { a, ..s.base[0] };
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
        meta_row = meta_row.push(text(&agent.label).size(11).color(dim));
        if let Some(info) = agent.activity.as_deref().filter(|s| !s.is_empty()) {
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
            .interaction(mouse::Interaction::Pointer)
            .into()
    }

    fn footer(&self, s: &Base16) -> Element<'_, Message> {
        let dim = s.base[3];
        let hint = |label: &'static str, msg: Message| -> Element<'_, Message> {
            mouse_area(text(label).size(13).color(dim))
                .on_press(msg)
                .interaction(mouse::Interaction::Pointer)
                .into()
        };
        // Same labels, symbols and order as the TUI footer (ui.rs footer_items).
        row![
            text("↑↓←→ move").size(13).color(dim),
            hint("⏎ go", Message::Go),
            hint("⇧⏎ new", Message::Spawn),
            hint("/ filter", Message::FocusFilter),
            hint("m msg", Message::OpenCompose),
            hint("d delete", Message::Dismiss),
            hint("q quit", Message::Quit),
            Space::new(Length::Fill, 0.0),
            canvas(Mark { color: dim })
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(14.0)),
        ]
        .spacing(22)
        .align_y(Alignment::Center)
        .into()
    }
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
    let label = agent.title.clone().unwrap_or_else(|| "agent".into());
    let target = match agent.origin {
        Origin::Live => ComposeTarget::Live(agent.socket_path.clone()),
        Origin::Dormant => ComposeTarget::Dormant {
            cwd: agent.cwd.clone()?,
            resume_command: agent.resume_command.clone()?,
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
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, &agent.resume_command) {
            (Some(cwd), Some(command)) => launcher
                .launch(Path::new(cwd), command, None, &agent.launch_mode())
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

/// The last path component (the working directory's leaf), shown in the pill.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
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
