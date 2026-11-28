use super::{Control, PAGE_SIZE, SettingsAction, SettingsDialog};
use crate::assignments::Assignment;
use Control::*;
use daedalus_proto::PaneRole;

#[derive(Clone, Debug, Default)]
pub(super) struct AssignmentPage {
    drafts: Vec<Draft>,
    pub(super) selected: usize,
    pub(super) argument_page: usize,
}

impl AssignmentPage {
    pub(super) fn new(entries: Vec<Assignment>) -> Self {
        Self {
            drafts: entries.into_iter().map(Draft::from).collect(),
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.drafts.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}

#[derive(Clone, Debug)]
struct Draft {
    id: String,
    name: String,
    role: PaneRole,
    executable: String,
    args: Vec<String>,
}

impl Draft {
    fn validated_assignment(&self) -> Result<Assignment, (Control, &'static str)> {
        if self.name.trim().is_empty() {
            return Err((Name, "Enter an assignment name."));
        }
        if self.executable.trim().is_empty() || self.executable.contains('\0') {
            return Err((Executable, "Enter an executable without a null character."));
        }
        if let Some(index) = self.args.iter().position(|arg| arg.contains('\0')) {
            return Err((
                Argument(index),
                "Remove the null character from this argument.",
            ));
        }
        Ok(Assignment {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role,
            argv: std::iter::once(&self.executable)
                .chain(&self.args)
                .cloned()
                .collect(),
        })
    }
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

impl SettingsDialog {
    pub(super) fn assignment_rows(&self, pages: Vec<Control>) -> Vec<Vec<Control>> {
        let mut rows = vec![
            pages,
            vec![Prev, Next, Remove],
            vec![Add, Agy, Claude, Codex],
            vec![Name],
            vec![Role],
            vec![Executable],
        ];
        let count = self
            .assignments
            .drafts
            .get(self.assignments.selected)
            .map_or(0, |d| d.args.len());
        let start = self.assignments.argument_page * PAGE_SIZE;
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

    pub(super) fn assignment_label(&self, c: Control) -> Option<String> {
        let draft = self.assignments.drafts.get(self.assignments.selected);
        Some(match c {
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
            Launch => "Save & launch".into(),
            _ => return None,
        })
    }

    pub(super) fn assignment_enabled(&self, c: Control) -> bool {
        match c {
            Prev | Next => self.assignments.drafts.len() > 1,
            PreviousArguments => self.assignments.argument_page > 0,
            NextArguments => self
                .assignments
                .drafts
                .get(self.assignments.selected)
                .is_some_and(|d| (self.assignments.argument_page + 1) * PAGE_SIZE < d.args.len()),
            Argument(i) | DropArgument(i) => self
                .assignments
                .drafts
                .get(self.assignments.selected)
                .is_some_and(|d| i < d.args.len()),
            Remove | Name | Role | Executable | AddArgument | Launch => {
                !self.assignments.drafts.is_empty()
            }
            _ => true,
        }
    }

    pub(super) fn assignment_field(&mut self) -> Option<&mut String> {
        let draft = self.assignments.drafts.get_mut(self.assignments.selected)?;
        match self.focus {
            Name => Some(&mut draft.name),
            Executable => Some(&mut draft.executable),
            Argument(i) => draft.args.get_mut(i),
            _ => None,
        }
    }

    pub(super) fn assignment_value(&self, control: Control) -> Option<&str> {
        let draft = self.assignments.drafts.get(self.assignments.selected)?;
        match control {
            Name => Some(&draft.name),
            Executable => Some(&draft.executable),
            Argument(i) => draft.args.get(i).map(String::as_str),
            _ => None,
        }
    }

    pub(super) fn apply_assignment_control(&mut self, c: Control) -> SettingsAction {
        match c {
            Prev => {
                self.assignments.selected =
                    (self.assignments.selected + self.assignments.drafts.len() - 1)
                        % self.assignments.drafts.len()
            }
            Next => {
                self.assignments.selected =
                    (self.assignments.selected + 1) % self.assignments.drafts.len()
            }
            Add | Agy | Claude | Codex => {
                let (executable, role) = match c {
                    Agy => ("agy", PaneRole::Agent),
                    Claude => ("claude", PaneRole::Agent),
                    Codex => ("codex", PaneRole::Agent),
                    _ => ("", PaneRole::Runner),
                };
                let assignment = Assignment {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: executable.into(),
                    role,
                    argv: vec![executable.into()],
                };
                self.assignments.selected = self.assignments.drafts.len();
                self.assignments.drafts.push(Draft::from(assignment));
                self.focus = Name;
                self.select_all = true;
            }
            Remove => {
                self.assignments.drafts.remove(self.assignments.selected);
                self.assignments.selected = self
                    .assignments
                    .selected
                    .min(self.assignments.drafts.len().saturating_sub(1));
                self.focus = Add;
            }
            Role => {
                let role = &mut self.assignments.drafts[self.assignments.selected].role;
                *role = match role {
                    PaneRole::Agent => PaneRole::Service,
                    PaneRole::Service => PaneRole::Runner,
                    PaneRole::Runner => PaneRole::Agent,
                };
            }
            Name | Executable | Argument(_) => self.select_all = true,
            AddArgument => {
                let args = &mut self.assignments.drafts[self.assignments.selected].args;
                let i = args.len();
                args.push(String::new());
                self.assignments.argument_page = i / PAGE_SIZE;
                self.focus = Argument(i);
            }
            DropArgument(i) => {
                let args = &mut self.assignments.drafts[self.assignments.selected].args;
                args.remove(i);
                let next = i.min(args.len().saturating_sub(1));
                self.assignments.argument_page = next / PAGE_SIZE;
                self.focus = if args.is_empty() {
                    AddArgument
                } else {
                    Argument(next)
                };
            }
            PreviousArguments | NextArguments => {
                if c == PreviousArguments {
                    self.assignments.argument_page -= 1;
                } else {
                    self.assignments.argument_page += 1;
                }
                self.focus = Argument(self.assignments.argument_page * PAGE_SIZE);
                self.select_all = true;
            }
            Save | Launch => {
                let mut values = Vec::new();
                for (i, d) in self.assignments.drafts.iter().enumerate() {
                    let assignment = match d.validated_assignment() {
                        Ok(assignment) => assignment,
                        Err((control, message)) => {
                            self.assignments.selected = i;
                            if let Argument(index) = control {
                                self.assignments.argument_page = index / PAGE_SIZE;
                            }
                            self.focus = control;
                            self.select_all = true;
                            self.invalid_field = Some(control);
                            self.error = Some(message.into());
                            return SettingsAction::None;
                        }
                    };
                    values.push(assignment);
                }
                return if c == Launch {
                    SettingsAction::Launch(
                        values,
                        self.assignments.drafts[self.assignments.selected]
                            .id
                            .clone(),
                    )
                } else {
                    SettingsAction::Save(values)
                };
            }
            _ => {}
        }
        SettingsAction::None
    }
    pub(super) fn scroll_assignments(&mut self, delta_y: f32) {
        if delta_y > 0.0 {
            self.activate(Prev);
        }
        if delta_y < 0.0 {
            self.activate(Next);
        }
    }
}
