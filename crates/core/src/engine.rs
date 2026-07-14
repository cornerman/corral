//! The registry-reflect loop, shared by any presentation shell. It owns the
//! `Board` and drives it from the filesystem registry: on a ~1s cadence it
//! scans and prunes the registry, spawns a watcher per live socket, and folds
//! watcher updates into the board while tracking per-agent age timers. A shell
//! (TUI or GUI) calls `tick` each frame and then renders `board()` plus the age
//! maps.
//!
//! (The ratatui board still runs an equivalent loop inline; converging it onto
//! this engine, or retiring it, is tracked in TODO.md.)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::discovery::{self, RegistryEntry};
use crate::model::{Board, Update};
use crate::watch;

/// How often the registry is rescanned.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);

/// A dormant record untouched for this long is pruned (its session file is
/// stale or abandoned), measured from the registry file's mtime.
const DORMANT_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// A compact age string: `8s` / `5m` / `2h` / `3d`.
pub fn age_label(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// The shared registry-reflect state. Construct with `new`, call `tick` on each
/// frame, then read `board` and the age maps.
pub struct Engine {
    dir: PathBuf,
    board: Board,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    known: HashSet<PathBuf>,
    dead_sockets: HashSet<PathBuf>,
    state_since: HashMap<PathBuf, Instant>,
    last_event: HashMap<PathBuf, Instant>,
    dormant_ages: HashMap<String, String>,
    last_scan: Instant,
}

impl Engine {
    pub fn new(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            dir,
            board: Board::default(),
            tx,
            rx,
            known: HashSet::new(),
            dead_sockets: HashSet::new(),
            state_since: HashMap::new(),
            last_event: HashMap::new(),
            dormant_ages: HashMap::new(),
            // Force a scan on the first tick.
            last_scan: Instant::now() - SCAN_INTERVAL * 2,
        }
    }

    /// One iteration: rescan/prune/watch on the scan cadence, then drain any
    /// pending watcher updates into the board.
    pub fn tick(&mut self) {
        if self.last_scan.elapsed() >= SCAN_INTERVAL {
            let entries = prune(&self.dir, discovery::scan_registry(&self.dir));
            self.dead_sockets.retain(|p| {
                entries
                    .iter()
                    .any(|e| e.socket.as_deref() == Some(p.as_path()))
            });
            for entry in &entries {
                if let Some(sock) = discovery::live_socket(entry) {
                    if self.known.insert(sock.path.clone()) {
                        watch::spawn(sock, self.tx.clone());
                    }
                }
            }
            self.board.sync_registry(&entries, &self.dead_sockets);
            self.dormant_ages.clear();
            for e in &entries {
                let file = self.dir.join(format!("{}.json", e.session_id));
                if let Some(age) = std::fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(age_label)
                {
                    self.dormant_ages.insert(e.session_id.clone(), age);
                }
            }
            self.last_scan = Instant::now();
        }

        while let Ok(update) = self.rx.try_recv() {
            match &update {
                Update::Gone(path) => {
                    self.known.remove(path);
                    self.state_since.remove(path);
                    self.last_event.remove(path);
                    self.dead_sockets.insert(path.clone());
                }
                Update::SetState(path, _) => {
                    let now = Instant::now();
                    self.state_since.insert(path.clone(), now);
                    self.last_event.insert(path.clone(), now);
                }
                Update::SetActivity(path, _) => {
                    self.last_event.insert(path.clone(), Instant::now());
                }
                Update::Upsert(a) => {
                    let now = Instant::now();
                    self.state_since.entry(a.socket_path.clone()).or_insert(now);
                    self.last_event.entry(a.socket_path.clone()).or_insert(now);
                    self.dead_sockets.remove(&a.socket_path);
                }
                Update::SetTitle(..) => {}
            }
            self.board.apply(update);
        }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Time each live agent has been in its current state (by socket path).
    pub fn in_state_ages(&self) -> HashMap<PathBuf, String> {
        self.state_since
            .iter()
            .map(|(p, t)| (p.clone(), age_label(t.elapsed())))
            .collect()
    }

    /// Time since each live agent last produced activity (by socket path).
    pub fn quiet_ages(&self) -> HashMap<PathBuf, String> {
        self.last_event
            .iter()
            .map(|(p, t)| (p.clone(), age_label(t.elapsed())))
            .collect()
    }

    /// Age of each dormant session's registry record (by session id).
    pub fn dormant_ages(&self) -> &HashMap<String, String> {
        &self.dormant_ages
    }
}

/// Prune dormant records that are not resumable or have not been touched in
/// `DORMANT_MAX_AGE`. Live records (socket set) are never pruned. A deleted
/// session file is no longer detected here (the resume command is an opaque
/// argv, not a path to stat); such a record fails at resume time and ages out.
fn prune(dir: &Path, entries: Vec<RegistryEntry>) -> Vec<RegistryEntry> {
    entries
        .into_iter()
        .filter(|e| {
            if e.socket.is_some() {
                return true; // live: not ours to prune
            }
            let dead = e.resume_command.is_none();
            let file = dir.join(format!("{}.json", e.session_id));
            let stale = std::fs::metadata(&file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > DORMANT_MAX_AGE);
            if dead || stale {
                let _ = std::fs::remove_file(&file);
                return false;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_label_scales_units() {
        assert_eq!(age_label(Duration::from_secs(8)), "8s");
        assert_eq!(age_label(Duration::from_secs(5 * 60)), "5m");
        assert_eq!(age_label(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(age_label(Duration::from_secs(3 * 86400)), "3d");
    }

    #[test]
    fn empty_registry_yields_empty_board() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Engine::new(tmp.path().to_path_buf());
        e.tick();
        assert_eq!(e.board().column_counts().iter().sum::<usize>(), 0);
    }
}
