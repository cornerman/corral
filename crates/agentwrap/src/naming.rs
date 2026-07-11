//! Socket naming convention shared with discovering managers:
//! $XDG_RUNTIME_DIR/acp/<label>-<pid>.sock

use std::path::{Path, PathBuf};

/// Label for the socket: explicit --name wins, otherwise the command's
/// basename. Sanitized so the label never breaks the `<label>-<pid>.sock`
/// pattern managers parse (no path separators; '-' is allowed since parsers
/// split on the *last* '-').
pub fn derive_label(name: Option<&str>, command: &str) -> String {
    let raw = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => Path::new(command)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| command.to_string()),
    };
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "agent".to_string()
    } else {
        sanitized
    }
}

pub fn socket_path(runtime_dir: &Path, label: &str, pid: u32) -> PathBuf {
    runtime_dir.join("acp").join(format!("{label}-{pid}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_from_command_basename() {
        assert_eq!(
            derive_label(None, "/usr/bin/claude-agent-acp"),
            "claude-agent-acp"
        );
        assert_eq!(derive_label(None, "gemini"), "gemini");
    }

    #[test]
    fn explicit_name_wins() {
        assert_eq!(derive_label(Some("myagent"), "/usr/bin/x"), "myagent");
    }

    #[test]
    fn empty_or_whitespace_name_falls_back_to_command() {
        assert_eq!(derive_label(Some("  "), "/usr/bin/x"), "x");
    }

    #[test]
    fn weird_characters_are_sanitized() {
        assert_eq!(derive_label(Some("a b/c"), "x"), "a_b_c");
    }

    #[test]
    fn socket_path_follows_convention() {
        assert_eq!(
            socket_path(Path::new("/run/user/1000"), "claude", 42),
            PathBuf::from("/run/user/1000/acp/claude-42.sock")
        );
    }
}
