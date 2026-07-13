//! The dashboard: the flat egui attention board. It owns a
//! `core::engine::Engine` (the shared registry-reflect loop) and, each frame,
//! ticks it and draws a centered filter bar over four columns of flat cards.
//!
//! Interaction mirrors the TUI. The board is in command mode by default; `/`
//! focuses the centered filter, which narrows cards by their whole content
//! (title, path, activity, state). Keys: arrows / `hjkl` move the selection,
//! `Enter` goes (focus a live window via sway, resume a dormant session via
//! kitty), `Shift+Enter` spawns in the selection's dir, `m` messages (a compose
//! overlay), `d` dismisses (close a live window / forget a dormant record),
//! `Esc` clears the filter then quits, `q` quits.

use std::path::{Path, PathBuf};
use std::time::Duration;

use corral_core::focus::{SwayFocuser, WindowFocuser};
use corral_core::launch::{self, KittyLauncher, Launcher};
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::{engine::Engine, nav, paths, prompt};

use egui::{Color32, FontId, Frame, Key, Margin, Modifiers, Rect, RichText, Sense, TextEdit};

use crate::theme;

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
    Dormant { cwd: String, resume: String },
}

/// A deferred action, resolved after the render borrow is released.
enum Act {
    None,
    Go,
    Spawn,
    Message,
    Dismiss,
    Quit,
}

pub struct Dashboard {
    engine: Engine,
    focuser: SwayFocuser,
    launcher: KittyLauncher,
    dir: PathBuf,
    filter: String,
    /// Flat selection index across the (filtered) columns, TUI-style.
    selected: usize,
    compose: Option<Compose>,
    status: String,
}

