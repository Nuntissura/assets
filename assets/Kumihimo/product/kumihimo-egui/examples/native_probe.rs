//! Bounded hidden native observations. Never requests activation or global input.
#[cfg(feature = "native")]
use kumihimo_egui::DesktopHost;
#[cfg(feature = "native")]
struct ExternalProbeHost(kumihimo_egui::demo::DemoHost);
#[cfg(feature = "native")]
impl kumihimo_egui::DesktopHost for ExternalProbeHost {
    fn policy(&self) -> kumihimo_egui::HostPolicy { self.0.policy() }
    fn capabilities(&self) -> kumihimo_egui::Capabilities { self.0.capabilities() }
    fn dispatch(&mut self, controller: &mut kumihimo_content::Controller, command: kumihimo_core::Command)
        -> kumihimo_egui::Result<kumihimo_core::Response> { self.0.dispatch(controller, command) }
    fn paint_content(&mut self, ui: &mut egui::Ui, slot: &kumihimo_egui::RenderSlot,
        controller: &mut kumihimo_content::Controller) -> kumihimo_egui::Result<()> {
        self.0.paint_content(ui, slot, controller)
    }
    fn content_visibility(&mut self, controller: &mut kumihimo_content::Controller,
        surface: kumihimo_egui::SurfaceBinding, visible: &[kumihimo_core::ViewId]) -> kumihimo_egui::Result<()> {
        self.0.content_visibility(controller, surface, visible)
    }
    fn content_input(&mut self, controller: &mut kumihimo_content::Controller,
        slot: &kumihimo_egui::RenderSlot, input: kumihimo_content::Input) -> kumihimo_egui::Result<()> {
        self.0.content_input(controller, slot, input)
    }
    fn tool_action(&mut self, controller: &mut kumihimo_content::Controller,
        tool: kumihimo_core::ToolId, payload: &str) -> kumihimo_egui::Result<()> {
        self.0.tool_action(controller, tool, payload)
    }
    fn native_action(&mut self, controller: &mut kumihimo_content::Controller,
        action: kumihimo_egui::AdapterAction) -> kumihimo_egui::Result<()> {
        self.0.native_action(controller, action)
    }
    #[cfg(feature = "navigation")]
    fn navigation_permission(&self, controller: &kumihimo_content::Controller,
        scope: &kumihimo_content::Scope) -> kumihimo_content::HostPermission {
        self.0.navigation_permission(controller, scope)
    }
    #[cfg(feature = "navigation")]
    fn navigation_pending(&mut self, controller: &mut kumihimo_content::Controller) -> kumihimo_egui::Result<()> {
        self.0.navigation_pending(controller)
    }
}
#[cfg(feature = "native")]
impl kumihimo_egui::native::NativeHost for ExternalProbeHost {
    fn take_native_actions(&mut self) -> Vec<kumihimo_egui::AdapterAction> {
        kumihimo_egui::native::NativeHost::take_native_actions(&mut self.0)
    }
    fn native_host_snapshot(&self, controller: &kumihimo_content::Controller) -> serde_json::Value {
        kumihimo_egui::native::NativeHost::native_host_snapshot(&self.0, controller)
    }
    fn surface_observation(&mut self, controller: &mut kumihimo_content::Controller,
        surface: kumihimo_egui::SurfaceBinding, minimized: bool,
        occlusion: kumihimo_content::Observation<bool>) -> kumihimo_egui::Result<()> {
        kumihimo_egui::native::NativeHost::surface_observation(&mut self.0, controller, surface, minimized, occlusion)
    }
    fn external_native_handoff(
        &mut self, coordinator: &mut kumihimo_egui::native::ExternalNativeCoordinator<'_>,
        surface: kumihimo_egui::SurfaceBinding, controller: &mut kumihimo_content::Controller,
        pixels_per_point: f32, primitives: &[egui::ClippedPrimitive], textures: &egui::TexturesDelta,
    ) -> kumihimo_egui::Result<kumihimo_egui::native::NativeCommitOutcome> {
        coordinator.paint(surface, controller.snapshot(), pixels_per_point,
            [0.025, 0.03, 0.045, 1.0], primitives, textures)?;
        loop {
            let (outcome, forwarded) = coordinator.poll_and_commit(surface, controller, self)?;
            if !forwarded.is_empty() { return Err(kumihimo_egui::AdapterError::Invalid("unexpected public probe readback")); }
            if let Some(outcome) = outcome { return Ok(outcome); }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
#[cfg(feature = "native")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kumihimo_core::{Change, Command, EntityId};
    use kumihimo_egui::{native::NativeOptions, DesktopHost};
    let mut options = NativeOptions::default();
    options.external_native_probe = true;
    let mut eframe_mode = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--eframe" => eframe_mode = true,
            "--output" => options.output = args.next().ok_or("missing output")?.into(),
            "--observer-script" => {
                options.observer_script = Some(args.next().ok_or("missing observer script")?.into())
            }
            "--no-uia" => options.observer_script = None,
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if eframe_mode {
        #[cfg(all(feature = "native-eframe", windows))]
        return eframe_probe::run(options.output, options.timeout);
        #[cfg(not(all(feature = "native-eframe", windows)))]
        return Err("--eframe requires native-eframe on Windows".into());
    }
    if options.observer_script.is_none() && !std::env::args().any(|a| a == "--no-uia") {
        // Resolve from compile-time package location; moving/rebuilding the project refreshes it.
        options.observer_script =
            Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/observe-uia.ps1"));
    }
    let mut demo = kumihimo_egui::demo::new()?;
    options.probe_view = Some(demo.text_view);
    options.probe_frame = Some(demo.controller.snapshot().views[&demo.text_view].frame);
    let text_frame = demo.controller.snapshot().views[&demo.text_view].frame;
    options.observer_target = demo.controller.snapshot().frames[&text_frame]
        .views
        .get(1)
        .copied();
    let canvas_frame = demo.controller.snapshot().views[&demo.canvas_view].frame;
    let group = match demo
        .host
        .dispatch(
            &mut demo.controller,
            Command::CreateGroup {
                frame: canvas_frame,
                label: "Native group demonstration".into(),
                color: Some("blue".into()),
            },
        )?
        .outcome
        .result?
    {
        Change::Created(EntityId::Group(id)) => id,
        _ => return Err("group creation response".into()),
    };
    demo.host
        .dispatch(
            &mut demo.controller,
            Command::AssignGroup {
                view: demo.canvas_view,
                group: Some(group),
            },
        )?
        .outcome
        .result?;
    options.probe_group = Some(group);
    options.probe_partial_close = true;
    let result = kumihimo_egui::native::run(demo.controller, ExternalProbeHost(demo.host), demo.root, options)?;
    println!(
        "native_manifest={} captures={} returned={} uia_forwarded={}",
        result.manifest.display(),
        result.captures,
        result.returned,
        result.uia_actions
    );
    Ok(())
}
#[cfg(not(feature = "native"))]
fn main() {
    eprintln!("native_probe requires the native feature");
}

#[cfg(all(feature = "native-eframe", windows))]
mod eframe_probe {
    use kumihimo_egui::{demo, native::EframeNativeCoordinator, persistence::NativeContent,
        window_slots::{PendingNative, WindowSlots}, AdapterError, DesktopAdapter, DesktopHost, SurfaceBinding};
    use kumihimo_core::{Rect, WorkspaceId};
    use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex}, time::{Duration, Instant}};

