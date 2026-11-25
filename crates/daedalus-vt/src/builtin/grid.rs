use daedalus_proto::types::LinkSpan;
use daedalus_proto::{
    Cell, CellFlags, Color, CursorState, GridDelta, GridRow, GridSnapshot, PaneId, SearchMatch,
};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

pub const MAX_TRACE_BYTES: usize = 64 * 1024;
pub const MAX_SEARCH_MATCHES: usize = 2_000;
// Combining input can arrive indefinitely without advancing the cursor.
const MAX_CELL_TEXT_BYTES: usize = 256;

#[derive(Debug, Clone)]
struct StoredRow {
    links: Vec<LinkSpan>,
    wraps: bool,
    abs_row: u64,
    cells: Vec<Cell>,
}

struct SavedScreen {
    rows: Vec<StoredRow>,
    cursor_col: u16,
    cursor_row: u16,
    wrap_pending: bool,
}

impl StoredRow {
    /// Clear only the halves of wide glyphs that lie outside a replacement range.
    fn clear_overlapping_glyphs(&mut self, start: usize, end: usize) {
        if start == end {
            return;
        }
        let left =
            start.saturating_sub(usize::from(start > 0 && self.cells[start].width == 0)) as u16;
        let right =
            (end + usize::from(end < self.cells.len() && self.cells[end].width == 0)) as u16;
        let mut tail = None;
        self.links.retain_mut(|span| {
            if span.start >= right || span.end <= left {
                return true;
            }
            if span.start < left {
                if span.end > right {
                    tail = Some(LinkSpan {
                        start: right,
                        ..span.clone()
                    });
                }
                span.end = left;
                true
            } else {
                span.start = right;
                span.end > right
            }
        });
        if let Some(tail) = tail
            && self.links.len() < LinkSpan::MAX_PER_ROW
        {
            self.links.push(tail);
        }
        if end == self.cells.len() {
            self.wraps = false;
        }
        if start > 0 && self.cells[start].width == 0 {
            self.cells[start - 1] = Cell::default();
        }
        if end < self.cells.len() && self.cells[end].width == 0 {
            self.cells[end] = Cell::default();
        }
    }
}

pub struct Grid {
    pub(super) terminal_modes: daedalus_proto::TerminalModes,
    last_emitted_modes: daedalus_proto::TerminalModes,
    cols: u16,
    rows: u16,
    scrollback_limit: usize,
    scrollback: VecDeque<StoredRow>,
    viewport: Vec<StoredRow>,
    dirty: Vec<bool>,
    last_emitted_cursor: Option<CursorState>,
    history_generation: u64,
    last_emitted_history_generation: u64,
    cursor_col: u16,
    cursor_row: u16,
    cursor_visible: bool,
    scroll_top: u16,
    scroll_bottom: u16,
    next_abs_row: u64,
    active_url: Option<String>,
    pen_fg: Color,
    pen_bg: Color,
    pen_flags: CellFlags,
    saved_main: Option<SavedScreen>,
}

