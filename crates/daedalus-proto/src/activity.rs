use crate::{AgentState, PaneId, PaneRole, StatusLevel, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCursor {
    pub epoch: WorkspaceId,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPage {
    pub workspace_id: WorkspaceId,
    pub cursor: ActivityCursor,
    pub gap: bool,
    pub entries: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub seq: u64,
    pub at_ms: u64,
    pub kind: ActivityKind,
}

/// Bounded metadata only. Terminal bytes, commands, paths, status labels and details
/// are deliberately absent from the history representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityKind {
    AgentOutput {
        pane_id: PaneId,
        abs_row: u64,
        pane_seq: u64,
        history_generation: u64,
    },
    Agent {
        pane_id: PaneId,
        state: AgentState,
    },
    Command {
        pane_id: PaneId,
        role: PaneRole,
        exit_code: i32,
    },
    Exit {
        pane_id: PaneId,
        role: PaneRole,
        exit_code: Option<i32>,
    },
    Ports {
        bound: usize,
    },
    Containers {
        total: usize,
        unhealthy: usize,
    },
    Git,
    External {
        level: StatusLevel,
    },
    ExternalCleared,
}

impl fmt::Display for ActivityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentOutput { .. } => f.write_str("Agent pane produced output"),
            Self::Agent { state, .. } => f.write_str(match state {
                AgentState::Idle => "Agent became idle",
                AgentState::Thinking => "Agent started thinking",
                AgentState::WaitingForApproval => "Check agent · approval signal",
                AgentState::Error => "Agent reported an error",
            }),
            Self::Command {
                role, exit_code, ..
            } => write!(f, "{role} command · exit {exit_code}"),
            Self::Exit {
                role,
                exit_code: Some(code),
                ..
            } => write!(f, "{role} exited · code {code}"),
            Self::Exit { role, .. } => write!(f, "{role} exited"),
            Self::Ports { bound } => write!(f, "Ports changed · {bound} bound"),
            Self::Containers { total, unhealthy } => {
                write!(f, "Containers · {total} reported, {unhealthy} unhealthy")
            }
            Self::Git => f.write_str("Git status changed"),
            Self::External { level } => write!(f, "External status · {level:?}"),
            Self::ExternalCleared => f.write_str("External status cleared"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResponsePayload;

    #[test]
    fn previous_daemon_pong_defaults_to_no_journal() {
        #[derive(Serialize)]
        enum OldPayload {
            Pong {
                version: String,
                vt_backend: String,
                isolation_mode: String,
            },
        }
        let bytes = rmp_serde::to_vec_named(&OldPayload::Pong {
            version: "old".into(),
            vt_backend: "test".into(),
            isolation_mode: "local".into(),
        })
        .unwrap();
        let pong: ResponsePayload = rmp_serde::from_slice(&bytes).unwrap();
        assert!(matches!(
            pong,
            ResponsePayload::Pong {
                journal_supported: false,
                ..
            }
        ));
    }
}
