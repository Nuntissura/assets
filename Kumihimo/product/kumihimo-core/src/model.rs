use crate::{FrameId, GroupId, ProviderId, ResourceId, ViewId, WorkspaceId};
use std::collections::BTreeMap;

/// Practical host policy; None leaves that dimension unrestricted by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Global sum of workspace, frame, provider, resource, group and view records.
    pub max_records: Option<usize>,
    /// Root workspace depth is 0; frames add one, owned workspaces share owner depth.
    pub max_depth: Option<usize>,
    pub max_views_per_frame: Option<usize>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: Some(16_384),
            max_depth: None,
            max_views_per_frame: None,
        }
    }
}
impl Limits {
    /// Explicit host opt-in: the host manages all resource admission externally.
    pub const fn unbounded() -> Self {
        Self {
            max_records: None,
            max_depth: None,
            max_views_per_frame: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub owner: Option<FrameId>,
    pub frames: Vec<FrameId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: FrameId,
    pub workspace: WorkspaceId,
    pub child_workspace: Option<WorkspaceId>,
    pub views: Vec<ViewId>,
    pub selected_view: Option<ViewId>,
    pub groups: Vec<GroupId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: ProviderId,
    pub supports_shared_views: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub id: ResourceId,
    pub provider: ProviderId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub id: ViewId,
    pub frame: FrameId,
    pub resource: ResourceId,
    pub group: Option<GroupId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub frame: FrameId,
    pub label: String,
    pub color: Option<String>,
}

/// Read-only through Core. A cloned snapshot is detached diagnostic data, not a restore API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub version: u32,
    pub generation: u64,
    pub limits: Limits,
    pub workspaces: BTreeMap<WorkspaceId, Workspace>,
    pub frames: BTreeMap<FrameId, Frame>,
    pub providers: BTreeMap<ProviderId, Provider>,
    pub resources: BTreeMap<ResourceId, Resource>,
    pub views: BTreeMap<ViewId, View>,
    pub groups: BTreeMap<GroupId, Group>,
}
impl Snapshot {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            version: 1,
            generation: 0,
            limits,
            workspaces: BTreeMap::new(),
            frames: BTreeMap::new(),
            providers: BTreeMap::new(),
            resources: BTreeMap::new(),
            views: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }
    pub fn record_count(&self) -> usize {
        self.workspaces.len()
            + self.frames.len()
            + self.providers.len()
            + self.resources.len()
            + self.views.len()
            + self.groups.len()
    }
}
