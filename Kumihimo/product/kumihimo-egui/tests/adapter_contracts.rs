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

fn theme_frame(
    ctx: &egui::Context,
    d: &mut demo::Demo,
    adapter: &mut DesktopAdapter,
    binding: SurfaceBinding,
) -> (egui::FullOutput, FrameReport) {
    let mut report = None;
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(to_egui(binding.bounds)),
            ..Default::default()
        },
        |ctx| {
            report = Some(adapter.show(ctx, &mut d.controller, binding, &mut d.host).unwrap());
        },
    );
    (output, report.unwrap())
}

fn custom_theme() -> chrome::ThemeTokens {
    chrome::ThemeTokens {
        preview_stroke: egui::Color32::from_rgb(219, 37, 173),
        resize_handle_stroke: egui::Color32::from_rgb(13, 193, 211),
        divider_stroke: egui::Color32::from_rgb(211, 151, 17),
        rail_fill: egui::Color32::from_rgb(83, 19, 143),
        rail_thumb_fill: egui::Color32::from_rgb(173, 229, 41),
    }
}

fn has_theme_paint(shape: &egui::Shape, color: egui::Color32, filled: bool) -> bool {
    match shape {
        egui::Shape::Vec(shapes) => shapes.iter().any(|shape| has_theme_paint(shape, color, filled)),
        egui::Shape::Rect(rect) if filled => rect.fill == color,
        egui::Shape::Rect(rect) => rect.stroke.color == color,
        egui::Shape::LineSegment { stroke, .. } if !filled => stroke.color == color,
        _ => false,
    }
}

#[test]
fn caller_visuals_supply_default_semantic_theme() {
    assert!(HostPolicy::default().theme_tokens.is_none());
    let dark = egui::Visuals::dark();
    let light = egui::Visuals::light();
    for visuals in [&dark, &light] {
        let theme = chrome::ThemeTokens::from_visuals(visuals);
        assert_eq!(theme.preview_stroke, visuals.selection.stroke.color);
        assert_eq!(theme.resize_handle_stroke, visuals.widgets.inactive.fg_stroke.color);
        assert_eq!(theme.divider_stroke, visuals.widgets.noninteractive.bg_stroke.color);
        assert_eq!(theme.rail_fill, visuals.extreme_bg_color);
        assert_eq!(theme.rail_thumb_fill, visuals.widgets.inactive.bg_fill);
    }
    assert_ne!(chrome::ThemeTokens::from_visuals(&dark), chrome::ThemeTokens::from_visuals(&light));
}

#[test]
fn host_theme_reaches_shared_controls_without_changing_identity_geometry_or_input() {
    let mut d = demo::new().unwrap();
    let binding = surface(d.root);
    let ctx = egui::Context::default();
    let mut adapter = DesktopAdapter::default();
    for _ in 0..3 {
        theme_frame(&ctx, &mut d, &mut adapter, binding);
    }
    #[cfg(feature = "navigation")]
    {
        d.host.navigation_mode(&mut d.controller, kumihimo_content::NavigationMode::ScrollbarOnly).unwrap();
        for _ in 0..3 {
            theme_frame(&ctx, &mut d, &mut adapter, binding);
        }
    }
    let snapshot = d.controller.snapshot().clone();
    let (_, baseline) = theme_frame(&ctx, &mut d, &mut adapter, binding);
    let controls = |report: &FrameReport| report.controls.iter().map(|control| {
        (control.id.clone(), control.bounds, control.target, control.enabled)
    }).collect::<Vec<_>>();
    let slots = |report: &FrameReport| report.slots.iter().map(|slot| {
        (slot.view, slot.resource, slot.frame, slot.workspace, slot.bounds, slot.clip, slot.epoch)
    }).collect::<Vec<_>>();
    let input = (adapter.input.focused_view, adapter.input.last_settlement, adapter.input.consumed_pointer);
    let theme = custom_theme();
    for (visuals, selected) in [
        (egui::Visuals::dark(), None),
        (egui::Visuals::light(), None),
        (egui::Visuals::dark(), Some(theme)),
        (egui::Visuals::light(), Some(theme)),
    ] {
        let expected = selected.unwrap_or_else(|| chrome::ThemeTokens::from_visuals(&visuals));
        ctx.set_visuals(visuals);
        d.host.policy.theme_tokens = selected;
        let (output, report) = theme_frame(&ctx, &mut d, &mut adapter, binding);
        assert!(output.shapes.iter().any(|shape| has_theme_paint(&shape.shape, expected.resize_handle_stroke, false)));
        assert!(output.shapes.iter().any(|shape| has_theme_paint(&shape.shape, expected.divider_stroke, false)));
        #[cfg(feature = "navigation")]
        for color in [expected.rail_fill, expected.rail_thumb_fill] {
            assert!(output.shapes.iter().any(|shape| has_theme_paint(&shape.shape, color, true)));
        }
        assert_eq!(d.controller.snapshot(), &snapshot);
        assert_eq!(controls(&report), controls(&baseline));
        assert_eq!(slots(&report), slots(&baseline));
        assert_eq!((adapter.input.focused_view, adapter.input.last_settlement, adapter.input.consumed_pointer), input);
        assert!(adapter.input.active_capture().is_none());
    }
}

