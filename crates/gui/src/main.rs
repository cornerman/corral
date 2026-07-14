//! corral-gui: iced desktop attention board (skeleton — theme wired, board next).
use iced::widget::text;

mod theme;

fn main() -> iced::Result {
    iced::run("corral", update, view)
}

#[derive(Default)]
struct State;

#[derive(Debug, Clone)]
enum Message {}

fn update(_state: &mut State, _message: Message) {}

fn view(_state: &State) -> iced::Element<'_, Message> {
    // Touch the theme module so its selection path is exercised until the board
    // view consumes it.
    let (dark, _light) = theme::selected_pair();
    text(format!("corral · {}", dark.name)).into()
}
