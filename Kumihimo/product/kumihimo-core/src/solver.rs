use crate::geometry::{finite, tracks};
use crate::*;
use std::collections::BTreeMap;

fn edge_tools(model: &Snapshot, anchor: ToolAnchor) -> Vec<&Tool> {
    let mut tools: Vec<_> = model
        .tools
        .values()
        .filter(|t| {
            t.presentation().reserves_space
                && matches!(t.placement, ToolPlacement::Edge { anchor: a, .. } if a == anchor)
        })
        .collect();
    tools.sort_by_key(|t| {
        let ToolPlacement::Edge { edge, .. } = t.placement else {
            unreachable!()
        };
        (edge, t.policy.order, t.id)
    });
    tools
}
fn requirement(content: Size, tools: &[&Tool]) -> Result<Size, Error> {
    let (mut horizontal, mut vertical, mut cross_width, mut cross_height) =
        (0.0, 0.0, 0.0_f64, 0.0_f64);
    for t in tools {
        let ToolPlacement::Edge { edge, .. } = t.placement else {
            unreachable!()
        };
        match edge {
            Edge::Top | Edge::Bottom => {
                horizontal = finite(horizontal + t.policy.preferred.height())?;
                cross_width = cross_width.max(t.policy.minimum.width());
            }
            _ => {
                vertical = finite(vertical + t.policy.preferred.width())?;
                cross_height = cross_height.max(t.policy.minimum.height());
            }
        }
    }
    Size::new(
        finite(content.width() + vertical)?.max(cross_width),
        finite(content.height().max(cross_height) + horizontal)?,
    )
}
fn reserve(
    mut content: Rect,
    tools: &[&Tool],
    output: &mut BTreeMap<ToolId, Rect>,
) -> Result<Rect, Error> {
    for t in tools {
        let ToolPlacement::Edge { edge, .. } = t.placement else {
            unreachable!()
        };
        let rect;
        match edge {
            Edge::Top | Edge::Bottom => {
                let height = t.policy.preferred.height();
                if height > content.height() || content.width() < t.policy.minimum.width() {
                    return Err(GeometryError::Infeasible.into());
                }
                if edge == Edge::Top {
                    rect = Rect::new(content.x(), content.y(), content.width(), height)?;
                    content = Rect::new(
                        content.x(),
                        finite(content.y() + height)?,
                        content.width(),
                        content.height() - height,
                    )?;
                } else {
                    rect = Rect::new(
                        content.x(),
                        finite(content.bottom() - height)?,
                        content.width(),
                        height,
                    )?;
                    content = Rect::new(
                        content.x(),
                        content.y(),
                        content.width(),
                        content.height() - height,
                    )?;
                }
            }
            _ => {
                let width = t.policy.preferred.width();
                if width > content.width() || content.height() < t.policy.minimum.height() {
                    return Err(GeometryError::Infeasible.into());
                }
                if edge == Edge::Left {
                    rect = Rect::new(content.x(), content.y(), width, content.height())?;
                    content = Rect::new(
                        finite(content.x() + width)?,
                        content.y(),
                        content.width() - width,
                        content.height(),
                    )?;
                } else {
                    rect = Rect::new(
                        finite(content.right() - width)?,
                        content.y(),
                        width,
                        content.height(),
                    )?;
                    content = Rect::new(
                        content.x(),
                        content.y(),
                        content.width() - width,
                        content.height(),
                    )?;
                }
            }
        }
        output.insert(t.id, rect);
    }
    Ok(content)
}
fn frame_tools(model: &Snapshot, frame: FrameId) -> Vec<&Tool> {
    let mut tools = edge_tools(model, ToolAnchor::Frame(frame));
    for view in &model.frames[&frame].views {
        tools.extend(edge_tools(model, ToolAnchor::View(*view)));
    }
    tools.sort_by_key(|t| {
        let ToolPlacement::Edge { edge, .. } = t.placement else {
            unreachable!()
        };
        (edge, t.policy.order, t.id)
    });
    tools
}