#[test]
fn selected_theme_paints_actual_menu_preview_without_committing_it() {
    let mut h = ui_proof(3, false);
    h.run_steps(3);
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Preview in-app float");
    assert!(h.state().adapter.chrome.preview.is_some());
    let snapshot = h.state().demo.controller.snapshot().clone();
    let generation = h.state().adapter.chrome.preview.as_ref().unwrap().generation();
    let ctx = h.ctx.clone();
    for (visuals, selected) in [
        (egui::Visuals::dark(), None),
        (egui::Visuals::light(), None),
        (egui::Visuals::light(), Some(custom_theme())),
    ] {
        let expected = selected.unwrap_or_else(|| chrome::ThemeTokens::from_visuals(&visuals));
        ctx.set_visuals(visuals);
        let state = h.state_mut();
        state.demo.host.policy.theme_tokens = selected;
        let (output, _) = theme_frame(&ctx, &mut state.demo, &mut state.adapter, state.binding);
        assert!(output.shapes.iter().any(|shape| has_theme_paint(&shape.shape, expected.preview_stroke, false)));
        assert_eq!(state.demo.controller.snapshot(), &snapshot);
        assert_eq!(state.adapter.chrome.preview.as_ref().unwrap().generation(), generation);
        assert!(state.adapter.input.active_capture().is_none());
    }
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

    let mut d = demo::new().unwrap();
    let before = d.controller.snapshot().clone();
    let (&floating_frame, floating) = before.floats.iter().next().unwrap();
    assert_eq!(
        floating.rect,
        core::Rect::new(570.0, 320.0, 330.0, 250.0).unwrap()
    );
    let mut narrow = surface(d.root);
    narrow.bounds = core::Rect::new(0.0, 0.0, 460.0, 420.0).unwrap();
    narrow.epoch += 1;
    let projection = workspace::project(&mut d.controller, narrow, &mut d.host).unwrap();
    assert!(projection.rejected.is_empty());
    let after = d.controller.snapshot();
    assert_eq!(after.identity, before.identity);
    assert_eq!(after.frames, before.frames);
    assert_eq!(after.views, before.views);
    assert_eq!(after.workspaces, before.workspaces);
    assert_eq!(
        after.floats.keys().collect::<Vec<_>>(),
        before.floats.keys().collect::<Vec<_>>()
    );
    let fitted = &after.floats[&floating_frame];
    assert_eq!(fitted.return_location, floating.return_location);
    assert_eq!(fitted.kind, floating.kind);
    assert_eq!(
        fitted.rect,
        core::Rect::new(130.0, 170.0, 330.0, 250.0).unwrap()
    );
    assert!(narrow.bounds.contains(fitted.rect));
    assert_eq!(after.contexts[&d.root].viewport, narrow.bounds);
    assert!(projection
        .frames
        .iter()
        .any(|f| f.frame == floating_frame && f.bounds == fitted.rect));

    let mut context = after.contexts[&d.root].clone();
    context
        .minimums
        .insert(floating_frame, core::Size::new(330.0, 250.0).unwrap());
    let root = d.root;
    command(
        &mut d,
        Command::SetGeometryContext {
            workspace: root,
            context,
        },
    );
    let before_rejection = d.controller.snapshot().clone();
    narrow.bounds = core::Rect::new(0.0, 0.0, 200.0, 200.0).unwrap();
    narrow.epoch += 1;
    let (output, report) = paint(&mut d, &mut DesktopAdapter::default(), narrow);
    assert_eq!(d.controller.snapshot(), &before_rejection);
    assert!(!report.rejected.is_empty());
    assert!(report.slots.is_empty());
    assert!(report
        .controls
        .iter()
        .any(|c| c.id == "workspace/layout-unavailable"));
    assert!(output.shapes.iter().any(|shape| match &shape.shape {
        egui::epaint::Shape::Text(text) => text.galley.text().contains("Panes / reveal"),
        _ => false,
    }));
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
    // Prepare destination geometry before moving its view-anchored toolbar.
    command(
        &mut d,
        Command::SetGeometryContext {
            workspace: root,
            context: core::GeometryContext::new(surface(root).bounds),
        },
    );
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

#[cfg(feature = "capture")]
#[test]
fn e012_session_negative_bindings_reject_before_native_association() {
    let mut d = demo::new().unwrap();
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    let origin = d.root;
    let root = match command(&mut d, Command::CreateWorkspace { owner: None }) {
        core::Change::Created(EntityId::Workspace(id)) => id,
        _ => panic!("native destination root"),
    };
    command(
        &mut d,
        Command::SetGeometryContext {
            workspace: root,
            context: core::GeometryContext::new(surface(root).bounds),
        },
    );
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
            workspace: origin,
            location: core::DockLocation::WorkspaceEdge {
                workspace: origin,
                edge: core::Edge::Left,
            },
        },
    };
    let session = DesktopSession::capture(d.controller.snapshot(), 8, vec![binding]).unwrap();
    let before = d.controller.snapshot().clone();
    for (case, rejected) in [
        ("foreign root", {
            let mut value = session.clone();
            value.bindings[0].root = core::WorkspaceId::from_raw(u64::MAX - 1);
            value
        }),
        ("foreign frame", {
            let mut value = session.clone();
            value.bindings[0].content = NativeContent::Frame(core::FrameId::from_raw(u64::MAX - 1));
            value
        }),
        ("foreign carrier", {
            let mut value = session.clone();
            value.bindings[0].carrier = Some(core::FrameId::from_raw(u64::MAX - 1));
            value
        }),
        ("owned root", {
            let mut value = session.clone();
            value.bindings[0].root = d
                .controller
                .snapshot()
                .workspaces
                .values()
                .find(|workspace| workspace.owner.is_some())
                .unwrap()
                .id;
            value
        }),
        ("duplicate binding", {
            let mut value = session.clone();
            value.bindings.push(value.bindings[0].clone());
            value
        }),
        ("stale generation", {
            let mut value = session.clone();
            value.source_generation = value.source_generation.checked_sub(1).unwrap();
            value
        }),
        ("foreign incarnation", {
            let mut value = session.clone();
            value.source =
                core::Identity::new(value.source.lineage.clone(), "different-session-owner")
                    .unwrap();
            value
        }),
        ("wrong anchor kind", {
            let mut value = session.clone();
            value.bindings[0].return_anchor = ReturnAnchor::View { frame, index: 0 };
            value
        }),
    ] {
        assert!(
            rejected.validate_current(d.controller.snapshot()).is_err(),
            "{case}"
        );
        assert_eq!(
            d.controller.snapshot(),
            &before,
            "{case} mutated canonical state"
        );
    }
    command(&mut d, Command::Maximize { frame });
    assert!(matches!(
        DesktopSession::capture(d.controller.snapshot(), 8, session.bindings.clone()),
        Err(AdapterError::Invalid(
            "owned/duplicate/maximized native root"
        ))
    ));
    command(&mut d, Command::RestoreLayout { workspace: root });
    DesktopSession::capture(d.controller.snapshot(), 8, session.bindings.clone()).unwrap();
    command(
        &mut d,
        Command::CreateFrame {
            workspace: root,
            index: 1,
        },
    );
    let occupied = d.controller.snapshot().clone();
    assert!(matches!(
        DesktopSession::capture(d.controller.snapshot(), 8, session.bindings),
        Err(AdapterError::Invalid(
            "occupied or empty committed native root"
        ))
    ));
    assert_eq!(d.controller.snapshot(), &occupied);
    assert_eq!(d.controller.snapshot().identity, before.identity);
    assert_eq!(d.controller.snapshot().views, before.views);
}

#[cfg(feature = "native")]
struct PreparationFaultHost {
    inner: demo::DemoHost,
    fail_workspace: bool,
    fail_carrier: bool,
}
#[cfg(feature = "native")]
impl DesktopHost for PreparationFaultHost {
    fn policy(&self) -> HostPolicy {
        self.inner.policy()
    }
    fn dispatch(
        &mut self,
        controller: &mut kumihimo_content::Controller,
        command: Command,
    ) -> Result<core::Response> {
        if (self.fail_workspace && matches!(command, Command::CreateWorkspace { .. }))
            || (self.fail_carrier && matches!(command, Command::CreateFrame { .. }))
        {
            return Err(AdapterError::Host(
                "injected preparation dispatch failure".into(),
            ));
        }
        self.inner.dispatch(controller, command)
    }
    fn paint_content(
        &mut self,
        ui: &mut egui::Ui,
        slot: &RenderSlot,
        controller: &mut kumihimo_content::Controller,
    ) -> Result<()> {
        self.inner.paint_content(ui, slot, controller)
    }
}

#[cfg(feature = "native")]
#[test]
fn e012_failed_and_interrupted_preparation_retains_source_and_bounded_slots() {
    let mut d = demo::new().unwrap();
    d.host.policy.max_native_slots = 1;
    let view = d.text_view;
    let source = d.controller.snapshot().views[&view].frame;
    let before = d.controller.snapshot().clone();
    let mut host = PreparationFaultHost {
        inner: d.host,
        fail_workspace: true,
        fail_carrier: false,
    };
    let mut slots = window_slots::WindowSlots::default();
    assert!(slots
        .prepare(&mut d.controller, &mut host, NativeContent::View(view))
        .is_err());
    assert_eq!(d.controller.snapshot(), &before);
    assert!(slots.prepared().is_empty());
    assert!(slots.bindings().is_empty());

    host.fail_workspace = false;
    host.fail_carrier = true;
    assert!(slots
        .prepare(&mut d.controller, &mut host, NativeContent::View(view))
        .is_err());
    assert_eq!(d.controller.snapshot().views, before.views);
    assert_eq!(d.controller.snapshot().frames, before.frames);
    assert_eq!(slots.prepared().len(), 1);
    assert!(slots.prepared()[0].carrier.is_none());
    assert!(slots.bindings().is_empty());
    let partially_prepared = d.controller.snapshot().clone();
    assert!(slots
        .prepare(&mut d.controller, &mut host, NativeContent::View(view))
        .is_err());
    assert_eq!(d.controller.snapshot(), &partially_prepared);
    assert_eq!(slots.prepared().len(), 1);
    let mut pending = slots
        .prepare(&mut d.controller, &mut host, NativeContent::Frame(source))
        .unwrap();
    slots.cancel(&pending, d.controller.snapshot()).unwrap();
    assert_eq!(slots.prepared()[0].root, pending.slot.root);
    assert!(slots.bindings().is_empty());

    // Interrupt the pending handoff with a real canonical source move, not a forged Ready.
    let destination_bounds = surface(pending.slot.root).bounds;
    slots
        .arm(
            &mut pending,
            &mut d.controller,
            &mut host,
            destination_bounds,
        )
        .unwrap();
    host.inner
        .dispatch(
            &mut d.controller,
            Command::MoveFrame {
                frame: source,
                destination: pending.slot.root,
                index: 0,
            },
        )
        .unwrap()
        .outcome
        .result
        .unwrap();
    let interrupted = d.controller.snapshot().clone();
    assert!(slots.cancel(&pending, d.controller.snapshot()).is_err());
    let mut stale = pending;
    assert!(slots
        .arm(&mut stale, &mut d.controller, &mut host, destination_bounds)
        .is_err());
    assert_eq!(d.controller.snapshot(), &interrupted);
    assert!(slots.bindings().is_empty());
    assert_eq!(d.controller.snapshot().identity, before.identity);
    assert_eq!(d.controller.snapshot().views, before.views);
}

use egui_kittest::kittest::Queryable as _;

