//! Iterative canonical geometry projection; no browser placement state is imported.
use crate::WebError;
use kumihimo_content::Controller;
use kumihimo_core as core;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const HEADER_HEIGHT: f64 = 36.0;
const MAX_PROJECTION_BYTES: usize = 4 * 1024 * 1024;

pub fn rect_value(rect: core::Rect) -> Value {
    json!({"x":rect.x(),"y":rect.y(),"width":rect.width(),"height":rect.height()})
}
fn intersection(first: core::Rect, second: core::Rect) -> Result<core::Rect, WebError> {
    let x = first.x().max(second.x());
    let y = first.y().max(second.y());
    core::Rect::new(
        x,
        y,
        (first.right().min(second.right()) - x).max(0.0),
        (first.bottom().min(second.bottom()) - y).max(0.0),
    )
    .map_err(|_| WebError::new("geometry_overflow"))
}
fn usable(rect: core::Rect) -> Result<core::Rect, WebError> {
    let reserved = HEADER_HEIGHT.min(rect.height());
    core::Rect::new(
        rect.x(),
        rect.y() + reserved,
        rect.width(),
        (rect.height() - reserved).max(0.0),
    )
    .map_err(|_| WebError::new("geometry_overflow"))
}
fn expanded(snapshot: &core::Snapshot, frame: &core::Frame) -> bool {
    frame
        .selected_view
        .and_then(|id| snapshot.views.get(&id))
        .is_some_and(|v| v.closed.is_none() && v.state.visible)
}

#[derive(Clone, Copy)]
struct FrameGeometry {
    rect: core::Rect,
    content: core::Rect,
    clip: core::Rect,
    visible: bool,
    depth: usize,
}

