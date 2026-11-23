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
pub(crate) enum Control {
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

pub(crate) fn controls(modal: Rect) -> impl Iterator<Item = (Control, Rect)> {
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
    pub(crate) focus: Control,
    name: String,
    folder: String,
    cursor: usize,
    select_all: bool,
}

impl WorkspaceForm {
    pub fn begin(&mut self, folder: &Path) {
        *self = Self {
            open: true,
            folder: folder.to_string_lossy().into_owned(),
            ..Default::default()
        };
    }

    fn field(&self) -> Option<&str> {
        match self.focus {
            Control::Name => Some(&self.name),
            Control::Folder => Some(&self.folder),
            _ => None,
        }
    }

    fn set_focus(&mut self, control: Control) {
        self.focus = control;
        self.cursor = self.field().map_or(0, str::len);
        self.select_all = self.field().is_some() && self.cursor > 0;
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
        if !text.is_empty() && !text.chars().any(char::is_control) {
            self.replace(self.cursor, self.cursor, text);
        }
    }

    fn submit(&mut self) -> WorkspaceFormAction {
        if self.name.trim().is_empty() {
            self.set_focus(Control::Name);
            self.error = Some("Enter a workspace name.".into());
        } else if self.folder.trim().is_empty() {
            self.set_focus(Control::Folder);
            self.error = Some("Enter the workspace folder.".into());
        } else {
            return WorkspaceFormAction::Save {
                name: self.name.trim().into(),
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
        self.set_focus(control);
        match control {
            Control::Cancel => WorkspaceFormAction::Cancel,
            Control::Save => self.submit(),
            _ => WorkspaceFormAction::None,
        }
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> WorkspaceFormAction {
        let field = self.field().unwrap_or("");
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

    pub(crate) fn label(&self, control: Control) -> String {
        let (label, text) = match control {
            Control::Name => ("Name", &self.name),
            Control::Folder => ("Folder", &self.folder),
            Control::Cancel => return "Cancel".into(),
            Control::Save => return "Create workspace".into(),
        };
        if control == self.focus && !self.select_all {
            let (before, after) = text.split_at(self.cursor);
            format!("{label}   {before}│{after}")
        } else {
            format!("{label}   {text}")
        }
    }

    pub fn accessibility_description(&self) -> String {
        let focus = match self.focus {
            Control::Name => "Name",
            Control::Folder => "Folder",
            Control::Cancel => "Cancel",
            Control::Save => "Create workspace",
        };
        format!(
            "New workspace. Name: {}. Folder: {}. Focus: {focus}. {}",
            self.name,
            self.folder,
            self.error
                .as_deref()
                .unwrap_or("Tab changes focus. Enter creates the workspace. Escape cancels.")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_creates_named_workspace_and_replaces_prefilled_folder() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/initial"));
        let mods = ModifiersState::empty();
        assert_eq!(
            form.key(&Key::Named(NamedKey::Enter), mods),
            WorkspaceFormAction::None
        );
        assert!(form.error.is_some());
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
    fn mouse_uses_same_focus_and_actions_as_keyboard() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("."));
        for (control, rect) in controls(modal_rect(800.0, 600.0, 1.0)) {
            let action = form.click(rect.x + 1.0, rect.y + 1.0, 800.0, 600.0, 1.0);
            match control {
                Control::Name => form.insert_text("Demo"),
                Control::Folder => {
                    assert!(form.accessibility_description().contains("Focus: Folder"))
                }
                Control::Save => assert!(matches!(action, WorkspaceFormAction::Save { .. })),
                Control::Cancel => assert_eq!(action, WorkspaceFormAction::Cancel),
            }
        }
    }
}
