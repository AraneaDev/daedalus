use daedalus_proto::{
    Cell, CellFlags, Color, GridRow, PaneId, PaneRole, PaneStatus, PaneSummary, WorkspaceId,
    WorkspaceInfo,
};
use std::{collections::HashMap, path::PathBuf};

pub(super) fn make_row(row_index: u16, abs_row: u64, text: &str) -> GridRow {
    GridRow {
        row_index,
        abs_row,
        cells: text
            .chars()
            .map(|ch| Cell {
                text: ch.to_string(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 1,
            })
            .collect(),
    }
}

pub(super) fn row_text(row: &GridRow) -> String {
    row.cells.iter().map(|c| c.text.as_str()).collect()
}

pub(super) fn make_pane_summary(ws_id: WorkspaceId, role: PaneRole) -> PaneSummary {
    PaneSummary {
        id: PaneId::new(),
        workspace_id: ws_id,
        role,
        status: PaneStatus::Idle,
        cols: 80,
        rows: 24,
        cwd: Some(PathBuf::from("/tmp/repo")),
        last_exit_code: None,
        has_failure_trace: false,
        agent_state: None,
    }
}

pub(super) fn make_workspace_with_three_panes() -> (WorkspaceInfo, PaneId, PaneId, PaneId) {
    let ws_id = WorkspaceId::new();
    let agent = make_pane_summary(ws_id, PaneRole::Agent);
    let service = make_pane_summary(ws_id, PaneRole::Service);
    let runner = make_pane_summary(ws_id, PaneRole::Runner);
    let ids = (agent.id, service.id, runner.id);
    let ws = WorkspaceInfo {
        project: Default::default(),
        default_panes_pending: None,
        id: ws_id,
        name: "demo".to_string(),
        root_dir: PathBuf::from("/tmp/repo"),
        slot_index: 0,
        injected_env: HashMap::new(),
        panes: vec![agent, service, runner],
        port_offset: 0,
        compose_project_name: None,
        git_branch: None,
        git_status: None,
        bound_ports: Vec::new(),
        containers: Vec::new(),
        external_statuses: Vec::new(),
        manifest: None,
    };
    (ws, ids.0, ids.1, ids.2)
}