impl Dashboard {
    pub fn new() -> Self {
        let dir = paths::registry_dir().expect("registry dir (set $HOME or $CORRAL_REGISTRY_DIR)");
        Self {
            engine: Engine::new(dir.clone()),
            focuser: SwayFocuser,
            launcher: KittyLauncher,
            dir,
            filter: String::new(),
            selected: 0,
            compose: None,
            status: String::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.engine.tick();
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        // The compose overlay, when open, captures all input.
        if self.compose.is_some() {
            self.compose_overlay(ui);
            return;
        }

        // Key hints along the bottom, like the TUI footer. Reserved first so
        // the columns fill the space above it.
        let _hints = egui::Panel::bottom(egui::Id::new("hints"))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "arrows move  ·  Enter go  ·  Shift+Enter new  ·  \
                         m message  ·  d dismiss  ·  / filter  ·  q quit",
                    )
                    .weak()
                    .small(),
                );
            });

        let dark = ui.ctx().theme() == egui::Theme::Dark;
        let scheme = theme::scheme(dark);
        let in_state = self.engine.in_state_ages();
        let quiet = self.engine.quiet_ages();

        // Snapshot the filtered columns (owned) so key handling and delivery do
        // not fight the board borrow.
        let filter = self.filter.clone();
        let columns: Vec<Vec<Agent>> = {
            let board = self.engine.board();
            Column::ALL
                .iter()
                .map(|c| {
                    board
                        .column(*c)
                        .into_iter()
                        .filter(|a| matches(a, &filter))
                        .cloned()
                        .collect()
                })
                .collect()
        };
        let counts: [usize; 4] = std::array::from_fn(|i| columns[i].len());
        let total: usize = counts.iter().sum();
        if self.selected >= total {
            self.selected = total.saturating_sub(1);
        }
        let dormant_ages = self.engine.dormant_ages().clone();

        // --- top: mark + status, then the centered filter bar ---
        let mut want_focus_filter = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("corral").weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ new").clicked() {
                    let cwd = launch::default_cwd(self.selected_agent(&columns).and_then(|a| a.cwd.as_deref()));
                    self.status = match self.launcher.spawn(&cwd, None) {
                        Ok(()) => format!("spawned in {}", tilde(&cwd.to_string_lossy())),
                        Err(e) => format!("spawn: {e}"),
                    };
                }
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).weak());
                }
            });
        });
        ui.add_space(8.0);
        let mut filter_resp = None;
        ui.vertical_centered(|ui| {
            let w = (ui.available_width() * 0.5).clamp(240.0, 560.0);
            // Frameless, left-aligned text (centering the text makes the caret
            // jump); the field itself is centered on screen, with a thin
            // underline instead of a filled box.
            let r = ui.add_sized(
                [w, 28.0],
                TextEdit::singleline(&mut self.filter)
                    .frame(egui::Frame::NONE)
                    .hint_text("filter…")
                    .font(FontId::proportional(18.0)),
            );
            ui.painter().hline(
                r.rect.left()..=r.rect.right(),
                r.rect.bottom() + 2.0,
                egui::Stroke::new(1.0, scheme.base[2]),
            );
            filter_resp = Some(r);
        });
        let filter_resp = filter_resp.unwrap();
        let filtering = filter_resp.has_focus();
        ui.add_space(14.0);

        // --- keyboard ---
        let mut act = Act::None;
        ui.input_mut(|i| {
            // Navigation (Up/Down in both modes; Left/Right only in command
            // mode, so the text cursor still works while filtering).
            if i.consume_key(Modifiers::NONE, Key::ArrowDown)
                || (!filtering && i.consume_key(Modifiers::NONE, Key::J))
            {
                self.selected = nav::move_row(self.selected, &counts, true);
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp)
                || (!filtering && i.consume_key(Modifiers::NONE, Key::K))
            {
                self.selected = nav::move_row(self.selected, &counts, false);
            }
            if (!filtering && i.consume_key(Modifiers::NONE, Key::ArrowRight))
                || (!filtering && i.consume_key(Modifiers::NONE, Key::L))
            {
                self.selected = nav::move_col(self.selected, &counts, true);
            }
            if (!filtering && i.consume_key(Modifiers::NONE, Key::ArrowLeft))
                || (!filtering && i.consume_key(Modifiers::NONE, Key::H))
            {
                self.selected = nav::move_col(self.selected, &counts, false);
            }
            if i.consume_key(Modifiers::SHIFT, Key::Enter) {
                act = Act::Spawn;
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                act = Act::Go;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                if filtering {
                    // handled below by surrendering focus
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                } else {
                    act = Act::Quit;
                }
            }
            if !filtering {
                if i.consume_key(Modifiers::NONE, Key::Slash) {
                    want_focus_filter = true;
                }
                if i.consume_key(Modifiers::NONE, Key::M) {
                    act = Act::Message;
                }
                if i.consume_key(Modifiers::NONE, Key::D) {
                    act = Act::Dismiss;
                }
                if i.consume_key(Modifiers::NONE, Key::Q) {
                    act = Act::Quit;
                }
            }
        });
        if want_focus_filter {
            filter_resp.request_focus();
        }
        if filtering && ui.input(|i| i.key_pressed(Key::Escape)) {
            filter_resp.surrender_focus();
        }

        // --- columns ---
        let mut flat = 0usize;
        ui.columns(Column::ALL.len(), |cols| {
            for (i, column) in Column::ALL.into_iter().enumerate() {
                let ui = &mut cols[i];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(column.title()).strong());
                    ui.label(RichText::new(format!("{}", counts[i])).weak());
                });
                ui.separator();
                let base = flat;
                egui::ScrollArea::vertical()
                    .id_salt(column.title())
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (j, agent) in columns[i].iter().enumerate() {
                            let idx = base + j;
                            let age = card_age(agent, column, &in_state, &quiet, &dormant_ages);
                            let resp = card(ui, agent, &scheme, age, idx == self.selected);
                            if resp.clicked() {
                                self.selected = idx;
                                act = Act::Go;
                            }
                        }
                    });
                flat += counts[i];
            }
        });

        self.run(act, &columns);
    }

    /// The currently selected agent within the filtered columns, if any.
    fn selected_agent<'a>(&self, columns: &'a [Vec<Agent>]) -> Option<&'a Agent> {
        columns.iter().flatten().nth(self.selected)
    }

    /// Execute a deferred action against the current selection.
    fn run(&mut self, act: Act, columns: &[Vec<Agent>]) {
        let agent = self.selected_agent(columns).cloned();
        match act {
            Act::None => {}
            Act::Go => {
                if let Some(a) = agent {
                    self.status = match activate(&a, &self.focuser, &self.launcher) {
                        Ok(()) => format!("→ {}", a.title.as_deref().unwrap_or("agent")),
                        Err(e) => e,
                    };
                }
            }
            Act::Spawn => {
                let cwd = launch::default_cwd(agent.as_ref().and_then(|a| a.cwd.as_deref()));
                self.status = match self.launcher.spawn(&cwd, None) {
                    Ok(()) => format!("spawned in {}", tilde(&cwd.to_string_lossy())),
                    Err(e) => format!("spawn: {e}"),
                };
            }
            Act::Message => {
                if let Some(a) = agent {
                    self.compose = compose_for(&a);
                    if self.compose.is_none() {
                        self.status = "cannot message: no target".into();
                    }
                }
            }
            Act::Dismiss => {
                if let Some(a) = agent {
                    self.status = self.dismiss(&a);
                }
            }
            Act::Quit => std::process::exit(0),
        }
    }

    /// `d`: close a live agent's window, or forget a dormant record.
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

    /// The `m` compose overlay: a centered window with a message field. Enter
    /// sends, Esc cancels.
    fn compose_overlay(&mut self, ui: &mut egui::Ui) {
        let mut send = false;
        let mut cancel = false;
        let compose = self.compose.as_mut().expect("compose open");
        egui::Window::new(format!("message {}", compose.label))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let r = ui.add(
                    TextEdit::singleline(&mut compose.buf)
                        .hint_text("message…")
                        .desired_width(420.0),
                );
                r.request_focus();
                ui.horizontal(|ui| {
                    if ui.button("send").clicked() {
                        send = true;
                    }
                    if ui.button("cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if ui.input(|i| i.key_pressed(Key::Enter)) {
            send = true;
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            cancel = true;
        }
        if send {
            let compose = self.compose.take().expect("compose open");
            let text = compose.buf.trim().to_string();
            if !text.is_empty() {
                self.status = self.deliver(&compose.target, &compose.label, &text);
            }
        } else if cancel {
            self.compose = None;
        }
    }

    /// Deliver an operator message: to a live socket, or by resuming a dormant
    /// session with the message as its first prompt. Ungated (operator trust).
    fn deliver(&self, target: &ComposeTarget, label: &str, text: &str) -> String {
        match target {
            ComposeTarget::Live(socket) => match prompt::send_prompt(socket, text) {
                Ok(()) => format!("sent to {label}"),
                Err(e) => format!("send: {e}"),
            },
            ComposeTarget::Dormant { cwd, resume } => {
                match self.launcher.resume(Path::new(cwd), resume, Some(text)) {
                    Ok(()) => format!("resuming {label} to deliver"),
                    Err(e) => format!("resume: {e}"),
                }
            }
        }
    }
}

/// Build a compose target for an agent, or `None` if it cannot be messaged.
fn compose_for(agent: &Agent) -> Option<Compose> {
    let label = agent.title.clone().unwrap_or_else(|| "agent".into());
    let target = match agent.origin {
        Origin::Live => ComposeTarget::Live(agent.socket_path.clone()),
        Origin::Dormant => ComposeTarget::Dormant {
            cwd: agent.cwd.clone()?,
            resume: agent.resume.clone()?,
        },
    };
    Some(Compose {
        target,
        label,
        buf: String::new(),
    })
}

/// Whether an agent's card matches the filter: every whitespace-separated term
/// must appear (case-insensitive) somewhere in the card's content.
fn matches(agent: &Agent, filter: &str) -> bool {
    let q = filter.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = format!(
        "{} {} {} {}",
        agent.title.as_deref().unwrap_or(""),
        agent.cwd.as_deref().unwrap_or(""),
        agent.activity.as_deref().unwrap_or(""),
        state_word(agent),
    )
    .to_lowercase();
    q.split_whitespace().all(|term| hay.contains(term))
}

/// The state word used for filtering (so "running"/"dormant" narrows).
fn state_word(agent: &Agent) -> &'static str {
    match agent.origin {
        Origin::Dormant => "dormant",
        Origin::Live => match agent.state {
            State::RequiresAction => "requires action",
            State::Running => "running",
            State::Idle => "idle",
        },
    }
}