/// Read-only rendering result. All ownership records remain present when hidden.
pub fn project(
    controller: &Controller,
    root: core::WorkspaceId,
    scale: f64,
    epoch: u64,
) -> Result<Value, WebError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(WebError::new("invalid_scale"));
    }
    let snapshot = controller.snapshot();
    if !snapshot.workspaces.contains_key(&root) {
        return Err(WebError::new("missing_root"));
    }
    let root_rect = snapshot
        .contexts
        .get(&root)
        .ok_or_else(|| WebError::new("missing_root_context"))?
        .viewport;
    let mut queue = VecDeque::from([(root, root_rect, true, 0usize)]);
    let mut visited = BTreeSet::new();
    let mut frames: BTreeMap<core::FrameId, FrameGeometry> = BTreeMap::new();
    let mut workspace_geometry = BTreeMap::new();
    let mut nodes = Vec::new();
    let mut handles = Vec::new();
    while let Some((workspace, ancestor_clip, ancestor_visible, depth)) = queue.pop_front() {
        if !visited.insert(workspace) {
            return Err(WebError::new("workspace_cycle"));
        }
        let record = snapshot
            .workspaces
            .get(&workspace)
            .ok_or_else(|| WebError::new("missing_workspace"))?;
        let geometry = snapshot
            .geometry(workspace)
            .map_err(|error| WebError::from(error).scoped(workspace.raw().to_string()))?;
        let workspace_clip = intersection(geometry.viewport, ancestor_clip)?;
        let visible =
            ancestor_visible && workspace_clip.width() > 0.0 && workspace_clip.height() > 0.0;
        workspace_geometry.insert(workspace, json!({"rect":rect_value(geometry.viewport),"content":rect_value(geometry.content),"clip":rect_value(workspace_clip),"visible":visible,"depth":depth,"required_extent":{"width":geometry.required_extent.width(),"height":geometry.required_extent.height()},"virtual_extent":{"width":geometry.virtual_extent.width(),"height":geometry.virtual_extent.height()}}));
        for id in &record.frames {
            let frame = snapshot
                .frames
                .get(id)
                .ok_or_else(|| WebError::new("missing_frame"))?;
            // Maximized-out leaves have no solved geometry. Retain their IDs and child chain.
            let frame_rect = geometry.frames.get(id).copied();
            let rect = frame_rect.unwrap_or(
                core::Rect::new(geometry.viewport.x(), geometry.viewport.y(), 0.0, 0.0)
                    .map_err(|_| WebError::new("geometry_overflow"))?,
            );
            let content = usable(geometry.frame_content.get(id).copied().unwrap_or(rect))?;
            let clip = intersection(
                geometry.clips.get(id).copied().unwrap_or(rect),
                workspace_clip,
            )?;
            let frame_visible =
                visible && frame_rect.is_some() && clip.width() > 0.0 && clip.height() > 0.0;
            frames.insert(
                *id,
                FrameGeometry {
                    rect,
                    content,
                    clip,
                    visible: frame_visible,
                    depth,
                },
            );
            if let Some(child) = frame.child_workspace {
                let child_clip = intersection(content, clip)?;
                queue.push_back((
                    child,
                    child_clip,
                    frame_visible && expanded(snapshot, frame),
                    depth
                        .checked_add(1)
                        .ok_or_else(|| WebError::new("depth_overflow"))?,
                ));
            }
        }
        if let Some(plan) = snapshot.layouts.get(&workspace) {
            for (id, node) in &plan.nodes {
                let rect = geometry.nodes.get(id).copied();
                let mut value = json!({"workspace":workspace.raw().to_string(),"id":id.0.to_string(),"rect":rect.map(rect_value),"visible":visible && rect.is_some(),"children":[],"weights":[],"rows":[],"columns":[]});
                match node {
                    core::LayoutNode::Leaf(item) => {
                        value["kind"] = json!("leaf");
                        match item {
                            core::LayoutItem::Frame(f) => {
                                value["frame"] = json!(f.raw().to_string())
                            }
                            core::LayoutItem::Tool(t) => value["tool"] = json!(t.raw().to_string()),
                        }
                    }
                    core::LayoutNode::Split { axis, children } => {
                        value["kind"] = json!("split");
                        value["axis"] = json!(format!("{axis:?}"));
                        value["children"] = json!(children
                            .iter()
                            .map(|c| c.node.0.to_string())
                            .collect::<Vec<_>>());
                        value["weights"] =
                            json!(children.iter().map(|c| c.weight.get()).collect::<Vec<_>>());
                        for pair in children.windows(2) {
                            if let (Some(a), Some(b)) = (
                                geometry.nodes.get(&pair[0].node),
                                geometry.nodes.get(&pair[1].node),
                            ) {
                                let handle = match axis {
                                    core::Axis::Horizontal => core::Rect::new(
                                        a.right(),
                                        a.y(),
                                        (b.x() - a.right()).max(0.0),
                                        a.height(),
                                    ),
                                    core::Axis::Vertical => core::Rect::new(
                                        a.x(),
                                        a.bottom(),
                                        a.width(),
                                        (b.y() - a.bottom()).max(0.0),
                                    ),
                                }
                                .map_err(|_| WebError::new("geometry_overflow"))?;
                                handles.push(json!({"workspace":workspace.raw().to_string(),"node":id.0.to_string(),"first":pair[0].node.0.to_string(),"second":pair[1].node.0.to_string(),"axis":format!("{axis:?}"),"rect":rect_value(handle),"clip":rect_value(intersection(handle, workspace_clip)?),"visible":visible}));
                            }
                        }
                    }
                    core::LayoutNode::Grid {
                        rows,
                        columns,
                        cells,
                    } => {
                        value["kind"] = json!("grid");
                        value["rows"] = json!(rows.iter().map(|v| v.get()).collect::<Vec<_>>());
                        value["columns"] =
                            json!(columns.iter().map(|v| v.get()).collect::<Vec<_>>());
                        value["children"] = json!(cells
                            .iter()
                            .map(|c| c.node.0.to_string())
                            .collect::<Vec<_>>());
                        value["cells"] = json!(cells.iter().map(|c| json!({"row":c.row,"column":c.column,"node":c.node.0.to_string(),"rect":geometry.cells.get(&(*id,c.row,c.column)).copied().map(rect_value)})).collect::<Vec<_>>());
                    }
                }
                nodes.push(value);
            }
        }
    }
    let frame_records = snapshot.frames.values().map(|f| {
        let geometry = frames.get(&f.id);
        json!({"id":f.id.raw().to_string(),"workspace":f.workspace.raw().to_string(),"child_workspace":f.child_workspace.map(|v|v.raw().to_string()),"views":f.views.iter().map(|v|v.raw().to_string()).collect::<Vec<_>>(),"selected_view":f.selected_view.map(|v|v.raw().to_string()),"groups":f.groups.iter().map(|v|v.raw().to_string()).collect::<Vec<_>>(),"rect":geometry.map(|g|rect_value(g.rect)),"content":geometry.map(|g|rect_value(g.content)),"clip":geometry.map(|g|rect_value(g.clip)),"visible":geometry.is_some_and(|g|g.visible),"expanded":expanded(snapshot,f),"depth":geometry.map(|g|g.depth),"floating":snapshot.floats.contains_key(&f.id)})
    }).collect::<Vec<_>>();
    let workspaces = snapshot.workspaces.values().map(|w| {
        let mut value = json!({"id":w.id.raw().to_string(),"owner":w.owner.map(|v|v.raw().to_string()),"frames":w.frames.iter().map(|v|v.raw().to_string()).collect::<Vec<_>>(),"visible":false,"rect":null,"content":null,"clip":null,"depth":null});
        if let Some(Value::Object(geometry)) = workspace_geometry.get(&w.id) { for (key, field) in geometry { value[key.as_str()] = field.clone(); } }
        value
    }).collect::<Vec<_>>();
    let views = snapshot.views.values().map(|v| {
        let resource = snapshot.resources.get(&v.resource);
        let provider = resource.and_then(|r|snapshot.providers.get(&r.provider));
        let selected = snapshot.frames.get(&v.frame).is_some_and(|f|f.selected_view == Some(v.id));
        let visible = frames.get(&v.frame).is_some_and(|f|f.visible) && selected && v.state.visible && v.closed.is_none();
        json!({"id":v.id.raw().to_string(),"frame":v.frame.raw().to_string(),"resource":v.resource.raw().to_string(),"provider":resource.map(|r|r.provider.raw().to_string()),"group":v.group.map(|g|g.raw().to_string()),"label":v.label,"pinned":v.pinned,"unsaved":v.unsaved,"closed":v.closed.is_some(),"selected":selected,"visible":visible,"usable":v.state.usable,"placeholder":provider.is_none_or(|p|p.availability != core::ProviderAvailability::Available)})
    }).collect::<Vec<_>>();
    let ledger = controller.core().ledger_status();
    let result = json!({
        "version":crate::wire::VERSION,"snapshot_version":snapshot.version,"identity":snapshot.identity,
        "generation":snapshot.generation.to_string(),"high_watermark":ledger.high_watermark.sequence().to_string(),
        "next_operation":ledger.high_watermark.checked_next().map(|o|o.sequence().to_string()),
        "root":root.raw().to_string(),"epoch":epoch.to_string(),"scale":scale,"header_height":HEADER_HEIGHT,
        "workspaces":workspaces,"frames":frame_records,"views":views,"nodes":nodes,"split_handles":handles,
        "groups":snapshot.groups.values().map(|g|json!({"id":g.id.raw().to_string(),"frame":g.frame.raw().to_string(),"label":g.label,"color":g.color,"description":g.description,"collapsed":g.collapsed})).collect::<Vec<_>>(),
        "providers":snapshot.providers.values().map(|p|json!({"id":p.id.raw().to_string(),"key":p.key,"available":p.availability == core::ProviderAvailability::Available,"availability":format!("{:?}",p.availability),"supports_shared_views":p.supports_shared_views})).collect::<Vec<_>>(),
        "resources":snapshot.resources.values().map(|r|json!({"id":r.id.raw().to_string(),"provider":r.provider.raw().to_string(),"key":r.key,"restore_reference":r.restore_reference})).collect::<Vec<_>>(),
        "layouts":snapshot.layouts.iter().map(|(w,p)|json!({"workspace":w.raw().to_string(),"root":p.root.map(|n|n.0.to_string()),"plan":p})).collect::<Vec<_>>(),
        "limits":snapshot.limits
    });
    if serde_json::to_vec(&result)
        .map_err(|_| WebError::new("projection_encoding"))?
        .len()
        > MAX_PROJECTION_BYTES
    {
        return Err(WebError::new("projection_size_limit"));
    }
    Ok(result)
}

