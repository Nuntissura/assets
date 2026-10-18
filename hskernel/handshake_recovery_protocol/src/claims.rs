use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimScope {
    Worktree {
        worktree_id: String,
    },
    Workspace {
        workspace_id: String,
    },
    RichDocument {
        workspace_id: String,
        document_id: String,
    },
    GraphMutation {
        workspace_id: String,
        graph_id: String,
    },
    IndexRun {
        workspace_id: String,
        source_root_id: String,
    },
}

impl ClaimScope {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Worktree { .. } => "worktree",
            Self::Workspace { .. } => "workspace",
            Self::RichDocument { .. } => "rich_document",
            Self::GraphMutation { .. } => "graph_mutation",
            Self::IndexRun { .. } => "index_run",
        }
    }

    pub fn scope_id(&self) -> String {
        match self {
            Self::Worktree { worktree_id } => worktree_id.clone(),
            Self::Workspace { workspace_id } => workspace_id.clone(),
            Self::RichDocument {
                workspace_id,
                document_id,
            } => format!("{workspace_id}/{document_id}"),
            Self::GraphMutation {
                workspace_id,
                graph_id,
            } => format!("{workspace_id}/{graph_id}"),
            Self::IndexRun {
                workspace_id,
                source_root_id,
            } => format!("{workspace_id}/{source_root_id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Active,
    Held,
    Released,
    Reclaimed,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Held => "held",
            Self::Released => "released",
            Self::Reclaimed => "reclaimed",
        }
    }

    pub fn parse<E>(value: &str, unknown: impl FnOnce(String) -> E) -> Result<Self, E> {
        match value {
            "active" => Ok(Self::Active),
            "held" => Ok(Self::Held),
            "released" => Ok(Self::Released),
            "reclaimed" => Ok(Self::Reclaimed),
            other => Err(unknown(format!("unknown claim status: {other}"))),
        }
    }
}
