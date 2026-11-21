use daedalus_proto::{
    Cell, CellFlags, Color, CursorState, GridDelta, GridRow, GridSnapshot, PaneId,
};
use std::collections::VecDeque;

pub const MAX_TRACE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct StoredRow {
    abs_row: u64,
    cells: Vec<Cell>,
}

pub struct Grid {
    cols: u16,
    rows: u16,
    scrollback_limit: usize,
    scrollback: VecDeque<StoredRow>,
    viewport: Vec<StoredRow>,
    dirty: Vec<bool>,
    cursor_col: u16,
    cursor_row: u16,
    cursor_visible: bool,
    scroll_top: u16,
    scroll_bottom: u16,
    next_abs_row: u64,
    pen_fg: Color,
    pen_bg: Color,
    pen_flags: CellFlags,
    saved_main: Option<Vec<StoredRow>>,
}

impl Grid {
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut viewport = Vec::with_capacity(rows as usize);
        for i in 0..rows {
            viewport.push(StoredRow {
                abs_row: i as u64,
                cells: vec![Cell::default(); cols as usize],
            });
        }
        Self {
            cols,
            rows,
            scrollback_limit,
            scrollback: VecDeque::new(),
            viewport,
            dirty: vec![true; rows as usize],
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            next_abs_row: rows as u64,
            pen_fg: Color::Default,
            pen_bg: Color::Default,
            pen_flags: CellFlags::empty(),
            saved_main: None,
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cursor(&self) -> CursorState {
        CursorState {
            col: self.cursor_col.min(self.cols.saturating_sub(1)),
            row: self.cursor_row.min(self.rows.saturating_sub(1)),
            visible: self.cursor_visible,
        }
    }

    pub fn current_abs_row(&self) -> u64 {
        let idx = (self.cursor_row as usize).min(self.viewport.len().saturating_sub(1));
        self.viewport[idx].abs_row
    }

    pub fn set_pen_fg(&mut self, fg: Color) {
        self.pen_fg = fg;
    }

    pub fn set_pen_bg(&mut self, bg: Color) {
        self.pen_bg = bg;
    }

    pub fn pen_flags_mut(&mut self) -> &mut CellFlags {
        &mut self.pen_flags
    }

    pub fn reset_pen(&mut self) {
        self.pen_fg = Color::Default;
        self.pen_bg = Color::Default;
        self.pen_flags = CellFlags::empty();
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.linefeed();
        }
        let r = self.cursor_row as usize;
        let c = self.cursor_col as usize;
        if r < self.viewport.len() && c < self.viewport[r].cells.len() {
            self.viewport[r].cells[c] = Cell {
                text: ch.to_string(),
                fg: self.pen_fg,
                bg: self.pen_bg,
                flags: self.pen_flags,
                width: 1,
            };
            self.dirty[r] = true;
        }
        self.cursor_col = self.cursor_col.saturating_add(1);
    }

    pub fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub fn tab(&mut self) {
        let next = ((self.cursor_col / 8) + 1) * 8;
        self.cursor_col = next.min(self.cols.saturating_sub(1));
    }

    pub fn linefeed(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
    }

