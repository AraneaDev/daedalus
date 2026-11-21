use std::cmp::Ordering;

use daedalus_proto::{Cell, GridRow};

/// Logical cell position in a terminal pane's coordinate space.
///
/// `abs_row` is the monotonic absolute row index (including scrollback history),
/// and `col` is the 0-based column index within that row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPos {
    pub col: u16,
    pub abs_row: u64,
}

impl CellPos {
    pub const fn new(col: u16, abs_row: u64) -> Self {
        Self { col, abs_row }
    }
}

impl Ord for CellPos {
    fn cmp(&self, other: &Self) -> Ordering {
        self.abs_row
            .cmp(&other.abs_row)
            .then_with(|| self.col.cmp(&other.col))
    }
}

impl PartialOrd for CellPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Inclusive cell-range selection anchored at `anchor` and extended to `head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub head: CellPos,
}

impl Selection {
    pub const fn new(anchor: CellPos, head: CellPos) -> Self {
        Self { anchor, head }
    }

    /// Returns `(start, end)` ordered in reading order (`(abs_row, col)`).
    pub fn normalized(&self) -> (CellPos, CellPos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Returns `true` if `(col, abs_row)` lies within this inclusive selection.
    pub fn contains(&self, col: u16, abs_row: u64) -> bool {
        let (start, end) = self.normalized();
        if abs_row < start.abs_row || abs_row > end.abs_row {
            return false;
        }
        if start.abs_row == end.abs_row {
            col >= start.col && col <= end.col
        } else if abs_row == start.abs_row {
            col >= start.col
        } else if abs_row == end.abs_row {
            col <= end.col
        } else {
            true
        }
    }
}

/// Normalizes a `Selection` into `(start, end)` ordered by `(abs_row, col)`.
pub fn normalize_selection(selection: &Selection) -> (CellPos, CellPos) {
    selection.normalized()
}

/// Extracts plain text covered by `selection` from `rows`, trimming trailing
/// whitespace on each line and joining multiple lines with `'\n'`.
pub fn extract_selected_text(rows: &[GridRow], selection: &Selection) -> String {
    let (start, end) = selection.normalized();

    let mut matching: Vec<&GridRow> = rows
        .iter()
        .filter(|row| row.abs_row >= start.abs_row && row.abs_row <= end.abs_row)
        .collect();
    matching.sort_by_key(|row| row.abs_row);

    let mut lines = Vec::with_capacity(matching.len());
    for row in matching {
        if row.cells.is_empty() {
            lines.push(String::new());
            continue;
        }

        let col_start = if row.abs_row == start.abs_row {
            start.col as usize
        } else {
            0
        };
        let col_end = if row.abs_row == end.abs_row {
            (end.col as usize).min(row.cells.len().saturating_sub(1))
        } else {
            row.cells.len().saturating_sub(1)
        };

        if col_start > col_end || col_start >= row.cells.len() {
            lines.push(String::new());
            continue;
        }

        let mut line = String::new();
        for cell in &row.cells[col_start..=col_end] {
            if cell.width == 0 {
                continue;
            }
            if cell.text.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&cell.text);
            }
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

fn is_whitespace_cell(cell: &Cell) -> bool {
    cell.text.is_empty() || cell.text.chars().all(char::is_whitespace)
}

/// Selects the contiguous non-whitespace word containing `col` on `row`.
/// Returns `None` if `col` is out of bounds or points to whitespace.
pub fn select_word_at(row: &GridRow, col: u16) -> Option<Selection> {
    let len = row.cells.len();
    let mut idx = col as usize;
    if idx >= len {
        return None;
    }

    // If the user clicked the trailing continuation cell of a wide character,
    // step back to its leading cell.
    if row.cells[idx].width == 0 && idx > 0 {
        idx -= 1;
    }

    if is_whitespace_cell(&row.cells[idx]) {
        return None;
    }

    let mut start = idx;
    while start > 0 {
        let prev = &row.cells[start - 1];
        if prev.width == 0 {
            if start >= 2 && !is_whitespace_cell(&row.cells[start - 2]) {
                start -= 2;
            } else {
                break;
            }
        } else if !is_whitespace_cell(prev) {
            start -= 1;
        } else {
            break;
        }
    }

    let mut end = idx;
    while end + 1 < len {
        let next = &row.cells[end + 1];
        if next.width == 0 || !is_whitespace_cell(next) {
            end += 1;
        } else {
            break;
        }
    }

    Some(Selection::new(
        CellPos::new(start as u16, row.abs_row),
        CellPos::new(end as u16, row.abs_row),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::{CellFlags, Color};

    fn make_row(row_index: u16, abs_row: u64, text: &str, total_cols: usize) -> GridRow {
        let mut cells: Vec<Cell> = text
            .chars()
            .map(|ch| Cell {
                text: ch.to_string(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 1,
            })
            .collect();
        while cells.len() < total_cols {
            cells.push(Cell::default());
        }
        GridRow {
            row_index,
            abs_row,
            cells,
        }
    }

    #[test]
    fn forward_drag_normalizes_and_contains_single_line() {
        let sel = Selection::new(CellPos::new(2, 10), CellPos::new(5, 10));
        assert_eq!(
            normalize_selection(&sel),
            (CellPos::new(2, 10), CellPos::new(5, 10))
        );
        assert!(!sel.contains(1, 10));
        assert!(sel.contains(2, 10));
        assert!(sel.contains(4, 10));
        assert!(sel.contains(5, 10));
        assert!(!sel.contains(6, 10));
        assert!(!sel.contains(3, 9));
        assert!(!sel.contains(3, 11));
    }

    #[test]
    fn reverse_drag_normalizes_across_multiple_lines() {
        let sel = Selection::new(CellPos::new(3, 12), CellPos::new(6, 10));
        assert_eq!(
            sel.normalized(),
            (CellPos::new(6, 10), CellPos::new(3, 12))
        );

        // Start row: col >= 6
        assert!(!sel.contains(5, 10));
        assert!(sel.contains(6, 10));
        assert!(sel.contains(79, 10));

        // Middle row: all columns
        assert!(sel.contains(0, 11));
        assert!(sel.contains(79, 11));

        // End row: col <= 3
        assert!(sel.contains(0, 12));
        assert!(sel.contains(3, 12));
        assert!(!sel.contains(4, 12));
    }

    #[test]
    fn extract_selected_text_handles_single_and_multiline_with_trailing_trim() {
        let rows = vec![
            make_row(0, 100, "hello world   ", 16),
            make_row(1, 101, "  middle line  ", 16),
            make_row(2, 102, "final row!   ", 16),
        ];

        // Single-line forward selection
        let single = Selection::new(CellPos::new(6, 100), CellPos::new(15, 100));
        assert_eq!(extract_selected_text(&rows, &single), "world");

        // Multi-line reverse selection
        let multi = Selection::new(CellPos::new(4, 102), CellPos::new(6, 100));
        assert_eq!(
            extract_selected_text(&rows, &multi),
            "world\n  middle line\nfinal"
        );
    }

    #[test]
    fn select_word_at_finds_contiguous_non_whitespace_bounds() {
        let row = make_row(0, 42, "  cargo test --workspace  ", 28);

        // Clicking inside "cargo" (cols 2..=6)
        let sel = select_word_at(&row, 4).expect("should select cargo");
        assert_eq!(sel, Selection::new(CellPos::new(2, 42), CellPos::new(6, 42)));
        assert_eq!(extract_selected_text(std::slice::from_ref(&row), &sel), "cargo");

        // Clicking inside "--workspace" (cols 13..=23)
        let flag_sel = select_word_at(&row, 18).expect("should select --workspace");
        assert_eq!(
            flag_sel,
            Selection::new(CellPos::new(13, 42), CellPos::new(23, 42))
        );
        assert_eq!(
            extract_selected_text(std::slice::from_ref(&row), &flag_sel),
            "--workspace"
        );

        // Clicking on whitespace or out-of-bounds returns None
        assert_eq!(select_word_at(&row, 0), None);
        assert_eq!(select_word_at(&row, 7), None);
        assert_eq!(select_word_at(&row, 99), None);
    }
}
