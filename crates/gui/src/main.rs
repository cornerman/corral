//! corral-gui: iced skeleton (build de-risk).
use iced::widget::text;

fn main() -> iced::Result {
    iced::run("corral", update, view)
}

#[derive(Default)]
struct State;

#[derive(Debug, Clone)]
enum Message {}

fn update(_state: &mut State, _message: Message) {}

fn view(_state: &State) -> iced::Element<'_, Message> {
    text("corral").into()
}
