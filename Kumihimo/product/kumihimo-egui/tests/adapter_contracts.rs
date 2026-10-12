//! Focused consumer proof. Native OS/UIA, physical devices and raster readback are
//! separately observed by capture_host/native_probe; semantic tests do not replace them.
use kumihimo_core::{self as core, Command, EntityId};
use kumihimo_egui::*;
use persistence::*;

fn surface(root: core::WorkspaceId) -> SurfaceBinding {
    SurfaceBinding {
        root,
        viewport: egui::ViewportId::ROOT,
        epoch: 1,
        bounds: core::Rect::new(0.0, 0.0, 960.0, 640.0).unwrap(),
        pixels_per_point: 1.0,
    }
}
fn paint(
    demo: &mut demo::Demo,
    adapter: &mut DesktopAdapter,
    binding: SurfaceBinding,
) -> (egui::FullOutput, FrameReport) {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut report = None;
    let mut output = None;
    for _ in 0..3 {
        output = Some(ctx.run(
            egui::RawInput {
                screen_rect: Some(to_egui(binding.bounds)),
                ..Default::default()
            },
            |ctx| {
                report = Some(
                    adapter
                        .show(ctx, &mut demo.controller, binding, &mut demo.host)
                        .unwrap(),
                );
            },
        ));
    }
    (output.unwrap(), report.unwrap())
}
fn command(demo: &mut demo::Demo, cmd: Command) -> core::Change {
    demo.host
        .dispatch(&mut demo.controller, cmd)
        .unwrap()
        .outcome
        .result
        .unwrap()
}

#[test]
fn e001_shared_widgets_render_recursive_mixed_providers_and_clip_children() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let projection = workspace::project(&mut d.controller, binding, &mut d.host).unwrap();
    let owner = projection
        .frames
        .iter()
        .find(|f| f.child.is_some())
        .unwrap();
    let child = projection
        .frames
        .iter()
        .find(|f| Some(f.workspace) == owner.child)
        .unwrap();
    assert_eq!(
        d.controller.snapshot().contexts[&child.workspace].viewport,
        owner.usable
    );
    assert!(owner.usable.contains(child.clip));
    let (output, report) = paint(&mut d, &mut DesktopAdapter::default(), binding);
    assert!(!output.shapes.is_empty());
    assert!(report.slots.iter().any(|s| s.view == d.text_view));
    assert!(report.slots.iter().any(|s| s.view == d.canvas_view));
    assert!(report.painted_views >= 2, "{:?}", report.rejected);
}

#[test]
fn e002_saved_membership_outlives_mounted_tab_page() {
    let mut d = demo::with_counts(2, 12).unwrap();
    d.host.policy.tab_page_size = 2;
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    let members = d.controller.snapshot().frames[&frame].views.clone();
    let chosen = *members.last().unwrap();
    command(&mut d, Command::SelectView { view: chosen });
    let binding = surface(d.root);
    let (_, report) = paint(&mut d, &mut DesktopAdapter::default(), binding);
    assert_eq!(d.controller.snapshot().frames[&frame].views, members);
    assert!(report.slots.iter().any(|s| s.view == chosen));
    assert_eq!(report.canonical_views, 24);
}

#[test]
fn e003_cancelled_and_stale_preview_never_move_ownership() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    workspace::project(&mut d.controller, binding, &mut d.host).unwrap();
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    let preview = d
        .controller
        .core()
        .preview(core::DockIntent::Float {
            frame,
            rect: core::Rect::new(20.0, 20.0, 300.0, 200.0).unwrap(),
        })
        .unwrap();
    preview.clone().cancel();
    assert!(!d.controller.snapshot().floats.contains_key(&frame));
    let v = d.controller.snapshot().views[&d.text_view].clone();
    command(
        &mut d,
        Command::SetViewMetadata {
            view: v.id,
            label: "Updated label".into(),
            pinned: v.pinned,
            unsaved: v.unsaved,
        },
    );
    let result = d
        .host
        .dispatch(&mut d.controller, Command::CommitPreview(preview))
        .unwrap();
    assert!(matches!(
        result.outcome.result,
        Err(core::Error::StalePreview { .. })
    ));
    assert_eq!(d.controller.snapshot().frames[&frame].workspace, d.root);
    assert!(!d.controller.snapshot().floats.contains_key(&frame));
}

