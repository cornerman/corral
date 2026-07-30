//! The poll loop: normalize, fingerprint, and on a change ensure exactly one
//! dispatcher is awake in the todo directory.
//!
//! Polling rather than inotify on purpose: a few seconds of latency is
//! invisible for a todo board, the fingerprint gives idempotence for free, and
//! there is no watch-descriptor bookkeeping to get wrong when an editor
//! replaces the file instead of writing into it.
//!
//! This runs as its own process rather than inside the dispatcher because the
//! thing that must not fail is *noticing*: wake logic inside the dispatcher
//! dies with the dispatcher, and a supervisor belongs outside the process whose
//! liveness is in question.

use crate::item::Item;
use crate::store::Store;
use crate::wake::{plan, Wake, WAKE_MESSAGE};
use corral_core::discovery::{scan_registry, RegistryEntry};
use corral_core::launch::Launcher;
use corral_core::prompt::send_prompt;
use std::path::PathBuf;
use std::time::Duration;

/// A content hash of the **normalized** items. Normalized, so that stamping an
/// id or a creation date is not itself seen as a change; otherwise a fresh
/// brain-dump would wake the dispatcher twice.
pub fn fingerprint(items: &[Item]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in items
        .iter()
        .map(|i| i.render())
        .collect::<Vec<_>>()
        .join("\n")
        .bytes()
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub struct Watcher {
    store: Store,
    dir: PathBuf,
    dispatch_argv: Vec<String>,
    interval: Duration,
    seen: Option<u64>,
}

impl Watcher {
    pub fn new(
        store: Store,
        dir: PathBuf,
        dispatch_argv: Vec<String>,
        interval: Duration,
    ) -> Watcher {
        Watcher {
            store,
            dir,
            dispatch_argv,
            interval,
            seen: None,
        }
    }

    /// The todo directory's own registry. Deliberately not corrald's vetted
    /// `state/registry`: the record's physical location proves its directory,
    /// the watcher runs as the operator on the trusted side of that boundary,
    /// and the wake path must keep working while corrald is down.
    fn records(&self) -> Vec<RegistryEntry> {
        let mut entries = scan_registry(&self.dir.join(".corral").join("registry"));
        for entry in &mut entries {
            // Records carry no `cwd` field (CONVENTION v2); the directory they
            // were read from is the trusted value.
            entry.cwd = Some(self.dir.to_string_lossy().to_string());
        }
        entries
    }

    /// One poll. Returns the wake that succeeded, or `None` when the file did
    /// not change. Returning the action is what makes the loop testable without
    /// a real agent.
    ///
    /// The fingerprint advances only after a wake lands, so a change whose wake
    /// failed stays pending and is retried next tick.
    pub fn tick(&mut self, launcher: &dyn Launcher) -> Result<Option<Wake>, String> {
        if !self.store.path().exists() {
            // Nothing to watch yet. Not an error: the operator may create the
            // file after starting the service.
            return Ok(None);
        }
        let items = self.store.read_normalized()?;
        let print = fingerprint(&items);
        if self.seen == Some(print) {
            return Ok(None);
        }
        // Try each step of the chain, keeping the last error: a record's socket
        // being set is no proof it connects, so a crashed dispatcher falls
        // through to resume and then to a fresh spawn.
        let mut last = String::from("no wake step available");
        for step in plan(&self.records(), &self.dispatch_argv) {
            let attempt = match &step {
                Wake::Inject { socket } => send_prompt(socket, WAKE_MESSAGE)
                    .map_err(|e| format!("cannot wake over {}: {e}", socket.display())),
                Wake::Launch { argv, mode } => {
                    launcher.launch(&self.dir, argv, Some(WAKE_MESSAGE), mode)
                }
            };
            match attempt {
                Ok(()) => {
                    self.seen = Some(print);
                    return Ok(Some(step));
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Poll forever. A failed wake is reported and the loop continues; since the
    /// fingerprint did not advance, the pending change is retried next tick.
    pub fn run(&mut self, launcher: &dyn Launcher) {
        loop {
            if let Err(e) = self.tick(launcher) {
                eprintln!("corral-todo watch: {e}");
            }
            std::thread::sleep(self.interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corral_core::launch::LaunchMode;
    use std::path::Path;
    use std::sync::Mutex;

    /// One recorded call to `Launcher::launch`.
    struct LaunchCall {
        cwd: PathBuf,
        command: Vec<String>,
        message: Option<String>,
        mode: LaunchMode,
    }

    /// Records what it was asked to launch instead of launching anything.
    struct FakeLauncher(Mutex<Vec<LaunchCall>>);

    impl Launcher for FakeLauncher {
        fn launch(
            &self,
            cwd: &Path,
            command: &[String],
            message: Option<&str>,
            mode: &LaunchMode,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(LaunchCall {
                cwd: cwd.to_path_buf(),
                command: command.to_vec(),
                message: message.map(|m| m.to_string()),
                mode: mode.clone(),
            });
            Ok(())
        }
    }

    fn watcher(dir: &Path) -> Watcher {
        Watcher::new(
            Store::new(dir.join("todo.txt")),
            dir.to_path_buf(),
            vec!["pi".to_string()],
            Duration::from_secs(5),
        )
    }

    #[test]
    fn normalizing_a_fresh_dump_costs_exactly_one_wake() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "brand new idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        // First tick sees a change (nothing was known before).
        assert!(w.tick(&launcher).unwrap().is_some());
        // Second tick must be quiet: normalization stamped an id and a date,
        // and hashing the normalized items is what keeps that from counting.
        assert!(w.tick(&launcher).unwrap().is_none());
    }

    #[test]
    fn an_edit_wakes_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, "one idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        assert!(w.tick(&launcher).unwrap().is_none());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("another idea\n");
        std::fs::write(&path, text).unwrap();
        assert!(w.tick(&launcher).unwrap().is_some());
    }

    #[test]
    fn with_no_record_it_spawns_the_dispatcher_hidden_with_the_wake_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "an idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        let calls = launcher.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cwd, dir.path());
        assert_eq!(calls[0].command, vec!["pi".to_string()]);
        assert_eq!(calls[0].message.as_deref(), Some(WAKE_MESSAGE));
        assert!(calls[0].mode.hidden, "a dispatcher must never pop a window");
    }

    #[test]
    fn a_change_whose_wake_failed_is_retried_on_the_next_tick() {
        // Noticing is this process's one job, so a failed wake must not be
        // recorded as handled.
        struct Failing;
        impl Launcher for Failing {
            fn launch(
                &self,
                _: &Path,
                _: &[String],
                _: Option<&str>,
                _: &LaunchMode,
            ) -> Result<(), String> {
                Err("no terminal".into())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "an idea\n").unwrap();
        let mut w = watcher(dir.path());
        assert!(w.tick(&Failing).is_err());
        assert!(
            w.tick(&Failing).is_err(),
            "the change must still be pending"
        );
        // Once a wake can succeed, the pending change is delivered.
        let ok = FakeLauncher(Mutex::new(Vec::new()));
        assert!(w.tick(&ok).unwrap().is_some());
        assert!(w.tick(&ok).unwrap().is_none(), "and then it settles");
    }

    #[test]
    fn a_missing_todo_file_is_not_a_wake_and_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        assert!(w.tick(&launcher).unwrap().is_none());
        assert!(launcher.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_fingerprint_tracks_content_and_nothing_else() {
        let a = vec![Item::parse("one id:a").unwrap()];
        let b = vec![Item::parse("one   id:a").unwrap()];
        // Whitespace collapses in parsing, so it cannot cause a spurious wake.
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let c = vec![Item::parse("one id:a status:progress").unwrap()];
        assert_ne!(fingerprint(&a), fingerprint(&c));
    }
}
