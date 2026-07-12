//! Fuzzy picker for the `/` jump overlay. Candidates are the labels of the
//! board's agents; a subsequence fuzzy filter narrows the list as the operator
//! types, and `selected_original` maps the chosen label back to its agent.

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

    /// Index into the *original* candidate list of the selected match. Lets a
    /// caller recover a parallel value (e.g. the agent behind a focus label)
    /// without relying on the candidate string being unique.
    pub fn selected_original(&self) -> Option<usize> {
        self.candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| fuzzy(&self.query, c))
            .nth(self.selected)
            .map(|(i, _)| i)
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
        assert_eq!(p.matches(), vec!["/home/u/projects/nixos"]);
        // The match maps back to its original index (nixos is candidate 1).
        assert_eq!(p.selected_original(), Some(1));
        p.backspace();
        assert_eq!(p.matches().len(), 3);
        assert_eq!(p.selected_original(), Some(0));
    }
}
