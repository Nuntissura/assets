//! Host-owned copied pixels. No engine, clock, decoder or renderer is owned here.
use std::collections::BTreeMap;

pub const BRIDGE_VERSION: u16 = 1;
pub type Result<T> = std::result::Result<T, BridgeError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeError {
    Invalid(&'static str),
    Budget(&'static str),
    UnknownView,
    Stale(&'static str),
    Duplicate,
    Busy,
    Expired,
    Protected,
    Unsupported(&'static str),
    ReconciliationRequired,
    Exhausted(&'static str),
}
impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for BridgeError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeLimits {
    pub max_message_bytes: usize,
    pub max_png_bytes: usize,
    pub max_rgba_bytes: usize,
    pub max_side: u32,
    pub max_live_engines: usize,
    pub max_host_frames: usize,
}
impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 4 * 1024 * 1024,
            max_png_bytes: 16 * 1024 * 1024,
            max_rgba_bytes: 16 * 1024 * 1024,
            max_side: 2048,
            max_live_engines: 2,
            max_host_frames: 4,
        }
    }
}
impl BridgeLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_message_bytes == 0
            || self.max_png_bytes == 0
            || self.max_rgba_bytes == 0
            || self.max_side == 0
            || self.max_live_engines == 0
            || self.max_host_frames == 0
        {
            return Err(BridgeError::Invalid("zero bridge budget"));
        }
        self.max_rgba_bytes
            .checked_mul(self.max_host_frames)
            .ok_or(BridgeError::Invalid("host frame budget overflow"))?;
        Ok(())
    }
    pub fn check_message(&self, bytes: usize) -> Result<()> {
        if bytes > self.max_message_bytes {
            Err(BridgeError::Budget("message"))
        } else {
            Ok(())
        }
    }
    pub fn check_png(&self, bytes: usize) -> Result<()> {
        if bytes == 0 {
            return Err(BridgeError::Invalid("empty PNG"));
        }
        if bytes > self.max_png_bytes {
            Err(BridgeError::Budget("encoded PNG"))
        } else {
            Ok(())
        }
    }
    /// Call on decoder-reported dimensions before allocating decoded storage.
    pub fn rgba_layout(&self, width: u32, height: u32) -> Result<(u32, usize)> {
        if width == 0 || height == 0 {
            return Err(BridgeError::Invalid("zero pixel extent"));
        }
        if width > self.max_side || height > self.max_side {
            return Err(BridgeError::Budget("pixel side"));
        }
        let stride = width
            .checked_mul(4)
            .ok_or(BridgeError::Invalid("row stride overflow"))?;
        let len = usize::try_from(stride)
            .ok()
            .and_then(|s| usize::try_from(height).ok().and_then(|h| s.checked_mul(h)))
            .ok_or(BridgeError::Invalid("pixel length overflow"))?;
        if len > self.max_rgba_bytes {
            return Err(BridgeError::Budget("raw RGBA"));
        }
        Ok((stride, len))
    }
}

#[cfg_attr(feature = "desktop", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "desktop", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeIdentity {
    pub lineage: String,
    pub incarnation: String,
}

#[cfg_attr(feature = "desktop", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "desktop",
    serde(
        tag = "kind",
        content = "id",
        rename_all = "snake_case",
        deny_unknown_fields
    )
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopeTarget {
    View(u64),
    FrameBackground(u64),
    WorkspaceBackground(u64),
}
impl ScopeTarget {
    pub fn raw(self) -> u64 {
        match self {
            Self::View(id) | Self::FrameBackground(id) | Self::WorkspaceBackground(id) => id,
        }
    }
}

