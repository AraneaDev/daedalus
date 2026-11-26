use daedalus_proto::transport::IpcStream;
use daedalus_proto::workbench::{
    PaneWidgets, WidgetDefinition, WidgetInstance, WidgetTemplate, WorkbenchEdit, WorkbenchState,
    WorkspaceDocument, WorkspaceImportPreview, WorkspaceImportSource,
};
use daedalus_proto::{
    Assignment, ClientMessage, PaneRole, ProjectSetupBaseline, ProtoError, Request,
    ResponsePayload, ServerMessage, WorkspaceId, read_frame, write_frame,
};
use daedalusd::server::DaemonServer;
use daedalusd::supervisor::{IsolationMode, Supervisor};
use std::path::Path;
use tokio::sync::oneshot;

struct DaemonFixture {
    client: IpcStream,
    shutdown: oneshot::Sender<()>,
    daemon: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl DaemonFixture {
    async fn start(root: &Path) -> Self {
        let socket = root.join("control.sock");
        #[cfg(windows)]
        let isolation = IsolationMode::WindowsProcessTree;
        #[cfg(not(windows))]
        let isolation = IsolationMode::PgidFallback;
        let server = DaemonServer::bind_with_options(
            socket.clone(),
            root.join("runtime"),
            Some(root.join("events.sock")),
            Some(root.join("state.json")),
            Supervisor::with_mode(isolation),
        )
        .await
        .unwrap();
        let (shutdown, done) = oneshot::channel();
        let daemon = tokio::spawn(server.serve(async {
            let _ = done.await;
        }));
        let client = IpcStream::connect(&socket).await.unwrap();
        Self {
            client,
            shutdown,
            daemon,
        }
    }

    async fn call(&mut self, payload: ClientMessage) -> Result<ResponsePayload, ProtoError> {
        write_frame(&mut self.client, &Request { id: 1, payload })
            .await
            .unwrap();
        loop {
            if let ServerMessage::Response { id: 1, result } =
                read_frame(&mut self.client).await.unwrap().unwrap()
            {
                return result;
            }
        }
    }

    async fn workspace(&mut self, root: &Path) -> WorkspaceId {
        let ResponsePayload::WorkspaceCreated(workspace) = self
            .call(ClientMessage::CreateManualWorkspace {
                name: "Workspace".into(),
                root_dir: root.to_path_buf(),
            })
            .await
            .unwrap()
        else {
            panic!("expected workspace")
        };
        workspace.id
    }

    async fn workbench(&mut self, workspace_id: WorkspaceId) -> WorkbenchState {
        let ResponsePayload::Workbench(workbench) = self
            .call(ClientMessage::GetWorkbench { workspace_id })
            .await
            .unwrap()
        else {
            panic!("expected workbench")
        };
        workbench
    }

    async fn repo_preview(&mut self, workspace_id: WorkspaceId) -> WorkspaceImportPreview {
        let ResponsePayload::WorkspaceImportPreview(preview) = self
            .call(ClientMessage::PreviewWorkspaceImport {
                workspace_id,
                source: WorkspaceImportSource::Repo,
                linked: true,
            })
            .await
            .unwrap()
        else {
            panic!("expected import preview")
        };
        preview
    }

    async fn apply(
        &mut self,
        preview: WorkspaceImportPreview,
    ) -> Result<ResponsePayload, ProtoError> {
        self.call(ClientMessage::ApplyWorkspaceImport {
            workspace_id: preview.workspace_id,
            revision: preview.revision,
            incoming: preview.incoming,
            linked: preview.linked,
            resolutions: Default::default(),
        })
        .await
    }

    async fn finish(self) {
        drop(self.client);
        self.shutdown.send(()).unwrap();
        self.daemon.await.unwrap().unwrap();
    }
}

fn runner(name: &str) -> Vec<Assignment> {
    vec![Assignment {
        id: "runner".into(),
        name: name.into(),
        role: PaneRole::Runner,
        argv: vec!["cargo".into(), "test".into()],
    }]
}

#[tokio::test]
async fn stale_setup_save_returns_current_assignments_across_workbench_changes() {
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = DaemonFixture::start(scratch.path()).await;
    let root = scratch.path();
    let workspace_id = daemon.workspace(root).await;
    let request = |assignments, expected| ClientMessage::SetProjectSetup {
        workspace_id,
        assignments,
        expected: Some(ProjectSetupBaseline {
            assignments: expected,
        }),
    };

    // An open dialog with no saved setup can still save when a different
    // client creates the first workbench document by changing only its layout.
    let mut initial = daemon.workbench(workspace_id).await;
    initial.document.layout.split_view = true;
    assert!(matches!(
        daemon
            .call(ClientMessage::UpdateWorkbench {
                workspace_id,
                revision: initial.revision,
                document: initial.document,
                layout_only: true,
                edit: None,
            })
            .await
            .unwrap(),
        ResponsePayload::Workbench(_)
    ));
    let mut original = runner("Original runner");
    original.push(Assignment {
        id: "service".into(),
        name: "Original service".into(),
        role: PaneRole::Service,
        argv: vec!["service".into()],
    });
    assert_eq!(
        daemon.call(request(original.clone(), None)).await.unwrap(),
        ResponsePayload::Ok
    );

    let mut first = original.clone();
    first[0].name = "Runner changed by A".into();
    assert_eq!(
        daemon
            .call(request(first.clone(), Some(original.clone())))
            .await
            .unwrap(),
        ResponsePayload::Ok
    );
    let saved = daemon.workbench(workspace_id).await;
    let mut second = original.clone();
    second[1].name = "Service changed by B".into();
    assert_eq!(
        daemon
            .call(request(second.clone(), Some(original)))
            .await
            .unwrap(),
        ResponsePayload::ProjectSetupConflict(ProjectSetupBaseline {
            assignments: Some(first.clone()),
        })
    );
    assert_eq!(daemon.workbench(workspace_id).await, saved);

    second[0].name = first[0].name.clone();
    assert_eq!(
        daemon
            .call(request(second.clone(), Some(first)))
            .await
            .unwrap(),
        ResponsePayload::Ok
    );
    let persisted = daemon.workbench(workspace_id).await;
    assert_eq!(persisted.document.assignments, second);
    assert!(persisted.document.layout.split_view);
    daemon.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn old_literal_setup_keys_follow_the_project_across_folder_aliases() {
    use daedalusd::persistence::{
        PersistedProjectSetup, PersistedRecentProject, PersistedState, StateStore,
    };

    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let project = root.join("project");
    let alias = root.join("alias");
    std::fs::create_dir(&project).unwrap();
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    let original = runner("Legacy setup");
    let store = StateStore::new(root.join("state.json"));
    store
        .save(&PersistedState {
            project_setups: vec![PersistedProjectSetup {
                root_dir: alias.clone(),
                assignments: original.clone(),
            }],
            recent_projects: vec![PersistedRecentProject {
                name: "Seen through alias".into(),
                root_dir: alias.clone(),
                last_used: 1,
            }],
            ..PersistedState::default()
        })
        .unwrap();

    let mut daemon = DaemonFixture::start(&root).await;
    let ResponsePayload::RecentProjects(recent) = daemon
        .call(ClientMessage::ListRecentProjects)
        .await
        .unwrap()
    else {
        panic!("expected recent projects");
    };
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].root_dir, alias);
    assert!(recent[0].has_setup);

