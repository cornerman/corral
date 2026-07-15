//! Agent-spawn seam. The core calls `Launcher::launch` with an argv the
//! registry record supplied; the terminal implementation is the only place
//! that knows how a new agent window is started. corral names neither the
//! agent (pi, opencode, … — that command rides in the record) nor the terminal
//! (chosen from the environment), so it stays neutral on both axes.

use std::path::Path;
use std::process::Command;

/// How a record wants its kind launched: the two launch-affecting properties a
/// registry record carries. Bundled so the `launch` signature stays narrow as
/// launch options grow (model-free: built from a record/agent by the caller).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LaunchMode {
    /// Run the command directly (a self-windowing GUI agent, e.g. quine)
    /// instead of wrapping it in a terminal.
    pub gui: bool,
    /// CLI flag that carries an initial launch message (e.g. `"--message"`).
    /// `None` passes the message as a trailing positional argument.
    pub message_flag: Option<String>,
    /// Run inside a headless `cage` so the window never maps on the host
    /// compositor. Set by a background ("hidden") spawn; the agent runs and
    /// announces normally, revealed later by resume in a real window.
    pub hidden: bool,
}

pub trait Launcher {
    /// Launch an agent window rooted at `cwd`, running the argv `command`
    /// (from the record's `spawnCommand` for a fresh session, or
    /// `resumeCommand` to resume an exact one). An optional initial `message`
    /// is appended to the argv, so a launch can deliver a message atomically
    /// without waiting for the new session to announce.
    /// An empty `command`, or no resolvable terminal, is an error.
    ///
    /// `mode` selects how: `gui` runs the command directly (else terminal-
    /// wrapped), and `message_flag` decides whether the message rides as a
    /// flag value or a trailing positional.
    fn launch(
        &self,
        cwd: &Path,
        command: &[String],
        message: Option<&str>,
        mode: &LaunchMode,
    ) -> Result<(), String>;
}

/// Build the argv that follows `setsid --fork`. A GUI agent is run directly
/// (its command only); a terminal agent gets the resolved terminal prefix in
/// front. The initial message, if any, is appended in both modes via
/// `with_message`.
fn setsid_args(
    mode: &LaunchMode,
    terminal: &[String],
    command: &[String],
    message: Option<&str>,
) -> Vec<String> {
    let tail = with_message(command, message, mode.message_flag.as_deref());
    let mut inner = if mode.gui {
        tail
    } else {
        let mut args = terminal.to_vec();
        args.extend(tail);
        args
    };
    if mode.hidden {
        // WLR_BACKENDS=headless is load-bearing: without it wlroots picks its
        // X11 backend on an X11 host and opens a visible nested window, the
        // exact blink hidden mode avoids. cage brings XWayland, so terminal
        // and GUI agents alike render into its headless output. CORRAL_HIDDEN
        // signals the adapter to record `hidden` on the session.
        let mut wrapped = vec![
            "env".to_string(),
            "WLR_BACKENDS=headless".to_string(),
            "CORRAL_HIDDEN=1".to_string(),
            "cage".to_string(),
            "--".to_string(),
        ];
        wrapped.append(&mut inner);
        wrapped
    } else {
        inner
    }
}

