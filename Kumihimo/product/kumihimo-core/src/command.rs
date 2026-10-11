use crate::{
    EntityId, FrameId, GroupId, Limits, OperationId, ProviderId, ResourceId, ViewId, WorkspaceId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    CreateWorkspace {
        owner: Option<FrameId>,
    },
    CreateFrame {
        workspace: WorkspaceId,
        index: usize,
    },
    RegisterProvider {
        supports_shared_views: bool,
    },
    RegisterResource {
        provider: ProviderId,
    },
    CreateView {
        frame: FrameId,
        resource: ResourceId,
        index: usize,
    },
    CreateGroup {
        frame: FrameId,
        label: String,
        color: Option<String>,
    },
    AssignGroup {
        view: ViewId,
        group: Option<GroupId>,
    },
    SelectView {
        view: ViewId,
    },
    MoveFrame {
        frame: FrameId,
        destination: WorkspaceId,
        index: usize,
    },
    MoveView {
        view: ViewId,
        destination: FrameId,
        index: usize,
    },
    SetLimits(Limits),
    SetLayout {
        workspace: WorkspaceId,
        plan: crate::LayoutPlan,
    },
    SetGeometryContext {
        workspace: WorkspaceId,
        context: crate::GeometryContext,
    },
    CreateTool(crate::ToolSpec),
    ConfigureTool {
        tool: crate::ToolId,
        policy: crate::ToolPolicy,
    },
    SetToolState {
        tool: crate::ToolId,
        state: crate::TargetState,
    },
    RebindTool {
        tool: crate::ToolId,
        target: crate::EditingTarget,
    },
    Dock(crate::DockIntent),
    CommitPreview(crate::DockPreview),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub actor: String,
    pub operation: OperationId,
    pub expected_generation: u64,
    pub command: Command,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Budget {
    Records,
    Depth,
    ViewsPerFrame,
    LayoutEntries,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    EntityId,
    Generation,
    Depth,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Missing(EntityId),
    ChildWorkspaceExists(FrameId),
    Cycle {
        frame: FrameId,
        destination: WorkspaceId,
    },
    InvalidIndex {
        index: usize,
        available: usize,
    },
    SharingDenied(ResourceId),
    GroupFrameMismatch {
        view: ViewId,
        group: GroupId,
    },
    Admission {
        budget: Budget,
        limit: usize,
        attempted: usize,
    },
    StaleGeneration {
        expected: u64,
        actual: u64,
    },
    IntentMismatch(OperationId),
    ExpiredOperation(OperationId),
    ZeroOperation,
    Exhausted(Counter),
    Geometry(crate::GeometryError),
    ForbiddenDestination,
    IncompatibleTarget,
    StalePreview {
        expected: u64,
        actual: u64,
    },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Created(EntityId),
    Moved(EntityId),
    Selected(ViewId),
    Grouped(ViewId),
    LimitsChanged,
    Unchanged,
    LayoutChanged(WorkspaceId),
    ContextChanged(WorkspaceId),
    ToolChanged(crate::ToolId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub actor: String,
    pub operation: OperationId,
    pub generation: u64,
    pub result: Result<Change, Error>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub outcome: Outcome,
    pub replayed: bool,
}

/// Operation bookkeeping is separate from canonical model snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerStatus {
    pub high_watermark: OperationId,
    pub capacity: usize,
    pub retained_operations: Vec<OperationId>,
}
