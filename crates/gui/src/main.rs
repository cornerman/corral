//! corral-gui: a desktop (iced) attention board, a parallel presentation shell
//! to the ratatui `corral`. A flat, crisp, base16-themed window following the
//! system light/dark preference; the TUI stays the zero-friction terminal path.
//!
//! `--launcher` turns it into an ephemeral rofi-style popup: it boots focused on
//! the filter, exits after go / new, and dismisses on focus loss, so a WM keybind
//! can respawn a fresh one each summon. A distinct `app_id` (`corral-launcher`)
//! lets a WM window rule float/center only the popup, not a normal window.

mod dashboard;
mod theme;

use dashboard::Board;

fn main() -> iced::Result {
    let launcher = std::env::args().any(|a| a == "--launcher");
    let app_id = if launcher {
        "corral-launcher"
    } else {
        "corral"
    };
    let window = iced::window::Settings {
        size: iced::Size::new(900.0, 600.0),
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: app_id.to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    iced::application("corral", Board::update, Board::view)
        .subscription(Board::subscription)
        .theme(Board::theme)
        .window(window)
        .run_with(move || Board::new(launcher))
}
