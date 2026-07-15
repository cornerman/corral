//! The board's in-memory model. Live agents are keyed by socket path (one
//! socket is one live agent), driven by the per-socket watchers. Dormant
//! agents are a derived view of the registry: cleanly shut-down, resumable
//! sessions that are not currently live. Nothing here is persisted.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::discovery::RegistryEntry;

/// Triage states, adopting the ACP v2 `state_update` vocabulary
/// (agentclientprotocol.com/rfds/v2/prompt): the agent is running, idle (turn
/// done, awaiting the next prompt), or blocked needing user input to continue.
/// For the board, RequiresAction is the most urgent call on the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Idle,
    RequiresAction,
}

impl State {
    /// Map the ACP v2 wire word to a state.
    pub fn from_wire(s: &str) -> Option<State> {
        match s {
            "running" => Some(State::Running),
            "idle" => Some(State::Idle),
            "requires_action" => Some(State::RequiresAction),
            _ => None,
        }
    }
}

/// Whether an agent is a live process (a watched socket) or a dormant,
/// resumable session record. Enter focuses a live window but resumes a
/// dormant session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Live,
    Dormant,
}

/// The board's columns, left to right in attention priority. Defined once
/// here; navigation, hit-testing, and rendering all derive their column set
/// and order from `Column::ALL`, so they can never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    RequiresAction,
    Idle,
    Running,
    Dormant,
}

impl Column {
    /// Every column, in display and selection order.
    pub const ALL: [Column; 4] = [
        Column::RequiresAction,
        Column::Idle,
        Column::Running,
        Column::Dormant,
    ];

    /// The column heading shown by any presentation shell.
    pub fn title(&self) -> &'static str {
        match self {
            Column::RequiresAction => "Requires Action",
            Column::Idle => "Idle",
            Column::Running => "Running",
            Column::Dormant => "Dormant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub socket_path: PathBuf,
    pub pid: u32,
    pub label: String,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub state: State,
    pub origin: Origin,
    /// argv to spawn a fresh session of this agent's kind (from the record's
    /// `spawnCommand`), rooted at a cwd the caller supplies. Carried on both
    /// live and dormant agents so Shift+Enter beside a card spawns the same
    /// kind. `None` when the producer announced no spawn command.
    pub spawn_command: Option<Vec<String>>,
    /// argv to resume this exact dormant session (from the record's
    /// `resumeCommand`). `None` for live agents and non-resumable sessions.
    pub resume_command: Option<Vec<String>>,
    /// The current or most recent tool activity (from a `tool_call`
    /// broadcast), e.g. "edit model.rs". Shows what a running agent is doing
    /// and what an idle one just finished. `None` until the first tool runs.
    pub activity: Option<String>,
    /// Whether corral launches this agent's command directly (self-windowing
    /// GUI app) instead of terminal-wrapped. Stamped from the record's `gui`
    /// on both dormant and live agents, so spawn/resume beside any card picks
    /// the right launch mode.
    pub gui: bool,
    /// Optional CLI flag carrying a launch message (from the record's
    /// `messageFlag`, e.g. `"--message"`). `None` means the message is passed
    /// positionally. Stamped from the record like `gui`.
    pub message_flag: Option<String>,
    /// Whether this session runs hidden (headless cage). Stamped from the
    /// record on both live and dormant agents; the board shows a `hidden`
    /// badge on a live hidden card and reveals it by resume instead of focus.
    pub hidden: bool,
}

impl Agent {
    /// The launch options this agent's kind declared (gui + message flag), for
    /// `Launcher::launch`.
    pub fn launch_mode(&self) -> crate::launch::LaunchMode {
        crate::launch::LaunchMode {
            gui: self.gui,
            message_flag: self.message_flag.clone(),
            hidden: self.hidden,
        }
    }

    /// Whether this agent's card content fuzzily matches a filter query: every
    /// whitespace-separated term must appear (case-insensitive) as an in-order
    /// subsequence of the title, cwd, activity, state word, or harness label.
    /// An empty query matches everything.
    pub fn matches_query(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let state = match self.origin {
            Origin::Dormant => "dormant",
            Origin::Live => match self.state {
                State::RequiresAction => "requires action",
                State::Running => "running",
                State::Idle => "idle",
            },
        };
        let hay = format!(
            "{} {} {} {} {}",
            self.title.as_deref().unwrap_or(""),
            self.cwd.as_deref().unwrap_or(""),
            self.activity.as_deref().unwrap_or(""),
            state,
            self.label,
        )
        .to_lowercase();
        q.split_whitespace().all(|term| is_subsequence(term, &hay))
    }
}

/// Whether `needle` occurs in `hay` as an in-order (not necessarily
/// contiguous) subsequence — the fuzzy-match primitive. Both are already
/// lowercased by the caller.
fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut chars = hay.chars();
    needle.chars().all(|c| chars.by_ref().any(|hc| hc == c))
}