#[test]
fn e004_tool_action_ack_stays_with_pinned_provider_resource() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    paint(&mut d, &mut DesktopAdapter::default(), binding);
    let target = d.controller.snapshot().tools[&d.tool].target;
    let tool = d.tool;
    d.host
        .tool_action(&mut d.controller, tool, "test-tool")
        .unwrap();
    let receipt = d
        .host
        .receipts
        .iter()
        .rev()
        .find(|r| r.kind == "tool")
        .unwrap();
    assert_eq!(
        (receipt.view, receipt.resource),
        (target.view, target.resource)
    );
    assert_ne!(receipt.view, d.canvas_view);
}

#[test]
fn e005_settings_precedence_reserves_exact_effective_header() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let mut state = chrome::ChromeState::default();
    state.workspace_settings.insert(
        d.root,
        chrome::SettingsPatch {
            header_height: Some(40.0),
            ..Default::default()
        },
    );
    state.surface_settings.insert(
        binding.viewport,
        chrome::SettingsPatch {
            header_height: Some(50.0),
            recreation_needed: true,
            ..Default::default()
        },
    );
    let p =
        workspace::project_with_chrome(&mut d.controller, binding, &mut d.host, &state).unwrap();
    for f in p.frames {
        assert_eq!(f.settings_source, "surface");
        assert_eq!(
            f.usable.y() - f.content.y(),
            50.0_f64.min(f.content.height())
        );
        assert!(f.recreation_needed);
    }
    state.surface_settings.insert(
        binding.viewport,
        chrome::SettingsPatch {
            hit_size: Some(f32::NAN),
            ..Default::default()
        },
    );
    assert!(state
        .resolve(&d.host.policy, d.root, binding.viewport)
        .is_err());
}

#[test]
fn e006_global_clip_admission_precedes_single_local_mapping() {
    let slot = RenderSlot {
        view: core::ViewId::from_raw(1),
        resource: core::ResourceId::from_raw(2),
        frame: core::FrameId::from_raw(3),
        workspace: core::WorkspaceId::from_raw(4),
        canonical_bounds: core::Rect::new(100.0, 80.0, 200.0, 180.0).unwrap(),
        bounds: core::Rect::new(100.0, 108.0, 200.0, 152.0).unwrap(),
        clip: core::Rect::new(120.0, 110.0, 150.0, 100.0).unwrap(),
        physical_size: [400, 304],
        scale: 2.0,
        epoch: 5,
        focused: false,
        visible: true,
    };
    assert_eq!(
        slot.local_point(egui::pos2(130.0, 120.0)).unwrap(),
        egui::pos2(30.0, 12.0)
    );
    assert!(slot.local_point(egui::pos2(110.0, 120.0)).is_err());
    assert!(slot.local_point(egui::pos2(f32::NAN, 120.0)).is_err());
}

#[test]
fn e007_pointer_kind_owner_and_epoch_settle_through_actual_adapter_path() {
    use input::{HostPointerEvent, PointerPhase, PointerSource};
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let mut adapter = DesktopAdapter::default();
    let (_, mut report) = paint(&mut d, &mut adapter, binding);
    let drag = report
        .controls
        .iter()
        .find(|c| c.id.ends_with("/drag") && c.id.starts_with("frame/"))
        .expect("discoverable drag control");
    let point = to_egui(drag.bounds).center();
    let p = workspace::project(&mut d.controller, binding, &mut d.host).unwrap();
    let ctx = egui::Context::default();
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 77,
            source: PointerSource::HostPen,
            phase: PointerPhase::Press,
            position: point,
            epoch: 1,
        })
        .unwrap();
    let _ = ctx.run(
        egui::RawInput {
            focused: true,
            time: Some(1.0),
            ..Default::default()
        },
        |ctx| {
            input::process(
                ctx,
                &mut d.controller,
                &p,
                binding,
                &mut d.host,
                &mut adapter.input,
                &mut report,
            )
        },
    );
    assert_eq!(
        adapter.input.active_capture().unwrap().source,
        PointerSource::HostPen
    );
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 78,
            source: PointerSource::Touch,
            phase: PointerPhase::Move,
            position: point + egui::vec2(10.0, 0.0),
            epoch: 1,
        })
        .unwrap();
    let _ = ctx.run(
        egui::RawInput {
            focused: true,
            time: Some(1.1),
            ..Default::default()
        },
        |ctx| {
            input::process(
                ctx,
                &mut d.controller,
                &p,
                binding,
                &mut d.host,
                &mut adapter.input,
                &mut report,
            )
        },
    );
    assert_eq!(adapter.input.active_capture().unwrap().pointer, 77);
    let changed = SurfaceBinding {
        epoch: 2,
        ..binding
    };
    let _ = ctx.run(
        egui::RawInput {
            focused: true,
            time: Some(1.2),
            ..Default::default()
        },
        |ctx| {
            input::process(
                ctx,
                &mut d.controller,
                &p,
                changed,
                &mut d.host,
                &mut adapter.input,
                &mut report,
            )
        },
    );
    assert!(adapter.input.active_capture().is_none());
    assert_eq!(adapter.input.last_settlement, Some("invalidated"));
    let process =
        |state: &mut input::InputState, demo: &mut demo::Demo, report: &mut FrameReport, time| {
            let _ = ctx.run(
                egui::RawInput {
                    focused: true,
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| {
                    input::process(
                        ctx,
                        &mut demo.controller,
                        &p,
                        binding,
                        &mut demo.host,
                        state,
                        report,
                    )
                },
            );
        };
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 90,
            source: PointerSource::Touch,
            phase: PointerPhase::Press,
            position: point,
            epoch: 1,
        })
        .unwrap();
    process(&mut adapter.input, &mut d, &mut report, 2.0);
    assert!(!adapter.input.active_capture().unwrap().activated);
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 90,
            source: PointerSource::Touch,
            phase: PointerPhase::Move,
            position: point + egui::vec2(30.0, 0.0),
            epoch: 1,
        })
        .unwrap();
    process(&mut adapter.input, &mut d, &mut report, 2.1);
    assert!(adapter.input.active_capture().is_none());
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 91,
            source: PointerSource::Touch,
            phase: PointerPhase::Press,
            position: point,
            epoch: 1,
        })
        .unwrap();
    process(&mut adapter.input, &mut d, &mut report, 3.0);
    process(&mut adapter.input, &mut d, &mut report, 3.6);
    assert!(adapter.input.active_capture().unwrap().activated);
    adapter
        .input
        .feed_pointer(HostPointerEvent {
            pointer: 91,
            source: PointerSource::Touch,
            phase: PointerPhase::Cancel,
            position: point,
            epoch: 1,
        })
        .unwrap();
    process(&mut adapter.input, &mut d, &mut report, 3.7);
    assert!(adapter.input.active_capture().is_none());
}