    let id = daemon.workspace(&project).await;
    assert_eq!(daemon.workbench(id).await.document.assignments, original);
    assert_eq!(
        daemon
            .call(ClientMessage::ListRecentProjects)
            .await
            .unwrap(),
        ResponsePayload::RecentProjects(Vec::new()),
    );
    let latest = runner("Latest setup");
    assert_eq!(
        daemon
            .call(ClientMessage::SetProjectSetup {
                workspace_id: id,
                assignments: latest.clone(),
                expected: Some(ProjectSetupBaseline {
                    assignments: Some(original)
                }),
            })
            .await
            .unwrap(),
        ResponsePayload::Ok,
    );
    daemon
        .call(ClientMessage::CloseWorkspace {
            id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        })
        .await
        .unwrap();
    daemon.finish().await;

    let mut daemon = DaemonFixture::start(&root).await;
    let reopened = daemon.workspace(&alias).await;
    assert_eq!(
        daemon.workbench(reopened).await.document.assignments,
        latest
    );
    let ResponsePayload::Workspaces(workspaces) =
        daemon.call(ClientMessage::ListWorkspaces).await.unwrap()
    else {
        panic!("expected open workspaces");
    };
    assert_eq!(workspaces[0].root_dir, alias);
    assert_eq!(workspaces[0].setup.as_deref(), Some(latest.as_slice()));
    assert_eq!(store.load().unwrap().project_setups[0].root_dir, project);
    daemon.finish().await;
}

