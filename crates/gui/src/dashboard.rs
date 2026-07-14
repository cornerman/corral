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
use corral_core::launch::{self, Launcher, TerminalLauncher};
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::{engine::Engine, nav, paths, prompt};

use iced::widget::{
    canvas, column, container, mouse_area, row, scrollable, text, text_input, Space,
};
use iced::{
    keyboard, mouse, Alignment, Background, Border, Color, Element, Font, Length, Point,
    Rectangle, Renderer, Size, Subscription, Task, Theme,
};

use crate::theme::{self, Base16};

/// Fixed card height (points): fits title + cwd + activity·age with padding.
const CARD_H: f32 = 62.0;

/// The filter field's focus id (for programmatic focus/blur).
fn filter_id() -> text_input::Id {
    text_input::Id::new("corral-filter")
}

/// Messages the board reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Key(keyboard::Key, keyboard::Modifiers),
    FilterInput(String),
    FilterSubmit,
    FocusFilter,
    CardClicked(usize),
    Go,
    Spawn,
    OpenCompose,
    Dismiss,
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
    /// Flat selection index across the (filtered) columns, TUI-style.
    selected: usize,
    /// Re-select this agent (by session id, socket) once the columns rebuild,
    /// so clearing the filter keeps the selection despite the index shift.
    reselect: Option<(Option<String>, PathBuf)>,
    status: String,
    compose: Option<Compose>,
    // Snapshot rebuilt each tick / filter change; view and actions read it.
    columns: Vec<Vec<Agent>>,
    in_state: HashMap<PathBuf, String>,
    quiet: HashMap<PathBuf, String>,
    dormant_ages: HashMap<String, String>,
}

impl Board {
    pub fn new() -> Self {
        let dir =
            paths::registry_dir().expect("registry dir (set $HOME or $CORRAL_REGISTRY_DIR)");
        let mut b = Board {
            engine: Engine::new(dir.clone()),
            focuser: focus::detect(),
            launcher: TerminalLauncher,
            dir,
            dark: system_prefers_dark(),
            last_theme_check: Instant::now(),
            filter: String::new(),
            filtering: false,
            selected: 0,
            reselect: None,
            status: String::new(),
            compose: None,
            columns: vec![Vec::new(); Column::ALL.len()],
            in_state: HashMap::new(),
            quiet: HashMap::new(),
            dormant_ages: HashMap::new(),
        };
        b.refresh();
        b
    }

    pub fn scheme(&self) -> Base16 {
        let (dark, light) = theme::selected_pair();
        if self.dark {
            dark
        } else {
            light
        }
    }

