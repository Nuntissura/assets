//! The window path: winit (this crate only) for the window and event loop, a typed [`HostEvent`] stream past it, the
//! Vulkan presenter for the frame. `quiet` never takes focus or the foreground (HBR-QUIET): created hidden and
//! inactive, WS_EX_NOACTIVATE and WS_EX_TOOLWINDOW set before it is shown, shown with SW_SHOWNOACTIVATE at the bottom
//! of the Z order and placed left of every monitor, minimised and restored with the no-activate show commands.
use crate::capture::{Json, device_json, layers_json, sha256_hex, write_png, write_receipt};
use crate::error::HostError;
use crate::event::{EventLog, HostEvent, from_winit};
use crate::offscreen::document_json;
use crate::scene::{BACKGROUND, CHROME, Content, DocumentFacts, GuiArgs, Scene, content};
use crate::win32;
use hsk_studio_vulkan::{
    Capture, FrameOutcome, GpuClientPort, Owner, OwnerConfig, PaneId, PresentCounters, Presenter, SWAPCHAIN_FORMAT,
    WindowHandles, WindowSignal,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::{EventLoopBuilderExtWindows, WindowAttributesExtWindows};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

/// Largest frame count a run may ask for.
pub const MAX_FRAMES: u32 = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowArgs {
    pub width: u32,
    pub height: u32,
    pub scene: Scene,
    /// Never focus, never foreground, off every monitor (agents and tests). Off: the Operator's own window.
    pub quiet: bool,
    /// Frames to present before exit; 0 = until the window is closed (not allowed with `quiet`).
    pub frames: u32,
    /// Scripted resize, minimise and restore before the final frame (quiet only).
    pub exercise: bool,
    pub capture: Option<PathBuf>,
    pub receipt: Option<PathBuf>,
    pub allow_overlays: bool,
    /// Wall-clock bound for the whole run.
    pub deadline: Duration,
    /// Scene `gui` only.
    pub gui: GuiArgs,
}

/// What a window run observed about focus. `focus_changes` counts only changes this process caused: focus-gained
/// events on its window plus samples where one of its windows became the foreground. A watcher thread samples
/// `GetForegroundWindow` about every millisecond from before the window exists until after the swapchain is gone.
/// The Operator switching between their own windows is counted in `foreground_changes`, never in `focus_changes`.
/// The style fields are read from the live window at the end of the run, not before its first show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusReport {
    pub focus_changes: u64,
    pub focus_gained_events: u64,
    pub foreground_takes: u64,
    pub foreground_changes: u64,
    pub samples: u64,
    pub no_activate_style: bool,
    pub tool_window_style: bool,
    /// WS_EX_APPWINDOW (a taskbar button) at the end of the run; false for a quiet run.
    pub app_window_style: bool,
    /// Times the quiet style had to be put back after winit rewrote the extended style.
    pub style_restores: u64,
}

/// Foreground watcher: samples until stopped and returns (samples, foreground changes, takes by this process).
struct Watch {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<(u64, u64, u64)>,
}

impl Watch {
    fn spawn() -> Result<Self, HostError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("hsk-foreground-watch".into())
            .spawn(move || {
                let (mut last, mut owned_last) = win32::foreground();
                let (mut samples, mut changes, mut takes) = (1u64, 0u64, u64::from(owned_last));
                while !flag.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                    let (fg, owned) = win32::foreground();
                    samples = samples.saturating_add(1);
                    if fg != last {
                        changes = changes.saturating_add(1);
                        last = fg;
                    }
                    if owned && !owned_last {
                        takes = takes.saturating_add(1);
                    }
                    owned_last = owned;
                }
                (samples, changes, takes)
            })
            .map_err(|_| HostError::Window("watch"))?;
        Ok(Self { stop, handle })
    }
    fn finish(self) -> (u64, u64, u64) {
        self.stop.store(true, Ordering::Release);
        // A panicked watcher proves nothing: report one take so a quiet run fails rather than passes.
        self.handle.join().unwrap_or((0, 0, 1))
    }
}

#[derive(Clone, Debug)]
pub struct WindowResult {
    pub capture: Option<Capture>,
    pub frame_sha256: Option<String>,
    pub focus: FocusReport,
    pub counters: PresentCounters,
    pub final_state: &'static str,
    /// `from:signal>to` per state change.
    pub transitions: Vec<String>,
    pub events: Vec<HostEvent>,
    pub pane_extent: (u32, u32),
    pub swap_extent: (u32, u32),
    pub scale_factor_milli: u32,
    pub process_dpi_aware: bool,
    pub exercised: Vec<&'static str>,
    pub document: Option<DocumentFacts>,
    pub receipt: Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Warm,
    Grown,
    Minimized,
    Restored,
    Back,
    Final,
    Done,
}

