//! The board's in-memory model. Identity is the socket path: one socket is one
//! live agent. Nothing is persisted; the model is rebuilt live from discovery
//! and the per-socket watchers.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The two triage states. `NeedsYou` is the whole point of the board: the agent
/// is idle and waiting for the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    NeedsYou,
}

impl State {
    /// Map the extension's wire word ("working"/"idle") to a state.
    pub fn from_wire(s: &str) -> Option<State> {
        match s {
            "working" => Some(State::Working),
            "idle" => Some(State::NeedsYou),
            _ => None,
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
}

/// A change pushed from a watcher thread to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Seed or refresh an agent's metadata (from initialize + session/list).
    Upsert(Agent),
    /// A state transition (from a session/update state broadcast).
    SetState(PathBuf, State),
    /// The socket closed or refused: the agent is gone.
    Gone(PathBuf),
}

/// The board state the UI renders. Keyed and ordered by socket path so the
/// layout is stable across ticks.
#[derive(Debug, Default)]
pub struct Board {
    agents: BTreeMap<PathBuf, Agent>,
}

impl Board {
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Upsert(agent) => {
                self.agents.insert(agent.socket_path.clone(), agent);
            }
            Update::SetState(path, state) => {
                if let Some(a) = self.agents.get_mut(&path) {
                    a.state = state;
                }
            }
            Update::Gone(path) => {
                self.agents.remove(&path);
            }
        }
    }

    /// Agents needing the operator, in stable order.
    pub fn needs_you(&self) -> Vec<&Agent> {
        self.agents
            .values()
            .filter(|a| a.state == State::NeedsYou)
            .collect()
    }

    /// Agents currently working, in stable order.
    pub fn working(&self) -> Vec<&Agent> {
        self.agents
            .values()
            .filter(|a| a.state == State::Working)
            .collect()
    }

    /// Selectable agents: NeedsYou first (they want attention), then Working.
    /// The UI's selection index is over this flattened order.
    pub fn selectable(&self) -> Vec<&Agent> {
        let mut v = self.needs_you();
        v.extend(self.working());
        v
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
            session_id: None,
            title: None,
            cwd: None,
            state,
        }
    }

    #[test]
    fn state_wire_mapping() {
        assert_eq!(State::from_wire("working"), Some(State::Working));
        assert_eq!(State::from_wire("idle"), Some(State::NeedsYou));
        assert_eq!(State::from_wire("bogus"), None);
    }

    #[test]
    fn upsert_then_set_state() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Working)));
        assert_eq!(b.working().len(), 1);
        b.apply(Update::SetState(
            PathBuf::from("/s/pi-1.sock"),
            State::NeedsYou,
        ));
        assert_eq!(b.working().len(), 0);
        assert_eq!(b.needs_you().len(), 1);
    }

    #[test]
    fn set_state_on_unknown_is_ignored() {
        let mut b = Board::default();
        b.apply(Update::SetState(
            PathBuf::from("/s/ghost.sock"),
            State::Working,
        ));
        assert!(b.selectable().is_empty());
    }

    #[test]
    fn gone_removes_agent() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Working)));
        b.apply(Update::Gone(PathBuf::from("/s/pi-1.sock")));
        assert!(b.selectable().is_empty());
    }

    #[test]
    fn selectable_orders_needs_you_first() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Working)));
        b.apply(Update::Upsert(agent("/s/pi-2.sock", State::NeedsYou)));
        let sel = b.selectable();
        assert_eq!(sel[0].state, State::NeedsYou);
        assert_eq!(sel[1].state, State::Working);
    }
}
