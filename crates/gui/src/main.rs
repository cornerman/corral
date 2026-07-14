//! corral-gui: a desktop (iced) attention board, a parallel presentation shell
//! to the ratatui `corral`. A flat, crisp, base16-themed window following the
//! system light/dark preference; the TUI stays the zero-friction terminal path.

mod dashboard;
mod theme;

use dashboard::Board;

fn main() -> iced::Result {
    iced::application("corral", Board::update, Board::view)
        .subscription(Board::subscription)
        .theme(Board::theme)
        .window_size((900.0, 600.0))
        .run_with(|| (Board::new(), iced::Task::none()))
}
