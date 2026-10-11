use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditingTarget {
    pub view: ViewId,
    pub resource: ResourceId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolAnchor {
    View(ViewId),
    Frame(FrameId),
    Workspace(WorkspaceId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolPlacement {
    Edge {
        anchor: ToolAnchor,
        edge: Edge,
    },
    Cell {
        workspace: WorkspaceId,
        grid: NodeId,
        row: usize,
        column: usize,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingPolicy {
    Pinned,
    ActiveCompatible,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibilityPolicy {
    TargetVisibleUsable,
    OriginVisibleUsable,
    Always,
    Hidden,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiddenSpace {
    Release,
    Retain,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TargetState {
    pub target_visible: bool,
    pub target_usable: bool,
    pub origin_visible: bool,
    pub origin_usable: bool,
    pub applicable: bool,
    pub keyboard_focused: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPolicy {
    pub binding: BindingPolicy,
    pub visibility: VisibilityPolicy,
    pub hidden_space: HiddenSpace,
    pub orientation: Axis,
    pub orientations: Vec<Axis>,
    pub preferred: Size,
    pub minimum: Size,
    pub order: i64,
    pub allowed: Vec<ToolPlacement>,
    pub compatible_targets: Vec<EditingTarget>,
}
impl ToolPolicy {
    pub fn new(preferred: Size, minimum: Size, allowed: Vec<ToolPlacement>) -> Self {
        Self {
            binding: BindingPolicy::Pinned,
            visibility: VisibilityPolicy::TargetVisibleUsable,
            hidden_space: HiddenSpace::Release,
            orientation: Axis::Horizontal,
            orientations: vec![Axis::Horizontal],
            preferred,
            minimum,
            order: 0,
            allowed,
            compatible_targets: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    pub owner: ProviderId,
    pub origin: ViewId,
    pub target: EditingTarget,
    pub placement: ToolPlacement,
    pub policy: ToolPolicy,
    pub state: TargetState,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub id: ToolId,
    pub owner: ProviderId,
    pub origin: ViewId,
    pub target: EditingTarget,
    pub placement: ToolPlacement,
    pub policy: ToolPolicy,
    pub state: TargetState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolPresentation {
    pub visible: bool,
    pub usable: bool,
    pub applicable: bool,
    pub reserves_space: bool,
}
impl Tool {
    pub fn presentation(&self) -> ToolPresentation {
        let visible = match self.policy.visibility {
            VisibilityPolicy::TargetVisibleUsable => {
                self.state.target_visible && self.state.target_usable
            }
            VisibilityPolicy::OriginVisibleUsable => {
                self.state.origin_visible && self.state.origin_usable
            }
            VisibilityPolicy::Always => true,
            VisibilityPolicy::Hidden => false,
        };
        ToolPresentation {
            visible,
            usable: self.state.target_usable,
            applicable: self.state.applicable && self.state.target_usable,
            reserves_space: visible || self.policy.hidden_space == HiddenSpace::Retain,
        }
    }
}
impl Snapshot {
    pub(crate) fn target_valid(&self, target: EditingTarget) -> Result<(), Error> {
        let view = self
            .views
            .get(&target.view)
            .ok_or(Error::Missing(EntityId::View(target.view)))?;
        if view.resource != target.resource {
            return Err(Error::IncompatibleTarget);
        }
        Ok(())
    }
    pub(crate) fn validate_tool(&self, t: &Tool) -> Result<(), Error> {
        if !self.providers.contains_key(&t.owner) {
            return Err(Error::Missing(EntityId::Provider(t.owner)));
        }
        if !self.views.contains_key(&t.origin) {
            return Err(Error::Missing(EntityId::View(t.origin)));
        }
        if self.resources[&self.views[&t.origin].resource].provider != t.owner {
            return Err(Error::IncompatibleTarget);
        }
        self.target_valid(t.target)?;
        if !t.policy.allowed.contains(&t.placement)
            || !t.policy.orientations.contains(&t.policy.orientation)
        {
            return Err(Error::ForbiddenDestination);
        }
        if t.policy.preferred.width() < t.policy.minimum.width()
            || t.policy.preferred.height() < t.policy.minimum.height()
        {
            return Err(GeometryError::Infeasible.into());
        }
        for target in &t.policy.compatible_targets {
            self.target_valid(*target)?;
        }
        for placement in &t.policy.allowed {
            self.validate_tool_destination(*placement)?;
        }
        self.validate_tool_destination(t.placement)?;
        let workspace = match t.placement {
            ToolPlacement::Cell { workspace, .. } => workspace,
            ToolPlacement::Edge { anchor, .. } => match anchor {
                ToolAnchor::Workspace(id) => id,
                ToolAnchor::Frame(id) => self.frames[&id].workspace,
                ToolAnchor::View(id) => self.frames[&self.views[&id].frame].workspace,
            },
        };
        if !self.contexts.contains_key(&workspace) {
            return Err(GeometryError::MissingContext.into());
        }
        if let ToolPlacement::Edge { edge, .. } = t.placement {
            let expected = match edge {
                Edge::Top | Edge::Bottom => Axis::Horizontal,
                _ => Axis::Vertical,
            };
            if expected != t.policy.orientation {
                return Err(Error::ForbiddenDestination);
            }
        }
        Ok(())
    }
    pub(crate) fn validate_tool_destination(&self, placement: ToolPlacement) -> Result<(), Error> {
        match placement {
            ToolPlacement::Edge { anchor, .. } => match anchor {
                ToolAnchor::Workspace(id) => {
                    if !self.workspaces.contains_key(&id) {
                        return Err(Error::Missing(EntityId::Workspace(id)));
                    }
                }
                ToolAnchor::Frame(id) => {
                    if !self.frames.contains_key(&id) {
                        return Err(Error::Missing(EntityId::Frame(id)));
                    }
                }
                ToolAnchor::View(id) => {
                    if !self.views.contains_key(&id) {
                        return Err(Error::Missing(EntityId::View(id)));
                    }
                }
            },
            ToolPlacement::Cell {
                workspace,
                grid,
                row,
                column,
            } => {
                let plan = self
                    .layouts
                    .get(&workspace)
                    .ok_or(GeometryError::InvalidPlan(
                        "cell tool needs configured plan",
                    ))?;
                let Some(LayoutNode::Grid { rows, columns, .. }) = plan.nodes.get(&grid) else {
                    return Err(GeometryError::InvalidPlan("destination grid absent").into());
                };
                if row >= rows.len() || column >= columns.len() {
                    return Err(GeometryError::InvalidPlan("cell outside grid").into());
                }
            }
        }
        Ok(())
    }
}
