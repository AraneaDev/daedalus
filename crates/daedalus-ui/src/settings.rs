use crate::{assignments::Assignment, layout::Rect};
use daedalus_proto::PaneRole;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug)]
pub enum SettingsAction {
    None,
    Cancel,
    Save(Vec<Assignment>),
    Launch(Vec<Assignment>, String),
}

#[derive(Clone, Debug)]
struct Draft {
    id: String,
    name: String,
    role: PaneRole,
    executable: String,
    args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    Prev,
    Next,
    Add,
    Agy,
    Claude,
    Codex,
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
            _ => return None,
        })
    }
}

pub fn modal_rect(w: f32, h: f32, dpi: f32) -> Rect {
    let width = (w - 16.0 * dpi).max(1.0).min(650.0 * dpi);
    let height = (h - 16.0 * dpi).max(1.0).min(480.0 * dpi);
    Rect::new((w - width) * 0.5, (h - height) * 0.5, width, height)
}
#[derive(Clone, Debug, Default)]
pub struct SettingsDialog {
    pub open: bool,
    pub error: Option<String>,
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
    fn rows(&self) -> Vec<Vec<Control>> {
        let mut rows = vec![
            vec![Prev, Next, Remove],
            vec![Add, Agy, Claude, Codex],
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
        let actions_row = 10;
        let last = rows.len() - 1;
        rows.into_iter().enumerate().flat_map(move |(index, row)| {
            let index = if index == last { actions_row } else { index };
            let pad = modal.width * 0.025;
            let width = modal.width - 2.0 * pad;
            let top = modal.y + (0.09 + 0.082 * index as f32) * modal.height;
            let count = row.len() as f32;
            row.into_iter().enumerate().map(move |(i, control)| {
                let (offset, fraction) = match control {
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
                        modal.height * 0.078,
                    ),
                )
            })
        })
    }

    pub fn begin(&mut self, entries: Vec<Assignment>) {
        *self = Self {
            open: true,
            revision: self.revision.wrapping_add(1),
            drafts: entries
                .into_iter()
                .map(|a| Draft {
                    id: a.id,
                    name: a.name,
                    role: a.role,
                    executable: a.argv.first().cloned().unwrap_or_default(),
                    args: a.argv.into_iter().skip(1).collect(),
                })
                .collect(),
            ..Default::default()
        };
        if self.drafts.is_empty() {
            self.focus = Add;
        }
    }

    pub fn label(&self, c: Control) -> String {
        let draft = self.drafts.get(self.selected);
        match c {
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
            Cancel => "Cancel".into(),
            Save => "Save".into(),
            Launch => "Save & launch".into(),
        }
    }

    pub fn accessibility_description(&self) -> String {
        let mut description = format!("{}. Focused: {}.", self.heading(), self.label(self.focus));
        if self.select_all && self.focus.field_label().is_some() {
            description.push_str(" All text selected.");
        }
        if let Some(error) = &self.error {
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
        if let Some(error) = &self.error {
            error
        } else if self.select_all && self.field_has_text() {
            "All text selected · Type to replace"
        } else {
            "Tab moves focus · Enter activates"
        }
    }

    fn field_has_text(&self) -> bool {
        self.value(self.focus)
            .is_some_and(|value| !value.is_empty())
    }

    pub fn heading(&self) -> String {
        if self.drafts.is_empty() {
            return "Workspace assignments · No assignments".into();
        }
        format!(
            "Workspace assignments · {} of {}",
            self.selected + 1,
            self.drafts.len()
        )
    }

    pub fn enabled(&self, c: Control) -> bool {
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
        let action = self.apply_control(control);
        if previous != (self.selected, self.drafts.len()) {
            self.argument_page = match self.focus {
                Argument(i) => i / PAGE_SIZE,
                _ => 0,
            };
        }
        if previous != (self.selected, self.drafts.len())
            || previous_page != self.argument_page
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
        match c {
            Prev => self.selected = (self.selected + self.drafts.len() - 1) % self.drafts.len(),
            Next => self.selected = (self.selected + 1) % self.drafts.len(),
            Add | Agy | Claude | Codex => {
                let preset = match c {
                    Agy => "agy",
                    Claude => "claude",
                    Codex => "codex",
                    _ => "",
                };
                self.selected = self.drafts.len();
                self.drafts.push(Draft {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: preset.into(),
                    role: if c == Add {
                        PaneRole::Runner
                    } else {
                        PaneRole::Agent
                    },
                    executable: preset.into(),
                    args: Vec::new(),
                });
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
            Name | Executable | Argument(_) => self.select_all = true,
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
        if delta_y > 0.0 {
            self.activate(Prev);
        }
        if delta_y < 0.0 {
            self.activate(Next);
        }
    }

    pub fn click(&mut self, x: f32, y: f32, w: f32, h: f32, dpi: f32) -> SettingsAction {
        self.controls(modal_rect(w, h, dpi))
            .find(|(_, r)| r.contains(x, y))
            .map_or(SettingsAction::None, |(c, _)| self.activate(c))
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> SettingsAction {
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
                .controls(modal_rect(w, h, dpi))
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
}
