#[cfg(feature = "capture")]
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use kumihimo_core::{Command, Rect};
    let mut coverage = Vec::new();
    use kumihimo_egui::{
        capture::OffscreenRenderer, demo, ChromeProfile, DesktopAdapter, DesktopHost,
        SurfaceBinding,
    };
    let mut args = std::env::args().skip(1);
    let mut output = std::path::PathBuf::from(".local/desktop-proof");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().ok_or("--output needs a directory")?.into(),
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    std::fs::create_dir_all(&output)?;
    let mut demo = demo::new()?;
    let mut adapter = DesktopAdapter::default();
    let budget = demo.host.policy.max_capture_bytes;
    let mut renderer = OffscreenRenderer::new(budget)?;
    let mut surface = SurfaceBinding {
        root: demo.root,
        viewport: egui::ViewportId::ROOT,
        epoch: 1,
        bounds: Rect::new(0.0, 0.0, 960.0, 640.0)?,
        pixels_per_point: 1.0,
    };
    // A warm draw lets immediate-mode hit rectangles settle; both draws use shipped code.
    renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    let mut capture = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    capture.write(&output.join("standard"), budget)?;
    coverage.push(pick_provider_marks(&mut demo, &capture, "standard")?);
    let slot = capture
        .report
        .slots
        .iter()
        .find(|s| s.view == demo.canvas_view)
        .ok_or("canvas slot unavailable")?
        .clone();
    let point = kumihimo_egui::to_egui(slot.clip).center();
    demo.host.content_input(
        &mut demo.controller,
        &slot,
        kumihimo_content::Input {
            owner: kumihimo_content::InputOwner::Content,
            payload: "injected-capture-canvas-click".into(),
            x: Some(kumihimo_core::Scalar::new(point.x as f64)?),
            y: Some(kumihimo_core::Scalar::new(point.y as f64)?),
        },
    )?;
    demo.host
        .tool_action(&mut demo.controller, demo.tool, "demo-append")?;
    let nested_owner = demo
        .controller
        .snapshot()
        .workspaces
        .values()
        .find(|w| w.owner.is_some())
        .and_then(|w| w.owner)
        .ok_or("nested owner missing")?;
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::Maximize {
                frame: nested_owner,
            },
        )?
        .outcome
        .result?;
    renderer
        .capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?
        .write(&output.join("nested"), budget)?;
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::RestoreLayout {
                workspace: demo.root,
            },
        )?
        .outcome
        .result?;
    demo.host.policy.settings.profile = ChromeProfile::Compact;
    surface.bounds = Rect::new(0.0, 0.0, 460.0, 420.0)?;
    surface.epoch += 1;
    let narrow = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    narrow.write(&output.join("compact-narrow"), budget)?;
    coverage.push(pick_provider_marks(&mut demo, &narrow, "compact-narrow")?);
    demo.host.policy.settings.profile = ChromeProfile::Full;
    surface.bounds = Rect::new(0.0, 0.0, 960.0, 640.0)?;
    surface.pixels_per_point = 1.5;
    surface.epoch += 1;
    let scaled = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    scaled.write(&output.join("injected-scale-150"), budget)?;
    coverage.push(pick_provider_marks(
        &mut demo,
        &scaled,
        "injected-scale-150",
    )?);
    surface.pixels_per_point = 1.0;
    surface.epoch += 1;
    #[cfg(feature = "navigation")]
    {
        use kumihimo_content::NavigationMode;
        use kumihimo_egui::input::{HostPointerEvent, PointerPhase, PointerSource};
        for (mode, name) in [
            (NavigationMode::ScrollbarOnly, "scrollbar"),
            (NavigationMode::MinimapAlways, "minimap-always"),
            (NavigationMode::ScrollbarAndMinimap, "scrollbar-and-minimap"),
            (NavigationMode::ScrubToMinimap, "transient-idle"),
        ] {
            demo.host.navigation_mode(&mut demo.controller, mode)?;
            capture = renderer.capture(
                &mut adapter,
                &mut demo.controller,
                surface,
                &mut demo.host,
                Default::default(),
            )?;
            capture.write(&output.join(name), budget)?;
        }
        let mount = demo.host.mounts[&demo.text_view];
        let scope = demo.controller.inspect().mounts[&mount].scope.clone();
        let projection = demo.controller.navigation_projection(&scope)?;
        let thumb = projection.thumb.ok_or("navigation thumb unavailable")?;
        let rail = capture
            .report
            .controls
            .iter()
            .find(|c| c.id == format!("view/{}/navigation", demo.text_view.raw()))
            .ok_or("navigation picking projection missing")?;
        let position = egui::pos2(
            kumihimo_egui::to_egui(rail.bounds).center().x,
            (thumb.start.get() + thumb.length.get() * 0.5) as f32,
        );
        adapter.input.feed_pointer(HostPointerEvent {
            pointer: 1,
            source: PointerSource::Mouse,
            phase: PointerPhase::Press,
            position,
            epoch: surface.epoch,
        })?;
        let grab = renderer.capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?;
        if !demo.controller.navigation_projection(&scope)?.held_viewport {
            return Err("transient injected pointer did not acquire stationary grab".into());
        }
        let idle_slot = capture
            .report
            .slots
            .iter()
            .find(|s| s.view == demo.text_view)
            .ok_or("idle document slot")?;
        if grab
            .report
            .slots
            .iter()
            .find(|s| s.view == demo.text_view)
            .map(|s| s.bounds)
            != Some(idle_slot.bounds)
        {
            return Err("transient reveal moved document slot".into());
        }
        grab.write(&output.join("transient-stationary-grab"), budget)?;
        adapter.input.feed_pointer(HostPointerEvent {
            pointer: 1,
            source: PointerSource::Mouse,
            phase: PointerPhase::Move,
            position: egui::pos2(position.x, position.y + 90.0),
            epoch: surface.epoch,
        })?;
        renderer
            .capture(
                &mut adapter,
                &mut demo.controller,
                surface,
                &mut demo.host,
                Default::default(),
            )?
            .write(&output.join("transient-moved"), budget)?;
        adapter.input.feed_pointer(HostPointerEvent {
            pointer: 1,
            source: PointerSource::Mouse,
            phase: PointerPhase::Release,
            position,
            epoch: surface.epoch,
        })?;
        renderer
            .capture(
                &mut adapter,
                &mut demo.controller,
                surface,
                &mut demo.host,
                Default::default(),
            )?
            .write(&output.join("transient-released"), budget)?;
    }
    coverage.push(layout_variants(&mut renderer, &output, budget)?);
    coverage.push(demand_variants(&mut renderer, &output, budget)?);
    coverage.push(tab_group_variants(&mut renderer, &output, budget)?);
    coverage.push(dock_variants(&mut renderer, &output, budget)?);
    coverage.push(tool_variants(&mut renderer, &output, budget)?);
    coverage.push(chrome_input_variants(&mut renderer, &output, budget)?);
    std::fs::write(
        output.join("coverage-observations.json"),
        kumihimo_egui::diagnostics::encode_bounded(
            &serde_json::json!({
                "kind":"original-case-render-observations", "observations":coverage,
                "provenance":"actual DX12 readbacks and provider ACKs; layout/scale/availability/delay injected; elapsed time measured; no CPU/memory or native resource-benefit claim"
            }),
            budget,
        )?,
    )?;
    let receipts = serde_json::json!({"kind":"demo-provider-receipts", "provenance":"injected capture interactions; actual scoped provider ACKs", "receipts":demo.host.receipts.iter().map(|r| serde_json::json!({"view":r.view.raw().to_string(),"resource":r.resource.raw().to_string(),"request":r.request.to_string(),"kind":r.kind,"provider_local_point":r.local_point})).collect::<Vec<_>>(), "unobserved":["native presentation", "physical touch/pen", "OS accessibility", "external GPU fault", "real application engines", "video decoder/audio"]});
    std::fs::write(
        output.join("provider-receipts.json"),
        kumihimo_egui::diagnostics::encode_bounded(&receipts, budget)?,
    )?;
    println!("Actual offscreen captures written to {}. Native and physical-device observations remain separate.", output.display());
    Ok(())
}
#[cfg(not(feature = "capture"))]
fn main() {
    eprintln!(
        "capture_host requires the capture feature; add navigation/backgrounds for their controls."
    );
}

