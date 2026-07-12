//! Directory picker for `N` (Shift+N) spawn. Candidates are the cwds of agents
//! already on the board plus the immediate subdirectories of configurable
//! roots (`$CORRAL_PROJECT_ROOTS`, colon-separated; default `$HOME/projects`).
//! A subsequence fuzzy filter narrows the list as the operator types.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::model::Board;

pub struct Picker {
    pub query: String,
    /// Sorted, unique candidate directories.
    candidates: Vec<String>,
    pub selected: usize,
}

impl Picker {
    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            query: String::new(),
            candidates,
            selected: 0,
        }
    }

    /// Candidates matching the current query, in candidate order.
    pub fn matches(&self) -> Vec<&str> {
        self.candidates
            .iter()
            .filter(|c| fuzzy(&self.query, c))
            .map(String::as_str)
            .collect()
    }

    pub fn selected_dir(&self) -> Option<String> {
        self.matches().get(self.selected).map(|s| s.to_string())
    }

    pub fn push(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        let n = self.matches().len();
        if n > 0 {
            self.selected = (self.selected + 1).min(n - 1);
        }
    }
}

/// Case-insensitive subsequence match: every query char appears in order. An
/// empty query matches everything.
fn fuzzy(query: &str, cand: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    for ch in cand.chars().flat_map(char::to_lowercase) {
        match q.peek() {
            Some(&qc) if qc == ch => {
                q.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    q.peek().is_none()
}

/// Gather candidate directories: board agents' cwds, then the immediate
/// subdirectories of each project root. Sorted and de-duplicated.
pub fn gather_dirs(board: &Board) -> Vec<String> {
    let mut set = BTreeSet::new();
    for agent in board.selectable() {
        if let Some(cwd) = &agent.cwd {
            set.insert(cwd.clone());
        }
    }
    for root in project_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            // Skip dotfiles/dirs (.git, .cache, .direnv, ...): noise in a
            // project picker.
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                set.insert(entry.path().to_string_lossy().into_owned());
            }
        }
    }
    set.into_iter().collect()
}

/// `$CORRAL_PROJECT_ROOTS` (colon-separated) or `$HOME/projects`.
fn project_roots() -> Vec<PathBuf> {
    if let Some(v) = std::env::var_os("CORRAL_PROJECT_ROOTS") {
        return std::env::split_paths(&v).collect();
    }
    std::env::var_os("HOME")
        .map(|h| vec![PathBuf::from(h).join("projects")])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_is_case_insensitive_subsequence() {
        assert!(fuzzy("", "/home/x/anything"));
        assert!(fuzzy("cor", "/home/u/projects/corral"));
        assert!(fuzzy("crl", "/home/u/projects/corral")); // subsequence, gaps ok
        assert!(!fuzzy("xyz", "/home/u/projects/corral"));
        assert!(!fuzzy("corx", "/home/u/projects/corral"));
    }

    #[test]
    fn matches_filter_and_selection() {
        let mut p = Picker::new(vec![
            "/home/u/projects/corral".into(),
            "/home/u/projects/nixos".into(),
            "/tmp".into(),
        ]);
        assert_eq!(p.matches().len(), 3);
        p.push('n');
        // "n" appears in corral (corral has an 'n'? no) -> check: nixos yes,
        // /home has no... "/home/u/projects/nixos" contains n; corral contains
        // no 'n'; /tmp no. Expect nixos only among these.
        assert_eq!(p.matches(), vec!["/home/u/projects/nixos"]);
        assert_eq!(p.selected_dir().as_deref(), Some("/home/u/projects/nixos"));
        p.backspace();
        assert_eq!(p.matches().len(), 3);
    }
}