struct UiProof {
    demo: demo::Demo,
    adapter: DesktopAdapter,
    binding: SurfaceBinding,
    report: FrameReport,
    native_enabled: bool,
    trace: Vec<serde_json::Value>,
}
struct NoNative<'a>(&'a mut demo::DemoHost);
impl DesktopHost for NoNative<'_> {
    fn policy(&self) -> HostPolicy {
        self.0.policy()
    }
    fn capabilities(&self) -> Capabilities {
        let mut caps = self.0.capabilities();
        caps.native = false;
        caps
    }
    fn dispatch(
        &mut self,
        controller: &mut kumihimo_content::Controller,
        command: Command,
    ) -> Result<core::Response> {
        self.0.dispatch(controller, command)
    }
    fn paint_content(
        &mut self,
        ui: &mut egui::Ui,
        slot: &RenderSlot,
        controller: &mut kumihimo_content::Controller,
    ) -> Result<()> {
        self.0.paint_content(ui, slot, controller)
    }
}
fn ui_proof(tabs: usize, maximize: bool) -> egui_kittest::Harness<'static, UiProof> {
    let mut d = demo::with_counts(2, tabs).unwrap();
    let frame = d.controller.snapshot().views[&d.text_view].frame;
    if maximize {
        command(&mut d, Command::Maximize { frame });
    }
    let binding = surface(d.root);
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(960.0, 640.0))
        .build_state(
            |ctx, state: &mut UiProof| {
                state.report = if state.native_enabled {
                    state
                        .adapter
                        .show(
                            ctx,
                            &mut state.demo.controller,
                            state.binding,
                            &mut state.demo.host,
                        )
                        .unwrap()
                } else {
                    state
                        .adapter
                        .show(
                            ctx,
                            &mut state.demo.controller,
                            state.binding,
                            &mut NoNative(&mut state.demo.host),
                        )
                        .unwrap()
                };
                let events = ctx.input(|input| input.events.iter().filter(|event| matches!(event,
                    egui::Event::Touch { .. } | egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. }
                    | egui::Event::PointerGone | egui::Event::Key { .. } | egui::Event::WindowFocused(_)))
                    .take(64).cloned().collect::<Vec<_>>());
                if !events.is_empty() || state.adapter.chrome.preview.is_some() || !state.report.rejected.is_empty() {
                    if state.trace.len() == 16 { state.trace.remove(0); }
                    state.trace.push(serde_json::json!({
                        "events":format!("{events:?}"),"generation":state.demo.controller.snapshot().generation,
                        "epoch":state.binding.epoch,"report_epoch":state.report.epoch,
                        "pending_preview_generation":state.adapter.chrome.preview.as_ref().map(|preview|preview.generation()),
                        "floats":state.demo.controller.snapshot().floats.keys().map(|frame|frame.raw()).collect::<Vec<_>>(),
                        "rejected":state.report.rejected,"focus":format!("{:?}",ctx.memory(|memory|memory.focused())),
                        "input_focused":ctx.input(|input|input.focused),"popup":egui::Popup::is_any_open(ctx),
                        "controls":state.report.controls.iter().take(128).map(|control|(&control.id,control.enabled)).collect::<Vec<_>>(),
                        "capture":format!("{:?}",state.adapter.input.active_capture())
                        ,"pass":ctx.cumulative_pass_nr(),
                        "control_hit":ctx.data(|data|data.get_temp::<serde_json::Value>(egui::Id::new("kumihimo-control-hit"))),
                        "group_move_result":ctx.data(|data|data.get_temp::<serde_json::Value>(egui::Id::new("kumihimo-group-move-result"))),
                        "swap_result":ctx.data(|data|data.get_temp::<serde_json::Value>(egui::Id::new("kumihimo-swap-result"))),
                        "focused_response":ctx.memory(|memory|memory.focused()).and_then(|id|ctx.read_response(id))
                            .map(|response|serde_json::json!({"actual_id":format!("{:?}",response.id),
                                "rect":[response.rect.min.x,response.rect.min.y,response.rect.max.x,response.rect.max.y],
                                "layer":format!("{:?}",response.layer_id)}))
                    }));
                }
            },
            UiProof {
                demo: d,
                adapter: DesktopAdapter::default(),
                binding,
                report: FrameReport::default(),
                native_enabled: true,
                trace: Vec::new(),
            },
        )
}
fn ui_click_point(h: &mut egui_kittest::Harness<'_, UiProof>, point: egui::Pos2) {
    h.hover_at(point);
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    }
    h.run_steps(3);
}
fn ui_click_id(h: &mut egui_kittest::Harness<'_, UiProof>, id: &str) {
    let point = to_egui(
        h.state()
            .report
            .controls
            .iter()
            .find(|c| c.id == id && c.enabled)
            .unwrap_or_else(|| panic!("current pointer target {id}; trace: {:?}", h.state().trace))
            .bounds,
    )
    .center();
    ui_click_point(h, point);
}
fn ui_click(h: &mut egui_kittest::Harness<'_, UiProof>, label: &str) {
    if label == "Frame" || label == "Tabs / groups" {
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        let state = h.state();
        let frame = state.demo.controller.snapshot().views[&state.demo.text_view].frame;
        let suffix = if label == "Frame" {
            "menu"
        } else {
            "tabs-menu"
        };
        let point = to_egui(
            state
                .report
                .controls
                .iter()
                .find(|c| c.id == format!("frame/{}/{suffix}", frame.raw()))
                .unwrap_or_else(|| {
                    panic!(
                        "current frame {} menu {suffix}; trace: {:?}",
                        frame.raw(),
                        state.trace
                    )
                })
                .bounds,
        )
        .center();
        ui_click_point(h, point);
        return;
    }
    if label.starts_with("Frame ") {
        for _ in 0..64 {
            if let Some(target) = h
                .state()
                .report
                .controls
                .iter()
                .find(|control| {
                    control.enabled
                        && control.label == label
                        && (control.id.starts_with("destination/group-destinations/")
                            || control.id.starts_with("destination/swap-destinations/"))
                })
                .cloned()
            {
                let mut target = target;
                for _ in 0..8 {
                    h.hover_at(to_egui(target.bounds).center());
                    h.run_steps(1);
                    let current = h
                        .state()
                        .report
                        .controls
                        .iter()
                        .find(|control| control.id == target.id && control.enabled)
                        .unwrap_or_else(|| {
                            panic!(
                                "current destination {}; trace: {:?}",
                                target.id,
                                h.state().trace
                            )
                        })
                        .clone();
                    if current.bounds == target.bounds {
                        ui_click_id(h, &current.id);
                        return;
                    }
                    target = current;
                }
                panic!(
                    "unstable destination {}; trace: {:?}",
                    target.id,
                    h.state().trace
                );
            }
            let next = h
                .state()
                .report
                .controls
                .iter()
                .find(|c| {
                    c.enabled
                        && (c.id == "page/group-destinations/next"
                            || c.id == "page/swap-destinations/next")
                })
                .unwrap_or_else(|| {
                    panic!(
                        "destination {label}; pages: {:?}; controls: {:?}; popup: {}; trace: {:?}",
                        h.state().adapter.chrome.pages,
                        h.state()
                            .report
                            .controls
                            .iter()
                            .map(|control| (&control.id, control.enabled))
                            .collect::<Vec<_>>(),
                        egui::Popup::is_any_open(&h.ctx),
                        h.state().trace
                    )
                })
                .clone();
            let point = to_egui(next.bounds).center();
            ui_click_point(h, point);
        }
    }
    if let Some(point) = h
        .state()
        .report
        .controls
        .iter()
        .find(|c| c.label == label && c.enabled)
        .map(|c| to_egui(c.bounds).center())
    {
        ui_click_point(h, point);
        return;
    }
    let decorated = format!("{label} \u{23f5}");
    let accessible = if h.query_all_by_label(label).next().is_some() {
        label
    } else {
        &decorated
    };
    h.get_all_by_label(accessible)
        .next()
        .unwrap()
        .click_accesskit();
    h.run_steps(3);
}
fn ui_tab_actions(h: &mut egui_kittest::Harness<'_, UiProof>, view: core::ViewId) {
    let state = h.state_mut();
    let frame = state.demo.controller.snapshot().views[&view].frame;
    let index = state.demo.controller.snapshot().frames[&frame]
        .views
        .iter()
        .position(|v| *v == view)
        .unwrap();
    state.demo.host.policy.tab_page_size = 1;
    state
        .adapter
        .chrome
        .pages
        .insert(format!("tabs/{}", frame.raw()), index);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    ui_click_id(h, &format!("frame/{}/tabs-menu", frame.raw()));
    ui_click_id(h, &format!("view/{}/actions-menu", view.raw()));
}
fn ui_pointer(
    h: &mut egui_kittest::Harness<'_, UiProof>,
    pointer: u64,
    source: input::PointerSource,
    phase: input::PointerPhase,
    position: egui::Pos2,
) {
    let epoch = h.state().binding.epoch;
    h.state_mut()
        .adapter
        .input
        .feed_pointer(input::HostPointerEvent {
            pointer,
            source,
            phase,
            position,
            epoch,
        })
        .unwrap();
    h.run_steps(1);
}

