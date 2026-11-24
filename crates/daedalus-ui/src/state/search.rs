use super::remove_last_grapheme;
use daedalus_proto::{PaneId, SearchMatch};
use std::sync::atomic::{AtomicU64, Ordering};

/// State of the per-pane scrollback search overlay (`Ctrl+Shift+F`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub pane_id: PaneId,
    pub query: String,
    pub case_sensitive: bool,
    pub generation: u64,
    pub total_scrollback: usize,
    pub matches: Vec<SearchMatch>,
    pub current_index: usize,
    results_received: bool,
}

static NEXT_SEARCH_GENERATION: AtomicU64 = AtomicU64::new(1);

impl SearchState {
    fn next_generation() -> u64 {
        NEXT_SEARCH_GENERATION.fetch_add(1, Ordering::Relaxed)
    }

    pub fn new(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            query: String::new(),
            case_sensitive: false,
            generation: Self::next_generation(),
            total_scrollback: 0,
            matches: Vec::new(),
            current_index: 0,
            results_received: false,
        }
    }

    pub(super) fn invalidate_results(&mut self) {
        self.generation = Self::next_generation();
        self.matches.clear();
        self.current_index = 0;
        self.results_received = false;
    }

    pub fn toggle_case(&mut self) {
        self.case_sensitive = !self.case_sensitive;
        self.invalidate_results();
    }

    pub fn result_label(&self) -> Option<String> {
        if self.query.is_empty() || !self.results_received {
            return None;
        }
        if self.matches.is_empty() {
            return Some("No matches".into());
        }
        let (current, total) = self.display_counter();
        Some(format!("({current}/{total})"))
    }

    pub fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.query.push_str(text);
            self.invalidate_results();
        }
    }

    pub fn pop_char(&mut self) -> bool {
        let popped = remove_last_grapheme(&mut self.query);
        if popped {
            self.invalidate_results();
        }
        popped
    }

    pub fn set_results(&mut self, total_scrollback: usize, matches: Vec<SearchMatch>) {
        self.total_scrollback = total_scrollback;
        self.matches = matches;
        self.results_received = true;
        self.current_index = 0;
    }

    /// Applies search results only if `generation` matches the current query generation
    /// and the query is non-empty, ignoring stale out-of-order responses.
    pub fn apply_results(
        &mut self,
        generation: u64,
        total_scrollback: usize,
        matches: Vec<SearchMatch>,
    ) -> bool {
        if self.generation != generation || self.query.is_empty() {
            return false;
        }
        self.set_results(total_scrollback, matches);
        true
    }

    pub fn active_match(&self) -> Option<SearchMatch> {
        self.matches.get(self.current_index).copied()
    }

    pub fn step_next(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.current_index = 0;
            return None;
        }
        self.current_index = (self.current_index + 1) % self.matches.len();
        self.active_match()
    }

    pub fn step_prev(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            self.current_index = 0;
            return None;
        }
        self.current_index = if self.current_index == 0 {
            self.matches.len() - 1
        } else {
            self.current_index - 1
        };
        self.active_match()
    }

    pub fn display_counter(&self) -> (usize, usize) {
        if self.matches.is_empty() {
            (0, 0)
        } else {
            (self.current_index + 1, self.matches.len())
        }
    }

    /// Computes the desired `scrollback_offset` to center/reveal the active [`SearchMatch`]
    /// in a viewport of `viewport_rows` rows, where `abs_row` spans
    /// `0..total_scrollback + viewport_rows` and the live viewport starts at `total_scrollback`.
    pub fn target_scroll_offset(&self, viewport_rows: usize) -> Option<usize> {
        self.target_scroll_offset_with_live_start(viewport_rows, None)
    }

    /// Computes the desired `scrollback_offset` using an optional explicit `live_start_abs_row`
    /// (supporting panes where oldest scrollback rows have been evicted past the history cap).
    pub fn target_scroll_offset_with_live_start(
        &self,
        viewport_rows: usize,
        live_start_abs_row: Option<u64>,
    ) -> Option<usize> {
        let active = self.active_match()?;
        let vp = viewport_rows.max(1);
        let live_start = live_start_abs_row.unwrap_or(self.total_scrollback as u64);
        if active.abs_row >= live_start {
            return Some(0);
        }
        let rows_above_live = (live_start - active.abs_row) as usize;
        let offset = rows_above_live
            .saturating_add(vp / 2)
            .min(self.total_scrollback);
        Some(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_reports_no_matches_only_after_the_current_reply() {
        let mut search = SearchState::new(PaneId::new());
        assert_eq!(search.result_label(), None);
        search.push_text("old");
        let previous = search.generation;
        search.push_text("er");
        assert!(!search.apply_results(previous, 0, vec![]));
        assert_eq!(search.result_label(), None);
        assert!(search.apply_results(search.generation, 0, vec![]));
        assert_eq!(search.result_label().as_deref(), Some("No matches"));
        search.pop_char();
        assert_eq!(search.result_label(), None);
        search.set_results(
            0,
            vec![SearchMatch {
                abs_row: 0,
                start_col: 1,
                end_col: 2,
            }],
        );
        assert_eq!(search.result_label().as_deref(), Some("(1/1)"));
        let previous = search.generation;
        search.toggle_case();
        assert!(search.case_sensitive);
        assert_eq!(search.query, "olde");
        assert_eq!(search.active_match(), None);
        assert_eq!(search.result_label(), None);
        assert!(!search.apply_results(previous, 0, vec![]));
        search.toggle_case();
        assert!(!search.case_sensitive);
    }

    #[test]
    fn search_state_steps_with_wrap_and_computes_scrollback_offset() {
        let pane_id = PaneId::new();
        let mut search = SearchState::new(pane_id);
        assert_eq!(search.display_counter(), (0, 0));
        assert_eq!(search.step_next(), None);
        assert_eq!(search.step_prev(), None);
        assert_eq!(search.target_scroll_offset(20), None);

        search.push_text("err");
        search.push_text("or");
        assert_eq!(search.query, "error");
        assert!(search.pop_char());
        assert_eq!(search.query, "erro");

        // Suppose total_scrollback = 100, viewport_rows = 20 -> live viewport is abs_row 100..120
        let matches = vec![
            SearchMatch {
                abs_row: 5,
                start_col: 0,
                end_col: 4,
            },
            SearchMatch {
                abs_row: 50,
                start_col: 2,
                end_col: 6,
            },
            SearchMatch {
                abs_row: 112,
                start_col: 1,
                end_col: 5,
            },
        ];
        search.set_results(100, matches.clone());
        assert_eq!(search.display_counter(), (1, 3));
        assert_eq!(search.active_match(), Some(matches[0]));
        // abs_row=5 near top clamps to total_scrollback=100 (window 0..20)
        assert_eq!(search.target_scroll_offset(20), Some(100));

        // Step next -> abs_row=50 centered in 20-row window: offset = (100 - 50) + 10 = 60 (window 40..60)
        assert_eq!(search.step_next(), Some(matches[1]));
        assert_eq!(search.display_counter(), (2, 3));
        assert_eq!(search.target_scroll_offset(20), Some(60));

        // Step next -> abs_row=112 in live viewport (100..120): offset = 0
        assert_eq!(search.step_next(), Some(matches[2]));
        assert_eq!(search.display_counter(), (3, 3));
        assert_eq!(search.target_scroll_offset(20), Some(0));

        // Wrap forward -> index 0; wrap backward -> index 2
        assert_eq!(search.step_next(), Some(matches[0]));
        assert_eq!(search.current_index, 0);
        assert_eq!(search.step_prev(), Some(matches[2]));
        assert_eq!(search.current_index, 2);
        assert_eq!(search.step_prev(), Some(matches[1]));
        assert_eq!(search.current_index, 1);

        // Editing query clears stale matches and resets index
        search.push_text("r");
        assert!(search.matches.is_empty());
        assert_eq!(search.display_counter(), (0, 0));
    }
}
