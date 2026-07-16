//! The registry-reflect loop, shared by any presentation shell. It owns the
//! `Board` and drives it from the filesystem registry: on a ~1s cadence it
//! scans and prunes the registry, spawns a watcher per live socket, and folds
//! watcher updates into the board. The per-agent age clocks live on the model
//! (`Board::apply`); the engine reads them to format the card age labels. A
//! shell (TUI or GUI) calls `tick` each frame and then renders `board()` plus
//! the age maps. Both shells run on this engine, so scan/prune/watch behavior
//! cannot drift between them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::discovery;
use crate::model::{Board, Update};
use crate::watch;

/// Safety-poll cadence backing the inotify watch on the vetted registry.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);

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
    board: Board,
    dir: PathBuf,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    known: HashSet<PathBuf>,
    dead_sockets: HashSet<PathBuf>,
    dormant_ages: HashMap<String, String>,
    last_scan: Instant,
    /// inotify watch on the vetted registry dir, so a change corrald writes
    /// triggers an immediate rescan instead of waiting for the safety poll.
    /// `None` until the dir exists (re-armed each tick); the fd is forced
    /// non-blocking, so draining events can never block the shell's frame.
    inotify: Option<inotify::Inotify>,
}

/// Arm an inotify watch on `dir` (the vetted registry), non-blocking. Returns
/// `None` if the dir does not exist yet or the watch cannot be set up, so the
/// caller falls back to the safety poll and retries.
fn arm_watch(dir: &Path) -> Option<inotify::Inotify> {
    use std::os::fd::AsRawFd;
    let inotify = inotify::Inotify::init().ok()?;
    // Force O_NONBLOCK so read_events never blocks the UI frame, whatever the
    // crate's init default is.
    let fd = inotify.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return None;
        }
    }
    let mut inotify = inotify;
    inotify
        .add_watch(
            dir,
            inotify::WatchMask::CREATE
                | inotify::WatchMask::MODIFY
                | inotify::WatchMask::MOVED_TO
                | inotify::WatchMask::MOVED_FROM
                | inotify::WatchMask::DELETE,
        )
        .ok()?;
    Some(inotify)
}

impl Engine {
    pub fn new(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            board: Board::default(),
            tx,
            rx,
            known: HashSet::new(),
            dead_sockets: HashSet::new(),
            dormant_ages: HashMap::new(),
            // Force a scan on the first tick.
            last_scan: Instant::now() - SCAN_INTERVAL * 2,
            inotify: arm_watch(&dir),
            dir,
        }
    }

    /// One iteration: rescan/prune/watch when the registry changed (inotify) or
    /// the safety interval elapsed, then drain any pending watcher updates into
    /// the board.
    pub fn tick(&mut self) {
        // Re-arm the watch if it was never set up (dir absent at start) or
        // dropped after an error, then drain events non-blocking.
        if self.inotify.is_none() {
            self.inotify = arm_watch(&self.dir);
        }
        let mut changed = false;
        if let Some(ino) = &mut self.inotify {
            let mut buf = [0u8; 4096];
            match ino.read_events(&mut buf) {
                Ok(events) => changed = events.count() > 0,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => self.inotify = None, // watch broke; poll + re-arm next tick
            }
        }
        if changed || self.last_scan.elapsed() >= SCAN_INTERVAL {
            // Read the vetted registry as-is. Pruning stale/dormant records is
            // corrald's job (it owns the source records and this store); the
            // engine only reflects.
            let entries = discovery::scan_registry(&self.dir);
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
            // The per-agent age clocks live on the model now (Board::apply); the
            // engine only tracks socket liveness for scan/watch bookkeeping.
            match &update {
                Update::Gone(path) => {
                    self.known.remove(path);
                    self.dead_sockets.insert(path.clone());
                }
                Update::Upsert(a) => {
                    self.dead_sockets.remove(&a.socket_path);
                }
                _ => {}
            }
            self.board.apply(update);
        }
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Set the board's inline content filter (empty shows all). A passthrough
    /// so shells never need mutable access to the board itself.
    pub fn set_filter(&mut self, filter: String) {
        self.board.set_filter(filter);
    }

    /// Time each live agent has been in its current state (by socket path),
    /// formatted from the model's per-agent clock.
    pub fn in_state_ages(&self) -> HashMap<PathBuf, String> {
        self.board
            .live_agents()
            .map(|a| (a.socket_path.clone(), age_label(a.state_since.elapsed())))
            .collect()
    }

    /// Time since each live agent last produced activity (by socket path),
    /// formatted from the model's per-agent clock.
    pub fn quiet_ages(&self) -> HashMap<PathBuf, String> {
        self.board
            .live_agents()
            .map(|a| (a.socket_path.clone(), age_label(a.last_activity.elapsed())))
            .collect()
    }

    /// Age of each dormant session's registry record (by session id).
    pub fn dormant_ages(&self) -> &HashMap<String, String> {
        &self.dormant_ages
    }
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
