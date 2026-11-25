use daedalus_proto::transport::IpcStream;
use daedalus_proto::workbench::{
    WidgetDefinition, WidgetTemplate, WorkbenchState, WorkspaceDocument, WorkspaceImportPreview,
    WorkspaceImportSource,
};
use daedalus_proto::{
    ClientMessage, ProtoError, Request, ResponsePayload, ServerMessage, WorkspaceId, read_frame,
    write_frame,
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
    let ResponsePayload::Workbench(saved) = daemon
        .call(apply(
            renewed,
            [
                (paths[0].into(), ConflictChoice::Incoming),
                (paths[1].into(), ConflictChoice::Local),
            ]
            .into(),
        ))
        .await
        .unwrap()
    else {
        panic!("expected resolved workbench");
    };
    assert_eq!(saved.revision, edited.revision + 1);
    assert_eq!(saved.document.name, "Repo name");
    assert_eq!(saved.document.widgets, local.widgets);
    assert_eq!(saved.document.templates, local.templates);
    assert_eq!(saved.repo_baseline, Some(incoming));
    assert!(daemon.repo_preview(workspace_id).await.conflicts.is_empty());
    daemon.finish().await;
}
