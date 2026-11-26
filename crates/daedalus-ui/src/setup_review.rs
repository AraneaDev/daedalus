mod command_line;
mod controls;
mod fields;

use crate::layout::Rect;
use crate::setup_tools::{guide_url, install_command};
use command_line::CommandLine;
pub use controls::{modal_rect, region};
use daedalus_proto::{PaneRole, SetupInspection};
use fields::CommandFields;
use std::path::PathBuf;
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
    fields: CommandFields,
    launch: [bool; 3],
    missing_index: usize,
    checked: Vec<String>,
    tool_revision: u64,
    revision: u64,
}

impl SetupReview {
    pub fn begin(&mut self, name: String, folder: PathBuf, inspection: SetupInspection) {
        let fields = ROLES.map(|role| {
            inspection
                .commands
                .iter()
                .find(|(r, _)| *r == role)
                .map(|(_, cmd)| CommandLine::for_platform(&inspection.platform).format(cmd))
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
            fields: CommandFields::new(fields),
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
            return format!("Install command   {}", self.command_line().format(&command));
        }
        if self.tool().is_some() {
            return "Install on the daemon host, then choose Check again.".into();
        }
        "Tools are ready. Choose which commands to start when the workspace opens.".into()
    }

    pub fn status(&self, index: usize) -> &'static str {
        match self.command_line().parse(self.command(index)) {
            Err(_) => "Incomplete command",
            Ok(args) if args.is_empty() => "Optional",
            Ok(args) if self.available(&args[0]) => "Installed",
            Ok(args) if AGENTS.contains(&args[0].as_str()) || self.checked.contains(&args[0]) => {
                "Missing"
            }
            _ => "Needs check",
        }
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
                    .map(|argv| self.command_line().format(argv))
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
                    choices.push(self.command_line().format(command));
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
                    .position(|a| *a == self.fields.values[0])
                    .map_or(0, |i| (i + 1) % choices.len());
                self.tool_revision = self.tool_revision.wrapping_add(1);
                self.fields.values[0] = choices.swap_remove(index);
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
                    .ordered_controls()
                    .into_iter()
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
            _ => self.command_key(key, mods),
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
    fn windows_daemon_paths_survive_edit_probe_and_launch() {
        let mut host = inspection();
        host.platform = "windows".into();
        let exe = r"C:\Tools\agent.exe";
        host.available.push(exe.into());
        let mut review = SetupReview::default();
        review.begin("App".into(), "C:\\App".into(), host);
        review.set_value(
            0,
            r#"C:\Tools\agent.exe --out "C:\work tree\logs" C:\tmp\data"#,
        );
        assert_eq!(review.status(0), "Installed");
        assert!(review.executables().contains(&exe.to_string()));
        assert_eq!(
            review.commands().unwrap()[0].1,
            [exe, "--out", r"C:\work tree\logs", r"C:\tmp\data"]
        );
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

    #[test]
    fn keyboard_edits_whole_graphemes_and_checks_the_edited_command() {
        let mut review = SetupReview::default();
        review.begin("demo".into(), "/repo".into(), inspection());
        review.set_focus(Control::Command(1));
        let named =
            |review: &mut SetupReview, key| review.key(&Key::Named(key), ModifiersState::empty());
        review.insert_text("a🇳🇱é終");
        named(&mut review, NamedKey::Home);
        named(&mut review, NamedKey::ArrowRight);
        named(&mut review, NamedKey::Delete);
        assert_eq!(review.command(1), "aé終");
        named(&mut review, NamedKey::End);
        named(&mut review, NamedKey::ArrowLeft);
        named(&mut review, NamedKey::Backspace);
        assert_eq!(review.command(1), "a終");
        named(&mut review, NamedKey::Space);
        assert_eq!(review.command(1), "a 終");
        review.key(&Key::Character("a".into()), ModifiersState::CONTROL);
        assert_eq!(review.selection(1), 0..review.command(1).len());
        review.key(&Key::Character("npm".into()), ModifiersState::empty());
        assert_eq!(review.command(1), "npm");
        assert_eq!(named(&mut review, NamedKey::Enter), SetupAction::Check);
        review.busy = true;
        assert_eq!(named(&mut review, NamedKey::Escape), SetupAction::None);
        review.busy = false;
        assert_eq!(named(&mut review, NamedKey::Escape), SetupAction::Cancel);
    }

    #[test]
    fn keyboard_traversal_skips_disabled_controls_and_can_activate_dependencies() {
        let mut review = SetupReview::default();
        review.begin("demo".into(), "/repo".into(), inspection());
        review.set_focus(Control::Dependencies);
        let deps = review.focus;
        review.key(&Key::Named(NamedKey::Tab), ModifiersState::empty());
        assert_ne!(review.focus, deps);
        review.key(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(review.focus, deps);
        assert_eq!(
            review.key(&Key::Named(NamedKey::Space), ModifiersState::empty()),
            SetupAction::Install(vec!["npm".into(), "ci".into()])
        );
        let m = modal_rect(1200., 800., 1.);
        for (control, expected) in [
            (Control::Back, SetupAction::Back),
            (Control::Cancel, SetupAction::Cancel),
            (Control::Check, SetupAction::Check),
        ] {
            let (_, rect) = review
                .controls(m)
                .into_iter()
                .find(|(c, _)| *c == control)
                .unwrap();
            assert_eq!(
                review.click(
                    rect.x + rect.width / 2.,
                    rect.y + rect.height / 2.,
                    1200.,
                    800.,
                    1.
                ),
                expected
            );
        }
        assert_eq!(review.click(0., 0., 1200., 800., 1.), SetupAction::None);
    }

    #[test]
    fn missing_tools_cycle_between_installable_and_manual_choices_without_changing_commands() {
        let mut review = SetupReview::default();
        review.begin("demo".into(), "/repo".into(), inspection());
        review.set_value(0, "codex");
        review.set_value(1, "docker compose up");
        let before = review.commands().unwrap();
        assert!(review.detail().contains("npm install"));
        assert_eq!(
            review.activate(Control::Install),
            SetupAction::Install(vec![
                "npm".into(),
                "install".into(),
                "-g".into(),
                "@openai/codex".into()
            ])
        );
        let revision = review.tool_revision();
        review.activate(Control::Tool);
        assert_ne!(review.tool_revision(), revision);
        assert!(!review.enabled(Control::Install));
        assert!(review.detail().contains("daemon host"));
        assert_eq!(
            review.activate(Control::Guide),
            SetupAction::Help("https://docs.docker.com/get-started/get-docker/".into())
        );
        assert_eq!(review.commands().unwrap(), before);
    }
}
