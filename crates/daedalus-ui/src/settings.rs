use crate::{
    assignments::Assignment, layout::Rect, widget_editor::WidgetEditor,
    workspace_widgets::WorkspaceWidgets,
};
use daedalus_proto::WorkspaceId;
use std::sync::atomic::{AtomicU64, Ordering};
use winit::keyboard::{Key, ModifiersState, NamedKey};
mod assignments;
mod custom;
mod pane_widgets;

#[derive(Debug)]
pub enum SettingsAction {
    None,
    Cancel,
    Save(Vec<Assignment>),
    Launch(Vec<Assignment>, String),
    SaveWidgets(WorkspaceWidgets),
    Workbench {
        operation: crate::bridge::WorkbenchOperation,
        request: Box<daedalus_proto::ClientMessage>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    AssignmentsPage,
    WidgetsPage,
    CustomWidgetsPage,
    CustomNew,
    CustomEdit,
    CustomPrev,
    CustomNext,
    CustomDuplicate,
    CustomRemove,
    CustomPin,
    CustomMove,
    CustomSharing,
    CustomBack,
    CustomTemplate,
    CustomName,
    CustomExecutable,
    CustomArguments,
    CustomInterval,
    CustomOutput,
    CustomPublish,
    CustomSummary,
    CustomDetails,
    CustomUrl,
    CustomPreview,
    CustomDetail(usize),
    CustomDetailPrev,
    CustomDetailNext,
    CustomViewAction(usize),
    CustomRefresh,
    CustomAdvanced,
    CustomDefinition,
    CustomApplyDefinition,
    CustomSave,
    CustomSaveTemplate,
    CustomPath,
    CustomRoot,
    CustomImport,
    CustomExport,
    CustomImportTemplate,
    CustomExportTemplate,
    CustomRepo,
    CustomApplyImport,
    CustomResolve,
    CustomResolveIncoming,
    ServiceWidget,
    RunnerWidget,
    ServicePreset,
    ServiceLabel,
    RunnerPreset,
    RunnerLabel,
    RunnerDuration,
    RunnerWarnings,
    Prev,
    Next,
    Add,
    Agy,
    Claude,
    Codex,
    CargoTest,
    CargoCheck,
    Remove,
    #[default]
    Name,
    Role,
    Executable,
    Argument(usize),
    DropArgument(usize),
    AddArgument,
    PreviousArguments,
    NextArguments,
    Cancel,
    Save,
    Launch,
}
use Control::*;

const PAGE_SIZE: usize = 3;

impl Control {
    pub(crate) fn primary(self) -> bool {
        matches!(
            self,
            Save | CustomSave | CustomApplyImport | CustomApplyDefinition
        )
    }
    pub fn field_label(self) -> Option<String> {
        Some(match self {
            Name => "Name".into(),
            Executable => "Executable".into(),
            Argument(i) => format!("Argument {}", i + 1),
            ServiceLabel => "Service label".into(),
            RunnerLabel => "Runner label".into(),
            CustomName => "Widget name".into(),
            CustomExecutable => "Executable".into(),
            CustomArguments => "Arguments (JSON)".into(),
            CustomInterval => "Refresh seconds".into(),
            CustomPublish => "Published status key".into(),
            CustomSummary => "Bar fields".into(),
            CustomDetails => "Detail fields".into(),
            CustomUrl => "Open URL".into(),
            CustomPath => "TOML file path".into(),
            CustomRoot => "New workspace folder (optional, on daemon host)".into(),
            CustomDefinition => "Widget definition (JSON)".into(),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Assignments,
    PaneWidgets,
    CustomWidgets,
}

static NEXT_SAVE_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct PendingSave {
    request_id: u64,
    workspace_id: WorkspaceId,
    launch: Option<Assignment>,
}

#[derive(Clone, Debug, Default)]
pub struct SettingsDialog {
    pub open: bool,
    pub error: Option<String>,
    pending_save: Option<PendingSave>,
    pending_pane_widgets: Option<(WorkspaceId, u64)>,
    page: Page,
    pub custom_supported: bool,
    pub custom: WidgetEditor,
    pub widgets: WorkspaceWidgets,
    pub widget_workspace: Option<WorkspaceId>,
    pub widgets_saved: bool,
    automatic_service_widget: bool,
    automatic_runner_widget: bool,
    assignments: assignments::AssignmentPage,
    pub focus: Control,
    pub invalid_field: Option<Control>,
    revision: u64,
    select_all: bool,
    // Byte offset from the end keeps the caret at the end when switching fields.
    from_end: usize,
}

impl SettingsDialog {
    pub fn is_saving(&self) -> bool {
        self.pending_save.is_some() || self.pending_pane_widgets.is_some()
    }

    pub fn start_pane_widget_save(&mut self, workspace_id: WorkspaceId) -> u64 {
        let token = NEXT_SAVE_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.pending_pane_widgets = Some((workspace_id, token));
        self.widgets_saved = false;
        self.error = None;
        token
    }

    pub fn finish_pane_widget_save(&mut self, workspace_id: WorkspaceId, token: u64) -> bool {
        if self.pending_pane_widgets != Some((workspace_id, token)) {
            return false;
        }
        self.pending_pane_widgets = None;
        true
    }

    pub fn fail_pane_widget_save(&mut self) {
        if self.pending_pane_widgets.take().is_some() {
            self.error = Some(
                "Connection lost before the daemon confirmed the widget settings. Reconnect to retry."
                    .into(),
            );
        }
    }

    pub fn start_save(&mut self, workspace_id: WorkspaceId, launch: Option<Assignment>) -> u64 {
        let request_id = NEXT_SAVE_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.error = None;
        self.pending_save = Some(PendingSave {
            request_id,
            workspace_id,
            launch,
        });
        request_id
    }

    pub fn finish_save(
        &mut self,
        workspace_id: WorkspaceId,
        request_id: u64,
    ) -> Option<Option<Assignment>> {
        let pending = self.pending_save.as_ref()?;
        if pending.workspace_id != workspace_id || pending.request_id != request_id {
            return None;
        }
        self.pending_save.take().map(|pending| pending.launch)
    }

    pub fn fail_pending_save(&mut self, message: String) {
        if self.pending_save.take().is_some() {
            self.error = Some(message);
        }
    }

    fn logical_height(&self) -> f32 {
        112.0 + self.rows().len() as f32 * 40.0
    }

    pub fn modal_rect(&self, w: f32, h: f32, dpi: f32) -> Rect {
        let width = (w - 16.0 * dpi).max(1.0).min(650.0 * dpi);
        let height = (h - 16.0 * dpi).max(1.0).min(self.logical_height() * dpi);
        Rect::new((w - width) * 0.5, (h - height) * 0.5, width, height)
    }

    pub(crate) fn content_scale(&self, modal: Rect) -> f32 {
        modal.height / self.logical_height()
    }

    fn rows(&self) -> Vec<Vec<Control>> {
        let mut pages = vec![AssignmentsPage, WidgetsPage];
        if self.custom_supported {
            pages.push(CustomWidgetsPage);
        }
        match self.page {
            Page::Assignments => self.assignment_rows(pages),
            Page::PaneWidgets => self.pane_widget_rows(pages),
            Page::CustomWidgets => self.custom_rows(pages),
        }
    }

    pub(crate) fn is_pane_widgets_page(&self) -> bool {
        self.page == Page::PaneWidgets
    }

    pub fn is_custom_page(&self) -> bool {
        self.page == Page::CustomWidgets
    }

    pub fn open_custom_page(&mut self) {
        self.activate(CustomWidgetsPage);
    }

    pub fn control_order(&self) -> impl Iterator<Item = Control> + use<> {
        self.rows().into_iter().flatten()
    }

    pub fn controls(&self, modal: Rect) -> impl Iterator<Item = (Control, Rect)> + use<> {
        let rows = self.rows();
        let unit = self.content_scale(modal);
        let last = rows.len() - 1;
        let custom_page = self.is_custom_page();
        rows.into_iter().enumerate().flat_map(move |(index, row)| {
            let pad = modal.width * 0.025;
            let width = modal.width - 2.0 * pad;
            let top = if index == last {
                modal.bottom() - 52.0 * unit
            } else {
                modal.y + (52.0 + 40.0 * index as f32) * unit
            };
            let count = row.len() as f32;
            row.into_iter().enumerate().map(move |(i, control)| {
                // Keep navigation together and separate destructive and commit actions.
                // The equal-width fallback leaves room for labels on narrow screens.
                let roomy = width >= 500.0 * unit;
                let (offset, fraction) = match control {
                    AssignmentsPage if count == 3.0 => (0., 0.34),
                    WidgetsPage if count == 3.0 => (0.34, 0.41),
                    CustomWidgetsPage => (0.75, 0.25),
                    Prev if roomy => (0., 0.17),
                    Next if roomy => (0.17, 0.15),
                    Remove if roomy => (0.84, 0.16),
                    Cancel if custom_page && roomy && count == 2.0 => (0.83, 0.17),
                    Cancel if roomy => (0., 0.17),
                    CustomBack if roomy => (0., 0.17),
                    CustomSave if roomy => (0.75, 0.25),
                    Save if roomy && count == 2.0 => (0.75, 0.25),
                    Save if roomy => (0.58, 0.17),
                    Launch if roomy => (0.75, 0.25),
                    AddArgument if roomy && count == 1.0 => (0., 0.24),
                    Argument(_) => (0., 0.80),
                    DropArgument(_) => (0.80, 0.20),
                    ServicePreset | RunnerPreset if roomy => (0., 0.70),
                    ServiceWidget | RunnerWidget if roomy && count == 2.0 => (0.70, 0.30),
                    _ => (i as f32 / count, 1. / count),
                };
                (
                    control,
                    Rect::new(
                        modal.x + pad + offset * width,
                        top,
                        width * fraction,
                        36.0 * unit,
                    ),
                )
            })
        })
    }

    pub fn begin(&mut self, entries: Vec<Assignment>) {
        *self = Self {
            open: true,
            revision: self.revision.wrapping_add(1),
            assignments: assignments::AssignmentPage::new(entries),
            ..Default::default()
        };
        if self.assignments.is_empty() {
            self.focus = Add;
        }
    }

    pub fn label(&self, c: Control) -> String {
        if let Some(label) = self.custom_label(c) {
            return label;
        }
        if let Some(label) = self
            .pane_widget_label(c)
            .or_else(|| self.assignment_label(c))
        {
            return label;
        }
        match c {
            AssignmentsPage => "Assignments".into(),
            WidgetsPage => "Widgets".into(),
            Cancel => "Cancel".into(),
            Save => "Save".into(),
            _ => String::new(),
        }
    }

    pub fn accessibility_description(&self) -> String {
        let mut description = format!("{}. Focused: {}.", self.heading(), self.label(self.focus));
        if self.select_all && self.focus.field_label().is_some() {
            description.push_str(" All text selected.");
        }
        if self.is_saving() {
            description.push_str(" Saving workspace settings.");
        } else if let Some(error) = &self.error {
            description.push(' ');
            description.push_str(error);
        }
        description
    }

    pub(crate) fn visible_label(&self, control: Control, columns: usize) -> String {
        if control == self.focus
            && let (Some(label), Some(text)) = (control.field_label(), self.value(control))
        {
            return crate::renderer::text::editable_label(
                &label,
                text,
                text.len() - self.from_end,
                columns,
            );
        }
        self.label(control)
    }

    pub(crate) fn hint(&self) -> &str {
        if self.is_saving() {
            "Saving workspace settings…"
        } else if let Some(error) = &self.error {
            error
        } else if self.is_custom_page()
            && let Some(path) = self.next_import_conflict()
        {
            path
        } else if self.is_custom_page()
            && let Some(preview) = &self.custom.preview
        {
            &preview.label
        } else if self.select_all && self.field_has_text() {
            "All text selected · Type to replace"
        } else if self.is_pane_widgets_page() && self.widgets_saved {
            "Widget settings saved"
        } else if self.is_pane_widgets_page() {
            "Defaults use the project configuration · Blank labels inherit"
        } else {
            "Tab moves focus · Enter activates"
        }
    }

    fn field_has_text(&self) -> bool {
        self.value(self.focus)
            .is_some_and(|value| !value.is_empty())
    }

    pub(crate) fn position_label(&self) -> String {
        if self.is_pane_widgets_page() || self.is_custom_page() {
            String::new()
        } else if self.assignments.is_empty() {
            "No assignments".into()
        } else {
            format!(
                "{} of {}",
                self.assignments.selected + 1,
                self.assignments.len()
            )
        }
    }

    pub fn heading(&self) -> String {
        if self.is_custom_page() {
            if self.custom.sharing {
                "Workspace sharing".into()
            } else if self.custom.showing_details {
                self.custom
                    .selected()
                    .map_or_else(|| "Widget details".into(), |widget| widget.name.clone())
            } else if self.custom.draft.is_some() {
                "Widget editor".into()
            } else {
                "Custom widgets".into()
            }
        } else if self.is_pane_widgets_page() {
            "Workspace widgets".into()
        } else {
            format!("Workspace assignments · {}", self.position_label())
        }
    }

    pub fn enabled(&self, c: Control) -> bool {
        if self.is_saving() {
            return false;
        }
        if self.is_custom_page() {
            return self.custom_enabled(c);
        }
        if self.is_pane_widgets_page() {
            self.pane_widget_enabled(c)
        } else {
            self.assignment_enabled(c)
        }
    }

    fn field(&mut self) -> Option<&mut String> {
        if self.is_saving() {
            return None;
        }
        match self.page {
            Page::Assignments => self.assignment_field(),
            Page::PaneWidgets => self.pane_widget_field(),
            Page::CustomWidgets => self.custom_field(),
        }
    }

    pub fn insert_text(&mut self, text: &str) {
        let replace = self.select_all;
        let from_end = self.from_end;
        if let Some(field) = self.field() {
            if replace {
                field.clear();
            }
            let caret = field.len().saturating_sub(from_end);
            field.insert_str(caret, text);
            self.from_end = field.len() - caret - text.len();
            self.select_all = false;
            self.error = None;
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn reconcile_controls(&mut self, previous: &[Control]) {
        let current: Vec<_> = self.control_order().collect();
        if previous != current {
            self.revision = self.revision.wrapping_add(1);
        }
        if !current.contains(&self.focus) {
            self.focus = current
                .iter()
                .copied()
                .find(|&control| self.enabled(control))
                .unwrap_or_default();
            self.from_end = 0;
            self.select_all = false;
        }
    }

    pub fn value(&self, control: Control) -> Option<&str> {
        if self.is_custom_page() {
            return self.custom_value(control);
        }
        if self.is_pane_widgets_page() {
            self.pane_widget_value(control)
        } else {
            self.assignment_value(control)
        }
    }

    pub fn selection(&self, control: Control) -> std::ops::Range<usize> {
        let len = self.value(control).map_or(0, str::len);
        if self.focus != control {
            return 0..0;
        }
        if self.select_all {
            return 0..len;
        }
        let cursor = len.saturating_sub(self.from_end);
        cursor..cursor
    }

    pub fn set_focus(&mut self, control: Control) {
        if self.enabled(control) && self.focus != control {
            self.focus = control;
            self.from_end = 0;
            self.select_all = control.field_label().is_some();
        }
    }

    pub fn set_value(&mut self, control: Control, value: &str) {
        if self.enabled(control)
            && self.value(control).is_some()
            && !value.chars().any(char::is_control)
        {
            self.set_focus(control);
            self.select_all = true;
            self.from_end = 0;
            self.insert_text(value);
        }
    }

    pub fn activate(&mut self, control: Control) -> SettingsAction {
        if !self.enabled(control) {
            return SettingsAction::None;
        }
        let previous = (self.assignments.selected, self.assignments.len());
        let previous_page = self.assignments.argument_page;
        let previous_dialog_page = self.page;
        let action = self.apply_control(control);
        if previous != (self.assignments.selected, self.assignments.len()) {
            self.assignments.argument_page = match self.focus {
                Argument(i) => i / PAGE_SIZE,
                _ => 0,
            };
        }
        if previous != (self.assignments.selected, self.assignments.len())
            || previous_page != self.assignments.argument_page
            || previous_dialog_page != self.page
            || self.is_custom_page()
            || matches!(
                control,
                AddArgument | DropArgument(_) | ServiceWidget | RunnerWidget
            )
        {
            // Ignore queued platform actions addressed to a previous set of rows.
            self.revision = self.revision.wrapping_add(1);
        }
        action
    }

    fn apply_control(&mut self, c: Control) -> SettingsAction {
        self.focus = c;
        self.from_end = 0;
        self.select_all = false;
        self.error = None;
        self.invalid_field = None;
        match c {
            AssignmentsPage => self.page = Page::Assignments,
            WidgetsPage => self.page = Page::PaneWidgets,
            CustomWidgetsPage => self.page = Page::CustomWidgets,
            Cancel => return SettingsAction::Cancel,
            _ => {
                return match self.page {
                    Page::Assignments => self.apply_assignment_control(c),
                    Page::PaneWidgets => self.apply_pane_widget_control(c),
                    Page::CustomWidgets => self.apply_custom_control(c),
                };
            }
        }
        SettingsAction::None
    }

    pub fn scroll(&mut self, delta_y: f32) {
        if self.page == Page::Assignments {
            self.scroll_assignments(delta_y);
        }
    }

    pub(crate) fn page_selected(&self, control: Control) -> bool {
        matches!(
            (self.page, control),
            (Page::Assignments, AssignmentsPage)
                | (Page::PaneWidgets, WidgetsPage)
                | (Page::CustomWidgets, CustomWidgetsPage)
        )
    }

    pub fn click(&mut self, x: f32, y: f32, w: f32, h: f32, dpi: f32) -> SettingsAction {
        self.controls(self.modal_rect(w, h, dpi))
            .find(|(_, r)| r.contains(x, y))
            .map_or(SettingsAction::None, |(c, _)| self.activate(c))
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> SettingsAction {
        if self.is_saving() {
            return SettingsAction::None;
        }
        match key {
            Key::Named(NamedKey::Escape) => return SettingsAction::Cancel,
            Key::Named(NamedKey::Enter) => return self.activate(self.focus),
            Key::Named(NamedKey::Tab) => {
                let enabled: Vec<_> = self.control_order().filter(|&c| self.enabled(c)).collect();
                let i = enabled.iter().position(|&c| c == self.focus).unwrap_or(0);
                self.focus = enabled[(i + if mods.shift_key() {
                    enabled.len() - 1
                } else {
                    1
                }) % enabled.len()];
                self.select_all = self.focus.field_label().is_some();
                self.from_end = 0;
            }
            Key::Named(
                key @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::Delete
                | NamedKey::Backspace),
            ) => {
                let from_end = self.from_end;
                let all = self.select_all;
                if let Some(field) = self.field() {
                    let caret = field.len() - from_end;
                    let prev = field[..caret]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    let next = caret + field[caret..].chars().next().map_or(0, char::len_utf8);
                    let new_caret = match key {
                        NamedKey::ArrowLeft if all => 0,
                        NamedKey::ArrowRight if all => field.len(),
                        NamedKey::ArrowLeft => prev,
                        NamedKey::ArrowRight => next,
                        NamedKey::Home => 0,
                        NamedKey::End => field.len(),
                        _ if all => {
                            field.clear();
                            0
                        }
                        NamedKey::Backspace => {
                            field.replace_range(prev..caret, "");
                            prev
                        }
                        _ => {
                            field.replace_range(caret..next, "");
                            caret
                        }
                    };
                    self.from_end = field.len() - new_caret;
                }
                self.select_all = false;
            }
            Key::Named(NamedKey::Space) if self.focus.field_label().is_none() => {
                return self.activate(self.focus);
            }
            Key::Named(NamedKey::Space) => self.insert_text(" "),
            Key::Character(s) if mods.control_key() || mods.super_key() => {
                match s.to_ascii_lowercase().as_str() {
                    "a" => self.select_all = true,
                    "s" => return self.activate(Save),
                    _ => {}
                }
            }
            Key::Character(s) if !mods.alt_key() => self.insert_text(s),
            _ => {}
        }
        SettingsAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::PaneRole;

    #[test]
    fn cargo_templates_create_editable_runner_assignments() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(Vec::new());
        dialog.activate(CargoTest);
        dialog.activate(CargoCheck);
        dialog.set_value(Argument(1), "--all-targets");
        let SettingsAction::Launch(entries, id) = dialog.activate(Launch) else {
            panic!("expected launch");
        };
        assert_eq!(entries[0].role, PaneRole::Runner);
        assert_eq!(entries[0].argv, ["cargo", "test", "--workspace"]);
        assert_eq!(entries[1].role, PaneRole::Runner);
        assert_eq!(entries[1].argv, ["cargo", "check", "--all-targets"]);
        assert_eq!(entries[1].id, id);
    }

    #[test]
    fn editing_preserves_argument_boundaries_and_selection() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(Vec::new());
        dialog.activate(Codex);
        dialog.activate(Add);
        dialog.insert_text("Test");
        dialog.activate(Executable);
        dialog.insert_text("printf");
        for arg in ["", "two words", "$HOME", "discard", "α"] {
            dialog.activate(AddArgument);
            dialog.insert_text(arg);
        }
        dialog.activate(DropArgument(3));
        let SettingsAction::Launch(entries, id) = dialog.activate(Launch) else {
            panic!("expected launch");
        };
        assert_eq!(entries[1].id, id);
        assert_eq!(entries[1].argv, ["printf", "", "two words", "$HOME", "α"]);
        dialog.begin(entries);
        dialog.activate(Next);
        dialog.activate(Remove);
        let SettingsAction::Save(entries) = dialog.activate(Save) else {
            panic!("expected save");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "codex");
    }

    #[test]
    fn keyboard_edits_and_mouse_presets_share_the_same_controls() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(Vec::new());
        let (w, h, dpi) = (850.0, 600.0, 1.5);
        let click = |dialog: &mut SettingsDialog, control| {
            let (_, r) = dialog
                .controls(dialog.modal_rect(w, h, dpi))
                .find(|(c, _)| *c == control)
                .unwrap();
            dialog.click(r.x + r.width * 0.5, r.y + r.height * 0.5, w, h, dpi)
        };
        click(&mut dialog, Claude);
        dialog.insert_text("αβ");
        dialog.key(&Key::Named(NamedKey::ArrowLeft), ModifiersState::empty());
        dialog.key(&Key::Named(NamedKey::Space), ModifiersState::empty());
        assert!(dialog.accessibility_description().contains("α β"));
        dialog.key(&Key::Named(NamedKey::Backspace), ModifiersState::empty());
        click(&mut dialog, Agy);
        dialog.scroll(1.0);
        let SettingsAction::Launch(entries, id) = click(&mut dialog, Launch) else {
            panic!()
        };
        assert_eq!(entries[0].name, "αβ");
        assert_eq!(entries[0].id, id);
    }

    #[test]
    fn keyboard_presets_and_long_unicode_names_keep_focus_visible() {
        use unicode_width::UnicodeWidthStr;
        let mut dialog = SettingsDialog::default();
        dialog.begin(Vec::new());
        dialog.key(&Key::Named(NamedKey::Space), ModifiersState::empty());
        assert_eq!(dialog.focus, Name);
        dialog.insert_text("Example 長いコマンド é");
        dialog.activate(Name);
        assert!(dialog.hint().contains("All text selected"));
        let visible = dialog.visible_label(Name, 18);
        assert!(visible.starts_with("Name   …") && visible.ends_with("é│"));
        assert!(visible.width() <= 18);
        dialog.key(&Key::Named(NamedKey::Home), ModifiersState::empty());
        assert!(
            dialog
                .visible_label(Name, 18)
                .starts_with("Name   │Example")
        );
    }

    #[test]
    fn invalid_drafts_focus_the_field_and_block_launch() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(Vec::new());
        dialog.activate(Claude);
        dialog.activate(Executable);
        dialog.insert_text("");
        assert!(matches!(dialog.activate(Launch), SettingsAction::None));
        assert_eq!(dialog.focus, Executable);
        assert!(dialog.error.is_some());
    }

    #[test]
    fn argument_pagination_tracks_focus_and_returns_to_invalid_argument_on_save() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(vec![]);
        dialog.activate(Codex);
        for i in 0..=PAGE_SIZE {
            dialog.activate(AddArgument);
            dialog.insert_text(&format!("arg{i}"));
        }
        assert_eq!(dialog.focus, Argument(PAGE_SIZE));
        let last_page = dialog.revision();
        assert!(!dialog.enabled(NextArguments));
        dialog.activate(PreviousArguments);
        assert_ne!(dialog.revision(), last_page);
        assert_eq!(dialog.focus, Argument(0));
        assert!(dialog.control_order().any(|c| c == Argument(0)));
        assert!(!dialog.control_order().any(|c| c == Argument(PAGE_SIZE)));
        dialog.activate(NextArguments);
        assert_eq!(dialog.focus, Argument(PAGE_SIZE));
        let SettingsAction::Save(mut saved) = dialog.activate(Save) else {
            panic!("valid arguments should save");
        };
        saved[0].argv[1 + PAGE_SIZE] = "bad\0argument".into();
        dialog.begin(saved);
        dialog.activate(Save);
        assert_eq!(dialog.focus, Argument(PAGE_SIZE));
        assert!(dialog.control_order().any(|c| c == Argument(PAGE_SIZE)));
        assert!(
            dialog
                .accessibility_description()
                .contains("null character")
        );
        dialog.activate(DropArgument(PAGE_SIZE));
        assert_eq!(dialog.focus, Argument(PAGE_SIZE - 1));
        let SettingsAction::Save(entries) = dialog.activate(Save) else {
            panic!("could not save corrected arguments");
        };
        assert_eq!(entries[0].argv.len(), 1 + PAGE_SIZE);
    }

    #[test]
    fn keyboard_navigation_replaces_selection_and_saves_a_changed_assignment_role() {
        let mut dialog = SettingsDialog::default();
        dialog.begin(vec![]);
        let named = |dialog: &mut SettingsDialog, key| {
            dialog.key(&Key::Named(key), ModifiersState::empty())
        };
        dialog.activate(Claude);
        named(&mut dialog, NamedKey::ArrowLeft);
        assert_eq!(dialog.selection(Name), 0..0);
        named(&mut dialog, NamedKey::Delete);
        assert_eq!(dialog.value(Name), Some("laude"));
        named(&mut dialog, NamedKey::End);
        named(&mut dialog, NamedKey::Backspace);
        dialog.key(&Key::Character("a".into()), ModifiersState::CONTROL);
        assert_eq!(dialog.selection(Name), 0..4);
        dialog.key(&Key::Character("build".into()), ModifiersState::empty());
        named(&mut dialog, NamedKey::Tab);
        assert_eq!(dialog.focus, Role);
        for role in [PaneRole::Service, PaneRole::Runner, PaneRole::Agent] {
            named(&mut dialog, NamedKey::Enter);
            assert!(dialog.label(Role).contains(&role.to_string()));
        }
        dialog.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(dialog.focus, Name);
        let SettingsAction::Save(entries) =
            dialog.key(&Key::Character("s".into()), ModifiersState::CONTROL)
        else {
            panic!("save shortcut was ignored");
        };
        assert_eq!(entries[0].name, "build");
        assert_eq!(entries[0].argv, ["claude"]);
        assert!(matches!(
            named(&mut dialog, NamedKey::Escape),
            SettingsAction::Cancel
        ));
    }
}