struct Live {
    owner: Owner,
    pane: PaneId,
    presenter: Presenter,
}

struct App {
    args: WindowArgs,
    content: Option<Content>,
    document: Option<DocumentFacts>,
    window: Option<Window>,
    hwnd: isize,
    live: Option<Live>,
    error: Option<HostError>,
    events: EventLog,
    focus: FocusReport,
    watch: Option<Watch>,
    start: Instant,
    iterations: u64,
    presented: u32,
    phase: Phase,
    phase_frames: u32,
    phase_iterations: u32,
    exercised: Vec<&'static str>,
    /// Physical client size the run wants; a DPI change keeps it (pixels map 1:1 to the pane).
    target: (u32, u32),
    capture: Option<Capture>,
    scale_milli: u32,
    report: Option<(PresentCounters, &'static str, Vec<String>, (u32, u32))>,
    device: Option<Json>,
    layers: Option<Json>,
    validation: &'static str,
    /// Milliseconds from the event loop to a created window, an opened owner and a ready presenter. The frame
    /// deadline starts after them: loading the driver alone has taken 6 to 20 s on this machine under load.
    startup_ms: [u64; 3],
}

/// Runs a window until `frames` frames were presented (and the exercise finished), the window closes, or the
/// deadline passes. Builds its own event loop: one window run per process.
pub fn run(args: &WindowArgs) -> Result<WindowResult, HostError> {
    if args.quiet && args.frames == 0 {
        return Err(HostError::Usage("quiet_needs_frames"));
    }
    if args.frames > MAX_FRAMES {
        return Err(HostError::Usage("frames"));
    }
    if args.exercise && !args.quiet {
        return Err(HostError::Usage("exercise_needs_quiet"));
    }
    let (content, document) = content(&args.scene, args.width, args.height)?;
    let event_loop = EventLoop::builder()
        .with_any_thread(true)
        .with_dpi_aware(true)
        .build()
        .map_err(|_| HostError::Window("event_loop"))?;
    let watch = Watch::spawn()?;
    let mut app = App {
        args: args.clone(),
        content: Some(content),
        document,
        window: None,
        hwnd: 0,
        live: None,
        error: None,
        events: EventLog::default(),
        focus: FocusReport::default(),
        watch: Some(watch),
        start: Instant::now(),
        iterations: 0,
        presented: 0,
        phase: if args.exercise { Phase::Warm } else { Phase::Final },
        phase_frames: 0,
        phase_iterations: 0,
        exercised: Vec::new(),
        target: (0, 0),
        capture: None,
        scale_milli: 1000,
        report: None,
        device: None,
        layers: None,
        validation: "absent",
        startup_ms: [0; 3],
    };
    event_loop.run_app(&mut app).map_err(|_| HostError::Window("event_loop"))?;
    app.finish()
}

impl App {
    fn start(&mut self, el: &ActiveEventLoop) -> Result<(), HostError> {
        let content = self.content.take().ok_or(HostError::Window("state"))?;
        let (pw, ph) = content.extent();
        let (ww, wh) = (pw.min(2560), ph.min(1440));
        self.target = (ww, wh);
        let title = format!("Handshake Studio, asset-local Vulkan host (chrome: {CHROME})");
        let mut attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(PhysicalSize::new(ww, wh))
            .with_visible(false)
            .with_active(false);
        if self.args.quiet {
            let min_x = el.available_monitors().map(|m| m.position().x).min().unwrap_or(0);
            let x = min_x.saturating_sub(i32::try_from(ww).unwrap_or(i32::MAX)).saturating_sub(64);
            attrs = attrs
                .with_position(PhysicalPosition::new(x, 0))
                .with_window_level(WindowLevel::AlwaysOnBottom)
                .with_skip_taskbar(true);
        }
        let t0 = Instant::now();
        let window = el.create_window(attrs).map_err(|_| HostError::Window("create_window"))?;
        self.startup_ms[0] = millis(t0);
        let handles = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => WindowHandles { hwnd: h.hwnd.get(), hinstance: h.hinstance.map_or(0, |i| i.get()) },
            _ => return Err(HostError::Window("handle")),
        };
        self.hwnd = handles.hwnd;
        if self.args.quiet {
            win32::make_quiet(handles.hwnd)?;
        }
        // The first show of a window created inactive is SW_SHOWNOACTIVATE in winit (window_state.rs apply_diff), but
        // that same apply_diff rewrites GWL_EXSTYLE (WS_EX_APPWINDOW, no WS_EX_NOACTIVATE), so the quiet style goes
        // back on right after the show.
        window.set_visible(true);
        if self.args.quiet {
            win32::make_quiet(handles.hwnd)?;
        }
        self.scale_milli = (window.scale_factor() * 1000.0).round().clamp(0.0, 1.0e6) as u32;
        self.window = Some(window);
        let t1 = Instant::now();
        let mut owner = Owner::open(OwnerConfig { surface: true, allow_overlays: self.args.allow_overlays, allow_cpu_device: false })?;
        self.startup_ms[1] = millis(t1);
        let t2 = Instant::now();
        let pane = owner.create_pane(pw, ph)?;
        match &content {
            Content::Clear { rgba, .. } => owner.clear_pane(pane, *rgba)?,
            Content::Pixels { rgba8, .. } => owner.upload_pane(pane, rgba8)?,
        }
        let presenter = Presenter::new(&owner, handles, BACKGROUND)?;
        self.startup_ms[2] = millis(t2);
        self.device = Some(device_json(&owner));
        self.layers = Some(layers_json(&owner));
        self.validation = owner.validation();
        self.live = Some(Live { owner, pane, presenter });
        // The frame deadline counts from here.
        self.start = Instant::now();
        el.set_control_flow(ControlFlow::Poll);
        Ok(())
    }

