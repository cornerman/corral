//! corral-gui: a desktop (egui/eframe) attention board, a parallel presentation
//! shell to the ratatui `corral`. This is the proof-spike: it opens a themed
//! window (base16 Solarized, following the system light/dark preference). The
//! dashboard (columns of cards over the registry) builds on top of it.

mod dashboard;
mod theme;

use dashboard::Dashboard;

/// The corral mark: a pen (⟦ ⟧) enclosing three dots (∴), matching the TUI
/// footer glyph and the tray icon.
pub const MARK: &str = "⟦∴⟧";

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
            // Provide both appearances up front; egui then follows the system
            // light/dark preference on its own, no per-frame theme juggling.
            cc.egui_ctx.set_visuals_of(
                egui::Theme::Dark,
                theme::visuals(&theme::SOLARIZED_DARK, true),
            );
            cc.egui_ctx.set_visuals_of(
                egui::Theme::Light,
                theme::visuals(&theme::SOLARIZED_LIGHT, false),
            );
            Ok(Box::new(App {
                dashboard: Dashboard::new(),
            }))
        }),
    )
}

struct App {
    dashboard: Dashboard,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.dashboard.ui(ui);
    }
}
