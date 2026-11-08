//! The Vulkan UI pass (SMB-VK-UI-PASS-1): kumihimo-ui's display list, all 11 primitives, drawn on the owner's device
//! into a pane from offline SPIR-V. The [`Recorder`] is plain data (no device): it computes every per-primitive value
//! with the CPU painter's own expressions and plans instances, batches, atlas uploads and pass breaks; [`UiPass`] runs
//! the plan on the owner's Core and submitter. No ash type crosses this API.
use crate::atlas::Atlas;
use crate::owner::{Owner, PaneId};
use crate::refusal::Refusal;
use kumihimo_ui::display::{DisplayList, IconId, ImageId};
use kumihimo_ui::geom::{IRect, Rgba8, Scale};
use kumihimo_ui::paint::Resources;
use kumihimo_ui::text::{GlyphKey, Text};

/// Instance kinds (the shader's `kind`).
pub mod kind {
    pub const FLAT: u32 = 0;
    pub const CHECKER: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const GLYPH: u32 = 3;
    pub const ICON: u32 = 4;
    pub const RRECT: u32 = 5;
    pub const LINE: u32 = 6;
    pub const DASH: u32 = 7;
    pub const SHADOW: u32 = 8;
    pub const RAMP: u32 = 9;
}

/// One instanced quad: 4 vertices, no index buffer. `quad` is already intersected with the clip and the pane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Instance {
    /// x0, y0, x1, y1 in physical px.
    pub quad: [i32; 4],
    pub kind: u32,
    /// Kind-specific selector (icon channel, border or fill, dash with an off colour).
    pub sel: u32,
    /// Texture origin: texel = pixel - org (glyph and icon pages, images).
    pub org: [i32; 2],
    /// Packed RGBA8 colours, r in the low byte.
    pub colors: [u32; 4],
    /// Per-kind constants computed on the CPU with the painter's expressions.
    pub params: [f32; 8],
}

/// Bytes per instance.
pub const INSTANCE_BYTES: usize = std::mem::size_of::<Instance>();

/// Bounds of one pass (proposals in the brief, stated here so tests can force small pages).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiLimits {
    pub glyph_min_side: u32,
    pub glyph_max_side: u32,
    pub icon_min_side: u32,
    pub icon_max_side: u32,
    /// Instance buffer cap per pass, bytes.
    pub instance_cap_bytes: usize,
    /// Passes per frame before `ui_pass_limit`.
    pub max_passes: u32,
    /// The device's maxImageDimension2D (images above it are skipped and counted).
    pub max_image_side: u32,
}

impl Default for UiLimits {
    fn default() -> Self {
        Self {
            glyph_min_side: 1024,
            glyph_max_side: 4096,
            icon_min_side: 512,
            icon_max_side: 2048,
            instance_cap_bytes: 16 << 20,
            max_passes: 16,
            max_image_side: 16384,
        }
    }
}

/// Texels to copy into a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub bytes: Vec<u8>,
}

/// A run of instances drawn with one descriptor set: the pages, plus `image` when the run draws an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
    pub image: Option<ImageId>,
    pub first: u32,
    pub count: u32,
}

/// One pass: one submit, one render pass instance; a pass after the first loads the pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassPlan {
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    /// The glyph page side in this pass and whether it was reset (or newly created) before the pass.
    pub glyph_side: u32,
    pub glyph_reset: bool,
    pub glyph_uploads: Vec<Upload>,
    pub icon_side: u32,
    pub icon_reset: bool,
    pub icon_uploads: Vec<Upload>,
}

/// One primitive of the trace (`--ui-trace`, visual debugging): no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    pub kind: &'static str,
    /// Snapped rect (physical px) before clipping.
    pub rect: IRect,
    /// `flat`, `aa`, `glyph`, `icon`, `image`, `image_translucent`, `scissor`, `skipped`.
    pub class: &'static str,
    pub instances: u32,
    pub pass: u32,
}

/// What one recording did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordStats {
    pub prims: u64,
    pub glyphs: u64,
    pub glyph_refusals: u64,
    pub missing_resources: u64,
    pub image_over_limit: u64,
    pub skipped_non_finite: u64,
    pub scissor_underflow: u64,
    /// Recordings restarted after a page growth.
    pub restarts: u64,
}

