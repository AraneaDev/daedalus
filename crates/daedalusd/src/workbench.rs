pub(crate) mod command;
pub mod documents;
pub mod merge;
pub mod runtime;
pub(crate) mod scheduler;
pub(crate) mod sources;
pub(crate) mod store;
pub(crate) mod templates;

#[cfg(test)]
fn test_workspace(root_dir: std::path::PathBuf) -> daedalus_proto::WorkspaceInfo {
    daedalus_proto::WorkspaceInfo {
        id: daedalus_proto::WorkspaceId::new(),
        name: "Widget fixture".into(),
        root_dir,
        slot_index: 0,
        injected_env: Default::default(),
        panes: Vec::new(),
        default_panes_pending: None,
        port_offset: 0,
        compose_project_name: None,
        git_branch: None,
        git_status: None,
        bound_ports: Vec::new(),
        containers: Vec::new(),
        external_statuses: Vec::new(),
        manifest: None,
        project: Default::default(),
        setup: None,
    }
}