#[tokio::test]
async fn first_widget_edits_preserve_newer_project_setup() {
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = DaemonFixture::start(scratch.path()).await;
    let ResponsePayload::WidgetLibrary(library) = daemon
        .call(ClientMessage::ListWidgetTemplates)
        .await
        .unwrap()
    else {
        panic!("expected widget library");
    };
    let template = library
        .templates
        .into_iter()
        .find(|item| item.id == "git-worktree")
        .unwrap();
    let assignments = runner("New runner");
    for edit in [WorkbenchEdit::Widgets, WorkbenchEdit::PaneWidgets] {
        let root = scratch.path().join(format!("{edit:?}"));
        std::fs::create_dir(&root).unwrap();
        let workspace_id = daemon.workspace(&root).await;
        let mut stale = daemon.workbench(workspace_id).await;
        assert_eq!(stale.revision, 0);
        assert!(stale.document.assignments.is_empty());
        daemon
            .call(ClientMessage::SetProjectSetup {
                workspace_id,
                assignments: assignments.clone(),
                expected: None,
            })
            .await
            .unwrap();
        assert_eq!(daemon.workbench(workspace_id).await.revision, 0);

        stale.document.widgets.push(WidgetInstance {
            id: "branch".into(),
            name: template.name.clone(),
            pinned: true,
            template: Some(template.id.clone()),
            definition: None,
        });
        stale.document.templates.push(template.clone());
        stale.document.pane_widgets = Some(PaneWidgets::default());
        let request = ClientMessage::UpdateWorkbench {
            workspace_id,
            revision: stale.revision,
            document: stale.document,
            layout_only: false,
            edit: Some(edit),
        };
        let ResponsePayload::Workbench(saved) = daemon.call(request.clone()).await.unwrap() else {
            panic!("expected scoped save");
        };
        assert_eq!(saved.document.assignments, assignments);
        assert_eq!(
            saved.document.widgets.len(),
            usize::from(edit == WorkbenchEdit::Widgets)
        );
        assert_eq!(
            saved.document.pane_widgets.is_some(),
            edit == WorkbenchEdit::PaneWidgets
        );
        assert_eq!(
            daemon.call(request).await.unwrap(),
            ResponsePayload::WorkbenchConflict(saved)
        );
    }
    daemon.finish().await;
}

