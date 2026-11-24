use crate::layout::Rect;
use crate::setup_tools::{guide_url, install_command};
use daedalus_proto::{PaneRole, SetupInspection};
use std::path::PathBuf;
use unicode_segmentation::UnicodeSegmentation;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub const ROLES: [PaneRole; 3] = [PaneRole::Agent, PaneRole::Service, PaneRole::Runner];
const AGENTS: [&str; 3] = ["codex", "claude", "agy"];

pub fn role_label(index: usize) -> &'static str {
    ["Coding agent", "Development server", "Tests"][index]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupAction {
    None,
    Back,
    Cancel,
    Check,
    Install(Vec<String>),
    Help(String),
    Open,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Control {
    Command(usize),
    Launch(usize),
    Agent,
    Tool,
    Install,
    Guide,
    Dependencies,
    Check,
    Back,
    Cancel,
    #[default]
    Open,
}

#[derive(Clone, Debug, Default)]
pub struct SetupReview {
    pub open: bool,
    pub name: String,
    pub folder: PathBuf,
    pub feedback: String,
    pub busy: bool,
    pub focus: Control,
    inspection: Option<SetupInspection>,
    fields: [String; 3],
    launch: [bool; 3],
    missing_index: usize,
    checked: Vec<String>,
    tool_revision: u64,
    caret: usize,
    selected: bool,
    revision: u64,
}

pub fn modal_rect(w: f32, h: f32, dpi: f32) -> Rect {
    let width = (w - 12. * dpi).max(1.).min(700. * dpi);
    let height = (h - 12. * dpi).max(1.).min(500. * dpi);
    Rect::new((w - width) / 2., (h - height) / 2., width, height)
}

pub fn region(m: Rect, x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(
        m.x + m.width * x,
        m.y + m.height * y,
        m.width * w,
        m.height * h,
    )
}

impl SetupReview {
    pub fn begin(&mut self, name: String, folder: PathBuf, inspection: SetupInspection) {
        let fields = ROLES.map(|role| {
            inspection
                .commands
                .iter()
                .find(|(r, _)| *r == role)
                .map(|(_, cmd)| shell_words::join(cmd))
                .unwrap_or_else(|| {
                    if role == PaneRole::Agent {
                        AGENTS
                            .iter()
                            .find(|agent| inspection.available.iter().any(|exe| exe == **agent))
                            .unwrap_or(&"codex")
                            .to_string()
                    } else {
                        String::new()
                    }
                })
        });
        *self = Self {
            open: true,
            name,
            folder,
            fields,
            launch: [true, false, false],
            inspection: Some(inspection),
            revision: self.revision.wrapping_add(1),
            ..Default::default()
        };
        self.checked = self.executables();
    }

    pub fn refresh(&mut self, inspection: SetupInspection) {
        self.checked = self.executables();
        self.inspection = Some(inspection);
        self.busy = false;
        self.revision = self.revision.wrapping_add(1);
        if !self.enabled(self.focus) {
            self.set_focus(Control::Check);
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn tool_revision(&self) -> u64 {
        self.tool_revision
    }
    pub fn host(&self) -> &str {
        self.inspection.as_ref().map_or("", |i| i.host.as_str())
    }
    pub fn command(&self, index: usize) -> &str {
        &self.fields[index]
    }
    pub fn selection(&self, index: usize) -> std::ops::Range<usize> {
        if self.focus != Control::Command(index) {
            0..0
        } else if self.selected {
            0..self.fields[index].len()
        } else {
            self.caret..self.caret
        }
    }
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn commands(&self) -> Result<Vec<(PaneRole, Vec<String>, bool)>, String> {
        let mut result = vec![];
        for (i, role) in ROLES.into_iter().enumerate() {
            let argv = shell_words::split(&self.fields[i])
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
        for field in &self.fields {
            if let Some(exe) = shell_words::split(field)
                .ok()
                .and_then(|args| args.into_iter().next())
                && !tools.contains(&exe)
            {
                tools.push(exe);
            }
        }
        tools
    }

    fn missing(&self) -> Vec<String> {
        self.executables()
            .into_iter()
            .filter(|e| !self.available(e))
            .collect()
    }
    fn available(&self, exe: &str) -> bool {
        self.inspection
            .as_ref()
            .is_some_and(|i| i.available.iter().any(|a| a == exe))
    }
    fn tool(&self) -> Option<String> {
        let missing = self.missing();
        missing
            .get(self.missing_index % missing.len().max(1))
            .cloned()
    }
    fn install(&self) -> Option<Vec<String>> {
        install_command(&self.tool()?, self.inspection.as_ref()?)
    }
    fn guide(&self) -> Option<&'static str> {
        guide_url(&self.tool()?)
    }
    pub fn detail(&self) -> String {
        if let Some(command) = self.install() {
            return format!("Install command   {}", shell_words::join(command));
        }
        if self.tool().is_some() {
            return "Install on the daemon host, then choose Check again.".into();
        }
        "Tools are ready. Choose which commands to start when the workspace opens.".into()
    }

    pub fn status(&self, index: usize) -> &'static str {
        match shell_words::split(self.command(index)) {
            Err(_) => "Incomplete command",
            Ok(args) if args.is_empty() => "Optional",
            Ok(args) if self.available(&args[0]) => "Installed",
            Ok(args) if AGENTS.contains(&args[0].as_str()) || self.checked.contains(&args[0]) => {
                "Missing"
            }
            _ => "Needs check",
        }
    }

    pub fn controls(&self, m: Rect) -> Vec<(Control, Rect)> {
        let mut rows = Vec::new();
        for i in 0..3 {
            let y = 0.225 + i as f32 * 0.135;
            rows.push((Control::Command(i), region(m, 0.035, y, 0.70, 0.068)));
            rows.push((Control::Launch(i), region(m, 0.753, y, 0.212, 0.068)));
        }
        rows.push((Control::Agent, region(m, 0.69, 0.153, 0.275, 0.056)));
        if self.tool().is_some() {
            rows.push((Control::Tool, region(m, 0.035, 0.615, 0.38, 0.071)));
            if self.install().is_some() {
                rows.push((Control::Install, region(m, 0.43, 0.615, 0.24, 0.071)));
            }
            if self.guide().is_some() {
                rows.push((Control::Guide, region(m, 0.69, 0.615, 0.275, 0.071)));
            }
        }
        if self.dependency_install().is_some() {
            rows.push((Control::Dependencies, region(m, 0.035, 0.747, 0.93, 0.058)));
        }
        for (c, x, w) in [
            (Control::Back, 0.035, 0.135),
            (Control::Cancel, 0.183, 0.155),
            (Control::Check, 0.355, 0.245),
            (Control::Open, 0.62, 0.345),
        ] {
            rows.push((c, region(m, x, 0.875, w, 0.09)));
        }
        rows
    }

    fn dependency_install(&self) -> Option<&Vec<String>> {
        self.inspection.as_ref()?.dependency_install.as_ref()
    }

    pub fn label(&self, c: Control) -> String {
        match c {
            Control::Command(i) => self.command(i).into(),
            Control::Launch(i) => if self.launch[i] {
                "Start on open"
            } else {
                "On demand"
            }
            .into(),
            Control::Agent => "Change agent".into(),
            Control::Tool => format!(
                "{}{}",
                self.tool().unwrap_or_default(),
                if self.missing().len() > 1 {
                    "  ›"
                } else {
                    ""
                }
            ),
            Control::Install => "Install here".into(),
            Control::Guide => "Official guide".into(),
            Control::Dependencies => format!(
                "Install project dependencies   {}",
                self.dependency_install()
                    .map(shell_words::join)
                    .unwrap_or_default()
            ),
            Control::Back => "Back".into(),
            Control::Cancel => "Cancel".into(),
            Control::Check => "Check again".into(),
            Control::Open => "Open workspace".into(),
        }
    }

    pub fn enabled(&self, c: Control) -> bool {
        if self.busy {
            return false;
        }
        match c {
            Control::Launch(i) => !self.command(i).trim().is_empty(),
            Control::Dependencies => self
                .dependency_install()
                .and_then(|cmd| cmd.first())
                .is_some_and(|exe| self.available(exe)),
            Control::Tool => self.missing().len() > 1,
            Control::Guide => self.guide().is_some(),
            Control::Install => self.install().is_some(),
            _ => true,
        }
    }

    pub fn set_focus(&mut self, c: Control) {
        if !self.enabled(c) {
            return;
        }
        self.focus = c;
        self.caret = if let Control::Command(i) = c {
            self.fields[i].len()
        } else {
            0
        };
        self.selected = self.caret > 0;
    }
    pub fn set_value(&mut self, i: usize, value: &str) {
        if self.busy || value.chars().any(char::is_control) {
            return;
        }
        self.tool_revision = self.tool_revision.wrapping_add(1);
        self.fields[i] = value.into();
        self.set_focus(Control::Command(i));
        self.selected = false;
        self.feedback.clear();
    }
    fn replace(&mut self, range: std::ops::Range<usize>, text: &str) {
        if let Control::Command(i) = self.focus {
            let range = if self.selected {
                0..self.fields[i].len()
            } else {
                range
            };
            self.caret = range.start + text.len();
            self.tool_revision = self.tool_revision.wrapping_add(1);
            self.fields[i].replace_range(range, text);
            self.selected = false;
            self.feedback.clear();
        }
    }
    pub fn insert_text(&mut self, text: &str) {
        if !self.busy && !text.is_empty() && !text.chars().any(char::is_control) {
            self.replace(self.caret..self.caret, text);
        }
    }
    pub fn activate(&mut self, c: Control) -> SetupAction {
        if !self.enabled(c) {
            return SetupAction::None;
        }
        self.set_focus(c);
        match c {
            Control::Launch(i) => self.launch[i] = !self.launch[i],
            Control::Agent => {
                let mut choices = vec![];
                if let Some(report) = &self.inspection
                    && let Some((_, command)) = report
                        .commands
                        .iter()
                        .find(|(role, _)| *role == PaneRole::Agent)
                {
                    choices.push(shell_words::join(command));
                }
                for agent in AGENTS.into_iter().chain(
                    ["sh", "cmd"]
                        .into_iter()
                        .filter(|shell| self.available(shell)),
                ) {
                    if !choices.iter().any(|choice| choice == agent) {
                        choices.push(agent.to_owned());
                    }
                }
                let index = choices
                    .iter()
                    .position(|a| *a == self.fields[0])
                    .map_or(0, |i| (i + 1) % choices.len());
                self.tool_revision = self.tool_revision.wrapping_add(1);
                self.fields[0] = choices.swap_remove(index);
                self.feedback.clear();
            }
            Control::Tool => {
                self.tool_revision = self.tool_revision.wrapping_add(1);
                self.missing_index = self.missing_index.wrapping_add(1);
            }
            Control::Install => {
                if let Some(cmd) = self.install() {
                    return SetupAction::Install(cmd);
                }
            }
            Control::Guide => {
                if let Some(url) = self.guide() {
                    return SetupAction::Help(url.into());
                }
            }
            Control::Dependencies => {
                if let Some(cmd) = self.dependency_install() {
                    return SetupAction::Install(cmd.clone());
                }
            }
            Control::Back => return SetupAction::Back,
            Control::Cancel => return SetupAction::Cancel,
            Control::Check => return SetupAction::Check,
            Control::Open => match self.commands() {
                Err(e) => self.feedback = e,
                Ok(rows) => {
                    if let Some((role, argv, _)) = rows
                        .iter()
                        .find(|(_, a, start)| *start && !self.available(&a[0]))
                    {
                        self.feedback = format!("Check {} or set {} to On demand.", argv[0], role);
                    } else {
                        return SetupAction::Open;
                    }
                }
            },
            Control::Command(_) => {}
        }
        SetupAction::None
    }

    pub fn click(&mut self, x: f32, y: f32, w: f32, h: f32, scale: f32) -> SetupAction {
        self.controls(modal_rect(w, h, scale))
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map_or(SetupAction::None, |(c, _)| self.activate(c))
    }

    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> SetupAction {
        if self.busy {
            return SetupAction::None;
        }
        match key {
            Key::Named(NamedKey::Escape) => return SetupAction::Cancel,
            Key::Named(NamedKey::Tab) => {
                let list: Vec<_> = self
                    .controls(Rect::new(0., 0., 1., 1.))
                    .into_iter()
                    .map(|(c, _)| c)
                    .filter(|c| self.enabled(*c))
                    .collect();
                let p = list.iter().position(|c| *c == self.focus).unwrap_or(0);
                self.set_focus(
                    list[(p + if mods.shift_key() { list.len() - 1 } else { 1 }) % list.len()],
                );
            }
            Key::Named(NamedKey::Enter) if matches!(self.focus, Control::Command(_)) => {
                return self.activate(Control::Check);
            }
            Key::Named(NamedKey::Enter) => return self.activate(self.focus),
            Key::Named(NamedKey::Space) if !matches!(self.focus, Control::Command(_)) => {
                return self.activate(self.focus);
            }
            _ => {
                if let Control::Command(i) = self.focus {
                    let value = &self.fields[i];
                    let prev = value[..self.caret]
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(0, |(n, _)| n);
                    let next = self.caret
                        + value[self.caret..]
                            .graphemes(true)
                            .next()
                            .map_or(0, str::len);
                    match key {
                        Key::Named(
                            NamedKey::Home
                            | NamedKey::End
                            | NamedKey::ArrowLeft
                            | NamedKey::ArrowRight,
                        ) => {
                            self.caret = match key {
                                Key::Named(NamedKey::Home) => 0,
                                Key::Named(NamedKey::End) => value.len(),
                                Key::Named(NamedKey::ArrowLeft) if self.selected => 0,
                                Key::Named(NamedKey::ArrowRight) if self.selected => value.len(),
                                Key::Named(NamedKey::ArrowLeft) => prev,
                                _ => next,
                            };
                            self.selected = false;
                        }
                        Key::Named(NamedKey::Backspace) => self.replace(prev..self.caret, ""),
                        Key::Named(NamedKey::Delete) => self.replace(self.caret..next, ""),
                        Key::Named(NamedKey::Space) => self.insert_text(" "),
                        Key::Character(c) if mods.control_key() || mods.super_key() => {
                            if c.eq_ignore_ascii_case("a") {
                                self.selected = true;
                            }
                        }
                        Key::Character(c) if !mods.alt_key() => self.insert_text(c),
                        _ => (),
                    }
                }
            }
        }
        SetupAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inspection() -> SetupInspection {
        SetupInspection {
            commands: vec![
                (
                    PaneRole::Service,
                    vec!["npm".into(), "run".into(), "serve app".into()],
                ),
                (PaneRole::Runner, vec!["npm".into(), "test".into()]),
            ],
            available: vec!["claude".into(), "npm".into()],
            host: "development".into(),
            platform: "linux".into(),
            dependency_install: Some(vec!["npm".into(), "ci".into()]),
        }
    }

    #[test]
    fn review_round_trips_arguments_and_only_starts_the_agent_by_default() {
        let mut review = SetupReview::default();
        review.begin("App".into(), "/app".into(), inspection());
        let commands = review.commands().unwrap();
        assert_eq!(commands[0], (PaneRole::Agent, vec!["claude".into()], true));
        assert_eq!(
            commands[1],
            (
                PaneRole::Service,
                vec!["npm".into(), "run".into(), "serve app".into()],
                false
            )
        );
        assert!(!commands[2].2);
        assert_eq!(review.activate(Control::Open), SetupAction::Open);
        let deps = review.activate(Control::Dependencies);
        assert_eq!(deps, SetupAction::Install(vec!["npm".into(), "ci".into()]));
    }

    #[test]
    fn cycling_agent_presets_can_restore_the_full_project_command() {
        let mut host = inspection();
        host.commands
            .push((PaneRole::Agent, vec!["sh".into(), "-l".into()]));
        let mut review = SetupReview::default();
        review.begin("App".into(), "/app".into(), host);
        let initial = review.command(0).to_string();
        for agent in ["codex", "claude", "agy", initial.as_str()] {
            review.activate(Control::Agent);
            assert_eq!(review.command(0), agent);
        }
    }

    #[test]
    fn editable_commands_and_launch_choices_survive_host_rechecks() {
        let mut review = SetupReview::default();
        review.begin("App".into(), "/app".into(), inspection());
        review.set_value(1, "'custom executable' 'my folder'");
        review.activate(Control::Launch(1));
        assert_eq!(review.activate(Control::Open), SetupAction::None);
        assert!(review.feedback.contains("custom executable"));
        let mut result = inspection();
        result.available.push("custom executable".into());
        review.refresh(result);
        assert_eq!(
            review.commands().unwrap()[1],
            (
                PaneRole::Service,
                vec!["custom executable".into(), "my folder".into()],
                true
            )
        );
        assert_eq!(review.activate(Control::Open), SetupAction::Open);
        review.set_value(2, "npm '");
        assert_eq!(review.activate(Control::Open), SetupAction::None);
        assert!(review.feedback.contains("quote"));
    }
    #[test]
    fn missing_tool_shared_by_two_roles_has_one_installation_choice() {
        let mut review = SetupReview::default();
        let mut host = inspection();
        host.available.retain(|exe| exe != "npm");
        review.begin("App".into(), "/app".into(), host);
        assert_eq!(review.missing(), ["npm"]);
        assert!(!review.enabled(Control::Tool));
        assert!(!review.enabled(Control::Dependencies));
    }
}
