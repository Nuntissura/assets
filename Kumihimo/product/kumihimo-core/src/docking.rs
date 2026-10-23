use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum DockLocation {
    Append {
        workspace: WorkspaceId,
    },
    WorkspaceEdge {
        workspace: WorkspaceId,
        edge: Edge,
    },
    Beside {
        frame: FrameId,
        edge: Edge,
    },
    Cell {
        workspace: WorkspaceId,
        grid: NodeId,
        row: usize,
        column: usize,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockIntent {
    Frame {
        frame: FrameId,
        location: DockLocation,
    },
    Tool {
        tool: ToolId,
        placement: ToolPlacement,
    },
    Float {
        frame: FrameId,
        rect: Rect,
    },
    Redock {
        frame: FrameId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SurfaceKind {
    InAppFloat,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub struct FloatingFrame {
    pub kind: SurfaceKind,
    pub rect: Rect,
    pub return_location: DockLocation,
}

/// Constructed only by Core::preview. Geometry is derived and never imported by commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockPreview {
    identity: Identity,
    generation: u64,
    intent: DockIntent,
    geometry: Vec<GeometryOutput>,
}
impl DockPreview {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn intent(&self) -> &DockIntent {
        &self.intent
    }
    pub fn geometry(&self) -> &[GeometryOutput] {
        &self.geometry
    }
    /// Consuming a preview cancels it without access to or mutation of the core.
    pub fn cancel(self) {}
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapCandidate {
    pub location: DockLocation,
    pub distance: Scalar,
}
impl Core {
    pub fn preview(&self, intent: DockIntent) -> Result<DockPreview, Error> {
        let mut candidate = self.model_candidate();
        candidate.mutate(Command::Dock(intent.clone()))?;
        let geometry = candidate
            .model
            .contexts
            .keys()
            .filter(|id| candidate.model.geometry_workspace_visible(**id))
            .map(|id| candidate.model.geometry(*id))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DockPreview {
            identity: self.model.identity.clone(),
            generation: self.model.generation,
            intent,
            geometry,
        })
    }
    pub(crate) fn preview_command(
        preview: DockPreview,
        generation: u64,
        identity: &Identity,
    ) -> Result<DockIntent, Error> {
        if &preview.identity != identity {
            return Err(Error::IdentityMismatch);
        }
        if preview.generation != generation {
            return Err(Error::StalePreview {
                expected: preview.generation,
                actual: generation,
            });
        }
        Ok(preview.intent)
    }
    pub(crate) fn dock(&mut self, intent: DockIntent) -> Result<Change, Error> {
        let before = self.model.clone();
        match intent {
            DockIntent::Frame { frame, location } => self.dock_frame(frame, location)?,
            DockIntent::Tool { tool, placement } => {
                let record = self
                    .model
                    .tools
                    .get(&tool)
                    .ok_or(Error::Missing(EntityId::Tool(tool)))?;
                if !record.policy.allowed.contains(&placement) {
                    return Err(Error::ForbiddenDestination);
                }
                let old = record.placement;
                if old == placement {
                    return Ok(Change::Unchanged);
                }
                if let ToolPlacement::Cell { workspace, .. } = old {
                    self.model
                        .layouts
                        .get_mut(&workspace)
                        .unwrap()
                        .remove(LayoutItem::Tool(tool))?;
                }
                let target = self.model.tools.get_mut(&tool).unwrap();
                target.placement = placement;
                if let ToolPlacement::Edge { edge, .. } = placement {
                    target.policy.orientation = match edge {
                        Edge::Top | Edge::Bottom => Axis::Horizontal,
                        _ => Axis::Vertical,
                    };
                }
                if let ToolPlacement::Cell {
                    workspace,
                    grid,
                    row,
                    column,
                } = placement
                {
                    self.insert_cell(workspace, grid, row, column, LayoutItem::Tool(tool))?;
                }
            }
            DockIntent::Float { frame, rect } => {
                if self
                    .model
                    .floats
                    .get(&frame)
                    .is_some_and(|f| f.rect == rect)
                {
                    return Ok(Change::Unchanged);
                }
                let workspace = self
                    .model
                    .frames
                    .get(&frame)
                    .ok_or(Error::Missing(EntityId::Frame(frame)))?
                    .workspace;
                if !self
                    .model
                    .contexts
                    .get(&workspace)
                    .is_some_and(|c| c.allow_floating)
                {
                    return Err(Error::ForbiddenDestination);
                }
                self.ensure_plan(workspace)?;
                self.model
                    .layouts
                    .get_mut(&workspace)
                    .unwrap()
                    .remove(LayoutItem::Frame(frame))?;
                let return_location = self
                    .model
                    .floats
                    .get(&frame)
                    .map(|f| f.return_location)
                    .unwrap_or(DockLocation::Append { workspace });
                self.model.floats.insert(
                    frame,
                    FloatingFrame {
                        kind: SurfaceKind::InAppFloat,
                        rect,
                        return_location,
                    },
                );
                let mut locations = vec![return_location];
                locations.extend(
                    self.model.contexts[&workspace]
                        .allowed_docks
                        .iter()
                        .copied(),
                );
                let mut usable = None;
                for location in locations {
                    if self.location_workspace(location).ok() != Some(workspace) {
                        continue;
                    }
                    self.model.floats.get_mut(&frame).unwrap().return_location = location;
                    let mut probe = self.model_candidate();
                    if probe
                        .mutate(Command::Dock(DockIntent::Redock { frame }))
                        .is_ok()
                    {
                        usable = Some(location);
                        break;
                    }
                }
                self.model.floats.get_mut(&frame).unwrap().return_location =
                    usable.ok_or(Error::ForbiddenDestination)?;
            }
            DockIntent::Redock { frame } => {
                let location = self
                    .model
                    .floats
                    .get(&frame)
                    .ok_or(Error::ForbiddenDestination)?
                    .return_location;
                self.dock_frame(frame, location)?;
            }
        }
        if self.model == before {
            return Ok(Change::Unchanged);
        }
        self.model.generation = before
            .generation
            .checked_add(1)
            .ok_or(Error::Exhausted(Counter::Generation))?;
        Ok(
            match self
                .model
                .tools
                .keys()
                .find(|id| before.tools.get(id) != self.model.tools.get(id))
            {
                Some(tool) => Change::ToolChanged(*tool),
                _ => Change::LayoutChanged(
                    self.model
                        .frames
                        .values()
                        .find(|f| before.frames.get(&f.id) != Some(f))
                        .map(|f| f.workspace)
                        .unwrap_or_else(|| *self.model.workspaces.keys().next().unwrap()),
                ),
            },
        )
    }
    pub(crate) fn ensure_plan(&mut self, workspace: WorkspaceId) -> Result<(), Error> {
        if !self.model.layouts.contains_key(&workspace) {
            let plan = self.model.effective_plan(workspace)?;
            self.model.layouts.insert(workspace, plan);
        }
        Ok(())
    }
    pub(crate) fn location_workspace(&self, location: DockLocation) -> Result<WorkspaceId, Error> {
        Ok(match location {
            DockLocation::Append { workspace }
            | DockLocation::WorkspaceEdge { workspace, .. }
            | DockLocation::Cell { workspace, .. } => workspace,
            DockLocation::Beside { frame, .. } => {
                self.model
                    .frames
                    .get(&frame)
                    .ok_or(Error::Missing(EntityId::Frame(frame)))?
                    .workspace
            }
        })
    }
    fn dock_frame(&mut self, frame: FrameId, location: DockLocation) -> Result<(), Error> {
        let source = self
            .model
            .frames
            .get(&frame)
            .ok_or(Error::Missing(EntityId::Frame(frame)))?
            .workspace;
        let destination = self.location_workspace(location)?;
        if !self
            .model
            .contexts
            .get(&destination)
            .is_some_and(|c| c.allowed_docks.contains(&location))
        {
            return Err(Error::ForbiddenDestination);
        }
        if matches!(location, DockLocation::Beside { frame: target, .. } if target == frame) {
            return Err(Error::ForbiddenDestination);
        }
        self.ensure_plan(source)?;
        self.ensure_plan(destination)?;
        self.model
            .layouts
            .get_mut(&source)
            .unwrap()
            .remove(LayoutItem::Frame(frame))?;
        let index = self
            .model
            .workspaces
            .get(&destination)
            .ok_or(Error::Missing(EntityId::Workspace(destination)))?
            .frames
            .len()
            - usize::from(source == destination);
        self.mutate_ownership(Command::MoveFrame {
            frame,
            destination,
            index,
        })?;
        self.model.floats.remove(&frame);
        match location {
            DockLocation::Append { .. } => self
                .model
                .layouts
                .get_mut(&destination)
                .unwrap()
                .append(LayoutItem::Frame(frame))?,
            DockLocation::Cell {
                grid, row, column, ..
            } => self.insert_cell(destination, grid, row, column, LayoutItem::Frame(frame))?,
            DockLocation::WorkspaceEdge { edge, .. } => {
                let plan = self.model.layouts.get_mut(&destination).unwrap();
                let item = plan.next_node()?;
                plan.nodes
                    .insert(item, LayoutNode::Leaf(LayoutItem::Frame(frame)));
                match plan.root {
                    None => plan.root = Some(item),
                    Some(root) => {
                        let parent = plan.next_node()?;
                        plan.nodes.insert(parent, split(edge, root, item)?);
                        plan.root = Some(parent);
                    }
                }
            }
            DockLocation::Beside {
                frame: target,
                edge,
            } => {
                let plan = self.model.layouts.get_mut(&destination).unwrap();
                let node = plan
                    .nodes
                    .iter()
                    .find_map(|(id, n)| {
                        if *n == LayoutNode::Leaf(LayoutItem::Frame(target)) {
                            Some(*id)
                        } else {
                            None
                        }
                    })
                    .ok_or(Error::ForbiddenDestination)?;
                let retained = plan.next_node()?;
                plan.nodes
                    .insert(retained, LayoutNode::Leaf(LayoutItem::Frame(target)));
                let item = plan.next_node()?;
                plan.nodes
                    .insert(item, LayoutNode::Leaf(LayoutItem::Frame(frame)));
                plan.nodes.insert(node, split(edge, retained, item)?);
            }
        }
        Ok(())
    }
    pub(crate) fn insert_cell(
        &mut self,
        workspace: WorkspaceId,
        grid: NodeId,
        row: usize,
        column: usize,
        item: LayoutItem,
    ) -> Result<(), Error> {
        let plan = self
            .model
            .layouts
            .get_mut(&workspace)
            .ok_or(GeometryError::InvalidPlan("configured grid absent"))?;
        let Some(LayoutNode::Grid {
            rows,
            columns,
            cells,
        }) = plan.nodes.get(&grid)
        else {
            return Err(GeometryError::InvalidPlan("grid absent").into());
        };
        if row >= rows.len()
            || column >= columns.len()
            || cells.iter().any(|c| c.row == row && c.column == column)
        {
            return Err(GeometryError::InvalidPlan("invalid or occupied cell").into());
        }
        let node = plan.next_node()?;
        plan.nodes.insert(node, LayoutNode::Leaf(item));
        let LayoutNode::Grid { cells, .. } = plan.nodes.get_mut(&grid).unwrap() else {
            unreachable!()
        };
        cells.push(Cell { row, column, node });
        Ok(())
    }
}
fn split(edge: Edge, retained: NodeId, inserted: NodeId) -> Result<LayoutNode, Error> {
    let ids = if matches!(edge, Edge::Left | Edge::Top) {
        [inserted, retained]
    } else {
        [retained, inserted]
    };
    Ok(LayoutNode::Split {
        axis: if matches!(edge, Edge::Left | Edge::Right) {
            Axis::Horizontal
        } else {
            Axis::Vertical
        },
        children: ids
            .into_iter()
            .map(|node| {
                Ok(WeightedNode {
                    node,
                    weight: Scalar::new(1.0)?,
                })
            })
            .collect::<Result<_, Error>>()?,
    })
}
impl Snapshot {
    pub fn snap_candidates(
        &self,
        workspace: WorkspaceId,
        x: Scalar,
        y: Scalar,
        radius: Scalar,
    ) -> Result<Vec<SnapCandidate>, Error> {
        crate::geometry::nonnegative(radius.get())?;
        let context = self
            .contexts
            .get(&workspace)
            .ok_or(GeometryError::MissingContext)?;
        let geometry = self.geometry(workspace)?;
        let mut result = vec![];
        for location in &context.allowed_docks {
            let shape = match *location {
                DockLocation::WorkspaceEdge {
                    workspace: id,
                    edge,
                } if id == workspace => Some(edge_rect(geometry.content, edge)?),
                DockLocation::Beside { frame, edge } => geometry
                    .frames
                    .get(&frame)
                    .map(|r| edge_rect(*r, edge))
                    .transpose()?,
                DockLocation::Cell {
                    workspace: id,
                    grid,
                    row,
                    column,
                } if id == workspace => geometry.cells.get(&(grid, row, column)).copied(),
                _ => None,
            };
            if let Some(r) = shape {
                let dx = (r.x() - x.get()).max(0.0).max(x.get() - r.right());
                let dy = (r.y() - y.get()).max(0.0).max(y.get() - r.bottom());
                let distance = crate::geometry::finite(dx.hypot(dy))?;
                if distance <= radius.get() {
                    result.push(SnapCandidate {
                        location: *location,
                        distance: Scalar::new(distance)?,
                    });
                }
            }
        }
        result.sort_by(|a, b| {
            a.distance
                .get()
                .total_cmp(&b.distance.get())
                .then(a.location.cmp(&b.location))
        });
        result.dedup_by_key(|c| c.location);
        Ok(result)
    }
}
fn edge_rect(rect: Rect, edge: Edge) -> Result<Rect, Error> {
    match edge {
        Edge::Top => Rect::new(rect.x(), rect.y(), rect.width(), 0.0),
        Edge::Bottom => Rect::new(rect.x(), rect.bottom(), rect.width(), 0.0),
        Edge::Left => Rect::new(rect.x(), rect.y(), 0.0, rect.height()),
        Edge::Right => Rect::new(rect.right(), rect.y(), 0.0, rect.height()),
    }
}
