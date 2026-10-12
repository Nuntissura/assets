use kumihimo_content::*;
use kumihimo_core::{self as core, Command, Core, EntityId, Envelope};

fn create(c: &mut Core, command: Command) -> EntityId {
    let r = c.execute(Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "example-host".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    });
    match r.outcome.result.unwrap() {
        core::Change::Created(id) => id,
        _ => panic!("creation"),
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Production hosts guarantee fresh Core/controller tokens across earlier lifetimes.
    let mut core = Core::new(
        core::Identity::new("example-layout", "example-core-1")?,
        Default::default(),
        16,
    )?;
    let EntityId::Workspace(workspace) =
        create(&mut core, Command::CreateWorkspace { owner: None })
    else {
        unreachable!()
    };
    let EntityId::Frame(frame) = create(
        &mut core,
        Command::CreateFrame {
            workspace,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Provider(provider) = create(
        &mut core,
        Command::RegisterProvider {
            key: "recording-provider".into(),
            supports_shared_views: false,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(resource) = create(
        &mut core,
        Command::RegisterResource {
            provider,
            key: "example-document".into(),
            restore_reference: "opaque-host-reference".into(),
        },
    ) else {
        unreachable!()
    };
    let EntityId::View(view) = create(
        &mut core,
        Command::CreateView {
            frame,
            resource,
            index: 0,
        },
    ) else {
        unreachable!()
    };
    let mut controller = Controller::new(core, "example-host-session-1", Default::default())?;
    controller.negotiate(
        provider,
        ProtocolVersion::V2,
        Default::default(),
        Default::default(),
    )?;
    let permission = HostPermission {
        allowed: true,
        eligibility_revision: 0,
    };
    let mount = controller.attach(view, Default::default(), permission, 0)?;
    let attach = controller
        .dispatch(mount, permission, 0)?
        .expect("attach request");
    controller.ack(
        Ack {
            ticket: attach.ticket,
            result: AckResult::Confirmed(Confirmation::Attached {
                strength: RestorationStrength::LayoutOnly,
            }),
        },
        0,
    )?;
    controller.request(mount, RequestKind::Invalidate, permission, 0)?;
    let redraw = controller
        .dispatch(mount, permission, 0)?
        .expect("redraw request");
    controller.ack(
        Ack {
            ticket: redraw.ticket,
            result: AckResult::Confirmed(Confirmation::Done),
        },
        0,
    )?;
    let counts = controller.inspect().counts;
    println!("Recorded attach/redraw acknowledgments: {} reserved instance, {} mount, {} terminal receipts; host engines and physical usage unmeasured",counts.instances,counts.mounts,counts.receipts);
    Ok(())
}