#[cfg(feature = "capture")]
fn pick_provider_marks(
    demo: &mut kumihimo_egui::demo::Demo,
    frame: &kumihimo_egui::capture::CapturedFrame,
    name: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use kumihimo_egui::DesktopHost;
    let mut picks = Vec::new();
    for view in [demo.text_view, demo.canvas_view] {
        let slot = frame
            .report
            .slots
            .iter()
            .find(|s| s.view == view)
            .ok_or("provider mark slot missing")?;
        let (point, color) = demo.host.provider_mark(slot)?;
        let x = (point.x * slot.scale).floor() as u32;
        let y = (point.y * slot.scale).floor() as u32;
        let pixel = frame
            .image
            .get_pixel_checked(x, y)
            .ok_or("provider mark outside raster")?
            .0;
        let expected = color.to_array();
        if pixel
            .iter()
            .zip(expected)
            .take(3)
            .any(|(a, b)| a.abs_diff(b) > 8)
        {
            return Err(format!(
                "{name} provider {} mark pixel mismatch: {pixel:?} vs {expected:?}",
                view.raw()
            )
            .into());
        }
        let local = slot.local_point(point)?;
        demo.host.content_input(
            &mut demo.controller,
            slot,
            kumihimo_content::Input {
                owner: kumihimo_content::InputOwner::Content,
                payload: "readback-mark-pick".into(),
                x: Some(kumihimo_core::Scalar::new(point.x as f64)?),
                y: Some(kumihimo_core::Scalar::new(point.y as f64)?),
            },
        )?;
        let receipt = demo.host.receipts.back().ok_or("mark input ACK missing")?;
        if receipt.view != view
            || receipt.resource != slot.resource
            || receipt.kind != "input"
            || receipt
                .local_point
                .is_none_or(|p| (p[0] - local.x).abs() > 0.01 || (p[1] - local.y).abs() > 0.01)
        {
            return Err("provider mark recipient/local transform mismatch".into());
        }
        let outside = egui::pos2(slot.clip.x() as f32 - 1.0, point.y);
        if demo
            .host
            .content_input(
                &mut demo.controller,
                slot,
                kumihimo_content::Input {
                    owner: kumihimo_content::InputOwner::Content,
                    payload: "outside-clip-probe".into(),
                    x: Some(kumihimo_core::Scalar::new(outside.x as f64)?),
                    y: Some(kumihimo_core::Scalar::new(outside.y as f64)?),
                },
            )
            .is_ok()
        {
            return Err("clipped provider input accepted".into());
        }
        picks.push(serde_json::json!({"view":view.raw().to_string(),"resource":slot.resource.raw().to_string(),
            "epoch":slot.epoch,"scale":slot.scale,"physical_pixel":[x,y],"rgba":pixel,
            "global_dip":[point.x,point.y],"provider_local_dip":[local.x,local.y],"outside_clip_rejected":true}));
    }
    Ok(serde_json::json!({"variant":name,"provider_marks":picks}))
}

#[cfg(feature = "capture")]
fn layout_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use kumihimo_core::{Command, EntityId, LayoutItem, LayoutNode, NodeId, Scalar};
    use kumihimo_egui::{demo, DesktopAdapter, DesktopHost, SurfaceBinding};
    let mut demo = demo::with_counts(5, 2)?;
    let canvas_frame = demo.controller.snapshot().views[&demo.canvas_view].frame;
    let nested = demo.controller.snapshot().frames[&canvas_frame].workspace;
    let response = demo
        .host
        .dispatch(
            &mut demo.controller,
            Command::CreateFrame {
                workspace: nested,
                index: 1,
            },
        )?
        .outcome
        .result?;
    let kumihimo_core::Change::Created(EntityId::Frame(owner)) = response else {
        return Err("extra containment frame".into());
    };
    let response = demo
        .host
        .dispatch(
            &mut demo.controller,
            Command::CreateWorkspace { owner: Some(owner) },
        )?
        .outcome
        .result?;
    let kumihimo_core::Change::Created(EntityId::Workspace(deep)) = response else {
        return Err("extra containment workspace".into());
    };
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::MoveFrame {
                frame: canvas_frame,
                destination: deep,
                index: 0,
            },
        )?
        .outcome
        .result?;
    let frames = demo.controller.snapshot().workspaces[&demo.root]
        .frames
        .iter()
        .copied()
        .filter(|f| !demo.controller.snapshot().floats.contains_key(f))
        .collect::<Vec<_>>();
    let mut nodes = std::collections::BTreeMap::new();
    let mut cells = Vec::new();
    for (i, frame) in frames.into_iter().enumerate() {
        let node = NodeId(i as u64 + 2);
        nodes.insert(node, LayoutNode::Leaf(LayoutItem::Frame(frame)));
        cells.push(kumihimo_core::Cell {
            row: i / 3,
            column: i % 3,
            node,
        });
    }
    nodes.insert(
        NodeId(1),
        LayoutNode::Grid {
            rows: vec![Scalar::new(2.0)?, Scalar::new(1.0)?],
            columns: vec![Scalar::new(2.0)?, Scalar::new(3.0)?, Scalar::new(1.0)?],
            cells,
        },
    );
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::SetLayout {
                workspace: demo.root,
                plan: kumihimo_core::LayoutPlan {
                    root: Some(NodeId(1)),
                    nodes,
                },
            },
        )?
        .outcome
        .result?;
    let mut adapter = DesktopAdapter::default();
    let mut surface = SurfaceBinding {
        root: demo.root,
        viewport: egui::ViewportId::ROOT,
        epoch: 100,
        bounds: kumihimo_core::Rect::new(0.0, 0.0, 960.0, 640.0)?,
        pixels_per_point: 1.0,
    };
    let mut observations = Vec::new();
    for (name, w, h) in [
        ("weighted-holes-deep", 960.0, 640.0),
        ("weighted-holes-shrink", 520.0, 440.0),
        ("weighted-holes-restored", 960.0, 640.0),
    ] {
        surface.bounds = kumihimo_core::Rect::new(0.0, 0.0, w, h)?;
        surface.epoch += 1;
        renderer.capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?;
        let frame = renderer.capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?;
        frame.write(&output.join(name), budget)?;
        if frame
            .report
            .slots
            .iter()
            .all(|s| s.view != demo.canvas_view)
        {
            return Err("deep canvas not rendered".into());
        }
        observations.push(serde_json::json!({"variant":name,"canonical_views":demo.controller.snapshot().views.len(),
            "containment_workspaces":demo.controller.snapshot().workspaces.len(),"painted_views":frame.report.painted_views,
            "row_weights":[2,1],"column_weights":[2,3,1],"empty_cells":2}));
    }
    #[cfg(feature="backgrounds")]
    let backgrounds=demo.controller.inspect().backgrounds.values().filter_map(|b| b.settings.effective.as_ref())
        .map(|s|serde_json::json!({"brightness":s.brightness.value.get(),"opacity":s.opacity.value.get(),
            "overlay_opacity":s.overlay_opacity.value.get(),"fit":format!("{:?}",s.fit.value)})).collect::<Vec<_>>();
    #[cfg(not(feature = "backgrounds"))]
    let backgrounds = Vec::<serde_json::Value>::new();
    Ok(
        serde_json::json!({"case":"001","variants":observations,"actual_still_background_settings":backgrounds}),
    )
}