#[test]
fn e002_widget_reorder_group_move_collapse_and_guarded_close() {
    let mut h = ui_proof(8, true);
    let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
    let original = h.state().demo.controller.snapshot().frames[&frame]
        .views
        .clone();
    let last = *original.last().unwrap();
    ui_tab_actions(&mut h, last);
    ui_click(&mut h, "Move tab first");
    assert_eq!(
        h.state().demo.controller.snapshot().frames[&frame].views[0],
        last,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert_eq!(
        h.state().report.canonical_views,
        16,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    let group = {
        let state = h.state_mut();
        let core::Change::Created(EntityId::Group(group)) = command(
            &mut state.demo,
            Command::CreateGroup {
                frame,
                label: "Proof group".into(),
                color: Some("#ee2211".into()),
            },
        ) else {
            panic!("group");
        };
        for view in [original[0], last] {
            command(
                &mut state.demo,
                Command::AssignGroup {
                    view,
                    group: Some(group),
                },
            );
        }
        state
            .adapter
            .chrome
            .workspace_settings
            .entry(state.demo.root)
            .or_default()
            .group_colors = Some(false);
        group
    };
    h.run_steps(3);
    ui_click(&mut h, "Tabs / groups");
    ui_click(&mut h, "Group: Proof group");
    ui_click(&mut h, "Collapse group");
    assert!(
        h.state().demo.controller.snapshot().groups[&group].collapsed,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    ui_click(&mut h, "Tabs / groups");
    ui_click(&mut h, "Group: Proof group");
    ui_click(&mut h, "Expand group");
    assert!(
        !h.state().demo.controller.snapshot().groups[&group].collapsed,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    // Derive the nested context through the actual restore route before hiding it again.
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Restore workspace");
    let destination = h.state().demo.controller.snapshot().views[&h.state().demo.canvas_view].frame;
    let destination_workspace = h.state().demo.controller.snapshot().frames[&destination].workspace;
    assert!(
        h.state()
            .demo
            .controller
            .snapshot()
            .contexts
            .contains_key(&destination_workspace),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Maximize frame");
    assert!(
        !h.state()
            .report
            .controls
            .iter()
            .any(|control| control.id == format!("frame/{}/menu", destination.raw())),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Tabs / groups");
    ui_click(&mut h, "Group: Proof group");
    ui_click(&mut h, "Move entire canonical group to\u{2026}");
    ui_click(&mut h, &format!("Frame {}", destination.raw()));
    let snapshot = h.state().demo.controller.snapshot();
    assert_eq!(
        snapshot.groups[&group].frame,
        destination,
        "group destination stage; trace: {:?}",
        h.state().trace
    );
    assert!(
        snapshot.frames[&destination].views.contains(&original[0]),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert!(
        snapshot.frames[&destination].views.contains(&last),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    ui_tab_actions(&mut h, original[2]);
    ui_click(&mut h, "Close (host/provider approval)");
    assert!(
        h.state().demo.controller.snapshot().views[&original[2]]
            .closed
            .is_none(),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert!(
        h.state()
            .demo
            .host
            .receipts
            .iter()
            .any(|r| r.view == original[2] && r.close_approved == Some(false)),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    {
        let state = h.state_mut();
        command(
            &mut state.demo,
            Command::SetPinPolicy(core::PinPolicy {
                deny_close: true,
                deny_reorder: true,
                deny_cross_frame_move: true,
            }),
        );
    }
    let before = h.state().demo.controller.snapshot().frames[&frame]
        .views
        .clone();
    let pinned_index = before.iter().position(|view| *view == original[1]).unwrap();
    assert_eq!(
        pinned_index,
        0,
        "pin fixture index; trace: {:?}",
        h.state().trace
    );
    assert!(
        before.len() > 1,
        "pin fixture needs distinct last index; trace: {:?}",
        h.state().trace
    );
    assert!(h.state().demo.controller.snapshot().views[&original[1]].pinned);
    ui_tab_actions(&mut h, original[1]);
    let attempt_after_pass = h.ctx.cumulative_pass_nr();
    let pin_error = core::Error::Pinned(original[1]).to_string();
    ui_click(&mut h, "Move tab last");
    assert_eq!(
        h.state().demo.controller.snapshot().frames[&frame].views,
        before,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert!(
        h.state().trace.iter().any(|receipt| receipt["pass"]
            .as_u64()
            .is_some_and(|pass| pass > attempt_after_pass)
            && receipt["rejected"]
                .as_array()
                .is_some_and(|rejections| rejections
                    .iter()
                    .any(|error| error.as_str() == Some(pin_error.as_str())))),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
}

fn assert_preview_controls_fit(h: &egui_kittest::Harness<'_, UiProof>) {
    let report = &h.state().report;
    let bounds = to_egui(h.state().binding.bounds);
    let controls: Vec<_> = report
        .controls
        .iter()
        .filter(|control| control.id.starts_with("workspace/preview/"))
        .collect();
    assert_eq!(controls.len(), 2, "both preview actions remain reachable");
    for control in &controls {
        let rect = to_egui(control.bounds);
        assert!(
            bounds.contains_rect(rect),
            "preview action exceeds surface: {control:?}"
        );
        let winner = h.ctx.layer_id_at(rect.center()).expect("preview hit layer");
        assert_eq!(
            winner.order,
            egui::Order::Foreground,
            "preview is above pane content"
        );
        assert!(
            h.ctx
                .memory(|memory| memory.area_rect(winner.id))
                .is_some_and(|area| area.contains_rect(rect)),
            "actual preview area contains control"
        );
        for header in report.controls.iter().filter(|other| {
            (other.id.starts_with("frame/")
                && ["/drag", "/menu", "/tabs-menu"]
                    .iter()
                    .any(|suffix| other.id.ends_with(suffix)))
                || other.id.ends_with("/tab")
        }) {
            assert!(
                !rect.intersects(to_egui(header.bounds)),
                "preview action overlaps pane header: {control:?} / {header:?}"
            );
        }
    }
    assert!(!to_egui(controls[0].bounds).intersects(to_egui(controls[1].bounds)));
}

fn assert_revealed_header_geometry(h: &egui_kittest::Harness<'_, UiProof>) {
    let state = h.state();
    for slot in &state.report.slots {
        if !state.adapter.chrome.revealed.contains(&slot.frame) {
            continue;
        }
        let (settings, _, _) = state
            .adapter
            .chrome
            .resolve(
                &state.demo.host.policy,
                slot.workspace,
                state.binding.viewport,
            )
            .unwrap();
        let bottom = slot.canonical_bounds.y()
            + f64::from(settings.header_height).min(slot.canonical_bounds.height());
        assert!(
            slot.bounds.y() >= bottom && slot.clip.y() >= bottom,
            "revealed header must be outside provider bounds and clip: {slot:?}"
        );
        for control in state.report.controls.iter().filter(|control| {
            control.id == format!("frame/{}/menu", slot.frame.raw())
                || control.id == format!("frame/{}/tabs-menu", slot.frame.raw())
        }) {
            assert!(
                !to_egui(control.bounds).intersects(to_egui(slot.clip)),
                "revealed controls cannot be painted over by provider: {control:?} / {slot:?}"
            );
        }
    }
}

#[test]
fn e003_widget_preview_cancel_commit_redock_equalize_swap_and_restore() {
    let mut layered = demo::new().unwrap();
    let floating = *layered.controller.snapshot().floats.keys().next().unwrap();
    let binding = surface(layered.root);
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut layered_adapter = DesktopAdapter::default();
    let render = |demo: &mut demo::Demo, adapter: &mut DesktopAdapter| {
        let mut report = None;
        let output = ctx.run(
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
        );
        (output, report.unwrap())
    };
    let assert_paint_order = |output: &egui::FullOutput, report: &FrameReport| {
        let menu = report
            .controls
            .iter()
            .find(|control| control.id == format!("frame/{}/menu", floating.raw()))
            .expect("actual floating header control");
        let menu_rect = to_egui(menu.bounds);
        let overlap = menu_rect.center();
        let docked = report
            .slots
            .iter()
            .find(|slot| slot.frame != floating && to_egui(slot.clip).contains(overlap))
            .expect("floating header overlaps actual docked provider clip");
        let docked_clip = to_egui(docked.clip);
        let last_docked_paint = output
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, shape)| {
                docked_clip.contains_rect(shape.clip_rect)
                    && shape.shape.visual_bounding_rect().intersects(docked_clip)
            })
            .map(|(index, _)| index)
            .last()
            .expect("actual docked provider paint shapes");
        let floating_label = output
            .shapes
            .iter()
            .position(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => menu_rect.contains(text.pos),
                _ => false,
            })
            .expect("actual floating menu text shape");
        assert!(floating_label > last_docked_paint,
        "float header paint must follow docked provider paint at overlap: {floating_label} <= {last_docked_paint}");
    };
    for _ in 0..3 {
        let (output, report) = render(&mut layered, &mut layered_adapter);
        assert_paint_order(&output, &report);
    }
    let owner = layered
        .controller
        .snapshot()
        .workspaces
        .values()
        .find_map(|workspace| workspace.owner)
        .expect("nested owner");
    command(&mut layered, Command::Maximize { frame: owner });
    let (_, hidden_report) = render(&mut layered, &mut layered_adapter);
    assert!(
        !hidden_report
            .controls
            .iter()
            .any(|control| control.id == format!("frame/{}/menu", floating.raw())),
        "maximize actually hides the float in the persistent context"
    );
    command(
        &mut layered,
        Command::RestoreLayout {
            workspace: binding.root,
        },
    );
    for _ in 0..3 {
        let (output, report) = render(&mut layered, &mut layered_adapter);
        assert_paint_order(&output, &report);
    }
    let mut h = ui_proof(3, false);
    let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Preview in-app float");
    assert!(
        h.state().adapter.chrome.preview.is_some(),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert!(
        !h.state()
            .demo
            .controller
            .snapshot()
            .floats
            .contains_key(&frame),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    assert_preview_controls_fit(&h);
    ui_click(&mut h, "Cancel preview (Escape)");
    assert!(
        h.state().adapter.chrome.preview.is_none(),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Preview in-app float");
    assert!(
        h.state().adapter.chrome.preview.is_some(),
        "float commit stage; trace: {:?}",
        h.state().trace
    );
    assert_preview_controls_fit(&h);
    ui_click(&mut h, "Commit preview");
    assert!(
        h.state()
            .demo
            .controller
            .snapshot()
            .floats
            .contains_key(&frame),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Preview redock");
    assert!(
        h.state().adapter.chrome.preview.is_some(),
        "redock commit stage; trace: {:?}",
        h.state().trace
    );
    assert_preview_controls_fit(&h);
    ui_click(&mut h, "Commit preview");
    assert!(
        !h.state()
            .demo
            .controller
            .snapshot()
            .floats
            .contains_key(&frame),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Equalize workspace");
    let snapshot = h.state().demo.controller.snapshot();
    let root = h.state().demo.root;
    let second = snapshot.workspaces[&root]
        .frames
        .iter()
        .copied()
        .find(|candidate| {
            *candidate != frame
                && !snapshot.floats.contains_key(candidate)
                && snapshot.layouts[&root].nodes.values().any(|node| {
                    *node == core::LayoutNode::Leaf(core::LayoutItem::Frame(*candidate))
                })
        })
        .unwrap_or_else(|| {
            panic!(
                "distinct docked swap destination; trace: {:?}",
                h.state().trace
            )
        });
    assert_ne!(
        second,
        frame,
        "distinct swap fixture; trace: {:?}",
        h.state().trace
    );
    let before = snapshot.layouts.clone();
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Swap with\u{2026}");
    let swap_after_pass = h.ctx.cumulative_pass_nr();
    ui_click(&mut h, &format!("Frame {}", second.raw()));
    let swap_result = h
        .ctx
        .data(|data| data.get_temp::<serde_json::Value>(egui::Id::new("kumihimo-swap-result")))
        .unwrap_or_else(|| panic!("missing actual swap dispatch; trace: {:?}", h.state().trace));
    assert!(
        swap_result["pass"]
            .as_u64()
            .is_some_and(|pass| pass > swap_after_pass),
        "fresh swap dispatch; trace: {:?}",
        h.state().trace
    );
    assert_eq!(
        swap_result["first"].as_u64(),
        Some(frame.raw()),
        "actual swap source; trace: {:?}",
        h.state().trace
    );
    assert_eq!(
        swap_result["second"].as_u64(),
        Some(second.raw()),
        "actual swap destination; trace: {:?}",
        h.state().trace
    );
    assert_ne!(
        h.state().demo.controller.snapshot().layouts,
        before,
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Maximize frame");
    assert!(
        !h.state().demo.controller.snapshot().maximized.is_empty(),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
    ui_click(&mut h, "Frame");
    ui_click(&mut h, "Restore workspace");
    assert!(
        h.state().demo.controller.snapshot().maximized.is_empty(),
        "canonical/widget stage; trace: {:?}",
        h.state().trace
    );
}

#[test]
fn e004_widget_tool_rotation_hidden_space_focus_and_closed_target() {
    let mut h = ui_proof(3, false);
    let tool = h.state().demo.tool;
    let target = h.state().demo.controller.snapshot().tools[&tool].target;
    let left = core::ToolPlacement::Edge {
        anchor: core::ToolAnchor::View(target.view),
        edge: core::Edge::Left,
    };
    {
        let state = h.state_mut();
        let mut policy = state.demo.controller.snapshot().tools[&tool].policy.clone();
        policy.orientations.push(core::Axis::Vertical);
        policy.allowed.push(left);
        command(&mut state.demo, Command::ConfigureTool { tool, policy });
    }
    h.run_steps(3);
    ui_click(&mut h, "Dock tool");
    ui_click(&mut h, &format!("{left:?}"));
    ui_click(&mut h, "Commit preview");
    assert_eq!(
        h.state().demo.controller.snapshot().tools[&tool]
            .policy
            .orientation,
        core::Axis::Vertical
    );
    let frame = h.state().demo.controller.snapshot().views[&target.view].frame;
    let other = h.state().demo.controller.snapshot().frames[&frame].views[1];
    {
        let state = h.state_mut();
        command(&mut state.demo, Command::SelectView { view: other });
        state.adapter.input.focused_view = Some(other);
    }
    h.run_steps(3);
    ui_click_id(&mut h, &format!("tool/{}/action", tool.raw()));
    let receipt = h
        .state()
        .demo
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
    for hidden_space in [core::HiddenSpace::Retain, core::HiddenSpace::Release] {
        {
            let state = h.state_mut();
            let mut policy = state.demo.controller.snapshot().tools[&tool].policy.clone();
            policy.hidden_space = hidden_space;
            policy.visibility = core::VisibilityPolicy::Hidden;
            command(&mut state.demo, Command::ConfigureTool { tool, policy });
        }
        h.run_steps(3);
        let state = h.state_mut();
        let projection = workspace::project_with_chrome(
            &mut state.demo.controller,
            state.binding,
            &mut state.demo.host,
            &state.adapter.chrome,
        )
        .unwrap();
        let item = projection.tools.iter().find(|t| t.tool == tool);
        if hidden_space == core::HiddenSpace::Retain {
            let item = item.unwrap();
            assert!(!item.presentation.visible);
            assert!(item.presentation.reserves_space);
        } else {
            assert!(item.is_none());
        }
    }
    {
        let state = h.state_mut();
        let record = state.demo.controller.snapshot().views[&target.view].clone();
        command(
            &mut state.demo,
            Command::SetViewMetadata {
                view: target.view,
                label: record.label,
                pinned: false,
                unsaved: false,
            },
        );
        command(
            &mut state.demo,
            Command::CloseView {
                view: target.view,
                approved: true,
            },
        );
    }
    let state = h.state_mut();
    assert!(state
        .demo
        .host
        .tool_action(&mut state.demo.controller, tool, "closed-target")
        .is_err());
    assert_eq!(state.demo.controller.snapshot().tools[&tool].target, target);
}

#[test]
fn e005_profiles_keyboard_and_touch_reveal_keep_settings_precedence() {
    for profile in [
        ChromeProfile::Full,
        ChromeProfile::Compact,
        ChromeProfile::Contextual,
        ChromeProfile::Hidden,
    ] {
        let mut h = ui_proof(3, true);
        {
            let state = h.state_mut();
            state.adapter.chrome.workspace_settings.insert(
                state.demo.root,
                chrome::SettingsPatch {
                    profile: Some(profile),
                    header_height: Some(38.0),
                    ..Default::default()
                },
            );
        }
        h.run_steps(3);
        let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
        if profile == ChromeProfile::Hidden || profile == ChromeProfile::Contextual {
            assert!(h
                .state()
                .report
                .controls
                .iter()
                .any(|c| c.id == format!("frame/{}/reveal", frame.raw())));
            h.key_press(egui::Key::F6);
            h.run_steps(3);
            assert!(h.state().adapter.chrome.revealed.contains(&frame));
            assert!(h
                .state()
                .report
                .controls
                .iter()
                .any(|c| c.id == format!("frame/{}/drag", frame.raw())));
            assert_revealed_header_geometry(&h);
        }
        if profile == ChromeProfile::Compact {
            assert!(h.query_by_label("\u{22ee}").is_some());
            assert!(h.query_by_label("Tabs").is_some());
            for suffix in ["menu", "tabs-menu"] {
                assert!(h.state().report.controls.iter().any(|control| control.id
                    == format!("frame/{}/{suffix}", frame.raw())
                    && control.enabled));
            }
        }
        {
            let state = h.state_mut();
            state.adapter.chrome.revealed.clear();
            state.adapter.chrome.surface_settings.insert(
                state.binding.viewport,
                chrome::SettingsPatch {
                    profile: Some(ChromeProfile::Hidden),
                    header_height: Some(46.0),
                    recreation_needed: true,
                    ..Default::default()
                },
            );
        }
        h.run_steps(3);
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id == format!("frame/{}/reveal", frame.raw()))
                .unwrap()
                .bounds,
        )
        .center();
        let mut touch_trace = Vec::new();
        for (phase, pressed) in [
            (egui::TouchPhase::Start, true),
            (egui::TouchPhase::End, false),
        ] {
            h.input_mut().events.push(egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(9),
                phase,
                pos: point,
                force: None,
            });
            h.input_mut().events.push(egui::Event::PointerMoved(point));
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.run_steps(1);
            touch_trace.push(h.ctx.data(|data| {
                data.get_temp::<serde_json::Value>(egui::Id::new("kumihimo-reveal-hit"))
            }));
        }
        h.run_steps(3);
        assert!(
            h.state().adapter.chrome.revealed.contains(&frame),
            "touch reveal frame {} at {:?}; revealed: {:?}; controls: {:?}; focus: {:?}; popup: {}; hit: {:?}; trace: {:?}",
            frame.raw(),
            point,
            h.state().adapter.chrome.revealed,
            h.state()
                .report
                .controls
                .iter()
                .map(|c| (&c.id, c.enabled))
                .collect::<Vec<_>>(),
            h.ctx.memory(|m| m.focused()),
            egui::Popup::is_any_open(&h.ctx), touch_trace, h.state().trace
        );
        assert_revealed_header_geometry(&h);
        let state = h.state_mut();
        let p = workspace::project_with_chrome(
            &mut state.demo.controller,
            state.binding,
            &mut state.demo.host,
            &state.adapter.chrome,
        )
        .unwrap();
        assert!(p.frames.iter().all(|f| f.settings_source == "surface"
            && f.recreation_needed
            && f.settings.profile == ChromeProfile::Hidden
            && f.settings.header_height == 46.0));
        for frame in p
            .frames
            .iter()
            .filter(|f| state.adapter.chrome.revealed.contains(&f.frame))
        {
            assert_eq!(
                frame.usable.y() - frame.content.y(),
                46.0_f64.min(frame.content.height())
            );
            if let Some(child) = frame.child {
                assert_eq!(
                    state.demo.controller.snapshot().contexts[&child].viewport,
                    frame.usable
                );
            }
        }
    }
}

#[test]
fn e006_actual_mouse_tab_and_resize_release_escape_capture_and_focus_loss() {
    let mut h = ui_proof(3, false);
    let view = h.state().demo.text_view;
    let frame = h.state().demo.controller.snapshot().views[&view].frame;
    let next = h.state().demo.controller.snapshot().frames[&frame].views[1];
    let label = h
        .state()
        .report
        .controls
        .iter()
        .find(|c| c.id == format!("view/{}/tab", next.raw()))
        .unwrap()
        .label
        .clone();
    h.get_by_label(&label).click();
    h.run_steps(3);
    assert_eq!(
        h.state().demo.controller.snapshot().frames[&frame].selected_view,
        Some(next)
    );
    let control = h
        .state()
        .report
        .controls
        .iter()
        .find(|c| c.id.starts_with("resize/"))
        .unwrap()
        .clone();
    let point = to_egui(control.bounds).center();
    let horizontal = control.id.split('/').nth(3) == Some("x");
    let moved = point
        + if horizontal {
            egui::vec2(35.0, 0.0)
        } else {
            egui::vec2(0.0, 35.0)
        };
    let before = h.state().demo.controller.snapshot().layouts.clone();
    h.hover_at(point);
    h.drag_at(point);
    h.run_steps(1);
    h.hover_at(moved);
    h.run_steps(1);
    h.drop_at(moved);
    h.run_steps(3);
    assert_eq!(h.state().adapter.input.last_settlement, Some("release"));
    assert_ne!(h.state().demo.controller.snapshot().layouts, before);
    for terminal in [
        egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerGone,
        egui::Event::WindowFocused(false),
    ] {
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id.starts_with("resize/"))
                .unwrap()
                .bounds,
        )
        .center();
        ui_pointer(
            &mut h,
            77,
            input::PointerSource::Mouse,
            input::PointerPhase::Press,
            point,
        );
        assert!(h.state().adapter.input.active_capture().is_some());
        let before = h.state().demo.controller.snapshot().layouts.clone();
        h.event(terminal);
        h.run_steps(1);
        assert!(h.state().adapter.input.active_capture().is_none());
        assert_eq!(h.state().demo.controller.snapshot().layouts, before);
        h.input_mut().focused = true;
        h.event(egui::Event::WindowFocused(true));
        h.run_steps(1);
    }
}

#[test]
fn e007_semantic_touch_and_pen_taps_cancel_outside_stale_and_scrolling() {
    for source in [input::PointerSource::Touch, input::PointerSource::HostPen] {
        let mut h = ui_proof(3, true);
        let view = h.state().demo.text_view;
        let frame = h.state().demo.controller.snapshot().views[&view].frame;
        let next = h.state().demo.controller.snapshot().frames[&frame].views[1];
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id == format!("view/{}/tab", next.raw()))
                .unwrap()
                .bounds,
        )
        .center();
        ui_pointer(&mut h, 81, source, input::PointerPhase::Press, point);
        ui_pointer(&mut h, 82, source, input::PointerPhase::Release, point);
        assert_eq!(
            h.state().demo.controller.snapshot().frames[&frame].selected_view,
            Some(view)
        );
        ui_pointer(&mut h, 81, source, input::PointerPhase::Release, point);
        assert_eq!(
            h.state().demo.controller.snapshot().frames[&frame].selected_view,
            Some(next)
        );
        let selected_generation = h.state().demo.controller.snapshot().generation;
        ui_pointer(&mut h, 81, source, input::PointerPhase::Release, point);
        assert_eq!(
            h.state().demo.controller.snapshot().generation,
            selected_generation
        );
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id == format!("view/{}/tab", view.raw()))
                .unwrap()
                .bounds,
        )
        .center();
        for phase in [input::PointerPhase::Cancel, input::PointerPhase::Move] {
            ui_pointer(&mut h, 83, source, input::PointerPhase::Press, point);
            ui_pointer(&mut h, 83, source, phase, point + egui::vec2(100.0, 0.0));
            ui_pointer(&mut h, 83, source, input::PointerPhase::Release, point);
            assert_eq!(
                h.state().demo.controller.snapshot().generation,
                selected_generation
            );
        }
        ui_pointer(&mut h, 84, source, input::PointerPhase::Press, point);
        h.state_mut().binding.epoch += 1;
        h.run_steps(1);
        assert!(h.state().adapter.input.active_capture().is_none());
        assert_eq!(h.state().adapter.input.last_settlement, Some("invalidated"));
    }
}

#[test]
fn e008_actual_editor_typing_and_both_conflicting_shortcut_policies() {
    for frame_first in [false, true] {
        let mut h = ui_proof(3, true);
        h.state_mut()
            .demo
            .host
            .policy
            .settings
            .frame_shortcuts_first = frame_first;
        let view = h.state().demo.text_view;
        let frame = h.state().demo.controller.snapshot().views[&view].frame;
        h.get_by_role(egui::accesskit::Role::MultilineTextInput)
            .click();
        h.run_steps(3);
        assert_eq!(h.state().adapter.input.focused_view, Some(view));
        h.get_by_role(egui::accesskit::Role::MultilineTextInput)
            .type_text("actual editor typing proof");
        h.run_steps(3);
        assert!(h
            .get_by_role(egui::accesskit::Role::MultilineTextInput)
            .value()
            .unwrap()
            .contains("actual editor typing proof"));
        h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Tab);
        h.run_steps(3);
        let selected = h.state().demo.controller.snapshot().frames[&frame].selected_view;
        if frame_first {
            assert_ne!(selected, Some(view));
        } else {
            assert_eq!(selected, Some(view));
        }
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        ui_tab_actions(&mut h, view);
        ui_click(&mut h, "Select tab");
        assert_eq!(
            h.state().demo.controller.snapshot().frames[&frame].selected_view,
            Some(view)
        );
    }
}

#[cfg(feature = "navigation")]
#[test]
fn e006_all_modes_rail_wheel_ack_is_distinct_from_content_wheel() {
    use kumihimo_content::{NavigationMode, Observation};
    for mode in [
        NavigationMode::ScrollbarOnly,
        NavigationMode::MinimapAlways,
        NavigationMode::ScrollbarAndMinimap,
        NavigationMode::ScrubToMinimap,
    ] {
        let mut h = ui_proof(3, true);
        {
            let state = h.state_mut();
            state
                .demo
                .host
                .navigation_mode(&mut state.demo.controller, mode)
                .unwrap();
        }
        h.run_steps(3);
        let view = h.state().demo.text_view;
        let mount = h.state().demo.host.mounts[&view];
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id == format!("view/{}/navigation", view.raw()))
                .unwrap()
                .bounds,
        )
        .center();
        let content_bounds = h
            .state()
            .report
            .slots
            .iter()
            .find(|s| s.view == view)
            .unwrap()
            .bounds;
        let receipt_count = h
            .state()
            .demo
            .host
            .receipts
            .iter()
            .filter(|r| r.kind == "navigation")
            .count();
        h.hover_at(point);
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -3.0),
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(3);
        assert!(
            h.state()
                .demo
                .host
                .receipts
                .iter()
                .filter(|r| r.kind == "navigation")
                .count()
                > receipt_count
        );
        let navigation = &h.state().demo.controller.inspect().navigation[&mount];
        assert!(
            matches!(&navigation.observation, Observation::Known(actual) if actual.offset.get() > 0.0)
        );
        assert_eq!(
            h.state()
                .report
                .slots
                .iter()
                .find(|s| s.view == view)
                .unwrap()
                .bounds,
            content_bounds
        );
        let receipt_count = h
            .state()
            .demo
            .host
            .receipts
            .iter()
            .filter(|r| r.kind == "navigation")
            .count();
        let inputs = h
            .state()
            .demo
            .host
            .receipts
            .iter()
            .filter(|r| r.kind == "input")
            .count();
        h.hover_at(to_egui(content_bounds).center());
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -4.0),
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(3);
        assert_eq!(
            h.state()
                .demo
                .host
                .receipts
                .iter()
                .filter(|r| r.kind == "navigation")
                .count(),
            receipt_count
        );
        assert!(
            h.state()
                .demo
                .host
                .receipts
                .iter()
                .filter(|r| r.kind == "input")
                .count()
                > inputs
        );
    }
}

#[test]
fn e003_actual_drag_preview_forbidden_cancel_and_valid_release() {
    let mut h = ui_proof(3, false);
    let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
    {
        let state = h.state_mut();
        let workspace = state.demo.root;
        let mut plan = state.demo.controller.snapshot().layouts[&workspace].clone();
        let core::LayoutNode::Grid { rows, .. } = plan.nodes.get_mut(&plan.root.unwrap()).unwrap()
        else {
            panic!("demo grid");
        };
        // A vacant second row leaves room for an eligible viewport-contained float.
        rows.push(core::Scalar::new(1.0).unwrap());
        command(&mut state.demo, Command::SetLayout { workspace, plan });
    }
    h.run_steps(3);
    let point = to_egui(
        h.state()
            .report
            .controls
            .iter()
            .find(|c| c.id == format!("frame/{}/drag", frame.raw()))
            .unwrap()
            .bounds,
    )
    .center();
    let outside = point + egui::vec2(80.0, 90.0);
    let snapshot = h.state().demo.controller.snapshot();
    let bounds = snapshot.geometry(h.state().demo.root).unwrap().frames[&frame];
    let delta = outside - point;
    let floated = core::Rect::new(
        bounds.x() + delta.x as f64,
        bounds.y() + delta.y as f64,
        bounds.width(),
        bounds.height(),
    )
    .unwrap();
    assert!(snapshot.contexts[&h.state().demo.root]
        .viewport
        .contains(floated));
    for workspace in snapshot.contexts.keys() {
        assert!(snapshot
            .snap_candidates(
                *workspace,
                core::Scalar::new(outside.x as f64).unwrap(),
                core::Scalar::new(outside.y as f64).unwrap(),
                core::Scalar::new(24.0).unwrap()
            )
            .unwrap()
            .is_empty());
    }
    let before = h.state().demo.controller.snapshot().layouts.clone();
    h.hover_at(point);
    h.drag_at(point);
    h.run_steps(1);
    h.hover_at(outside);
    h.run_steps(1);
    assert!(h
        .state()
        .adapter
        .input
        .active_capture()
        .is_some_and(|c| c.activated));
    assert_eq!(h.state().demo.controller.snapshot().layouts, before);
    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    h.drop_at(outside);
    h.run_steps(3);
    assert!(h.state().adapter.input.active_capture().is_none());
    assert!(!h
        .state()
        .demo
        .controller
        .snapshot()
        .floats
        .contains_key(&frame));
    {
        let state = h.state_mut();
        let mut context = state.demo.controller.snapshot().contexts[&state.demo.root].clone();
        context.allow_floating = false;
        let workspace = state.demo.root;
        command(
            &mut state.demo,
            Command::SetGeometryContext { workspace, context },
        );
    }
    h.run_steps(3);
    h.hover_at(point);
    h.drag_at(point);
    h.run_steps(1);
    h.hover_at(outside);
    h.run_steps(1);
    assert!(!h.state().report.rejected.is_empty());
    h.drop_at(outside);
    h.run_steps(3);
    assert!(!h
        .state()
        .demo
        .controller
        .snapshot()
        .floats
        .contains_key(&frame));
    {
        let state = h.state_mut();
        let mut context = state.demo.controller.snapshot().contexts[&state.demo.root].clone();
        context.allow_floating = true;
        let workspace = state.demo.root;
        command(
            &mut state.demo,
            Command::SetGeometryContext { workspace, context },
        );
    }
    h.run_steps(3);
    h.hover_at(point);
    h.drag_at(point);
    h.run_steps(1);
    h.hover_at(outside);
    h.run_steps(1);
    h.drop_at(outside);
    h.run_steps(3);
    assert!(h
        .state()
        .demo
        .controller
        .snapshot()
        .floats
        .contains_key(&frame));
    assert_eq!(h.state().adapter.input.last_settlement, Some("release"));
}

#[test]
fn e007_touch_long_press_and_host_pen_resize_commit_and_cancel() {
    for source in [input::PointerSource::Touch, input::PointerSource::HostPen] {
        let mut h = ui_proof(3, false);
        let control = h
            .state()
            .report
            .controls
            .iter()
            .find(|c| c.id.starts_with("resize/"))
            .unwrap()
            .clone();
        let point = to_egui(control.bounds).center();
        let moved = point
            + if control.id.split('/').nth(3) == Some("x") {
                egui::vec2(20.0, 0.0)
            } else {
                egui::vec2(0.0, 20.0)
            };
        ui_pointer(&mut h, 91, source, input::PointerPhase::Press, point);
        if source == input::PointerSource::Touch {
            h.run_steps(45);
        }
        assert!(h
            .state()
            .adapter
            .input
            .active_capture()
            .is_some_and(|c| c.activated && c.source == source));
        let before = h.state().demo.controller.snapshot().layouts.clone();
        ui_pointer(&mut h, 91, source, input::PointerPhase::Move, moved);
        assert_eq!(h.state().demo.controller.snapshot().layouts, before);
        ui_pointer(&mut h, 92, source, input::PointerPhase::Cancel, moved);
        assert!(h.state().adapter.input.active_capture().is_some());
        ui_pointer(&mut h, 91, source, input::PointerPhase::Release, moved);
        assert_ne!(h.state().demo.controller.snapshot().layouts, before);
        assert_eq!(h.state().adapter.input.last_settlement, Some("release"));
        let point = to_egui(
            h.state()
                .report
                .controls
                .iter()
                .find(|c| c.id.starts_with("resize/"))
                .unwrap()
                .bounds,
        )
        .center();
        ui_pointer(&mut h, 93, source, input::PointerPhase::Press, point);
        ui_pointer(&mut h, 93, source, input::PointerPhase::Cancel, point);
        assert!(h.state().adapter.input.active_capture().is_none());
    }
}

#[test]
fn e008_keyboard_resize_focus_uses_actual_arrangement_response() {
    let mut h = ui_proof(3, false);
    let control = h
        .state()
        .report
        .controls
        .iter()
        .find(|c| c.id.starts_with("resize/"))
        .unwrap()
        .clone();
    let workspace: u64 = control.id.split('/').nth(1).unwrap().parse().unwrap();
    let index: usize = control.id.split('/').nth(4).unwrap().parse().unwrap();
    let label = format!("Resize {workspace} tracks {} and {}", index + 1, index + 2);
    h.get_all_by_label(&label).next().unwrap().focus();
    h.run_steps(3);
    assert!(h.get_all_by_label(&label).next().unwrap().is_focused());
    let before = h.state().demo.controller.snapshot().layouts.clone();
    h.key_press(if control.id.split('/').nth(3) == Some("x") {
        egui::Key::ArrowRight
    } else {
        egui::Key::ArrowDown
    });
    h.run_steps(3);
    assert_ne!(h.state().demo.controller.snapshot().layouts, before);
}

#[test]
fn e008_disabled_native_control_explains_explicit_host_capability() {
    let mut h = ui_proof(3, true);
    h.state_mut().native_enabled = false;
    h.run_steps(3);
    ui_click(&mut h, "Frame");
    let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
    let button = h
        .state()
        .report
        .controls
        .iter()
        .find(|c| c.id == format!("frame/{}/native-popout", frame.raw()))
        .unwrap();
    assert!(!button.enabled);
    assert!(h
        .state()
        .report
        .controls
        .iter()
        .any(|c| c.label == "Native popout unavailable: host capability disabled"));
    h.get_by_label("Native popout frame").click_accesskit();
    h.run_steps(3);
    assert!(h.state().demo.host.native_actions.is_empty());
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    let view = h.state().demo.text_view;
    ui_tab_actions(&mut h, view);
    assert!(h
        .state()
        .report
        .controls
        .iter()
        .any(|c| c.id == format!("view/{}/native-popout", view.raw()) && !c.enabled));
    assert!(h
        .state()
        .report
        .controls
        .iter()
        .any(|c| c.label == "Native popout unavailable: host capability disabled"));
}

fn ui_tab_enter(h: &mut egui_kittest::Harness<'_, UiProof>, control_id: &str) {
    if control_id.starts_with("frame/")
        && (control_id.ends_with("/menu") || control_id.ends_with("/tabs-menu"))
    {
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
    }
    for _ in 0..256 {
        let target = h
            .state()
            .report
            .controls
            .iter()
            .find(|c| c.id == control_id && c.enabled)
            .unwrap_or_else(|| {
                panic!(
                    "current keyboard target {control_id}; controls: {:?}; focus: {:?}; popup: {}",
                    h.state()
                        .report
                        .controls
                        .iter()
                        .map(|c| (&c.id, c.enabled))
                        .collect::<Vec<_>>(),
                    h.ctx.memory(|m| m.focused()),
                    egui::Popup::is_any_open(&h.ctx)
                )
            })
            .bounds;
        let focused = h
            .ctx
            .memory(|m| m.focused())
            .and_then(|id| h.ctx.read_response(id));
        if focused.is_some_and(|response| response.enabled() && response.rect == to_egui(target)) {
            h.key_press(egui::Key::Enter);
            h.run_steps(3);
            return;
        }
        h.key_press(egui::Key::Tab);
        h.run_steps(1);
    }
    panic!("Tab traversal did not focus {control_id}");
}

#[test]
fn e008_actual_tab_enter_group_float_redock_and_close_routes() {
    let mut h = ui_proof(3, false);
    let frame = h.state().demo.controller.snapshot().views[&h.state().demo.text_view].frame;
    let group = {
        let state = h.state_mut();
        let core::Change::Created(EntityId::Group(group)) = command(
            &mut state.demo,
            Command::CreateGroup {
                frame,
                label: "Keyboard group".into(),
                color: None,
            },
        ) else {
            panic!("group");
        };
        let view = state.demo.text_view;
        command(
            &mut state.demo,
            Command::AssignGroup {
                view,
                group: Some(group),
            },
        );
        group
    };
    h.run_steps(3);
    ui_tab_enter(&mut h, &format!("frame/{}/tabs-menu", frame.raw()));
    ui_tab_enter(&mut h, &format!("group/{}/menu", group.raw()));
    ui_tab_enter(&mut h, &format!("group/{}/collapse", group.raw()));
    assert!(h.state().demo.controller.snapshot().groups[&group].collapsed);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    ui_tab_enter(&mut h, &format!("frame/{}/menu", frame.raw()));
    ui_tab_enter(
        &mut h,
        &format!("frame/{}/preview-float-redock", frame.raw()),
    );
    assert!(h.state().adapter.chrome.preview.is_some());
    ui_tab_enter(&mut h, "workspace/preview/commit");
    assert!(h
        .state()
        .demo
        .controller
        .snapshot()
        .floats
        .contains_key(&frame));
    ui_tab_enter(&mut h, &format!("frame/{}/menu", frame.raw()));
    let popup_control = h
        .state()
        .report
        .controls
        .iter()
        .find(|control| control.id == format!("frame/{}/Equalize-workspace", frame.raw()))
        .expect("floating menu actually projected");
    let point = to_egui(popup_control.bounds).center();
    assert!(
        h.state()
            .report
            .slots
            .iter()
            .any(|slot| slot.frame == frame && to_egui(slot.clip).contains(point)),
        "probe overlaps floating provider"
    );
    let winner = h.ctx.layer_id_at(point).expect("floating popup hit layer");
    assert_eq!(
        winner.order,
        egui::Order::Foreground,
        "menu wins over floating provider"
    );
    let state = h.state_mut();
    let projection = workspace::project_with_chrome(
        &mut state.demo.controller,
        state.binding,
        &mut state.demo.host,
        &state.adapter.chrome,
    )
    .unwrap();
    let pane = projection
        .frames
        .iter()
        .find(|pane| pane.frame == frame)
        .unwrap();
    assert_eq!(pane.layer().order, egui::Order::Middle);
    assert_ne!(winner, pane.layer());
    ui_tab_enter(
        &mut h,
        &format!("frame/{}/preview-float-redock", frame.raw()),
    );
    assert_preview_controls_fit(&h);
    ui_tab_enter(&mut h, "workspace/preview/commit");
    assert!(!h
        .state()
        .demo
        .controller
        .snapshot()
        .floats
        .contains_key(&frame));
    let view = h.state().demo.controller.snapshot().frames[&frame].views[2];
    {
        let state = h.state_mut();
        let record = state.demo.controller.snapshot().views[&view].clone();
        command(
            &mut state.demo,
            Command::SetViewMetadata {
                view,
                label: record.label,
                pinned: false,
                unsaved: false,
            },
        );
    }
    h.run_steps(3);
    ui_tab_enter(&mut h, &format!("frame/{}/tabs-menu", frame.raw()));
    ui_tab_enter(&mut h, &format!("view/{}/actions-menu", view.raw()));
    ui_tab_enter(&mut h, &format!("view/{}/close", view.raw()));
    assert!(h.state().demo.controller.snapshot().views[&view]
        .closed
        .is_some());
}
