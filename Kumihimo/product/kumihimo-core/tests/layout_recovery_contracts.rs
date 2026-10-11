use kumihimo_core::*;
use std::collections::BTreeMap;

fn scalar(n: f64) -> Scalar {
    Scalar::new(n).unwrap()
}
fn size(w: f64, h: f64) -> Size {
    Size::new(w, h).unwrap()
}
fn rect(w: f64, h: f64) -> Rect {
    Rect::new(0.0, 0.0, w, h).unwrap()
}
fn envelope(core: &Core, command: Command) -> Envelope {
    Envelope {
        identity: core.snapshot().identity.clone(),
        actor: "layout-recovery-consumer".into(),
        operation: core.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: core.snapshot().generation,
        command,
    }
}
fn apply(core: &mut Core, command: Command) -> Change {
    let envelope = envelope(core, command);
    core.execute(envelope).outcome.result.unwrap()
}
fn reject(core: &mut Core, command: Command) -> Error {
    let before = core.snapshot().clone();
    let envelope = envelope(core, command);
    let error = core.execute(envelope).outcome.result.unwrap_err();
    assert_eq!(core.snapshot(), &before);
    error
}
fn workspace(core: &mut Core, owner: Option<FrameId>) -> WorkspaceId {
    let Change::Created(EntityId::Workspace(id)) = apply(core, Command::CreateWorkspace { owner })
    else {
        panic!()
    };
    id
}
fn frame(core: &mut Core, workspace: WorkspaceId) -> FrameId {
    let index = core.snapshot().workspaces[&workspace].frames.len();
    let Change::Created(EntityId::Frame(id)) =
        apply(core, Command::CreateFrame { workspace, index })
    else {
        panic!()
    };
    id
}
fn fixture() -> (Core, WorkspaceId, FrameId, FrameId) {
    let mut core = Core::new(
        Identity::new("layout-lineage", "layout-instance").unwrap(),
        Limits::default(),
        32,
    )
    .unwrap();
    let w = workspace(&mut core, None);
    let a = frame(&mut core, w);
    let b = frame(&mut core, w);
    (core, w, a, b)
}
fn context(core: &mut Core, w: WorkspaceId, context: GeometryContext) {
    apply(
        core,
        Command::SetGeometryContext {
            workspace: w,
            context,
        },
    );
}
fn split(a: FrameId, b: FrameId) -> LayoutPlan {
    LayoutPlan {
        root: Some(NodeId(3)),
        nodes: BTreeMap::from([
            (NodeId(1), LayoutNode::Leaf(LayoutItem::Frame(a))),
            (NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(b))),
            (
                NodeId(3),
                LayoutNode::Split {
                    axis: Axis::Horizontal,
                    children: vec![
                        WeightedNode {
                            node: NodeId(1),
                            weight: scalar(1.0),
                        },
                        WeightedNode {
                            node: NodeId(2),
                            weight: scalar(3.0),
                        },
                    ],
                },
            ),
        ]),
    }
}
fn view(core: &mut Core, frame: FrameId) -> (ProviderId, ViewId) {
    let Change::Created(EntityId::Provider(provider)) = apply(
        core,
        Command::RegisterProvider {
            key: format!("provider-{}", frame.raw()),
            supports_shared_views: true,
        },
    ) else {
        panic!()
    };
    let Change::Created(EntityId::Resource(resource)) = apply(
        core,
        Command::RegisterResource {
            provider,
            key: format!("resource-{}", frame.raw()),
            restore_reference: "host-ref".into(),
        },
    ) else {
        panic!()
    };
    let Change::Created(EntityId::View(view)) = apply(
        core,
        Command::CreateView {
            frame,
            resource,
            index: 0,
        },
    ) else {
        panic!()
    };
    (provider, view)
}

