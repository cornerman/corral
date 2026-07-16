//! Fuzzy picker for the `/` jump overlay. Holds the board's agents, groups them
//! by their working directory, and fuzzy-filters on path or title as the
//! operator types. A Tab scope filter narrows to Live or Dormant. Pure logic:
//! all glyph and color styling (and path abbreviation) lives in `ui.rs`.

use crate::model::{Agent, Origin};

/// Scope filter cycled by Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Live,
    Dormant,
}

impl Filter {
    fn accepts(self, origin: Origin) -> bool {
        match self {
            Filter::All => true,
            Filter::Live => origin == Origin::Live,
            Filter::Dormant => origin == Origin::Dormant,
        }
    }

    fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Live,
            Filter::Live => Filter::Dormant,
            Filter::Dormant => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Live => "live",
            Filter::Dormant => "dormant",
        }
    }
}

/// A visible row: a directory group header (a pure label) or an agent under it.
pub enum Row<'a> {
    Header(&'a str),
    Agent(&'a Agent),
}

pub struct Picker {
    pub query: String,
    pub filter: Filter,
    /// Index into the flat list of visible agent rows (headers excluded).
    pub selected: usize,
    /// Agents in board attention-priority order (from `board.selectable()`).
    agents: Vec<Agent>,
}

/// The directory group an agent belongs to: its full cwd, or a fallback. Full
/// path (not basename) so same-named leaves under different roots stay distinct
/// groups; `ui.rs` abbreviates it for display.
fn group_of(a: &Agent) -> &str {
    a.cwd.as_deref().unwrap_or("(no dir)")
}

impl Picker {
    pub fn new(agents: Vec<Agent>) -> Self {
        Self {
            query: String::new(),
            filter: Filter::All,
            selected: 0,
            agents,
        }
    }

    /// Whether an agent survives the current filter and query. The query is a
    /// subsequence match against the title or the full path, so typing a
    /// directory name keeps every agent under it.
    fn survives(&self, a: &Agent) -> bool {
        if !self.filter.accepts(a.origin) {
            return false;
        }
        let title = a.title.as_deref().unwrap_or("(unnamed)");
        let path = a.cwd.as_deref().unwrap_or("");
        fuzzy(&self.query, title) || fuzzy(&self.query, path)
    }

    /// Surviving agents grouped by directory, groups in first-appearance order,
    /// agents within a group in original (attention) order. Each entry is the
    /// group name and the indices into `self.agents`.
    fn grouped(&self) -> Vec<(&str, Vec<usize>)> {
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, a) in self.agents.iter().enumerate() {
            if !self.survives(a) {
                continue;
            }
            let g = group_of(a);
            match groups.iter_mut().find(|(k, _)| *k == g) {
                Some((_, v)) => v.push(i),
                None => groups.push((g, vec![i])),
            }
        }
        groups
    }

    /// The visible rows in order: each group's header followed by its agents.
    pub fn rows(&self) -> Vec<Row<'_>> {
        let mut out = Vec::new();
        for (g, idxs) in self.grouped() {
            out.push(Row::Header(g));
            for i in idxs {
                out.push(Row::Agent(&self.agents[i]));
            }
        }
        out
    }

    /// Flat indices of the visible agent rows, in row order (headers excluded).
    fn agent_order(&self) -> Vec<usize> {
        self.grouped().into_iter().flat_map(|(_, v)| v).collect()
    }

    /// The agent under the current selection, if any.
    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agent_order()
            .get(self.selected)
            .map(|&i| &self.agents[i])
    }

    pub fn filter_label(&self) -> &'static str {
        self.filter.label()
    }

    pub fn push(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    pub fn cycle_filter(&mut self) {
        self.filter = self.filter.next();
        self.selected = 0;
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        let n = self.agent_order().len();
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
    use crate::model::State;
    use std::path::PathBuf;

    fn agent(title: &str, cwd: &str, origin: Origin) -> Agent {
        Agent {
            socket_path: PathBuf::from(format!("/s/{title}.sock")),
            pid: 1,
            label: "pi".into(),
            session_id: Some(title.into()),
            title: Some(title.into()),
            cwd: Some(cwd.into()),
            state: State::Idle,
            origin,
            spawn_command: None,
            resume_command: None,
            activity: None,
            gui: false,
            message_flag: None,
            hidden: false,
            state_since: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        }
    }

    fn sample() -> Picker {
        Picker::new(vec![
            agent("alpha", "/home/u/projects/corral", Origin::Live),
            agent("beta", "/home/u/projects/nixos", Origin::Live),
            agent("gamma", "/home/u/projects/corral", Origin::Dormant),
        ])
    }

    #[test]
    fn groups_by_directory_in_first_appearance_order() {
        let p = sample();
        let rows = p.rows();
        // corral (alpha, gamma) appears before nixos (beta); headers are full paths.
        assert!(matches!(rows[0], Row::Header("/home/u/projects/corral")));
        assert!(matches!(rows[1], Row::Agent(a) if a.title.as_deref() == Some("alpha")));
        assert!(matches!(rows[2], Row::Agent(a) if a.title.as_deref() == Some("gamma")));
        assert!(matches!(rows[3], Row::Header("/home/u/projects/nixos")));
        assert!(matches!(rows[4], Row::Agent(a) if a.title.as_deref() == Some("beta")));
    }

    #[test]
    fn query_matches_title_or_path_and_drops_empty_groups() {
        let mut p = sample();
        p.push('b');
        p.push('e');
        p.push('t'); // "bet" matches title beta only
        let rows = p.rows();
        assert_eq!(rows.len(), 2); // nixos header + beta
        assert!(matches!(rows[0], Row::Header("/home/u/projects/nixos")));
    }

    #[test]
    fn dir_query_keeps_all_agents_in_group() {
        let mut p = sample();
        for c in "corral".chars() {
            p.push(c);
        }
        let rows = p.rows();
        // both corral agents survive; nixos group is gone.
        assert!(matches!(rows[0], Row::Header("/home/u/projects/corral")));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn tab_filter_restricts_origin() {
        let mut p = sample();
        p.cycle_filter(); // All -> Live
        assert_eq!(p.filter, Filter::Live);
        let live: Vec<_> = p
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Agent(a) => a.title.clone(),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(live, vec!["alpha", "beta"]);
        p.cycle_filter(); // Live -> Dormant
        let dormant: Vec<_> = p
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Agent(a) => a.title.clone(),
                Row::Header(_) => None,
            })
            .collect();
        assert_eq!(dormant, vec!["gamma"]);
    }

    #[test]
    fn down_skips_headers_and_selected_agent_maps_back() {
        let mut p = sample();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("alpha"));
        p.down();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("gamma"));
        p.down();
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("beta"));
        p.down(); // clamps at end
        assert_eq!(p.selected_agent().unwrap().title.as_deref(), Some("beta"));
    }
}
