//! The dashboard: the egui presentation of the attention board. It owns a
//! `core::engine::Engine` (the shared registry-reflect loop) and, each frame,
//! ticks it and draws the four columns of cards. Clicking a card goes to that
//! agent (focus a live window via sway, or resume a dormant session via kitty),
//! exactly as the TUI's Enter does. The top bar spawns a fresh agent.

use std::path::Path;
use std::time::Duration;

use corral_core::focus::{SwayFocuser, WindowFocuser};
use corral_core::launch::{self, KittyLauncher, Launcher};
use corral_core::model::{Agent, Column, Origin, State};
use corral_core::{engine::Engine, paths};

use egui::{Color32, RichText};

use crate::theme;
use crate::MARK;

pub struct Dashboard {
    engine: Engine,
    focuser: SwayFocuser,
    launcher: KittyLauncher,
    /// Last action feedback, shown in the top bar.
    status: String,
}

impl Dashboard {
    pub fn new() -> Self {
        // The registry dir must resolve; main() already checked, but be safe.
        let dir = paths::registry_dir().expect("registry dir (set $HOME or $CORRAL_REGISTRY_DIR)");
        Self {
            engine: Engine::new(dir),
            focuser: SwayFocuser,
            launcher: KittyLauncher,
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

        // An action deferred out of the render borrow: the clicked agent (go)
        // and whether the spawn button was pressed.
        let mut go: Option<Agent> = None;
        let mut spawn = false;

        ui.horizontal(|ui| {
            ui.heading(RichText::new(format!("{MARK} corral")).strong());
            ui.separator();
            if ui.button("+ new agent").clicked() {
                spawn = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).weak());
                }
            });
        });
        ui.separator();

        let board = self.engine.board();
        let dormant_ages = self.engine.dormant_ages();
        ui.columns(Column::ALL.len(), |cols| {
            for (i, column) in Column::ALL.into_iter().enumerate() {
                let ui = &mut cols[i];
                let agents = board.column(column);
                ui.horizontal(|ui| {
                    ui.heading(RichText::new(column.title()).size(15.0));
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

/// Go to an agent: focus a live window, or resume a dormant session. Mirrors
/// the TUI's `activate`.
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

/// Draw one agent card (title, cwd, activity line) with a state-colored accent
/// bar, and return its click response.
fn card(ui: &mut egui::Ui, agent: &Agent, scheme: &theme::Base16, age: String) -> egui::Response {
    let accent = state_color(agent, scheme);
    let frame = egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(8, 6))
        .outer_margin(egui::Margin::symmetric(0, 3));
    let resp = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(accent).size(10.0));
                    ui.label(RichText::new(agent.title.as_deref().unwrap_or("(unnamed)")).strong());
                });
                if let Some(cwd) = &agent.cwd {
                    ui.label(RichText::new(tilde(cwd)).weak().small());
                }
                let info = agent.activity.as_deref().unwrap_or("");
                ui.label(RichText::new(format!("{info}  ·  {age}").trim().to_owned()).small());
            });
        })
        .response;
    resp.interact(egui::Sense::click()).on_hover_text("go")
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
fn state_color(agent: &Agent, scheme: &theme::Base16) -> Color32 {
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
