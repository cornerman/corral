//! corral-gui: a desktop (egui/eframe) attention board, a parallel presentation
//! shell to the ratatui `corral`. A flat, base16-Solarized window (following the
//! system light/dark preference) showing the four columns of cards, with a
//! filter line that narrows cards by their whole content.

use std::process::Command;
use std::time::{Duration, Instant};

mod dashboard;
mod theme;

use dashboard::Dashboard;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("corral")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([420.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "corral",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx); // flat, airy spacing (once)
            Ok(Box::new(App {
                dashboard: Dashboard::new(),
                dark: system_prefers_dark(),
                last_theme_check: Instant::now(),
            }))
        }),
    )
}

/// Whether the desktop prefers a dark appearance, read from the freedesktop
/// settings portal (`org.freedesktop.appearance color-scheme`: 1 = dark,
/// 2 = light). Zero-dependency shell-out, like corral's other integrations;
/// defaults to dark if the portal is absent. egui/winit does not surface this
/// on Wayland, hence the explicit query.
fn system_prefers_dark() -> bool {
    let out = Command::new("dbus-send")
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
        // "… uint32 2" is the explicit light preference; anything else
        // (dark, no-preference, or no portal) falls to dark.
        Ok(o) => !String::from_utf8_lossy(&o.stdout).contains("uint32 2"),
        Err(_) => true,
    }
}

struct App {
    dashboard: Dashboard,
    /// The current system appearance; re-polled on an interval so a desktop
    /// light/dark switch is picked up live.
    dark: bool,
    last_theme_check: Instant,
}

impl App {
    /// Force the Solarized + flat visuals every frame (so egui's rounded,
    /// shadowed defaults can never leak), re-polling the system appearance
    /// every couple of seconds.
    fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.last_theme_check.elapsed() >= Duration::from_secs(2) {
            self.dark = system_prefers_dark();
            self.last_theme_check = Instant::now();
        }
        ctx.set_visuals(theme::visuals(&theme::scheme(self.dark), self.dark));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sync_theme(ui.ctx());
        // eframe snapshots the root ui's style before our set_visuals, so adopt
        // the fresh visuals here or widgets (buttons, selection) render with
        // egui defaults instead of the palette.
        let style = ui.ctx().style_of(ui.ctx().theme());
        ui.set_style(style.clone());
        // `central_panel` fills the whole area with the palette background;
        // `Frame::default` paints nothing, leaving the window clear color (black).
        egui::Frame::central_panel(&style)
            .inner_margin(egui::Margin::symmetric(18, 12))
            .show(ui, |ui| self.dashboard.ui(ui, self.dark));
    }
}
