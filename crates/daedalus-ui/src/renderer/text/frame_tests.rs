use super::*;
use crate::renderer::tests::make_state_with_agent_row;
use crate::settings::Control;
use crate::state::{AgentPreset, ConnectionState, SearchState};

struct Frame {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: TextRenderer,
    state: AppState,
    layout: WorkspaceLayout,
}

impl Frame {
    fn new() -> Self {
        let (device, queue) = wgpu::Device::noop(&Default::default());
        let renderer = TextRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            1200,
            800,
            1.,
            14.,
            None,
        );
        let (state, layout) = make_state_with_agent_row();
        Self {
            device,
            queue,
            renderer,
            state,
            layout,
        }
    }

    fn labels(&mut self, w: f32, h: f32) -> Vec<String> {
        self.renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.state,
            &self.layout,
            &Theme::dark(),
            w,
            h,
        );
        self.renderer
            .modal_items
            .iter()
            .map(|item| item.buffer.lines[0].text().to_owned())
            .collect()
    }
}

#[test]
fn dialog_titles_and_actions_share_their_type_scale() {
    let mut frame = Frame::new();
    let sizes = |frame: &mut Frame, title: &str, action: &str| {
        frame.labels(1000.0, 570.0);
        [title, action].map(|label| {
            frame
                .renderer
                .modal_items
                .iter()
                .find(|item| item.buffer.lines[0].text() == label)
                .expect("visible control")
                .buffer
                .metrics()
                .font_size
        })
    };
    frame.state.settings.begin(vec![]);
    let settings = sizes(&mut frame, "Workspace assignments", "Cancel");
    frame.state.settings.open = false;
    frame.state.worktree_modal.open();
    let worktree = sizes(&mut frame, "New Git Worktree", "Cancel (Esc)");
    assert_eq!(settings, worktree);
    assert!(settings[0] > settings[1]);
}

#[test]
fn preset_and_action_pickers_replace_the_previous_selection_and_empty_hint() {
    let mut frame = Frame::new();
    frame.state.picker.open = true;
    frame.state.picker.selected_index = 1;
    let labels = frame.labels(1200., 800.);
    assert_eq!(labels[0], "Agent preset");
    assert!(labels.contains(&format!("▸ {}", AgentPreset::ALL[1].label())));
    assert!(labels.contains(&format!("  {}", AgentPreset::ALL[0].label())));
    frame.state.picker.open = false;
    frame.state.action_palette.open = true;
    let labels = frame.labels(1200., 800.);
    assert_eq!(labels[0], "Workspace actions");
    assert_eq!(labels.last().unwrap(), "Add actions in .daedalus.toml");
    frame.state.workspaces[0].manifest = Some(daedalus_proto::WorkspaceManifest {
        actions: vec![daedalus_proto::CustomAction {
            id: "test".into(),
            label: "Run tests".into(),
            target: daedalus_proto::PaneRole::Runner,
            command: "cargo test".into(),
            shortcut: Some("F6".into()),
        }],
        ..Default::default()
    });
    let labels = frame.labels(1200., 800.);
    assert_eq!(labels.last().unwrap(), "▸ Run tests · Runner F6");
}

#[test]
fn worktree_dialog_reports_missing_branch_then_preserves_typed_suffix_on_resize() {
    let mut frame = Frame::new();
    frame.state.worktree_modal.open();
    assert!(frame.labels(1200., 800.).contains(&"feat/...".into()));
    assert_eq!(frame.state.worktree_modal.branch_for_submit(), None);
    assert!(
        frame
            .labels(1200., 800.)
            .contains(&"Enter a branch name".into())
    );
    frame
        .state
        .worktree_modal
        .push_text(&format!("{}-日本", "feature".repeat(24)));
    frame.state.connection = ConnectionState::Reconnecting;
    let labels = frame.labels(320., 240.);
    assert!(labels.contains(&"Waiting for daemon".into()));
    assert!(
        labels
            .iter()
            .any(|line| line.starts_with('…') && line.ends_with("-日本|"))
    );
    frame.state.connection = ConnectionState::Connected;
    assert!(
        frame
            .labels(1200., 800.)
            .iter()
            .any(|line| line.starts_with("Create"))
    );
}