/// The plan of one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub passes: Vec<PassPlan>,
    pub trace: Vec<TraceRecord>,
    pub stats: RecordStats,
}

/// The CPU half of the pass: residency of the glyph and icon pages across frames, and the per-frame plan.
pub struct Recorder {
    glyphs: Atlas<GlyphKey>,
    icons: Atlas<(IconId, u32)>,
    limits: UiLimits,
}

impl Recorder {
    pub fn new(limits: UiLimits) -> Self {
        Self {
            glyphs: Atlas::new(limits.glyph_min_side, limits.glyph_max_side),
            icons: Atlas::new(limits.icon_min_side, limits.icon_max_side),
            limits,
        }
    }
    pub fn limits(&self) -> UiLimits {
        self.limits
    }
    pub fn glyph_atlas(&self) -> &Atlas<GlyphKey> {
        &self.glyphs
    }
    pub fn icon_atlas(&self) -> &Atlas<(IconId, u32)> {
        &self.icons
    }
    /// Plans `lists` (back to front) over a pane of `extent` at `scale`.
    pub fn record(
        &mut self,
        _extent: (u32, u32),
        _lists: &[&DisplayList],
        _scale: Scale,
        _text: &mut Text,
        _res: &mut dyn Resources,
        _trace: bool,
    ) -> Result<Plan, Refusal> {
        Err(Refusal::FeatureMissing)
    }
}

/// One frame for [`UiPass::draw`]: the frame clear, the ordered lists, the scale, the text and the resources.
pub struct UiFrame<'a> {
    pub clear: Rgba8,
    pub lists: &'a [&'a DisplayList],
    pub scale: Scale,
    pub text: &'a mut Text,
    pub res: &'a mut dyn Resources,
}

/// Counters of the pass (receipt member `ui_pass`): allocation sizes come from VkMemoryRequirements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiStats {
    pub draws: u64,
    pub passes: u64,
    pub instances: u64,
    pub glyph_side: u32,
    pub glyph_entries: u64,
    pub glyph_uploads: u64,
    pub glyph_upload_bytes: u64,
    pub glyph_resets: u64,
    pub glyph_growths: u64,
    pub icon_side: u32,
    pub icon_entries: u64,
    pub icon_uploads: u64,
    pub icon_upload_bytes: u64,
    pub icon_resets: u64,
    pub image_textures: u64,
    pub image_uploads: u64,
    pub image_upload_bytes: u64,
    /// Device memory of the pages, image textures, instance and staging buffers (VkMemoryRequirements sizes).
    pub page_bytes: u64,
    pub image_bytes: u64,
    pub instance_bytes: u64,
    pub staging_bytes: u64,
    pub glyph_refusals: u64,
    pub missing_resources: u64,
    pub image_over_limit: u64,
    pub skipped_non_finite: u64,
    pub scissor_underflow: u64,
}

/// The GPU half: pipeline, pages, image textures, instance and staging buffers, on the owner's device.
pub struct UiPass {
    recorder: Recorder,
    stats: UiStats,
    trace: Vec<TraceRecord>,
    trace_on: bool,
}

impl UiPass {
    /// Builds the pipeline on `owner`'s device. Refusals: `format_unsupported`, `feature_missing`, Vulkan errors.
    pub fn new(owner: &Owner) -> Result<Self, Refusal> {
        Self::with_limits(owner, UiLimits::default())
    }
    pub fn with_limits(_owner: &Owner, _limits: UiLimits) -> Result<Self, Refusal> {
        Err(Refusal::FeatureMissing)
    }
    /// Draws one frame into `pane`: clear, every list in order, pane left in TRANSFER_SRC_OPTIMAL.
    pub fn draw(&mut self, _owner: &mut Owner, _pane: PaneId, _frame: UiFrame<'_>) -> Result<(), Refusal> {
        Err(Refusal::FeatureMissing)
    }
    pub fn stats(&self) -> UiStats {
        self.stats
    }
    pub fn recorder(&self) -> &Recorder {
        &self.recorder
    }
    /// Keep one trace record per primitive of the next draws.
    pub fn set_trace(&mut self, on: bool) {
        self.trace_on = on;
    }
    /// The trace of the last draw.
    pub fn trace(&self) -> &[TraceRecord] {
        &self.trace
    }
}
