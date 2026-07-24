//! Key-event capture tool for diagnosing layout issues (e.g. the Neo layout's
//! layer-4 arrow keys not reaching corral). It enables the SAME kitty keyboard
//! enhancement flags the board uses, then prints the full `KeyEvent` for every
//! key you press so we can see exactly what the terminal reports.
//!
//! Run:  cargo run -p corral --example keycap
//! Then: press the layer-4 arrow keys (up/down/left/right) a few times, then
//!       press Ctrl+C to quit. Paste the printed lines back.

use crossterm::event::{
    read, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement};

fn main() {
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    enable_raw_mode().unwrap();
    if enhanced {
        // Match the board: DISAMBIGUATE globally, plus move-mode's extra flags
        // so we see event types (press/release/repeat) and the raw keys.
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
    }

    println!("supports_keyboard_enhancement = {enhanced}\r");
    println!("Press keys (Neo layer-4 arrows especially). Ctrl+C to quit.\r");

    loop {
        match read().unwrap() {
            Event::Key(k) => {
                println!("{k:?}\r");
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
            }
            other => println!("{other:?}\r"),
        }
    }

    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    disable_raw_mode().unwrap();
}
