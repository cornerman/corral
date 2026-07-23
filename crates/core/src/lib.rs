//! Shared foundation for both corral binaries. The board (`corral`) and the
//! daemon (`corrald`) each read the same filesystem registry and drive agents
//! the same way, so discovery, prompt delivery, and the launch seam live here
//! once. Everything UI-specific (the board's ratatui, the daemon's tray) stays
//! in its own crate; this crate stays dependency-light on purpose.

pub mod approved_commands;
pub mod click;
pub mod curation;
pub mod discovery;
pub mod engine;
pub mod focus;
pub mod history;
pub mod launch;
pub mod menu;
pub mod model;
pub mod nav;
pub mod palette;
pub mod paths;
pub mod picker;
pub mod placement;
pub mod prompt;
pub mod transition;
pub mod watch;
