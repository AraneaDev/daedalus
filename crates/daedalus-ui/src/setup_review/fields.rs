use super::{
    CommandLine, Control, Key, ModifiersState, NamedKey, PaneRole, ROLES, SetupReview, role_label,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default)]
pub(super) struct CommandFields {
    pub(super) values: [String; 3],
    caret: usize,
    selected: bool,
}

impl CommandFields {
    pub(super) fn new(values: [String; 3]) -> Self {
        Self {
            values,
            ..Self::default()
        }
    }
}

impl SetupReview {
    pub fn command(&self, index: usize) -> &str {
        &self.fields.values[index]
    }
    pub fn selection(&self, index: usize) -> std::ops::Range<usize> {
        if self.focus != Control::Command(index) {
            0..0
        } else if self.fields.selected {
            0..self.fields.values[index].len()
        } else {
            self.fields.caret..self.fields.caret
        }
    }
    pub fn caret(&self) -> usize {
        self.fields.caret
    }

    pub(super) fn command_line(&self) -> CommandLine {
        self.inspection.as_ref().map_or(CommandLine::Posix, |host| {
            CommandLine::for_platform(&host.platform)
        })
    }

    pub fn commands(&self) -> Result<Vec<(PaneRole, Vec<String>, bool)>, String> {
        let mut result = vec![];
        for (i, role) in ROLES.into_iter().enumerate() {
            let argv = self
                .command_line()
                .parse(&self.fields.values[i])
                .map_err(|_| format!("Close the quote in the {} command.", role_label(i)))?;
            if argv.is_empty() {
                continue;
            }
            if argv[0].is_empty() {
                return Err(format!("Enter an executable for {}.", role_label(i)));
            }
            result.push((role, argv, self.launch[i]));
        }
        Ok(result)
    }

    pub fn executables(&self) -> Vec<String> {
        let mut tools = vec![];
        for field in &self.fields.values {
            if let Some(exe) = self
                .command_line()
                .parse(field)
                .ok()
                .and_then(|args| args.into_iter().next())
                && !tools.contains(&exe)
            {
                tools.push(exe);
            }
        }
        tools
    }

    pub fn set_focus(&mut self, c: Control) {
        if !self.enabled(c) {
            return;
        }
        self.focus = c;
        self.fields.caret = if let Control::Command(i) = c {
            self.fields.values[i].len()
        } else {
            0
        };
        self.fields.selected = self.fields.caret > 0;
    }
    pub fn set_value(&mut self, i: usize, value: &str) {
        if self.busy || value.chars().any(char::is_control) {
            return;
        }
        self.tool_revision = self.tool_revision.wrapping_add(1);
        self.fields.values[i] = value.into();
        self.set_focus(Control::Command(i));
        self.fields.selected = false;
        self.feedback.clear();
    }
    fn replace(&mut self, range: std::ops::Range<usize>, text: &str) {
        if let Control::Command(i) = self.focus {
            let range = if self.fields.selected {
                0..self.fields.values[i].len()
            } else {
                range
            };
            self.fields.caret = range.start + text.len();
            self.tool_revision = self.tool_revision.wrapping_add(1);
            self.fields.values[i].replace_range(range, text);
            self.fields.selected = false;
            self.feedback.clear();
        }
    }
    pub fn insert_text(&mut self, text: &str) {
        if !self.busy && !text.is_empty() && !text.chars().any(char::is_control) {
            self.replace(self.fields.caret..self.fields.caret, text);
        }
    }
    pub(super) fn command_key(&mut self, key: &Key, mods: ModifiersState) {
        if let Control::Command(i) = self.focus {
            let value = &self.fields.values[i];
            let prev = value[..self.fields.caret]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(n, _)| n);
            let next = self.fields.caret
                + value[self.fields.caret..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len);
            match key {
                Key::Named(
                    NamedKey::Home | NamedKey::End | NamedKey::ArrowLeft | NamedKey::ArrowRight,
                ) => {
                    self.fields.caret = match key {
                        Key::Named(NamedKey::Home) => 0,
                        Key::Named(NamedKey::End) => value.len(),
                        Key::Named(NamedKey::ArrowLeft) if self.fields.selected => 0,
                        Key::Named(NamedKey::ArrowRight) if self.fields.selected => value.len(),
                        Key::Named(NamedKey::ArrowLeft) => prev,
                        _ => next,
                    };
                    self.fields.selected = false;
                }
                Key::Named(NamedKey::Backspace) => self.replace(prev..self.fields.caret, ""),
                Key::Named(NamedKey::Delete) => self.replace(self.fields.caret..next, ""),
                Key::Named(NamedKey::Space) => self.insert_text(" "),
                Key::Character(c) if mods.control_key() || mods.super_key() => {
                    if c.eq_ignore_ascii_case("a") {
                        self.fields.selected = true;
                    }
                }
                Key::Character(c) if !mods.alt_key() => self.insert_text(c),
                _ => (),
            }
        }
    }
}
