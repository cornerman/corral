//! The dashboard: the flat egui presentation of the attention board. It owns a
//! `core::engine::Engine` (the shared registry-reflect loop) and, each frame,
//! ticks it and draws the four columns of flat cards. A filter line narrows the
//! cards by matching the query against each card's whole content (title, path,
//! activity, state). Clicking a card goes to that agent (focus a live window via
//! sway, or resume a dormant session via kitty). The top bar spawns a fresh
//! agent.

use std::path::Path;
use std::time::Duration;

use corral_core::focus::{SwayFocuser, WindowFocuser};
use corral_core::launch::{self, KittyLauncher, Launcher};
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::{engine::Engine, paths};

use egui::{Frame, Margin, Rect, RichText, Sense, TextEdit};

pub struct Dashboard {
    engine: Engine,
    focuser: SwayFocuser,
    launcher: KittyLauncher,
    /// Fuzzy/substring filter over card content; empty shows everything.
    filter: String,
    /// The filter field grabs focus once, so typing filters immediately.
    focused_once: bool,
    /// Last action feedback, shown in the top bar.
    status: String,
}

impl Dashboard {
    pub fn new() -> Self {
        let dir = paths::registry_dir().expect("registry dir (set $HOME or $CORRAL_REGISTRY_DIR)");
        Self {
            engine: Engine::new(dir),
            focuser: SwayFocuser,
            launcher: KittyLauncher,
            filter: String::new(),
            focused_once: false,
            status: String::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.engine.tick();
        // Keep the board live: repaint on the scan cadence even without input.
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        let dark = ui.ctx().theme() == egui::Theme::Dark;
        let scheme = theme::scheme(dark);
        let in_state = self.engine.in_state_ages();
        let quiet = self.engine.quiet_ages();

        // Deferred out of the render borrow.
        let mut go: Option<Agent> = None;
        let mut spawn = false;

        ui.horizontal(|ui| {
            ui.heading(RichText::new(format!("{MARK} corral")).strong());
            ui.add_space(8.0);
            let field = ui.add(
                TextEdit::singleline(&mut self.filter)
                    .hint_text("filter…")
                    .desired_width(260.0),
            );
            if !self.focused_once {
                field.request_focus();
                self.focused_once = true;
            }
            if ui.button("+ new").clicked() {
                spawn = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).weak());
                }
            });
        });
        ui.add_space(6.0);

        let filter = self.filter.clone();
        let board = self.engine.board();
        let dormant_ages = self.engine.dormant_ages();
        ui.columns(Column::ALL.len(), |cols| {
            for (i, column) in Column::ALL.into_iter().enumerate() {
                let ui = &mut cols[i];
                let agents: Vec<&Agent> = board
                    .column(column)
                    .into_iter()
                    .filter(|a| matches(a, &filter))
                    .collect();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(column.title()).strong());
                    ui.label(RichText::new(format!("{}", agents.len())).weak());
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt(column.title())
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for agent in agents {
                            let age = card_age(agent, column, &in_state, &quiet, dormant_ages);
                            if card(ui, agent, &scheme, age).clicked() {
                                go = Some(agent.clone());
                            }
                        }
                    });
            }
        });

        if spawn {
            let cwd = launch::default_cwd(None);
            self.status = match self.launcher.spawn(&cwd, None) {
                Ok(()) => format!("spawned in {}", tilde(&cwd.to_string_lossy())),
                Err(e) => format!("spawn: {e}"),
            };
        }
        if let Some(agent) = go {
            self.status = match activate(&agent, &self.focuser, &self.launcher) {
                Ok(()) => format!("→ {}", agent.title.as_deref().unwrap_or("agent")),
                Err(e) => e,
            };
        }
    }
}

use crate::theme;
use crate::MARK;

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

/// The state word used for filtering (so "running" or "dormant" narrows).
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

/// Draw one flat agent card (no box): a thin state-colored left bar, the title,
/// the `~`-abbreviated cwd, and an activity·age line. Returns the click response.
fn card(ui: &mut egui::Ui, agent: &Agent, scheme: &theme::Base16, age: String) -> egui::Response {
    let accent = state_color(agent, scheme);
    let inner = Frame::default()
        .inner_margin(Margin {
            left: 12,
            right: 8,
            top: 5,
            bottom: 5,
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
    // The state indicator: a thin left accent bar (thicker on hover).
    let w = if resp.hovered() { 3.0 } else { 2.0 };
    ui.painter().rect_filled(
        Rect::from_min_max(rect.left_top(), egui::pos2(rect.left() + w, rect.bottom())),
        0.0,
        accent,
    );
    ui.add_space(3.0);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The age string a column shows: time blocked (Requires Action), idle (Idle),
/// quiet (Running), or record age (Dormant).
fn card_age(
    agent: &Agent,
    column: Column,
    in_state: &std::collections::HashMap<std::path::PathBuf, String>,
    quiet: &std::collections::HashMap<std::path::PathBuf, String>,
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
fn state_color(agent: &Agent, scheme: &theme::Base16) -> egui::Color32 {
    match agent.origin {
        Origin::Dormant => scheme.base[3], // dim
        Origin::Live => match agent.state {
            State::RequiresAction => scheme.accent[theme::Base16::RED],
            State::Running => scheme.accent[theme::Base16::GREEN],
            State::Idle => scheme.base[5], // foreground
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
