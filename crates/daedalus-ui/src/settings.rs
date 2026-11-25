use crate::{
    assignments::{Assignment, CargoPreset},
    layout::Rect,
    workspace_widgets::WorkspaceWidgets,
};
use daedalus_proto::{PaneRole, WorkspaceId};
use std::sync::atomic::{AtomicU64, Ordering};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug)]
pub enum SettingsAction {
    None,
    Cancel,
    Save(Vec<Assignment>),
    Launch(Vec<Assignment>, String),
    SaveWidgets(WorkspaceWidgets),
}

#[derive(Clone, Debug)]
struct Draft {
    id: String,
    name: String,
    role: PaneRole,
    executable: String,
    args: Vec<String>,
}

impl From<Assignment> for Draft {
    fn from(assignment: Assignment) -> Self {
        let mut argv = assignment.argv.into_iter();
        Self {
            id: assignment.id,
            name: assignment.name,
            role: assignment.role,
            executable: argv.next().unwrap_or_default(),
            args: argv.collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    AssignmentsPage,
    WidgetsPage,
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
    pub fn field_label(self) -> Option<String> {
        Some(match self {
            Name => "Name".into(),
            Executable => "Executable".into(),
            Argument(i) => format!("Argument {}", i + 1),
            ServiceLabel => "Service label".into(),
            RunnerLabel => "Runner label".into(),
            _ => return None,
        })
    }
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
    pub(crate) widgets_page: bool,
    pub widgets: WorkspaceWidgets,
    pub widget_workspace: Option<WorkspaceId>,
    pub widgets_saved: bool,
    drafts: Vec<Draft>,
    selected: usize,
    argument_page: usize,
    pub focus: Control,
    pub invalid_field: Option<Control>,
    revision: u64,
    select_all: bool,
    // Byte offset from the end keeps the caret at the end when switching fields.
    from_end: usize,
}

impl SettingsDialog {
    pub fn is_saving(&self) -> bool {
        self.pending_save.is_some()
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
        let pages = vec![AssignmentsPage, WidgetsPage];
        if self.widgets_page {
            return vec![
                pages,
                vec![ServicePreset],
                vec![ServiceLabel],
                vec![RunnerPreset],
                vec![RunnerLabel],
                vec![RunnerDuration, RunnerWarnings],
                vec![Cancel, Save],
            ];
        }
        let mut rows = vec![
            pages,
            vec![Prev, Next, Remove],
            vec![Add, Agy, Claude, Codex],
            vec![CargoTest, CargoCheck],
            vec![Name],
            vec![Role],
            vec![Executable],
        ];
        let count = self.drafts.get(self.selected).map_or(0, |d| d.args.len());
        let start = self.argument_page * PAGE_SIZE;
        for i in start..(start + PAGE_SIZE).min(count) {
            rows.push(vec![Argument(i), DropArgument(i)]);
        }
        let mut buttons = vec![AddArgument];
        if count > PAGE_SIZE {
            buttons.extend([PreviousArguments, NextArguments]);
        }
        rows.push(buttons);
        rows.push(vec![Cancel, Save, Launch]);
        rows
    }

    pub fn control_order(&self) -> impl Iterator<Item = Control> + use<> {
        self.rows().into_iter().flatten()
    }

    pub fn controls(&self, modal: Rect) -> impl Iterator<Item = (Control, Rect)> + use<> {
        let rows = self.rows();
        let unit = self.content_scale(modal);
        let last = rows.len() - 1;
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
                    Prev if roomy => (0., 0.17),
                    Next if roomy => (0.17, 0.15),
                    Remove if roomy => (0.84, 0.16),
                    Cancel if roomy => (0., 0.17),
                    Save if roomy && count == 2.0 => (0.75, 0.25),
                    Save if roomy => (0.58, 0.17),
                    Launch if roomy => (0.75, 0.25),
                    AddArgument if roomy && count == 1.0 => (0., 0.24),
                    Argument(_) => (0., 0.80),
                    DropArgument(_) => (0.80, 0.20),
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
            drafts: entries.into_iter().map(Draft::from).collect(),
            ..Default::default()
        };
        if self.drafts.is_empty() {
            self.focus = Add;
        }
    }

    pub fn label(&self, c: Control) -> String {
        let draft = self.drafts.get(self.selected);
        match c {
            AssignmentsPage => "Assignments".into(),
            WidgetsPage => "Widgets".into(),
            ServicePreset => format!(
                "Service preset   {}   ↻",
                preset_label(self.widgets.service.preset.as_deref())
            ),
            RunnerPreset => format!(
                "Runner preset   {}   ↻",
                preset_label(self.widgets.runner.preset.as_deref())
            ),
            ServiceLabel | RunnerLabel => format!(
                "{}   {}",
                c.field_label().unwrap(),
                self.value(c).unwrap_or_default()
            ),
            RunnerDuration => format!(
                "Duration · {}",
                detail_label(self.widgets.runner.show_duration)
            ),
            RunnerWarnings => format!(
                "Warnings · {}",
                detail_label(self.widgets.runner.show_warnings)
            ),
            Name => format!("Name   {}", draft.map_or("", |d| &d.name)),
            Role => format!(
                "Pane   {}   ↻",
                draft.map_or("—".into(), |d| d.role.to_string())
            ),
            Executable | Argument(_) => format!(
                "{}   {}",
                c.field_label().unwrap(),
                self.value(c).unwrap_or_default()
            ),
            DropArgument(i) => format!("Remove {}", i + 1),
            AddArgument => "+ Argument".into(),
            PreviousArguments => "‹ Arguments".into(),
            NextArguments => "Arguments ›".into(),
            Prev => "‹ Previous".into(),
            Next => "Next ›".into(),
            Remove => "Remove".into(),
            Add => "+ Command".into(),
            Agy => "+ Agy".into(),
            Claude => "+ Claude".into(),
            Codex => "+ Codex".into(),
            CargoTest => "+ Cargo Test".into(),
            CargoCheck => "+ Cargo Check".into(),
            Cancel if self.widgets_page && self.widgets_saved => "Done".into(),
            Cancel => "Cancel".into(),
            Save if self.widgets_page => "Save widgets".into(),
            Save => "Save".into(),
            Launch => "Save & launch".into(),
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
        } else if self.select_all && self.field_has_text() {
            "All text selected · Type to replace"
        } else if self.widgets_page && self.widgets_saved {
            "Widget settings saved"
        } else if self.widgets_page {
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
        if self.widgets_page {
            String::new()
        } else if self.drafts.is_empty() {
            "No assignments".into()
        } else {
            format!("{} of {}", self.selected + 1, self.drafts.len())
        }
    }

    pub fn heading(&self) -> String {
        if self.widgets_page {
            "Workspace widgets".into()
        } else {
            format!("Workspace assignments · {}", self.position_label())
        }
    }

    pub fn enabled(&self, c: Control) -> bool {
        if self.is_saving() {
            return false;
        }
        match c {
            Prev | Next => self.drafts.len() > 1,
            PreviousArguments => self.argument_page > 0,
            NextArguments => self
                .drafts
                .get(self.selected)
                .is_some_and(|d| (self.argument_page + 1) * PAGE_SIZE < d.args.len()),
            Argument(i) | DropArgument(i) => self
                .drafts
                .get(self.selected)
                .is_some_and(|d| i < d.args.len()),
            Remove | Name | Role | Executable | AddArgument | Launch => !self.drafts.is_empty(),
            _ => true,
        }
    }

    fn field(&mut self) -> Option<&mut String> {
        if self.is_saving() {
            return None;
        }
        if self.widgets_page {
            self.widgets_saved = false;
            return match self.focus {
                ServiceLabel => Some(self.widgets.service.label.get_or_insert_with(String::new)),
                RunnerLabel => Some(self.widgets.runner.label.get_or_insert_with(String::new)),
                _ => None,
            };
        }
        let draft = self.drafts.get_mut(self.selected)?;
        match self.focus {
            Name => Some(&mut draft.name),
            Executable => Some(&mut draft.executable),
            Argument(i) => draft.args.get_mut(i),
            _ => None,
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

    pub fn value(&self, control: Control) -> Option<&str> {
        if self.widgets_page {
            return match control {
                ServiceLabel => Some(self.widgets.service.label.as_deref().unwrap_or_default()),
                RunnerLabel => Some(self.widgets.runner.label.as_deref().unwrap_or_default()),
                _ => None,
            };
        }
        let draft = self.drafts.get(self.selected)?;
        match control {
            Name => Some(&draft.name),
            Executable => Some(&draft.executable),
            Argument(i) => draft.args.get(i).map(String::as_str),
            _ => None,
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
        if self.value(control).is_some() && !value.chars().any(char::is_control) {
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
        let previous = (self.selected, self.drafts.len());
        let previous_page = self.argument_page;
        let was_widgets_page = self.widgets_page;
        let action = self.apply_control(control);
        if previous != (self.selected, self.drafts.len()) {
            self.argument_page = match self.focus {
                Argument(i) => i / PAGE_SIZE,
                _ => 0,
            };
        }
        if previous != (self.selected, self.drafts.len())
            || previous_page != self.argument_page
            || was_widgets_page != self.widgets_page
            || matches!(control, AddArgument | DropArgument(_))
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
        if matches!(
            c,
            ServicePreset | RunnerPreset | RunnerDuration | RunnerWarnings
        ) {
            self.widgets_saved = false;
        }
        match c {
            AssignmentsPage => self.widgets_page = false,
            WidgetsPage => self.widgets_page = true,
            ServicePreset => {
                cycle_preset(&mut self.widgets.service.preset, &["service", "command"])
            }
            RunnerPreset => cycle_preset(
                &mut self.widgets.runner.preset,
                &["cargo-test", "cargo-check", "command"],
            ),
            RunnerDuration => cycle_detail(&mut self.widgets.runner.show_duration),
            RunnerWarnings => cycle_detail(&mut self.widgets.runner.show_warnings),
            Prev => self.selected = (self.selected + self.drafts.len() - 1) % self.drafts.len(),
            Next => self.selected = (self.selected + 1) % self.drafts.len(),
            Add | Agy | Claude | Codex | CargoTest | CargoCheck => {
                let id = uuid::Uuid::new_v4().to_string();
                let mut template = match c {
                    CargoTest => CargoPreset::Test.assignment(),
                    CargoCheck => CargoPreset::Check.assignment(),
                    _ => {
                        let executable = match c {
                            Agy => "agy",
                            Claude => "claude",
                            Codex => "codex",
                            _ => "",
                        };
                        Assignment {
                            id: String::new(),
                            name: executable.into(),
                            role: if c == Add {
                                PaneRole::Runner
                            } else {
                                PaneRole::Agent
                            },
                            argv: vec![executable.into()],
                        }
                    }
                };
                template.id = id;
                self.selected = self.drafts.len();
                self.drafts.push(Draft::from(template));
                self.focus = Name;
                self.select_all = true;
            }
            Remove => {
                self.drafts.remove(self.selected);
                self.selected = self.selected.min(self.drafts.len().saturating_sub(1));
                self.focus = Add;
            }
            Role => {
                let role = &mut self.drafts[self.selected].role;
                *role = match role {
                    PaneRole::Agent => PaneRole::Service,
                    PaneRole::Service => PaneRole::Runner,
                    PaneRole::Runner => PaneRole::Agent,
                };
            }
            Name | Executable | Argument(_) | ServiceLabel | RunnerLabel => self.select_all = true,
            AddArgument => {
                let args = &mut self.drafts[self.selected].args;
                let i = args.len();
                args.push(String::new());
                self.argument_page = i / PAGE_SIZE;
                self.focus = Argument(i);
            }
            DropArgument(i) => {
                let args = &mut self.drafts[self.selected].args;
                args.remove(i);
                let next = i.min(args.len().saturating_sub(1));
                self.argument_page = next / PAGE_SIZE;
                self.focus = if args.is_empty() {
                    AddArgument
                } else {
                    Argument(next)
                };
            }
            PreviousArguments | NextArguments => {
                if c == PreviousArguments {
                    self.argument_page -= 1;
                } else {
                    self.argument_page += 1;
                }
                self.focus = Argument(self.argument_page * PAGE_SIZE);
                self.select_all = true;
            }
            Cancel => return SettingsAction::Cancel,
            Save if self.widgets_page => {
                let mut widgets = self.widgets.clone();
                for config in [&mut widgets.service, &mut widgets.runner] {
                    config.label = config
                        .label
                        .as_deref()
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .map(str::to_owned);
                }
                return SettingsAction::SaveWidgets(widgets);
            }
            Save | Launch => {
                let mut values = Vec::new();
                for (i, d) in self.drafts.iter().enumerate() {
                    let invalid = if d.name.trim().is_empty() {
                        Some((Name, "Enter an assignment name."))
                    } else if d.executable.trim().is_empty() || d.executable.contains('\0') {
                        Some((Executable, "Enter an executable without a null character."))
                    } else {
                        d.args
                            .iter()
                            .position(|arg| arg.contains('\0'))
                            .map(|i| (Argument(i), "Remove the null character from this argument."))
                    };
                    if let Some((control, message)) = invalid {
                        self.selected = i;
                        if let Argument(index) = control {
                            self.argument_page = index / PAGE_SIZE;
                        }
                        self.focus = control;
                        self.select_all = true;
                        self.invalid_field = Some(control);
                        self.error = Some(message.into());
                        return SettingsAction::None;
                    }
                    values.push(Assignment {
                        id: d.id.clone(),
                        name: d.name.clone(),
                        role: d.role,
                        argv: std::iter::once(&d.executable)
                            .chain(&d.args)
                            .cloned()
                            .collect(),
                    });
                }
                return if c == Launch {
                    SettingsAction::Launch(values, self.drafts[self.selected].id.clone())
                } else {
                    SettingsAction::Save(values)
                };
            }
        }
        SettingsAction::None
    }

    pub fn scroll(&mut self, delta_y: f32) {
        if self.widgets_page {
            return;
        }
        if delta_y > 0.0 {
            self.activate(Prev);
        }
        if delta_y < 0.0 {
            self.activate(Next);
        }
    }

    pub(crate) fn page_selected(&self, control: Control) -> bool {
        matches!(
            (self.widgets_page, control),
            (true, WidgetsPage) | (false, AssignmentsPage)
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

fn preset_label(preset: Option<&str>) -> &'static str {
    match preset {
        Some("service") => "Service",
        Some("command") => "Command",
        Some("cargo-test") => "Cargo Test",
        Some("cargo-check") => "Cargo Check",
        _ => "Default",
    }
}

fn cycle_preset(preset: &mut Option<String>, choices: &[&str]) {
    let next = preset
        .as_deref()
        .and_then(|current| choices.iter().position(|&choice| choice == current))
        .map_or(0, |index| index + 1);
    *preset = choices.get(next).map(|choice| (*choice).to_owned());
}

fn detail_label(value: Option<bool>) -> &'static str {
    match value {
        None => "Default",
        Some(true) => "Show",
        Some(false) => "Hide",
    }
}

fn cycle_detail(value: &mut Option<bool>) {
    *value = match *value {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