#[test]
fn e008_explicit_keyboard_chord_selects_canonical_tab() {
    let mut d = demo::new().unwrap();
    d.host.policy.settings.frame_shortcuts_first = true;
    let binding = surface(d.root);
    let mut adapter = DesktopAdapter::default();
    let (_, mut report) = paint(&mut d, &mut adapter, binding);
    adapter.input.focused_view = Some(d.text_view);
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    let members = d.controller.snapshot().frames[&frame].views.clone();
    let p = workspace::project(&mut d.controller, binding, &mut d.host).unwrap();
    let ctx = egui::Context::default();
    let event = egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: Some(egui::Key::Tab),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::CTRL,
    };
    let _ = ctx.run(
        egui::RawInput {
            focused: true,
            modifiers: egui::Modifiers::CTRL,
            events: vec![event],
            ..Default::default()
        },
        |ctx| {
            input::process(
                ctx,
                &mut d.controller,
                &p,
                binding,
                &mut d.host,
                &mut adapter.input,
                &mut report,
            )
        },
    );
    assert_eq!(
        d.controller.snapshot().frames[&frame].selected_view,
        Some(members[1])
    );
}

#[test]
fn e009_semantic_accesskit_output_is_distinct_from_required_os_observer() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let (output, report) = paint(&mut d, &mut DesktopAdapter::default(), binding);
    assert!(output
        .platform_output
        .accesskit_update
        .as_ref()
        .is_some_and(|t| !t.nodes.is_empty()));
    assert!(!report.controls.is_empty());
    assert!(!Capabilities::default().native_observed);
}

#[test]
fn e010_projection_scaling_and_maximized_hidden_descendants() {
    let mut d = demo::new().unwrap();
    let mut binding = surface(d.root);
    binding.pixels_per_point = 1.5;
    assert_eq!(binding.physical_size().unwrap(), [1440, 960]);
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    command(&mut d, Command::Maximize { frame });
    let p = workspace::project(&mut d.controller, binding, &mut d.host).unwrap();
    assert!(p.frames.iter().all(|f| f.frame == frame));
    assert_eq!(d.controller.snapshot().views.len(), 9);
    binding.pixels_per_point = f32::INFINITY;
    assert!(binding.validate().is_err());
}