#[cfg(feature = "capture")]
fn demand_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use kumihimo_egui::{demo, DesktopAdapter, DesktopHost, SurfaceBinding};
    let mut demo = demo::with_counts(2, 1)?;
    let mut adapter = DesktopAdapter::default();
    let mut surface = SurfaceBinding {
        root: demo.root,
        viewport: egui::ViewportId::ROOT,
        epoch: 200,
        bounds: kumihimo_core::Rect::new(0.0, 0.0, 960.0, 640.0)?,
        pixels_per_point: 1.0,
    };
    let start = std::time::Instant::now();
    let mut observations = Vec::new();
    let mut last = None;
    for animated in [false, true] {
        demo.host.animate_provider(demo.canvas_view, animated)?;
        for index in 0..4 {
            let elapsed = start.elapsed();
            demo.host.now_ms = elapsed.as_millis() as u64;
            let frame = renderer.capture(
                &mut adapter,
                &mut demo.controller,
                surface,
                &mut demo.host,
                egui::RawInput {
                    time: Some(elapsed.as_secs_f64()),
                    ..Default::default()
                },
            )?;
            let animation_mark = if animated {
                let slot = frame
                    .report
                    .slots
                    .iter()
                    .find(|s| s.view == demo.canvas_view)
                    .ok_or("animated slot")?;
                let (_, color) = demo.host.provider_mark(slot)?;
                let width = (slot.bounds.width() - 32.0).max(1.0) as f32;
                let point = egui::pos2(
                    slot.bounds.x() as f32
                        + 16.0
                        + ((elapsed.as_secs_f64() * 80.0) as f32).rem_euclid(width),
                    (slot.bounds.y() + slot.bounds.height() - 16.0) as f32,
                );
                let pixel = frame
                    .image
                    .get_pixel_checked(
                        (point.x * slot.scale).floor() as u32,
                        (point.y * slot.scale).floor() as u32,
                    )
                    .ok_or("animated mark raster")?
                    .0;
                if pixel
                    .iter()
                    .zip(color.to_array())
                    .take(3)
                    .any(|(a, b)| a.abs_diff(b) > 8)
                {
                    return Err(
                        "animated provider mark not observed at current timeline position".into(),
                    );
                }
                Some(serde_json::json!({"global_dip":[point.x,point.y],"rgba":pixel}))
            } else {
                None
            };
            observations.push(
                serde_json::json!({"variant":if animated {"animated-demand"}else{"static-idle"},
                "frame":index,"measured_elapsed_ms":elapsed.as_secs_f64()*1000.0,
                "readback_nonce":frame.nonce,"render_ms":frame.render_ms,"animated_mark_readback":animation_mark,
                "egui_repaint_after_ms":frame.repaint_after.as_secs_f64()*1000.0}),
            );
            frame.write(
                &output.join(format!(
                    "{}-{index}",
                    if animated {
                        "animated-demand"
                    } else {
                        "static-idle"
                    }
                )),
                budget,
            )?;
            last = Some(frame);
        }
    }
    demo.host.animate_provider(demo.canvas_view, false)?;
    let slot = last
        .as_ref()
        .unwrap()
        .report
        .slots
        .iter()
        .find(|s| s.view == demo.canvas_view)
        .ok_or("demand canvas slot")?
        .clone();
    demo.host.set_surface_observation(
        &mut demo.controller,
        surface,
        false,
        true,
        kumihimo_content::Observation::Unknown,
    )?;
    let stale = demo
        .host
        .content_input(
            &mut demo.controller,
            &slot,
            kumihimo_content::Input {
                owner: kumihimo_content::InputOwner::Content,
                payload: "unavailable-surface-probe".into(),
                x: None,
                y: None,
            },
        )
        .is_err();
    if !stale {
        return Err("unavailable surface accepted old input".into());
    }
    let mount = demo.host.mounts[&demo.canvas_view];
    let signals = demo.controller.inspect().mounts[&mount].signals.clone();
    if signals.visible || !signals.minimized {
        return Err("injected surface facts overwritten".into());
    }
    let hidden = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    hidden.write(&output.join("injected-unavailable-minimized"), budget)?;
    if demo.controller.inspect().mounts[&mount].signals.visible
        || !demo.controller.inspect().mounts[&mount].signals.minimized
    {
        return Err("draw overwrote unavailable surface observations".into());
    }
    let mut zero = surface;
    zero.bounds = kumihimo_core::Rect::new(0.0, 0.0, 0.0, 0.0)?;
    if renderer
        .capture(
            &mut adapter,
            &mut demo.controller,
            zero,
            &mut demo.host,
            Default::default(),
        )
        .is_ok()
    {
        return Err("zero raster admitted".into());
    }
    observations.push(serde_json::json!({"variant":"injected-unavailable-minimized-zero","input_rejected":stale,"zero_raster_rejected":true,
        "canonical_visible":signals.visible,"canonical_minimized":signals.minimized,"occlusion":"Unknown"}));
    surface.epoch += 1;
    demo.host.set_surface_observation(
        &mut demo.controller,
        surface,
        true,
        false,
        kumihimo_content::Observation::Unknown,
    )?;
    let recovered = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    recovered.write(&output.join("availability-recovered"), budget)?;
    observations.push(pick_provider_marks(
        &mut demo,
        &recovered,
        "availability-recovered",
    )?);
    demo.host.delay_provider(demo.canvas_view, true)?;
    let previous_limits = demo.controller.inspect().limits.clone();
    let mut limits = previous_limits.clone();
    limits.pending = Some(1);
    limits.queue = Some(1);
    demo.controller.set_limits(limits)?;
    let permission = kumihimo_content::HostPermission {
        allowed: true,
        eligibility_revision: demo.controller.inspect().mounts[&mount]
            .eligibility
            .revision,
    };
    let request = demo.controller.request(
        mount,
        kumihimo_content::RequestKind::Input(kumihimo_content::Input {
            owner: kumihimo_content::InputOwner::Content,
            payload: "delayed-provider-input".into(),
            x: None,
            y: None,
        }),
        permission,
        demo.host.now_ms,
    )?;
    let overloaded = demo
        .controller
        .request(
            mount,
            kumihimo_content::RequestKind::Input(kumihimo_content::Input {
                owner: kumihimo_content::InputOwner::Content,
                payload: "overload-probe".into(),
                x: None,
                y: None,
            }),
            permission,
            demo.host.now_ms,
        )
        .is_err();
    if !overloaded {
        return Err("provider overload did not reject".into());
    }
    let delayed = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    delayed.write(&output.join("delayed-provider-overload"), budget)?;
    let other_slot = delayed
        .report
        .slots
        .iter()
        .find(|s| s.view == demo.text_view)
        .ok_or("other provider slot during delay")?;
    let (other_point, other_color) = demo.host.provider_mark(other_slot)?;
    let other_pixel = delayed
        .image
        .get_pixel_checked(
            (other_point.x * other_slot.scale).floor() as u32,
            (other_point.y * other_slot.scale).floor() as u32,
        )
        .ok_or("other provider mark raster")?
        .0;
    if other_pixel
        .iter()
        .zip(other_color.to_array())
        .take(3)
        .any(|(a, b)| a.abs_diff(b) > 8)
    {
        return Err("other provider did not render while canvas delayed".into());
    }
    let pending = demo
        .controller
        .inspect()
        .pending
        .get(&request)
        .ok_or("delayed request lost")?
        .status;
    demo.controller.cancel(request, demo.host.now_ms)?;
    let cancelled =
        demo.controller.inspect().receipts.iter().any(|r| {
            r.ticket.id == request && r.status == kumihimo_content::RequestStatus::Cancelled
        });
    if !cancelled {
        return Err("overload cancellation receipt missing".into());
    }
    demo.controller.set_limits(previous_limits)?;
    demo.host.delay_provider(demo.canvas_view, false)?;
    demo.host.pump(&mut demo.controller, mount)?;
    let frame = renderer.capture(
        &mut adapter,
        &mut demo.controller,
        surface,
        &mut demo.host,
        Default::default(),
    )?;
    frame.write(&output.join("delayed-provider-recovered"), budget)?;
    observations.push(pick_provider_marks(
        &mut demo,
        &frame,
        "delayed-provider-recovered",
    )?);
    observations.push(serde_json::json!({"variant":"delayed-overload-cancel","request":request.0.to_string(),"pending_status":format!("{pending:?}"),
        "overload_rejected":overloaded,"cancelled_receipt":cancelled,"delayed_readback_nonce":delayed.nonce,
        "other_provider_readback_rgba":other_pixel}));
    Ok(serde_json::json!({"case":"010/013","variants":observations}))
}