    /// Rebuild the filtered column snapshot + age maps from the engine, honoring
    /// any pending re-selection, and clamp the selection.
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
        if let Some((sid, sock)) = self.reselect.take() {
            if let Some(idx) = self.columns.iter().flatten().position(|a| match &sid {
                Some(_) => a.session_id == sid,
                None => a.socket_path == sock,
            }) {
                self.selected = idx;
            }
        }
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
                // Two-stage: first click selects, click on the selected goes.
                if idx == self.selected {
                    return self.act_go();
                }
                self.selected = idx;
            }
            Message::Go => return self.act_go(),
            Message::Spawn => self.act_spawn(),
            Message::OpenCompose => {
                if let Some(a) = self.selected_agent() {
                    self.compose = compose_for(a);
                    if self.compose.is_none() {
                        self.status = "cannot message: no target".into();
                    }
                }
            }
            Message::Dismiss => {
                if let Some(a) = self.selected_agent().cloned() {
                    self.status = self.dismiss(&a);
                }
            }
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
            Message::Key(key, mods) => return self.on_key(key, mods),
        }
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, mods: keyboard::Modifiers) -> Task<Message> {
        use keyboard::key::Named;
        let counts = self.counts();
        // In the compose overlay, Enter sends and Esc cancels; nothing else.
        if self.compose.is_some() {
            if let keyboard::Key::Named(Named::Enter) = key {
                return self.update(Message::ComposeSend);
            }
            if let keyboard::Key::Named(Named::Escape) = key {
                self.compose = None;
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
            keyboard::Key::Named(Named::ArrowRight) if !self.filtering => {
                self.selected = nav::move_col(self.selected, &counts, true);
            }
            keyboard::Key::Named(Named::ArrowLeft) if !self.filtering => {
                self.selected = nav::move_col(self.selected, &counts, false);
            }
            keyboard::Key::Named(Named::Enter) => {
                return if mods.shift() {
                    self.act_spawn();
                    Task::none()
                } else {
                    self.act_go()
                };
            }
            keyboard::Key::Named(Named::Escape) => {
                if self.filtering {
                    // Clear the filter, remembering the selection so it survives
                    // the rebuild, then leave filter mode (blur the field).
                    self.reselect = self
                        .selected_agent()
                        .map(|a| (a.session_id.clone(), a.socket_path.clone()));
                    self.filter.clear();
                    self.filtering = false;
                    self.refresh();
                    return text_input::focus(text_input::Id::new("corral-blur"));
                }
                std::process::exit(0);
            }
            keyboard::Key::Character(c) if !self.filtering => match c.as_str() {
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
        Task::none()
    }

    fn act_go(&mut self) -> Task<Message> {
        if let Some(a) = self.selected_agent().cloned() {
            self.status = match activate(&a, self.focuser.as_ref(), &self.launcher) {
                Ok(()) => format!("→ {}", a.title.as_deref().unwrap_or("agent")),
                Err(e) => e,
            };
        }
        Task::none()
    }

    fn act_spawn(&mut self) {
        let agent = self.selected_agent().cloned();
        self.status = match agent
            .as_ref()
            .and_then(|a| a.spawn_command.as_ref().map(|c| (a, c)))
        {
            Some((a, command)) => {
                let cwd = launch::default_cwd(a.cwd.as_deref());
                match self.launcher.launch(&cwd, command, None) {
                    Ok(()) => format!("spawned in {}", tilde(&cwd.to_string_lossy())),
                    Err(e) => format!("spawn: {e}"),
                }
            }
            None => "spawn: no launchable agent selected".into(),
        };
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
            } => match self.launcher.launch(Path::new(cwd), resume_command, Some(text)) {
                Ok(()) => format!("resuming {label} to deliver"),
                Err(e) => format!("resume: {e}"),
            },
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
            keyboard::on_key_press(|key, mods| Some(Message::Key(key, mods))),
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
        let filter = text_input("type to filter…", &self.filter)
            .id(filter_id())
            .on_input(Message::FilterInput)
            .on_submit(Message::FilterSubmit)
            .size(18)
            .width(Length::Fixed(380.0))
            .padding(6)
            .style(flat_input(s));
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
            let header = row![
                text(col.title()).size(15).color(fg).font(semibold()),
                text(format!("{count}")).size(15).color(dim),
            ]
            .spacing(8);
            let mut list = column![].spacing(6);
            for (j, agent) in self.columns[i].iter().enumerate() {
                let idx = base + j;
                let age = card_age(agent, col, &self.in_state, &self.quiet, &self.dormant_ages);
                list = list.push(self.card(agent, col, s, age, idx));
            }
            base += count;
            let body = scrollable(list).height(Length::Fill).width(Length::Fill);
            cols = cols.push(
                column![header, body]
                    .spacing(8)
                    .width(Length::Fill)
                    .height(Length::Fill),
            );
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
        let dim = s.base[3];
        let fg = s.base[5];
        let accent = state_color(agent, s);

        // Title row: title truncated (clipped) on the left, kind badge right.
        let title = container(
            text(agent.title.clone().unwrap_or_else(|| "(unnamed)".into()))
                .size(14)
                .color(fg)
                .font(semibold())
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true);
        let title_row = row![title, text(&agent.label).size(11).color(dim)]
            .spacing(6)
            .align_y(Alignment::Center);

        let mut body = column![title_row].spacing(2);
        if let Some(cwd) = &agent.cwd {
            body = body.push(
                text(tilde(cwd))
                    .size(12)
                    .color(dim)
                    .wrapping(text::Wrapping::None),
            );
        }
        let info = agent.activity.as_deref().unwrap_or("");
        let mut parts = Vec::new();
        if !info.is_empty() {
            parts.push(info.to_string());
        }
        if !age.is_empty() {
            parts.push(age);
        }
        if !parts.is_empty() {
            body = body.push(
                text(parts.join("  ·  "))
                    .size(12)
                    .color(dim)
                    .wrapping(text::Wrapping::None),
            );
        }

        // Thin state-colored left bar + faint accent tint when selected. Cards
        // are a fixed height (like the TUI): variable content stays aligned,
        // and a fixed bar height sidesteps `Length::Fill` (illegal inside a
        // scrollable — it makes the content report a fill height and panics).
        let bar = container(Space::new(Length::Fixed(if selected { 3.0 } else { 2.0 }), 0.0))
            .height(Length::Fixed(CARD_H))
            .style(move |_t| container::Style {
                background: Some(Background::Color(accent)),
                ..container::Style::default()
            });
        let fill = if selected {
            Some(Color { a: 0.10, ..accent })
        } else {
            None
        };
        let inner = container(body).padding([8, 10]).width(Length::Fill);
        let card = container(row![bar, inner].spacing(0))
            .width(Length::Fill)
            .height(Length::Fixed(CARD_H))
            .clip(true)
            .style(move |_t| container::Style {
                background: fill.map(Background::Color),
                ..container::Style::default()
            });
        let _ = col;
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
        row![
            text("arrows move").size(13).color(dim),
            hint("Enter go", Message::Go),
            hint("Shift+Enter new", Message::Spawn),
            hint("m message", Message::OpenCompose),
            hint("d dismiss", Message::Dismiss),
            hint("/ filter", Message::FocusFilter),
            Space::new(Length::Fill, 0.0),
            canvas(Mark { color: dim })
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(14.0)),
        ]
        .spacing(12)
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

/// Flat text-input style: a faint filled box, thin border, no rounding.
/// Returns a closure capturing owned colors (no borrow of the scheme).
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
                .launch(Path::new(cwd), command, None)
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
        Column::RequiresAction | Column::Idle => {
            in_state.get(&agent.socket_path).cloned().unwrap_or_default()
        }
        Column::Dormant => agent
            .session_id
            .as_deref()
            .and_then(|s| dormant_ages.get(s))
            .cloned()
            .unwrap_or_default(),
    }
}

/// The accent color for an agent's state, from the active base16 scheme.
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
