use kumihimo_core::*;

fn run(core: &mut Core, command: Command) -> Result<Change, Error> {
    let envelope = Envelope {
        identity: core.snapshot().identity.clone(),
        actor: "workspace-example".into(),
        operation: core
            .ledger_status()
            .high_watermark
            .checked_next()
            .expect("example sequence fits"),
        expected_generation: core.snapshot().generation,
        command,
    };
    core.execute(envelope).outcome.result
}
fn main() -> Result<(), Error> {
    let mut core = Core::new(
        Identity::new("workspace-example", "one-shot-demo")?,
        Limits {
            max_records: Some(100),
            ..Limits::default()
        },
        16,
    )?;
    let Change::Created(EntityId::Workspace(root)) =
        run(&mut core, Command::CreateWorkspace { owner: None })?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Frame(frame)) = run(
        &mut core,
        Command::CreateFrame {
            workspace: root,
            index: 0,
        },
    )?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Workspace(child)) =
        run(&mut core, Command::CreateWorkspace { owner: Some(frame) })?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Frame(nested)) = run(
        &mut core,
        Command::CreateFrame {
            workspace: child,
            index: 0,
        },
    )?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Provider(provider)) = run(
        &mut core,
        Command::RegisterProvider {
            key: "example-provider".into(),
            supports_shared_views: true,
        },
    )?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Resource(resource)) = run(
        &mut core,
        Command::RegisterResource {
            provider,
            key: "example-resource".into(),
            restore_reference: "host-owned-ref".into(),
        },
    )?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::View(view)) = run(
        &mut core,
        Command::CreateView {
            frame: nested,
            resource,
            index: 0,
        },
    )?
    else {
        unreachable!()
    };
    run(
        &mut core,
        Command::MoveFrame {
            frame: nested,
            destination: root,
            index: 1,
        },
    )?;
    let placement = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(root),
        edge: Edge::Top,
    };
    let mut context = GeometryContext::new(Rect::new(0.0, 0.0, 640.0, 360.0)?);
    context.gap = Scalar::new(8.0)?;
    context.minimums.insert(frame, Size::new(100.0, 80.0)?);
    context.minimums.insert(nested, Size::new(200.0, 80.0)?);
    context.frame_weights.insert(nested, Scalar::new(2.0)?);
    context.allowed_docks = vec![
        DockLocation::Append { workspace: root },
        DockLocation::Beside {
            frame,
            edge: Edge::Right,
        },
    ];
    run(
        &mut core,
        Command::SetGeometryContext {
            workspace: root,
            context,
        },
    )?;
    let Change::Created(EntityId::Tool(tool)) = run(
        &mut core,
        Command::CreateTool(ToolSpec {
            owner: provider,
            origin: view,
            target: EditingTarget { view, resource },
            placement,
            policy: ToolPolicy::new(
                Size::new(120.0, 24.0)?,
                Size::new(80.0, 16.0)?,
                vec![placement],
            ),
            state: TargetState {
                target_visible: true,
                target_usable: true,
                applicable: true,
                ..TargetState::default()
            },
        }),
    )?
    else {
        unreachable!()
    };
    let preview = core.preview(DockIntent::Frame {
        frame: nested,
        location: DockLocation::Beside {
            frame,
            edge: Edge::Right,
        },
    })?;
    run(&mut core, Command::CommitPreview(preview))?;
    let geometry = core.snapshot().geometry(root)?;
    println!(
        "generation={} root_frames={:?} view={:?} resource={:?} tool={:?} content={:?} panes={}",
        core.snapshot().generation,
        core.snapshot().workspaces[&root].frames,
        view,
        core.snapshot().views[&view].resource,
        tool,
        geometry.content,
        geometry.frames.len()
    );
    Ok(())
}