    pub fn scroll_up(&mut self, count: u16) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        if top > bottom || bottom >= self.viewport.len() {
            return;
        }
        for _ in 0..count {
            let removed = self.viewport.remove(top);
            if top == 0 && self.saved_main.is_none() && self.scrollback_limit > 0 {
                if self.scrollback.len() >= self.scrollback_limit {
                    self.scrollback.pop_front();
                }
                self.scrollback.push_back(removed);
            }
            let new_row = StoredRow {
                abs_row: self.next_abs_row,
                cells: vec![Cell::default(); self.cols as usize],
            };
            self.next_abs_row += 1;
            self.viewport.insert(bottom, new_row);
        }
        for d in &mut self.dirty[top..=bottom] {
            *d = true;
        }
    }

    pub fn move_cursor(&mut self, col: u16, row: u16) {
        self.cursor_col = col.min(self.cols.saturating_sub(1));
        self.cursor_row = row.min(self.rows.saturating_sub(1));
    }

    pub fn move_up(&mut self, n: u16) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub fn move_down(&mut self, n: u16) {
        self.cursor_row = (self.cursor_row.saturating_add(n)).min(self.rows.saturating_sub(1));
    }

    pub fn move_forward(&mut self, n: u16) {
        self.cursor_col = (self.cursor_col.saturating_add(n)).min(self.cols.saturating_sub(1));
    }

    pub fn move_backward(&mut self, n: u16) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1);
        }
        self.move_cursor(0, 0);
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        let r = self.cursor_row as usize;
        if r >= self.viewport.len() {
            return;
        }
        let col = (self.cursor_col as usize).min(self.cols as usize);
        match mode {
            0 => {
                for cell in &mut self.viewport[r].cells[col..] {
                    *cell = Cell::default();
                }
            }
            1 => {
                let end = (col + 1).min(self.cols as usize);
                for cell in &mut self.viewport[r].cells[..end] {
                    *cell = Cell::default();
                }
            }
            2 => {
                for cell in &mut self.viewport[r].cells {
                    *cell = Cell::default();
                }
            }
            _ => {}
        }
        self.dirty[r] = true;
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        let r = self.cursor_row as usize;
        match mode {
            0 => {
                self.erase_in_line(0);
                for row_idx in (r + 1)..self.viewport.len() {
                    for cell in &mut self.viewport[row_idx].cells {
                        *cell = Cell::default();
                    }
                    self.dirty[row_idx] = true;
                }
            }
            1 => {
                self.erase_in_line(1);
                for row_idx in 0..r {
                    for cell in &mut self.viewport[row_idx].cells {
                        *cell = Cell::default();
                    }
                    self.dirty[row_idx] = true;
                }
            }
            2 | 3 => {
                for row_idx in 0..self.viewport.len() {
                    for cell in &mut self.viewport[row_idx].cells {
                        *cell = Cell::default();
                    }
                    self.dirty[row_idx] = true;
                }
            }
            _ => {}
        }
    }

    pub fn enter_alt_screen(&mut self) {
        if self.saved_main.is_none() {
            let mut alt = Vec::with_capacity(self.rows as usize);
            for i in 0..self.rows {
                alt.push(StoredRow {
                    abs_row: i as u64,
                    cells: vec![Cell::default(); self.cols as usize],
                });
            }
            let main = std::mem::replace(&mut self.viewport, alt);
            self.saved_main = Some(main);
            self.dirty.fill(true);
            self.move_cursor(0, 0);
        }
    }

    pub fn leave_alt_screen(&mut self) {
        if let Some(mut main) = self.saved_main.take() {
            Self::conform_rows(
                &mut main,
                self.cols,
                self.rows,
                &mut self.next_abs_row,
                &mut self.scrollback,
                self.scrollback_limit,
                true,
            );
            self.viewport = main;
            self.dirty.fill(true);
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let push_to_scrollback = self.saved_main.is_none();
        Self::conform_rows(
            &mut self.viewport,
            cols,
            rows,
            &mut self.next_abs_row,
            &mut self.scrollback,
            self.scrollback_limit,
            push_to_scrollback,
        );
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.dirty = vec![true; rows as usize];
    }

    /// Resize a row buffer to `(cols, rows)`, optionally pushing evicted top rows to scrollback.
    fn conform_rows(
        buf: &mut Vec<StoredRow>,
        cols: u16,
        rows: u16,
        next_abs_row: &mut u64,
        scrollback: &mut VecDeque<StoredRow>,
        scrollback_limit: usize,
        push_to_scrollback: bool,
    ) {
        for row in buf.iter_mut() {
            row.cells.resize(cols as usize, Cell::default());
        }
        while buf.len() < rows as usize {
            buf.push(StoredRow {
                abs_row: *next_abs_row,
                cells: vec![Cell::default(); cols as usize],
            });
            *next_abs_row += 1;
        }
        while buf.len() > rows as usize {
            let removed = buf.remove(0);
            if push_to_scrollback && scrollback_limit > 0 {
                if scrollback.len() >= scrollback_limit {
                    scrollback.pop_front();
                }
                scrollback.push_back(removed);
            }
        }
    }

    pub fn snapshot(&self, pane_id: PaneId, seq: u64) -> GridSnapshot {
        let rows_data = self
            .viewport
            .iter()
            .enumerate()
            .map(|(i, row)| GridRow {
                row_index: i as u16,
                abs_row: row.abs_row,
                cells: row.cells.clone(),
            })
            .collect();
        GridSnapshot {
            pane_id,
            seq,
            cols: self.cols,
            rows: self.rows,
            cursor: self.cursor(),
            rows_data,
        }
    }

    pub fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta> {
        let mut dirty_rows = Vec::new();
        for (i, is_dirty) in self.dirty.iter_mut().enumerate() {
            if *is_dirty {
                *is_dirty = false;
                dirty_rows.push(GridRow {
                    row_index: i as u16,
                    abs_row: self.viewport[i].abs_row,
                    cells: self.viewport[i].cells.clone(),
                });
            }
        }
        if dirty_rows.is_empty() {
            return None;
        }
        Some(GridDelta {
            pane_id,
            seq,
            cols: self.cols,
            rows: self.rows,
            cursor: self.cursor(),
            dirty_rows,
        })
    }

    pub fn slice_plain_text(&self, start_abs_row: u64, end_abs_row: u64) -> String {
        if end_abs_row < start_abs_row {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        for row in self.scrollback.iter().chain(self.viewport.iter()) {
            if row.abs_row >= start_abs_row && row.abs_row <= end_abs_row {
                let raw: String = row.cells.iter().map(|c| c.text.as_str()).collect();
                lines.push(raw.trim_end().to_string());
            }
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let joined = lines.join("\n");
        truncate_utf8_tail(joined, MAX_TRACE_BYTES)
    }

    pub fn row_text_range(&self, abs_row: u64, start_col: u16, end_col: Option<u16>) -> String {
        for row in self.scrollback.iter().chain(self.viewport.iter()) {
            if row.abs_row == abs_row {
                let start = (start_col as usize).min(row.cells.len());
                let end = end_col
                    .map(|c| (c as usize).min(row.cells.len()))
                    .unwrap_or(row.cells.len());
                if end <= start {
                    return String::new();
                }
                let raw: String = row.cells[start..end]
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect();
                return raw.trim_end().to_string();
            }
        }
        String::new()
    }
}

pub fn truncate_utf8_tail(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}
