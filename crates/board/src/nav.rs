//! Selection navigation over the board's columns. Pure index math on the
//! per-column counts (`Board::column_counts`, in `Column::ALL` order): the
//! selection is one flat index across all columns, and these functions move it
//! within a column (up/down) or across columns (left/right).

/// Flat selectable index -> (column, row).
fn locate(index: usize, counts: &[usize; 4]) -> (usize, usize) {
    let mut i = index;
    for (c, &n) in counts.iter().enumerate() {
        if i < n {
            return (c, i);
        }
        i -= n;
    }
    (0, 0)
}

/// (column, row) -> flat selectable index.
fn flat(col: usize, row: usize, counts: &[usize; 4]) -> usize {
    counts[..col].iter().sum::<usize>() + row
}

/// Move within the current column (Up/Down), clamped to that column.
pub fn move_row(index: usize, counts: &[usize; 4], down: bool) -> usize {
    let (c, r) = locate(index, counts);
    if counts[c] == 0 {
        return index;
    }
    let r = if down {
        (r + 1).min(counts[c] - 1)
    } else {
        r.saturating_sub(1)
    };
    flat(c, r, counts)
}

/// Jump to the nearest non-empty column in a direction (Left/Right), keeping
/// the row where possible.
pub fn move_col(index: usize, counts: &[usize; 4], right: bool) -> usize {
    let (c, r) = locate(index, counts);
    let candidates: Vec<usize> = if right {
        (c + 1..counts.len()).collect()
    } else {
        (0..c).rev().collect()
    };
    for tc in candidates {
        if counts[tc] > 0 {
            return flat(tc, r.min(counts[tc] - 1), counts);
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_maps_flat_index_to_columns() {
        // RequiresAction=2, Idle=0, Running=1, Dormant=0. order: RA0, RA1, Run0.
        let counts = [2usize, 0, 1, 0];
        assert_eq!(locate(0, &counts), (0, 0));
        assert_eq!(locate(2, &counts), (2, 0));
        // Down within the column, clamped.
        assert_eq!(move_row(0, &counts, true), 1);
        assert_eq!(move_row(1, &counts, true), 1);
        assert_eq!(move_row(1, &counts, false), 0);
        // Right from RA skips the empty Idle column to Running.
        assert_eq!(move_col(1, &counts, true), 2);
        // Left from Running lands back in RA, row clamped.
        assert_eq!(move_col(2, &counts, false), 0);
        // Right from the last column stays put.
        assert_eq!(move_col(2, &counts, true), 2);
    }
}
