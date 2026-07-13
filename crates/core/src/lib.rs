//! Shared foundation for both corral binaries. The board (`corral`) and the
//! daemon (`corrald`) each read the same filesystem registry and drive agents
//! the same way, so discovery, prompt delivery, and the launch seam live here
//! once. Everything UI-specific (the board's ratatui, the daemon's tray) stays
//! in its own crate; this crate stays dependency-light on purpose.

pub mod discovery;
pub mod launch;
pub mod paths;
pub mod prompt;