/// Pure solver metadata for one workspace; child workspace minima are propagated iteratively.
struct Measurement {
    plan: LayoutPlan,
    nodes: BTreeMap<NodeId, Size>,
    required: Size,
    frames: BTreeMap<FrameId, Size>,
}
fn measure(model: &Snapshot, workspace: WorkspaceId) -> Result<Measurement, Error> {
    let mut pending = vec![workspace];
    let mut order = vec![];
    while let Some(id) = pending.pop() {
        let ws = model
            .workspaces
            .get(&id)
            .ok_or(Error::Missing(EntityId::Workspace(id)))?;
        order.push(id);
        for frame in &ws.frames {
            if let Some(child) = model.frames[frame].child_workspace {
                pending.push(child);
            }
        }
    }
    let mut measured = BTreeMap::<WorkspaceId, Measurement>::new();
    for id in order.into_iter().rev() {
        let plan = model.effective_plan(id)?;
        model.validate_plan(id, &plan)?;
        let gap = model.contexts.get(&id).map_or(0.0, |c| c.gap.get());
        let mut frame_min = BTreeMap::new();
        for frame in &model.workspaces[&id].frames {
            let mut minimum = model
                .contexts
                .get(&id)
                .and_then(|c| c.minimums.get(frame))
                .copied()
                .unwrap_or(Size::zero());
            if let Some(child) = model.frames[frame].child_workspace {
                minimum = minimum.max(measured[&child].required)?;
            }
            minimum = requirement(minimum, &frame_tools(model, *frame))?;
            frame_min.insert(*frame, minimum);
        }
        let mut sizes = BTreeMap::<NodeId, Size>::new();
        for node in plan.order()?.into_iter().rev() {
            let size = match &plan.nodes[&node] {
                LayoutNode::Leaf(LayoutItem::Frame(frame)) => frame_min[frame],
                LayoutNode::Leaf(LayoutItem::Tool(tool)) => {
                    let t = &model.tools[tool];
                    if t.presentation().reserves_space {
                        t.policy.minimum
                    } else {
                        Size::zero()
                    }
                }
                LayoutNode::Split { axis, children } => {
                    let mut along = finite(gap * children.len().saturating_sub(1) as f64)?;
                    let mut across = 0.0_f64;
                    for child in children {
                        let s = sizes[&child.node];
                        let (a, b) = match axis {
                            Axis::Horizontal => (s.width(), s.height()),
                            Axis::Vertical => (s.height(), s.width()),
                        };
                        along = finite(along + a)?;
                        across = across.max(b);
                    }
                    match axis {
                        Axis::Horizontal => Size::new(along, across)?,
                        Axis::Vertical => Size::new(across, along)?,
                    }
                }
                LayoutNode::Grid {
                    rows,
                    columns,
                    cells,
                } => {
                    let mut widths = vec![0.0_f64; columns.len()];
                    let mut heights = vec![0.0_f64; rows.len()];
                    for cell in cells {
                        let s = sizes[&cell.node];
                        widths[cell.column] = widths[cell.column].max(s.width());
                        heights[cell.row] = heights[cell.row].max(s.height());
                    }
                    let width = widths.into_iter().try_fold(
                        finite(gap * columns.len().saturating_sub(1) as f64)?,
                        |sum, value| finite(sum + value),
                    )?;
                    let height = heights.into_iter().try_fold(
                        finite(gap * rows.len().saturating_sub(1) as f64)?,
                        |sum, value| finite(sum + value),
                    )?;
                    Size::new(width, height)?
                }
            };
            sizes.insert(node, size);
        }
        let root = plan.root.map(|n| sizes[&n]).unwrap_or(Size::zero());
        let required = requirement(root, &edge_tools(model, ToolAnchor::Workspace(id)))?;
        measured.insert(
            id,
            Measurement {
                plan,
                nodes: sizes,
                required,
                frames: frame_min,
            },
        );
    }
    Ok(measured.remove(&workspace).unwrap())
}

