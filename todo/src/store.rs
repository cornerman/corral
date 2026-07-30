//! The one write path to `todo.txt`: exclusive `flock`, read, mutate, rewrite
//! through a temp file plus rename.
//!
//! Every mutation goes through `mutate`, so a read-modify-write is atomic
//! against a second dispatcher session, against the watcher's own
//! normalization, and against an editor that honors the lock.

use crate::item::Item;
use crate::normalize::normalize;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub struct Store {
    path: PathBuf,
}

/// An exclusive `flock` held for the lifetime of the value.
struct Lock(std::fs::File);

impl Lock {
    /// Lock a sidecar `<path>.lock`, never `todo.txt` itself: the write path
    /// replaces `todo.txt` by rename, so a lock on that inode would guard a
    /// file no later reader opens.
    fn acquire(path: &Path) -> Result<Lock, String> {
        let lock_path = path.with_extension("txt.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| format!("cannot open lock file {}: {e}", lock_path.display()))?;
        // Blocking LOCK_EX: a todo file has one or two writers and a turn is
        // milliseconds, so waiting is simpler and more correct than a retry
        // loop with a timeout.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "cannot lock {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(Lock(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the fd releases the lock anyway; unlocking explicitly keeps
        // the release visible at the end of the critical section.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Store {
        Store { path: path.into() }
    }

    /// The todo file this store owns. The watcher needs it so its existence
    /// check and its reads cannot disagree about which file is watched.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Today's date in todo.txt's `YYYY-MM-DD` form, from the system clock.
    /// The pure modules take a date string instead of reading a clock, so this
    /// is the one place time enters the crate.
    pub fn today() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        civil_date(secs)
    }

    /// Read the file's items. Blank lines are dropped rather than preserved:
    /// the item list is the file's whole meaning, and carrying blank lines
    /// through every mutation would mean tracking positions that no consumer
    /// reads. A file with grouping blank lines therefore loses them on first
    /// write (recorded in `todo/SPEC.md`'s known limits).
    fn read_items(&self) -> Result<Vec<Item>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text.lines().filter_map(Item::parse).collect()),
            // A todo file that does not exist yet is an empty list, not an
            // error: `add` is allowed to create it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read {}: {e}", self.path.display())),
        }
    }

    fn write_items(&self, items: &[Item]) -> Result<(), String> {
        let body: String = items
            .iter()
            .map(|i| i.render())
            .collect::<Vec<_>>()
            .join("\n");
        write_atomic(&self.path, &format!("{body}\n"))
    }

    /// Lock, read, normalize, hand the items to `f`, and rewrite. An `Err` from
    /// `f` aborts the write, so a refused change leaves the file exactly as it
    /// was. Every mutation in the crate goes through here.
    pub fn mutate<T>(
        &self,
        f: impl FnOnce(&mut Vec<Item>) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = Lock::acquire(&self.path)?;
        let mut items = self.read_items()?;
        normalize(&mut items, &Store::today());
        let out = f(&mut items)?;
        self.write_items(&items)?;
        Ok(out)
    }

    /// The normalized items, writing back **only if** normalization changed
    /// something. A read that always wrote would make `list` rewrite the file
    /// every time and the watcher rewrite it every tick, forever.
    pub fn read_normalized(&self) -> Result<Vec<Item>, String> {
        let _lock = Lock::acquire(&self.path)?;
        let mut items = self.read_items()?;
        if normalize(&mut items, &Store::today()) {
            self.write_items(&items)?;
        }
        Ok(items)
    }

    /// Move completed lines out to `done.txt` beside the todo file, the
    /// todo.txt archive convention. Returns how many lines moved.
    pub fn archive(&self) -> Result<usize, String> {
        let done_path = self.path.with_file_name("done.txt");
        self.mutate(|items| {
            let (done, keep): (Vec<Item>, Vec<Item>) =
                items.iter().cloned().partition(|i| i.completed);
            if done.is_empty() {
                return Ok(0);
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&done_path)
                .map_err(|e| format!("cannot open {}: {e}", done_path.display()))?;
            for item in &done {
                writeln!(file, "{}", item.render())
                    .map_err(|e| format!("cannot append to {}: {e}", done_path.display()))?;
            }
            *items = keep;
            Ok(done.len())
        })
    }
}

