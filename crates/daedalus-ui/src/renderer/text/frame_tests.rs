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
    frame.labels(1200., 800.);
    let save = frame
        .renderer
        .modal_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "Save")
        .unwrap();
    assert_eq!(
        save.buffer.lines[0].attrs_list().defaults().weight,
        glyphon::Weight::BOLD
    );
    frame.state.settings.set_focus(Control::Executable);
    frame.state.settings.set_value(Control::Executable, "");
    frame.state.settings.activate(Control::Save);
    let hint = frame.state.settings.error.clone().unwrap();
    let labels = frame.labels(900., 650.);
    assert!(labels.iter().any(|s| s.contains(&hint)), "{labels:?}");
}

#[test]
fn widget_editor_aligns_full_width_field_values() {
    use daedalus_proto::workbench::{WorkbenchState, WorkspaceDocument};

    let mut frame = Frame::new();
    let settings = &mut frame.state.settings;
    settings.begin(vec![]);
    settings.open_custom_page();
    settings.custom.accept_snapshot(WorkbenchState {
        revision: 0,
        document: WorkspaceDocument::new("Orbit".into()),
        repo_baseline: None,
    });
    settings.custom.create();
    settings.set_value(Control::CustomName, "Git worktree");
    settings.set_value(Control::CustomSummary, "main:branch");
    settings.set_value(Control::CustomDetails, "main:changes");
    settings.set_focus(Control::CustomSave);

    for size in [(1000., 800.), (420., 600.)] {
        frame.labels(size.0, size.1);
        let value_positions = ["Git worktree", "main:branch", "main:changes"].map(|value| {
            frame
                .renderer
                .modal_items
                .iter()
                .find(|item| item.buffer.lines[0].text() == value)
                .unwrap_or_else(|| panic!("missing {value} at {size:?}"))
                .left
        });
        assert_eq!(value_positions, [value_positions[0]; 3]);
    }
}

