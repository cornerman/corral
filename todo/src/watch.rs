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

use crate::item::{Item, State};
use crate::store::Store;
use crate::wake::{plan, Wake, POLICY_FILE};
use corral_core::discovery::{scan_registry, RegistryEntry};
use corral_core::launch::Launcher;
use corral_core::prompt::send_prompt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a fresh spawn is trusted to still be booting. An agent needs
/// several seconds between process start and announcing its record, and a poll
/// interval is shorter than that, so without a grace window a second edit in
/// that gap would spawn a second dispatcher (observed in the VM: two sessions
/// 4s apart). While the grace holds, a pending change waits and is delivered
/// via inject once the record appears; after it expires the spawn is presumed
/// dead and spawning is allowed again.
const SPAWN_GRACE: Duration = Duration::from_secs(60);

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

/// What one successful wake did. Returned so the shell can log it and a test can
/// assert on it, keeping the decision and its reporting in separate places.
///
/// The fingerprint is in the line because convergence is the property most worth
/// watching: a settled system logs nothing, while a dispatcher that rewrites the
/// file pointlessly shows up as a run of wakes with *different* fingerprints, and
/// a genuine repeat (a wake that failed and was retried) as the same one twice.
#[derive(Debug)]
pub struct Woke {
    pub step: Wake,
    pub fingerprint: u64,
    pub items: usize,
    pub open: usize,
}

impl std::fmt::Display for Woke {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wake {:016x} via {} ({} item{}, {} open)",
            self.fingerprint,
            self.step.kind_name(),
            self.items,
            if self.items == 1 { "" } else { "s" },
            self.open
        )
    }
}