/// Append an initial message to a launch argv. With a `message_flag` the
/// message rides as that flag's value (two args: flag, then text), bound to
/// the flag so no guard is needed. Without one it is a trailing positional; a
/// message starting with `-` or `@` is then space-guarded (prefixed with a
/// single space) so an arg parser does not mistake it for a flag or a file.
/// The message is a runtime value the static record command cannot carry, so
/// this is the one launch detail corral keeps.
fn with_message(
    command: &[String],
    message: Option<&str>,
    message_flag: Option<&str>,
) -> Vec<String> {
    let mut args = command.to_vec();
    if let Some(m) = message {
        match message_flag {
            Some(flag) => {
                args.push(flag.to_string());
                args.push(m.to_string());
            }
            None => {
                let guarded = if m.starts_with('-') || m.starts_with('@') {
                    format!(" {m}")
                } else {
                    m.to_string()
                };
                args.push(guarded);
            }
        }
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
    fn launch(
        &self,
        cwd: &Path,
        command: &[String],
        message: Option<&str>,
        mode: &LaunchMode,
    ) -> Result<(), String> {
        if command.is_empty() {
            return Err("launch: empty command".into());
        }
        // A GUI agent draws its own window, so it needs no terminal (and must
        // not resolve one). setsid --fork still detaches it from corral so the
        // focus parent-walk cannot climb into corral's own window.
        let terminal = if mode.gui {
            Vec::new()
        } else {
            resolve_terminal().ok_or(
                "no terminal found: install xdg-terminal-exec, or set $CORRAL_TERMINAL \
                 (e.g. \"alacritty -e\") or $TERMINAL",
            )?
        };
        let args = setsid_args(mode, &terminal, command, message);
        let ok = Command::new("setsid")
            .arg("--fork")
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
    use super::{on_path, resolve_terminal_from, setsid_args, with_message, LaunchMode};

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
        assert_eq!(with_message(&["pi".to_string()], None, None), ["pi"]);
    }

    #[test]
    fn message_is_appended_as_final_arg() {
        assert_eq!(
            with_message(
                &["pi".to_string(), "--session".to_string(), "/s".to_string()],
                Some("hello"),
                None
            ),
            ["pi", "--session", "/s", "hello"]
        );
    }

    #[test]
    fn message_flag_rides_as_a_flag_value_unguarded() {
        // With a flag, the message is the flag's value: two args, no guard
        // even for a leading dash (it is bound to the flag, not positional).
        assert_eq!(
            with_message(
                &["quine".to_string(), "--corral".to_string()],
                Some("hi"),
                Some("--message")
            ),
            ["quine", "--corral", "--message", "hi"]
        );
        assert_eq!(
            with_message(&["quine".to_string()], Some("-x"), Some("--message")),
            ["quine", "--message", "-x"]
        );
    }

    #[test]
    fn leading_dash_or_at_is_space_guarded() {
        // An arg parser would otherwise read these as a flag / file argument.
        assert_eq!(
            with_message(&["pi".to_string()], Some("-x"), None),
            ["pi", " -x"]
        );
        assert_eq!(
            with_message(&["pi".to_string()], Some("@f"), None),
            ["pi", " @f"]
        );
        // A tag-prefixed agent message starts with '[', so it is untouched.
        assert_eq!(
            with_message(&["pi".to_string()], Some("[from agent] hi"), None),
            ["pi", "[from agent] hi"]
        );
    }

    #[test]
    fn gui_launch_omits_the_terminal_prefix() {
        let term = vec!["xdg-terminal-exec".to_string()];
        let cmd = vec!["quine".to_string(), "--corral".to_string()];
        let gui = LaunchMode {
            gui: true,
            message_flag: None,
            hidden: false,
        };
        let term_mode = LaunchMode::default();
        // GUI: run the command directly, no terminal prefix.
        assert_eq!(
            setsid_args(&gui, &term, &cmd, None),
            vec!["quine".to_string(), "--corral".to_string()]
        );
        // Non-GUI: terminal prefix in front, exactly as before.
        assert_eq!(
            setsid_args(&term_mode, &term, &cmd, None),
            vec![
                "xdg-terminal-exec".to_string(),
                "quine".to_string(),
                "--corral".to_string()
            ]
        );
        // The message is appended in both modes.
        assert_eq!(
            setsid_args(&gui, &term, &cmd, Some("hi")),
            vec!["quine".to_string(), "--corral".to_string(), "hi".to_string()]
        );
    }

    #[test]
    fn hidden_wraps_argv_in_headless_cage() {
        let term = vec!["xdg-terminal-exec".to_string()];
        let cmd = vec!["pi".to_string()];
        // Hidden terminal agent: cage wraps the terminal+command.
        let hidden = LaunchMode {
            gui: false,
            message_flag: None,
            hidden: true,
        };
        assert_eq!(
            setsid_args(&hidden, &term, &cmd, None),
            vec![
                "env",
                "WLR_BACKENDS=headless",
                "CORRAL_HIDDEN=1",
                "cage",
                "--",
                "xdg-terminal-exec",
                "pi",
            ]
        );
        // Hidden GUI agent: cage wraps the command directly (no terminal).
        let hidden_gui = LaunchMode {
            gui: true,
            message_flag: None,
            hidden: true,
        };
        let gui_cmd = vec!["quine".to_string(), "--corral".to_string()];
        assert_eq!(
            setsid_args(&hidden_gui, &term, &gui_cmd, None),
            vec![
                "env",
                "WLR_BACKENDS=headless",
                "CORRAL_HIDDEN=1",
                "cage",
                "--",
                "quine",
                "--corral",
            ]
        );
        // A launch message still appends inside the wrapped argv.
        assert_eq!(
            setsid_args(&hidden, &term, &cmd, Some("hi")),
            vec![
                "env",
                "WLR_BACKENDS=headless",
                "CORRAL_HIDDEN=1",
                "cage",
                "--",
                "xdg-terminal-exec",
                "pi",
                "hi",
            ]
        );
    }

    #[test]
    fn on_path_finds_a_ubiquitous_binary_and_rejects_nonsense() {
        assert!(on_path("sh"));
        assert!(!on_path("corral-definitely-not-a-real-binary-xyz"));
    }
}
