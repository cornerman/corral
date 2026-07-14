//! Agent-spawn seam. The core calls `Launcher::launch` with an argv the
//! registry record supplied; the kitty implementation is the only place that
//! knows how a new agent window is started. corral never names a specific
//! agent (pi, opencode, …): the command rides in the record, so the board runs
//! whatever kind the selected card is.

use std::path::Path;
use std::process::Command;

pub trait Launcher {
    /// Launch an agent window rooted at `cwd`, running the argv `command`
    /// (from the record's `spawnCommand` for a fresh session, or
    /// `resumeCommand` to resume an exact one). An optional initial `message`
    /// is appended as the final positional argument, so a launch can deliver a
    /// message atomically without waiting for the new session to announce.
    /// An empty `command` is an error (nothing to run).
    fn launch(&self, cwd: &Path, command: &[String], message: Option<&str>) -> Result<(), String>;
}

/// Append an initial message to a launch argv as a trailing positional
/// argument. A message starting with `-` or `@` is space-guarded (prefixed
/// with a single space): a generic CLI-safety convention so an arg parser does
/// not mistake the message for a flag or a file. This is the one launch detail
/// corral keeps, because the message is a runtime value the static record
/// command template cannot carry.
fn with_message(command: &[String], message: Option<&str>) -> Vec<String> {
    let mut args = command.to_vec();
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

impl Launcher for KittyLauncher {
    /// Launch `kitty --directory <cwd> -e <command…> [message]`, detached from
    /// the caller.
    ///
    /// `setsid --fork` reparents kitty to init, which matters twice over: the
    /// window outlives the board and leaves no zombie child, and — critically —
    /// the window is no longer a descendant of corral. The focus seam finds a
    /// window by walking up the agent's `/proc` parent chain; if the spawned
    /// kitty were a child of corral, that walk would continue past it into
    /// corral's own terminal and could focus or close the board itself.
    /// Detaching stops the walk at `agent -> kitty -> init`. `setsid` exits
    /// immediately after forking, so waiting on it reaps at once.
    fn launch(&self, cwd: &Path, command: &[String], message: Option<&str>) -> Result<(), String> {
        let Some((program, rest)) = command.split_first() else {
            return Err("launch: empty command".into());
        };
        let args = with_message(rest, message);
        let ok = Command::new("setsid")
            .arg("--fork")
            .arg("kitty")
            .arg("--directory")
            .arg(cwd)
            .arg("-e")
            .arg(program)
            .args(&args)
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
    use super::with_message;

    #[test]
    fn no_message_is_the_bare_command() {
        assert_eq!(with_message(&["pi".to_string()], None), ["pi"]);
    }

    #[test]
    fn message_is_appended_as_final_arg() {
        assert_eq!(
            with_message(&["pi".to_string(), "--session".to_string(), "/s".to_string()], Some("hello")),
            ["pi", "--session", "/s", "hello"]
        );
    }

    #[test]
    fn leading_dash_or_at_is_space_guarded() {
        // An arg parser would otherwise read these as a flag / file argument.
        assert_eq!(with_message(&["pi".to_string()], Some("-x")), ["pi", " -x"]);
        assert_eq!(with_message(&["pi".to_string()], Some("@f")), ["pi", " @f"]);
        // A tag-prefixed agent message starts with '[', so it is untouched.
        assert_eq!(
            with_message(&["pi".to_string()], Some("[from agent] hi")),
            ["pi", "[from agent] hi"]
        );
    }
}