#[test]
fn import_review_styles_content_as_readable_summary() {
    use daedalus_proto::{
        StatusLevel, WorkspaceId,
        workbench::{WidgetResult, WorkspaceDocument, WorkspaceImportPreview},
    };

    let mut frame = Frame::new();
    let settings = &mut frame.state.settings;
    settings.begin(vec![]);
    settings.open_custom_page();
    settings.custom.sharing = true;
    let document = WorkspaceDocument::new("Orbit".into());
    settings.custom.import = Some(WorkspaceImportPreview {
        workspace_id: WorkspaceId::new(),
        revision: 1,
        incoming: document.clone(),
        current: document.clone(),
        proposed: document,
        linked: false,
        conflicts: vec![],
    });
    settings.custom.show_import_review(WidgetResult {
        label: "Orbit".into(),
        level: StatusLevel::Ok,
        details: vec![("Coding agent".into(), "codex".into())],
        element_values: Default::default(),
    });

    let (width, height) = (1000., 800.);
    frame.labels(width, height);
    let theme = Theme::dark();
    let item = frame
        .renderer
        .modal_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "Coding agent · codex")
        .unwrap();
    assert_eq!(item.default_color, theme.text_primary.to_glyphon_color());
    let settings = &frame.state.settings;
    let (_, row) = settings
        .controls(settings.modal_rect(width, height, 1.))
        .find(|(control, _)| *control == Control::CustomDetail(0))
        .unwrap();
    assert!(!settings.enabled(Control::CustomDetail(0)));
    let (quads, base) = crate::renderer::collect_quads(
        &frame.state,
        &frame.layout,
        &theme,
        width,
        height,
        1.,
        frame.renderer.cell_size(),
    );
    assert!(quads[base as usize..].iter().any(|quad| {
        quad.rect[0] >= row.x
            && quad.rect[0] + quad.rect[2] <= row.right()
            && quad.rect[1] >= row.y
            && quad.rect[1] + quad.rect[3] <= row.bottom()
            && quad.rect[2] > row.width * 0.5
            && quad.rect[3] <= 1.
    }));
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
    frame.state.panes.get_mut(&active).unwrap().unread_attention = true;
    let entries = [
        (active, "Next ›".into()),
        (PaneId::new(), "Test suite".into()),
    ];
    frame.layout.set_named_tabs(&entries, Some(active), 1.);
    frame.labels(1200., 800.);
    assert!(
        frame.renderer.chrome_items.iter().any(|item| {
            item.buffer.lines[0].text() == "●"
                && item.bounds.left == frame.layout.named_tabs[0].rect.x.floor() as i32
        }),
        "a session matching the pager caption still draws its unread marker"
    );
    for tab in &frame.layout.named_tabs {
        let item = frame
            .renderer
            .chrome_items
            .iter()
            .find(|i| i.buffer.lines[0].text() == tab.label)
            .unwrap();
        assert_eq!(item.bounds.left, tab.rect.x.floor() as i32);
        assert_eq!(
            item.default_color,
            if tab.kind.session() == Some(active) {
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

#[test]
fn custom_widget_chrome_renders_reports_icons_overflow_and_action_details() {
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{
        NamedAction, WidgetAction, WidgetDefinition, WidgetDisplay, WidgetInstance, WidgetResult,
        WidgetSnapshot, WorkbenchState, WorkspaceDocument,
    };

    let mut frame = Frame::new();
    let id = frame.state.active_workspace_id.unwrap();
    let theme = Theme::dark();
    let cases = [
        (None, "Pending report", theme.text_primary),
        (
            Some(StatusLevel::Info),
            "Status received",
            theme.text_primary,
        ),
        (Some(StatusLevel::Ok), "Ready", theme.status_ok),
        (
            Some(StatusLevel::Warn),
            "Needs attention",
            theme.status_busy,
        ),
        (Some(StatusLevel::Error), "Failed", theme.status_err),
    ];
    let mut document = WorkspaceDocument::new("Widget reports".into());
    frame.layout.widget_bar = Some(Rect::new(0., 766., 1200., 34.));
    for (index, (level, label, _)) in cases.into_iter().enumerate() {
        let widget = WidgetInstance {
            id: format!("check-{index}"),
            name: if level.is_none() {
                label.into()
            } else {
                format!("Check {index}")
            },
            pinned: true,
            template: None,
            definition: Some(WidgetDefinition {
                display: WidgetDisplay {
                    icon: (index == 2).then(|| "★".into()),
                    ..Default::default()
                },
                actions: vec![NamedAction {
                    id: "details".into(),
                    label: "Open logs".into(),
                    action: WidgetAction::Logs {
                        role: daedalus_proto::PaneRole::Runner,
                    },
                }],
                ..Default::default()
            }),
        };
        if let Some(level) = level {
            frame.state.widget_results.push(WidgetSnapshot {
                id: widget.id.clone(),
                result: WidgetResult {
                    label: label.into(),
                    level,
                    details: vec![],
                    element_values: Default::default(),
                },
            });
        }
        document.widgets.push(widget);
        frame.layout.widget_buttons.push((
            Some(index),
            Rect::new(5. + 195. * index as f32, 771., 190., 24.),
        ));
    }
    frame
        .layout
        .widget_buttons
        .push((None, Rect::new(980., 771., 190., 24.)));
    let snapshot = WorkbenchState {
        revision: 1,
        document,
        repo_baseline: None,
    };
    frame.state.workbench = Some((id, snapshot.clone()));
    frame.state.settings.begin(Vec::new());
    frame.state.settings.custom_supported = true;
    frame.state.settings.open_custom_page();
    frame.state.settings.custom.accept_snapshot(snapshot);
    assert!(frame.state.settings.custom.open_details(4));
    frame.state.settings.custom.action_output = true;
    frame.state.settings.custom.preview = Some(WidgetResult {
        label: "Runner diagnostics".into(),
        level: StatusLevel::Error,
        details: vec![("Failure".into(), "compile error\ncheck the log".into())],
        element_values: Default::default(),
    });
    frame.state.settings.focus = Control::CustomRefresh;

    let modal = frame.labels(1200., 800.);
    for expected in [
        "Check 4",
        "Failure · compile error",
        "check the log",
        "Open logs",
        "Refresh widget",
        "Runner diagnostics",
    ] {
        assert!(
            modal.iter().any(|label| label == expected),
            "missing {expected}"
        );
    }
    let footer_items: Vec<_> = frame
        .renderer
        .chrome_items
        .iter()
        .filter(|item| item.bounds.top == 771)
        .map(|item| (item.buffer.lines[0].text(), item.default_color))
        .collect();
    assert_eq!(footer_items.len(), 6);
    for (index, ((_, label, color), (text, actual_color))) in
        cases.iter().zip(&footer_items).enumerate()
    {
        let expected = if index == 2 {
            format!("★ {label}")
        } else {
            (*label).into()
        };
        assert_eq!(
            (*text, *actual_color),
            (expected.as_str(), color.to_glyphon_color())
        );
    }
    assert_eq!(
        footer_items[5],
        ("More widgets", theme.text_primary.to_glyphon_color())
    );
}

#[test]
fn styled_footer_renders_a_button_and_a_labeled_led() {
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{
        WidgetDefinition, WidgetDisplay, WidgetInstance, WidgetResult, WidgetSnapshot, WidgetStyle,
        WorkbenchState, WorkspaceDocument,
    };

    let mut frame = Frame::new();
    let theme = Theme::dark();
    let mut document = WorkspaceDocument::new("Styles".into());
    frame.layout.widget_bar = Some(Rect::new(0., 766., 1200., 34.));
    let button = Rect::new(8., 771., 200., 24.);
    let led = Rect::new(
        216.,
        771.,
        crate::layout::WIDGET_BUTTON_MIN_LOGICAL_WIDTH,
        24.,
    );
    for (style, name, rect) in [
        (WidgetStyle::Button, "Tests", button),
        (WidgetStyle::Led, "Endpoint", led),
    ] {
        frame
            .layout
            .widget_buttons
            .push((Some(document.widgets.len()), rect));
        document.widgets.push(WidgetInstance {
            id: name.into(),
            name: name.into(),
            pinned: true,
            template: None,
            definition: Some(WidgetDefinition {
                display: WidgetDisplay {
                    style,
                    ..Default::default()
                },
                ..Default::default()
            }),
        });
    }
    frame.state.workbench = Some((
        frame.state.active_workspace_id.unwrap(),
        WorkbenchState {
            revision: 1,
            document,
            repo_baseline: None,
        },
    ));
    frame.state.widget_results.push(WidgetSnapshot {
        id: "Endpoint".into(),
        result: WidgetResult {
            label: "Ready".into(),
            level: StatusLevel::Ok,
            details: Vec::new(),
            element_values: Default::default(),
        },
    });

    frame.labels(1200., 800.);
    let led_text = frame
        .renderer
        .chrome_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "OK · Ready · Endpoint")
        .unwrap();
    assert_eq!(led_text.left, led.x + 20.);
    let rendered = led_text.buffer.layout_runs().next().unwrap();
    assert!(rendered.glyphs.iter().any(|glyph| glyph.start == 0));
    let (quads, _) = crate::renderer::collect_quads(
        &frame.state,
        &frame.layout,
        &theme,
        1200.,
        800.,
        1.,
        (8., 19.),
    );
    assert!(quads.iter().any(|quad| quad.rect == [8., 771., 200., 24.]
        && quad.color == theme.border_focused.to_array()
        && quad.style[1] == 1.));
    assert!(quads.iter().any(|quad| quad.rect == [222., 779., 8., 8.]
        && quad.color == theme.status_ok.to_array()
        && quad.style[0] == 4.));

    let (_, workspace) = frame.state.workbench.as_mut().unwrap();
    workspace.document.widgets[1]
        .definition
        .as_mut()
        .unwrap()
        .display
        .appearance
        .status = daedalus_proto::workbench::WidgetStatusPresentation::Text;
    frame.state.widget_results[0].result.level = StatusLevel::Warn;
    frame.labels(1200., 800.);
    assert!(
        frame
            .renderer
            .chrome_items
            .iter()
            .any(|item| item.buffer.lines[0].text() == "Warning · Ready · Endpoint")
    );
}

#[test]
fn composed_footer_paints_ordered_text_and_a_progress_fill() {
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{
        NamedAction, WidgetAction, WidgetDefinition, WidgetDisplay, WidgetElement, WidgetField,
        WidgetInstance, WidgetResult, WidgetSnapshot, WorkbenchState, WorkspaceDocument,
    };

    let mut frame = Frame::new();
    let theme = Theme::dark();
    let mut document = WorkspaceDocument::new("Build".into());
    let field = |field: &str, label: &str| WidgetField {
        source: "build".into(),
        field: field.into(),
        label: Some(label.into()),
    };
    document.widgets.push(WidgetInstance {
        id: "build".into(),
        name: "Build pipeline".into(),
        pinned: true,
        template: None,
        definition: Some(WidgetDefinition {
            display: WidgetDisplay {
                elements: vec![
                    WidgetElement::Label {
                        text: "Build pipeline".into(),
                    },
                    WidgetElement::Status,
                    WidgetElement::Counter {
                        field: field("failed", "Failures"),
                    },
                    WidgetElement::Progress {
                        field: field("passed", "Passed"),
                        max: 20,
                    },
                    WidgetElement::Button {
                        action: "logs".into(),
                    },
                ],
                ..Default::default()
            },
            actions: vec![NamedAction {
                id: "logs".into(),
                label: "Open logs".into(),
                action: WidgetAction::Logs {
                    role: daedalus_proto::PaneRole::Runner,
                },
            }],
            ..Default::default()
        }),
    });
    frame.state.widget_results.push(WidgetSnapshot {
        id: "build".into(),
        result: WidgetResult {
            label: "Checks pending".into(),
            level: StatusLevel::Warn,
            details: Vec::new(),
            element_values: [(
                "build".into(),
                [("failed".into(), "3".into()), ("passed".into(), "5".into())].into(),
            )]
            .into(),
        },
    });
    frame.layout.set_widget_footer(
        Rect::new(0., 766., 1200., 34.),
        &document,
        &frame.state.widget_results,
        1.,
    );
    frame.state.workbench = Some((
        frame.state.active_workspace_id.unwrap(),
        WorkbenchState {
            revision: 1,
            document,
            repo_baseline: None,
        },
    ));

    frame.labels(1200., 800.);
    let footer: Vec<_> = frame
        .renderer
        .chrome_items
        .iter()
        .filter(|item| item.top >= 766.)
        .map(|item| item.buffer.lines[0].text())
        .collect();
    assert_eq!(
        footer,
        [
            "Build pipeline",
            "Warning · Checks pending",
            "Failures: 3",
            "Passed: 5/20",
            "Open logs"
        ]
    );
    let (quads, _) = crate::renderer::collect_quads(
        &frame.state,
        &frame.layout,
        &theme,
        1200.,
        800.,
        1.,
        (8., 19.),
    );
    let row = frame.layout.widget_row(Some(0)).unwrap();
    let status = row.cells[1].bounds;
    let dot = quads
        .iter()
        .find(|quad| {
            let [x, y, width, height] = quad.rect;
            status.contains(x, y)
                && width == 8.
                && height == 8.
                && quad.color == theme.status_busy.to_array()
        })
        .expect("the warning has a colored status light");
    let status_text = frame
        .renderer
        .chrome_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "Warning · Checks pending")
        .unwrap();
    assert!(status_text.left > dot.rect[0] + dot.rect[2]);
    let action = row.cells[4].bounds;
    assert!(quads.iter().any(|quad| quad.rect
        == [action.x, action.y, action.width, action.height]
        && quad.color == theme.border_focused.to_array()
        && quad.style[1] > 0.));
    let progress = row.cells[3].bounds;
    assert!(quads.iter().any(|quad| {
        let [x, y, width, _] = quad.rect;
        progress.contains(x, y)
            && quad.color == theme.border_focused.to_array()
            && width > 0.
            && (width * 4. - (progress.width - (x - progress.x) * 2.)).abs() < 0.1
    }));
}

fn pinned_widget(
    name: &str,
    display: daedalus_proto::workbench::WidgetDisplay,
) -> daedalus_proto::workbench::WidgetInstance {
    use daedalus_proto::workbench::{WidgetDefinition, WidgetInstance};

    WidgetInstance {
        id: name.into(),
        name: name.into(),
        pinned: true,
        template: None,
        definition: Some(WidgetDefinition {
            display,
            ..Default::default()
        }),
    }
}

#[test]
fn footer_width_preferences_leave_room_for_composed_content_and_overflow() {
    use daedalus_proto::workbench::{
        WidgetAppearance, WidgetDisplay, WidgetElement, WorkspaceDocument,
    };

    let (_, mut layout) = make_state_with_agent_row();
    layout.cargo_bar = None;
    let mut document = WorkspaceDocument::new("Widths".into());
    document.widgets = vec![
        pinned_widget(
            "Wide stand-alone widget",
            WidgetDisplay {
                appearance: WidgetAppearance {
                    width: Some(500),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        pinned_widget(
            "Composition",
            WidgetDisplay {
                appearance: WidgetAppearance {
                    width: Some(110),
                    ..Default::default()
                },
                elements: vec![WidgetElement::Label {
                    text: "Configure lint rules".into(),
                }],
                ..Default::default()
            },
        ),
        pinned_widget("Neighbor", WidgetDisplay::default()),
    ];

    let mut layout_at = |width| {
        layout.set_widget_footer(Rect::new(0., 766., width, 34.), &document, &[], 1.);
        layout.clone()
    };
    let roomy = layout_at(1200.);
    assert_eq!(roomy.widget_buttons[0].1.width, 500.);
    assert!(roomy.widget_buttons[1].1.width > 110.);
    assert!(roomy.widget_row(Some(1)).is_some());

    let crowded = layout_at(460.);
    assert_eq!(crowded.widget_buttons.len(), 3);
    assert!(crowded.widget_buttons[0].1.width < 500.);
    assert_eq!(
        crowded.widget_buttons[1].1.width,
        roomy.widget_buttons[1].1.width
    );
    assert_eq!(
        crowded.widget_row(Some(1)).unwrap().cells[0].bounds.width,
        roomy.widget_row(Some(1)).unwrap().cells[0].bounds.width
    );
    assert!(crowded.widget_buttons[2].1.width >= 110.);
    assert!(crowded.widget_buttons[2].1.right() < 460.);

    let compact = layout_at(360.);
    assert_eq!(compact.widget_buttons[1].0, Some(1));
    assert!(compact.widget_row(Some(1)).is_none());

    let narrow = layout_at(340.);
    assert_eq!(narrow.widget_buttons.len(), 2);
    assert_eq!(narrow.widget_buttons[0].0, Some(0));
    assert_eq!(narrow.widget_buttons[1].0, None);
    assert!(narrow.widget_buttons[1].1.right() < 340.);
}

#[test]
fn widget_appearance_styles_footer_controls_and_preserves_status_meaning() {
    use crate::theme::Rgba;
    use daedalus_proto::StatusLevel;
    use daedalus_proto::workbench::{
        NamedAction, WidgetAction, WidgetAppearance, WidgetDensity, WidgetDisplay, WidgetElement,
        WidgetField, WidgetResult, WidgetSnapshot, WidgetStatusPresentation, WidgetStyle,
        WidgetSurface, WorkbenchState, WorkspaceDocument,
    };

    let theme = Theme::dark();
    let accent = Rgba::from_rgb_u8(0x74, 0x9f, 0xfa).to_array();
    let mut frame = Frame::new();
    frame.layout.cargo_bar = None;
    let mut document = WorkspaceDocument::new("Appearance".into());
    let mut build = pinned_widget(
        "Checks",
        WidgetDisplay {
            appearance: WidgetAppearance {
                accent: Some("#749FFA".into()),
                density: WidgetDensity::Compact,
                surface: WidgetSurface::Outline,
                status: WidgetStatusPresentation::Light,
                width: Some(460),
            },
            elements: vec![
                WidgetElement::Status,
                WidgetElement::Progress {
                    field: WidgetField {
                        source: "build".into(),
                        field: "passed".into(),
                        label: Some("Passed".into()),
                    },
                    max: 4,
                },
                WidgetElement::Button {
                    action: "logs".into(),
                },
            ],
            ..Default::default()
        },
    );
    build.definition.as_mut().unwrap().actions = vec![NamedAction {
        id: "logs".into(),
        label: "Open logs".into(),
        action: WidgetAction::Logs {
            role: daedalus_proto::PaneRole::Runner,
        },
    }];
    document.widgets = vec![
        build,
        pinned_widget(
            "Endpoint",
            WidgetDisplay {
                style: WidgetStyle::Led,
                appearance: WidgetAppearance {
                    density: WidgetDensity::Spacious,
                    status: WidgetStatusPresentation::Text,
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        pinned_widget(
            "Flat label",
            WidgetDisplay {
                appearance: WidgetAppearance {
                    surface: WidgetSurface::Flat,
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    ];
    frame.state.widget_results = vec![
        WidgetSnapshot {
            id: "Checks".into(),
            result: WidgetResult {
                label: "Checks pending".into(),
                level: StatusLevel::Warn,
                details: Vec::new(),
                element_values: [("build".into(), [("passed".into(), "1".into())].into())].into(),
            },
        },
        WidgetSnapshot {
            id: "Endpoint".into(),
            result: WidgetResult {
                label: "Serving".into(),
                level: StatusLevel::Ok,
                details: Vec::new(),
                element_values: Default::default(),
            },
        },
    ];
    frame.layout.set_widget_footer(
        Rect::new(0., 766., 1200., 34.),
        &document,
        &frame.state.widget_results,
        1.,
    );
    frame.state.workbench = Some((
        frame.state.active_workspace_id.unwrap(),
        WorkbenchState {
            revision: 1,
            document,
            repo_baseline: None,
        },
    ));

    frame.labels(1200., 800.);
    let [(_, build), (_, endpoint), (_, flat)] = frame.layout.widget_buttons[..] else {
        panic!("expected all three widget slots");
    };
    assert_eq!(build.width, 460.);
    let row = frame.layout.widget_row(Some(0)).unwrap();
    assert_eq!(row.cells[0].text, "Warning · Checks pending");
    assert_eq!(row.cells[1].bounds.x - row.cells[0].bounds.right(), 2.);
    assert!(
        !frame
            .renderer
            .chrome_items
            .iter()
            .any(|item| item.buffer.lines[0].text().contains("Checks pending"))
    );
    let endpoint_text = frame
        .renderer
        .chrome_items
        .iter()
        .find(|item| item.buffer.lines[0].text() == "OK · Serving · Endpoint")
        .unwrap();
    assert!(endpoint_text.left > endpoint.x + 6. && endpoint_text.left < endpoint.x + 20.);
    assert_eq!(
        endpoint_text.default_color,
        theme.status_ok.to_glyphon_color()
    );

    let (quads, _) = crate::renderer::collect_quads(
        &frame.state,
        &frame.layout,
        &theme,
        1200.,
        800.,
        1.,
        (8., 19.),
    );
    for bounds in [build, row.cells[2].bounds] {
        assert!(quads.iter().any(|quad| quad.rect
            == [bounds.x, bounds.y, bounds.width, bounds.height]
            && quad.color == accent
            && quad.style[1] > 0.));
    }
    assert_eq!(
        quads
            .iter()
            .filter(|quad| quad.rect == [build.x, build.y, build.width, build.height])
            .count(),
        1
    );
    let status = row.cells[0].bounds;
    assert!(quads.iter().any(|quad| {
        let [x, y, width, height] = quad.rect;
        status.contains(x, y)
            && width == 8.
            && height == 8.
            && quad.color == theme.status_busy.to_array()
    }));
    let progress = row.cells[1].bounds;
    assert!(quads.iter().any(|quad| {
        let [x, y, _, height] = quad.rect;
        x == progress.x + 4. && progress.contains(x, y) && height == 2. && quad.color == accent
    }));
    for bounds in [endpoint, flat] {
        assert!(
            !quads
                .iter()
                .any(|quad| bounds.contains(quad.rect[0], quad.rect[1]))
        );
    }

    let action = row.cells[2].bounds;
    frame.state.widget_results[0].result.label = "A refreshed report with a longer label".into();
    frame.state.widget_results[0].result.element_values.clear();
    let document = &frame.state.workbench.as_ref().unwrap().1.document;
    frame.layout.set_widget_footer(
        Rect::new(0., 766., 1200., 34.),
        document,
        &frame.state.widget_results,
        1.,
    );
    assert_eq!(
        frame.layout.widget_row(Some(0)).unwrap().cells[2].bounds,
        action
    );
}
