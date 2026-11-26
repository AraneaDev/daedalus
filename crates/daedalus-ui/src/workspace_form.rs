mod fields;
mod layout;

use daedalus_proto::RecentProject;
use std::path::{Path, PathBuf};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug, PartialEq, Eq)]
pub enum WorkspaceFormAction {
    None,
    Cancel,
    Save { name: String, folder: PathBuf },
    Reopen(RecentProject),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    Recent(usize),
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

/// The dialog shows this many recent projects; the rest stay available to `daedalus-ctl`.
pub const MAX_RECENT_ROWS: usize = 5;

pub(crate) fn relative_time(then: u64, now: u64) -> String {
    let age = now.saturating_sub(then);
    let count = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match age {
        0..60 => "just now".into(),
        60..3_600 => count(age / 60, "minute"),
        3_600..86_400 => count(age / 3_600, "hour"),
        86_400..172_800 => "yesterday".into(),
        _ => count(age / 86_400, "day"),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
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
    committed_project: Option<(String, String)>,
    cursor: usize,
    select_all: bool,
    recent: Vec<RecentProject>,
    /// Folders that are open as a workspace; they are never offered for reopening.
    open_roots: Vec<PathBuf>,
}

impl WorkspaceForm {
    pub fn begin(&mut self, folder: &Path) {
        *self = Self {
            open: true,
            session: self.session.wrapping_add(1),
            folder: folder.to_string_lossy().into_owned(),
            recent: std::mem::take(&mut self.recent),
            open_roots: std::mem::take(&mut self.open_roots),
            ..Default::default()
        };
    }

    /// Records which folders are open as workspaces and drops their rows from the list now
    /// and from every list set later, so an open project is never reopened a second time.
    pub fn hide_open(&mut self, open_roots: Vec<PathBuf>) {
        self.open_roots = open_roots;
        let kept = std::mem::take(&mut self.recent);
        self.set_recent(kept);
    }

    /// Replaces the recent projects shown above the form, most recent first.
    pub fn set_recent(&mut self, mut projects: Vec<RecentProject>) {
        projects.retain(|project| !self.open_roots.contains(&project.root_dir));
        projects.truncate(MAX_RECENT_ROWS);
        self.recent = projects;
        if let Control::Recent(i) = self.focus
            && i >= self.recent.len()
        {
            self.focus = match self.recent.len() {
                0 if self.name_is_fixed() => Control::Folder,
                0 => Control::Name,
                len => Control::Recent(len - 1),
            };
            self.cursor = self.value(self.focus).map_or(0, str::len);
            self.select_all = false;
        }
    }

    fn order(&self) -> Vec<Control> {
        (0..self.recent.len())
            .map(Control::Recent)
            .chain(CONTROLS)
            .collect()
    }

    pub fn heading(&self) -> &'static str {
        if self.recent.is_empty() {
            "Set up workspace · Choose project"
        } else {
            "Set up a new project"
        }
    }

    /// One recent row: name, folder (shortened from the left to fit) and when it was last used.
    pub(crate) fn recent_label(&self, index: usize, columns: usize, now: u64) -> String {
        use crate::renderer::text::{truncate_left_with_ellipsis, truncate_right_with_ellipsis};
        use unicode_width::UnicodeWidthStr;
        let Some(project) = self.recent.get(index) else {
            return String::new();
        };
        let prefix = format!("{}   ", project.name);
        let suffix = format!("   {}", relative_time(project.last_used, now));
        let folder = project.root_dir.to_string_lossy();
        let available = columns.saturating_sub(prefix.width() + suffix.width());
        let folder = truncate_left_with_ellipsis(&folder, available);
        truncate_right_with_ellipsis(&format!("{prefix}{folder}{suffix}"), columns)
    }

    pub fn recent(&self) -> &[RecentProject] {
        &self.recent
    }

    pub fn revisit(&mut self, folder: &Path, name: &str, created: bool) {
        self.begin(folder);
        self.name = name.into();
        if created {
            self.committed_project = Some((self.folder.clone(), name.into()));
        }
        self.set_focus(Control::Folder);
    }

    pub fn name_is_fixed(&self) -> bool {
        self.committed_project
            .as_ref()
            .is_some_and(|(folder, _)| Path::new(folder) == Path::new(&self.folder))
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
        let available = match control {
            Control::Name => !self.name_is_fixed(),
            Control::Recent(i) => i < self.recent.len(),
            _ => true,
        };
        available && (!self.submitting || control == Control::Cancel)
    }

    pub fn set_focus(&mut self, control: Control) {
        if !self.enabled(control) {
            return;
        }
        self.focus = control;
        self.cursor = self.value(self.focus).map_or(0, str::len);
        self.select_all = self.value(self.focus).is_some() && self.cursor > 0;
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
        let Some((control, _)) = self
            .controls(self.modal_rect(w, h, dpi))
            .into_iter()
            .find(|(_, rect)| rect.contains(x, y))
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
            Control::Recent(i) => WorkspaceFormAction::Reopen(self.recent[i].clone()),
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
        match key {
            Key::Named(NamedKey::Escape) => return WorkspaceFormAction::Cancel,
            Key::Named(NamedKey::Enter) if self.focus == Control::Cancel => {
                return WorkspaceFormAction::Cancel;
            }
            Key::Named(NamedKey::Enter) if matches!(self.focus, Control::Recent(_)) => {
                return self.activate(self.focus);
            }
            Key::Named(NamedKey::Enter) => return self.submit(),
            Key::Named(NamedKey::Space) => match self.focus {
                Control::Cancel => return WorkspaceFormAction::Cancel,
                Control::Save => return self.submit(),
                Control::Recent(_) => return self.activate(self.focus),
                Control::Name | Control::Folder => self.insert_text(" "),
            },
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                // Up and Down move within the recent list and stop at its ends.
                if let Control::Recent(i) = self.focus {
                    let last = self.recent.len().saturating_sub(1);
                    self.set_focus(Control::Recent(match key {
                        Key::Named(NamedKey::ArrowUp) => i.saturating_sub(1),
                        _ => (i + 1).min(last),
                    }));
                }
            }
            Key::Named(NamedKey::Tab) => {
                let order = self.order();
                let i = order.iter().position(|c| *c == self.focus).unwrap_or(0);
                let step = if mods.shift_key() { order.len() - 1 } else { 1 };
                if let Some(control) = (1..=order.len())
                    .map(|offset| order[(i + offset * step) % order.len()])
                    .find(|control| self.enabled(*control))
                {
                    self.set_focus(control);
                }
            }
            _ => self.edit_key(key, mods),
        }
        WorkspaceFormAction::None
    }