impl Grid {
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut viewport = Vec::with_capacity(rows as usize);
        for i in 0..rows {
            viewport.push(StoredRow {
                links: Vec::new(),
                wraps: false,
                abs_row: i as u64,
                cells: vec![Cell::default(); cols as usize],
            });
        }
        Self {
            terminal_modes: Default::default(),
            last_emitted_modes: Default::default(),
            cols,
            rows,
            scrollback_limit,
            scrollback: VecDeque::new(),
            viewport,
            dirty: vec![true; rows as usize],
            last_emitted_cursor: None,
            history_generation: 0,
            last_emitted_history_generation: 0,
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            next_abs_row: rows as u64,
            active_url: None,
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

    pub fn set_hyperlink(&mut self, uri: &[u8]) {
        self.active_url = std::str::from_utf8(uri)
            .ok()
            .filter(|uri| {
                uri.len() <= LinkSpan::MAX_URI_BYTES
                    && (uri.starts_with("http://") || uri.starts_with("https://"))
                    && !uri.chars().any(|ch| ch.is_control() || ch.is_whitespace())
            })
            .map(str::to_owned);
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub fn put_char(&mut self, ch: char) {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            if let Some(previous) = self.cursor_col.checked_sub(1) {
                let row = &mut self.viewport[self.cursor_row as usize];
                let mut col = previous as usize;
                if row.cells[col].width == 0 {
                    col = col.saturating_sub(1);
                }
                if row.cells[col].text.len() + ch.len_utf8() <= MAX_CELL_TEXT_BYTES {
                    row.cells[col].text.push(ch);
                    self.dirty[self.cursor_row as usize] = true;
                }
            }
            return;
        }

        // A one-column viewport cannot represent a double-width glyph and its continuation.
        let (ch, width) = if width > self.cols as usize {
            ('�', 1)
        } else {
            (ch, width)
        };
        if self.cursor_col as usize + width > self.cols as usize {
            self.viewport[self.cursor_row as usize].wraps = true;
            self.dirty[self.cursor_row as usize] = true;
            self.cursor_col = 0;
            self.linefeed();
        }
        let r = self.cursor_row as usize;
        let col = self.cursor_col as usize;
        let row = &mut self.viewport[r];
        row.clear_overlapping_glyphs(col, col + width);
        if let Some(url) = self.active_url.as_ref() {
            if let Some(span) = row
                .links
                .iter_mut()
                .find(|span| span.end == col as u16 && span.url == *url)
            {
                span.end += width as u16;
            } else if row.links.len() < LinkSpan::MAX_PER_ROW {
                row.links.push(LinkSpan {
                    start: col as u16,
                    end: (col + width) as u16,
                    url: url.clone(),
                });
            }
        }
        row.cells[col] = Cell {
            text: ch.to_string(),
            fg: self.pen_fg,
            bg: self.pen_bg,
            flags: self.pen_flags,
            width: width as u8,
        };
        if width == 2 {
            row.cells[col + 1] = Cell {
                text: String::new(),
                fg: self.pen_fg,
                bg: self.pen_bg,
                flags: self.pen_flags,
                width: 0,
            };
        }
        self.dirty[r] = true;
        self.cursor_col += width as u16;
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
                links: Vec::new(),
                wraps: false,
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
        // A pending wrap may put the write position one past the visible cursor.
        let col = usize::from(self.cursor().col);
        let cols = self.cols as usize;
        let (start, end) = match mode {
            0 => (col, cols),
            1 => (0, (col + 1).min(cols)),
            2 => (0, cols),
            _ => return,
        };
        let row = &mut self.viewport[r];
        row.clear_overlapping_glyphs(start, end);
        row.cells[start..end].fill(Cell::default());
        self.dirty[r] = true;
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        let r = self.cursor_row as usize;
        match mode {
            0 => {
                self.erase_in_line(0);
                for row_idx in (r + 1)..self.viewport.len() {
                    self.viewport[row_idx].links.clear();
                    self.viewport[row_idx].wraps = false;
                    for cell in &mut self.viewport[row_idx].cells {
                        *cell = Cell::default();
                    }
                    self.dirty[row_idx] = true;
                }
            }
            1 => {
                self.erase_in_line(1);
                for row_idx in 0..r {
                    self.viewport[row_idx].links.clear();
                    self.viewport[row_idx].wraps = false;
                    for cell in &mut self.viewport[row_idx].cells {
                        *cell = Cell::default();
                    }
                    self.dirty[row_idx] = true;
                }
            }
            3 => {
                if !self.scrollback.is_empty() {
                    self.scrollback.clear();
                    self.history_generation += 1;
                }
            }
            2 => {
                for row_idx in 0..self.viewport.len() {
                    self.viewport[row_idx].links.clear();
                    self.viewport[row_idx].wraps = false;
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
                    links: Vec::new(),
                    wraps: false,
                    abs_row: i as u64,
                    cells: vec![Cell::default(); self.cols as usize],
                });
            }
            let main = std::mem::replace(&mut self.viewport, alt);
            self.saved_main = Some(SavedScreen {
                rows: main,
                cursor_col: self.cursor_col,
                cursor_row: self.cursor_row,
                wrap_pending: self.cursor_col == self.cols,
            });
            self.dirty.fill(true);
            self.move_cursor(0, 0);
        }
    }