#[test]
fn equalize_changes_spare_shares_and_all_weight_sources_without_changing_ownership() {
    let (mut core, w, a, b) = fixture();
    let mut c = GeometryContext::new(rect(200.0, 100.0));
    c.minimums = BTreeMap::from([(a, size(20.0, 10.0)), (b, size(40.0, 10.0))]);
    c.frame_weights = BTreeMap::from([(a, scalar(1.0)), (b, scalar(3.0))]);
    context(&mut core, w, c);
    assert_eq!(
        core.snapshot().geometry(w).unwrap().frames[&a].width(),
        55.0
    );
    let frames = core.snapshot().frames.clone();
    apply(&mut core, Command::Equalize { workspace: w });
    let g = core.snapshot().geometry(w).unwrap();
    assert_eq!((g.frames[&a].width(), g.frames[&b].width()), (90.0, 110.0));
    assert_eq!(core.snapshot().frames, frames);
    assert!(core.snapshot().layouts.is_empty());
    apply(
        &mut core,
        Command::SetLayout {
            workspace: w,
            plan: split(a, b),
        },
    );
    apply(&mut core, Command::Equalize { workspace: w });
    let LayoutNode::Split { children, .. } = &core.snapshot().layouts[&w].nodes[&NodeId(3)] else {
        panic!()
    };
    assert!(children.iter().all(|c| c.weight == scalar(1.0)));
    let grid = LayoutPlan {
        root: Some(NodeId(3)),
        nodes: BTreeMap::from([
            (NodeId(1), LayoutNode::Leaf(LayoutItem::Frame(a))),
            (NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(b))),
            (
                NodeId(3),
                LayoutNode::Grid {
                    rows: vec![scalar(2.0), scalar(3.0)],
                    columns: vec![scalar(4.0), scalar(5.0)],
                    cells: vec![
                        Cell {
                            row: 0,
                            column: 0,
                            node: NodeId(1),
                        },
                        Cell {
                            row: 1,
                            column: 1,
                            node: NodeId(2),
                        },
                    ],
                },
            ),
        ]),
    };
    apply(
        &mut core,
        Command::SetLayout {
            workspace: w,
            plan: grid,
        },
    );
    apply(&mut core, Command::Equalize { workspace: w });
    let LayoutNode::Grid { rows, columns, .. } = &core.snapshot().layouts[&w].nodes[&NodeId(3)]
    else {
        panic!()
    };
    assert!(rows.iter().chain(columns).all(|w| *w == scalar(1.0)));
    let generation = core.snapshot().generation;
    assert_eq!(
        apply(&mut core, Command::Equalize { workspace: w }),
        Change::Unchanged
    );
    assert_eq!(core.snapshot().generation, generation);
}

#[test]
fn swap_is_spatial_only_and_rejects_cross_workspace_or_float_targets() {
    let (mut core, w, a, b) = fixture();
    let mut c = GeometryContext::new(rect(200.0, 100.0));
    c.allow_floating = true;
    c.allowed_docks.push(DockLocation::Append { workspace: w });
    context(&mut core, w, c);
    let (_, v) = view(&mut core, a);
    let before = core.snapshot().clone();
    apply(
        &mut core,
        Command::SwapFrames {
            first: a,
            second: b,
        },
    );
    let g = core.snapshot().geometry(w).unwrap();
    assert_eq!(g.frames[&b].x(), 0.0);
    assert_eq!(g.frames[&a].x(), 100.0);
    assert_eq!(core.snapshot().workspaces, before.workspaces);
    assert_eq!(core.snapshot().frames, before.frames);
    assert_eq!(core.snapshot().views[&v], before.views[&v]);
    let other = workspace(&mut core, None);
    let outside = frame(&mut core, other);
    assert_eq!(
        reject(
            &mut core,
            Command::SwapFrames {
                first: a,
                second: outside
            }
        ),
        Error::ForbiddenDestination
    );
    apply(
        &mut core,
        Command::Dock(DockIntent::Float {
            frame: b,
            rect: Rect::new(5.0, 5.0, 40.0, 40.0).unwrap(),
        }),
    );
    assert_eq!(
        reject(
            &mut core,
            Command::SwapFrames {
                first: a,
                second: b
            }
        ),
        Error::ForbiddenDestination
    );
    assert_eq!(
        reject(&mut core, Command::Maximize { frame: b }),
        Error::ForbiddenDestination
    );
}

