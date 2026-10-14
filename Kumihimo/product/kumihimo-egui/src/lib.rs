//! Replaceable desktop presentation over one host-owned canonical Controller.
//! Native and capture are optional; automation never requests foreground interaction.
#[cfg(feature = "capture")]
pub mod capture;
pub mod chrome;
pub mod content;
pub mod demo;
pub mod diagnostics;
#[cfg(all(feature = "native-apps", windows))]
pub mod external_apps;
#[cfg(feature = "drops")]
pub mod drop;
pub mod input;
#[cfg(feature = "native")]
pub mod native;
#[cfg(all(feature = "native-drops", windows))]
pub mod native_drop;
#[cfg(feature = "navigation")]
pub mod navigation;
pub mod persistence;
#[cfg(feature = "native")]
pub mod window_slots;
pub mod workspace;

use kumihimo_content::Controller;
use kumihimo_core::{Command, FrameId, Rect, ResourceId, Response, ToolId, ViewId, WorkspaceId};
pub const ADAPTER_VERSION: &str = "0.1.0";
pub const SETTINGS_VERSION: u32 = 1;
pub type Result<T> = std::result::Result<T, AdapterError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    Invalid(&'static str),
    Unsupported(&'static str),
    Budget(&'static str),
    Stale(&'static str),
    Core(kumihimo_core::Error),
    Content(kumihimo_content::Error),
    Host(String),
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(_) => f.write_str("Host failure; inspect host-owned diagnostics"),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for AdapterError {}
impl From<kumihimo_core::Error> for AdapterError {
    fn from(e: kumihimo_core::Error) -> Self {
        Self::Core(e)
    }
}
impl From<kumihimo_content::Error> for AdapterError {
    fn from(e: kumihimo_content::Error) -> Self {
        Self::Content(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceBinding {
    pub root: WorkspaceId,
    pub viewport: egui::ViewportId,
    pub epoch: u64,
    pub bounds: Rect,
    pub pixels_per_point: f32,
}
impl SurfaceBinding {
    pub fn validate(&self) -> Result<()> {
        if self.epoch == 0 || !self.pixels_per_point.is_finite() || self.pixels_per_point <= 0.0 {
            return Err(AdapterError::Invalid("surface epoch/scale"));
        }
        for v in [
            self.bounds.x(),
            self.bounds.y(),
            self.bounds.right(),
            self.bounds.bottom(),
        ] {
            if !v.is_finite() || v.abs() > f32::MAX as f64 {
                return Err(AdapterError::Invalid("surface coordinates"));
            }
        }
        self.physical_size()?;
        Ok(())
    }
    pub fn physical_size(&self) -> Result<[u32; 2]> {
        let mut result = [0; 2];
        for (i, extent) in [self.bounds.width(), self.bounds.height()]
            .into_iter()
            .enumerate()
        {
            let pixels = extent * self.pixels_per_point as f64;
            if !pixels.is_finite() || pixels < 0.0 || pixels > u32::MAX as f64 {
                return Err(AdapterError::Invalid("physical extent"));
            }
            result[i] = pixels.ceil() as u32;
        }
        Ok(result)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeProfile {
    Full,
    Compact,
    Contextual,
    Hidden,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterSettings {
    pub version: u32,
    pub profile: ChromeProfile,
    pub header_height: f32,
    pub hit_size: f32,
    pub long_press_seconds: f64,
    pub group_colors: bool,
    pub frame_shortcuts_first: bool,
}
impl Default for AdapterSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            profile: ChromeProfile::Full,
            header_height: 28.0,
            hit_size: 8.0,
            long_press_seconds: 0.45,
            group_colors: true,
            frame_shortcuts_first: false,
        }
    }
}
impl AdapterSettings {
    pub fn validate(&self) -> Result<()> {
        if self.version != SETTINGS_VERSION
            || !self.header_height.is_finite()
            || !(18.0..=96.0).contains(&self.header_height)
            || !self.hit_size.is_finite()
            || !(2.0..=64.0).contains(&self.hit_size)
            || !self.long_press_seconds.is_finite()
            || !(0.1..=5.0).contains(&self.long_press_seconds)
        {
            return Err(AdapterError::Invalid("adapter settings"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct HostPolicy {
    pub max_regions: usize,
    pub tab_page_size: usize,
    pub max_pending: usize,
    pub max_native_slots: usize,
    pub max_capture_bytes: usize,
    pub navigation_width: f32,
    pub settings: AdapterSettings,
    pub settings_source: &'static str,
    pub float_order: Vec<FrameId>,
}
impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            max_regions: 4096,
            tab_page_size: 32,
            max_pending: 64,
            max_native_slots: 8,
            max_capture_bytes: 64 * 1024 * 1024,
            navigation_width: 64.0,
            settings: AdapterSettings::default(),
            settings_source: "host-default",
            float_order: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub capture: bool,
    pub native: bool,
    pub native_observed: bool,
    pub physical_pen_observed: bool,
    pub native_child: bool,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            capture: cfg!(feature = "capture"),
            native: cfg!(feature = "native"),
            native_observed: false,
            physical_pen_observed: false,
            native_child: false,
        }
    }
}
#[derive(Clone, Debug)]
pub enum AdapterAction {
    Command(Command),
    Reveal(FrameId),
    PopoutFrame(FrameId),
    PopoutView(ViewId),
    PopoutGroup(kumihimo_core::GroupId),
    ReturnNative(WorkspaceId),
}
#[derive(Clone, Debug)]
pub struct RenderSlot {
    pub view: ViewId,
    pub resource: ResourceId,
    pub frame: FrameId,
    pub workspace: WorkspaceId,
    pub canonical_bounds: Rect,
    pub bounds: Rect,
    pub clip: Rect,
    pub physical_size: [u32; 2],
    pub scale: f32,
    pub epoch: u64,
    pub focused: bool,
    pub visible: bool,
}
impl RenderSlot {
    pub fn local_point(&self, point: egui::Pos2) -> Result<egui::Pos2> {
        if !point.x.is_finite() || !point.y.is_finite() || !to_egui(self.clip).contains(point) {
            return Err(AdapterError::Invalid("point outside global clip"));
        }
        Ok(egui::pos2(
            point.x - self.bounds.x() as f32,
            point.y - self.bounds.y() as f32,
        ))
    }
}
/// The host owns operation allocation, permission, providers and native policy.
pub trait DesktopHost {
    fn policy(&self) -> HostPolicy {
        HostPolicy::default()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn dispatch(&mut self, controller: &mut Controller, command: Command) -> Result<Response>;
    fn paint_content(
        &mut self,
        ui: &mut egui::Ui,
        slot: &RenderSlot,
        controller: &mut Controller,
    ) -> Result<()>;
    fn content_visibility(
        &mut self,
        _controller: &mut Controller,
        _surface: SurfaceBinding,
        _visible: &[ViewId],
    ) -> Result<()> {
        Ok(())
    }
    fn content_input(
        &mut self,
        _controller: &mut Controller,
        _slot: &RenderSlot,
        _input: kumihimo_content::Input,
    ) -> Result<()> {
        Err(AdapterError::Unsupported("host input route"))
    }
    fn tool_action(
        &mut self,
        _controller: &mut Controller,
        _tool: ToolId,
        _payload: &str,
    ) -> Result<()> {
        Err(AdapterError::Unsupported("host tool action"))
    }
    fn native_action(
        &mut self,
        _controller: &mut Controller,
        _action: AdapterAction,
    ) -> Result<()> {
        Err(AdapterError::Unsupported("native action unavailable"))
    }
    #[cfg(feature = "navigation")]
    fn navigation_permission(
        &self,
        _controller: &Controller,
        _scope: &kumihimo_content::Scope,
    ) -> kumihimo_content::HostPermission {
        kumihimo_content::HostPermission {
            allowed: false,
            eligibility_revision: 0,
        }
    }
    #[cfg(feature = "navigation")]
    fn navigation_pending(&mut self, _controller: &mut Controller) -> Result<()> {
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct ControlRecord {
    pub id: String,
    pub label: String,
    pub bounds: Rect,
    pub target: Option<ViewId>,
    pub enabled: bool,
}
#[derive(Clone, Debug, Default)]
pub struct FrameReport {
    #[cfg(all(feature="native-apps",windows))]
    pub native_child_overlay_blocked:bool,
    pub generation: u64,
    pub post_generation: u64,
    pub epoch: u64,
    pub regions: usize,
    pub canonical_views: usize,
    pub painted_views: usize,
    pub controls: Vec<ControlRecord>,
    pub control_limit: usize,
    pub rejected: Vec<String>,
    pub slots: Vec<RenderSlot>,
    pub repaint_requested: bool,
}
impl FrameReport {
    pub fn reject(&mut self, error: impl std::fmt::Display) {
        if self.rejected.len() < 64 {
            self.rejected.push(error.to_string());
        }
    }
}
pub struct DesktopAdapter {
    pub chrome: chrome::ChromeState,
    pub input: input::InputState,
}
impl Default for DesktopAdapter {
    fn default() -> Self {
        Self {
            chrome: chrome::ChromeState::default(),
            input: input::InputState::default(),
        }
    }
}
impl DesktopAdapter {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        controller: &mut Controller,
        surface: SurfaceBinding,
        host: &mut impl DesktopHost,
    ) -> Result<FrameReport> {
        surface.validate()?;
        let policy = host.policy();
        policy.settings.validate()?;
        // Register the base before floats: egui drains unregistered layers last.
        let base_layer = egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new(("kumihimo-surface", 0_u64)),
        );
        ctx.move_to_top(base_layer);
        let projection = workspace::project_with_chrome(controller, surface, host, &self.chrome)?;
        let control_limit = policy
            .max_regions
            .checked_mul(policy.tab_page_size.max(16))
            .ok_or(AdapterError::Budget("control count arithmetic"))?;
        let mut report = FrameReport {
            generation: controller.snapshot().generation,
            epoch: surface.epoch,
            regions: projection.frames.len(),
            canonical_views: controller
                .snapshot()
                .views
                .values()
                .filter(|v| v.closed.is_none())
                .count(),
            control_limit,
            ..Default::default()
        };
        for failure in &projection.rejected {
            report.reject(failure);
        }
        chrome::draw(
            ctx,
            controller,
            &projection,
            surface,
            host,
            &mut self.chrome,
            &mut report,
        );
        content::draw(ctx, controller, &projection, surface, host, &mut report);
        #[cfg(feature = "navigation")]
        navigation::draw(
            ctx,
            controller,
            &projection,
            surface,
            host,
            &mut self.input,
            &mut report,
        );
        input::process(
            ctx,
            controller,
            &projection,
            surface,
            host,
            &mut self.input,
            &mut report,
        );
        // Host-ordered pane floats remain below Foreground menus and overlays.
        let mut raised = std::collections::BTreeSet::new();
        for frame in &projection.frames {
            if let Some(root) = frame.z_root {
                if raised.insert(root) {
                    ctx.move_to_top(frame.layer());
                    // Visibility changes can invert cached Middle order; pin floats above base.
                    ctx.set_sublayer(base_layer, frame.layer());
                }
            }
        }
        if controller.snapshot().generation != projection.generation {
            report.repaint_requested = true;
            ctx.request_repaint();
        }
        report.post_generation = controller.snapshot().generation;
        #[cfg(all(feature="native-apps",windows))]
        {report.native_child_overlay_blocked=external_apps::native_child_overlays_blocked(ctx,&report.slots,self.chrome.preview.is_some()||self.input.active_capture().is_some());}
        Ok(report)
    }
}
pub fn to_egui(rect: Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(rect.x() as f32, rect.y() as f32),
        egui::vec2(rect.width() as f32, rect.height() as f32),
    )
}
pub fn from_egui(rect: egui::Rect) -> Result<Rect> {
    Ok(Rect::new(
        rect.min.x as f64,
        rect.min.y as f64,
        rect.width() as f64,
        rect.height() as f64,
    )?)
}
pub fn intersect(a: Rect, b: Rect) -> Result<Rect> {
    let x = a.x().max(b.x());
    let y = a.y().max(b.y());
    Ok(Rect::new(
        x,
        y,
        (a.right().min(b.right()) - x).max(0.0),
        (a.bottom().min(b.bottom()) - y).max(0.0),
    )?)
}
pub fn usable_content(rect: Rect, settings: &AdapterSettings) -> Result<Rect> {
    usable_content_with_reveal(rect, settings, false)
}
pub(crate) fn usable_content_with_reveal(
    rect: Rect,
    settings: &AdapterSettings,
    revealed: bool,
) -> Result<Rect> {
    let header = if settings.profile == ChromeProfile::Hidden && !revealed {
        0.0
    } else {
        settings.header_height as f64
    };
    let height = header.min(rect.height());
    Ok(Rect::new(
        rect.x(),
        rect.y() + height,
        rect.width(),
        rect.height() - height,
    )?)
}
/// A constant reserved gutter keeps provider geometry unchanged during scrub reveal.
pub fn content_and_navigation_bounds(
    controller: &Controller,
    frame: &workspace::FrameProjection,
    policy: &HostPolicy,
) -> Result<(Rect, Option<Rect>)> {
    if !policy.navigation_width.is_finite() || policy.navigation_width < 0.0 {
        return Err(AdapterError::Invalid("navigation gutter"));
    }
    #[cfg(not(feature = "navigation"))]
    let width = 0.0_f64;
    #[cfg(feature = "navigation")]
    let width = {
        let selected = controller
            .snapshot()
            .frames
            .get(&frame.frame)
            .and_then(|f| f.selected_view);
        let mount = controller
            .inspect()
            .mounts
            .values()
            .find(|m| m.active && m.scope.target.view() == selected && selected.is_some())
            .map(|m| m.scope.clone());
        match mount.and_then(|scope| controller.navigation_projection(&scope).ok()) {
            Some(n) if n.effective.is_some() => (policy.navigation_width as f64)
                .max(n.hit_width.get())
                .max(n.rail_width.get())
                .max(n.thumb_width.get())
                .min(frame.usable.width()),
            _ => 0.0,
        }
    };
    let _ = controller;
    let content = Rect::new(
        frame.usable.x(),
        frame.usable.y(),
        frame.usable.width() - width,
        frame.usable.height(),
    )?;
    let gutter = if width > 0.0 {
        Some(Rect::new(
            content.right(),
            content.y(),
            width,
            content.height(),
        )?)
    } else {
        None
    };
    Ok((content, gutter))
}
