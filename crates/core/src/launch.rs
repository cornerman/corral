//! Agent-spawn seam. The core calls `Launcher::spawn`; the kitty implementation
//! is the only place that knows how a new agent window is started.

use std::path::Path;
use std::process::Command;

pub trait Launcher {
    /// Start a fresh pi agent in its own window, rooted at `cwd`. An optional
    /// initial `message` is submitted as pi's first prompt (a positional arg),
    /// so a spawn can deliver a message atomically without waiting for the new
    /// session to announce a socket.
    fn spawn(&self, cwd: &Path, message: Option<&str>) -> Result<(), String>;

    /// Resume a dormant session in its own window: `resume` is the session-file
    /// path (`pi --session <resume>`), rooted at the session's original `cwd`.
    /// An optional initial `message` is submitted as the first prompt on
    /// resume (same atomic-delivery reason as `spawn`).
    fn resume(&self, cwd: &Path, resume: &str, message: Option<&str>) -> Result<(), String>;
}

/// Build the `pi` argument vector for a spawn/resume, optionally carrying an
/// initial message as a positional prompt.
///
/// pi's CLI parser (as of 0.80) has no `--` end-of-options marker and treats
/// any argument starting with `-` or `@` as a flag or a file, never a message.
/// A message starting with those would be misparsed, so we prefix a single
/// space to force it onto pi's `messages` list; pi trims it before submitting.
fn pi_args(session: Option<&str>, message: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(s) = session {
        args.push("--session".to_string());
        args.push(s.to_string());
    }
    if let Some(m) = message {
        let guarded = if m.starts_with('-') || m.starts_with('@') {
            format!(" {m}")
        } else {
            m.to_string()
        };
        args.push(guarded);
    }
    args
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
    fn launch(&self, cwd: &Path, pi_args: &[String]) -> Result<(), String> {
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
    fn spawn(&self, cwd: &Path, message: Option<&str>) -> Result<(), String> {
        self.launch(cwd, &pi_args(None, message))
    }

    // `pi --session <path|id>` reloads that session, keeping its sessionId, so
    // the resumed process reconnects live under the same identity.
    fn resume(&self, cwd: &Path, resume: &str, message: Option<&str>) -> Result<(), String> {
        self.launch(cwd, &pi_args(Some(resume), message))
    }
}

/// Default spawn directory: the given cwd if known, else $HOME. Takes a plain
/// cwd (not an `Agent`) so this crate stays free of the board's model.
pub fn default_cwd(cwd: Option<&str>) -> std::path::PathBuf {
    if let Some(cwd) = cwd {
        return std::path::PathBuf::from(cwd);
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::pi_args;

    #[test]
    fn spawn_no_message_is_empty() {
        assert!(pi_args(None, None).is_empty());
    }

    #[test]
    fn resume_passes_session() {
        assert_eq!(pi_args(Some("/s.json"), None), ["--session", "/s.json"]);
    }

    #[test]
    fn message_is_a_positional_arg() {
        assert_eq!(pi_args(None, Some("hello")), ["hello"]);
        assert_eq!(
            pi_args(Some("/s.json"), Some("hello")),
            ["--session", "/s.json", "hello"]
        );
    }

    #[test]
    fn leading_dash_or_at_is_space_guarded() {
        // pi would otherwise parse these as a flag / file argument.
        assert_eq!(pi_args(None, Some("-x")), [" -x"]);
        assert_eq!(pi_args(None, Some("@f")), [" @f"]);
        // A tag-prefixed agent message starts with '[', so it is untouched.
        assert_eq!(pi_args(None, Some("[from agent] hi")), ["[from agent] hi"]);
    }
}