    pub(crate) fn visible_label(&self, control: Control, columns: usize) -> String {
        let (label, text) = match control {
            Control::Name if self.name_is_fixed() => ("Workspace name", &self.name),
            Control::Name => ("Name (optional)", &self.name),
            Control::Folder => ("Folder", &self.folder),
            Control::Cancel if self.submitting => return "Dismiss".into(),
            Control::Cancel => return "Cancel".into(),
            Control::Save => return "Review environment".into(),
            Control::Recent(i) => return self.recent_label(i, columns, unix_now()),
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
        } else if matches!(self.focus, Control::Recent(_)) {
            "Enter opens this project · Tab to move · Esc to cancel"
        } else {
            "Tab to move · Esc to cancel"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;

    #[test]
    fn returning_to_a_created_workspace_preserves_its_committed_name() {
        let mut form = WorkspaceForm::default();
        form.revisit(Path::new("/project"), "Existing", true);
        assert!(!form.enabled(Control::Name));
        form.set_value(Control::Name, "Duplicate");
        form.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(form.focus, Control::Save);
        assert_eq!(
            form.activate(Control::Save),
            WorkspaceFormAction::Save {
                name: "Existing".into(),
                folder: "/project".into()
            }
        );
        form.set_value(Control::Folder, "/alternative");
        form.set_value(Control::Name, "Different");
        assert_eq!(form.value(Control::Name), Some("Different"));
        form.set_value(Control::Folder, "/project");
        assert_eq!(form.value(Control::Name), Some("Existing"));
        assert!(!form.enabled(Control::Name));
        form.revisit(Path::new("/project"), "Draft", false);
        assert!(form.enabled(Control::Name));
    }

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

    fn project(name: &str, root: &str, last_used: u64) -> RecentProject {
        RecentProject {
            name: name.into(),
            root_dir: root.into(),
            last_used,
            has_setup: true,
        }
    }

    fn two_projects() -> Vec<RecentProject> {
        vec![
            project("daedalus", "/home/tim/daedalus", 1_000),
            project("notes", "/home/tim/notes", 500),
        ]
    }

    #[test]
    fn recent_rows_come_first_in_focus_order() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/home/dev"));
        form.set_recent(two_projects());
        let modal = form.modal_rect(800.0, 600.0, 1.0);
        let order: Vec<_> = form.controls(modal).into_iter().map(|(c, _)| c).collect();
        assert_eq!(
            order,
            [
                Control::Recent(0),
                Control::Recent(1),
                Control::Name,
                Control::Folder,
                Control::Cancel,
                Control::Save
            ]
        );
        let rects: Vec<_> = form.controls(modal).into_iter().map(|(_, r)| r).collect();
        assert!(rects[0].bottom() <= rects[1].y && rects[1].bottom() <= rects[2].y);
        let mods = ModifiersState::empty();
        form.set_focus(Control::Name);
        form.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(form.focus, Control::Recent(1));
        form.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(form.focus, Control::Recent(0));
        form.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(form.focus, Control::Save);
        form.key(&Key::Named(NamedKey::Tab), mods);
        assert_eq!(form.focus, Control::Recent(0));
        form.key(&Key::Named(NamedKey::ArrowUp), mods);
        assert_eq!(form.focus, Control::Recent(0));
        form.key(&Key::Named(NamedKey::ArrowDown), mods);
        assert_eq!(form.focus, Control::Recent(1));
        form.key(&Key::Named(NamedKey::ArrowDown), mods);
        assert_eq!(form.focus, Control::Recent(1));
        form.key(&Key::Named(NamedKey::ArrowUp), mods);
        assert_eq!(form.focus, Control::Recent(0));
        form.insert_text("typed");
        assert_eq!(form.value(Control::Name), Some(""));
        form.key(&Key::Named(NamedKey::ArrowDown), mods);
        form.key(&Key::Named(NamedKey::Tab), mods);
        assert_eq!(form.focus, Control::Name);
        // A refreshed, shorter list never leaves focus on a row that is gone.
        form.set_focus(Control::Recent(1));
        form.set_recent(two_projects()[..1].to_vec());
        assert_eq!(form.focus, Control::Recent(0));
        form.set_recent(Vec::new());
        assert_eq!(form.focus, Control::Name);
    }

    #[test]
    fn open_folders_are_hidden_from_the_recent_list() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/home/dev"));
        form.set_recent(two_projects());
        // A project reopened while the list was stale disappears at once.
        form.hide_open(vec![PathBuf::from("/home/tim/daedalus")]);
        assert_eq!(form.recent.len(), 1);
        assert_eq!(form.recent[0].name, "notes");
        // A refresh that still lists it (the daemon filters on its own timing) stays hidden,
        // also after the form is opened again.
        form.set_recent(two_projects());
        assert_eq!(form.recent.len(), 1);
        form.begin(Path::new("/home/dev"));
        form.set_recent(two_projects());
        assert_eq!(form.recent.len(), 1);
        // Closing the workspace makes it eligible again.
        form.hide_open(Vec::new());
        form.set_recent(two_projects());
        assert_eq!(form.recent.len(), 2);
    }

