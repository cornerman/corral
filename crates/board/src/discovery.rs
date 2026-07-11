//! Socket discovery: the filesystem is the registry. Sockets follow the
//! `<label>-<pid>.sock` convention (pi announces `pi-<pid>.sock`).

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Clone)]
pub struct SocketEntry {
    pub path: PathBuf,
    pub label: String,
    pub pid: u32,
}

/// Parse `<label>-<pid>.sock`. The pid is everything after the *last* '-',
/// so labels themselves may contain dashes.
pub fn parse_socket_filename(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_suffix(".sock")?;
    let (label, pid) = stem.rsplit_once('-')?;
    if label.is_empty() {
        return None;
    }
    Some((label.to_string(), pid.parse().ok()?))
}

/// Scan a directory for convention-following sockets. A missing directory is
/// an empty result, not an error: no agent has announced yet.
pub fn scan(dir: &Path) -> Vec<SocketEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (label, pid) = parse_socket_filename(&name)?;
            Some(SocketEntry {
                path: e.path(),
                label,
                pid,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_filename() {
        assert_eq!(
            parse_socket_filename("claude-1234.sock"),
            Some(("claude".to_string(), 1234))
        );
    }

    #[test]
    fn label_may_contain_dashes() {
        assert_eq!(
            parse_socket_filename("claude-agent-acp-99.sock"),
            Some(("claude-agent-acp".to_string(), 99))
        );
    }

    #[test]
    fn rejects_non_sockets_and_malformed_names() {
        assert_eq!(parse_socket_filename("readme.txt"), None);
        assert_eq!(parse_socket_filename("nopid.sock"), None);
        assert_eq!(parse_socket_filename("label-notanumber.sock"), None);
        assert_eq!(parse_socket_filename("-42.sock"), None);
    }

    #[test]
    fn scan_missing_dir_is_empty() {
        assert!(scan(Path::new("/nonexistent/definitely-not-here")).is_empty());
    }
}