#[tokio::test]
async fn saved_project_setup_survives_reopen_without_inheriting_a_workspace_import() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let workspace_id = daemon.workspace(root).await;
    let mut document = daemon.workbench(workspace_id).await.document;
    document.assignments = runner("Imported runner");
    let imported = document.clone();
    let replace = |revision, document| ClientMessage::ApplyWorkspaceImport {
        workspace_id,
        revision,
        incoming: document,
        linked: false,
        resolutions: Default::default(),
    };
    let ResponsePayload::Workbench(initial) = daemon.call(replace(0, document)).await.unwrap()
    else {
        panic!("expected imported workbench");
    };
    let latest = runner("Saved project runner");
    daemon
        .call(ClientMessage::SetProjectSetup {
            workspace_id,
            assignments: latest.clone(),
            expected: None,
        })
        .await
        .unwrap();
    let saved = daemon.workbench(workspace_id).await;
    assert_eq!(saved.revision, initial.revision + 1);
    assert_eq!(saved.document.assignments, latest);
    assert_eq!(
        daemon
            .call(replace(initial.revision, imported.clone()))
            .await
            .unwrap(),
        ResponsePayload::WorkbenchConflict(saved.clone())
    );
    daemon.finish().await;

    let mut daemon = DaemonFixture::start(root).await;
    assert_eq!(daemon.workbench(workspace_id).await, saved);
    let ResponsePayload::Workbench(local) = daemon
        .call(replace(saved.revision, imported.clone()))
        .await
        .unwrap()
    else {
        panic!("expected workspace-local replacement");
    };
    assert_eq!(local.document.assignments, imported.assignments);
    daemon
        .call(ClientMessage::CloseWorkspace {
            id: workspace_id,
            compose_down: false,
            remove_worktree: false,
            force_remove_worktree: false,
        })
        .await
        .unwrap();
    daemon.finish().await;

    let mut daemon = DaemonFixture::start(root).await;
    let ResponsePayload::RecentProjects(recent) = daemon
        .call(ClientMessage::ListRecentProjects)
        .await
        .unwrap()
    else {
        panic!("expected recent projects");
    };
    assert!(
        recent
            .iter()
            .any(|project| project.root_dir == root && project.has_setup)
    );
    let reopened = daemon.workspace(root).await;
    assert_ne!(workspace_id, reopened);
    assert_eq!(
        daemon.workbench(reopened).await.document.assignments,
        latest
    );
    daemon.finish().await;
}

#[tokio::test]
async fn first_layout_preserves_newer_project_setup_and_is_exported_after_restart() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let workspace_id = daemon.workspace(root).await;
    let mut initial = daemon.workbench(workspace_id).await;
    assert_eq!(initial.revision, 0);
    assert!(initial.document.assignments.is_empty());
    let assignments = vec![Assignment {
        id: "agent".into(),
        name: "Agent setup".into(),
        role: PaneRole::Agent,
        argv: vec!["agent".into()],
    }];
    daemon
        .call(ClientMessage::SetProjectSetup {
            workspace_id,
            assignments: assignments.clone(),
            expected: None,
        })
        .await
        .unwrap();
    initial.document.layout.split_view = true;
    let reply = daemon
        .call(ClientMessage::UpdateWorkbench {
            workspace_id,
            revision: 0,
            document: initial.document,
            layout_only: true,
            edit: None,
        })
        .await
        .unwrap();
    let ResponsePayload::Workbench(saved) = reply else {
        panic!("expected the first layout save")
    };
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.document.assignments, assignments);
    assert!(saved.document.layout.split_view);
    daemon.finish().await;

    let mut daemon = DaemonFixture::start(root).await;
    assert_eq!(daemon.workbench(workspace_id).await, saved);
    let ResponsePayload::WorkbenchToml(export) = daemon
        .call(ClientMessage::ExportWorkspace { workspace_id })
        .await
        .unwrap()
    else {
        panic!("expected export")
    };
    assert!(export.contains("split_view = true"));
    daemon.finish().await;
}

