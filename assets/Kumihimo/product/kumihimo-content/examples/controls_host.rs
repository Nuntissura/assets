use kumihimo_content::*;
use kumihimo_core::{self as core, Command, Core, EntityId, Envelope, Scalar};
use std::collections::BTreeSet;

fn create(c: &mut Core, command: Command) -> EntityId {
    let response = c.execute(Envelope {
        identity: c.snapshot().identity.clone(),
        actor: "controls-host".into(),
        operation: c.ledger_status().high_watermark.checked_next().unwrap(),
        expected_generation: c.snapshot().generation,
        command,
    });
    match response.outcome.result.unwrap() {
        core::Change::Created(id) => id,
        _ => panic!("creation"),
    }
}
fn attached(c: &mut Controller, mount: MountId, p: HostPermission) -> Result<()> {
    let request = c.dispatch(mount, p, 0)?.expect("attach");
    c.ack(
        Ack {
            ticket: request.ticket,
            result: AckResult::Confirmed(Confirmation::Attached {
                strength: RestorationStrength::LayoutOnly,
            }),
        },
        0,
    )?;
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new(
        core::Identity::new("controls-layout", "controls-core")?,
        Default::default(),
        32,
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
            key: "controls-provider".into(),
            supports_shared_views: true,
        },
    ) else {
        unreachable!()
    };
    let EntityId::Resource(resource) = create(
        &mut core,
        Command::RegisterResource {
            provider,
            key: "host-resource".into(),
            restore_reference: "host-restore".into(),
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
    let mut controller = Controller::new(core, "controls-session", Default::default())?;
    let mut caps = Capabilities::default();
    caps.backgrounds = BackgroundCapabilities {
        image: true,
        video: true,
        compositing: true,
        fits: BTreeSet::from([BackgroundFit::Cover]),
        poster: true,
        audio: false,
        host_contrast: false,
        media_budget: true,
    };
    controller.negotiate(provider, ProtocolVersion::V2, caps.clone(), caps)?;
    let p = HostPermission {
        allowed: true,
        eligibility_revision: 0,
    };
    let mount = controller.attach(view, Default::default(), p, 0)?;
    attached(&mut controller, mount, p)?;
    let scope = controller.inspect().mounts[&mount].scope.clone();
    controller.update_navigation(
        &scope,
        DocumentModel {
            revision: 1,
            semantic: "host-model".into(),
            unit: "document-units".into(),
            extent: Scalar::new(1000.0)?,
            viewport: Scalar::new(100.0)?,
        },
        NavigationObservation {
            model_revision: 1,
            revision: 1,
            offset: Scalar::new(0.0)?,
        },
    )?;
    controller.update_navigation_rail(
        &scope,
        1,
        RailGeometry {
            origin: Scalar::new(0.0)?,
            length: Scalar::new(200.0)?,
            map_origin: Scalar::new(0.0)?,
            map_length: Scalar::new(200.0)?,
            document_rect: core::Rect::new(0.0, 0.0, 400.0, 300.0)?,
        },
    )?;
    controller.navigation_key(&scope, NavigationKey::End, p, 0)?;
    let request = controller.dispatch(mount, p, 0)?.expect("navigation");
    controller.ack(
        Ack {
            ticket: request.ticket,
            result: AckResult::Confirmed(Confirmation::Navigation(NavigationObservation {
                model_revision: 1,
                revision: 2,
                offset: Scalar::new(900.0)?,
            })),
        },
        0,
    )?;
    let background = controller.attach_background(
        MountTarget::FrameBackground(frame),
        resource,
        BackgroundKind::Video,
        Default::default(),
        p,
        0,
    )?;
    attached(&mut controller, background, p)?;
    let scope = controller.inspect().mounts[&background].scope.clone();
    controller.background_settings(scope.clone(), Default::default(), Default::default(), 0)?;
    controller.media_policy(scope, Default::default(), p, 0)?;
    let request = controller
        .dispatch(background, p, 0)?
        .expect("media budget");
    let RequestKind::MediaBudget(budget) = request.kind else {
        unreachable!()
    };
    controller.ack(
        Ack {
            ticket: request.ticket,
            result: AckResult::Confirmed(Confirmation::MediaBudget(MediaObservation {
                aggregate_revision: budget.aggregate_revision,
                observation_revision: 1,
                paused: false,
                fps: 24,
                width: 1280,
                height: 720,
            })),
        },
        0,
    )?;
    println!(
        "Protocol 2: navigation and media observations confirmed; host rendering remains external."
    );
    Ok(())
}