    struct Model {
        demo: demo::Demo,
        slots: WindowSlots,
        adapters: HashMap<egui::ViewportId, DesktopAdapter>,
        error: Option<String>,
    }
    struct Probe {
        model: Arc<Mutex<Model>>,
        handle: Option<egui_wgpu::NativeCaptureHandle>,
        coordinator: Option<EframeNativeCoordinator>,
        pending: Option<PendingNative>,
        direct: Option<egui_wgpu::NativeCaptureRequest>,
        request: Option<u64>,
        old_target: Option<egui_wgpu::NativeCaptureTarget>,
        child: Option<(WorkspaceId, bool)>,
        child_size: [f32; 2],
        phase: u8,
        started: Instant,
        timeout: Duration,
        output: PathBuf,
        captures: Vec<serde_json::Value>,
        observations: Vec<serde_json::Value>,
        hwnds: Vec<isize>,
    }
    fn child_id() -> egui::ViewportId { egui::ViewportId::from_hash_of("kumihimo-eframe-native-child") }
    fn observe(window: Option<&Arc<winit::window::Window>>) -> serde_json::Value {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let foreground = unsafe { GetForegroundWindow() };
        let handle = window.and_then(|window| window.window_handle().ok()).and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(value) => Some(value.hwnd.get()), _ => None,
        }).unwrap_or(0);
        let mut owner = 0;
        if handle != 0 { unsafe { GetWindowThreadProcessId(handle as _, &mut owner); } }
        serde_json::json!({"hwnd":handle,"owner_pid":owner,"expected_pid":std::process::id(),
            "visible":window.and_then(|window|window.is_visible()),"focused":window.map(|window|window.has_focus()),
            "foreground":foreground as isize})
    }
    fn paint(model: &Arc<Mutex<Model>>, handle: &egui_wgpu::NativeCaptureHandle,
        ctx: &egui::Context, root: WorkspaceId) {
        let target = match handle.target(ctx.viewport_id()) {
            Ok(target) => target,
            Err(_) => { ctx.request_repaint(); return; }
        };
        let dimensions = target.physical_dimensions();
        let scale = target.scale_factor();
        let result = (|| -> kumihimo_egui::Result<()> {
            let bounds = Rect::new(0.0, 0.0, dimensions[0] as f64 / scale, dimensions[1] as f64 / scale)?;
            let surface = SurfaceBinding { root, viewport:ctx.viewport_id(), epoch:target.surface_incarnation(),
                bounds, pixels_per_point:scale as f32 };
            let mut model = model.lock().unwrap();
            let mut adapter = model.adapters.remove(&ctx.viewport_id()).unwrap_or_default();
            let demo::Demo { controller, host, .. } = &mut model.demo;
            let report = adapter.show(ctx, controller, surface, host)?;
            if !report.rejected.is_empty() { return Err(AdapterError::Host(format!("eframe adapter: {:?}",report.rejected))); }
            model.adapters.insert(ctx.viewport_id(), adapter);
            Ok(())
        })();
        if let Err(error) = result { model.lock().unwrap().error = Some(format!("{error:?}")); }
    }
    impl Probe {
        fn save(&mut self, label: &str, image: &egui::ColorImage, proof: serde_json::Value) -> kumihimo_egui::Result<()> {
            if self.captures.len() >= 16 { return Err(AdapterError::Budget("eframe probe captures")); }
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|pixel|pixel.to_array()).collect();
            let file = format!("eframe-{:02}-{label}.png", self.captures.len()+1);
            image::save_buffer(self.output.join(&file), &rgba, image.size[0] as u32, image.size[1] as u32,
                image::ColorType::Rgba8).map_err(|error|AdapterError::Host(error.to_string()))?;
            self.captures.push(serde_json::json!({"file":file,"label":label,"readback_bytes":rgba.len(),
                "physical_dimensions":image.size,"pixels_nonuniform":image.pixels.first().is_some_and(|first|image.pixels.iter().any(|pixel|pixel!=first)),"sdk":proof}));
            Ok(())
        }
        fn capture(&mut self, viewport: egui::ViewportId, label: &str) -> kumihimo_egui::Result<bool> {
            let handle = self.handle.as_ref().unwrap().clone();
            if self.direct.is_none() {
                self.direct = Some(handle.request(viewport).map_err(|error|AdapterError::Host(format!("SDK capture: {error:?}")))?);
                return Ok(false);
            }
            let request = self.direct.as_ref().unwrap();
            match handle.poll(request).map_err(|error|AdapterError::Host(format!("SDK poll: {error:?}")))? {
                None => Ok(false),
                Some(egui_wgpu::NativeCaptureOutcome::Failed(error)) => Err(AdapterError::Host(format!("SDK readback: {error:?}"))),
                Some(egui_wgpu::NativeCaptureOutcome::Presented(receipt)) => {
                    let observation = observe(Some(receipt.window()));
                    if observation["owner_pid"] != observation["expected_pid"] || observation["visible"] != false
                        || observation["focused"] != false {
                        return Err(AdapterError::Invalid("eframe probe window ownership/quietness"));
                    }
                    self.hwnds.push(observation["hwnd"].as_i64().unwrap_or(0) as isize);
                    self.observations.push(serde_json::json!({"phase":"actual-sdk-native-target","label":label,"window":observation}));
                    if !matches!(handle.poll(request), Err(egui_wgpu::NativeCaptureFailure::AlreadyConsumed)) {
                        return Err(AdapterError::Invalid("SDK receipt replay accepted"));
                    }
                    let proof = serde_json::json!({"request_id":receipt.request_id(),"viewport":format!("{:?}",receipt.viewport_id()),
                        "renderer_incarnation":receipt.renderer_incarnation(),"surface_incarnation":receipt.surface_incarnation(),
                        "submission_id":receipt.submission_id(),"scale_factor":receipt.scale_factor(),"readback_bytes":receipt.readback_bytes()});
                    self.save(label, receipt.image(), proof)?;
                    self.direct = None;
                    Ok(true)
                }
            }
        }
        fn prepare(&mut self, immediate: bool) -> kumihimo_egui::Result<()> {
            let mut model = self.model.lock().unwrap();
            let Model { demo, slots, .. } = &mut *model;
            let view = if immediate { demo.canvas_view } else { demo.text_view };
            let pending = slots.prepare(&mut demo.controller, &mut demo.host, NativeContent::View(view))?;
            self.child = Some((pending.slot.root, immediate));
            self.pending = Some(pending);
            Ok(())
        }
        fn start_handoff(&mut self, cancellation_case: bool) -> kumihimo_egui::Result<bool> {
            let handle = self.handle.as_ref().unwrap();
            if handle.target(child_id()).is_err() { return Ok(false); }
            let mut model = self.model.lock().unwrap();
            let Model { demo, slots, .. } = &mut *model;
            let coordinator = self.coordinator.as_mut().unwrap();
            if cancellation_case {
                let before = demo.controller.snapshot().views.clone();
                let cancelled = coordinator.request(child_id(), slots, self.pending.as_mut().unwrap(),
                    &mut demo.controller, &mut demo.host)?;
                coordinator.cancel(cancelled, slots, demo.controller.snapshot())?;
                if coordinator.poll_and_commit(cancelled, slots, &mut demo.controller, &mut demo.host).is_ok()
                    || demo.controller.snapshot().views != before {
                    return Err(AdapterError::Invalid("eframe cancelled handoff moved source"));
                }
                let view = demo.text_view;
                let fresh = slots.prepare(&mut demo.controller, &mut demo.host, NativeContent::View(view))?;
                self.child = Some((fresh.slot.root, false));
                self.pending = Some(fresh);
                self.observations.push(serde_json::json!({"phase":"canonical-handoff-cancelled","request":cancelled,"source_unchanged":true,"SDK_queue_settled":true}));
            }
            self.request = Some(coordinator.request(child_id(), slots, self.pending.as_mut().unwrap(),
                &mut demo.controller, &mut demo.host)?);
            let before = demo.controller.snapshot().clone();
            if coordinator.poll_and_commit(u64::MAX, slots, &mut demo.controller, &mut demo.host).is_ok()
                || demo.controller.snapshot() != &before {
                return Err(AdapterError::Invalid("eframe foreign request changed source"));
            }
            Ok(true)
        }
        fn commit(&mut self, label: &str) -> kumihimo_egui::Result<bool> {
            let outcome = {
                let mut model = self.model.lock().unwrap();
                let Model { demo, slots, .. } = &mut *model;
                self.coordinator.as_mut().unwrap().poll_and_commit(self.request.unwrap(), slots,
                    &mut demo.controller, &mut demo.host)?
            };
            let Some(outcome) = outcome else { return Ok(false); };
            self.save(label, outcome.image(), outcome.sdk_observation().cloned().unwrap())?;
            let mut model = self.model.lock().unwrap();
            let Model { demo, slots, .. } = &mut *model;
            let before = demo.controller.snapshot().clone();
            if self.coordinator.as_mut().unwrap().poll_and_commit(self.request.unwrap(), slots,
                &mut demo.controller, &mut demo.host).is_ok() || demo.controller.snapshot() != &before {
                return Err(AdapterError::Invalid("canonical SDK handoff replay accepted"));
            }
            self.observations.push(serde_json::json!({"phase":"canonical-sdk-detach","label":label,
                "root":outcome.binding().root.raw(),"generation":before.generation,"replay_rejected":true}));
            self.request = None;
            Ok(true)
        }
        fn return_content(&mut self) -> kumihimo_egui::Result<()> {
            let root = self.child.unwrap().0;
            let mut model = self.model.lock().unwrap();
            let Model { demo, slots, .. } = &mut *model;
            slots.return_content(root, None, &mut demo.controller, &mut demo.host)?;
            self.observations.push(serde_json::json!({"phase":"semantic-native-return","root":root.raw(),
                "remaining_bindings":slots.bindings().len(),"identity":format!("{:?}",demo.controller.snapshot().identity)}));
            Ok(())
        }
        fn step(&mut self, ctx: &egui::Context) -> kumihimo_egui::Result<()> {
            if self.started.elapsed() >= self.timeout { return Err(AdapterError::Host("eframe probe deadline".into())); }
            let handle = self.handle.as_ref().unwrap().clone();
            let phase = self.phase;
            match phase {
                0 => {
                    let mut queued = Vec::new();
                    loop {
                        match handle.request(egui::ViewportId::ROOT) {
                            Ok(request) if queued.len()<16 => queued.push(request),
                            Err(egui_wgpu::NativeCaptureFailure::QueueFull) => break,
                            _ => return Err(AdapterError::Invalid("SDK capture queue did not enforce bounded admission")),
                        }
                    }
                    let count = queued.len();
                    for request in queued {
                        handle.cancel(&request).map_err(|error|AdapterError::Host(format!("SDK cancel: {error:?}")))?;
                        if !matches!(handle.poll(&request), Ok(Some(egui_wgpu::NativeCaptureOutcome::Failed(egui_wgpu::NativeCaptureFailure::Cancelled)))) {
                            return Err(AdapterError::Invalid("SDK cancelled request did not settle"));
                        }
                    }
                    self.direct = Some(handle.request(egui::ViewportId::ROOT).map_err(|error|AdapterError::Host(format!("SDK root capture: {error:?}")))?);
                    ctx.input_mut(|input|input.events.push(egui::Event::Screenshot { viewport_id:egui::ViewportId::ROOT,
                        user_data:egui::UserData::new("forged-input"),image:Arc::new(egui::ColorImage::filled([2,2],egui::Color32::RED)) }));
                    if !matches!(handle.poll(self.direct.as_ref().unwrap()),Ok(None)) {
                        return Err(AdapterError::Invalid("raw screenshot input produced native receipt"));
                    }
                    self.observations.push(serde_json::json!({"phase":"bounded-sdk-producer-negatives","queue_full_after":count,
                        "cancelled_requests_settled":count,"raw_Screenshot_not_receipt":true}));
                    self.phase = 1;
                }
                1 if self.capture(egui::ViewportId::ROOT,"root")? => { self.prepare(false)?; self.phase=2; }
                2 if self.start_handoff(true)? => self.phase=3,
                3 if self.commit("deferred-preflight")? => self.phase=4,
                4 if self.capture(child_id(),"deferred-populated")? => {
                    self.old_target=Some(handle.target(child_id()).map_err(|error|AdapterError::Host(format!("SDK old target: {error:?}")))?);
                    self.direct=Some(handle.request(child_id()).map_err(|error|AdapterError::Host(format!("SDK removed request: {error:?}")))?);
                    self.return_content()?; self.child=None; self.phase=5;
                }
                5 if matches!(handle.target(child_id()),Err(egui_wgpu::NativeCaptureFailure::UnknownViewport)) => {
                    if !matches!(handle.poll(self.direct.as_ref().unwrap()),Ok(Some(egui_wgpu::NativeCaptureOutcome::Failed(_)))) {
                        return Err(AdapterError::Invalid("removed SDK surface accepted old request"));
                    }
                    self.direct=None; self.prepare(true)?; self.phase=6;
                }
                6 if handle.target(child_id()).is_ok() => {
                    let actual=handle.target(child_id()).unwrap();let old=self.old_target.as_ref().unwrap();
                    if actual.surface_incarnation()==old.surface_incarnation() || Arc::ptr_eq(actual.window(),old.window()) {
                        return Err(AdapterError::Invalid("SDK surface recreation reused old target identity"));
                    }
                    self.observations.push(serde_json::json!({"phase":"same-viewport-recreated","old_incarnation":old.surface_incarnation(),
                        "new_incarnation":actual.surface_incarnation(),"new_actual_Window":true}));
                    if self.start_handoff(false)? { self.old_target=None;self.phase=7; }
                }
                7 if self.commit("immediate-preflight")? => self.phase=8,
                8 if self.capture(child_id(),"immediate-populated")? => {
                    self.old_target=Some(handle.target(child_id()).unwrap());self.direct=Some(handle.request(child_id()).unwrap());
                    self.child_size=[920.0,680.0];self.phase=9;
                }
                9 if handle.target(child_id()).is_ok_and(|target|target.physical_dimensions()!=self.old_target.as_ref().unwrap().physical_dimensions()) => {
                    if !matches!(handle.poll(self.direct.as_ref().unwrap()),Ok(Some(egui_wgpu::NativeCaptureOutcome::Failed(_)))) {
                        return Err(AdapterError::Invalid("resized SDK surface accepted old request"));
                    }
                    self.direct=None;self.old_target=None;
                    self.observations.push(serde_json::json!({"phase":"actual-resize-stale-readback-rejected"}));self.phase=10;
                }
                10 if self.capture(child_id(),"immediate-resized")? => { self.return_content()?;self.phase=11; }
                11 if self.capture(egui::ViewportId::ROOT,"returned-root")? => { self.child=None;self.phase=12; }
                12 => {
                    let model=self.model.lock().unwrap();
                    if !model.slots.bindings().is_empty() { return Err(AdapterError::Invalid("eframe probe stranded native content")); }
                    let manifest=serde_json::json!({"schema":"kumihimo.eframe-native-proof.v1","captures":self.captures,
                        "observations":self.observations,"generation":model.demo.controller.snapshot().generation,
                        "remaining_native_bindings":model.slots.bindings().len(),"owned_hwnds":self.hwnds,"error":null,
                        "not_proven":["Handshake adoption","physical display presentation","physical input","other OS/ARM"]});
                    std::fs::write(self.output.join("eframe-native-manifest.json"),serde_json::to_vec_pretty(&manifest).unwrap())
                        .map_err(|error|AdapterError::Host(error.to_string()))?;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);self.phase=13;
                }
                _=>{}
            }
            Ok(())
        }
    }
    impl eframe::App for Probe {
        fn update(&mut self,ctx:&egui::Context,frame:&mut eframe::Frame) {
            if self.handle.is_none() {
                let Some(handle)=frame.native_capture_handle().cloned() else {
                    self.model.lock().unwrap().error=Some("native-wgpu producer handle unavailable".into());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);return;
                };
                self.coordinator=Some(EframeNativeCoordinator::new(handle.clone(),4,32*1024*1024,self.timeout).unwrap());
                self.handle=Some(handle);
            }
            let root=self.model.lock().unwrap().demo.root;
            paint(&self.model,self.handle.as_ref().unwrap(),ctx,root);
            if let Err(error)=self.step(ctx) { self.model.lock().unwrap().error=Some(format!("{error:?}")); }
            if self.model.lock().unwrap().error.is_some() { ctx.send_viewport_cmd(egui::ViewportCommand::Close);return; }
            if let Some((root,immediate))=self.child {
                let builder=egui::ViewportBuilder::default().with_title("Kumihimo eframe native proof")
                    .with_inner_size(self.child_size).with_visible(false).with_active(false);
                let model=self.model.clone();let handle=self.handle.as_ref().unwrap().clone();
                if immediate { ctx.show_viewport_immediate(child_id(),builder,move |ctx,class| {
                    if class!=egui::ViewportClass::Immediate { model.lock().unwrap().error=Some("immediate viewport was embedded".into()); }
                    else { paint(&model,&handle,ctx,root); }
                }); } else { ctx.show_viewport_deferred(child_id(),builder,move |ctx,class| {
                    if class!=egui::ViewportClass::Deferred { model.lock().unwrap().error=Some("deferred viewport was embedded".into()); }
                    else { paint(&model,&handle,ctx,root); }
                    ctx.request_repaint_after(Duration::from_millis(10));
                }); }
            }
            if self.phase<13 { ctx.request_repaint_after(Duration::from_millis(10)); }
        }
    }
    pub fn run(output:PathBuf,timeout:Duration)->Result<(),Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&output)?;
        let before=observe(None);
        let model=Arc::new(Mutex::new(Model { demo:demo::new()?, slots:WindowSlots::default(),adapters:HashMap::new(),error:None }));
        let mut setup=egui_wgpu::WgpuSetupCreateNew::default();setup.instance_descriptor.backends=wgpu::Backends::DX12;
        let options=eframe::NativeOptions { renderer:eframe::Renderer::Wgpu,run_and_return:true,persist_window:false,
            viewport:egui::ViewportBuilder::default().with_inner_size([960.0,640.0]).with_visible(false).with_active(false),
            wgpu_options:egui_wgpu::WgpuConfiguration { wgpu_setup:setup.into(),..Default::default() },..Default::default() };
        let app_model=model.clone();let app_output=output.clone();
        eframe::run_native("Kumihimo eframe native proof",options,Box::new(move |_|Ok(Box::new(Probe {
            model:app_model,handle:None,coordinator:None,pending:None,direct:None,request:None,old_target:None,child:None,
            child_size:[1000.0,720.0],phase:0,started:Instant::now(),timeout,output:app_output,captures:Vec::new(),
            observations:vec![serde_json::json!({"phase":"before","native":before})],hwnds:Vec::new(),
        }))))?;
        if let Some(error)=model.lock().unwrap().error.clone() { return Err(error.into()); }
        let path=output.join("eframe-native-manifest.json");
        let mut manifest:serde_json::Value=serde_json::from_slice(&std::fs::read(&path)?)?;
        let after=observe(None);let before=manifest["observations"][0]["native"]["foreground"].clone();
        if before!=after["foreground"] { return Err("eframe probe changed foreground window".into()); }
        let destroyed=manifest["owned_hwnds"].as_array().unwrap().iter().all(|value| {
            let mut owner=0;unsafe {windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(value.as_i64().unwrap() as _,&mut owner);}
            owner!=std::process::id()
        });
        if !destroyed { return Err("eframe probe left owned HWNDs alive".into()); }
        manifest["cleanup"]=serde_json::json!({"owned_HWNDs_destroyed":destroyed,"foreground_unchanged":true,"after":after});
        std::fs::write(&path,serde_json::to_vec_pretty(&manifest)?)?;
        println!("eframe_native_manifest={}",path.display());Ok(())
    }
}