#[test]
fn close_dialog_exposes_dirty_worktree_options_and_acknowledges_submission() {
    let mut frame = Frame::new();
    let cm = &mut frame.state.close_modal;
    cm.open = true;
    cm.workspace_name = "daedalus".into();
    let clean = frame.labels(1200., 800.);
    assert!(clean.contains(&"daedalus".into()));
    assert!(!clean.iter().any(|s| s.contains("Force")));
    let cm = &mut frame.state.close_modal;
    cm.branch = Some("feature".into());
    cm.is_dirty = true;
    cm.is_worktree = true;
    cm.remove_worktree = true;
    cm.has_compose = true;
    cm.compose_down = true;
    let labels = frame.labels(1200., 800.);
    for value in [
        "daedalus  (feature)",
        "[x] Stop Docker Compose (c)",
        "[x] Remove git worktree (w)",
        "[ ] Force remove dirty worktree (f)",
    ] {
        assert!(labels.contains(&value.to_owned()), "{labels:?}");
    }
    frame.state.close_modal.force_remove_worktree = true;
    frame.state.close_modal.submitting = true;
    let labels = frame.labels(300., 175.);
    assert!(labels.contains(&"Closing…".into()));
    assert!(labels.iter().any(|s| s.starts_with("[x] Force")));
    assert!(!labels.iter().any(|s| s.starts_with("Enter: Confirm")));
}

#[test]
fn search_frame_displays_query_case_and_received_empty_result() {
    let mut frame = Frame::new();
    let mut search = SearchState::new(frame.state.focused_pane_id().unwrap());
    search.push_text("compile");
    search.toggle_case();
    search.set_results(32, vec![]);
    frame.state.search = Some(search);
    let labels = frame.labels(1200., 800.);
    for expected in ["Search: compile|", "Next", "No matches"] {
        assert!(labels.contains(&expected.to_owned()), "{labels:?}");
    }
    assert!(labels.iter().any(|s| s.contains("On")));
}

#[test]
fn settings_frame_shows_selected_command_and_validation_feedback() {
    let mut frame = Frame::new();
    frame.state.settings.begin(vec![]);
    frame.labels(1200., 800.);
    let add = frame
        .renderer
        .modal_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "+ Command")
        .unwrap();
    let width = add.buffer.layout_runs().next().unwrap().line_w;
    let left_gap = add.left - add.bounds.left as f32;
    let right_gap = add.bounds.right as f32 - add.left - width;
    assert!(
        (left_gap - right_gap).abs() <= 2.0,
        "button gutters: {left_gap}, {right_gap}"
    );
    frame.state.settings.activate(Control::Codex);
    frame.state.settings.set_focus(Control::Executable);
    frame.state.settings.set_value(Control::Executable, "");
    frame.state.settings.activate(Control::Save);
    let hint = frame.state.settings.error.clone().unwrap();
    let labels = frame.labels(900., 650.);
    assert!(labels.iter().any(|s| s.contains(&hint)), "{labels:?}");
}

#[test]
fn font_and_dpi_changes_rebuild_cached_terminal_rows_and_prune_closed_panes() {
    let mut frame = Frame::new();
    let pane = frame.state.focused_pane_id().unwrap();
    frame.labels(1200., 800.);
    let initial_size = frame.renderer.cell_size();
    assert!(!frame.renderer.set_font_size(frame.renderer.font_size()));
    assert!(frame.renderer.pane_buffers.contains_key(&pane));
    assert!(frame.renderer.set_font_size(50.));
    assert_eq!(frame.renderer.font_size(), 32.);
    assert!(frame.renderer.cell_size().1 > initial_size.1);
    assert!(frame.renderer.pane_buffers.is_empty());
    frame.labels(1200., 800.);
    assert_eq!(
        frame.renderer.pane_buffers[&pane].rows[0].lines[0].text(),
        "abc"
    );
    let larger_size = frame.renderer.cell_size();
    frame.renderer.resize(&frame.queue, 1200, 800, 2.);
    assert!(frame.renderer.cell_size().1 > larger_size.1);
    assert!(frame.renderer.pane_buffers.is_empty());
    frame.labels(1200., 800.);
    frame.renderer.resize(&frame.queue, 640, 480, 2.);
    assert!(frame.renderer.pane_buffers.contains_key(&pane));
    frame.state.panes.remove(&pane);
    frame.labels(640., 480.);
    assert!(frame.renderer.pane_buffers.is_empty());
}