#[test]
fn maximize_hides_sibling_descendant_minima_and_failed_exit_preserves_intervening_tabs() {
    let (mut core, w, a, b) = fixture();
    let child = workspace(&mut core, Some(b));
    let leaf = frame(&mut core, child);
    let mut nested = GeometryContext::new(rect(500.0, 100.0));
    nested.minimums.insert(leaf, size(500.0, 10.0));
    context(&mut core, child, nested.clone());
    context(&mut core, w, GeometryContext::new(rect(600.0, 100.0)));
    apply(
        &mut core,
        Command::SetLayout {
            workspace: w,
            plan: split(a, b),
        },
    );
    let retained = core.snapshot().layouts[&w].clone();
    apply(&mut core, Command::Maximize { frame: a });
    nested.viewport = rect(1.0, 1.0);
    // The hidden child is retained and validated structurally, not solved as an active surface.
    context(&mut core, child, nested);
    context(&mut core, w, GeometryContext::new(rect(100.0, 100.0)));
    let (_, v) = view(&mut core, a);
    apply(
        &mut core,
        Command::SetViewMetadata {
            view: v,
            label: "intervening-label".into(),
            pinned: false,
            unsaved: true,
        },
    );
    let g = core.snapshot().geometry(w).unwrap();
    assert_eq!(g.frames, BTreeMap::from([(a, rect(100.0, 100.0))]));
    assert_eq!(core.snapshot().layouts[&w], retained);
    assert_eq!(
        reject(&mut core, Command::RestoreLayout { workspace: w }),
        Error::Geometry(GeometryError::Infeasible)
    );
    assert_eq!(core.snapshot().maximized[&w], a);
    assert_eq!(core.snapshot().views[&v].label, "intervening-label");
    let mut nested = core.snapshot().contexts[&child].clone();
    nested.overflow = Overflow::Scrollable;
    context(&mut core, child, nested);
    let mut root = core.snapshot().contexts[&w].clone();
    root.overflow = Overflow::Scrollable;
    context(&mut core, w, root);
    apply(&mut core, Command::RestoreLayout { workspace: w });
    assert!(!core.snapshot().maximized.contains_key(&w));
    assert_eq!(core.snapshot().geometry(w).unwrap().frames.len(), 2);
    assert_eq!(core.snapshot().views[&v].label, "intervening-label");
    assert_eq!(core.snapshot().layouts[&w], retained);
}

#[test]
fn maximize_guard_covers_both_ownership_scopes_preview_and_child_attachment_with_noops() {
    let (mut core, w, a, b) = fixture();
    let mut c = GeometryContext::new(rect(200.0, 100.0));
    c.allowed_docks.push(DockLocation::Append { workspace: w });
    c.allow_floating = true;
    context(&mut core, w, c);
    apply(
        &mut core,
        Command::SetLayout {
            workspace: w,
            plan: split(a, b),
        },
    );
    let preview = core
        .preview(DockIntent::Frame {
            frame: b,
            location: DockLocation::Append { workspace: w },
        })
        .unwrap();
    apply(&mut core, Command::Maximize { frame: a });
    let other = workspace(&mut core, None);
    let outside = frame(&mut core, other);
    for command in [
        Command::CreateFrame {
            workspace: w,
            index: 2,
        },
        Command::CreateWorkspace { owner: Some(a) },
        Command::MoveFrame {
            frame: b,
            destination: other,
            index: 1,
        },
        Command::MoveFrame {
            frame: outside,
            destination: w,
            index: 2,
        },
        Command::Dock(DockIntent::Float {
            frame: b,
            rect: rect(20.0, 20.0),
        }),
        Command::Dock(DockIntent::Frame {
            frame: outside,
            location: DockLocation::Append { workspace: w },
        }),
        Command::Equalize { workspace: w },
        Command::SwapFrames {
            first: a,
            second: b,
        },
    ] {
        assert_eq!(reject(&mut core, command), Error::Maximized(w));
    }
    assert!(matches!(
        reject(&mut core, Command::CommitPreview(preview)),
        Error::StalePreview { .. }
    ));
    assert!(
        matches!(core.preview(DockIntent::Redock { frame: b }), Err(Error::Maximized(id)) if id == w)
    );
    let before = core.snapshot().clone();
    assert_eq!(
        apply(&mut core, Command::Maximize { frame: a }),
        Change::Unchanged
    );
    assert_eq!(
        apply(
            &mut core,
            Command::MoveFrame {
                frame: a,
                destination: w,
                index: 0
            }
        ),
        Change::Unchanged
    );
    assert_eq!(
        apply(
            &mut core,
            Command::SwapFrames {
                first: a,
                second: a
            }
        ),
        Change::Unchanged
    );
    assert_eq!(
        apply(
            &mut core,
            Command::SetLayout {
                workspace: w,
                plan: before.layouts[&w].clone()
            }
        ),
        Change::Unchanged
    );
    assert_eq!(core.snapshot(), &before);
    frame(&mut core, other);
}

