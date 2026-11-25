use super::DaedalusApp;
use daedalus_ui::bridge::UiCommand;
use daedalus_ui::input::{UiAction, filter_modal_text};

impl DaedalusApp {
    pub(super) fn run_pane_search(&self) {
        let Some(ref search) = self.state.search else {
            return;
        };
        if search.query.is_empty() {
            return;
        }
        self.send_command(UiCommand::SearchPane {
            pane_id: search.pane_id,
            query: search.query.clone(),
            case_sensitive: search.case_sensitive,
            generation: search.generation,
        });
    }

    pub(super) fn scroll_to_active_search_match(&mut self) {
        let Some(ref search) = self.state.search else {
            return;
        };
        let Some(active) = search.active_match() else {
            return;
        };
        let pane_id = search.pane_id;
        {
            let Some(pane) = self.state.panes.get_mut(&pane_id) else {
                return;
            };
            pane.scrollback_len = search.total_scrollback;
            if pane
                .visible_rows()
                .iter()
                .any(|r| r.abs_row == active.abs_row)
            {
                pane.set_scroll_offset(pane.scroll_offset);
            } else {
                let viewport_rows = (pane.summary.rows as usize).max(1);
                let live_start = pane
                    .viewport_rows
                    .last()
                    .and_then(|r| (r.abs_row + 1).checked_sub(viewport_rows as u64))
                    .max(Some(search.total_scrollback as u64));
                let target_offset = search
                    .target_scroll_offset_with_live_start(viewport_rows, live_start)
                    .unwrap_or(0);
                pane.set_scroll_offset(target_offset);
            }
        }
        self.request_scrollback_if_needed(pane_id);
    }

    pub(super) fn handle_search_action(&mut self, action: UiAction) {
        match action {
            UiAction::ToggleSearch => {
                if let Some(pane_id) = self.state.focused_pane_id() {
                    self.state.toggle_search(pane_id);
                    self.request_redraw();
                }
            }
            UiAction::PasteSearchQuery => {
                if let Some(text) = self
                    .clipboard_text()
                    .and_then(|text| filter_modal_text(&text, true))
                {
                    self.handle_ui_action(UiAction::SearchText(text));
                }
            }
            UiAction::SearchText(text) => {
                if let Some(ref mut search) = self.state.search {
                    search.push_text(&text);
                    self.run_pane_search();
                    self.request_redraw();
                }
            }
            UiAction::SearchBackspace => {
                if let Some(ref mut search) = self.state.search
                    && search.pop_char()
                {
                    self.run_pane_search();
                    self.request_redraw();
                }
            }
            UiAction::SearchToggleCase => {
                if let Some(ref mut search) = self.state.search {
                    search.toggle_case();
                    self.run_pane_search();
                    self.request_redraw();
                }
            }
            UiAction::SearchNext => {
                if let Some(ref mut search) = self.state.search {
                    search.step_next();
                }
                self.scroll_to_active_search_match();
                self.request_redraw();
            }
            UiAction::SearchPrev => {
                if let Some(ref mut search) = self.state.search {
                    search.step_prev();
                }
                self.scroll_to_active_search_match();
                self.request_redraw();
            }
            UiAction::SearchCancel => {
                self.state.close_search();
                self.request_redraw();
            }
            _ => unreachable!("action routed to the wrong handler"),
        }
    }
}