impl Snapshot {
    pub fn geometry(&self, workspace: WorkspaceId) -> Result<GeometryOutput, Error> {
        let context = self
            .contexts
            .get(&workspace)
            .ok_or(GeometryError::MissingContext)?;
        let m = measure(self, workspace)?;
        let viewport = context.viewport;
        if context.overflow == Overflow::Reject
            && (viewport.width() < m.required.width() || viewport.height() < m.required.height())
        {
            return Err(GeometryError::Infeasible.into());
        }
        let virtual_extent = viewport.size().max(m.required)?;
        let outer = Rect::new(
            viewport.x(),
            viewport.y(),
            virtual_extent.width(),
            virtual_extent.height(),
        )?;
        let mut output = GeometryOutput {
            generation: self.generation,
            workspace,
            required_extent: m.required,
            virtual_extent,
            viewport,
            content: outer,
            frames: BTreeMap::new(),
            tools: BTreeMap::new(),
            clips: BTreeMap::new(),
            frame_content: BTreeMap::new(),
            tool_states: self
                .tools
                .iter()
                .map(|(id, t)| (*id, t.presentation()))
                .collect(),
            nodes: BTreeMap::new(),
            cells: BTreeMap::new(),
        };
        output.content = reserve(
            outer,
            &edge_tools(self, ToolAnchor::Workspace(workspace)),
            &mut output.tools,
        )?;
        let mut pending = m
            .plan
            .root
            .map(|id| vec![(id, output.content)])
            .unwrap_or_default();
        while let Some((id, rect)) = pending.pop() {
            output.nodes.insert(id, rect);
            match &m.plan.nodes[&id] {
                LayoutNode::Leaf(LayoutItem::Frame(frame)) => {
                    output.frames.insert(*frame, rect);
                    output.clips.insert(*frame, rect.clip(viewport)?);
                    let content = reserve(rect, &frame_tools(self, *frame), &mut output.tools)?;
                    output.frame_content.insert(*frame, content);
                }
                LayoutNode::Leaf(LayoutItem::Tool(tool)) => {
                    if self.tools[tool].presentation().reserves_space {
                        output.tools.insert(*tool, rect);
                    }
                }
                LayoutNode::Split { axis, children } => {
                    let mins: Vec<_> = children
                        .iter()
                        .map(|c| match axis {
                            Axis::Horizontal => m.nodes[&c.node].width(),
                            Axis::Vertical => m.nodes[&c.node].height(),
                        })
                        .collect();
                    let weights: Vec<_> = children.iter().map(|c| c.weight).collect();
                    let extent = match axis {
                        Axis::Horizontal => rect.width(),
                        Axis::Vertical => rect.height(),
                    };
                    for (child, (offset, length)) in
                        children
                            .iter()
                            .zip(tracks(&mins, &weights, extent, context.gap.get())?)
                    {
                        let r = match axis {
                            Axis::Horizontal => Rect::new(
                                finite(rect.x() + offset)?,
                                rect.y(),
                                length,
                                rect.height(),
                            )?,
                            Axis::Vertical => Rect::new(
                                rect.x(),
                                finite(rect.y() + offset)?,
                                rect.width(),
                                length,
                            )?,
                        };
                        pending.push((child.node, r));
                    }
                }
                LayoutNode::Grid {
                    rows,
                    columns,
                    cells,
                } => {
                    let mut widths = vec![0.0_f64; columns.len()];
                    let mut heights = vec![0.0_f64; rows.len()];
                    for cell in cells {
                        widths[cell.column] = widths[cell.column].max(m.nodes[&cell.node].width());
                        heights[cell.row] = heights[cell.row].max(m.nodes[&cell.node].height());
                    }
                    let x = tracks(&widths, columns, rect.width(), context.gap.get())?;
                    let y = tracks(&heights, rows, rect.height(), context.gap.get())?;
                    // Materialize only cells that are present or eligible, not the Cartesian product of holes.
                    for cell in cells {
                        let (dx, w) = x[cell.column];
                        let (dy, h) = y[cell.row];
                        let r = Rect::new(finite(rect.x() + dx)?, finite(rect.y() + dy)?, w, h)?;
                        output.cells.insert((id, cell.row, cell.column), r);
                        pending.push((cell.node, r));
                    }
                    for location in &context.allowed_docks {
                        if let DockLocation::Cell {
                            workspace: target,
                            grid,
                            row,
                            column,
                        } = location
                        {
                            if *target == workspace
                                && *grid == id
                                && *row < y.len()
                                && *column < x.len()
                            {
                                let (dx, w) = x[*column];
                                let (dy, h) = y[*row];
                                output.cells.insert(
                                    (id, *row, *column),
                                    Rect::new(
                                        finite(rect.x() + dx)?,
                                        finite(rect.y() + dy)?,
                                        w,
                                        h,
                                    )?,
                                );
                            }
                        }
                    }
                }
            }
        }
        for (frame, float) in &self.floats {
            if self.frames[frame].workspace != workspace {
                continue;
            }
            let minimum = m.frames[frame];
            if !viewport.contains(float.rect)
                || float.rect.width() < minimum.width()
                || float.rect.height() < minimum.height()
            {
                return Err(GeometryError::OutsideViewport.into());
            }
            output.frames.insert(*frame, float.rect);
            output.clips.insert(*frame, float.rect.clip(viewport)?);
            let content = reserve(float.rect, &frame_tools(self, *frame), &mut output.tools)?;
            output.frame_content.insert(*frame, content);
        }
        Ok(output)
    }
}