fn context_for(
    snapshot: &core::Snapshot,
    workspace: core::WorkspaceId,
    viewport: core::Rect,
) -> Result<core::GeometryContext, WebError> {
    let w = snapshot
        .workspaces
        .get(&workspace)
        .ok_or_else(|| WebError::new("missing_workspace"))?;
    let mut context = snapshot
        .contexts
        .get(&workspace)
        .cloned()
        .unwrap_or_else(|| {
            let mut c = core::GeometryContext::new(viewport);
            c.gap = core::Scalar::new(8.0).expect("finite constant");
            c.overflow = core::Overflow::Scrollable;
            c.allow_floating = true;
            c
        });
    context.viewport = viewport;
    context.minimums.retain(|id, _| w.frames.contains(id));
    context.frame_weights.retain(|id, _| w.frames.contains(id));
    for f in &w.frames {
        context
            .minimums
            .entry(*f)
            .or_insert(core::Size::new(96.0, 100.0).map_err(|_| WebError::new("invalid_minimum"))?);
    }
    // Eligibility is rebuilt from canonical targets. A saved stale list cannot refer to moved frames.
    context.allowed_docks = vec![core::DockLocation::Append { workspace }];
    for edge in [
        core::Edge::Left,
        core::Edge::Right,
        core::Edge::Top,
        core::Edge::Bottom,
    ] {
        context
            .allowed_docks
            .push(core::DockLocation::WorkspaceEdge { workspace, edge });
        for frame in &w.frames {
            context.allowed_docks.push(core::DockLocation::Beside {
                frame: *frame,
                edge,
            });
        }
    }
    if let Some(plan) = snapshot.layouts.get(&workspace) {
        for (grid, node) in &plan.nodes {
            if let core::LayoutNode::Grid {
                rows,
                columns,
                cells,
            } = node
            {
                let slots = rows
                    .len()
                    .checked_mul(columns.len())
                    .ok_or_else(|| WebError::new("layout_budget"))?;
                if snapshot
                    .limits
                    .max_layout_entries
                    .is_some_and(|limit| slots > limit)
                {
                    return Err(WebError::new("layout_budget"));
                }
                for row in 0..rows.len() {
                    for column in 0..columns.len() {
                        if !cells.iter().any(|c| c.row == row && c.column == column) {
                            context.allowed_docks.push(core::DockLocation::Cell {
                                workspace,
                                grid: *grid,
                                row,
                                column,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(context)
}
fn set_context(
    controller: &mut Controller,
    workspace: core::WorkspaceId,
    context: core::GeometryContext,
) -> Result<(), WebError> {
    if controller.snapshot().contexts.get(&workspace) == Some(&context) {
        return Ok(());
    }
    let snapshot = controller.snapshot();
    let envelope = core::Envelope {
        identity: snapshot.identity.clone(),
        actor: "host-geometry".into(),
        operation: controller
            .core()
            .ledger_status()
            .high_watermark
            .checked_next()
            .ok_or_else(|| WebError::new("operation_exhausted"))?,
        expected_generation: snapshot.generation,
        command: core::Command::SetGeometryContext { workspace, context },
    };
    let response = controller
        .execute(envelope)
        .map_err(|error| WebError::from(error).scoped(workspace.raw().to_string()))?;
    response
        .outcome
        .result
        .map(|_| ())
        .map_err(|error| WebError::from(error).scoped(workspace.raw().to_string()))
}

/// Host dispatch helper: changed-only contexts, one shared operation allocator.
pub fn synchronize_contexts(
    controller: &mut Controller,
    root: core::WorkspaceId,
    viewport: core::Rect,
) -> Result<(), WebError> {
    if !controller.snapshot().workspaces.contains_key(&root) {
        return Err(WebError::new("missing_root"));
    }
    // Install policies first so bottom-up core measurement sees all descendant minima.
    let workspaces = controller
        .snapshot()
        .workspaces
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for workspace in workspaces {
        let rect = if workspace == root {
            viewport
        } else {
            controller
                .snapshot()
                .contexts
                .get(&workspace)
                .map(|c| c.viewport)
                .unwrap_or(viewport)
        };
        let context = context_for(controller.snapshot(), workspace, rect)?;
        set_context(controller, workspace, context)?;
    }
    let mut queue = VecDeque::from([(root, viewport)]);
    let mut seen = BTreeSet::new();
    while let Some((workspace, rect)) = queue.pop_front() {
        if !seen.insert(workspace) {
            return Err(WebError::new("workspace_cycle"));
        }
        let context = context_for(controller.snapshot(), workspace, rect)?;
        set_context(controller, workspace, context)?;
        let snapshot = controller.snapshot();
        let geometry = snapshot
            .geometry(workspace)
            .map_err(|error| WebError::from(error).scoped(workspace.raw().to_string()))?;
        for frame in &snapshot.workspaces[&workspace].frames {
            if let Some(child) = snapshot.frames[frame].child_workspace {
                if let Some(content) = geometry.frame_content.get(frame) {
                    queue.push_back((child, usable(*content)?));
                }
            }
        }
    }
    Ok(())
}
