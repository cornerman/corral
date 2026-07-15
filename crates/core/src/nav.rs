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

/// True if a vertical move (`down`) from `index` would leave its column — it
/// already sits at the far edge (top for up, bottom for down). The hook a shell
/// uses to hand focus back to the filter input: the input sits as one node
/// above the first row and below the last, so navigation rings through it.
pub fn at_vertical_edge(index: usize, counts: &[usize; 4], down: bool) -> bool {
    let (c, r) = locate(index, counts);
    if counts[c] == 0 {
        return true;
    }
    if down {
        r + 1 >= counts[c]
    } else {
        r == 0
    }
}

/// Landing index when entering the board from the filter input: the first row
/// (`down`) or the last row (up) of the column holding `index`. The inverse of
/// `at_vertical_edge` — stepping off the input into the board. Empty column or
/// empty board leaves `index` unchanged.
pub fn column_entry(index: usize, counts: &[usize; 4], down: bool) -> usize {
    let (c, _) = locate(index, counts);
    if counts[c] == 0 {
        return index;
    }
    let r = if down { 0 } else { counts[c] - 1 };
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

    #[test]
    fn vertical_edges_ring_through_the_input() {
        // RA=2, Idle=0, Running=1, Dormant=0. flat: RA0=0, RA1=1, Run0=2.
        let counts = [2usize, 0, 1, 0];
        // Top of a column (row 0) is an up-edge; not a down-edge.
        assert!(at_vertical_edge(0, &counts, false));
        assert!(!at_vertical_edge(0, &counts, true));
        // Bottom of the RA column (RA1) is a down-edge; not an up-edge.
        assert!(at_vertical_edge(1, &counts, true));
        assert!(!at_vertical_edge(1, &counts, false));
        // A single-row column (Running) is both edges at once.
        assert!(at_vertical_edge(2, &counts, true));
        assert!(at_vertical_edge(2, &counts, false));
        // Entering from the input: Down -> that column's first row, Up -> last.
        assert_eq!(column_entry(1, &counts, true), 0);
        assert_eq!(column_entry(0, &counts, false), 1);
    }
}