#[cfg(feature = "capture")]
type SceneResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Shares the shipped renderer; events are injected toolkit input, never physical input.
#[cfg(feature = "capture")]
struct CaptureScene<'a> {
    renderer: &'a mut kumihimo_egui::capture::OffscreenRenderer,
    demo: kumihimo_egui::demo::Demo,
    adapter: kumihimo_egui::DesktopAdapter,
    surface: kumihimo_egui::SurfaceBinding,
    frame: kumihimo_egui::capture::CapturedFrame,
    output: &'a std::path::Path,
    budget: usize,
    observations: Vec<serde_json::Value>,
    time: f64,
}

#[cfg(feature = "capture")]
impl<'a> CaptureScene<'a> {
    fn new(
        renderer: &'a mut kumihimo_egui::capture::OffscreenRenderer,
        output: &'a std::path::Path,
        budget: usize,
        tabs: usize,
        epoch: u64,
    ) -> SceneResult<Self> {
        let mut demo = kumihimo_egui::demo::with_counts(2, tabs)?;
        let mut adapter = kumihimo_egui::DesktopAdapter::default();
        let surface = kumihimo_egui::SurfaceBinding {
            root: demo.root,
            viewport: egui::ViewportId::ROOT,
            epoch,
            bounds: kumihimo_core::Rect::new(0.0, 0.0, 960.0, 640.0)?,
            pixels_per_point: 1.0,
        };
        // Clear previous scene pointer/menu/focus state without changing the renderer.
        renderer.context.memory_mut(|m| *m = Default::default());
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerGone,
                egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            time: Some(epoch as f64),
            ..Default::default()
        };
        renderer.capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            input,
        )?;
        let frame = renderer.capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?;
        Ok(Self {
            renderer,
            demo,
            adapter,
            surface,
            frame,
            output,
            budget,
            observations: vec![],
            time: epoch as f64,
        })
    }
    fn command(&mut self, command: kumihimo_core::Command) -> SceneResult<kumihimo_core::Change> {
        use kumihimo_egui::DesktopHost;
        Ok(self
            .demo
            .host
            .dispatch(&mut self.demo.controller, command)?
            .outcome
            .result?)
    }
    fn input(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> SceneResult<()> {
        self.time += 0.05;
        self.frame = self.renderer.capture(
            &mut self.adapter,
            &mut self.demo.controller,
            self.surface,
            &mut self.demo.host,
            egui::RawInput {
                events,
                modifiers,
                time: Some(self.time),
                ..Default::default()
            },
        )?;
        if !self.frame.report.rejected.is_empty() {
            self.observations.push(serde_json::json!({"variant":"input-rejection","nonce":self.frame.nonce,
                "canonical_root":self.demo.root.raw().to_string(),"epoch":self.surface.epoch,
                "generation":self.demo.controller.snapshot().generation,"cause":self.frame.report.rejected}));
        }
        Ok(())
    }
    fn settle(&mut self) -> SceneResult<()> {
        for _ in 0..3 {
            self.input(vec![], egui::Modifiers::NONE)?;
        }
        Ok(())
    }
    fn point(&self, id: &str) -> SceneResult<egui::Pos2> {
        self.frame
            .report
            .controls
            .iter()
            .rev()
            .find(|c| c.id == id && c.enabled)
            .map(|c| kumihimo_egui::to_egui(c.bounds).center())
            .ok_or_else(|| format!("capture control missing: {id}").into())
    }
    fn mouse(&mut self, point: egui::Pos2, pressed: bool) -> SceneResult<()> {
        self.input(
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            egui::Modifiers::NONE,
        )
    }
    fn click_point(&mut self, point: egui::Pos2) -> SceneResult<()> {
        self.input(
            vec![egui::Event::PointerMoved(point)],
            egui::Modifiers::NONE,
        )?;
        self.mouse(point, true)?;
        self.mouse(point, false)?;
        self.settle()
    }
    fn click_id(&mut self, id: &str) -> SceneResult<()> {
        self.click_point(self.point(id)?)
    }
    fn click(&mut self, label: &str) -> SceneResult<()> {
        let point = self
            .frame
            .report
            .controls
            .iter()
            .rev()
            .find(|c| c.label == label && c.enabled)
            .map(|c| kumihimo_egui::to_egui(c.bounds).center())
            .ok_or_else(|| {
                format!(
                    "capture label missing: {label}; available {:?}",
                    self.frame
                        .report
                        .controls
                        .iter()
                        .map(|c| &c.label)
                        .collect::<Vec<_>>()
                )
            })?;
        self.click_point(point)
    }
    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> SceneResult<()> {
        for pressed in [true, false] {
            self.input(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                }],
                modifiers,
            )?;
        }
        self.settle()
    }
    fn keyboard_activate(&mut self, id: &str) -> SceneResult<()> {
        for _ in 0..96 {
            let bounds = self
                .frame
                .report
                .controls
                .iter()
                .rev()
                .find(|c| c.id == id && c.enabled)
                .map(|c| kumihimo_egui::to_egui(c.bounds))
                .ok_or_else(|| format!("keyboard control missing: {id}"))?;
            let focused = self.renderer.context.memory(|m| m.focused());
            if focused
                .and_then(|i| self.renderer.context.read_response(i))
                .is_some_and(|r| r.rect == bounds)
            {
                self.observations.push(serde_json::json!({"variant":"keyboard-control-focused","control":id,
                    "root":self.demo.root.raw().to_string(),"epoch":self.surface.epoch,"generation":self.demo.controller.snapshot().generation,
                    "nonce":self.frame.nonce,"focus":format!("{focused:?}")}));
                return self.key(egui::Key::Enter, egui::Modifiers::NONE);
            }
            for pressed in [true, false] {
                self.input(
                    vec![egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    egui::Modifiers::NONE,
                )?;
            }
        }
        Err(format!("keyboard Tab route cannot reach {id}").into())
    }
    fn checkpoint(&mut self, name: &str) -> SceneResult<()> {
        self.frame.write(&self.output.join(name), self.budget)?;
        let snapshot = self.demo.controller.snapshot();
        self.observations.push(serde_json::json!({
            "variant":name, "nonce":self.frame.nonce, "epoch":self.surface.epoch,
            "canonical_root":self.demo.root.raw().to_string(),"fixture_epoch":self.surface.epoch,
            "canonical_identity":format!("{:?}",snapshot.identity),
            "generation":snapshot.generation, "canonical_views":snapshot.views.len(),
            "painted_views":self.frame.report.painted_views, "projected_controls":self.frame.report.controls.len(),
            "focused_view":self.adapter.input.focused_view.map(|v|v.raw().to_string()),
            "egui_focus":format!("{:?}",self.renderer.context.memory(|m|m.focused())),
            "gesture_capture":format!("{:?}",self.adapter.input.active_capture()),
            "gesture_settlement":self.adapter.input.last_settlement,
            "rejected":self.frame.report.rejected,
            "frames":snapshot.frames.values().map(|f|serde_json::json!({
                "frame":f.id.raw().to_string(),"workspace":f.workspace.raw().to_string(),
                "members":f.views.iter().map(|v|v.raw().to_string()).collect::<Vec<_>>(),
                "selected":f.selected_view.map(|v|v.raw().to_string()),
                "groups":f.groups.iter().map(|g|g.raw().to_string()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "groups":snapshot.groups.values().map(|g|serde_json::json!({
                "group":g.id.raw().to_string(),"frame":g.frame.raw().to_string(),"label":g.label,
                "collapsed":g.collapsed,"color":g.color
            })).collect::<Vec<_>>(),
            "views":snapshot.views.values().map(|v|serde_json::json!({
                "view":v.id.raw().to_string(),"resource":v.resource.raw().to_string(),"frame":v.frame.raw().to_string(),
                "group":v.group.map(|g|g.raw().to_string()),"pinned":v.pinned,"unsaved":v.unsaved,"closed":v.closed.is_some(),
                "state":format!("{:?}",v.state)
            })).collect::<Vec<_>>(),
            "effective_root_settings":self.adapter.chrome.resolve(&self.demo.host.policy,self.demo.root,self.surface.viewport)
                .map(|(settings,source,recreation)|serde_json::json!({"profile":format!("{:?}",settings.profile),
                    "header_height":settings.header_height,"group_colors":settings.group_colors,"frame_shortcuts_first":settings.frame_shortcuts_first,
                    "source":source,"recreation_needed":recreation})).ok(),
            "tools":snapshot.tools.values().map(|t|serde_json::json!({
                "tool":t.id.raw().to_string(),"target_view":t.target.view.raw().to_string(),
                "target_resource":t.target.resource.raw().to_string(),"placement":format!("{:?}",t.placement),
                "orientation":format!("{:?}",t.policy.orientation),
                "presentation":format!("{:?}",snapshot.effective_tool_presentation(t))
            })).collect::<Vec<_>>(),
            "provider_acks":self.demo.host.receipts.iter().map(|r|serde_json::json!({
                "view":r.view.raw().to_string(),"resource":r.resource.raw().to_string(),
                "kind":r.kind,"request":r.request.to_string(),"close_approved":r.close_approved
            })).collect::<Vec<_>>()
        }));
        Ok(())
    }
    fn tabs(&mut self, frame: kumihimo_core::FrameId) -> SceneResult<()> {
        self.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        self.click_id(&format!("frame/{}/tabs-menu", frame.raw()))
    }
    fn tab_actions(&mut self, view: kumihimo_core::ViewId) -> SceneResult<()> {
        let frame = self.demo.controller.snapshot().views[&view].frame;
        let index = self.demo.controller.snapshot().frames[&frame]
            .views
            .iter()
            .position(|v| *v == view)
            .ok_or("tab member missing")?;
        // Host-configured page is setup; separate scene below exercises actual page buttons.
        self.adapter
            .chrome
            .pages
            .insert(format!("tabs/{}", frame.raw()), index);
        self.tabs(frame)?;
        self.click_id(&format!("view/{}/actions-menu", view.raw()))
    }
    fn frame_action(&mut self, frame: kumihimo_core::FrameId, label: &str) -> SceneResult<()> {
        self.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        self.click_id(&format!("frame/{}/menu", frame.raw()))?;
        self.click(label)
    }
    fn finish(self, case: &str) -> serde_json::Value {
        serde_json::json!({"case":case,"provenance":"injected egui RawInput events; actual shared DesktopAdapter DX12 readbacks; host-dispatched Core setup/commands separately labelled", "variants":self.observations})
    }
}

#[cfg(feature = "capture")]
fn tab_group_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> SceneResult<serde_json::Value> {
    use kumihimo_core::{Change, Command, EntityId};
    let mut s = CaptureScene::new(renderer, output, budget, 8, 300)?;
    let frame = s.demo.controller.snapshot().views[&s.demo.text_view].frame;
    let original = s.demo.controller.snapshot().frames[&frame].views.clone();
    let destination = s.demo.controller.snapshot().views[&s.demo.canvas_view].frame;
    s.demo.host.policy.tab_page_size = 1;
    s.command(Command::Maximize { frame })?;
    let Change::Created(EntityId::Group(group)) = s.command(Command::CreateGroup {
        frame,
        label: "Proof group".into(),
        color: Some("#ee2211".into()),
    })?
    else {
        return Err("capture group".into());
    };
    let Change::Created(EntityId::Group(second)) = s.command(Command::CreateGroup {
        frame,
        label: "Second group".into(),
        color: Some("#11aaee".into()),
    })?
    else {
        return Err("capture second group".into());
    };
    for view in [original[0], original[7]] {
        s.command(Command::AssignGroup {
            view,
            group: Some(group),
        })?;
    }
    s.command(Command::AssignGroup {
        view: original[4],
        group: Some(second),
    })?;
    s.settle()?;
    s.checkpoint("tabs-groups-colored-small-page")?;
    s.tabs(frame)?;
    for _ in 0..7 {
        s.click_id(&format!("page/tabs/{}/next", frame.raw()))?;
    }
    s.click_id(&format!("view/{}/tab", original[7].raw()))?;
    if s.demo.controller.snapshot().frames[&frame].selected_view != Some(original[7]) {
        return Err("overflow last canonical tab not selected".into());
    }
    s.checkpoint("tabs-overflow-last-selected")?;
    s.tab_actions(original[7])?;
    s.click("Move tab first")?;
    if s.demo.controller.snapshot().frames[&frame].views[0] != original[7] {
        return Err("widget tab reorder missing".into());
    }
    s.adapter
        .chrome
        .workspace_settings
        .entry(s.demo.root)
        .or_default()
        .group_colors = Some(false);
    s.tabs(frame)?;
    s.click("Group: Proof group")?;
    s.click("Collapse group")?;
    if !s.demo.controller.snapshot().groups[&group].collapsed {
        return Err("widget group collapse missing".into());
    }
    s.checkpoint("tabs-group-collapsed-colors-disabled")?;
    s.tabs(frame)?;
    s.click("Group: Proof group")?;
    s.click("Expand group")?;
    if s.demo.controller.snapshot().groups[&group].collapsed {
        return Err("widget group expand missing".into());
    }
    s.tabs(frame)?;
    s.click("Group: Proof group")?;
    s.click("Group last")?;
    if s.demo.controller.snapshot().frames[&frame].groups.last() != Some(&group) {
        return Err("widget group reorder missing".into());
    }
    s.checkpoint("tabs-group-reordered-expanded")?;
    s.adapter
        .chrome
        .pages
        .insert(format!("groups/{}", frame.raw()), 1);
    s.tabs(frame)?;
    s.click("Group: Proof group")?;
    s.click("Move entire canonical group to…")?;
    let position = s
        .demo
        .controller
        .snapshot()
        .frames
        .keys()
        .position(|f| *f == destination)
        .ok_or("destination frame")?;
    for _ in 0..position {
        s.click_id("page/group-destinations/next")?;
    }
    s.click(&format!("Frame {}", destination.raw()))?;
    let snapshot = s.demo.controller.snapshot();
    if snapshot.groups[&group].frame != destination
        || [original[0], original[7]]
            .iter()
            .any(|v| !snapshot.frames[&destination].views.contains(v))
    {
        return Err("canonical group members not moved together".into());
    }
    s.checkpoint("tabs-entire-group-moved")?;
    s.tab_actions(original[2])?;
    s.click("Close (host/provider approval)")?;
    if s.demo.controller.snapshot().views[&original[2]]
        .closed
        .is_some()
        || !s
            .demo
            .host
            .receipts
            .iter()
            .any(|r| r.view == original[2] && r.close_approved == Some(false))
    {
        return Err("dirty provider veto not retained".into());
    }
    s.checkpoint("tabs-dirty-close-veto")?;
    s.command(Command::SetPinPolicy(kumihimo_core::PinPolicy {
        deny_close: true,
        deny_reorder: true,
        deny_cross_frame_move: true,
    }))?;
    let before = s.demo.controller.snapshot().frames[&frame].views.clone();
    s.tab_actions(original[1])?;
    s.click("Move tab first")?;
    if s.demo.controller.snapshot().frames[&frame].views != before {
        return Err("forbidden pinned reorder mutated membership".into());
    }
    s.checkpoint("tabs-pinned-reorder-rejected")?;
    Ok(s.finish("002"))
}

#[cfg(feature = "capture")]
fn dock_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> SceneResult<serde_json::Value> {
    use kumihimo_core::{Command, DockIntent, DockLocation, Edge};
    use kumihimo_egui::DesktopHost;
    let mut s = CaptureScene::new(renderer, output, budget, 3, 400)?;
    let frame = s.demo.controller.snapshot().views[&s.demo.text_view].frame;
    let beside = s.demo.controller.snapshot().workspaces[&s.demo.root].frames[1];
    let identities = s.demo.controller.snapshot().views.clone();
    let projection = kumihimo_egui::workspace::project_with_chrome(
        &mut s.demo.controller,
        s.surface,
        &mut s.demo.host,
        &s.adapter.chrome,
    )?;
    let target = kumihimo_egui::to_egui(
        projection
            .frames
            .iter()
            .find(|f| f.frame == beside)
            .ok_or("dock target")?
            .bounds,
    )
    .left_center();
    let press = s.point(&format!("frame/{}/drag", frame.raw()))?;
    let before = s.demo.controller.snapshot().clone();
    s.mouse(press, true)?;
    s.input(
        vec![egui::Event::PointerMoved(target)],
        egui::Modifiers::NONE,
    )?;
    if s.adapter.input.active_capture().is_none() {
        return Err("actual drag did not capture".into());
    }
    if s.demo.controller.snapshot().layouts != before.layouts {
        return Err("drag preview mutated arrangement".into());
    }
    s.checkpoint("dock-eligible-snap-preview")?;
    s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
    s.mouse(target, false)?;
    if s.demo.controller.snapshot().layouts != before.layouts
        || s.adapter.input.active_capture().is_some()
    {
        return Err("cancelled drag changed arrangement".into());
    }
    s.checkpoint("dock-cancelled-preview")?;
    let press = s.point(&format!("frame/{}/drag", frame.raw()))?;
    s.mouse(press, true)?;
    s.input(
        vec![egui::Event::PointerMoved(target)],
        egui::Modifiers::NONE,
    )?;
    s.mouse(target, false)?;
    s.settle()?;
    if s.demo.controller.snapshot().layouts == before.layouts {
        return Err("snap drag commit did not move arrangement".into());
    }
    s.checkpoint("dock-snap-committed")?;
    // Remove one actual allowed edge: snap skips it and the resulting out-of-bounds float rejects.
    let mut context = s.demo.controller.snapshot().contexts[&s.demo.root].clone();
    context.allowed_docks.retain(|l| {
        *l != DockLocation::Beside {
            frame: beside,
            edge: Edge::Left,
        }
    });
    context.allow_floating = false;
    s.command(Command::SetGeometryContext {
        workspace: s.demo.root,
        context: context.clone(),
    })?;
    s.settle()?;
    let unchanged = s.demo.controller.snapshot().clone();
    let press = s.point(&format!("frame/{}/drag", frame.raw()))?;
    s.mouse(press, true)?;
    s.input(
        vec![egui::Event::PointerMoved(egui::pos2(-80.0, -80.0))],
        egui::Modifiers::NONE,
    )?;
    s.checkpoint("dock-forbidden-drag-preview")?;
    s.mouse(egui::pos2(-80.0, -80.0), false)?;
    s.settle()?;
    if s.demo.controller.snapshot().layouts != unchanged.layouts
        || s.demo.controller.snapshot().floats != unchanged.floats
    {
        return Err("forbidden drag mutated placement".into());
    }
    context.allow_floating = true;
    context.allowed_docks.push(DockLocation::Beside {
        frame: beside,
        edge: Edge::Left,
    });
    s.command(Command::SetGeometryContext {
        workspace: s.demo.root,
        context,
    })?;
    s.settle()?;
    s.frame_action(frame, "Preview in-app float")?;
    if s.adapter.chrome.preview.is_none() {
        return Err("float UI preview missing".into());
    }
    s.checkpoint("dock-float-menu-preview")?;
    s.click_id("workspace/preview/commit")?;
    if !s.demo.controller.snapshot().floats.contains_key(&frame) {
        return Err("UI float missing".into());
    }
    s.checkpoint("dock-in-app-float")?;
    s.frame_action(frame, "Preview redock")?;
    s.click_id("workspace/preview/commit")?;
    if s.demo.controller.snapshot().floats.contains_key(&frame) {
        return Err("UI redock missing".into());
    }
    s.frame_action(frame, "Equalize workspace")?;
    s.checkpoint("dock-redocked-equalized")?;
    s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
    // Frozen method also allows command paths: canonical swap is paired with real final pixels.
    s.command(Command::SwapFrames {
        first: frame,
        second: beside,
    })?;
    s.settle()?;
    s.checkpoint("dock-command-swap")?;
    s.frame_action(frame, "Maximize frame")?;
    s.checkpoint("dock-maximized")?;
    s.frame_action(frame, "Restore workspace")?;
    s.checkpoint("dock-restored")?;
    if s.demo.controller.snapshot().views != identities {
        return Err("dock sequence changed view/resource identities".into());
    }
    // Generation-bound preview rejects a command after an independent canonical mutation.
    let preview = s.demo.controller.core().preview(DockIntent::Float {
        frame,
        rect: kumihimo_core::Rect::new(40.0, 80.0, 300.0, 240.0)?,
    })?;
    let view = s.demo.controller.snapshot().views[&s.demo.text_view].clone();
    s.command(Command::SetViewMetadata {
        view: view.id,
        label: format!("{} stale-preview", view.label),
        pinned: view.pinned,
        unsaved: view.unsaved,
    })?;
    let stale = s
        .demo
        .host
        .dispatch(&mut s.demo.controller, Command::CommitPreview(preview))?
        .outcome
        .result;
    if stale.is_ok() {
        return Err("stale command preview accepted".into());
    }
    s.observations.push(serde_json::json!({"variant":"generation-stale-command-preview","rejected":format!("{:?}",stale.err()),"provenance":"host-dispatched canonical command path"}));
    Ok(s.finish("003"))
}

#[cfg(feature = "capture")]
fn tool_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> SceneResult<serde_json::Value> {
    use kumihimo_core::{Axis, Command, Edge, HiddenSpace, ToolAnchor, ToolPlacement};
    use kumihimo_egui::DesktopHost;
    let mut s = CaptureScene::new(renderer, output, budget, 3, 500)?;
    let tool = s.demo.tool;
    let target = s.demo.controller.snapshot().tools[&tool].target;
    let frame = s.demo.controller.snapshot().views[&target.view].frame;
    let other = s.demo.controller.snapshot().frames[&frame].views[1];
    let ancestor = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(s.demo.root),
        edge: Edge::Bottom,
    };
    s.click_id(&format!("tool/{}/dock-menu", tool.raw()))?;
    s.click(&format!("{ancestor:?}"))?;
    s.click_id("workspace/preview/commit")?;
    if s.demo.controller.snapshot().tools[&tool].placement != ancestor {
        return Err("ancestor tool placement missing".into());
    }
    s.checkpoint("tool-ancestor-beside-two-providers")?;
    // Select the second host file, and focus the beside generated canvas through actual input.
    s.click_id(&format!("view/{}/tab", other.raw()))?;
    s.click_id(&format!("tool/{}/action", tool.raw()))?;
    let recipient = s
        .demo
        .host
        .receipts
        .iter()
        .rev()
        .find(|r| r.kind == "tool")
        .ok_or("tool ACK missing")?;
    if (recipient.view, recipient.resource) != (target.view, target.resource) {
        return Err("toolbar silently retargeted".into());
    }
    s.checkpoint("tool-exact-pinned-recipient-other-file-selected")?;
    let canvas = s
        .frame
        .report
        .slots
        .iter()
        .find(|slot| slot.view == s.demo.canvas_view)
        .ok_or("beside canvas")?;
    let previous_tool_request = s
        .demo
        .host
        .receipts
        .iter()
        .rev()
        .find(|r| r.kind == "tool")
        .map(|r| r.request);
    s.click_point(kumihimo_egui::to_egui(canvas.bounds).center())?;
    s.click_id(&format!("tool/{}/action", tool.raw()))?;
    let recipient = s
        .demo
        .host
        .receipts
        .iter()
        .rev()
        .find(|r| r.kind == "tool")
        .ok_or("focused neighbor tool ACK")?;
    if (recipient.view, recipient.resource) != (target.view, target.resource) {
        return Err("neighbor focus retargeted tool".into());
    }
    if Some(recipient.request) == previous_tool_request {
        return Err("neighbor-focus tool click produced no new ACK".into());
    }
    s.checkpoint("tool-neighbor-focus-pinned-recipient")?;
    for hidden_space in [HiddenSpace::Retain, HiddenSpace::Release] {
        let mut policy = s.demo.controller.snapshot().tools[&tool].policy.clone();
        policy.hidden_space = hidden_space;
        s.command(Command::ConfigureTool { tool, policy })?;
        let mut state = s.demo.controller.snapshot().views[&target.view].state;
        state.visible = false;
        s.command(Command::SetViewState {
            view: target.view,
            state,
        })?;
        s.settle()?;
        let neighbor = s.demo.controller.snapshot().workspaces[&s.demo.root].frames[1];
        s.command(Command::Maximize { frame: neighbor })?;
        s.settle()?;
        let t = &s.demo.controller.snapshot().tools[&tool];
        let presentation = s.demo.controller.snapshot().effective_tool_presentation(t);
        if presentation.visible
            || presentation.reserves_space != (hidden_space == HiddenSpace::Retain)
            || t.target != target
        {
            return Err("hidden target space/binding mismatch".into());
        }
        let rejected = s
            .demo
            .host
            .tool_action(&mut s.demo.controller, tool, "hidden-target-probe")
            .err()
            .ok_or("hidden target accepted tool")?;
        s.checkpoint(if hidden_space == HiddenSpace::Retain {
            "tool-hidden-target-retain"
        } else {
            "tool-hidden-target-release"
        })?;
        s.observations.push(serde_json::json!({"variant":"hidden-target-action-rejection","space":format!("{hidden_space:?}"),"cause":rejected.to_string()}));
        state.visible = true;
        s.command(Command::RestoreLayout {
            workspace: s.demo.root,
        })?;
        s.command(Command::SetViewState {
            view: target.view,
            state,
        })?;
        s.settle()?;
        s.checkpoint(if hidden_space == HiddenSpace::Retain {
            "tool-target-restored-retain"
        } else {
            "tool-target-restored-release"
        })?;
    }
    let left = ToolPlacement::Edge {
        anchor: ToolAnchor::Workspace(s.demo.root),
        edge: Edge::Left,
    };
    let mut policy = s.demo.controller.snapshot().tools[&tool].policy.clone();
    policy.orientations.push(Axis::Vertical);
    policy.allowed.push(left);
    s.command(Command::ConfigureTool { tool, policy })?;
    s.settle()?;
    s.click_id(&format!("tool/{}/dock-menu", tool.raw()))?;
    s.click(&format!("{left:?}"))?;
    s.click_id("workspace/preview/commit")?;
    if s.demo.controller.snapshot().tools[&tool].policy.orientation != Axis::Vertical {
        return Err("eligible edge did not rotate tool atomically".into());
    }
    s.checkpoint("tool-eligible-left-edge-rotation")?;
    s.command(Command::CloseView {
        view: target.view,
        approved: true,
    })?;
    s.settle()?;
    let rejection = s
        .demo
        .host
        .tool_action(&mut s.demo.controller, tool, "closed-target-probe")
        .err()
        .ok_or("closed target tool accepted")?;
    if s.demo.controller.snapshot().tools[&tool].target != target {
        return Err("closed target silently rebound".into());
    }
    s.checkpoint("tool-closed-target-rejected")?;
    s.observations.push(serde_json::json!({"variant":"closed-target-rejection","target_view":target.view.raw().to_string(),"target_resource":target.resource.raw().to_string(),"cause":rejection.to_string()}));
    Ok(s.finish("004"))
}

#[cfg(feature = "capture")]
fn chrome_input_variants(
    renderer: &mut kumihimo_egui::capture::OffscreenRenderer,
    output: &std::path::Path,
    budget: usize,
) -> SceneResult<serde_json::Value> {
    use kumihimo_core::Command;
    use kumihimo_egui::{chrome::SettingsPatch, ChromeProfile};
    let mut all = Vec::new();
    for (index, profile) in [
        ChromeProfile::Full,
        ChromeProfile::Compact,
        ChromeProfile::Contextual,
        ChromeProfile::Hidden,
    ]
    .into_iter()
    .enumerate()
    {
        let mut s = CaptureScene::new(renderer, output, budget, 3, 600 + index as u64 * 10)?;
        let frame = s.demo.controller.snapshot().views[&s.demo.text_view].frame;
        let identities = s.demo.controller.snapshot().views.clone();
        s.command(Command::Maximize { frame })?;
        s.adapter.chrome.workspace_settings.insert(
            s.demo.root,
            SettingsPatch {
                profile: Some(profile),
                header_height: Some(38.0),
                ..Default::default()
            },
        );
        s.input(vec![egui::Event::PointerGone], egui::Modifiers::NONE)?;
        s.settle()?;
        s.checkpoint(&format!(
            "chrome-profile-{}",
            format!("{profile:?}").to_lowercase()
        ))?;
        if matches!(profile, ChromeProfile::Hidden | ChromeProfile::Contextual) {
            s.key(egui::Key::F6, egui::Modifiers::NONE)?;
            if !s.adapter.chrome.revealed.contains(&frame) {
                return Err("keyboard hidden-controls reveal missing".into());
            }
            s.checkpoint(&format!(
                "chrome-{}-keyboard-revealed",
                format!("{profile:?}").to_lowercase()
            ))?;
        }
        s.adapter.chrome.revealed.clear();
        s.adapter.chrome.surface_settings.insert(
            s.surface.viewport,
            SettingsPatch {
                profile: Some(ChromeProfile::Hidden),
                header_height: Some(46.0),
                recreation_needed: true,
                ..Default::default()
            },
        );
        s.settle()?;
        let point = s.point(&format!("frame/{}/reveal", frame.raw()))?;
        for (phase, pressed) in [
            (egui::TouchPhase::Start, true),
            (egui::TouchPhase::End, false),
        ] {
            s.input(
                vec![
                    egui::Event::Touch {
                        device_id: egui::TouchDeviceId(1),
                        id: egui::TouchId(9),
                        phase,
                        pos: point,
                        force: None,
                    },
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                egui::Modifiers::NONE,
            )?;
        }
        s.settle()?;
        if !s.adapter.chrome.revealed.contains(&frame) {
            return Err("supported injected touch reveal missing".into());
        }
        let projection = kumihimo_egui::workspace::project_with_chrome(
            &mut s.demo.controller,
            s.surface,
            &mut s.demo.host,
            &s.adapter.chrome,
        )?;
        if projection.frames.iter().any(|f| {
            f.settings_source != "surface"
                || f.settings.header_height != 46.0
                || !f.recreation_needed
        }) {
            return Err("surface effective settings mismatch".into());
        }
        if s.demo.controller.snapshot().views != identities {
            return Err("chrome profile changed content identities".into());
        }
        s.checkpoint(&format!(
            "chrome-{}-touch-revealed-surface-override",
            format!("{profile:?}").to_lowercase()
        ))?;
        all.push(s.finish("005"));
    }
    for (index, frame_first) in [false, true].into_iter().enumerate() {
        let mut s = CaptureScene::new(renderer, output, budget, 4, 700 + index as u64 * 10)?;
        let view = s.demo.text_view;
        let frame = s.demo.controller.snapshot().views[&view].frame;
        s.demo.host.policy.settings.frame_shortcuts_first = frame_first;
        s.command(Command::Maximize { frame })?;
        s.settle()?;
        let slot = s
            .frame
            .report
            .slots
            .iter()
            .find(|slot| slot.view == view)
            .ok_or("typing editor slot")?;
        let point = egui::pos2(slot.bounds.x() as f32 + 30.0, slot.bounds.y() as f32 + 85.0);
        s.click_point(point)?;
        let editor_id = egui::Id::new(("demo-document", view.raw()));
        if !s.renderer.context.memory(|m| m.has_focus(editor_id)) {
            return Err("actual click did not focus TextEdit".into());
        }
        let before_cursor = egui::TextEdit::load_state(&s.renderer.context, editor_id)
            .and_then(|state| state.cursor.char_range())
            .ok_or("editor cursor missing")?
            .primary
            .index;
        s.input(
            vec![egui::Event::Text("actual editor typing proof".into())],
            egui::Modifiers::NONE,
        )?;
        s.settle()?;
        let after_cursor = egui::TextEdit::load_state(&s.renderer.context, editor_id)
            .and_then(|state| state.cursor.char_range())
            .ok_or("edited cursor missing")?
            .primary
            .index;
        if after_cursor != before_cursor + "actual editor typing proof".chars().count() {
            return Err("TextEdit did not consume injected text".into());
        }
        s.observations.push(serde_json::json!({"variant":"actual-TextEdit-typing","view":view.raw().to_string(),
            "resource":s.demo.controller.snapshot().views[&view].resource.raw().to_string(),"before_cursor":before_cursor,"after_cursor":after_cursor,
            "typed":"actual editor typing proof","frame_shortcuts_first":frame_first}));
        if !s
            .demo
            .host
            .receipts
            .iter()
            .any(|r| r.kind == "input" && r.view == view)
        {
            return Err("actual editor input ACK missing".into());
        }
        s.checkpoint(if frame_first {
            "editor-typed-frame-first"
        } else {
            "editor-typed-content-first"
        })?;
        s.key(egui::Key::Tab, egui::Modifiers::CTRL)?;
        let selected = s.demo.controller.snapshot().frames[&frame].selected_view;
        if (selected != Some(view)) != frame_first {
            return Err("conflicting shortcut precedence mismatch".into());
        }
        s.checkpoint(if frame_first {
            "editor-conflict-frame-owned"
        } else {
            "editor-conflict-content-owned"
        })?;
        s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        s.tab_actions(view)?;
        s.click("Select tab")?;
        s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        let slot = s
            .frame
            .report
            .slots
            .iter()
            .find(|slot| slot.view == view)
            .ok_or("refocused editor slot")?;
        s.click_point(egui::pos2(
            slot.bounds.x() as f32 + 30.0,
            slot.bounds.y() as f32 + 85.0,
        ))?;
        // Leave the editor via actual keyboard focus traversal before arrangement chords.
        for _ in 0..3 {
            s.key(egui::Key::Tab, egui::Modifiers::NONE)?;
        }
        s.demo.host.policy.settings.frame_shortcuts_first = true;
        s.key(egui::Key::F10, egui::Modifiers::CTRL)?;
        if s.demo
            .controller
            .snapshot()
            .maximized
            .contains_key(&s.demo.root)
        {
            return Err("keyboard restore failed".into());
        }
        s.checkpoint(if frame_first {
            "keyboard-restored-frame-policy"
        } else {
            "keyboard-restored-content-policy"
        })?;
        let resize = s
            .frame
            .report
            .controls
            .iter()
            .find(|c| c.id.starts_with("resize/") && c.enabled)
            .ok_or("keyboard resize control")?
            .clone();
        let id = egui::Id::new(&resize.id);
        // Tab traversal owns focus. No direct Core resize or synthetic focus assignment.
        for _ in 0..64 {
            if s.renderer.context.memory(|m| m.has_focus(id)) {
                break;
            }
            s.key(egui::Key::Tab, egui::Modifiers::NONE)?;
        }
        if !s.renderer.context.memory(|m| m.has_focus(id)) {
            return Err("keyboard traversal could not reach resize handle".into());
        }
        s.checkpoint(if frame_first {
            "keyboard-resize-focus-frame-policy"
        } else {
            "keyboard-resize-focus-content-policy"
        })?;
        let before = s.demo.controller.snapshot().layouts.clone();
        s.key(
            if resize.id.split('/').nth(3) == Some("x") {
                egui::Key::ArrowRight
            } else {
                egui::Key::ArrowDown
            },
            egui::Modifiers::NONE,
        )?;
        if s.demo.controller.snapshot().layouts == before {
            return Err("focused keyboard resize did not change canonical layout".into());
        }
        s.key(egui::Key::F10, egui::Modifiers::CTRL)?;
        if !s
            .demo
            .controller
            .snapshot()
            .maximized
            .contains_key(&s.demo.root)
        {
            return Err("keyboard maximize failed".into());
        }
        s.checkpoint(if frame_first {
            "keyboard-maximized-frame-policy"
        } else {
            "keyboard-maximized-content-policy"
        })?;
        let kumihimo_core::Change::Created(kumihimo_core::EntityId::Group(group)) =
            s.command(Command::CreateGroup {
                frame,
                label: "Keyboard group".into(),
                color: Some("#44aa66".into()),
            })?
        else {
            return Err("keyboard group setup".into());
        };
        s.command(Command::AssignGroup {
            view,
            group: Some(group),
        })?;
        s.settle()?;
        s.keyboard_activate(&format!("frame/{}/tabs-menu", frame.raw()))?;
        s.keyboard_activate(&format!("group/{}/menu", group.raw()))?;
        s.keyboard_activate(&format!("group/{}/collapse", group.raw()))?;
        if !s.demo.controller.snapshot().groups[&group].collapsed {
            return Err("keyboard group collapse missing".into());
        }
        s.checkpoint(if frame_first {
            "keyboard-group-collapsed-frame-policy"
        } else {
            "keyboard-group-collapsed-content-policy"
        })?;
        s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        s.keyboard_activate(&format!("frame/{}/tabs-menu", frame.raw()))?;
        s.keyboard_activate(&format!("group/{}/menu", group.raw()))?;
        s.keyboard_activate(&format!("group/{}/collapse", group.raw()))?;
        if s.demo.controller.snapshot().groups[&group].collapsed {
            return Err("keyboard group expand missing".into());
        }
        s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        s.key(egui::Key::F10, egui::Modifiers::CTRL)?;
        for floating in [true, false] {
            s.keyboard_activate(&format!("frame/{}/menu", frame.raw()))?;
            s.keyboard_activate(&format!("frame/{}/preview-float-redock", frame.raw()))?;
            s.keyboard_activate("workspace/preview/commit")?;
            if s.demo.controller.snapshot().floats.contains_key(&frame) != floating {
                return Err("keyboard float/redock outcome missing".into());
            }
            s.checkpoint(&format!(
                "keyboard-{}-policy-{}",
                if floating { "floated" } else { "redocked" },
                frame_first
            ))?;
            s.key(egui::Key::Escape, egui::Modifiers::NONE)?;
        }
        let close_view = s.demo.controller.snapshot().frames[&frame].views[3];
        s.keyboard_activate(&format!("frame/{}/tabs-menu", frame.raw()))?;
        s.keyboard_activate(&format!("view/{}/actions-menu", close_view.raw()))?;
        s.keyboard_activate(&format!("view/{}/close", close_view.raw()))?;
        if s.demo.controller.snapshot().views[&close_view]
            .closed
            .is_none()
        {
            return Err("keyboard clean close missing".into());
        }
        s.checkpoint(&format!("keyboard-closed-clean-tab-policy-{frame_first}"))?;
        all.push(s.finish("008"));
    }
    Ok(
        serde_json::json!({"cases":"005/008","scenes":all,"physical_touch":"NOT_OBSERVED","native_system_chrome":"separate native observation"}),
    )
}