#[tokio::test]
async fn rejected_workbench_write_preserves_the_last_confirmed_document() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let workspace_id = daemon.workspace(root).await;
    let initial = WorkspaceDocument::new("Saved".into());
    let seed = ClientMessage::SeedLegacyWorkbench {
        workspace_id,
        document: initial.clone(),
    };
    assert!(
        matches!(daemon.call(seed.clone()).await.unwrap(), ResponsePayload::Workbench(state) if state.revision == 1)
    );
    assert!(
        matches!(daemon.call(seed).await.unwrap(), ResponsePayload::WorkbenchConflict(state) if state.revision == 1)
    );
    let path = root.join("workbench").join(format!("{workspace_id}.json"));
    let previous = std::fs::read(&path).unwrap();
    let parent = path.parent().unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let rejected = daemon
        .call(ClientMessage::UpdateWorkbench {
            workspace_id,
            revision: 1,
            document: WorkspaceDocument::new("Rejected".into()),
            layout_only: false,
            edit: None,
        })
        .await;
    assert!(matches!(rejected, Err(ProtoError::IoError(_))));
    assert_eq!(daemon.workbench(workspace_id).await.document, initial);
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, &previous).unwrap();
    daemon.finish().await;

    let mut restarted = DaemonFixture::start(root).await;
    assert_eq!(restarted.workbench(workspace_id).await.revision, 1);
    assert_eq!(restarted.workbench(workspace_id).await.document, initial);
    restarted.finish().await;

    let invalid = b"this is not a saved workbench";
    std::fs::write(&path, invalid).unwrap();
    let mut restarted = DaemonFixture::start(root).await;
    let result = restarted
        .call(ClientMessage::GetWorkbench { workspace_id })
        .await;
    assert!(
        matches!(result, Err(ProtoError::IoError(message)) if message.contains("invalid workbench data"))
    );
    assert_eq!(std::fs::read(path).unwrap(), invalid);
    restarted.finish().await;
}

#[tokio::test]
async fn shared_import_reports_repo_availability_and_rejects_both_forms_of_stale_preview() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let workspace_id = daemon.workspace(root).await;
    let status = ClientMessage::CheckWorkspaceRepo { workspace_id };
    assert_eq!(
        daemon.call(status.clone()).await.unwrap(),
        ResponsePayload::WorkspaceRepoStatus(None)
    );
    let config = root.join(".daedalus-workspace.toml");
    std::fs::write(&config, "[malformed").unwrap();
    assert_eq!(
        daemon.call(status.clone()).await.unwrap(),
        ResponsePayload::WorkspaceRepoStatus(Some(true))
    );
    let invalid = daemon
        .call(ClientMessage::PreviewWorkspaceImport {
            workspace_id,
            source: WorkspaceImportSource::Repo,
            linked: true,
        })
        .await;
    assert!(matches!(invalid, Err(ProtoError::InvalidRequest(_))));
    let linked_text = daemon
        .call(ClientMessage::PreviewWorkspaceImport {
            workspace_id,
            source: WorkspaceImportSource::Text("version = 1\nname = 'Invalid link'".into()),
            linked: true,
        })
        .await;
    assert!(
        matches!(linked_text, Err(ProtoError::InvalidRequest(message)) if message.contains("imported from the repo"))
    );

    std::fs::write(&config, "version = 1\nname = 'Initial'\n").unwrap();
    let preview = daemon.repo_preview(workspace_id).await;
    std::fs::write(&config, "version = 1\nname = 'Changed'\n").unwrap();
    assert!(
        matches!(daemon.apply(preview).await, Err(ProtoError::InvalidRequest(message)) if message.contains("file changed"))
    );
    assert_eq!(daemon.workbench(workspace_id).await.revision, 0);

    let stale = daemon.repo_preview(workspace_id).await;
    let response = daemon
        .call(ClientMessage::UpdateWorkbench {
            workspace_id,
            revision: 0,
            document: WorkspaceDocument::new("From another client".into()),
            layout_only: false,
            edit: None,
        })
        .await
        .unwrap();
    assert!(matches!(response, ResponsePayload::Workbench(state) if state.revision == 1));
    assert!(
        matches!(daemon.apply(stale).await.unwrap(), ResponsePayload::WorkbenchConflict(state) if state.revision == 1)
    );
    let current = daemon.repo_preview(workspace_id).await;
    assert!(
        matches!(daemon.apply(current).await.unwrap(), ResponsePayload::Workbench(state) if state.revision == 2 && state.document.name == "Changed")
    );
    assert_eq!(
        daemon.call(status).await.unwrap(),
        ResponsePayload::WorkspaceRepoStatus(Some(false))
    );
    daemon.finish().await;
}

