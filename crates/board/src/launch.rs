//! Agent-spawn seam. The core calls `Launcher::spawn`; the kitty implementation
//! is the only place that knows how a new agent window is started.

use std::path::Path;
use std::process::Command;

use crate::model::Agent;

pub trait Launcher {
    /// Start a fresh pi agent in its own window, rooted at `cwd`.
    fn spawn(&self, cwd: &Path) -> Result<(), String>;

    /// Resume a dormant session in its own window: `resume` is the session-file
    /// path (`pi --session <resume>`), rooted at the session's original `cwd`.
    fn resume(&self, cwd: &Path, resume: &str) -> Result<(), String>;
}

pub struct KittyLauncher;

impl Launcher for KittyLauncher {
    fn spawn(&self, cwd: &Path) -> Result<(), String> {
        // `kitty -e pi` gives one OS window per agent; --directory roots it.
        Command::new("kitty")
            .arg("--directory")
            .arg(cwd)
            .args(["-e", "pi"])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("kitty spawn failed: {e}"))
    }

    fn resume(&self, cwd: &Path, resume: &str) -> Result<(), String> {
        // `pi --session <path|id>` reloads that session, keeping its sessionId,
        // so the resumed process reconnects live under the same identity.
        Command::new("kitty")
            .arg("--directory")
            .arg(cwd)
            .args(["-e", "pi", "--session", resume])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("kitty resume failed: {e}"))
    }
}

/// Default spawn directory: the selected agent's cwd if known, else $HOME.
pub fn default_cwd(selected: Option<&Agent>) -> std::path::PathBuf {
    if let Some(cwd) = selected.and_then(|a| a.cwd.as_ref()) {
        return std::path::PathBuf::from(cwd);
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}
