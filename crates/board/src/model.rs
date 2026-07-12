//! The board's in-memory model. Live agents are keyed by socket path (one
//! socket is one live agent), driven by the per-socket watchers. Dormant
//! agents are a derived view of the registry: cleanly shut-down, resumable
//! sessions that are not currently live. Nothing here is persisted.

use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// The session-file path to resume a dormant session (`pi --session`).
    /// `None` for live agents and ephemeral sessions.
    pub resume: Option<String>,
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
    /// left to the live path and never flickers through Dormant. Only the most
    /// recent record per cwd is kept, so a busy directory shows one card, not a
    /// pile of old sessions.
    pub fn sync_registry(&mut self, entries: &[RegistryEntry], dead_sockets: &HashSet<PathBuf>) {
        let live_ids: HashSet<&str> = self
            .live
            .values()
            .filter_map(|a| a.session_id.as_deref())
            .collect();

        let mut latest: HashMap<&str, &RegistryEntry> = HashMap::new();
        for e in entries {
            if e.resume.is_none() {
                continue;
            }
            // A set, non-dead socket means the process is live or still
            // connecting: not dormant. Only None (clean shutdown) or a
            // known-dead socket (crash) makes a dormant candidate.
            let socket_alive = e.socket.as_ref().is_some_and(|s| !dead_sockets.contains(s));
            if socket_alive {
                continue;
            }
            if live_ids.contains(e.session_id.as_str()) {
                continue;
            }
            let cwd = e.cwd.as_deref().unwrap_or("");
            match latest.get(cwd) {
                // ISO-8601 last_seen sorts as a plain string; keep the newest.
                Some(prev) if prev.last_seen >= e.last_seen => {}
                _ => {
                    latest.insert(cwd, e);
                }
            }
        }

        let mut dormant: Vec<Agent> = latest
            .values()
            .map(|e| Agent {
                socket_path: PathBuf::new(),
                pid: 0,
                // Agent kind from the record; the board stays agent-agnostic.
                label: e.label.clone().unwrap_or_else(|| "pi".into()),
                session_id: Some(e.session_id.clone()),
                title: e.title.clone(),
                cwd: e.cwd.clone(),
                state: State::Idle,
                origin: Origin::Dormant,
                resume: e.resume.clone(),
            })
            .collect();
        // Newest first, stable across ticks (ties broken by cwd).
        dormant.sort_by(|a, b| b.cwd.cmp(&a.cwd));
        self.dormant = dormant;
    }

    /// Live agents in a given state, in stable order.
    pub fn in_state(&self, state: State) -> Vec<&Agent> {
        self.live.values().filter(|a| a.state == state).collect()
    }

    /// Live agents whose working directory is `dir`, in stable order (socket
    /// path). Used to route an inter-agent message to its target directory.
    pub fn live_in_dir(&self, dir: &str) -> Vec<&Agent> {
        self.live
            .values()
            .filter(|a| a.cwd.as_deref() == Some(dir))
            .collect()
    }

    /// The live agent for an exact session id, if running. Used to route a
    /// session-addressed message (e.g. a reply) to precisely that agent.
    pub fn live_by_session(&self, session_id: &str) -> Option<&Agent> {
        self.live
            .values()
            .find(|a| a.session_id.as_deref() == Some(session_id))
    }

    /// The dormant record for an exact session id, if any (to resume it).
    pub fn dormant_by_session(&self, session_id: &str) -> Option<&Agent> {
        self.dormant
            .iter()
            .find(|a| a.session_id.as_deref() == Some(session_id))
    }

    /// The agent for a session id, live first then dormant. Used to resolve a
    /// session-addressed target's cwd (for authorization).
    pub fn by_session(&self, session_id: &str) -> Option<&Agent> {
        self.live_by_session(session_id)
            .or_else(|| self.dormant_by_session(session_id))
    }

    /// The dormant column, latest-per-cwd, newest first.
    pub fn dormant(&self) -> Vec<&Agent> {
        self.dormant.iter().collect()
    }

    /// The agents in one column.
    pub fn column(&self, column: Column) -> Vec<&Agent> {
        match column {
            Column::RequiresAction => self.in_state(State::RequiresAction),
            Column::Idle => self.in_state(State::Idle),
            Column::Running => self.in_state(State::Running),
            Column::Dormant => self.dormant(),
        }
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
            resume: None,
        }
    }

    fn dormant_record(id: &str, cwd: &str, last_seen: &str) -> RegistryEntry {
        RegistryEntry {
            session_id: id.into(),
            cwd: Some(cwd.into()),
            title: Some(id.into()),
            socket: None,
            resume: Some(format!("/s/{id}.jsonl")),
            label: Some("pi".into()),
            last_seen: Some(last_seen.into()),
        }
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
    fn dormant_keeps_latest_per_cwd_and_excludes_live() {
        let mut b = Board::default();
        // A live session in /p1 (its sessionId must suppress a dormant twin).
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
        assert_eq!(d.len(), 1, "latest-per-cwd, live excluded");
        assert_eq!(d[0].session_id.as_deref(), Some("new"));
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
            // Ephemeral: no resume.
            RegistryEntry {
                resume: None,
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
    fn live_in_dir_filters_by_cwd() {
        let mut b = Board::default();
        let mut a1 = agent("/s/1.sock", State::Idle);
        a1.cwd = Some("/p".into());
        let mut a2 = agent("/s/2.sock", State::Idle);
        a2.cwd = Some("/q".into());
        b.apply(Update::Upsert(a1));
        b.apply(Update::Upsert(a2));
        let got = b.live_in_dir("/p");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cwd.as_deref(), Some("/p"));
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