#[cfg(feature = "capture")]
#[test]
fn e011_provider_handoff_rejects_old_epoch_and_session_incarnation() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let (_, r) = paint(&mut d, &mut DesktopAdapter::default(), binding);
    let slot = r.slots[0].clone();
    // Binding construction is renderer-owned; validate actual binding in capture_host.
    assert_eq!(slot.epoch, binding.epoch);
    let changed = SurfaceBinding {
        epoch: 2,
        ..binding
    };
    paint(&mut d, &mut DesktopAdapter::default(), changed);
    assert!(d
        .host
        .content_input(
            &mut d.controller,
            &slot,
            kumihimo_content::Input {
                owner: kumihimo_content::InputOwner::Content,
                payload: "stale-provider-input".into(),
                x: None,
                y: None
            }
        )
        .is_err());
    let session = DesktopSession::capture(d.controller.snapshot(), 8, vec![]).unwrap();
    let fresh = core::Identity::new(session.source.lineage.clone(), "fresh-incarnation").unwrap();
    assert!(session
        .validate_after_restore(
            &d.controller.core().save_layout(),
            d.controller.snapshot(),
            &fresh
        )
        .is_err());
}

#[cfg(feature = "native")]
#[test]
fn e012_preparation_cancel_reuses_empty_slots_without_native_ready() {
    let mut d = demo::new().unwrap();
    d.host.policy.max_native_slots = 1;
    let source = d.controller.snapshot().views[&d.text_view].frame;
    let view = d.text_view;
    let mut slots = window_slots::WindowSlots::default();
    let first = slots
        .prepare(&mut d.controller, &mut d.host, NativeContent::View(view))
        .unwrap();
    slots.cancel(&first, d.controller.snapshot()).unwrap();
    assert_eq!(d.controller.snapshot().views[&view].frame, source);
    let second = slots
        .prepare(&mut d.controller, &mut d.host, NativeContent::View(view))
        .unwrap();
    assert_eq!(first.slot.root, second.slot.root);
    assert_eq!(slots.prepared().len(), 1);
    assert!(slots.bindings().is_empty());
    let frame_content = NativeContent::Frame(source);
    assert!(slots
        .prepare(&mut d.controller, &mut d.host, frame_content)
        .is_err());
}

#[cfg(feature = "capture")]
#[test]
fn e012_session_codec_preserves_schema_and_requires_fresh_restored_identity() {
    let mut d = demo::new().unwrap();
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    let EntityId::Workspace(root) = (match command(&mut d, Command::CreateWorkspace { owner: None })
    {
        core::Change::Created(e) => e,
        _ => panic!("workspace creation"),
    }) else {
        panic!("workspace")
    };
    command(
        &mut d,
        Command::MoveFrame {
            frame,
            destination: root,
            index: 0,
        },
    );
    let binding = NativeBinding {
        root,
        carrier: None,
        content: NativeContent::Frame(frame),
        return_anchor: ReturnAnchor::Frame {
            workspace: d.root,
            location: core::DockLocation::Append { workspace: d.root },
        },
    };
    let session =
        DesktopSession::capture(d.controller.snapshot(), 8, vec![binding.clone()]).unwrap();
    let wire = session.to_json(16384).unwrap();
    let restored = DesktopSession::from_json(&wire, 16384).unwrap();
    assert_eq!(restored, session);
    let layout = d.controller.core().save_layout();
    let encoded = kumihimo_layout_json::encode(&layout).unwrap();
    let decoded = kumihimo_layout_json::decode(&encoded).unwrap();
    assert_eq!(decoded.schema, 1);
    let fresh = core::Identity::new(decoded.source.lineage.clone(), "restored-core-2").unwrap();
    let options = core::RestoreOptions {
        providers: decoded
            .providers
            .iter()
            .map(|p| core::ProviderResolution {
                key: p.key.clone(),
                supports_shared_views: p.supports_shared_views,
            })
            .collect(),
        ..Default::default()
    };
    let new_core =
        core::Core::from_persisted(decoded.clone(), fresh.incarnation.clone(), options, 128)
            .unwrap();
    session
        .validate_after_restore(&decoded, new_core.snapshot(), &fresh)
        .unwrap();
    let rebound = session
        .rebind_after_restore(&decoded, new_core.snapshot(), &fresh)
        .unwrap();
    rebound.validate_current(new_core.snapshot()).unwrap();
    let mut duplicate = rebound.clone();
    duplicate.bindings.push(binding);
    assert!(duplicate.validate_current(new_core.snapshot()).is_err());
    assert!(DesktopSession::from_json(&wire, 4).is_err());
}

#[cfg(feature = "capture")]
#[test]
fn e013_diagnostics_are_redacted_valid_and_byte_bounded() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let (_, report) = paint(&mut d, &mut DesktopAdapter::default(), binding);
    let v = diagnostics::report_json(&report);
    let bytes = diagnostics::encode_bounded(&v, 1_000_000).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("Type here"));
    assert!(!text.contains("demo-restore"));
    assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_ok());
    assert!(diagnostics::encode_bounded(&v, 8).is_err());
}
