use super::DaedalusApp;
use daedalus_proto::PaneRole;
use daedalus_ui::{
    bridge::UiCommand,
    links::{browser_link_at, detect_link_in_row, open_link},
    selection::extract_selected_text,
};
use tracing::warn;

impl DaedalusApp {
    pub(super) fn ensure_clipboard(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            let cb =
                arboard::Clipboard::new().map_err(|err| format!("Clipboard unavailable: {err}"))?;
            self.clipboard = Some(cb);
        }
        Ok(self.clipboard.as_mut().expect("clipboard initialized"))
    }

    pub(super) fn preview_image(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        if self
            .state
            .image_preview
            .as_ref()
            .is_some_and(|(id, _)| *id == pane_id)
        {
            self.state.image_preview = None;
            self.request_redraw();
            return;
        }
        let Some(pane) = self.state.panes.get(&pane_id) else {
            return;
        };
        let Some(path) = self.state.selected_png_path() else {
            self.show_toast(
                "Select a workspace PNG path in the terminal to preview it",
                false,
            );
            return;
        };
        self.send_command(UiCommand::PreviewImage {
            workspace_id: pane.summary.workspace_id,
            pane_id,
            path,
        });
    }

    pub(super) fn copy_focused_selection(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        let selected_text = self.state.panes.get(&pane_id).and_then(|pane| {
            let sel = pane.selection?;
            let text = extract_selected_text(pane.visible_rows(), &sel);
            (!text.is_empty()).then_some(text)
        });
        let Some(text) = selected_text else {
            return;
        };

        match self.ensure_clipboard().and_then(|cb| {
            cb.set_text(text)
                .map_err(|err| format!("Failed to copy to clipboard: {err}"))
        }) {
            Ok(()) => self.show_toast("Copied selection to clipboard", false),
            Err(err) => self.show_toast(err, true),
        }
    }

    pub(super) fn clipboard_text(&mut self) -> Option<String> {
        match self.ensure_clipboard().and_then(|cb| {
            cb.get_text()
                .map_err(|err| format!("Failed to read clipboard: {err}"))
        }) {
            Ok(text) => (!text.is_empty()).then_some(text),
            Err(err) => {
                self.show_toast(err, true);
                None
            }
        }
    }

    pub(super) fn paste_clipboard_to_focused(&mut self) {
        let Some(pane_id) = self.state.focused_pane_id() else {
            return;
        };
        let Some(text) = self.clipboard_text() else {
            return;
        };
        if let Some(pane) = self.state.panes.get_mut(&pane_id) {
            pane.reset_scroll();
            pane.selection = None;
        }
        self.send_command(UiCommand::Paste {
            pane_id,
            data: text.into_bytes(),
        });
        self.request_redraw();
    }

    pub(super) fn clicked_link(
        &self,
        role: PaneRole,
        col: u16,
        row: u16,
    ) -> Option<daedalus_ui::links::DetectedLink> {
        let pane = self.state.pane_for_role(role)?;
        let rows = pane.visible_rows();
        browser_link_at(rows, row as usize, col as usize).or_else(|| {
            if self.state.remote_host.is_some() {
                return None;
            }
            let cwd = pane.summary.cwd.as_deref();
            let root = self
                .state
                .active_workspace()
                .map(|ws| ws.root_dir.as_path());
            detect_link_in_row(rows.get(row as usize)?, col as usize, cwd, root)
                .filter(|link| matches!(link, daedalus_ui::links::DetectedLink::File { .. }))
        })
    }

    pub(super) fn open_clicked_link(&mut self, role: PaneRole, col: u16, row: u16) {
        let Some(link) = self.clicked_link(role, col, row) else {
            return;
        };

        if let Err(err) = open_link(&link, self.ui_config.editor.as_deref()) {
            warn!(error = %err, link = ?link, "failed to open clicked link");
            self.show_toast(format!("Failed to open link: {err}"), true);
        }
    }
}
