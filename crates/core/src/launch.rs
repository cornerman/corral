//! Agent-spawn seam. The core calls `Launcher::launch` with an argv the
//! registry record supplied; the terminal implementation is the only place
//! that knows how a new agent window is started. corral names neither the
//! agent (pi, opencode, … — that command rides in the record) nor the terminal
//! (chosen from the environment), so it stays neutral on both axes.

use std::path::Path;
use std::process::Command;

pub trait Launcher {
    /// Launch an agent window rooted at `cwd`, running the argv `command`
    /// (from the record's `spawnCommand` for a fresh session, or
    /// `resumeCommand` to resume an exact one). An optional initial `message`
    /// is appended as the final positional argument, so a launch can deliver a
    /// message atomically without waiting for the new session to announce.
    /// An empty `command`, or no resolvable terminal, is an error.
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

/// Whether `prog` is runnable: an absolute path that exists, or a bare name
/// found on `$PATH`.
fn on_path(prog: &str) -> bool {
    let p = Path::new(prog);
    if p.is_absolute() {
        return p.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file()))
}

/// Resolve how to open a terminal (pure): the argv prefix to which the agent
/// command is appended. The ladder, best to worst:
///   1. `xdg-terminal-exec` — the freedesktop standard ("xdg-open for
///      terminals"); the environment decides which terminal. Appends the
///      command directly (no `-e`).
///   2. `$CORRAL_TERMINAL` — an explicit argv template the user controls,
///      e.g. `"alacritty -e"` or `"foot"`. The command is appended, so the
///      user encodes any `-e`/`--` themselves.
///   3. `$TERMINAL -e` — the legacy convention.
///
/// No hardcoded terminal: if none resolve, the caller surfaces an error.
/// `exists` decides whether a program is runnable (real: `on_path`).
fn resolve_terminal_from(
    xdg_present: bool,
    corral_terminal: Option<&str>,
    terminal: Option<&str>,
    exists: &dyn Fn(&str) -> bool,
) -> Option<Vec<String>> {
    if xdg_present {
        return Some(vec!["xdg-terminal-exec".into()]);
    }
    if let Some(t) = corral_terminal {
        let parts: Vec<String> = t.split_whitespace().map(String::from).collect();
        if parts.first().is_some_and(|p| exists(p)) {
            return Some(parts);
        }
    }
    if let Some(t) = terminal {
        if !t.is_empty() && exists(t) {
            return Some(vec![t.to_string(), "-e".into()]);
        }
    }
    None
}

/// Resolve a terminal from the live environment.
fn resolve_terminal() -> Option<Vec<String>> {
    let corral = std::env::var("CORRAL_TERMINAL").ok();
    let terminal = std::env::var("TERMINAL").ok();
    resolve_terminal_from(
        on_path("xdg-terminal-exec"),
        corral.as_deref(),
        terminal.as_deref(),
        &on_path,
    )
}

pub struct TerminalLauncher;

impl Launcher for TerminalLauncher {
    /// Launch `setsid --fork <terminal…> <command…> [message]`, rooted at `cwd`
    /// via the child's working directory (no terminal-specific `--directory`
    /// flag), detached from the caller.
    ///
    /// `setsid --fork` reparents the terminal to init, which matters twice
    /// over: the window outlives the board and leaves no zombie child, and —
    /// critically — the window is no longer a descendant of corral. The focus
    /// seam finds a window by walking up the agent's `/proc` parent chain; if
    /// the spawned terminal were a child of corral, that walk would continue
    /// past it into corral's own terminal and could focus or close the board
    /// itself. Detaching stops the walk at `agent -> terminal -> init`.
    fn launch(&self, cwd: &Path, command: &[String], message: Option<&str>) -> Result<(), String> {
        if command.is_empty() {
            return Err("launch: empty command".into());
        }
        let terminal = resolve_terminal().ok_or(
            "no terminal found: install xdg-terminal-exec, or set $CORRAL_TERMINAL \
             (e.g. \"alacritty -e\") or $TERMINAL",
        )?;
        let args = with_message(command, message);
        let ok = Command::new("setsid")
            .arg("--fork")
            .args(&terminal)
            .args(&args)
            .current_dir(cwd)
            .status()
            .map_err(|e| format!("terminal launch failed: {e}"))?
            .success();
        if ok {
            Ok(())
        } else {
            Err("terminal launch returned non-zero".into())
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
    use super::{on_path, resolve_terminal_from, with_message};

    // A fake path-checker: only these programs "exist".
    fn only(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |p: &str| names.contains(&p)
    }

    #[test]
    fn ladder_prefers_xdg_terminal_exec() {
        // xdg present wins regardless of the other two.
        let r = resolve_terminal_from(true, Some("alacritty -e"), Some("foot"), &only(&[]));
        assert_eq!(r, Some(vec!["xdg-terminal-exec".to_string()]));
    }

    #[test]
    fn ladder_uses_corral_terminal_template_when_no_xdg() {
        // The user's explicit template is split into argv and its program checked.
        let r = resolve_terminal_from(false, Some("alacritty -e"), None, &only(&["alacritty"]));
        assert_eq!(r, Some(vec!["alacritty".to_string(), "-e".to_string()]));
    }

    #[test]
    fn ladder_skips_corral_terminal_whose_program_is_absent() {
        // CORRAL_TERMINAL set but its binary is not installed: fall through.
        let r = resolve_terminal_from(false, Some("ghostty"), Some("foot"), &only(&["foot"]));
        assert_eq!(r, Some(vec!["foot".to_string(), "-e".to_string()]));
    }

    #[test]
    fn ladder_falls_back_to_terminal_with_dash_e() {
        let r = resolve_terminal_from(false, None, Some("foot"), &only(&["foot"]));
        assert_eq!(r, Some(vec!["foot".to_string(), "-e".to_string()]));
    }

    #[test]
    fn ladder_none_when_nothing_resolves() {
        // No xdg, no CORRAL_TERMINAL, and $TERMINAL binary absent -> error path.
        assert_eq!(
            resolve_terminal_from(false, None, Some("foot"), &only(&[])),
            None
        );
        assert_eq!(resolve_terminal_from(false, None, None, &only(&[])), None);
    }

    #[test]
    fn ladder_ignores_empty_env_values() {
        assert_eq!(
            resolve_terminal_from(false, Some(""), Some(""), &only(&[""])),
            None
        );
    }

    #[test]
    fn no_message_is_the_bare_command() {
        assert_eq!(with_message(&["pi".to_string()], None), ["pi"]);
    }

    #[test]
    fn message_is_appended_as_final_arg() {
        assert_eq!(
            with_message(
                &["pi".to_string(), "--session".to_string(), "/s".to_string()],
                Some("hello")
            ),
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

    #[test]
    fn on_path_finds_a_ubiquitous_binary_and_rejects_nonsense() {
        assert!(on_path("sh"));
        assert!(!on_path("corral-definitely-not-a-real-binary-xyz"));
    }
}
