use kumihimo_core::*;

fn run(core: &mut Core, command: Command) -> Result<Change, Error> {
    let envelope = Envelope {
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
        Limits {
            max_records: Some(100),
            ..Limits::default()
        },
        16,
    );
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
            supports_shared_views: true,
        },
    )?
    else {
        unreachable!()
    };
    let Change::Created(EntityId::Resource(resource)) =
        run(&mut core, Command::RegisterResource { provider })?
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
    println!(
        "generation={} root_frames={:?} view={:?} resource={:?}",
        core.snapshot().generation,
        core.snapshot().workspaces[&root].frames,
        view,
        core.snapshot().views[&view].resource
    );
    Ok(())
}
