use crate::*;

pub const SAVED_SCHEMA: u32 = 1;

macro_rules! saved {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        #[cfg_attr(feature="serde", derive(serde::Serialize, serde::Deserialize))]
        #[cfg_attr(feature="serde", serde(deny_unknown_fields))]
        pub struct $name { $(pub $field: $ty),* }
    };
}
saved!(SavedWorkspace { id: WorkspaceId, owner: Option<FrameId>, frames: Vec<FrameId> });
saved!(SavedFrame { id: FrameId, workspace: WorkspaceId, child_workspace: Option<WorkspaceId>, views: Vec<ViewId>, selected_view: Option<ViewId>, groups: Vec<GroupId> });
saved!(SavedProvider {
    id: ProviderId,
    key: String,
    supports_shared_views: bool
});
saved!(SavedResource {
    id: ResourceId,
    provider: ProviderId,
    key: String,
    restore_reference: String
});
saved!(SavedView { id: ViewId, frame: FrameId, resource: ResourceId, group: Option<GroupId>, label: String, pinned: bool, unsaved: bool, closed: Option<ClosedView> });
saved!(SavedGroup { id: GroupId, frame: FrameId, label: String, color: Option<String>, description: String, collapsed: bool });
saved!(SavedTool {
    id: ToolId,
    owner: ProviderId,
    origin: ViewId,
    target: EditingTarget,
    placement: ToolPlacement,
    policy: ToolPolicy
});
saved!(SavedPlan {
    workspace: WorkspaceId,
    plan: LayoutPlan
});
saved!(SavedContext {
    workspace: WorkspaceId,
    context: GeometryContext
});
saved!(SavedFloat {
    frame: FrameId,
    floating: FloatingFrame
});
saved!(SavedMaximize {
    workspace: WorkspaceId,
    frame: FrameId
});

/// Schema 1 is the first durable format. Snapshot versions were never file formats.
/// Lists retain duplicate input until validation. No runtime handles or host flags are saved.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct PersistedLayout {
    pub schema: u32,
    pub source: Identity,
    #[cfg_attr(feature = "serde", serde(with = "crate::decimal"))]
    pub source_generation: u64,
    #[cfg_attr(feature = "serde", serde(with = "crate::decimal"))]
    pub next_entity: u64,
    pub limits: Limits,
    pub pin_policy: PinPolicy,
    pub workspaces: Vec<SavedWorkspace>,
    pub frames: Vec<SavedFrame>,
    pub providers: Vec<SavedProvider>,
    pub resources: Vec<SavedResource>,
    pub views: Vec<SavedView>,
    pub groups: Vec<SavedGroup>,
    pub tools: Vec<SavedTool>,
    pub layouts: Vec<SavedPlan>,
    pub contexts: Vec<SavedContext>,
    pub floats: Vec<SavedFloat>,
    pub maximized: Vec<SavedMaximize>,
}

/// Immutable source stamp. A host must check it immediately before publishing a save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStamp {
    identity: Identity,
    generation: u64,
}
impl SaveStamp {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveData {
    stamp: SaveStamp,
    layout: PersistedLayout,
}
impl SaveData {
    pub fn stamp(&self) -> &SaveStamp {
        &self.stamp
    }
    pub fn layout(&self) -> &PersistedLayout {
        &self.layout
    }
    pub fn into_layout(self) -> PersistedLayout {
        self.layout
    }
}
impl Core {
    pub fn save_stamp(&self) -> SaveStamp {
        SaveStamp {
            identity: self.model.identity.clone(),
            generation: self.model.generation,
        }
    }
    pub fn is_save_current(&self, stamp: &SaveStamp) -> bool {
        *stamp == self.save_stamp()
    }
    pub fn save_data(&self) -> SaveData {
        SaveData {
            stamp: self.save_stamp(),
            layout: self.save_layout(),
        }
    }
    pub fn save_layout(&self) -> PersistedLayout {
        let s = &self.model;
        PersistedLayout {
            schema: SAVED_SCHEMA,
            source: s.identity.clone(),
            source_generation: s.generation,
            next_entity: self.next_entity,
            limits: s.limits,
            pin_policy: s.pin_policy,
            workspaces: s
                .workspaces
                .values()
                .map(|v| SavedWorkspace {
                    id: v.id,
                    owner: v.owner,
                    frames: v.frames.clone(),
                })
                .collect(),
            frames: s
                .frames
                .values()
                .map(|v| SavedFrame {
                    id: v.id,
                    workspace: v.workspace,
                    child_workspace: v.child_workspace,
                    views: v.views.clone(),
                    selected_view: v.selected_view,
                    groups: v.groups.clone(),
                })
                .collect(),
            providers: s
                .providers
                .values()
                .map(|v| SavedProvider {
                    id: v.id,
                    key: v.key.clone(),
                    supports_shared_views: v.supports_shared_views,
                })
                .collect(),
            resources: s
                .resources
                .values()
                .map(|v| SavedResource {
                    id: v.id,
                    provider: v.provider,
                    key: v.key.clone(),
                    restore_reference: v.restore_reference.clone(),
                })
                .collect(),
            views: s
                .views
                .values()
                .map(|v| SavedView {
                    id: v.id,
                    frame: v.frame,
                    resource: v.resource,
                    group: v.group,
                    label: v.label.clone(),
                    pinned: v.pinned,
                    unsaved: v.unsaved,
                    closed: v.closed,
                })
                .collect(),
            groups: s
                .groups
                .values()
                .map(|v| SavedGroup {
                    id: v.id,
                    frame: v.frame,
                    label: v.label.clone(),
                    color: v.color.clone(),
                    description: v.description.clone(),
                    collapsed: v.collapsed,
                })
                .collect(),
            tools: s
                .tools
                .values()
                .map(|v| SavedTool {
                    id: v.id,
                    owner: v.owner,
                    origin: v.origin,
                    target: v.target,
                    placement: v.placement,
                    policy: v.policy.clone(),
                })
                .collect(),
            layouts: s
                .layouts
                .iter()
                .map(|(workspace, plan)| SavedPlan {
                    workspace: *workspace,
                    plan: plan.clone(),
                })
                .collect(),
            contexts: s
                .contexts
                .iter()
                .map(|(workspace, context)| SavedContext {
                    workspace: *workspace,
                    context: context.clone(),
                })
                .collect(),
            floats: s
                .floats
                .iter()
                .map(|(frame, floating)| SavedFloat {
                    frame: *frame,
                    floating: floating.clone(),
                })
                .collect(),
            maximized: s
                .maximized
                .iter()
                .map(|(workspace, frame)| SavedMaximize {
                    workspace: *workspace,
                    frame: *frame,
                })
                .collect(),
        }
    }
}