#[test]
fn zoomed_headers_switch_from_default_role_tabs_to_named_sessions_with_a_close_target() {
    use crate::layout::{LayoutInput, compute_layout};
    use daedalus_proto::PaneRole;
    let mut frame = Frame::new();
    frame.state.zoomed = true;
    frame.layout = compute_layout(&LayoutInput {
        window_w: 1200.,
        window_h: 800.,
        scale_factor: 1.,
        cell_w: 8.,
        cell_h: 19.,
        sidebar_override: Some(false),
        has_top_banner: false,
        zoomed: true,
        focused_role: PaneRole::Agent,
        runner_has_failure: false,
    });
    frame.labels(1200., 800.);
    let theme = Theme::dark();
    for (role, color) in [
        ("Agent", theme.border_focused),
        ("Runner", theme.text_muted),
        ("Service", theme.text_muted),
    ] {
        assert!(
            frame
                .renderer
                .chrome_items
                .iter()
                .any(|item| item.buffer.lines[0].text() == role
                    && item.default_color == color.to_glyphon_color())
        );
    }
    let active = frame.state.focused_pane_id().unwrap();
    let entries = [
        (active, "Builder".into()),
        (PaneId::new(), "Test suite".into()),
    ];
    frame.layout.set_named_tabs(&entries, Some(active), 1.);
    frame.labels(1200., 800.);
    for (id, label, rect) in &frame.layout.named_tabs {
        let item = frame
            .renderer
            .chrome_items
            .iter()
            .find(|i| i.buffer.lines[0].text() == label)
            .unwrap();
        assert_eq!(item.bounds.left, rect.x.floor() as i32);
        assert_eq!(
            item.default_color,
            if *id == active {
                theme.border_focused
            } else {
                theme.text_muted
            }
            .to_glyphon_color()
        );
    }
    let close = frame
        .renderer
        .chrome_items
        .iter()
        .find(|i| i.buffer.lines[0].text() == "×")
        .unwrap();
    let (pane, button) = frame.layout.close_tab.unwrap();
    assert_eq!(pane, active);
    assert_eq!(close.bounds.left, button.x.floor() as i32);
    assert_eq!(close.bounds.right, button.right().ceil() as i32);
}

#[test]
fn start_screen_lists_recent_projects_above_the_new_project_form() {
    let mut frame = Frame::new();
    let form = &mut frame.state.workspace_form;
    form.begin(std::path::Path::new("/home/dev"));
    let project = |name: &str, root: &str| daedalus_proto::RecentProject {
        name: name.into(),
        root_dir: root.into(),
        last_used: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        has_setup: true,
    };
    form.set_recent(vec![
        project("daedalus", "/home/tim/daedalus"),
        project("tim-vitepress", "/home/tim/tim-vitepress"),
    ]);
    let labels = frame.labels(1200., 800.);
    let at = |prefix: &str| {
        labels
            .iter()
            .position(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} missing from {labels:?}"))
    };
    let heading = at("Recent projects");
    let first = at("daedalus   /home/tim/daedalus   just now");
    let second = at("tim-vitepress   /home/tim/tim-vitepress   just now");
    at("Set up a new project");
    assert!(heading < first && first < second);
    assert!(labels.iter().any(|line| line.starts_with("Folder")));

    frame.state.workspace_form.set_recent(Vec::new());
    let labels = frame.labels(1200., 800.);
    assert_eq!(labels[0], "Set up workspace · Choose project");
    assert!(!labels.iter().any(|line| line == "Recent projects"));
}

#[test]
fn cargo_bar_and_layout_switch_keep_their_visible_labels() {
    let mut frame = Frame::new();
    frame.layout.cargo_bar = Some(Rect::new(0., 766., 1200., 34.));
    frame.layout.enable_layout_button(1.);
    for (zoomed, expected) in [(true, "Split"), (false, "Focus")] {
        frame.state.zoomed = zoomed;
        frame.labels(1200., 800.);
        let labels: Vec<_> = frame
            .renderer
            .chrome_items
            .iter()
            .map(|item| item.buffer.lines[0].text())
            .collect();
        for label in [expected, "Cargo Test", "Cargo Check"] {
            assert!(labels.contains(&label), "{label} missing from {labels:?}");
        }
    }
}
