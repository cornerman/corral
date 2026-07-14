//! Stable path→color-bucket mapping shared by both board shells. The board
//! tags each card's cwd with a colored basename pill; the same directory must
//! get the same color everywhere so the eye can group cards at a glance. This
//! module owns only the index math (a stable hash mod the palette size); each
//! shell owns its own palette (ratatui ANSI vs base16 accents), since the
//! actual colors are rendering-specific. Keyed on the full path, so two
//! same-named leaves under different roots stay distinguishable.

/// The trailing leaf of a path (its basename), the label shown on a cwd pill.
/// Trailing slashes are stripped first, so `/a/corral/` and `/a/corral` both
/// yield `corral` (a naive `rsplit('/')` would return an empty string for the
/// slash-terminated form, drawing an empty pill). Root `/` (or all-slashes)
/// has no leaf, so the whole input is returned rather than an empty string.
pub fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return path;
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Map a path to a palette bucket in `0..n` via a stable FNV-1a hash of the
/// full path string. Deterministic across processes and runs (unlike
/// `DefaultHasher`, which is randomly seeded), so a directory keeps its color.
/// `n == 0` yields 0 (no palette to index).
pub fn color_index(path: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    // FNV-1a 64-bit: tiny, dependency-free, well-spread for short strings.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % n as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_same_bucket() {
        assert_eq!(
            color_index("/home/u/proj", 8),
            color_index("/home/u/proj", 8)
        );
    }

    #[test]
    fn different_leaves_under_different_roots_differ() {
        // The whole point: same basename, different root → keyed on full path.
        // Not guaranteed distinct for every pair (limited palette), but these
        // two must land apart for the grouping aid to mean anything here.
        let a = color_index("/a/proj", 8);
        let b = color_index("/b/proj", 8);
        assert_ne!(a, b);
    }

    #[test]
    fn index_stays_in_range() {
        for p in ["/", "/x", "/home/user/projects/corral", "no-slash"] {
            assert!(color_index(p, 8) < 8);
        }
    }

    #[test]
    fn zero_palette_is_zero() {
        assert_eq!(color_index("/anything", 0), 0);
    }

    #[test]
    fn basename_strips_trailing_slash() {
        assert_eq!(basename("/home/u/projects/corral"), "corral");
        assert_eq!(basename("/home/u/projects/corral/"), "corral");
    }

    #[test]
    fn basename_of_root_is_not_empty() {
        assert_eq!(basename("/"), "/");
        assert_eq!(basename("//"), "//");
    }
}
