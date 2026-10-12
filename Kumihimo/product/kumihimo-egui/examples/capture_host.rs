#[cfg(feature = "capture")]
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use kumihimo_core::{Command, Rect};
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
    renderer
        .capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?
        .write(&output.join("compact-narrow"), budget)?;
    demo.host.policy.settings.profile = ChromeProfile::Full;
    surface.bounds = Rect::new(0.0, 0.0, 960.0, 640.0)?;
    surface.pixels_per_point = 1.5;
    surface.epoch += 1;
    renderer
        .capture(
            &mut adapter,
            &mut demo.controller,
            surface,
            &mut demo.host,
            Default::default(),
        )?
        .write(&output.join("injected-scale-150"), budget)?;
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