    #[test]
    fn enter_on_recent_row_reopens() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/home/dev"));
        form.set_recent(two_projects());
        let mods = ModifiersState::empty();
        form.set_focus(Control::Recent(1));
        assert_eq!(
            form.key(&Key::Named(NamedKey::Enter), mods),
            WorkspaceFormAction::Reopen(two_projects()[1].clone())
        );
        assert_eq!(
            form.key(&Key::Named(NamedKey::Space), mods),
            WorkspaceFormAction::Reopen(two_projects()[1].clone())
        );
        let (_, row) = form.controls(form.modal_rect(800.0, 600.0, 1.0))[0];
        assert_eq!(
            form.click(row.x + 1.0, row.y + 1.0, 800.0, 600.0, 1.0),
            WorkspaceFormAction::Reopen(two_projects()[0].clone())
        );
        assert_eq!(form.focus, Control::Recent(0));
        form.start_submission();
        assert!(!form.enabled(Control::Recent(0)));
        assert_eq!(form.activate(Control::Recent(0)), WorkspaceFormAction::None);
        // Reopening keeps the list when the form is opened again later.
        form.open = false;
        form.begin(Path::new("/home/dev"));
        assert_eq!(form.recent().len(), 2);
    }

    #[test]
    fn empty_recent_list_keeps_todays_form() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("/home/dev"));
        form.set_recent(Vec::new());
        let modal = form.modal_rect(800.0, 600.0, 1.0);
        assert_eq!(modal, Rect::new(140.0, 182.0, 520.0, 236.0));
        let controls = form.controls(modal);
        assert_eq!(controls.len(), 4);
        let (control, name) = controls[0];
        assert_eq!(control, Control::Name);
        for (got, want) in [
            (name.x, 160.8),
            (name.y, 231.56),
            (name.width, 478.4),
            (name.height, 42.48),
        ] {
            assert!((got - want).abs() < 0.01, "{got} != {want}");
        }
        assert_eq!(form.heading(), "Set up workspace · Choose project");
        form.set_recent(two_projects());
        assert_eq!(form.heading(), "Set up a new project");
        let taller = form.modal_rect(800.0, 600.0, 1.0);
        assert!(taller.height > modal.height);
        // The new-project block keeps its proportions under the list.
        let name = form.controls(taller)[2].1;
        assert!((name.height - 42.48).abs() < 0.01);
        // A small window shrinks the dialog instead of pushing it off screen.
        let small = form.modal_rect(320.0, 240.0, 1.0);
        assert!(small.y >= 0.0 && small.bottom() <= 240.0);
        assert!(
            form.controls(small)
                .iter()
                .all(|(_, r)| r.bottom() <= small.bottom() + 0.01)
        );
    }

    #[test]
    fn recent_rows_name_project_folder_and_when_it_was_used() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new(""));
        form.set_recent(two_projects());
        assert_eq!(
            form.recent_label(0, 60, 1_000 + 2 * 3600 + 5),
            "daedalus   /home/tim/daedalus   2 hours ago"
        );
        let narrow = form.recent_label(0, 30, 1_000);
        assert!(narrow.starts_with("daedalus   …") && narrow.ends_with("   just now"));
        assert!(narrow.chars().count() <= 30);
        for (age, text) in [
            (59, "just now"),
            (60, "1 minute ago"),
            (125, "2 minutes ago"),
            (3_600, "1 hour ago"),
            (86_400, "yesterday"),
            (3 * 86_400, "3 days ago"),
        ] {
            assert_eq!(relative_time(10_000_000 - age, 10_000_000), text);
        }
        assert_eq!(relative_time(20, 10), "just now");
        assert_eq!(form.hint(), "Tab to move · Esc to cancel");
        form.set_focus(Control::Recent(0));
        assert_eq!(
            form.hint(),
            "Enter opens this project · Tab to move · Esc to cancel"
        );
        assert!(
            form.visible_label(Control::Recent(0), 80)
                .starts_with("daedalus   /home/tim/daedalus   ")
        );
    }

    #[test]
    fn mouse_uses_same_focus_and_actions_as_keyboard() {
        let mut form = WorkspaceForm::default();
        form.begin(Path::new("."));
        for (control, rect) in form.controls(form.modal_rect(800.0, 600.0, 1.0)) {
            let action = form.click(rect.x + 1.0, rect.y + 1.0, 800.0, 600.0, 1.0);
            match control {
                Control::Name => form.insert_text("Demo"),
                Control::Folder => {
                    assert_eq!(form.focus, Control::Folder)
                }
                Control::Save => assert!(matches!(action, WorkspaceFormAction::Save { .. })),
                Control::Cancel => assert_eq!(action, WorkspaceFormAction::Cancel),
                Control::Recent(_) => unreachable!("no recent projects in this form"),
            }
        }
    }
}