    /// Puts the quiet style back if anything (winit's flag diff) removed it.
    fn keep_quiet(&mut self) -> Result<(), HostError> {
        if !self.args.quiet || self.hwnd == 0 {
            return Ok(());
        }
        let s = win32::quiet_style(self.hwnd);
        if !s.no_activate || !s.tool_window || s.app_window {
            win32::make_quiet(self.hwnd)?;
            self.focus.style_restores = self.focus.style_restores.saturating_add(1);
        }
        Ok(())
    }

    fn resize(&mut self, w: u32, h: u32, label: &'static str) {
        self.target = (w, h);
        if let Some(window) = &self.window {
            let _ = window.request_inner_size(PhysicalSize::new(w, h));
            self.exercised.push(label);
        }
    }

    fn advance(&mut self, next: Phase) {
        self.phase = next;
        self.phase_frames = 0;
        self.phase_iterations = 0;
    }

    /// Steps the exercise script; returns whether the next frame is the captured one.
    fn script(&mut self) -> Result<bool, HostError> {
        let Some(live) = &self.live else { return Ok(false) };
        let extent = live.presenter.extent();
        let minimized = live.presenter.state() == hsk_studio_vulkan::machine::SwapState::Minimized
            || live.presenter.counters().skipped_minimized > 0;
        let (pw, ph) = live.owner.pane_extent(live.pane)?;
        let (ww, wh) = (pw.min(2560), ph.min(1440));
        self.phase_iterations = self.phase_iterations.saturating_add(1);
        if self.phase != Phase::Final && self.phase_iterations > 600 {
            return Err(HostError::Timeout);
        }
        if self.phase == Phase::Final && extent != (ww, wh) {
            // Never capture at another size (a DPI change before the first frame resized the window).
            if extent != (0, 0) && (self.target != (ww, wh) || !self.exercised.contains(&"resize_to_pane")) {
                self.resize(ww, wh, "resize_to_pane");
            }
            return Ok(false);
        }
        match self.phase {
            Phase::Warm if self.phase_frames >= 2 => {
                self.resize(ww.saturating_add(64), wh.saturating_add(32), "resize_grow");
                self.advance(Phase::Grown);
            }
            Phase::Grown if self.phase_frames >= 1 && extent == (ww.saturating_add(64), wh.saturating_add(32)) => {
                win32::minimize_quiet(self.hwnd)?;
                self.exercised.push("minimize_no_activate");
                self.advance(Phase::Minimized);
            }
            Phase::Minimized if minimized => {
                if self.phase_iterations >= 3 {
                    win32::restore_quiet(self.hwnd)?;
                    self.exercised.push("restore_no_activate");
                    self.advance(Phase::Restored);
                }
            }
            Phase::Restored if self.phase_frames >= 1 && !win32::minimized(self.hwnd) => {
                self.resize(ww, wh, "resize_back");
                self.advance(Phase::Back);
            }
            Phase::Back if self.phase_frames >= 1 && extent == (ww, wh) => self.advance(Phase::Final),
            _ => {}
        }
        Ok(self.phase == Phase::Final && self.args.frames != 0 && self.presented.saturating_add(1) >= self.args.frames)
    }