/// A unix timestamp as a `YYYY-MM-DD` UTC date. Split out of `today` so the
/// arithmetic is testable at fixed instants rather than only against whatever
/// the clock says now.
///
/// No chrono in the workspace, so this is Howard Hinnant's days-from-civil
/// inverse (`http://howardhinnant.github.io/date_algorithms.html`), whose era
/// arithmetic handles leap years and century rules without a table.
fn civil_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Shift the epoch to 0000-03-01 so a leap day lands at the end of a
    // 400-year era, which is what makes the rest plain division.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    // March-based month back to January-based, carrying the year.
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Write via a temp file in the same directory plus rename, so a reader sees
/// either the old file or the new one and never a truncated one.
fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(contents: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.txt");
        std::fs::write(&path, contents).unwrap();
        (dir, Store::new(path))
    }

    #[test]
    fn reads_and_normalizes_then_persists_the_ids() {
        let (_dir, store) = store_with("first thing\nsecond thing\n");
        let items = store.read_normalized().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.key("id").is_some()));
        // Re-reading must find the same ids, i.e. the normalization was written
        // back rather than recomputed each time.
        let again = store.read_normalized().unwrap();
        assert_eq!(items[0].key("id"), again[0].key("id"));
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("todo.txt"));
        assert!(store.read_normalized().unwrap().is_empty());
    }

    #[test]
    fn mutate_writes_back_the_mutated_items() {
        let (_dir, store) = store_with("do it id:a7f\n");
        store
            .mutate(|items| {
                items[0].set_key("status", "progress");
                Ok(())
            })
            .unwrap();
        let items = store.read_normalized().unwrap();
        assert_eq!(items[0].key("status"), Some("progress"));
    }

    #[test]
    fn an_error_from_the_closure_leaves_the_file_untouched() {
        let (_dir, store) = store_with("do it id:a7f\n");
        let err = store
            .mutate(|items| {
                items[0].set_key("status", "progress");
                Err::<(), String>("nope".into())
            })
            .unwrap_err();
        assert_eq!(err, "nope");
        assert_eq!(store.read_normalized().unwrap()[0].key("status"), None);
    }

    #[test]
    fn a_read_of_an_already_normalized_file_does_not_write() {
        // Otherwise every `list`, and every watcher tick, rewrites the file
        // forever.
        let (_dir, store) = store_with("do it id:a7f\n");
        store.read_normalized().unwrap();
        let before = std::fs::metadata(store.path()).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        store.read_normalized().unwrap();
        let after = std::fs::metadata(store.path()).unwrap().modified().unwrap();
        assert_eq!(before, after, "a no-op read must not touch the file");
    }

    #[test]
    fn archive_moves_completed_lines_to_done_txt() {
        let (dir, store) = store_with("x 2026-07-25 done one id:b8c\nopen one id:a7f\n");
        assert_eq!(store.archive().unwrap(), 1);
        let remaining = store.read_normalized().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key("id"), Some("a7f"));
        let done = std::fs::read_to_string(dir.path().join("done.txt")).unwrap();
        assert!(done.contains("done one id:b8c"));
    }

    #[test]
    fn archive_appends_rather_than_replacing_done_txt() {
        let (dir, store) = store_with("x 2026-07-25 second id:b8c\n");
        std::fs::write(dir.path().join("done.txt"), "x 2026-07-01 first id:z1z\n").unwrap();
        store.archive().unwrap();
        let done = std::fs::read_to_string(dir.path().join("done.txt")).unwrap();
        assert!(done.contains("first id:z1z") && done.contains("second id:b8c"));
    }

    #[test]
    fn today_is_an_iso_date() {
        let today = Store::today();
        assert_eq!(today.len(), 10);
        assert_eq!(today.matches('-').count(), 2);
    }

    #[test]
    fn civil_date_converts_known_instants() {
        // Cross-checked against `date -u -d @<secs> +%F`.
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(86_399), "1970-01-01");
        assert_eq!(civil_date(86_400), "1970-01-02");
        // A leap day, and the day after, in a leap year divisible by 4.
        assert_eq!(civil_date(1_709_164_800), "2024-02-29");
        assert_eq!(civil_date(1_709_251_200), "2024-03-01");
        // 2000 was a leap year (divisible by 400) though 1900 was not.
        assert_eq!(civil_date(951_782_400), "2000-02-29");
        assert_eq!(civil_date(1_782_000_000), "2026-06-21");
        // Before the epoch: `div_euclid` floors, so the date does not jump.
        assert_eq!(civil_date(-1), "1969-12-31");
    }
}