/// Exact copy of the complete canonical content Scope, including target kind.
#[cfg_attr(feature = "desktop", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "desktop", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentScope {
    pub identity: ScopeIdentity,
    pub session: String,
    pub provider: u64,
    pub resource: u64,
    pub target: ScopeTarget,
    pub connection: u64,
    pub instance: u64,
    pub mount: u64,
}
impl ContentScope {
    pub fn validate(&self) -> Result<()> {
        for s in [
            &self.identity.lineage,
            &self.identity.incarnation,
            &self.session,
        ] {
            if s.is_empty() || s.len() > 128 {
                return Err(BridgeError::Invalid("scope token"));
            }
        }
        for id in [self.provider, self.resource, self.target.raw()] {
            if id == 0 || id == u64::MAX {
                return Err(BridgeError::Invalid("scope ID"));
            }
        }
        if self.connection == 0 || self.instance == 0 || self.mount == 0 {
            return Err(BridgeError::Invalid("scope connection/instance/mount"));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "desktop", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "desktop", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameBinding {
    pub scope: ContentScope,
    pub engine_incarnation: u64,
    pub geometry_revision: u64,
    pub surface_epoch: u64,
}
impl FrameBinding {
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        if self.engine_incarnation == 0 || self.geometry_revision == 0 || self.surface_epoch == 0 {
            return Err(BridgeError::Invalid("engine/geometry/surface identity"));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "desktop", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "desktop", serde(deny_unknown_fields))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRequest {
    pub version: u16,
    pub binding: FrameBinding,
    pub request_id: u64,
    pub sequence: u64,
    pub deadline_ms: u64,
    pub width: u32,
    pub height: u32,
}
impl FrameRequest {
    pub fn validate(&self, limits: &BridgeLimits) -> Result<()> {
        if self.version != BRIDGE_VERSION {
            return Err(BridgeError::Unsupported("bridge version"));
        }
        self.binding.validate()?;
        if self.request_id == 0 || self.sequence == 0 || self.deadline_ms == 0 {
            return Err(BridgeError::Invalid("request identity/deadline"));
        }
        limits.rgba_layout(self.width, self.height)?;
        Ok(())
    }
}

/// Top-left-origin, straight-alpha, sRGB RGBA8; rows have no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelFrame {
    pub request: FrameRequest,
    pub row_stride: u32,
    pub rgba: Vec<u8>,
}
impl PixelFrame {
    pub fn validate(&self, limits: &BridgeLimits) -> Result<()> {
        self.request.validate(limits)?;
        let (stride, len) = limits.rgba_layout(self.request.width, self.request.height)?;
        if self.row_stride != stride {
            return Err(BridgeError::Invalid("RGBA row stride"));
        }
        if self.rgba.len() != len {
            return Err(BridgeError::Invalid("RGBA exact length"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ViewFrames {
    binding: FrameBinding,
    width: u32,
    height: u32,
    sequence: u64,
    last_request_id: u64,
    pending: Option<FrameRequest>,
    last_good: Option<PixelFrame>,
}

/// Bounded live frame cache, separate from unlimited logical host tab records.
#[derive(Clone, Debug)]
pub struct FrameStore {
    limits: BridgeLimits,
    views: BTreeMap<ScopeTarget, ViewFrames>,
}
/// Original capture provenance remains unchanged during renderer-only recovery.
#[derive(Clone, Copy, Debug)]
pub struct RecoveryFrame<'a> {
    pub captured: &'a PixelFrame,
    pub current_binding: &'a FrameBinding,
}
impl FrameStore {
    pub fn new(limits: BridgeLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            views: BTreeMap::new(),
        })
    }
    pub fn limits(&self) -> &BridgeLimits {
        &self.limits
    }
    /// Superseded work is discarded; last good CPU pixels remain recoverable.
    pub fn bind(&mut self, binding: FrameBinding, width: u32, height: u32) -> Result<()> {
        binding.validate()?;
        self.limits.rgba_layout(width, height)?;
        let target = binding.scope.target;
        if let Some(view) = self.views.get_mut(&target) {
            if binding.scope == view.binding.scope {
                if binding.engine_incarnation < view.binding.engine_incarnation
                    || binding.geometry_revision < view.binding.geometry_revision
                    || binding.surface_epoch < view.binding.surface_epoch
                {
                    return Err(BridgeError::Stale("binding revision regression"));
                }
                if (view.width != width || view.height != height)
                    && binding.geometry_revision == view.binding.geometry_revision
                {
                    return Err(BridgeError::Invalid("resize without geometry revision"));
                }
            }
            if view.binding != binding || view.width != width || view.height != height {
                view.pending = None;
                view.binding = binding;
                view.width = width;
                view.height = height;
            }
        } else {
            if self.views.len() >= self.limits.max_host_frames {
                return Err(BridgeError::Budget("host frames"));
            }
            self.views.insert(
                target,
                ViewFrames {
                    binding,
                    width,
                    height,
                    sequence: 0,
                    last_request_id: 0,
                    pending: None,
                    last_good: None,
                },
            );
        }
        Ok(())
    }
    pub fn remove(&mut self, target: ScopeTarget) -> bool {
        self.views.remove(&target).is_some()
    }
    pub fn request(
        &mut self,
        target: ScopeTarget,
        request_id: u64,
        deadline_ms: u64,
        now_ms: u64,
    ) -> Result<FrameRequest> {
        let view = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if view.pending.is_some() {
            return Err(BridgeError::Busy);
        }
        if request_id == 0 || request_id <= view.last_request_id {
            return Err(BridgeError::Duplicate);
        }
        if now_ms >= deadline_ms {
            return Err(BridgeError::Expired);
        }
        let sequence = view
            .sequence
            .checked_add(1)
            .ok_or(BridgeError::Exhausted("frame sequence"))?;
        let request = FrameRequest {
            version: BRIDGE_VERSION,
            binding: view.binding.clone(),
            request_id,
            sequence,
            deadline_ms,
            width: view.width,
            height: view.height,
        };
        request.validate(&self.limits)?;
        view.sequence = sequence;
        view.last_request_id = request_id;
        view.pending = Some(request.clone());
        Ok(request)
    }
    /// Run before decoding or uploading an untrusted response.
    pub fn validate_response(&self, request: &FrameRequest, now_ms: u64) -> Result<()> {
        request.validate(&self.limits)?;
        let view = self
            .views
            .get(&request.binding.scope.target)
            .ok_or(BridgeError::UnknownView)?;
        if request.binding != view.binding {
            return Err(BridgeError::Stale("complete frame binding"));
        }
        if now_ms >= request.deadline_ms {
            return Err(BridgeError::Expired);
        }
        match &view.pending {
            Some(expected) if expected == request => Ok(()),
            Some(_) => Err(BridgeError::Stale("request/sequence/dimensions")),
            None => Err(BridgeError::Duplicate),
        }
    }
    pub fn accept(&mut self, frame: PixelFrame, now_ms: u64) -> Result<()> {
        self.validate_response(&frame.request, now_ms)?;
        frame.validate(&self.limits)?;
        let view = self
            .views
            .get_mut(&frame.request.binding.scope.target)
            .ok_or(BridgeError::UnknownView)?;
        view.last_good = Some(frame);
        view.pending = None;
        Ok(())
    }
    pub fn cancel(&mut self, target: ScopeTarget, request_id: u64) -> Result<()> {
        let view = self
            .views
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if view.pending.as_ref().map(|r| r.request_id) != Some(request_id) {
            return Err(BridgeError::Stale("cancel request"));
        }
        view.pending = None;
        Ok(())
    }
    pub fn last_good(&self, target: ScopeTarget) -> Option<&PixelFrame> {
        self.views
            .get(&target)
            .and_then(|view| view.last_good.as_ref())
    }
    /// Renderer recovery may use last_good explicitly; normal rendering cannot use stale bindings.
    pub fn current_frame(&self, target: ScopeTarget) -> Result<&PixelFrame> {
        let view = self.views.get(&target).ok_or(BridgeError::UnknownView)?;
        let frame = view
            .last_good
            .as_ref()
            .ok_or(BridgeError::Stale("no accepted frame"))?;
        if frame.request.binding != view.binding
            || frame.request.width != view.width
            || frame.request.height != view.height
        {
            return Err(BridgeError::Stale("retained CPU frame"));
        }
        Ok(frame)
    }
    pub fn pending(&self, target: ScopeTarget) -> Option<&FrameRequest> {
        self.views.get(&target).and_then(|v| v.pending.as_ref())
    }
    pub fn recovery_frame(&self, target: ScopeTarget) -> Result<RecoveryFrame<'_>> {
        let view = self.views.get(&target).ok_or(BridgeError::UnknownView)?;
        let captured = view
            .last_good
            .as_ref()
            .ok_or(BridgeError::Stale("no CPU recovery frame"))?;
        let old = &captured.request.binding;
        let current = &view.binding;
        if old.scope != current.scope
            || old.engine_incarnation != current.engine_incarnation
            || old.geometry_revision != current.geometry_revision
            || captured.request.width != view.width
            || captured.request.height != view.height
        {
            return Err(BridgeError::Stale("CPU recovery content/engine/geometry"));
        }
        if current.surface_epoch <= old.surface_epoch {
            return Err(BridgeError::Stale("CPU recovery renderer epoch"));
        }
        Ok(RecoveryFrame {
            captured,
            current_binding: current,
        })
    }
}