#[test]
fn maximized_float_is_hidden_but_scrollable_exit_still_rejects_outside_bounds() {
    let (mut core, w, a, b) = fixture();
    let mut c = GeometryContext::new(rect(200.0, 100.0));
    c.allowed_docks.push(DockLocation::Append { workspace: w });
    c.allow_floating = true;
    context(&mut core, w, c);
    apply(
        &mut core,
        Command::Dock(DockIntent::Float {
            frame: b,
            rect: Rect::new(150.0, 10.0, 40.0, 40.0).unwrap(),
        }),
    );
    apply(&mut core, Command::Maximize { frame: a });
    let mut c = core.snapshot().contexts[&w].clone();
    c.viewport = rect(100.0, 100.0);
    c.overflow = Overflow::Scrollable;
    context(&mut core, w, c);
    assert_eq!(core.snapshot().geometry(w).unwrap().frames.len(), 1);
    assert!(core.snapshot().floats.contains_key(&b));
    assert_eq!(
        reject(&mut core, Command::RestoreLayout { workspace: w }),
        Error::Geometry(GeometryError::OutsideViewport)
    );
    assert_eq!(core.snapshot().maximized[&w], a);
}

#[test]
fn foreign_incarnation_preview_rejects_before_any_receipt_effect() {
    let (mut first, w, a, _) = fixture();
    let mut c = GeometryContext::new(rect(200.0, 100.0));
    c.allowed_docks.push(DockLocation::Append { workspace: w });
    context(&mut first, w, c.clone());
    let preview = first
        .preview(DockIntent::Frame {
            frame: a,
            location: DockLocation::Append { workspace: w },
        })
        .unwrap();
    let mut second = Core::new(
        Identity::new("layout-lineage", "fresh-instance").unwrap(),
        Limits::default(),
        32,
    )
    .unwrap();
    let new_w = workspace(&mut second, None);
    frame(&mut second, new_w);
    frame(&mut second, new_w);
    context(&mut second, new_w, c);
    let ledger = second.ledger_status();
    assert_eq!(
        reject(&mut second, Command::CommitPreview(preview)),
        Error::IdentityMismatch
    );
    assert_eq!(second.ledger_status(), ledger);
}