#[tokio::test]
async fn widget_library_rejects_duplicates_and_preserves_a_concurrent_save_across_restart() {
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = DaemonFixture::start(scratch.path()).await;
    let template = WidgetTemplate {
        id: "local".into(),
        name: "Local".into(),
        definition: WidgetDefinition::default(),
    };
    let duplicate = daemon
        .call(ClientMessage::UpdateWidgetTemplates {
            revision: 0,
            templates: vec![template.clone(), template.clone()],
        })
        .await;
    assert!(
        matches!(duplicate, Err(ProtoError::InvalidRequest(message)) if message.contains("duplicate template ID"))
    );
    assert!(matches!(daemon.call(ClientMessage::UpdateWidgetTemplates {
        revision: 0, templates: vec![template.clone()],
    }).await.unwrap(), ResponsePayload::WidgetLibrary(library) if library.revision == 1));
    assert!(matches!(daemon.call(ClientMessage::UpdateWidgetTemplates {
        revision: 0, templates: Vec::new(),
    }).await.unwrap(), ResponsePayload::WidgetLibraryConflict(library) if library.revision == 1 && library.templates == [template.clone()]));
    daemon.finish().await;

    let mut daemon = DaemonFixture::start(scratch.path()).await;
    assert!(
        matches!(daemon.call(ClientMessage::ListWidgetTemplates).await.unwrap(),
        ResponsePayload::WidgetLibrary(library) if library.revision == 1 && library.templates == [template])
    );
    daemon.finish().await;
}

#[tokio::test]
async fn importing_a_new_workspace_validates_the_preview_and_rolls_back_failed_storage() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let source = WorkspaceImportSource::Text("version = 1\nname = 'Portable'\n".into());
    let missing = daemon
        .call(ClientMessage::PreviewWorkspaceCreation {
            root_dir: root.join("missing"),
            source: source.clone(),
            linked: false,
        })
        .await;
    assert!(
        matches!(missing, Err(ProtoError::InvalidRequest(message)) if message.contains("folder does not exist"))
    );
    let linked_text = daemon
        .call(ClientMessage::PreviewWorkspaceCreation {
            root_dir: root.to_path_buf(),
            source: source.clone(),
            linked: true,
        })
        .await;
    assert!(
        matches!(linked_text, Err(ProtoError::InvalidRequest(message)) if message.contains("imported from the repo"))
    );

    let config = root.join(".daedalus-workspace.toml");
    std::fs::write(&config, "version = 1\nname = 'First'\n").unwrap();
    let ResponsePayload::WorkspaceCreationPreview(stale) = daemon
        .call(ClientMessage::PreviewWorkspaceCreation {
            root_dir: root.to_path_buf(),
            source: WorkspaceImportSource::Repo,
            linked: true,
        })
        .await
        .unwrap()
    else {
        panic!("Expected linked creation preview")
    };
    std::fs::write(&config, "version = 1\nname = 'Second'\n").unwrap();
    assert!(
        matches!(daemon.call(ClientMessage::ApplyWorkspaceCreation { preview: stale }).await,
        Err(ProtoError::InvalidRequest(message)) if message.contains("file changed"))
    );

    let ResponsePayload::WorkspaceCreationPreview(preview) = daemon
        .call(ClientMessage::PreviewWorkspaceCreation {
            root_dir: root.to_path_buf(),
            source,
            linked: false,
        })
        .await
        .unwrap()
    else {
        panic!("Expected creation preview")
    };
    std::fs::write(
        root.join("workbench"),
        "file in place of configuration directory",
    )
    .unwrap();
    assert!(matches!(
        daemon
            .call(ClientMessage::ApplyWorkspaceCreation { preview })
            .await,
        Err(ProtoError::IoError(_))
    ));
    assert_eq!(
        daemon.call(ClientMessage::ListWorkspaces).await.unwrap(),
        ResponsePayload::Workspaces(Vec::new())
    );
    daemon.finish().await;
}

