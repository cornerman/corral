//! Selection navigation over the board's columns. Pure index math on the
//! per-column counts (`Board::column_counts`, in `Column::ALL` order): the
//! selection is one flat index across all columns. Down/Up flow across that
//! flat index (crossing column boundaries), Left/Right jump between columns,
//! and mouse scroll stays within one column.

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

/// Total selectable cards across all columns.
fn total(counts: &[usize; 4]) -> usize {
    counts.iter().sum()
}

/// Move one card down/up across the WHOLE board, flowing from a column's last
/// card into the next column's first (columns in `Column::ALL` order; empty
/// columns add no indices, so they are skipped for free). Clamped at the board
/// ends — the shell rings to the filter input there via `at_board_edge`.
pub fn move_selection(index: usize, counts: &[usize; 4], down: bool) -> usize {
    let t = total(counts);
    if t == 0 {
        return index;
    }
    if down {
        (index + 1).min(t - 1)
    } else {
        index.saturating_sub(1)
    }
}

/// Move within the current column (mouse scroll), clamped to that column.
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

/// True if a vertical move (`down`) from `index` would leave the board entirely
/// — it already sits at the very last card (down) or the very first card (up).
/// The single hook a shell uses to hand focus back to the filter input: the
/// input is one ring node above the first card of the whole board and below the
/// last, so navigation rings through it only at the two board ends. Empty board
/// is always an edge.
pub fn at_board_edge(index: usize, counts: &[usize; 4], down: bool) -> bool {
    let t = total(counts);
    if t == 0 {
        return true;
    }
    if down {
        index + 1 >= t
    } else {
        index == 0
    }
}

/// Landing index when entering the board from the filter input: the first card
/// of the whole board (`down`) or the last card (up). The inverse of
/// `at_board_edge` — the input is the single ring node of the vertical cycle
/// (input -> card0 -> ... -> cardN -> input), so stepping off it lands at the
/// matching board end. Empty board leaves `index` unchanged.
pub fn board_entry(index: usize, counts: &[usize; 4], down: bool) -> usize {
    let t = total(counts);
    if t == 0 {
        return index;
    }
    if down {
        0
    } else {
        t - 1
    }
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
        // Down within the column (mouse scroll), clamped.
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
    fn down_up_flow_across_columns_and_ring_at_board_ends() {
        // RA=2, Idle=0, Running=1, Dormant=0. flat: RA0=0, RA1=1, Run0=2.
        let counts = [2usize, 0, 1, 0];
        // Down flows from the RA column's last card into Running (crossing the
        // empty Idle column), not back to the input.
        assert_eq!(move_selection(1, &counts, true), 2);
        assert_eq!(move_selection(0, &counts, true), 1);
        // Up flows back the same way.
        assert_eq!(move_selection(2, &counts, false), 1);
        // Clamped at the board ends.
        assert_eq!(move_selection(2, &counts, true), 2);
        assert_eq!(move_selection(0, &counts, false), 0);
        // Only the very first/last card of the whole board is a ring edge.
        assert!(at_board_edge(0, &counts, false));
        assert!(!at_board_edge(0, &counts, true));
        assert!(at_board_edge(2, &counts, true));
        assert!(!at_board_edge(2, &counts, false));
        assert!(!at_board_edge(1, &counts, true));
        assert!(!at_board_edge(1, &counts, false));
        // Entering from the input rings to the matching board end: Down -> the
        // first card, Up -> the last.
        assert_eq!(board_entry(1, &counts, true), 0);
        assert_eq!(board_entry(0, &counts, false), 2);
    }
}
