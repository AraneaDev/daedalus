use super::{Control, WorkspaceForm};
use unicode_segmentation::UnicodeSegmentation;
use winit::keyboard::{Key, ModifiersState, NamedKey};

impl WorkspaceForm {
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
        if !self.enabled(control)
            || self.value(control).is_none()
            || value.chars().any(char::is_control)
        {
            return;
        }
        self.set_focus(control);
        self.select_all = true;
        self.replace(0, 0, value);
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
        if self.focus == Control::Folder
            && self.name_is_fixed()
            && let Some((_, name)) = &self.committed_project
        {
            self.name.clone_from(name);
        }
        self.error = None;
    }

    pub fn insert_text(&mut self, text: &str) {
        if !self.submitting && !text.is_empty() && !text.chars().any(char::is_control) {
            self.replace(self.cursor, self.cursor, text);
        }
    }

    pub(super) fn edit_key(&mut self, key: &Key, mods: ModifiersState) {
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
    }
}