#[test]
fn projected_tools_hide_sibling_and_cell_constraints_and_guard_all_movement_routes() {
    let (mut core, w, a, b) = fixture();
    let (provider, v) = view(&mut core, a);
    let grid = LayoutPlan {
        root: Some(NodeId(3)),
        nodes: BTreeMap::from([
            (NodeId(1), LayoutNode::Leaf(LayoutItem::Frame(a))),
            (NodeId(2), LayoutNode::Leaf(LayoutItem::Frame(b))),
            (
                NodeId(3),
                LayoutNode::Grid {
                    rows: vec![scalar(1.0)],
                    columns: vec![scalar(1.0); 3],
                    cells: vec![
                        Cell {
                            row: 0,
                            column: 0,
                            node: NodeId(1),
                        },
                        Cell {
                            row: 0,
                            column: 1,
                            node: NodeId(2),
                        },
                    ],
                },
            ),
        ]),
    };
    apply(
        &mut core,
        Command::SetLayout {
            workspace: w,
            plan: grid,
        },
    );
    context(&mut core, w, GeometryContext::new(rect(1000.0, 1000.0)));
    let root = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(w),
        edge: Edge::Top,
    };
    let selected = ToolPlacement::Edge {
        anchor: ToolAnchor::Frame(a),
        edge: Edge::Top,
    };
    let sibling = ToolPlacement::Edge {
        anchor: ToolAnchor::Frame(b),
        edge: Edge::Top,
    };
    let cell = ToolPlacement::Cell {
        workspace: w,
        grid: NodeId(3),
        row: 0,
        column: 2,
    };
    let target = EditingTarget {
        view: v,
        resource: core.snapshot().views[&v].resource,
    };
    let mut specs = vec![];
    for (placement, minimum) in [
        (root, size(1.0, 10.0)),
        (selected, size(1.0, 20.0)),
        (sibling, size(500.0, 500.0)),
        (cell, size(500.0, 500.0)),
    ] {
        let mut policy = ToolPolicy::new(minimum, minimum, vec![root, selected, sibling, cell]);
        policy.visibility = VisibilityPolicy::Always;
        specs.push(ToolSpec {
            owner: provider,
            origin: v,
            target,
            placement,
            policy,
            state: TargetState::default(),
        });
    }
    let mut ids = vec![];
    for spec in &specs {
        let Change::Created(EntityId::Tool(id)) =
            apply(&mut core, Command::CreateTool(spec.clone()))
        else {
            panic!()
        };
        ids.push(id);
    }
    apply(&mut core, Command::Maximize { frame: a });
    context(&mut core, w, GeometryContext::new(rect(100.0, 100.0)));
    let g = core.snapshot().geometry(w).unwrap();
    assert_eq!(g.tools.len(), 2);
    assert_eq!(g.content.y(), 10.0);
    assert_eq!(g.frame_content[&a].y(), 30.0);
    assert!(!g.tool_states[&ids[2]].visible);
    assert!(!g.tool_states[&ids[3]].reserves_space);
    assert_eq!(core.snapshot().tools.len(), 4);
    assert_eq!(
        reject(&mut core, Command::CreateTool(specs[0].clone())),
        Error::Maximized(w)
    );
    assert_eq!(
        reject(
            &mut core,
            Command::Dock(DockIntent::Tool {
                tool: ids[0],
                placement: sibling
            })
        ),
        Error::Maximized(w)
    );
    assert_eq!(
        reject(
            &mut core,
            Command::Dock(DockIntent::Tool {
                tool: ids[3],
                placement: root
            })
        ),
        Error::Maximized(w)
    );
    let before = core.snapshot().clone();
    assert_eq!(
        apply(
            &mut core,
            Command::Dock(DockIntent::Tool {
                tool: ids[3],
                placement: cell
            })
        ),
        Change::Unchanged
    );
    assert_eq!(core.snapshot(), &before);
    let other = workspace(&mut core, None);
    context(&mut core, other, GeometryContext::new(rect(100.0, 100.0)));
    let outward = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(other),
        edge: Edge::Top,
    };
    let mut policy = core.snapshot().tools[&ids[0]].policy.clone();
    policy.allowed.push(outward);
    apply(
        &mut core,
        Command::ConfigureTool {
            tool: ids[0],
            policy,
        },
    );
    assert_eq!(
        reject(
            &mut core,
            Command::Dock(DockIntent::Tool {
                tool: ids[0],
                placement: outward
            })
        ),
        Error::Maximized(w)
    );
    let mut spec = specs[0].clone();
    spec.placement = outward;
    spec.policy.allowed = vec![outward, root];
    let Change::Created(EntityId::Tool(outside)) = apply(&mut core, Command::CreateTool(spec))
    else {
        panic!()
    };
    assert_eq!(
        reject(
            &mut core,
            Command::Dock(DockIntent::Tool {
                tool: outside,
                placement: root
            })
        ),
        Error::Maximized(w)
    );
}
