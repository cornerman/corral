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

impl KittyLauncher {
    /// Launch `kitty --directory <cwd> -e pi <pi_args...>`, detached from the
    /// board.
    ///
    /// `setsid --fork` reparents kitty to init, which matters twice over: the
    /// window outlives the board and leaves no zombie child, and — critically —
    /// the window is no longer a descendant of corral. The focus seam finds a
    /// window by walking up pi's `/proc` parent chain; if the spawned kitty
    /// were a child of corral, that walk would continue past it into corral's
    /// own terminal and could focus or close the board itself. Detaching stops
    /// the walk at `pi -> kitty -> init`. `setsid` exits immediately after
    /// forking, so waiting on it reaps at once.
    fn launch(&self, cwd: &Path, pi_args: &[&str]) -> Result<(), String> {
        let ok = Command::new("setsid")
            .arg("--fork")
            .arg("kitty")
            .arg("--directory")
            .arg(cwd)
            .args(["-e", "pi"])
            .args(pi_args)
            .status()
            .map_err(|e| format!("kitty launch failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("kitty launch returned non-zero".into())
        }
    }
}

impl Launcher for KittyLauncher {
    // One OS window per agent; --directory roots it.
    fn spawn(&self, cwd: &Path) -> Result<(), String> {
        self.launch(cwd, &[])
    }

    // `pi --session <path|id>` reloads that session, keeping its sessionId, so
    // the resumed process reconnects live under the same identity.
    fn resume(&self, cwd: &Path, resume: &str) -> Result<(), String> {
        self.launch(cwd, &["--session", resume])
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
