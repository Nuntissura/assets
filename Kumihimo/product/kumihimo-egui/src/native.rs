//! Thin winit/Painter integration, following egui 0.33.3 public integration APIs.
//! API provenance: egui-winit/src/lib.rs and egui-wgpu/src/winit.rs (MIT OR Apache-2.0).
//! No eframe code, automatic first-frame showing, global input, or readiness callbacks.
use crate::{
    persistence::{NativeBinding, NativeContent, ReturnAnchor},
    window_slots::{PendingNative, WindowSlots},
    *,
};
use kumihimo_core::{Identity, Snapshot};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

/// This value cannot be constructed by hosts, tests, callbacks or window-slot callers.
/// It is minted only after successful set_window and an owned matched Screenshot event.
pub struct Ready {
    tag: CaptureTag,
    scale: f32,
    bytes: usize,
}
impl Ready {
    pub(crate) fn validate(&self, pending: &PendingNative, snapshot: &Snapshot) -> Result<()> {
        if self.tag.pending_nonce != Some(pending.nonce)
            || self.tag.root != pending.slot.root
            || self.tag.identity != pending.identity
            || self.tag.generation != pending.generation
            || snapshot.identity != self.tag.identity
            || snapshot.generation != self.tag.generation
            || self.tag.epoch == 0
            || self.tag.hwnd == 0
            || self.tag.size.contains(&0)
            || !self.scale.is_finite()
            || self.scale <= 0.0
            || self.bytes == 0
        {
            return Err(AdapterError::Stale("native readiness receipt"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq)]
struct CaptureTag {
    capture: u64,
    pending_nonce: Option<u64>,
    viewport: egui::ViewportId,
    root: WorkspaceId,
    hwnd: isize,
    epoch: u64,
    size: [u32; 2],
    scale: f32,
    identity: Identity,
    generation: u64,
    label: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceState {
    Unavailable,
    Recreating,
    Ready,
    Lost,
    Failed,
}
#[derive(Clone, Debug)]
pub struct NativeOptions {
    /// The only mode allowed to make windows visible; must be explicitly selected.
    pub interactive: bool,
    pub probe: bool,
    pub output: PathBuf,
    pub timeout: Duration,
    pub observer_timeout: Duration,
    pub observer_script: Option<PathBuf>,
    pub observer_target: Option<ViewId>,
    pub probe_view: Option<ViewId>,
    pub probe_frame: Option<FrameId>,
    pub probe_group: Option<kumihimo_core::GroupId>,
    pub probe_partial_close: bool,
    pub max_retries: u32,
    pub max_captures: usize,
}
impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            interactive: false,
            probe: true,
            output: PathBuf::from(".local/desktop-proof"),
            timeout: Duration::from_secs(45),
            observer_timeout: Duration::from_secs(10),
            observer_script: None,
            observer_target: None,
            probe_view: None,
            probe_frame: None,
            probe_group: None,
            probe_partial_close: false,
            max_retries: 3,
            max_captures: 24,
        }
    }
}
#[derive(Clone, Debug)]
pub struct NativeOutcome {
    pub manifest: PathBuf,
    pub captures: usize,
    pub returned: usize,
    pub uia_actions: usize,
}
#[derive(Clone, Copy, Debug)]
pub enum NativeClosePolicy {
    Return,
    CloseContent { host_approved: bool },
}
/// Host coordination extends the borrowed desktop path without duplicating command allocation.
pub trait NativeHost: DesktopHost {
    fn take_native_actions(&mut self) -> Vec<AdapterAction> {
        Vec::new()
    }
    fn close_policy(&self, _root: WorkspaceId) -> NativeClosePolicy {
        NativeClosePolicy::Return
    }
    fn return_fallback(
        &self,
        _controller: &Controller,
        _binding: &NativeBinding,
    ) -> Option<ReturnAnchor> {
        None
    }
    fn renderer_binding(&mut self, _surface: SurfaceBinding, _binding: crate::content::GpuBinding) {
    }
}
impl NativeHost for crate::demo::DemoHost {
    fn take_native_actions(&mut self) -> Vec<AdapterAction> {
        std::mem::take(&mut self.native_actions)
    }
}
enum NativeEvent {
    Accessibility(egui_winit::accesskit_winit::Event),
    Repaint {
        viewport: egui::ViewportId,
        after: Duration,
        pass: u64,
    },
}
impl From<egui_winit::accesskit_winit::Event> for NativeEvent {
    fn from(event: egui_winit::accesskit_winit::Event) -> Self {
        Self::Accessibility(event)
    }
}
struct NativeWindow {
    window: Arc<Window>,
    state: egui_winit::State,
    adapter: DesktopAdapter,
    surface: SurfaceBinding,
    status: SurfaceState,
    dirty: bool,
    deadline: Option<Instant>,
    capture: Option<CaptureTag>,
    capture_intent: Option<(String, Option<u64>)>,
    actual_size: [u32; 2],
    actual_scale: f32,
    retries: u32,
    focused: bool,
    occluded: Option<bool>,
    report: FrameReport,
}
enum ProbePhase {
    Initial,
    ObserveInitial,
    Moving,
    Populated,
    Resizing,
    Recreated,
    HeldSurvivors,
    Observer,
    Finished,
}
struct App<H: NativeHost> {
    controller: Controller,
    host: H,
    root: WorkspaceId,
    options: NativeOptions,
    ctx: egui::Context,
    painter: egui_wgpu::winit::Painter,
    proxy: EventLoopProxy<NativeEvent>,
    windows: HashMap<WindowId, NativeWindow>,
    slots: WindowSlots,
    pending: Option<PendingNative>,
    next_capture: u64,
    capture_bytes: usize,
    captures: Vec<serde_json::Value>,
    observations: Vec<serde_json::Value>,
    plan: Vec<NativeContent>,
    current: Option<WindowId>,
    phase: ProbePhase,
    started: Instant,
    observer: Option<std::process::Child>,
    observer_started: Option<Instant>,
    observer_path: PathBuf,
    observer_nonce: String,
    observer_result: Option<serde_json::Value>,
    accesskit_trace: Vec<serde_json::Value>,
    observer_generation: Option<u64>,
    observer_frame: Option<FrameId>,
    observer_previous: Option<ViewId>,
    uia_actions: usize,
    returned: usize,
    error: Option<AdapterError>,
    before: serde_json::Value,
}

/// Owns one shared Controller and dispatcher for all windows. Quiet is the default.
pub fn run<H: NativeHost + 'static>(
    controller: Controller,
    host: H,
    root: WorkspaceId,
    options: NativeOptions,
) -> Result<NativeOutcome> {
    #[cfg(not(windows))]
    {
        let _ = (controller, host, root, options);
        return Err(AdapterError::Unsupported(
            "native observations currently require Windows",
        ));
    }
    #[cfg(windows)]
    {
        if options.max_captures == 0
            || options.max_captures > 128
            || options.max_retries == 0
            || options.max_retries > 8
            || options.timeout.is_zero()
            || options.timeout > Duration::from_secs(120)
            || options.observer_timeout > Duration::from_secs(30)
            || controller
                .snapshot()
                .workspaces
                .get(&root)
                .is_none_or(|w| w.owner.is_some())
        {
            return Err(AdapterError::Invalid("native runner options/root"));
        }
        std::fs::create_dir_all(&options.output).map_err(|e| AdapterError::Host(e.to_string()))?;
        let event_loop = EventLoop::<NativeEvent>::with_user_event()
            .build()
            .map_err(|e| AdapterError::Host(e.to_string()))?;
        let proxy = event_loop.create_proxy();
        let ctx = egui::Context::default();
        let repaint_proxy = proxy.clone();
        ctx.set_request_repaint_callback(move |request| {
            let _ = repaint_proxy.send_event(NativeEvent::Repaint {
                viewport: request.viewport_id,
                after: request.delay,
                pass: request.current_cumulative_pass_nr,
            });
        });
        let mut setup = egui_wgpu::WgpuSetupCreateNew::default();
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        let configuration = egui_wgpu::WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        };
        let painter = pollster::block_on(egui_wgpu::winit::Painter::new(
            ctx.clone(),
            configuration,
            false,
            Default::default(),
        ));
        let mut plan = Vec::new();
        if let Some(f) = options.probe_frame {
            plan.push(NativeContent::Frame(f));
        }
        if let Some(v) = options.probe_view {
            plan.push(NativeContent::View(v));
        }
        if let Some(g) = options.probe_group {
            plan.push(NativeContent::Group(g));
        }
        let before = os_observation(None);
        let mut app = App {
            controller,
            host,
            root,
            observer_path: options.output.join("native-uia.json"),
            observer_nonce: format!(
                "kumihimo-{}-{}",
                std::process::id(),
                Instant::now().elapsed().as_nanos()
            ),
            options,
            ctx,
            painter,
            proxy,
            windows: HashMap::new(),
            slots: WindowSlots::default(),
            pending: None,
            next_capture: 0,
            capture_bytes: 0,
            captures: vec![],
            observations: vec![],
            plan,
            current: None,
            phase: ProbePhase::Initial,
            started: Instant::now(),
            observer: None,
            observer_started: None,
            observer_result: None,
            accesskit_trace: Vec::new(),
            observer_generation: None,
            observer_frame: None,
            observer_previous: None,
            uia_actions: 0,
            returned: 0,
            error: None,
            before,
        };
        event_loop
            .run_app(&mut app)
            .map_err(|e| AdapterError::Host(e.to_string()))?;
        // Dropping only our own Arc<Window>/Painter surfaces destroys owned HWNDs.
        let handles: Vec<_> = app.windows.values().map(|w| hwnd(&w.window)).collect();
        app.windows.clear();
        app.painter.gc_viewports(&Default::default());
        let after = os_observation(None);
        let cleanup: Vec<_> = handles
            .into_iter()
            .map(|handle| os_observation(Some(handle)))
            .collect();
        let manifest = app.options.output.join("native-manifest.json");
        let value = serde_json::json!({ "schema":"kumihimo.native-observation.v1", "pid":std::process::id(),
            "interactive_opt_in":app.options.interactive, "before":app.before, "during":app.observations, "after":after, "cleanup":cleanup,
            "backend":app.painter.render_state().map(|r|format!("{:?}",r.adapter.get_info().backend)),
            "captures":app.captures, "canonical_generation":app.controller.snapshot().generation,
            "prepared_slots":app.slots.prepared().len(), "returned":app.returned,
            "remaining_native_bindings":app.slots.bindings().len(), "uia":app.observer_result,
            "accesskit_forwarded_actions":app.uia_actions,
            "error":app.error.as_ref().map(ToString::to_string),
            "not_proven":["physical touch/pen", "physical GPU fault", "idle CPU/memory release", "external provider devices", "other OS/ARM"],
            "reproduction":"native_probe --output <directory>; desktop_host --interactive" });
        let bytes =
            crate::diagnostics::encode_bounded(&value, app.host.policy().max_capture_bytes)?;
        std::fs::write(&manifest, bytes).map_err(|e| AdapterError::Host(e.to_string()))?;
        if let Some(error) = app.error {
            return Err(error);
        }
        Ok(NativeOutcome {
            manifest,
            captures: app.captures.len(),
            returned: app.returned,
            uia_actions: app.uia_actions,
        })
    }
}
impl<H: NativeHost> App<H> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: AdapterError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
        if let Some(pending) = self.pending.take() {
            let check = self.slots.cancel(&pending, self.controller.snapshot());
            self.observations.push(serde_json::json!({"phase":"precommit-failure", "source_unchanged":check.is_ok(), "source_frame":pending.source_frame.raw(), "prepared_root":pending.slot.root.raw()}));
        }
        event_loop.exit();
    }
    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        root: WorkspaceId,
        viewport: egui::ViewportId,
    ) -> Result<WindowId> {
        if self.windows.len() >= self.host.policy().max_native_slots.saturating_add(1) {
            return Err(AdapterError::Budget("native windows"));
        }
        let attributes = Window::default_attributes()
            .with_title("Kumihimo demo workspace")
            .with_inner_size(winit::dpi::PhysicalSize::new(1000u32, 720u32))
            .with_visible(false)
            .with_active(false);
        #[cfg(windows)]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes.with_skip_taskbar(!self.options.interactive)
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|e| AdapterError::Host(e.to_string()))?,
        );
        let id = window.id();
        if !self.options.interactive {
            configure_owned_quiet(&window)?;
        }
        self.assert_quiet(&window, "created")?;
        pollster::block_on(self.painter.set_window(viewport, Some(window.clone())))
            .map_err(|e| AdapterError::Host(format!("Painter.set_window: {e}")))?;
        let mut state = egui_winit::State::new(
            self.ctx.clone(),
            viewport,
            event_loop,
            Some(window.scale_factor() as f32),
            window.theme(),
            self.painter.max_texture_side(),
        );
        state.init_accesskit(event_loop, &window, self.proxy.clone());
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let bounds = Rect::new(
            0.0,
            0.0,
            size.width as f64 / scale as f64,
            size.height as f64 / scale as f64,
        )?;
        let surface = SurfaceBinding {
            root,
            viewport,
            epoch: 1,
            bounds,
            pixels_per_point: scale,
        };
        self.windows.insert(
            id,
            NativeWindow {
                window: window.clone(),
                state,
                adapter: Default::default(),
                surface,
                status: SurfaceState::Recreating,
                dirty: true,
                deadline: None,
                capture: None,
                capture_intent: None,
                actual_size: [size.width, size.height],
                actual_scale: scale,
                retries: 0,
                focused: false,
                occluded: None,
                report: Default::default(),
            },
        );
        if self.options.interactive {
            window.set_visible(true);
        }
        Ok(id)
    }
    fn assert_quiet(&mut self, window: &Window, phase: &str) -> Result<()> {
        let observation = os_observation(Some(hwnd(window)));
        if !self.options.interactive
            && (observation["visible"] == true
                || observation["foreground_owned"] == true
                || observation["owned_pid"] != true
                || observation["taskbar_appwindow_style"] == true
                || observation["taskbar_excluded_toolwindow_style"] != true
                || observation["no_activate_style"] != true)
        {
            return Err(AdapterError::Host(format!(
                "native quiet/ownership failure at {phase}: {observation}"
            )));
        }
        if self.observations.len() < 256 {
            self.observations
                .push(serde_json::json!({"phase":phase,"window":observation}));
        }
        Ok(())
    }
    fn request_capture(
        &mut self,
        id: WindowId,
        label: &str,
        pending_nonce: Option<u64>,
    ) -> Result<()> {
        if self.captures.len()
            + self
                .windows
                .values()
                .filter(|w| w.capture.is_some())
                .count()
            >= self.options.max_captures
        {
            return Err(AdapterError::Budget("native captures"));
        }
        let w = self
            .windows
            .get_mut(&id)
            .ok_or(AdapterError::Invalid("capture window"))?;
        if w.capture.is_some() {
            return Err(AdapterError::Budget("one capture per window"));
        }
        w.capture_intent = Some((label.into(), pending_nonce));
        let size = w.window.inner_size();
        if size.width == 0 || size.height == 0 {
            w.status = SurfaceState::Unavailable;
            w.dirty = false;
            return Ok(());
        }
        let bytes = (size.width as usize)
            .checked_mul(size.height as usize)
            .and_then(|v| v.checked_mul(4))
            .ok_or(AdapterError::Budget("capture arithmetic"))?;
        if self
            .capture_bytes
            .checked_add(bytes)
            .is_none_or(|n| n > self.host.policy().max_capture_bytes)
        {
            return Err(AdapterError::Budget("native capture byte budget"));
        }
        self.next_capture = self
            .next_capture
            .checked_add(1)
            .ok_or(AdapterError::Budget("capture nonce"))?;
        w.capture = Some(CaptureTag {
            capture: self.next_capture,
            pending_nonce,
            viewport: w.surface.viewport,
            root: w.surface.root,
            hwnd: hwnd(&w.window),
            epoch: w.surface.epoch,
            size: [size.width, size.height],
            scale: w.window.scale_factor() as f32,
            identity: self.controller.snapshot().identity.clone(),
            generation: self.controller.snapshot().generation,
            label: label.into(),
        });
        w.dirty = true;
        Ok(())
    }
    fn draw(&mut self, id: WindowId) -> Result<()> {
        let window = self
            .windows
            .get(&id)
            .ok_or(AdapterError::Invalid("draw window"))?
            .window
            .clone();
        self.assert_quiet(&window, "draw-before")?;
        let w = self.windows.get_mut(&id).unwrap();
        if matches!(
            w.status,
            SurfaceState::Unavailable | SurfaceState::Lost | SurfaceState::Failed
        ) {
            w.dirty = false;
            return Ok(());
        }
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        if size.width == 0 || size.height == 0 {
            w.status = SurfaceState::Unavailable;
            w.dirty = false;
            return Ok(());
        }
        w.surface.bounds = Rect::new(
            0.0,
            0.0,
            size.width as f64 / scale as f64,
            size.height as f64 / scale as f64,
        )?;
        w.surface.pixels_per_point = scale;
        if let Some(render) = self.painter.render_state() {
            self.host.renderer_binding(
                w.surface,
                crate::content::GpuBinding::new(&render.device, w.surface)?,
            );
        }
        // Set canonical contexts before arming preflight; draw after arming is nonmutating
        // unless the host changes source state, which invalidates the generation receipt.
        let mut input = w.state.take_egui_input(&window);
        input.viewport_id = w.surface.viewport;
        input
            .viewports
            .entry(w.surface.viewport)
            .or_default()
            .native_pixels_per_point = Some(scale);
        input.screen_rect = Some(to_egui(w.surface.bounds));
        let mut result = Ok(FrameReport::default());
        let output = self.ctx.run(input, |ctx| {
            result = w
                .adapter
                .show(ctx, &mut self.controller, w.surface, &mut self.host);
        });
        w.report = result?;
        w.state
            .handle_platform_output(&window, output.platform_output);
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        // Tag captures after actual projection settles, but never refresh pending generation
        // silently: a changed preflight generation must be rearmed and freshly captured.
        if let Some(tag) = &mut w.capture {
            if tag.pending_nonce.is_none() {
                tag.generation = self.controller.snapshot().generation;
            }
        }
        let captures = w
            .capture
            .as_ref()
            .map(|t| vec![egui::UserData::new(t.clone())])
            .unwrap_or_default();
        self.painter.paint_and_update_textures(
            w.surface.viewport,
            output.pixels_per_point,
            [0.025, 0.03, 0.045, 1.0],
            &primitives,
            &output.textures_delta,
            captures,
        );
        w.dirty = w.report.repaint_requested;
        let repaint = output
            .viewport_output
            .get(&w.surface.viewport)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        w.deadline = if repaint == Duration::MAX {
            None
        } else {
            Instant::now().checked_add(repaint)
        };
        self.assert_quiet(&window, "draw-after")
    }
    fn receive_captures(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if let Some(render) = self.painter.render_state() {
            render
                .device
                .poll(wgpu::PollType::Poll)
                .map_err(|e| AdapterError::Host(e.to_string()))?;
        }
        let mut events = Vec::new();
        self.painter.handle_screenshots(&mut events);
        for event in events {
            let egui::Event::Screenshot {
                viewport_id,
                user_data,
                image,
            } = event
            else {
                continue;
            };
            let Some(tag) = user_data
                .data
                .as_ref()
                .and_then(|d| d.downcast_ref::<CaptureTag>())
                .cloned()
            else {
                continue;
            };
            let Some(id) = self
                .windows
                .iter()
                .find_map(|(id, w)| (w.surface.viewport == viewport_id).then_some(*id))
            else {
                continue;
            };
            let window = self.windows[&id].window.clone();
            self.assert_quiet(&window, "screenshot-readback")?;
            let w = self.windows.get_mut(&id).unwrap();
            if w.capture.as_ref() != Some(&tag)
                || tag.viewport != viewport_id
                || tag.hwnd != hwnd(&window)
                || tag.epoch != w.surface.epoch
                || tag.size != [window.inner_size().width, window.inner_size().height]
                || tag.scale != window.scale_factor() as f32
                || image.size != [tag.size[0] as usize, tag.size[1] as usize]
                || image.pixels.is_empty()
                || tag.identity != self.controller.snapshot().identity
            {
                self.observations.push(
                    serde_json::json!({"phase":"stale-screenshot-rejected","capture":tag.capture}),
                );
                continue;
            }
            let bytes = image
                .pixels
                .len()
                .checked_mul(4)
                .ok_or(AdapterError::Budget("image bytes"))?;
            self.capture_bytes = self
                .capture_bytes
                .checked_add(bytes)
                .ok_or(AdapterError::Budget("capture bytes"))?;
            if self.capture_bytes > self.host.policy().max_capture_bytes {
                return Err(AdapterError::Budget("capture total bytes"));
            }
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let file = format!("native-{:02}-{}.png", tag.capture, tag.label);
            image::save_buffer(
                self.options.output.join(&file),
                &rgba,
                tag.size[0],
                tag.size[1],
                image::ColorType::Rgba8,
            )
            .map_err(|e| AdapterError::Host(e.to_string()))?;
            self.captures.push(serde_json::json!({"file":file,"nonce":tag.capture,"pending_nonce":tag.pending_nonce,
                "root":tag.root.raw(),"hwnd":tag.hwnd,"epoch":tag.epoch,"physical_size":tag.size,"dpi_scale":tag.scale,
                "core_generation":tag.generation,"readback_bytes":bytes,"pixels_nonuniform":image.pixels.iter().any(|p|p!=&image.pixels[0]),
                "report":crate::diagnostics::report_json(&w.report)}));
            w.capture = None;
            w.capture_intent = None;
            w.status = SurfaceState::Ready;
            if let Some(pending_nonce) = tag.pending_nonce {
                let pending = self
                    .pending
                    .take()
                    .ok_or(AdapterError::Stale("missing pending native"))?;
                if pending.nonce != pending_nonce {
                    return Err(AdapterError::Stale("pending native nonce"));
                }
                // The constructor exists only here, behind Painter readback and live HWND checks.
                let ready = Ready {
                    scale: tag.scale,
                    bytes,
                    tag,
                };
                let binding =
                    self.slots
                        .commit(pending, ready, &mut self.controller, &mut self.host)?;
                self.observations.push(serde_json::json!({"phase":"committed-canonical-recipient","root":binding.root.raw(),"generation":self.controller.snapshot().generation}));
                let session = crate::persistence::DesktopSession::capture(
                    self.controller.snapshot(),
                    self.host.policy().max_native_slots,
                    self.slots.bindings().to_vec(),
                )?;
                let session_bytes = session.to_json(self.host.policy().max_capture_bytes)?;
                let decoded = crate::persistence::DesktopSession::from_json(
                    &session_bytes,
                    self.host.policy().max_capture_bytes,
                )?;
                decoded.validate_current(self.controller.snapshot())?;
                if decoded != session {
                    return Err(AdapterError::Invalid("native session DTO roundtrip"));
                }
                let stamp = self.controller.core().save_stamp();
                let layout = self.controller.core().save_layout();
                if !self.controller.core().is_save_current(&stamp)
                    || layout.source != session.source
                    || layout.source_generation != session.source_generation
                {
                    return Err(AdapterError::Stale("native session/schema1 source stamp"));
                }
                // Host sample write, deliberately no atomic-storage or filesystem-safety claim.
                std::fs::write(
                    self.options.output.join("native-session.json"),
                    &session_bytes,
                )
                .map_err(|e| AdapterError::Host(e.to_string()))?;
                self.observations.push(serde_json::json!({"phase":"committed-session-boundary","native_root":binding.root.raw(),"session_version":session.version,"source_generation":session.source_generation,"saved_layout_schema":layout.schema,"save_stamp_current":true,"dto_roundtrip":true,"atomic_storage_proven":false}));
                self.request_capture(id, "populated", None)?;
                self.phase = ProbePhase::Populated;
            } else if self.options.probe {
                self.advance_probe(event_loop, id)?;
            }
        }
        Ok(())
    }
    fn start_move(&mut self, event_loop: &ActiveEventLoop, content: NativeContent) -> Result<()> {
        if self.pending.is_some() || self.slots.bindings().len() >= self.host.policy().max_pending {
            return Err(AdapterError::Budget("native pending operation"));
        }
        let mut pending = self
            .slots
            .prepare(&mut self.controller, &mut self.host, content)?;
        // Explicit cancel path checks original source after preparation without deleting Core slots.
        self.slots.cancel(&pending, self.controller.snapshot())?;
        let viewport =
            egui::ViewportId::from_hash_of(("native-root", pending.slot.root.raw(), pending.nonce));
        let id = self.create_window(event_loop, pending.slot.root, viewport)?;
        let bounds = self.windows[&id].surface.bounds;
        self.slots
            .arm(&mut pending, &mut self.controller, &mut self.host, bounds)?;
        self.current = Some(id);
        self.pending = Some(pending);
        self.phase = ProbePhase::Moving;
        self.request_capture(id, "preflight", Some(self.pending.as_ref().unwrap().nonce))
    }
    fn advance_probe(&mut self, event_loop: &ActiveEventLoop, id: WindowId) -> Result<()> {
        match self.phase {
            ProbePhase::Initial => {
                self.start_observer(id)?;
                self.phase = ProbePhase::ObserveInitial;
            }
            ProbePhase::Populated => {
                // Request a real owned HWND resize; only Resized/current live size advances proof.
                let _ = self.windows[&id]
                    .window
                    .request_inner_size(winit::dpi::PhysicalSize::new(760u32, 520u32));
                self.phase = ProbePhase::Resizing;
                // Windows can return synchronously with no later event.
                if self.windows[&id].window.inner_size() == winit::dpi::PhysicalSize::new(760, 520)
                {
                    self.resize(id)?;
                    self.request_capture(id, "resized", None)?;
                }
            }
            ProbePhase::Resizing => {
                self.recreate(id)?;
                self.phase = ProbePhase::Recreated;
                self.request_capture(id, "recreated", None)?;
            }
            ProbePhase::Recreated
                if self.options.probe_partial_close
                    && self.slots.bindings().iter().any(|b| {
                        b.root == self.windows[&id].surface.root
                            && matches!(b.content, NativeContent::Frame(_))
                    }) =>
            {
                let root = self.windows[&id].surface.root;
                let result =
                    self.slots
                        .close_content(root, true, &mut self.controller, &mut self.host)?;
                let closed_proven = result.closed.iter().all(|view| {
                    self.controller
                        .snapshot()
                        .views
                        .get(view)
                        .is_some_and(|v| v.closed.is_some())
                });
                let held_proven = result.remaining.iter().all(|view| {
                    self.controller.snapshot().views.get(view).is_some_and(|v| {
                        v.closed.is_none()
                            && self.controller.snapshot().frames[&v.frame].workspace == root
                    })
                });
                let handle = hwnd(&self.windows[&id].window);
                self.observations.push(serde_json::json!({"phase":"partial-close-canonical-read-held-window","root":root.raw(),"closed":result.closed.iter().map(|v|v.raw()).collect::<Vec<_>>(),"remaining":result.remaining.iter().map(|v|v.raw()).collect::<Vec<_>>(),"rejected":result.rejected,"closed_proven":closed_proven,"remaining_proven":held_proven,"owned_window":os_observation(Some(handle))}));
                if result.closed.is_empty()
                    || result.remaining.is_empty()
                    || !closed_proven
                    || !held_proven
                {
                    return Err(AdapterError::Host(
                        "partial-close sample did not retain both closed and vetoed outcomes"
                            .into(),
                    ));
                }
                self.phase = ProbePhase::HeldSurvivors;
                self.request_capture(id, "partial-close-survivors", None)?;
            }
            ProbePhase::Recreated | ProbePhase::HeldSurvivors => {
                let root = self.windows[&id].surface.root;
                let before = self.controller.snapshot().generation;
                let fallback_used =
                    self.slots
                        .return_content(root, None, &mut self.controller, &mut self.host)?;
                self.observations.push(serde_json::json!({"phase":"returned","root":root.raw(),"fallback_used":fallback_used,"before_generation":before,"after_generation":self.controller.snapshot().generation}));
                self.returned += 1;
                self.remove_window(id)?;
                if let Some(content) = self.plan.pop() {
                    self.start_move(event_loop, content)?;
                } else {
                    self.phase = ProbePhase::Observer;
                    self.current = None;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn resize(&mut self, id: WindowId) -> Result<()> {
        let w = self
            .windows
            .get(&id)
            .ok_or(AdapterError::Invalid("resize window"))?;
        let size = w.window.inner_size();
        let actual_size = [size.width, size.height];
        let scale = w.window.scale_factor() as f32;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(AdapterError::Invalid("actual native resize scale"));
        }
        // Compare separately recorded OS measurements, not bounds last updated by draw.
        if actual_size == w.actual_size && scale == w.actual_scale {
            return Ok(());
        }
        let intent = w.capture_intent.clone();
        if let Some((_, Some(nonce))) = &intent {
            let pending = self
                .pending
                .as_ref()
                .ok_or(AdapterError::Stale("resize missing pending native"))?;
            let snapshot = self.controller.snapshot();
            // Reject prior unrelated drift BEFORE changing owned canonical geometry.
            if self.current != Some(id)
                || pending.nonce != *nonce
                || pending.slot.root != w.surface.root
                || pending.identity != snapshot.identity
                || pending.generation != snapshot.generation
            {
                return Err(AdapterError::Stale(
                    "resize pending identity/generation/window/nonce",
                ));
            }
            self.slots.cancel(pending, snapshot)?;
            crate::window_slots::eligible(snapshot, &pending.slot)?;
        }
        let bounds = Rect::new(
            0.0,
            0.0,
            size.width as f64 / scale as f64,
            size.height as f64 / scale as f64,
        )?;
        let w = self.windows.get_mut(&id).unwrap();
        w.surface.epoch = w
            .surface
            .epoch
            .checked_add(1)
            .ok_or(AdapterError::Budget("surface epoch"))?;
        w.capture = None;
        w.actual_size = actual_size;
        w.actual_scale = scale;
        w.surface.bounds = bounds;
        w.surface.pixels_per_point = scale;
        w.adapter.input.cancel("surface epoch changed");
        if let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            self.painter
                .on_window_resized(w.surface.viewport, width, height);
            w.status = SurfaceState::Recreating;
            w.dirty = true;
        } else {
            w.status = SurfaceState::Unavailable;
            w.dirty = false;
        }
        self.observations.push(serde_json::json!({"phase":"owned-resize-current-dpi","root":w.surface.root.raw(),"size":[size.width,size.height],"scale":w.window.scale_factor(),"epoch":w.surface.epoch}));
        if !actual_size.contains(&0) {
            if let Some((label, pending_nonce)) = intent {
                if pending_nonce.is_some() {
                    // The old generation was checked above; only arm may advance it for
                    // freshly prepared owned context. Old screenshot tags remain invalid.
                    let pending = self
                        .pending
                        .as_mut()
                        .ok_or(AdapterError::Stale("resize pending native"))?;
                    self.slots
                        .arm(pending, &mut self.controller, &mut self.host, bounds)?;
                }
                self.request_capture(id, &label, pending_nonce)?;
            }
        }
        Ok(())
    }
    fn recreate(&mut self, id: WindowId) -> Result<()> {
        let w = self
            .windows
            .get_mut(&id)
            .ok_or(AdapterError::Invalid("recreate window"))?;
        if w.retries >= self.options.max_retries {
            w.status = SurfaceState::Failed;
            return Err(AdapterError::Budget("surface recreation retries"));
        }
        let canonical = self.controller.snapshot().clone();
        w.retries += 1;
        w.status = SurfaceState::Lost;
        w.surface.epoch = w
            .surface
            .epoch
            .checked_add(1)
            .ok_or(AdapterError::Budget("surface epoch"))?;
        w.capture = None;
        // Painter.set_window(None) clears ALL surfaces; gc_viewports removes only this owned surface.
        let keep = self
            .windows
            .iter()
            .filter_map(|(key, w)| (*key != id).then_some(w.surface.viewport))
            .collect();
        self.painter.gc_viewports(&keep);
        let w = self.windows.get_mut(&id).unwrap();
        w.status = SurfaceState::Recreating;
        pollster::block_on(
            self.painter
                .set_window(w.surface.viewport, Some(w.window.clone())),
        )
        .map_err(|e| AdapterError::Host(format!("recreate set_window: {e}")))?;
        w.dirty = true;
        if self.controller.snapshot() != &canonical {
            return Err(AdapterError::Invalid(
                "surface teardown changed canonical state",
            ));
        }
        self.observations.push(serde_json::json!({"phase":"actual-owned-surface-teardown-recreation","epoch":w.surface.epoch,"canonical_unchanged":true,"physical_gpu_fault":false}));
        Ok(())
    }
    fn remove_window(&mut self, id: WindowId) -> Result<()> {
        let Some(w) = self.windows.remove(&id) else {
            return Ok(());
        };
        let handle = hwnd(&w.window);
        let keep = self.windows.values().map(|w| w.surface.viewport).collect();
        self.painter.gc_viewports(&keep);
        drop(w);
        self.observations.push(serde_json::json!({"phase":"owned-window-cleanup","window":os_observation(Some(handle))}));
        Ok(())
    }
    fn start_observer(&mut self, id: WindowId) -> Result<()> {
        let Some(script) = &self.options.observer_script else {
            return Ok(());
        };
        let handle = hwnd(&self.windows[&id].window);
        let target = self
            .options
            .observer_target
            .ok_or(AdapterError::Invalid("UIA observer target required"))?;
        let view = self
            .controller
            .snapshot()
            .views
            .get(&target)
            .ok_or(AdapterError::Invalid("UIA target view"))?;
        self.observer_frame = Some(view.frame);
        self.observer_previous = self.controller.snapshot().frames[&view.frame].selected_view;
        self.observer_generation = Some(self.controller.snapshot().generation);
        if self.observer_previous == Some(target) {
            return Err(AdapterError::Invalid(
                "UIA proof target must initially be unselected",
            ));
        }
        let control = self.windows[&id]
            .report
            .controls
            .iter()
            .find(|c| c.id == format!("view/{}/tab", target.raw()))
            .ok_or(AdapterError::Invalid("UIA target control not projected"))?;
        let request = serde_json::json!({"hwnd":handle,"pid":std::process::id(),"nonce":self.observer_nonce,"output":self.observer_path,"target_view":target.raw(),"target_label":control.label,"target_control_id":control.id});
        std::fs::write(
            self.options.output.join("native-uia-request.json"),
            serde_json::to_vec_pretty(&request).map_err(|e| AdapterError::Host(e.to_string()))?,
        )
        .map_err(|e| AdapterError::Host(e.to_string()))?;
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg("-Hwnd")
            .arg(handle.to_string())
            .arg("-ExpectedPid")
            .arg(std::process::id().to_string())
            .arg("-Nonce")
            .arg(&self.observer_nonce)
            .arg("-Output")
            .arg(&self.observer_path)
            .arg("-TargetView")
            .arg(target.raw().to_string())
            .arg("-TargetLabel")
            .arg(&control.label);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        self.observer = Some(
            command
                .spawn()
                .map_err(|e| AdapterError::Host(format!("independent UIA observer: {e}")))?,
        );
        self.observer_started = Some(Instant::now());
        Ok(())
    }
    fn poll_observer(&mut self) -> Result<()> {
        let Some(child) = &mut self.observer else {
            return Ok(());
        };
        if let Some(status) = child
            .try_wait()
            .map_err(|e| AdapterError::Host(e.to_string()))?
        {
            if self.observer_path.exists() {
                let bytes = std::fs::read(&self.observer_path)
                    .map_err(|e| AdapterError::Host(e.to_string()))?;
                if bytes.len() > 1024 * 1024 {
                    return Err(AdapterError::Budget("UIA observer result"));
                }
                let result: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|e| AdapterError::Host(e.to_string()))?;
                if result["nonce"] != self.observer_nonce || result["pid"] != std::process::id() {
                    return Err(AdapterError::Stale("UIA observer identity"));
                }
                self.observer_result = Some(result);
            }
            self.observer = None;
            if !status.success() {
                return Err(AdapterError::Host(format!("UIA observer exited {status}")));
            }
        } else if self
            .observer_started
            .is_some_and(|t| t.elapsed() > self.options.observer_timeout)
        {
            // This helper was started by this session; bounded cleanup may kill only it.
            child
                .kill()
                .map_err(|e| AdapterError::Host(e.to_string()))?;
            child
                .wait()
                .map_err(|e| AdapterError::Host(e.to_string()))?;
            self.observer = None;
            return Err(AdapterError::Host("UIA observer deadline".into()));
        }
        Ok(())
    }
}
impl<H: NativeHost> ApplicationHandler<NativeEvent> for App<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            let ids: Vec<_> = self
                .windows
                .iter()
                .filter_map(|(id, w)| (w.status == SurfaceState::Unavailable).then_some(*id))
                .collect();
            for id in ids {
                if let Err(e) = self.recreate(id) {
                    self.fail(event_loop, e);
                    return;
                }
            }
            return;
        }
        let result = (|| {
            let id = self.create_window(event_loop, self.root, egui::ViewportId::ROOT)?;
            self.request_capture(id, "root", None)
        })();
        if let Err(e) = result {
            self.fail(event_loop, e);
        }
    }
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.painter.gc_viewports(&Default::default());
        for w in self.windows.values_mut() {
            w.status = SurfaceState::Unavailable;
            w.capture = None;
            w.dirty = false;
        }
        self.observations
            .push(serde_json::json!({"phase":"os-suspend","source_ownership_unchanged":true}));
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: NativeEvent) {
        match event {
            NativeEvent::Accessibility(event) => {
                if let Some(w) = self.windows.get_mut(&event.window_id) {
                    use egui_winit::accesskit_winit::WindowEvent as AccessEvent;
                    match event.window_event {
                        AccessEvent::ActionRequested(request) => {
                            // Durable single-writer evidence precedes egui queueing, even
                            // when quiet failure prevents the next draw or final report.
                            let arrival = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|elapsed| elapsed.as_millis().to_string());
                            let checkpoint = serde_json::json!({
                                "phase":"action-requested-before-egui-queue",
                                "sequence":self.uia_actions.saturating_add(1),
                                "timestamp_unix_millis":arrival.ok(),
                                "arrival_elapsed_micros":self.started.elapsed().as_micros().to_string(),
                                "action_kind":format!("{:?}",request.action),
                                "action_type":"accesskit::ActionRequest",
                                "target_node":request.target.0.to_string(),
                                "expected_target_view":self.options.observer_target.map(|view|view.raw().to_string()),
                                "window_id":format!("{:?}",event.window_id),
                                "surface_epoch":w.surface.epoch.to_string(),
                                "core_generation":self.controller.snapshot().generation.to_string(),
                                "owned_window":os_observation(Some(hwnd(&w.window)))
                            });
                            let trace = write_accesskit_checkpoint(
                                &self.options.output,
                                &self.observer_nonce,
                                &mut self.accesskit_trace,
                                checkpoint,
                                self.host.policy().max_capture_bytes.min(1024 * 1024),
                            );
                            if let Err(error) = trace {
                                self.fail(event_loop, error);
                                return;
                            }
                            w.state.on_accesskit_action_request(request);
                            self.uia_actions += 1;
                            w.dirty = true;
                        }
                        AccessEvent::InitialTreeRequested => {
                            self.ctx.enable_accesskit();
                            w.dirty = true;
                        }
                        AccessEvent::AccessibilityDeactivated => {}
                    }
                }
            }
            NativeEvent::Repaint {
                viewport,
                after,
                pass,
            } => {
                if self.ctx.cumulative_pass_nr_for(viewport) <= pass.saturating_add(1) {
                    if let Some(w) = self
                        .windows
                        .values_mut()
                        .find(|w| w.surface.viewport == viewport)
                    {
                        if after.is_zero() {
                            w.dirty = true;
                        } else if let Some(deadline) = Instant::now().checked_add(after) {
                            w.deadline = Some(
                                w.deadline
                                    .map_or(deadline, |existing| existing.min(deadline)),
                            );
                        }
                    }
                }
            }
        }
        if let Err(e) = self.receive_captures(event_loop) {
            self.fail(event_loop, e);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        let response = w.state.on_window_event(&w.window, &event);
        if response.repaint {
            w.dirty = true;
        }
        match event {
            WindowEvent::CloseRequested => {
                let root = w.surface.root;
                if root == self.root {
                    if self.slots.bindings().is_empty() {
                        event_loop.exit();
                    }
                } else {
                    let fallback = self
                        .slots
                        .bindings()
                        .iter()
                        .find(|b| b.root == root)
                        .and_then(|binding| self.host.return_fallback(&self.controller, binding));
                    let result=match self.host.close_policy(root) {
                        NativeClosePolicy::Return => self.slots.return_content(root,fallback,&mut self.controller,&mut self.host).and_then(|_|self.remove_window(id)),
                        NativeClosePolicy::CloseContent { host_approved } => self.slots.close_content(root,host_approved,&mut self.controller,&mut self.host).and_then(|result| {
                            self.observations.push(serde_json::json!({"phase":"per-view-close","closed":result.closed.iter().map(|v|v.raw()).collect::<Vec<_>>(),"remaining":result.remaining.iter().map(|v|v.raw()).collect::<Vec<_>>(),"rejected":result.rejected}));
                            if result.remaining.is_empty() { self.remove_window(id) } else { Ok(()) }
                        }),
                    };
                    if let Err(error) = result {
                        self.observations.push(serde_json::json!({"phase":"close-return-rejected-window-retained","cause":error.to_string()}));
                    }
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                let result = self.resize(id).and_then(|_| {
                    if matches!(self.phase, ProbePhase::Resizing)
                        && self.current == Some(id)
                        && self.windows[&id].capture.is_none()
                        && self.windows[&id].window.inner_size()
                            == winit::dpi::PhysicalSize::new(760, 520)
                    {
                        self.request_capture(id, "resized", None)?;
                    }
                    Ok(())
                });
                if let Err(e) = result {
                    self.fail(event_loop, e);
                }
            }
            WindowEvent::Focused(focused) => {
                w.focused = focused;
                if !focused {
                    w.adapter.input.cancel("native focus lost");
                }
            }
            WindowEvent::Occluded(occluded) => {
                w.occluded = Some(occluded);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw(id) {
                    self.fail(event_loop, e);
                }
            }
            _ => {}
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let result = (|| {
            let quiet_cutoff = if self.options.interactive {
                None
            } else {
                Some(
                    self.started
                        .checked_add(self.options.timeout)
                        .ok_or(AdapterError::Budget("native quiet deadline"))?,
                )
            };
            if quiet_cutoff.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(AdapterError::Host("bounded native probe deadline".into()));
            }
            self.receive_captures(event_loop)?;
            self.poll_observer()?;
            let now = Instant::now();
            let ids: Vec<_> = self
                .windows
                .iter_mut()
                .filter_map(|(id, w)| {
                    if w.deadline.is_some_and(|d| d <= now) {
                        w.dirty = true;
                        w.deadline = None;
                    }
                    w.dirty.then_some(*id)
                })
                .collect();
            for id in ids {
                if self.pending.is_none() || self.current == Some(id) {
                    self.draw(id)?;
                }
            }
            if matches!(self.phase, ProbePhase::ObserveInitial) && self.observer.is_none() {
                if self.options.observer_script.is_some() {
                    let target = self
                        .options
                        .observer_target
                        .ok_or(AdapterError::Invalid("observer target"))?;
                    let frame = self
                        .observer_frame
                        .ok_or(AdapterError::Invalid("observer frame"))?;
                    let actual = self
                        .controller
                        .snapshot()
                        .frames
                        .get(&frame)
                        .and_then(|f| f.selected_view);
                    let changed = actual == Some(target)
                        && self.controller.snapshot().generation
                            > self.observer_generation.unwrap_or(u64::MAX)
                        && self.uia_actions > 0;
                    self.observations.push(serde_json::json!({"phase":"independent-uia-canonical-selection-read","target_view":target.raw(),"target_frame":frame.raw(),"before_selected":self.observer_previous.map(|v|v.raw()),"after_selected":actual.map(|v|v.raw()),"before_generation":self.observer_generation,"after_generation":self.controller.snapshot().generation,"matched":changed}));
                    if !changed {
                        return Err(AdapterError::Host(
                            "UIA action did not select exact canonical target".into(),
                        ));
                    }
                }
                if let Some(content) = self.plan.pop() {
                    self.start_move(event_loop, content)?;
                } else {
                    self.phase = ProbePhase::Observer;
                }
            }
            // Shared host queues are drained by the runner after adapter draw, never by a second Controller.
            let actions = self.host.take_native_actions();
            if actions.len() > self.host.policy().max_pending {
                return Err(AdapterError::Budget("host native action batch"));
            }
            for action in actions {
                let result = match action {
                    AdapterAction::PopoutFrame(frame) => {
                        self.start_move(event_loop, NativeContent::Frame(frame))
                    }
                    AdapterAction::PopoutView(view) => {
                        self.start_move(event_loop, NativeContent::View(view))
                    }
                    AdapterAction::PopoutGroup(group) => {
                        self.start_move(event_loop, NativeContent::Group(group))
                    }
                    AdapterAction::ReturnNative(root) => {
                        let fallback = self
                            .slots
                            .bindings()
                            .iter()
                            .find(|b| b.root == root)
                            .and_then(|b| self.host.return_fallback(&self.controller, b));
                        self.slots
                            .return_content(root, fallback, &mut self.controller, &mut self.host)
                            .and_then(|_| {
                                if let Some(id) = self
                                    .windows
                                    .iter()
                                    .find_map(|(id, w)| (w.surface.root == root).then_some(*id))
                                {
                                    self.remove_window(id)?;
                                }
                                Ok(())
                            })
                    }
                    _ => Err(AdapterError::Invalid("non-native action in native queue")),
                };
                if let Err(error) = result {
                    self.observations.push(serde_json::json!({"phase":"native-action-rejected","cause":error.to_string()}));
                }
            }
            if matches!(self.phase, ProbePhase::Observer) && self.observer.is_none() {
                self.phase = ProbePhase::Finished;
                if !self.options.interactive {
                    event_loop.exit();
                }
            }
            let io_pending =
                self.windows.values().any(|w| w.capture.is_some()) || self.observer.is_some();
            let poll_deadline = io_pending.then(|| Instant::now() + Duration::from_millis(16));
            // Quiet cutoff is unconditional: pending/zero/probe phases cannot strand it
            // behind Wait after losing their last capture or observer wake source.
            let next = self
                .windows
                .values()
                .filter_map(|w| w.deadline)
                .chain(poll_deadline)
                .chain(quiet_cutoff)
                .min();
            if let Some(deadline) = next {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.fail(event_loop, e);
        }
    }
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut child) = self.observer.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn write_accesskit_checkpoint(
    output: &std::path::Path,
    nonce: &str,
    checkpoints: &mut Vec<serde_json::Value>,
    checkpoint: serde_json::Value,
    max_bytes: usize,
) -> Result<()> {
    use std::io::Write;
    // Independent of the ordinary observation list; retain at most 128 latest arrivals.
    if checkpoints.len() >= 128 {
        checkpoints.remove(0);
    }
    checkpoints.push(checkpoint);
    let value = serde_json::json!({
        "schema":"kumihimo.accesskit-action-trace.v1",
        "writer":"native-runner", "nonce":nonce,
        "expected_owned_pid":std::process::id(),
        "checkpoints":checkpoints,
        "scope":"Read-only owned HWND/foreground observations before egui action queueing"
    });
    let bytes = crate::diagnostics::encode_bounded(&value, max_bytes)?;
    let mut file = std::fs::File::create(output.join("native-accesskit.trace.json"))
        .map_err(|error| AdapterError::Host(format!("AccessKit trace create: {error}")))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| AdapterError::Host(format!("AccessKit trace persist: {error}")))
}
fn hwnd(window: &Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}
fn configure_owned_quiet(window: &Window) -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{GetLastError, SetLastError},
            System::Threading::GetCurrentProcessId,
            UI::WindowsAndMessaging::{
                GetWindowLongPtrW, GetWindowThreadProcessId, SetWindowLongPtrW, GWL_EXSTYLE,
                WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            },
        };
        let handle = hwnd(window) as windows_sys::Win32::Foundation::HWND;
        // Mutation is limited to a freshly created window owned by this runner.
        unsafe {
            let mut owner = 0;
            GetWindowThreadProcessId(handle, &mut owner);
            if owner != GetCurrentProcessId() {
                return Err(AdapterError::Invalid("quiet style foreign HWND"));
            }
            let style = GetWindowLongPtrW(handle, GWL_EXSTYLE);
            SetLastError(0);
            let previous = SetWindowLongPtrW(
                handle,
                GWL_EXSTYLE,
                (style & !(WS_EX_APPWINDOW as isize))
                    | WS_EX_NOACTIVATE as isize
                    | WS_EX_TOOLWINDOW as isize,
            );
            if previous == 0 && GetLastError() != 0 {
                return Err(AdapterError::Host("owned quiet window style failed".into()));
            }
        }
    }
    Ok(())
}
fn os_observation(handle: Option<isize>) -> serde_json::Value {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            System::Threading::GetCurrentProcessId,
            UI::WindowsAndMessaging::{
                GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow,
                IsWindowVisible, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            },
        };
        // Read-only OS observations, restricted to foreground metadata and our target HWND.
        unsafe {
            let foreground = GetForegroundWindow();
            let mut foreground_pid = 0;
            if !foreground.is_null() {
                GetWindowThreadProcessId(foreground, &mut foreground_pid);
            }
            let pid = GetCurrentProcessId();
            if let Some(raw) = handle {
                let window = raw as windows_sys::Win32::Foundation::HWND;
                let mut owner = 0;
                if IsWindow(window) != 0 {
                    GetWindowThreadProcessId(window, &mut owner);
                }
                serde_json::json!({"hwnd":raw,"exists":IsWindow(window)!=0,"visible":IsWindowVisible(window)!=0,
                    "pid":owner,"owned_pid":owner==pid,"foreground":foreground as isize,"foreground_pid":foreground_pid,
                    "foreground_owned":foreground_pid==pid,"taskbar_appwindow_style":GetWindowLongPtrW(window,GWL_EXSTYLE)&WS_EX_APPWINDOW as isize!=0,
                    "no_activate_style":GetWindowLongPtrW(window,GWL_EXSTYLE)&WS_EX_NOACTIVATE as isize!=0,
                    "taskbar_excluded_toolwindow_style":GetWindowLongPtrW(window,GWL_EXSTYLE)&WS_EX_TOOLWINDOW as isize!=0})
            } else {
                serde_json::json!({"foreground":foreground as isize,"foreground_pid":foreground_pid,"foreground_owned":foreground_pid==pid})
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        serde_json::json!({"observation":"Unknown: unsupported OS"})
    }
}