#[test]
fn portable_documents_reject_invalid_command_and_display_fields() {
    use daedalusd::workbench::documents::{parse_template, parse_workspace, serialize_template};

    let template = r#"
version = 1
[template]
id = 'checks'
name = 'Checks'
[[template.definition.sources]]
id = 'build'
kind = 'command'
command = { argv = ['cargo', 'check'], env = ['PATH', '_WORKBENCH_TOKEN'], timeout_seconds = 3 }
refresh = { interval_seconds = 2 }
[[template.definition.display.summary]]
source = 'build'
field = 'status'
[[template.definition.actions]]
id = 'rerun'
label = 'Run checks'
kind = 'command'
command = { argv = ['cargo', 'check'] }
"#;
    let document = parse_template(template).unwrap();
    assert_eq!(
        parse_template(&serialize_template(&document).unwrap()).unwrap(),
        document
    );
    for (old, new, message) in [
        (
            "id = 'checks'",
            "id = 'checks/invalid'",
            "invalid template ID",
        ),
        ("name = 'Checks'", "name = '  '", "requires a valid name"),
        (
            "argv = ['cargo', 'check']",
            "argv = []",
            "requires an executable",
        ),
        (
            "timeout_seconds = 3",
            "timeout_seconds = 0",
            "timeout must be positive",
        ),
        (
            "interval_seconds = 2",
            "interval_seconds = 0",
            "refresh interval must be positive",
        ),
        (
            "'_WORKBENCH_TOKEN'",
            "'PATH'",
            "duplicate environment variable",
        ),
        (
            "'_WORKBENCH_TOKEN'",
            "'3TOKEN'",
            "invalid or duplicate environment",
        ),
        (
            "'_WORKBENCH_TOKEN'",
            "'BAD-NAME'",
            "invalid or duplicate environment",
        ),
        ("source = 'build'", "source = 'absent'", "unknown source"),
        ("field = 'status'", "field = ''", "requires a field name"),
        (
            "label = 'Run checks'",
            "label = ''",
            "requires a valid name",
        ),
    ] {
        let error = parse_template(&template.replacen(old, new, 1))
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
    let duplicate_action = format!(
        "{template}\n[[template.definition.actions]]\nid = 'rerun'\nlabel = 'Next'\nkind = 'diff'\n"
    );
    assert!(
        parse_template(&duplicate_action)
            .unwrap_err()
            .to_string()
            .contains("duplicate action")
    );
    let split = "version = 1\nname = 'Workspace'\n[layout.splits]\nleft = 1000";
    assert!(
        parse_workspace(split)
            .unwrap_err()
            .to_string()
            .contains("must be between 1 and 999")
    );
    let widget =
        "version = 1\nname = 'Workspace'\n[[widgets]]\nid = 'empty'\nname = 'Blank'\npinned = true";
    assert!(
        parse_workspace(widget)
            .unwrap_err()
            .to_string()
            .contains("requires a template or a definition")
    );
}

#[tokio::test]
async fn linked_import_applies_the_explicit_choices_from_a_renewed_preview() {
    use daedalus_proto::workbench::{ConflictChoice, SHARED_WORKSPACE_FILE, WidgetInstance};
    use daedalusd::workbench::documents::serialize_workspace;

    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut daemon = DaemonFixture::start(root).await;
    let workspace_id = daemon.workspace(root).await;
    let config = root.join(SHARED_WORKSPACE_FILE);

    let mut base = WorkspaceDocument::new("Initial setup".into());
    base.templates.push(WidgetTemplate {
        id: "repo-status".into(),
        name: "Repo status".into(),
        definition: WidgetDefinition::default(),
    });
    std::fs::write(&config, serialize_workspace(&base).unwrap()).unwrap();
    let original = daemon.repo_preview(workspace_id).await;
    assert!(original.conflicts.is_empty());
    let ResponsePayload::Workbench(linked) = daemon.apply(original).await.unwrap() else {
        panic!("expected linked workbench");
    };

    let mut local = linked.document;
    local.name = "Local name".into();
    local.widgets.push(WidgetInstance {
        id: "local-report".into(),
        name: "Local report".into(),
        pinned: true,
        template: Some("repo-status".into()),
        definition: None,
    });
    let ResponsePayload::Workbench(edited) = daemon
        .call(ClientMessage::UpdateWorkbench {
            workspace_id,
            revision: linked.revision,
            document: local.clone(),
            layout_only: false,
            edit: None,
        })
        .await
        .unwrap()
    else {
        panic!("expected local edit");
    };

    let mut incoming = base;
    incoming.name = "Repo name".into();
    incoming.templates.clear();
    std::fs::write(&config, serialize_workspace(&incoming).unwrap()).unwrap();
    let preview = daemon.repo_preview(workspace_id).await;
    assert_eq!(preview.revision, edited.revision);
    assert_eq!(preview.current, local);
    assert_eq!(preview.proposed, local);
    let paths = preview
        .conflicts
        .iter()
        .map(|conflict| conflict.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        ["/name", "/widgets/local-report/$template_reference"]
    );

    let apply =
        |preview: WorkspaceImportPreview, resolutions| ClientMessage::ApplyWorkspaceImport {
            workspace_id,
            revision: preview.revision,
            incoming: preview.incoming,
            linked: preview.linked,
            resolutions,
        };
    let incomplete = daemon
        .call(apply(
            preview.clone(),
            [(paths[0].into(), ConflictChoice::Incoming)].into(),
        ))
        .await;
    assert!(
        matches!(incomplete, Err(ProtoError::InvalidRequest(message))
        if message == "Resolve workspace conflicts at /widgets/local-report/$template_reference")
    );
    assert_eq!(daemon.workbench(workspace_id).await, edited);

    let renewed = daemon.repo_preview(workspace_id).await;
    assert_eq!(renewed, preview);
    let resolutions: std::collections::BTreeMap<_, _> = [
        (paths[0].into(), ConflictChoice::Incoming),
        (paths[1].into(), ConflictChoice::Local),
    ]
    .into();
    let resolve = |revision| ClientMessage::ResolveWorkspaceImport {
        workspace_id,
        revision,
        incoming: renewed.incoming.clone(),
        linked: renewed.linked,
        resolutions: resolutions.clone(),
    };
    assert!(matches!(
        daemon.call(resolve(renewed.revision - 1)).await,
        Err(ProtoError::InvalidRequest(message)) if message.contains("Preview the import again")
    ));
    let ResponsePayload::WorkspaceImportPreview(reviewed) =
        daemon.call(resolve(renewed.revision)).await.unwrap()
    else {
        panic!("expected resolved import review");
    };
    assert_eq!(reviewed.revision, renewed.revision);
    assert!(reviewed.conflicts.is_empty());
    assert_eq!(reviewed.proposed.name, "Repo name");
    assert_eq!(daemon.workbench(workspace_id).await, edited);
    let ResponsePayload::Workbench(saved) = daemon.call(apply(renewed, resolutions)).await.unwrap()
    else {
        panic!("expected resolved workbench");
    };
    assert_eq!(saved.revision, edited.revision + 1);
    assert_eq!(saved.document, reviewed.proposed);
    assert_eq!(saved.document.name, "Repo name");
    assert_eq!(saved.document.widgets, local.widgets);
    assert_eq!(saved.document.templates, local.templates);
    assert_eq!(saved.repo_baseline, Some(incoming));
    assert!(daemon.repo_preview(workspace_id).await.conflicts.is_empty());
    daemon.finish().await;
}
