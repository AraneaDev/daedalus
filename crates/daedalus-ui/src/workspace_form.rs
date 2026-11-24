use crate::layout::Rect;
use std::path::{Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug, PartialEq, Eq)]
pub enum WorkspaceFormAction {
    None,
    Cancel,
    Save { name: String, folder: PathBuf },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    #[default]
    Name,
    Folder,
    Cancel,
    Save,
}

const CONTROLS: [Control; 4] = [
    Control::Name,
    Control::Folder,
    Control::Cancel,
    Control::Save,
];

pub fn modal_rect(w: f32, h: f32, dpi: f32) -> Rect {
    let width = (w - 16.0 * dpi).max(1.0).min(520.0 * dpi);
    let height = (h - 16.0 * dpi).max(1.0).min(236.0 * dpi);
    Rect::new((w - width) / 2.0, (h - height) / 2.0, width, height)
}

pub fn controls(modal: Rect) -> impl Iterator<Item = (Control, Rect)> {
    CONTROLS
        .into_iter()
        .zip([
            (0.04, 0.21, 0.92, 0.18),
            (0.04, 0.40, 0.92, 0.18),
            (0.04, 0.77, 0.44, 0.17),
            (0.52, 0.77, 0.44, 0.17),
        ])
        .map(move |(control, (x, y, w, h))| {
            (
                control,
                Rect::new(
                    modal.x + x * modal.width,
                    modal.y + y * modal.height,
                    w * modal.width,
                    h * modal.height,
                ),
            )
        })
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceForm {
    pub open: bool,
    pub error: Option<String>,
    pub focus: Control,
    session: u64,
    submitting: bool,
    name: String,
    folder: String,
    cursor: usize,
    select_all: bool,
}

impl WorkspaceForm {
    pub fn begin(&mut self, folder: &Path) {
        *self = Self {
            open: true,
            session: self.session.wrapping_add(1),
            folder: folder.to_string_lossy().into_owned(),
            ..Default::default()
        };
    }

    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn submitting(&self) -> bool {
        self.submitting
    }

    pub fn start_submission(&mut self) {
        self.error = None;
        self.set_focus(Control::Cancel);
        self.submitting = true;
    }

    pub fn finish_submission(&mut self, session: u64, error: Option<String>) -> bool {
        if self.session != session || !self.submitting {
            return false;
        }
        self.submitting = false;
        if !self.open {
            return false;
        }
        if let Some(error) = error {
            self.error = Some(error);
            self.set_focus(Control::Folder);
        } else {
            self.open = false;
        }
        true
    }

    pub fn enabled(&self, control: Control) -> bool {
        !self.submitting || control == Control::Cancel
    }

    pub fn value(&self, control: Control) -> Option<&str> {
        match control {
            Control::Name => Some(&self.name),
            Control::Folder => Some(&self.folder),
            _ => None,
        }
    }

    pub fn selection(&self, control: Control) -> std::ops::Range<usize> {
        if control != self.focus {
            return 0..0;
        }
        if self.select_all {
            return 0..self.value(control).map_or(0, str::len);
        }
        self.cursor..self.cursor
    }

    pub fn set_value(&mut self, control: Control, value: &str) {
        if self.submitting || self.value(control).is_none() || value.chars().any(char::is_control) {
            return;
        }
        self.set_focus(control);
        self.select_all = true;
        self.replace(0, 0, value);
    }

    pub fn set_focus(&mut self, control: Control) {
        if !self.enabled(control) {
            return;
        }
        self.focus = control;
        self.cursor = self.value(self.focus).map_or(0, str::len);
        self.select_all = self.value(self.focus).is_some() && self.cursor > 0;
    }

    fn replace(&mut self, start: usize, end: usize, text: &str) {
        let field = match self.focus {
            Control::Name => &mut self.name,
            Control::Folder => &mut self.folder,
            _ => return,
        };
        let range = if self.select_all {
            0..field.len()
        } else {
            start..end
        };
        self.cursor = range.start + text.len();
        field.replace_range(range, text);
        self.select_all = false;
        self.error = None;
    }

    pub fn insert_text(&mut self, text: &str) {
        if !self.submitting && !text.is_empty() && !text.chars().any(char::is_control) {
            self.replace(self.cursor, self.cursor, text);
        }
    }

    fn submit(&mut self) -> WorkspaceFormAction {
        if self.folder.trim().is_empty() {
            self.set_focus(Control::Folder);
            self.error = Some("Enter the workspace folder.".into());
        } else {
            return WorkspaceFormAction::Save {
                name: if self.name.trim().is_empty() {
                    self.folder
                        .trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .filter(|name| !name.is_empty() && *name != ".")
                        .unwrap_or("Workspace")
                        .into()
                } else {
                    self.name.trim().into()
                },
                folder: PathBuf::from(&self.folder),
            };
        }
        WorkspaceFormAction::None
    }

    pub fn click(&mut self, x: f32, y: f32, w: f32, h: f32, dpi: f32) -> WorkspaceFormAction {
        let Some((control, _)) =
            controls(modal_rect(w, h, dpi)).find(|(_, rect)| rect.contains(x, y))
        else {
            return WorkspaceFormAction::None;
        };
        self.activate(control)
    }

    pub fn activate(&mut self, control: Control) -> WorkspaceFormAction {
        if !self.enabled(control) {
            return WorkspaceFormAction::None;
        }
        self.set_focus(control);
        match control {
            Control::Cancel => WorkspaceFormAction::Cancel,
            Control::Save => self.submit(),
            _ => WorkspaceFormAction::None,
        }
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> WorkspaceFormAction {
        if self.submitting {
            return match key {
                Key::Named(NamedKey::Escape | NamedKey::Enter | NamedKey::Space) => {
                    WorkspaceFormAction::Cancel
                }
                _ => WorkspaceFormAction::None,
            };
        }
        let field = self.value(self.focus).unwrap_or("");
        let prev = field[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i);
        let next = self.cursor
            + field[self.cursor..]
                .graphemes(true)
                .next()
                .map_or(0, str::len);
        match key {
            Key::Named(NamedKey::Escape) => return WorkspaceFormAction::Cancel,
            Key::Named(NamedKey::Enter) if self.focus == Control::Cancel => {
                return WorkspaceFormAction::Cancel;
            }
            Key::Named(NamedKey::Enter) => return self.submit(),
            Key::Named(NamedKey::Space) => match self.focus {
                Control::Cancel => return WorkspaceFormAction::Cancel,
                Control::Save => return self.submit(),
                Control::Name | Control::Folder => self.insert_text(" "),
            },
            Key::Named(NamedKey::Tab) => {
                let i = CONTROLS.iter().position(|c| *c == self.focus).unwrap_or(0);
                self.set_focus(
                    CONTROLS[(i + if mods.shift_key() { 3 } else { 1 }) % CONTROLS.len()],
                );
            }
            Key::Named(NamedKey::Backspace) => self.replace(prev, self.cursor, ""),
            Key::Named(NamedKey::Delete) => self.replace(self.cursor, next, ""),
            Key::Named(
                NamedKey::Home | NamedKey::End | NamedKey::ArrowLeft | NamedKey::ArrowRight,
            ) => {
                self.cursor = match key {
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => field.len(),
                    Key::Named(NamedKey::ArrowLeft) if self.select_all => 0,
                    Key::Named(NamedKey::ArrowRight) if self.select_all => field.len(),
                    Key::Named(NamedKey::ArrowLeft) => prev,
                    _ => next,
                };
                self.select_all = false;
            }
            Key::Character(s) if mods.control_key() || mods.super_key() => {
                if s.eq_ignore_ascii_case("a") {
                    self.select_all = true;
                }
            }
            Key::Character(s) if !mods.alt_key() => self.insert_text(s),
            _ => {}
        }
        WorkspaceFormAction::None
    }

    pub(crate) fn visible_label(&self, control: Control, columns: usize) -> String {
        let (label, text) = match control {
            Control::Name => ("Name (optional)", &self.name),
            Control::Folder => ("Folder", &self.folder),
            Control::Cancel if self.submitting => return "Dismiss".into(),
            Control::Cancel => return "Cancel".into(),
            Control::Save => return "Review environment".into(),
        };
        if control == self.focus {
            crate::renderer::text::editable_label(label, text, self.cursor, columns)
        } else {
            format!("{label}   {text}")
        }
    }

    pub(crate) fn hint(&self) -> &str {
        if self.submitting {
            "Checking project on daemon host…"
        } else if let Some(error) = &self.error {
            error
        } else if self.select_all && self.value(self.focus).is_some_and(|text| !text.is_empty()) {
            "All text selected · Type to replace"
        } else {
            "Tab to move · Esc to cancel"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_rejection_keeps_the_name_and_path_editable_for_retry() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/missing"));
        form.insert_text("Review");
        form.start_submission();
        assert!(!form.enabled(Control::Save));
        assert_eq!(form.activate(Control::Save), WorkspaceFormAction::None);
        form.set_value(Control::Name, "replacement");
        assert!(form.finish_submission(form.session(), Some("Folder missing".into())));
        assert!(form.open && form.enabled(Control::Save));
        assert_eq!(form.hint(), "Folder missing");
        assert_eq!(form.focus, Control::Folder);
        assert_eq!(form.value(Control::Name), Some("Review"));
        assert_eq!(form.value(Control::Folder), Some("/missing"));
        form.insert_text("/corrected");
        assert_eq!(
            form.activate(Control::Save),
            WorkspaceFormAction::Save {
                name: "Review".into(),
                folder: "/corrected".into()
            }
        );
        form.start_submission();
        assert!(form.finish_submission(form.session(), None));
        assert!(!form.open);
    }

    #[test]
    fn a_dismissed_creation_request_cannot_close_the_next_draft() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/one"));
        let first = form.session();
        form.start_submission();
        assert_eq!(form.activate(Control::Cancel), WorkspaceFormAction::Cancel);
        form.open = false;
        form.begin(Path::new("/two"));
        form.start_submission();
        assert!(!form.finish_submission(first, None));
        assert!(form.open && form.submitting());
        assert_eq!(form.value(Control::Folder), Some("/two"));
    }

    #[test]
    fn keyboard_creates_named_workspace_and_replaces_prefilled_folder() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/initial"));
        let mods = ModifiersState::empty();
        assert_eq!(
            form.key(&Key::Named(NamedKey::Enter), mods),
            WorkspaceFormAction::Save {
                name: "initial".into(),
                folder: "/initial".into()
            }
        );
        form.insert_text("Project");
        form.key(&Key::Named(NamedKey::Tab), mods);
        form.insert_text("/chosen folder");
        assert_eq!(
            form.key(&Key::Named(NamedKey::Enter), mods),
            WorkspaceFormAction::Save {
                name: "Project".into(),
                folder: "/chosen folder".into()
            }
        );
    }

    #[test]
    fn narrow_field_keeps_its_label_and_caret_visible_on_long_paths() {
        use unicode_width::UnicodeWidthStr;
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/worktrees/very-long-project/日本語/é"));
        form.set_focus(Control::Folder);
        assert!(form.hint().contains("All text selected"));
        form.key(&Key::Named(NamedKey::End), ModifiersState::empty());
        let end = form.visible_label(Control::Folder, 20);
        assert!(end.starts_with("Folder   …") && end.ends_with("é│"));
        assert!(end.width() <= 20);
        form.key(&Key::Named(NamedKey::Home), ModifiersState::empty());
        let start = form.visible_label(Control::Folder, 20);
        assert!(start.starts_with("Folder   │/work") && start.ends_with('…'));
        assert!(start.width() <= 20);
    }

    #[test]
    fn space_enters_text_and_activates_focused_buttons() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("."));
        form.insert_text("My");
        let space = Key::Named(NamedKey::Space);
        form.key(&space, ModifiersState::empty());
        form.insert_text("project");
        form.set_focus(Control::Save);
        assert_eq!(
            form.key(&space, ModifiersState::empty()),
            WorkspaceFormAction::Save {
                name: "My project".into(),
                folder: PathBuf::from("."),
            }
        );
        form.set_focus(Control::Cancel);
        assert_eq!(
            form.key(&space, ModifiersState::empty()),
            WorkspaceFormAction::Cancel
        );
    }

    #[test]
    fn mouse_uses_same_focus_and_actions_as_keyboard() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("."));
        for (control, rect) in controls(modal_rect(800.0, 600.0, 1.0)) {
            let action = form.click(rect.x + 1.0, rect.y + 1.0, 800.0, 600.0, 1.0);
            match control {
                Control::Name => form.insert_text("Demo"),
                Control::Folder => {
                    assert_eq!(form.focus, Control::Folder)
                }
                Control::Save => assert!(matches!(action, WorkspaceFormAction::Save { .. })),
                Control::Cancel => assert_eq!(action, WorkspaceFormAction::Cancel),
            }
        }
    }
}