/// A change pushed from a watcher thread to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Seed or refresh an agent's metadata (from initialize + session/list).
    Upsert(Agent),
    /// A state transition (from a session/update state broadcast).
    SetState(PathBuf, State),
    /// A title change (from a session_info_update broadcast on rename).
    SetTitle(PathBuf, Option<String>),
    /// The current tool activity (from a `tool_call` broadcast): a short
    /// summary like "edit model.rs" shown on the card.
    SetActivity(PathBuf, String),
    /// The socket closed or refused: the agent is gone.
    Gone(PathBuf),
}

/// The board state the UI renders. Live agents are keyed and ordered by socket
/// path (stable across ticks); dormant agents are rebuilt from the registry
/// snapshot on each scan.
#[derive(Debug, Default)]
pub struct Board {
    live: BTreeMap<PathBuf, Agent>,
    dormant: Vec<Agent>,
    /// Content filter: when non-empty, `column` (and thus counts, selection,
    /// rendering, hit-testing) shows only cards whose whole content matches.
    filter: String,
}

impl Board {
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Upsert(agent) => {
                self.live.insert(agent.socket_path.clone(), agent);
            }
            Update::SetState(path, state) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.state = state;
                }
            }
            Update::SetTitle(path, title) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.title = title;
                }
            }
            Update::SetActivity(path, activity) => {
                if let Some(a) = self.live.get_mut(&path) {
                    a.activity = Some(activity);
                }
            }
            Update::Gone(path) => {
                self.live.remove(&path);
            }
        }
    }

    /// Rebuild the dormant view from the latest registry snapshot. A record is
    /// dormant when it is resumable (`resume` set), its session is not
    /// currently live, and its socket is not reachable: either cleared to
    /// `None` on clean shutdown, or set but found dead (a crashed session that
    /// never unlinked). `dead_sockets` carries the socket paths whose watcher
    /// failed to connect, so a crashed agent surfaces as resumable instead of
    /// vanishing, while a freshly starting one (socket set, not yet dead) is
    /// left to the live path and never flickers through Dormant. Every
    /// resumable, not-live record is shown (one card per dormant session,
    /// newest first), so resuming one visibly drops the count.
    pub fn sync_registry(&mut self, entries: &[RegistryEntry], dead_sockets: &HashSet<PathBuf>) {
        let live_ids: HashSet<&str> = self
            .live
            .values()
            .filter_map(|a| a.session_id.as_deref())
            .collect();

        // Every resumable, not-live record: one card per dormant session.
        let mut recs: Vec<&RegistryEntry> = entries
            .iter()
            .filter(|e| {
                if e.resume_command.is_none() {
                    return false;
                }
                // A set, non-dead socket means the process is live or still
                // connecting: not dormant. Only None (clean shutdown) or a
                // known-dead socket (crash) is a dormant candidate.
                let socket_alive = e.socket.as_ref().is_some_and(|s| !dead_sockets.contains(s));
                !socket_alive && !live_ids.contains(e.session_id.as_str())
            })
            .collect();
        // Newest first; session id breaks ties for a stable order across ticks.
        recs.sort_by(|a, b| {
            b.last_seen
                .cmp(&a.last_seen)
                .then(a.session_id.cmp(&b.session_id))
        });
        self.dormant = recs
            .into_iter()
            .map(|e| Agent {
                socket_path: PathBuf::new(),
                pid: 0,
                // Agent kind from the record; the board stays agent-agnostic.
                label: e.label.clone().unwrap_or_else(|| "agent".into()),
                session_id: Some(e.session_id.clone()),
                title: e.title.clone(),
                cwd: e.cwd.clone(),
                state: State::Idle,
                origin: Origin::Dormant,
                spawn_command: e.spawn_command.clone(),
                resume_command: e.resume_command.clone(),
                // Dormant records carry no live activity.
                activity: None,
                gui: e.gui,
                message_flag: e.message_flag.clone(),
                hidden: e.hidden,
            })
            .collect();

        // A live agent's socket cannot report its spawn command (that is the
        // record's job), so stamp it from the matching record by session id.
        // Shift+Enter beside a live card then spawns the same kind.
        for a in self.live.values_mut() {
            if let Some(sid) = a.session_id.as_deref() {
                if let Some(e) = entries.iter().find(|e| e.session_id == sid) {
                    a.spawn_command = e.spawn_command.clone();
                    // Reveal/hide relaunch a live agent from its record, so a
                    // live card needs the resume argv too, not only spawn.
                    a.resume_command = e.resume_command.clone();
                    a.gui = e.gui;
                    a.message_flag = e.message_flag.clone();
                    a.hidden = e.hidden;
                }
            }
        }
    }

    /// Live agents in a given state, in stable order.
    pub fn in_state(&self, state: State) -> Vec<&Agent> {
        self.live.values().filter(|a| a.state == state).collect()
    }

    /// The dormant column: every resumable, not-live session, newest first.
    pub fn dormant(&self) -> Vec<&Agent> {
        self.dormant.iter().collect()
    }

    /// Set the content filter. Empty clears it.
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }

    /// How often each working directory occurs across all known sessions,
    /// live and dormant. Drives the within-column grouping: cards sharing a
    /// cwd sit together, and the busiest directories float to the top.
    fn cwd_occurrences(&self) -> std::collections::HashMap<&str, usize> {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for a in self.live.values().chain(self.dormant.iter()) {
            *counts.entry(a.cwd.as_deref().unwrap_or("")).or_default() += 1;
        }
        counts
    }

    /// The agents in one column, narrowed by the content filter if set. The
    /// live columns are then grouped by cwd with the most-used directories
    /// first (a stable sort, so the base order is preserved within each
    /// directory group). Dormant is exempt: it stays in its age order (newest
    /// first), since a resumable session is picked by recency, not by which
    /// directory is busiest.
    pub fn column(&self, column: Column) -> Vec<&Agent> {
        let base = match column {
            Column::RequiresAction => self.in_state(State::RequiresAction),
            Column::Idle => self.in_state(State::Idle),
            Column::Running => self.in_state(State::Running),
            Column::Dormant => self.dormant(),
        };
        let mut list: Vec<&Agent> = if self.filter.trim().is_empty() {
            base
        } else {
            base.into_iter()
                .filter(|a| a.matches_query(&self.filter))
                .collect()
        };
        // Dormant keeps its newest-first age order; only the live columns group.
        if column == Column::Dormant {
            return list;
        }
        let counts = self.cwd_occurrences();
        list.sort_by(|a, b| {
            let (ca, cb) = (
                a.cwd.as_deref().unwrap_or(""),
                b.cwd.as_deref().unwrap_or(""),
            );
            let (na, nb) = (
                counts.get(ca).copied().unwrap_or(0),
                counts.get(cb).copied().unwrap_or(0),
            );
            nb.cmp(&na).then_with(|| ca.cmp(cb))
        });
        list
    }

    /// The agent count of each column, in `Column::ALL` order. Drives
    /// navigation and hit-testing.
    pub fn column_counts(&self) -> [usize; 4] {
        Column::ALL.map(|c| self.column(c).len())
    }

    /// Selectable agents flattened in column order: RequiresAction (blocked on
    /// the operator), Idle (awaiting the next task), Running (leave alone),
    /// then Dormant (resumable). The UI's selection index is over this order.
    pub fn selectable(&self) -> Vec<&Agent> {
        Column::ALL
            .into_iter()
            .flat_map(|c| self.column(c))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(path: &str, state: State) -> Agent {
        Agent {
            socket_path: PathBuf::from(path),
            pid: 1,
            label: "pi".into(),
            session_id: Some(path.into()),
            title: None,
            cwd: None,
            state,
            origin: Origin::Live,
            spawn_command: None,
            resume_command: None,
            activity: None,
            gui: false,
            message_flag: None,
            hidden: false,
        }
    }

    #[test]
    fn live_agent_gets_hidden_and_resume_from_record() {
        let mut b = Board::default();
        // A live agent keyed by socket; session id links it to a record.
        b.apply(Update::Upsert(agent("sess-1", State::Running)));
        let rec = RegistryEntry {
            session_id: "sess-1".into(),
            cwd: Some("/tmp/p".into()),
            title: None,
            socket: Some(PathBuf::from("/tmp/p/.corral/pi-9.sock")),
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec!["pi".into(), "--session".into(), "sess-1".into()]),
            label: Some("pi".into()),
            last_seen: None,
            gui: false,
            message_flag: None,
            hidden: true,
            group: None,
            name: None,
        };
        b.sync_registry(&[rec], &HashSet::new());
        let live = b.in_state(State::Running);
        assert_eq!(live.len(), 1);
        assert!(live[0].hidden, "live agent must inherit hidden from its record");
        assert_eq!(
            live[0].resume_command.as_deref().unwrap(),
            ["pi", "--session", "sess-1"],
            "live agent must carry resume_command for reveal/hide"
        );
        assert!(live[0].launch_mode().hidden);
    }

    fn dormant_record(id: &str, cwd: &str, last_seen: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: id.into(),
            cwd: Some(cwd.into()),
            title: Some(id.into()),
            socket: None,
            spawn_command: Some(vec!["pi".into()]),
            resume_command: Some(vec![
                "pi".into(),
                "--session".into(),
                format!("/s/{id}.jsonl"),
            ]),
            label: Some("pi".into()),
            last_seen: Some(last_seen.into()),
            gui: false,
            message_flag: None,
            hidden: false,
            group: None,
            name: None,
        }
    }

    #[test]
    fn dormant_agent_inherits_gui_from_record() {
        let mut board = Board::default();
        let rec = RegistryEntry {
            session_id: "q1".into(),
            cwd: Some("/tmp/q".into()),
            title: None,
            socket: None, // cleared => dormant
            spawn_command: Some(vec!["quine".into(), "--corral".into()]),
            resume_command: Some(vec![
                "quine".into(),
                "--session".into(),
                "q1".into(),
                "--corral".into(),
            ]),
            label: Some("quine".into()),
            last_seen: None,
            gui: true,
            message_flag: Some("--message".into()),
            hidden: false,
            group: None,
            name: None,
        };
        board.sync_registry(&[rec], &HashSet::new());
        let dormant = board.dormant();
        assert_eq!(dormant.len(), 1);
        assert!(dormant[0].gui, "dormant quine card must carry gui=true");
        assert_eq!(
            dormant[0].message_flag.as_deref(),
            Some("--message"),
            "dormant quine card must carry the message flag"
        );
    }

    #[test]
    fn state_wire_mapping() {
        assert_eq!(State::from_wire("running"), Some(State::Running));
        assert_eq!(State::from_wire("idle"), Some(State::Idle));
        assert_eq!(
            State::from_wire("requires_action"),
            Some(State::RequiresAction)
        );
        assert_eq!(State::from_wire("bogus"), None);
    }

    #[test]
    fn matches_query_is_fuzzy_and_searches_the_label() {
        let mut a = agent("/s/pi-1.sock", State::Idle);
        a.title = Some("Fix parser".into());
        a.cwd = Some("/home/me/corral".into());
        a.label = "opencode".into();
        // Non-contiguous subsequence of the title matches.
        assert!(a.matches_query("fxprs"));
        // The harness label is part of the haystack, fuzzily too.
        assert!(a.matches_query("opencode"));
        assert!(a.matches_query("ocod"));
        // Order matters (subsequence, not anagram) and absent chars fail.
        assert!(!a.matches_query("edocnepo"));
        assert!(!a.matches_query("zzz"));
    }

    #[test]
    fn column_groups_by_cwd_most_used_first() {
        let mut b = Board::default();
        let mk = |sock: &str, cwd: &str| {
            let mut a = agent(sock, State::Idle);
            a.cwd = Some(cwd.into());
            a
        };
        b.apply(Update::Upsert(mk("/s/pi-1.sock", "/b")));
        b.apply(Update::Upsert(mk("/s/pi-2.sock", "/a")));
        b.apply(Update::Upsert(mk("/s/pi-3.sock", "/a")));
        let cwds: Vec<&str> = b
            .column(Column::Idle)
            .iter()
            .map(|a| a.cwd.as_deref().unwrap())
            .collect();
        // /a occurs twice, so its group sorts ahead of the single /b.
        assert_eq!(cwds, vec!["/a", "/a", "/b"]);
    }

    #[test]
    fn dormant_column_stays_age_ordered_ignoring_cwd_grouping() {
        let mut b = Board::default();
        // /busy occurs twice, /solo once. cwd grouping would float /busy up,
        // but the Dormant column must ignore that and stay newest-first.
        b.sync_registry(
            &[
                dormant_record("newest", "/solo", "2026-06-03T00:00:00Z"),
                dormant_record("middle", "/busy", "2026-06-02T00:00:00Z"),
                dormant_record("oldest", "/busy", "2026-06-01T00:00:00Z"),
            ],
            &HashSet::new(),
        );
        let ids: Vec<&str> = b
            .column(Column::Dormant)
            .iter()
            .map(|a| a.session_id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, vec!["newest", "middle", "oldest"]);
    }

    #[test]
    fn upsert_then_set_state() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Running)));
        assert_eq!(b.in_state(State::Running).len(), 1);
        b.apply(Update::SetState(PathBuf::from("/s/pi-1.sock"), State::Idle));
        assert_eq!(b.in_state(State::Running).len(), 0);
        assert_eq!(b.in_state(State::Idle).len(), 1);
    }

    #[test]
    fn set_state_on_unknown_is_ignored() {
        let mut b = Board::default();
        b.apply(Update::SetState(
            PathBuf::from("/s/ghost.sock"),
            State::Running,
        ));
        assert!(b.selectable().is_empty());
    }

    #[test]
    fn set_title_updates_label() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Idle)));
        b.apply(Update::SetTitle(
            PathBuf::from("/s/pi-1.sock"),
            Some("renamed".into()),
        ));
        assert_eq!(b.in_state(State::Idle)[0].title.as_deref(), Some("renamed"));
    }

    #[test]
    fn gone_removes_agent() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Running)));
        b.apply(Update::Gone(PathBuf::from("/s/pi-1.sock")));
        assert!(b.selectable().is_empty());
    }

    #[test]
    fn dormant_shows_all_resumable_newest_first_excluding_live() {
        let mut b = Board::default();
        // A live session (its sessionId must suppress a dormant twin).
        b.apply(Update::Upsert(agent("live-1", State::Running)));
        let entries = vec![
            dormant_record("old", "/p2", "2026-01-01T00:00:00Z"),
            dormant_record("new", "/p2", "2026-06-01T00:00:00Z"),
            // Same sessionId as the live agent -> not dormant.
            RegistryEntry {
                socket: None,
                ..dormant_record("live-1", "/p1", "2026-06-02T00:00:00Z")
            },
        ];
        b.sync_registry(&entries, &HashSet::new());
        let d = b.dormant();
        // Both /p2 records shown (no per-cwd dedup), newest first; live excluded.
        assert_eq!(d.len(), 2, "all resumable, live excluded");
        assert_eq!(d[0].session_id.as_deref(), Some("new"));
        assert_eq!(d[1].session_id.as_deref(), Some("old"));
        assert_eq!(d[0].origin, Origin::Dormant);
    }

    #[test]
    fn dormant_ignores_live_socketed_and_nonresumable_records() {
        let mut b = Board::default();
        let entries = vec![
            // Still live: socket present.
            RegistryEntry {
                socket: Some(PathBuf::from("/p/.corral/pi-1.sock")),
                ..dormant_record("a", "/p", "t")
            },
            // Ephemeral: no resume command.
            RegistryEntry {
                resume_command: None,
                ..dormant_record("b", "/q", "t")
            },
        ];
        b.sync_registry(&entries, &HashSet::new());
        assert!(b.dormant().is_empty());
    }

    #[test]
    fn crashed_socket_becomes_dormant() {
        let mut b = Board::default();
        let sock = PathBuf::from("/p/.corral/pi-1.sock");
        // Record still names a socket (no clean shutdown), but it is dead.
        let entries = vec![RegistryEntry {
            socket: Some(sock.clone()),
            ..dormant_record("crashed", "/p", "t")
        }];
        // Not yet known dead -> treated as live/connecting, not dormant.
        b.sync_registry(&entries, &HashSet::new());
        assert!(
            b.dormant().is_empty(),
            "a set socket is live until proven dead"
        );
        // Once the watcher reports it dead, it surfaces as resumable.
        let dead = HashSet::from([sock]);
        b.sync_registry(&entries, &dead);
        let d = b.dormant();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].session_id.as_deref(), Some("crashed"));
        assert_eq!(d[0].origin, Origin::Dormant);
    }

    #[test]
    fn selectable_orders_by_attention_priority() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Running)));
        b.apply(Update::Upsert(agent("/s/pi-2.sock", State::Idle)));
        b.apply(Update::Upsert(agent("/s/pi-3.sock", State::RequiresAction)));
        b.sync_registry(&[dormant_record("z", "/p9", "t")], &HashSet::new());
        let sel = b.selectable();
        assert_eq!(sel[0].state, State::RequiresAction);
        assert_eq!(sel[1].state, State::Idle);
        assert_eq!(sel[2].state, State::Running);
        assert_eq!(sel[3].origin, Origin::Dormant);
    }
}
