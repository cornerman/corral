//! The board's in-memory model. Identity is the socket path: one socket is one
//! live agent. Nothing is persisted; the model is rebuilt live from discovery
//! and the per-socket watchers.

use std::collections::BTreeMap;
use std::path::PathBuf;

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
    /// A title change (from a session_info_update broadcast on rename).
    SetTitle(PathBuf, Option<String>),
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
            Update::SetTitle(path, title) => {
                if let Some(a) = self.agents.get_mut(&path) {
                    a.title = title;
                }
            }
            Update::Gone(path) => {
                self.agents.remove(&path);
            }
        }
    }

    /// Agents in a given state, in stable order.
    pub fn in_state(&self, state: State) -> Vec<&Agent> {
        self.agents.values().filter(|a| a.state == state).collect()
    }

    /// Selectable agents in attention priority: RequiresAction first (blocked on
    /// the operator), then Idle (awaiting the next task), then Running (leave
    /// alone). The UI's selection index is over this flattened order.
    pub fn selectable(&self) -> Vec<&Agent> {
        let mut v = self.in_state(State::RequiresAction);
        v.extend(self.in_state(State::Idle));
        v.extend(self.in_state(State::Running));
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
    fn selectable_orders_by_attention_priority() {
        let mut b = Board::default();
        b.apply(Update::Upsert(agent("/s/pi-1.sock", State::Running)));
        b.apply(Update::Upsert(agent("/s/pi-2.sock", State::Idle)));
        b.apply(Update::Upsert(agent("/s/pi-3.sock", State::RequiresAction)));
        let sel = b.selectable();
        assert_eq!(sel[0].state, State::RequiresAction);
        assert_eq!(sel[1].state, State::Idle);
        assert_eq!(sel[2].state, State::Running);
    }
}