    fn tick(&mut self, el: &ActiveEventLoop) -> Result<(), HostError> {
        self.iterations = self.iterations.saturating_add(1);
        if self.start.elapsed() > self.args.deadline || self.iterations > 2_000_000 {
            return Err(HostError::Timeout);
        }
        self.keep_quiet()?;
        if self.live.is_none() || self.phase == Phase::Done {
            return Ok(());
        }
        let capture_now = self.script()?;
        let Some(live) = self.live.as_mut() else { return Ok(()) };
        match live.presenter.frame(&live.owner, live.pane, capture_now)? {
            FrameOutcome::Presented { capture, .. } => {
                self.presented = self.presented.saturating_add(1);
                self.phase_frames = self.phase_frames.saturating_add(1);
                if let Some(c) = capture {
                    self.capture = Some(c);
                }
                el.set_control_flow(ControlFlow::Poll);
                let enough = self.args.frames != 0 && self.presented >= self.args.frames;
                if self.phase == Phase::Final && enough && (self.args.capture.is_none() || self.capture.is_some()) {
                    self.phase = Phase::Done;
                    el.exit();
                }
            }
            FrameOutcome::Skipped(_) => {
                el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(15)));
            }
        }
        Ok(())
    }

    fn fail(&mut self, el: &ActiveEventLoop, e: HostError) {
        if self.error.is_none() {
            self.error = Some(e);
        }
        el.exit();
    }

    fn release(&mut self) {
        if self.live.is_some() && self.hwnd != 0 {
            // The live style at the end of the run, read while the window still exists.
            let s = win32::quiet_style(self.hwnd);
            self.focus.no_activate_style = s.no_activate;
            self.focus.tool_window_style = s.tool_window;
            self.focus.app_window_style = s.app_window;
        }
        if let Some(live) = self.live.take() {
            let p = &live.presenter;
            let transitions = p
                .machine()
                .log()
                .iter()
                .map(|t| format!("{}:{}>{}", t.from.code(), t.signal.code(), t.to.code()))
                .collect();
            self.report = Some((p.counters(), p.state().code(), transitions, p.extent()));
            let Live { owner, pane, presenter } = live;
            drop(presenter);
            let mut owner = owner;
            let _ = owner.release_pane(pane);
            drop(owner);
        }
    }

    fn finish(mut self) -> Result<WindowResult, HostError> {
        self.release();
        if let Some(w) = self.watch.take() {
            let (samples, changes, takes) = w.finish();
            self.focus.samples = samples;
            self.focus.foreground_changes = changes;
            self.focus.foreground_takes = takes;
        }
        self.focus.focus_changes = self.focus.focus_gained_events.saturating_add(self.focus.foreground_takes);
        if let Some(e) = self.error {
            // Codes and counters only, for the log of a failed run (no receipt is written).
            let (c, state, _, extent) = self.report.take().unwrap_or((PresentCounters::default(), "none", Vec::new(), (0, 0)));
            eprintln!(
                "window_run_failed code={e} startup_ms={:?} phase={:?} iterations={} presented={} elapsed_ms={} state={state} extent={extent:?} target={:?} creates={} acquire_timeouts={} skipped_minimized={} focus={:?}",
                self.startup_ms,
                self.phase,
                self.iterations,
                self.presented,
                self.start.elapsed().as_millis(),
                self.target,
                c.swapchain_creates,
                c.acquire_timeouts,
                c.skipped_minimized,
                self.focus
            );
            return Err(e);
        }
        if self.args.quiet && self.focus.focus_changes != 0 {
            return Err(HostError::FocusViolation);
        }
        let (counters, final_state, transitions, swap_extent) = self.report.take().ok_or(HostError::Window("never_started"))?;
        let pane_extent = self.capture.as_ref().map_or(swap_extent, |c| (c.width, c.height));
        let frame_sha256 = self.capture.as_ref().map(|c| sha256_hex(&c.rgba8));
        let process_dpi_aware = win32::process_dpi_aware();
        let receipt = Json::obj(vec![
            ("schema", Json::str("hsk.studio.native_host.receipt@1")),
            ("host", Json::str("asset-local host")),
            ("mode", Json::str(if self.args.quiet { "window_quiet" } else { "window" })),
            ("source", Json::str("swapchain")),
            ("backend", Json::str("vulkan")),
            ("issuer", Json::str("transient_asset_local")),
            ("device", self.device.take().unwrap_or(Json::Null)),
            ("validation", Json::str(self.validation)),
            ("layers", self.layers.take().unwrap_or(Json::Null)),
            ("hwnd_created", Json::Bool(true)),
            ("scene", Json::str(self.args.scene.code())),
            ("format", Json::str(SWAPCHAIN_FORMAT)),
            (
                "extent",
                Json::Arr(vec![Json::Num(u64::from(swap_extent.0)), Json::Num(u64::from(swap_extent.1))]),
            ),
            ("frame_sha256", frame_sha256.clone().map_or(Json::Null, Json::Str)),
            ("frames_presented", Json::Num(counters.frames_presented)),
            (
                "focus",
                Json::obj(vec![
                    ("focus_changes", Json::Num(self.focus.focus_changes)),
                    ("focus_gained_events", Json::Num(self.focus.focus_gained_events)),
                    ("foreground_takes", Json::Num(self.focus.foreground_takes)),
                    ("foreground_changes_any_process", Json::Num(self.focus.foreground_changes)),
                    ("samples", Json::Num(self.focus.samples)),
                    ("no_activate_style", Json::Bool(self.focus.no_activate_style)),
                    ("tool_window_style", Json::Bool(self.focus.tool_window_style)),
                    ("app_window_style", Json::Bool(self.focus.app_window_style)),
                    ("style_restores", Json::Num(self.focus.style_restores)),
                ]),
            ),
            (
                "swapchain",
                Json::obj(vec![
                    ("final_state", Json::str(final_state)),
                    ("creates", Json::Num(counters.swapchain_creates)),
                    ("surface_recreates", Json::Num(counters.surface_recreates)),
                    ("out_of_date", Json::Num(counters.out_of_date)),
                    ("suboptimal", Json::Num(counters.suboptimal)),
                    ("acquire_timeouts", Json::Num(counters.acquire_timeouts)),
                    ("skipped_minimized", Json::Num(counters.skipped_minimized)),
                    ("transitions", Json::strs(&transitions)),
                ]),
            ),
            ("exercised", Json::strs(&self.exercised)),
            (
                "dpi",
                Json::obj(vec![
                    ("scale_factor_milli", Json::Num(u64::from(self.scale_milli))),
                    ("process_dpi_aware", Json::Bool(process_dpi_aware)),
                    ("awareness_requested", Json::str("per_monitor_v2 (winit with_dpi_aware)")),
                ]),
            ),
            ("events", Json::Num(self.events.events().len() as u64)),
            (
                "startup_ms",
                Json::obj(vec![
                    ("window", Json::Num(self.startup_ms[0])),
                    ("owner", Json::Num(self.startup_ms[1])),
                    ("presenter", Json::Num(self.startup_ms[2])),
                ]),
            ),
            ("chrome", Json::str(CHROME)),
            ("document", document_json(self.document.as_ref())),
        ]);
        if let (Some(path), Some(c)) = (&self.args.capture, &self.capture) {
            write_png(path, c.width, c.height, &c.rgba8)?;
        }
        if let Some(path) = &self.args.receipt {
            write_receipt(path, &receipt)?;
        }
        Ok(WindowResult {
            capture: self.capture,
            frame_sha256,
            focus: self.focus,
            counters,
            final_state,
            transitions,
            events: self.events.events().to_vec(),
            pane_extent,
            swap_extent,
            scale_factor_milli: self.scale_milli,
            process_dpi_aware,
            exercised: self.exercised,
            document: self.document,
            receipt,
        })
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() && self.error.is_none() {
            if let Err(e) = self.start(el) {
                self.fail(el, e);
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, mut event: WindowEvent) {
        if let WindowEvent::ScaleFactorChanged { inner_size_writer, .. } = &mut event {
            // Keep the physical client size: the pane is shown 1:1 in physical pixels.
            if self.target != (0, 0) {
                let _ = inner_size_writer.request_inner_size(PhysicalSize::new(self.target.0, self.target.1));
            }
        }
        let Some(ev) = from_winit(&event, crate::event::Mods::default()) else { return };
        self.events.push(ev);
        match ev {
            HostEvent::Resized { width, height } => {
                if let Some(live) = self.live.as_mut() {
                    let signal =
                        if width == 0 || height == 0 { WindowSignal::Minimized } else { WindowSignal::Resized };
                    live.presenter.window_signal(signal);
                }
            }
            HostEvent::Focus { gained: true } => {
                self.focus.focus_gained_events = self.focus.focus_gained_events.saturating_add(1);
            }
            HostEvent::ScaleFactor { milli } => self.scale_milli = milli,
            HostEvent::CloseRequested => el.exit(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.error.is_some() {
            return;
        }
        if let Err(e) = self.tick(el) {
            self.fail(el, e);
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Swapchain and surface go before the window they belong to.
        self.release();
    }
}

fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
