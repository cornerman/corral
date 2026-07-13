//! corral-gui: a desktop (egui/eframe) attention board, a parallel presentation
//! shell to the ratatui `corral`. A flat, base16-Solarized window (following the
//! system light/dark preference) showing the four columns of cards, with a
//! filter line that narrows cards by their whole content.

use std::process::Command;

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
            // Install the flat Solarized dark+light visuals, then pick the one
            // the desktop prefers.
            theme::install(&cc.egui_ctx);
            let pref = if system_prefers_dark() {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            };
            cc.egui_ctx.set_theme(pref);
            Ok(Box::new(App {
                dashboard: Dashboard::new(),
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // A uniform breathing margin so content never touches the window edge.
        egui::Frame::default()
            .inner_margin(egui::Margin::symmetric(18, 12))
            .show(ui, |ui| self.dashboard.ui(ui));
    }
}
