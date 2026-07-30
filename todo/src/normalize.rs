//! Coin a missing `id:` and stamp a missing creation date. Pure: the caller
//! supplies today's date, so the function is testable without a clock.
//!
//! Normalization runs inside every read, so there is no way to look at the
//! file and see an unidentified item. It must therefore be idempotent: the
//! watcher hashes the *normalized* file, so a normalization that kept changing
//! bytes would wake the dispatcher forever.

use crate::item::Item;

/// FNV-1a, the same small non-cryptographic hash `core::palette` uses to key a
/// path to a color. An id only needs to be short, stable and unique within one
/// file, so a hash of the item's own text beats a counter (no state to carry)
/// and beats randomness (no dependency, and reproducible in tests).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn base36(mut n: u64, len: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; len];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out).expect("base36 digits are ascii")
}

/// A short id for an item, avoiding every id `taken` reports. Three base36
/// characters (46656 values) is plenty for a human todo file and stays easy to
/// quote in a task prompt and read back out of a report.
pub fn coin_id(text: &str, taken: &dyn Fn(&str) -> bool) -> String {
    for salt in 0u32..10_000 {
        let candidate = base36(fnv1a(format!("{text}{salt}").as_bytes()), 3);
        if !taken(&candidate) {
            return candidate;
        }
    }
    // 10k salted attempts all colliding means the id space is effectively
    // full; failing loud beats returning a duplicate id.
    panic!("could not coin a free id after 10000 attempts; the id space is full");
}

/// Stamp every open item with an `id:` and a creation date. Returns whether
/// anything changed, which is how the caller knows a rewrite is needed.
///
/// A completed line is left alone: it is history, and restamping it would
/// rewrite the file on every read.
pub fn normalize(items: &mut [Item], today: &str) -> bool {
    let mut changed = false;
    let mut taken: Vec<String> = items
        .iter()
        .filter_map(|i| i.key("id").map(|s| s.to_string()))
        .collect();
    for (index, item) in items.iter_mut().enumerate() {
        if item.completed {
            continue;
        }
        if item.key("id").is_none() {
            // Salt the hash input with the item's position so two identical
            // lines in one file still get different ids.
            let text = format!("{}#{index}", item.rest);
            let id = coin_id(&text, &|c| taken.iter().any(|t| t == c));
            taken.push(id.clone());
            item.set_key("id", &id);
            changed = true;
        }
        if item.creation_date.is_none() {
            item.creation_date = Some(today.to_string());
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Item;

    #[test]
    fn coins_ids_and_dates_for_a_bare_line() {
        let mut items = vec![Item::parse("brain dump this").unwrap()];
        assert!(normalize(&mut items, "2026-07-26"));
        assert_eq!(items[0].creation_date.as_deref(), Some("2026-07-26"));
        assert!(items[0].key("id").is_some());
    }

    #[test]
    fn is_idempotent_so_normalizing_is_not_a_change() {
        let mut items = vec![Item::parse("brain dump this").unwrap()];
        normalize(&mut items, "2026-07-26");
        let before = items[0].render();
        assert!(!normalize(&mut items, "2026-07-27"));
        assert_eq!(items[0].render(), before);
    }

    #[test]
    fn ids_are_unique_even_for_identical_text() {
        let mut items = vec![
            Item::parse("same text").unwrap(),
            Item::parse("same text").unwrap(),
        ];
        normalize(&mut items, "2026-07-26");
        assert_ne!(items[0].key("id"), items[1].key("id"));
    }

    #[test]
    fn does_not_restamp_a_completed_line() {
        let mut items = vec![Item::parse("x 2026-07-25 2026-07-23 done thing id:b8c").unwrap()];
        assert!(!normalize(&mut items, "2026-07-26"));
    }

    #[test]
    fn coined_ids_are_short_and_lowercase_alphanumeric() {
        let id = coin_id("anything", &|_| false);
        assert_eq!(id.len(), 3);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn coin_id_avoids_taken_ids() {
        let first = coin_id("anything", &|_| false);
        let second = coin_id("anything", &|id| id == first);
        assert_ne!(first, second);
    }
}