pub struct Watcher {
    store: Store,
    dir: PathBuf,
    dispatch_argv: Vec<String>,
    interval: Duration,
    seen: Option<u64>,
    /// When the last successful `Spawn` happened, while we wait for it to
    /// announce. Cleared once any other wake step lands.
    spawned_at: Option<Instant>,
    /// The grace window; a field (not the const) so tests can shrink it.
    spawn_grace: Duration,
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
            spawned_at: None,
            spawn_grace: SPAWN_GRACE,
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
    pub fn tick(&mut self, launcher: &dyn Launcher) -> Result<Option<Woke>, String> {
        // Refuse to wake an agent that has no policy to follow: nothing loads
        // `DISPATCHER.md` automatically, and a generic agent handed "run your
        // dispatcher loop" would flail silently. Checked every tick, not once
        // at startup, so deleting the file stops dispatch instead of quietly
        // degrading it.
        let policy = self.dir.join(POLICY_FILE);
        if !policy.exists() {
            return Err(format!(
                "no {POLICY_FILE} in {} — run `corral-todo init {}` to write it",
                self.dir.display(),
                self.dir.display()
            ));
        }
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
            // Never stack dispatchers: a spawn that has not announced yet is
            // still booting, so hold this change (the fingerprint stays
            // pending) instead of spawning a sibling next to it.
            if matches!(step, Wake::Spawn { .. }) {
                if let Some(at) = self.spawned_at {
                    if at.elapsed() < self.spawn_grace {
                        last = String::from(
                            "waiting for the spawned dispatcher to announce; change stays pending",
                        );
                        continue;
                    }
                }
            }
            // Each step carries its own text: only a session with no history
            // gets told what it is (see `wake::FIRST_PROMPT`).
            let message = step.message();
            let attempt = match (&step, step.launch_args()) {
                (Wake::Inject { socket }, _) => send_prompt(socket, message)
                    .map_err(|e| format!("cannot wake over {}: {e}", socket.display())),
                (_, Some((argv, mode))) => launcher.launch(&self.dir, argv, Some(message), mode),
                // Unreachable: every non-Inject variant launches.
                (_, None) => Err("wake step has nothing to run".to_string()),
            };
            match attempt {
                Ok(()) => {
                    self.spawned_at = match step {
                        Wake::Spawn { .. } => Some(Instant::now()),
                        // An inject or resume proves a session exists, so the
                        // boot watch is over.
                        _ => None,
                    };
                    self.seen = Some(print);
                    return Ok(Some(Woke {
                        step,
                        fingerprint: print,
                        items: items.len(),
                        open: items.iter().filter(|i| i.state() == State::Open).count(),
                    }));
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Poll forever, logging one line per wake and per failure. A settled system
    /// logs nothing, so anything in the journal is a real event.
    ///
    /// A failed wake is reported and the loop continues; since the fingerprint
    /// did not advance, the pending change is retried next tick.
    pub fn run(&mut self, launcher: &dyn Launcher) {
        loop {
            match self.tick(launcher) {
                Ok(Some(woke)) => eprintln!("corral-todo watch: {woke}"),
                Ok(None) => {}
                Err(e) => eprintln!("corral-todo watch: {e}"),
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

    /// A todo dir with the policy file present, which `tick` requires.
    fn todo_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(POLICY_FILE), "policy").unwrap();
        dir
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
        let dir = todo_dir();
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
        let dir = todo_dir();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, "one idea\n").unwrap();
        let mut w = watcher(dir.path());
        // The boot grace (its own tests below) would hold the second wake;
        // here the subject is only change detection.
        w.spawn_grace = Duration::ZERO;
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
        let dir = todo_dir();
        std::fs::write(dir.path().join("todo.txt"), "an idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        let calls = launcher.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cwd, dir.path());
        assert_eq!(calls[0].command, vec!["pi".to_string()]);
        // A brand-new session gets the first-run prompt, which tells it what it
        // is and where its policy lives.
        assert_eq!(calls[0].message.as_deref(), Some(crate::wake::FIRST_PROMPT));
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
        let dir = todo_dir();
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
    fn an_edit_during_dispatcher_boot_does_not_spawn_a_sibling() {
        // A spawned agent needs seconds to announce; a second change in that
        // gap must wait for it, not start a second dispatcher beside it.
        let dir = todo_dir();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, "one idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        assert_eq!(launcher.0.lock().unwrap().len(), 1);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("another idea\n");
        std::fs::write(&path, text).unwrap();
        // The change is held pending, not lost and not double-spawned.
        let err = w.tick(&launcher).unwrap_err();
        assert!(err.contains("pending"), "{err}");
        assert_eq!(launcher.0.lock().unwrap().len(), 1);
        // The dispatcher announces (a dormant record is enough: inject is
        // unavailable but resume reaches that same session).
        let reg = dir.path().join(".corral").join("registry");
        std::fs::create_dir_all(&reg).unwrap();
        std::fs::write(
            reg.join("S1.json"),
            r#"{"sessionId":"S1","label":"pi","resumeCommand":["pi","--session","{sessionId}"],"lastSeen":"2026-07-31T10:00:00Z"}"#,
        )
        .unwrap();
        let woke = w.tick(&launcher).unwrap().expect("the held change lands");
        assert_eq!(woke.step.kind_name(), "resume");
        // And the boot watch is over: a later change may spawn again if the
        // record disappears.
        std::fs::remove_file(reg.join("S1.json")).unwrap();
        std::fs::write(&path, "third idea\n").unwrap();
        assert_eq!(
            w.tick(&launcher).unwrap().expect("a wake").step.kind_name(),
            "spawn"
        );
    }

    #[test]
    fn after_the_grace_expires_a_silent_spawn_is_retried() {
        // A spawn that never announces is presumed dead; the system must not
        // wait on it forever.
        let dir = todo_dir();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, "one idea\n").unwrap();
        let mut w = watcher(dir.path());
        w.spawn_grace = Duration::ZERO;
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        w.tick(&launcher).unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("another idea\n");
        std::fs::write(&path, text).unwrap();
        assert!(w.tick(&launcher).unwrap().is_some());
        assert_eq!(launcher.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_missing_todo_file_is_not_a_wake_and_not_an_error() {
        let dir = todo_dir();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        assert!(w.tick(&launcher).unwrap().is_none());
        assert!(launcher.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_wake_reports_its_branch_the_fingerprint_and_the_counts() {
        let dir = todo_dir();
        std::fs::write(
            dir.path().join("todo.txt"),
            "open one\nx 2026-07-01 2026-07-01 done one id:d1\n",
        )
        .unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        let woke = w.tick(&launcher).unwrap().expect("a wake");
        assert_eq!(woke.items, 2);
        assert_eq!(woke.open, 1, "the completed line is not open");
        let line = woke.to_string();
        assert!(line.contains("via spawn"), "{line}");
        assert!(line.contains("2 items, 1 open"), "{line}");
        // Singular reads as "1 item", not "1 items".
        let one = Woke {
            step: Wake::Inject {
                socket: PathBuf::from("/x"),
            },
            fingerprint: 1,
            items: 1,
            open: 1,
        };
        assert!(one.to_string().contains("1 item,"), "{one}");
        assert!(
            line.contains(&format!("{:016x}", woke.fingerprint)),
            "{line}"
        );
    }

    #[test]
    fn without_a_policy_file_it_refuses_to_wake_anything() {
        // A dispatcher with no policy is a generic agent handed an
        // uninterpretable nudge, so this must fail loud rather than dispatch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "an idea\n").unwrap();
        let mut w = watcher(dir.path());
        let launcher = FakeLauncher(Mutex::new(Vec::new()));
        let err = w.tick(&launcher).unwrap_err();
        assert!(err.contains(POLICY_FILE), "{err}");
        assert!(err.contains("corral-todo init"), "{err}");
        assert!(launcher.0.lock().unwrap().is_empty(), "nothing may launch");
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