/// Go to an agent: focus a live window, or resume a dormant session.
fn activate(
    agent: &Agent,
    focuser: &dyn WindowFocuser,
    launcher: &dyn Launcher,
) -> Result<(), String> {
    match agent.origin {
        Origin::Live => focuser.focus(agent).map_err(|e| format!("focus: {e}")),
        Origin::Dormant => match (&agent.cwd, &agent.resume) {
            (Some(cwd), Some(resume)) => launcher
                .resume(Path::new(cwd), resume, None)
                .map_err(|e| format!("resume: {e}")),
            _ => Err("resume: dormant record missing cwd/resume".into()),
        },
    }
}

/// Draw one flat agent card: a thin state-colored left bar, the title, the
/// `~`-abbreviated cwd, and an activity·age line. The selected card gets a
/// faint background and a full-thickness bar. Returns the click response.
fn card(
    ui: &mut egui::Ui,
    agent: &Agent,
    scheme: &theme::Base16,
    age: String,
    selected: bool,
) -> egui::Response {
    let accent = state_color(agent, scheme);
    // Faint accent tint for the selected row (not the saturated Solarized
    // base01, which reads as a heavy teal block).
    let fill = if selected {
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 28)
    } else {
        Color32::TRANSPARENT
    };
    let inner = Frame::default()
        .fill(fill)
        .inner_margin(Margin {
            left: 14,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(agent.title.as_deref().unwrap_or("(unnamed)")).strong());
            if let Some(cwd) = &agent.cwd {
                ui.label(RichText::new(tilde(cwd)).weak().small());
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
                ui.label(RichText::new(parts.join("  ·  ")).weak().small());
            }
        });
    let rect = inner.response.rect;
    let id = ui.make_persistent_id(("card", &agent.socket_path, &agent.session_id));
    let resp = ui.interact(rect, id, Sense::click());
    let w = if selected || resp.hovered() { 3.0 } else { 2.0 };
    ui.painter().rect_filled(
        Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + w, rect.bottom())),
        0.0,
        accent,
    );
    ui.add_space(6.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The age string a column shows.
fn card_age(
    agent: &Agent,
    column: Column,
    in_state: &std::collections::HashMap<PathBuf, String>,
    quiet: &std::collections::HashMap<PathBuf, String>,
    dormant_ages: &std::collections::HashMap<String, String>,
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
fn state_color(agent: &Agent, scheme: &theme::Base16) -> Color32 {
    match agent.origin {
        Origin::Dormant => scheme.base[3],
        Origin::Live => match agent.state {
            State::RequiresAction => scheme.accent[theme::Base16::RED],
            State::Running => scheme.accent[theme::Base16::GREEN],
            State::Idle => scheme.base[5],
        },
    }
}

/// Replace a leading `$HOME` with `~` for a compact path.
fn tilde(path: &str) -> String {
    match std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()) {
        Some(home) if path == home => "~".into(),
        Some(home) if path.starts_with(&format!("{home}/")) => format!("~{}", &path[home.len()..]),
        _ => path.to_string(),
    }
}