    pub fn leave_alt_screen(&mut self) {
        if let Some(mut main) = self.saved_main.take() {
            Self::conform_rows(
                &mut main.rows,
                self.cols,
                self.rows,
                &mut self.next_abs_row,
                &mut self.scrollback,
                self.scrollback_limit,
                true,
            );
            self.viewport = main.rows;
            self.cursor_row = main.cursor_row.min(self.rows - 1);
            self.cursor_col = if main.wrap_pending && main.cursor_col == self.cols {
                self.cols
            } else {
                main.cursor_col.min(self.cols - 1)
            };
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
            if row.cells.len() != cols as usize {
                row.wraps = false;
            }
            row.links.retain_mut(|span| {
                span.end = span.end.min(cols);
                span.start < span.end
            });
            row.cells.resize(cols as usize, Cell::default());
            if let Some(last) = row.cells.last_mut()
                && last.width == 2
            {
                *last = Cell::default();
            }
        }
        while buf.len() < rows as usize {
            buf.push(StoredRow {
                links: Vec::new(),
                wraps: false,
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
                wraps: row.wraps,
                links: row.links.clone(),
                row_index: i as u16,
                abs_row: row.abs_row,
                cells: row.cells.clone(),
            })
            .collect();
        GridSnapshot {
            terminal_modes: self.terminal_modes,
            activity: None,
            insight: None,
            pane_id,
            seq,
            cols: self.cols,
            rows: self.rows,
            cursor: self.cursor(),
            rows_data,
            scrollback_len: self.scrollback.len(),
            history_generation: self.history_generation,
        }
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Returns up to `count` rows ordered top-to-bottom from the combined
    /// `[scrollback..., viewport...]` history ending `offset_from_bottom` rows
    /// above the bottom of the live viewport.
    pub fn scrollback_window(&self, offset_from_bottom: usize, count: usize) -> Vec<GridRow> {
        if count == 0 {
            return Vec::new();
        }
        let total = self.scrollback.len() + self.viewport.len();
        let clamped_offset = offset_from_bottom.min(self.scrollback.len());
        let end = total.saturating_sub(clamped_offset);
        let start = end.saturating_sub(count);
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        for (idx, row) in self
            .scrollback
            .iter()
            .chain(self.viewport.iter())
            .enumerate()
            .skip(start)
            .take(end - start)
        {
            out.push(GridRow {
                wraps: row.wraps,
                links: row.links.clone(),
                row_index: (idx - start) as u16,
                abs_row: row.abs_row,
                cells: row.cells.clone(),
            });
        }
        out
    }

    pub fn take_dirty_delta(&mut self, pane_id: PaneId, seq: u64) -> Option<GridDelta> {
        let mut dirty_rows = Vec::new();
        for (i, is_dirty) in self.dirty.iter_mut().enumerate() {
            if *is_dirty {
                *is_dirty = false;
                dirty_rows.push(GridRow {
                    wraps: self.viewport[i].wraps,
                    links: self.viewport[i].links.clone(),
                    row_index: i as u16,
                    abs_row: self.viewport[i].abs_row,
                    cells: self.viewport[i].cells.clone(),
                });
            }
        }
        let cursor = self.cursor();
        if dirty_rows.is_empty()
            && self.last_emitted_cursor == Some(cursor)
            && self.last_emitted_history_generation == self.history_generation
            && self.last_emitted_modes == self.terminal_modes
        {
            return None;
        }
        self.last_emitted_cursor = Some(cursor);
        self.last_emitted_modes = self.terminal_modes;
        self.last_emitted_history_generation = self.history_generation;
        Some(GridDelta {
            terminal_modes: self.terminal_modes,
            activity: None,
            insight: None,
            pane_id,
            seq,
            cols: self.cols,
            rows: self.rows,
            cursor,
            dirty_rows,
            history_generation: self.history_generation,
            scrollback_len: Some(self.scrollback.len()),
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

    /// Searches scrollback and viewport rows top-to-bottom, returning up to `MAX_SEARCH_MATCHES` matches.
    pub fn search(&self, query: &str, case_sensitive: bool) -> Vec<SearchMatch> {
        search_rows(
            query,
            case_sensitive,
            self.scrollback
                .iter()
                .chain(&self.viewport)
                .map(|row| (row.abs_row, &row.cells)),
        )
    }
}

pub(crate) fn search_rows<C: AsRef<[Cell]>>(
    query: &str,
    case_sensitive: bool,
    rows: impl IntoIterator<Item = (u64, C)>,
) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }

    let needle_owned;
    let needle = if case_sensitive {
        query
    } else {
        needle_owned = query.to_lowercase();
        &needle_owned
    };
    if needle.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut haystack = String::new();
    let mut byte_to_col: Vec<u16> = Vec::new();

    for (abs_row, cells) in rows {
        let cells = cells.as_ref();
        haystack.clear();
        byte_to_col.clear();

        for (col, cell) in cells.iter().enumerate() {
            let col_u16 = col as u16;
            let start = haystack.len();
            if case_sensitive {
                haystack.push_str(&cell.text);
            } else {
                for ch in cell.text.chars() {
                    for lc in ch.to_lowercase() {
                        haystack.push(lc);
                    }
                }
            }
            let added = haystack.len() - start;
            byte_to_col.resize(byte_to_col.len() + added, col_u16);
        }

        for (start_byte, matched) in haystack.match_indices(needle) {
            let end_byte = start_byte + matched.len();
            let start_col = byte_to_col[start_byte];
            let last_col = byte_to_col[end_byte - 1];
            let end_col = cells
                .get(last_col as usize)
                .map(|c| last_col.saturating_add(c.width.max(1) as u16))
                .unwrap_or_else(|| last_col.saturating_add(1));
            matches.push(SearchMatch {
                abs_row,
                start_col,
                end_col,
            });
            if matches.len() >= MAX_SEARCH_MATCHES {
                return matches;
            }
        }
    }

    matches
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
