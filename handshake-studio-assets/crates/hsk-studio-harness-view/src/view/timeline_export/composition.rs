//! Render queue from COMPOSITIONS (lanes SMB-AE-MOTION-TOOLS slice 4, SMB-AE-MOTION-RECORDS-2;
//! feature `media`; STU-MOT-120/122/124/125, STU-MOT-004 shutter, STU-CMP-010 step 5): a composition
//! render queue item and a sequence export recipe are two entry points to the ONE export pipeline
//! 14.13 owns, here reel's `ExportQueue`. The harness only adapts ports (CX-CAP-001: no codec and
//! no compositor here):
//!
//! * frames: a caller [`CompositionScenes`] evaluates the composition at a composition tick
//!   (composite's `CompositionTemporalSource::scene_at` behind `render_queue::TemporalScenes`; tests
//!   build scenes directly) and composite's `render_scene` renders it on render-cpu into straight
//!   RGBA;
//! * motion blur ([`MotionBlur`], STU-MOT-004 `shutter_angle` 0..=720, `shutter_phase`
//!   -360..=360, `samples_per_frame` 2..=64): the exposure of the frame at tick `T` is
//!   `[T + phase/360 * tpf, T + (phase + angle)/360 * tpf)`; sample `k` of `n` is at the tick
//!   `floor(start + width * (k + 0.5) / n)`; at every sample the blurred layers (all, or the checked
//!   layer ids) come from the scene at the sample tick and every other layer from the scene at `T`
//!   (in the sample scene's layer order); the `n` renders are averaged premultiplied
//!   (accumulation-buffer model; After Effects' adaptive sampling is not reproduced, UNVERIFIED D5);
//! * render options ([`RenderOptions`], lane SMB-AE-RENDER-QUEUE-2; STU-MOT-121 `resolution` and
//!   `color_depth`): a divisor pair renders the scene at `ceil(size / divisor)` per side (composite's
//!   `scale_scene` follows every layer matrix by the scale, so the layers resample with the scene's
//!   declared filter; no resampler is added); a [`RenderDepth`] picks the output stage of the plan
//!   (8: 8-bit working levels, 16: the unit-range binary32 pipeline, 32: the unbounded range) and
//!   quantises the delivered samples to that depth (8: 255 levels, 16: 65535 levels, 32: none);
//! * render once, encode many ([`SharedFrames`]): the frames of a span are rendered once into a
//!   bounded shared cache that every output module of an item reads (each frame is dropped after
//!   the last module took it); the cache budget is checked before anything renders;
//! * output module post (STU-MOT-122, [`OutputPost`]): CROP (top, left, bottom, right in pixels,
//!   negative expands with transparency), then RESIZE (bilinear on premultiplied values, pixel
//!   centres mapped `x_src = (x + 0.5) * W / w - 0.5`, edges clamped), then CHANNELS: `rgb` as
//!   straight or premultiplied colour (over black), `alpha` as a grey matte (alpha in all three
//!   channels); `rgb_alpha` needs an encoder that carries alpha (none in reel: refused by the
//!   render-queue mapping); the RGB frame goes through the timeline export's BT.709 conversion
//!   ([`super::rgb_to_planes`]);
//! * audio: the composition's audio layers ([`CompositionAudioLayer`]) summed sample-exactly.
//!
//! Refused before anything renders (STU-VID-062): partial or empty spans, between-sample audio
//! starts, odd buffers, too many audio layers, shutter values out of range, crop leaving no pixel,
//! resize sizes outside 1..=30000, a cache above its budget; per frame: a scene of another raster.
#![deny(clippy::arithmetic_side_effects)]

use super::rgb_to_planes;
use crate::view::monitor::MonitorFail;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_composite::{SceneComposition, SceneContext, SceneLimits, ScenePixels, render_scene, render_scene_with_output, scale_scene};
use hsk_studio_pigment::blend_math::Exactness;
use hsk_studio_pigment::render_plan::{ColourTag, ExecuteRequest, OutputStage, SampleRange, WorkingDepth, quantise_8bit};
use hsk_studio_reel::{
    ChromaFormat, ColorInfo, ColorOrigin, ExportAudioSource, ExportJob, ExportLimits, ExportRecipe,
    ExportVideoSource, ReelError, SourceSpan, VideoFormat,
};
use hsk_studio_render_cpu::Renderer;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Most audio layers of one composition export.
pub const MAX_AUDIO_LAYERS: usize = 256;
/// Default byte budget of a render-once frame cache.
pub const DEFAULT_SHARED_CACHE_BYTES: u64 = 1 << 30;

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// Evaluates a composition at composition ticks.
pub trait CompositionScenes: Send {
    /// The composition's frame at `tick` (may be negative inside a shutter window); an `Err`
    /// fails the export item.
    fn scene_at(&mut self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, String>;
    /// Pixels the scenes' tiles and masks resolve against.
    fn pixels(&self) -> &dyn ScenePixels;
}

/// One audio layer of a composition: interleaved stereo at the export rate, sample 0 at
/// composition tick `start_tick` (a whole sample), times `gain`.
#[derive(Clone, Debug)]
pub struct CompositionAudioLayer {
    pub samples: Arc<Vec<f32>>,
    pub start_tick: u64,
    pub gain: f32,
}

/// Motion blur (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct MotionBlur {
    pub shutter_angle: f64,
    pub shutter_phase: f64,
    pub samples_per_frame: u32,
    /// Blurred layer ids; empty = every layer.
    pub layers: BTreeSet<String>,
}

impl MotionBlur {
    fn check(&self) -> Result<(), MonitorFail> {
        if !(self.shutter_angle.is_finite() && (0.0..=720.0).contains(&self.shutter_angle))
            || !(self.shutter_phase.is_finite() && (-360.0..=360.0).contains(&self.shutter_phase))
            || !(2..=64).contains(&self.samples_per_frame)
        {
            return Err(fail("render_motion_blur_settings", "shutter angle 0..=720, phase -360..=360, samples 2..=64"));
        }
        Ok(())
    }

    /// Sample ticks of the frame at `tick` (module docs).
    pub fn sample_ticks(&self, tick: u64, ticks_per_frame: u64) -> Vec<i64> {
        let tpf = ticks_per_frame as f64;
        let start = tick as f64 + self.shutter_phase / 360.0 * tpf;
        let width = self.shutter_angle / 360.0 * tpf;
        let n = f64::from(self.samples_per_frame);
        (0..self.samples_per_frame)
            .map(|k| (start + width * (f64::from(k) + 0.5) / n).floor() as i64)
            .collect()
    }
}

/// A rendered frame: straight `rgb_f32` and alpha per pixel, composition size.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Straight colour, 3 per pixel.
    pub rgb: Vec<f32>,
    pub alpha: Vec<f32>,
}

fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| b.try_into().map_or(0.0, f32::from_le_bytes)).collect()
}

/// Colour depth of a render (STU-MOT-121 `color_depth`, STU-COL-120; module docs). `Float` is the
/// engine default (binary32, unit range, nothing quantised).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderDepth {
    #[default]
    Float,
    Bits8,
    Bits16,
    Bits32,
}

impl RenderDepth {
    /// The plan output stage of this depth (`None` = the plan's default stage).
    pub fn output_stage(self) -> Option<OutputStage> {
        match self {
            Self::Float | Self::Bits16 => None,
            Self::Bits8 => Some(OutputStage::new(SampleRange::Unit, None).with_working_depth(WorkingDepth::Eight)),
            Self::Bits32 => Some(OutputStage::new(SampleRange::Extended, None)),
        }
    }

    /// One sample on this depth's levels (`Float` and `Bits32` keep it; NaN becomes 0).
    pub fn quantise(self, v: f32) -> f32 {
        match self {
            Self::Float | Self::Bits32 => v,
            Self::Bits8 => quantise_8bit(v.max(0.0).min(1.0)),
            Self::Bits16 => (v.max(0.0).min(1.0) * 65535.0 + 0.5).floor() / 65535.0,
        }
    }

    fn quantise_frame(self, mut f: Frame) -> Frame {
        if matches!(self, Self::Bits8 | Self::Bits16) {
            for v in f.rgb.iter_mut().chain(f.alpha.iter_mut()) {
                *v = self.quantise(*v);
            }
        }
        f
    }
}

/// Render options of a [`FrameRenderer`] (module docs): resolution divisors and colour depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    /// Horizontal and vertical divisors, each `1..=99` (Folio's `StudioRenderResolution::Custom`
    /// bound); `(1, 1)` = full resolution.
    pub divisor: (u32, u32),
    pub depth: RenderDepth,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { divisor: (1, 1), depth: RenderDepth::Float }
    }
}

impl RenderOptions {
    /// Rendered size of a `width x height` composition: `ceil(side / divisor)`, at least 1.
    pub fn size(&self, width: u32, height: u32) -> Result<(u32, u32), MonitorFail> {
        let (x, y) = self.divisor;
        if !(1..=99).contains(&x) || !(1..=99).contains(&y) {
            return Err(fail("render_resolution", "resolution divisors are 1..=99"));
        }
        Ok((width.div_ceil(x).max(1), height.div_ceil(y).max(1)))
    }
}

/// Renders frames of one composition (with optional motion blur).
pub struct FrameRenderer<S> {
    scenes: S,
    renderer: Renderer,
    space: ColourTag,
    /// Rendered (output) size; the composition's own size is `source`.
    width: u32,
    height: u32,
    source: (u32, u32),
    options: RenderOptions,
    ticks_per_frame: u64,
    blur: Option<MotionBlur>,
    renders: u64,
}

impl<S: CompositionScenes> FrameRenderer<S> {
    pub fn new(scenes: S, space: ColourTag, width: u32, height: u32, ticks_per_frame: u64, blur: Option<MotionBlur>) -> Result<Self, MonitorFail> {
        if let Some(b) = &blur {
            b.check()?;
        }
        let renderer = Renderer::new(64).map_err(|e| fail("renderer", format!("{e:?}")))?;
        Ok(Self { scenes, renderer, space, width, height, source: (width, height), options: RenderOptions::default(), ticks_per_frame, blur, renders: 0 })
    }

    /// This renderer with `options` (module docs); the rendered size becomes the reduced size.
    pub fn with_options(mut self, options: RenderOptions) -> Result<Self, MonitorFail> {
        let (w, h) = options.size(self.source.0, self.source.1)?;
        self.width = w;
        self.height = h;
        self.options = options;
        Ok(self)
    }

    /// The rendered frame size.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Frame renders so far (motion-blur samples count once per frame).
    pub fn renders(&self) -> u64 {
        self.renders
    }

    fn render_scene(&mut self, scene: &SceneComposition, cancel: &CancellationToken) -> Result<Frame, ReelError> {
        if (scene.width, scene.height) != self.source {
            return Err(ReelError::Unsupported { what: "export-composition-size" });
        }
        let scaled;
        let scene = if (self.width, self.height) == self.source {
            scene
        } else {
            scaled = scale_scene(scene, self.width, self.height).map_err(|_| ReelError::Unsupported { what: "export-resolution-scale" })?;
            &scaled
        };
        let ctx = SceneContext {
            backend: &self.renderer,
            pixels: self.scenes.pixels(),
            request: ExecuteRequest { expected_revision: 0, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 },
            space: self.space,
            limits: SceneLimits::default(),
        };
        let rendered = match self.options.depth.output_stage() {
            Some(output) => render_scene_with_output(scene, &ctx, output, cancel),
            None => render_scene(scene, &ctx, cancel),
        };
        let r = rendered.map_err(|_| {
            if cancel.check().is_err() { ReelError::Canceled } else { ReelError::Unsupported { what: "export-render-failed" } }
        })?;
        Ok(Frame { width: r.width, height: r.height, rgb: decode_f32(&r.colour), alpha: decode_f32(&r.alpha) })
    }

    fn scene(&mut self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, ReelError> {
        self.scenes.scene_at(tick, cancel).map_err(|_| {
            if cancel.check().is_err() { ReelError::Canceled } else { ReelError::Unsupported { what: "export-composition-unavailable" } }
        })
    }

    /// The frame at composition tick `tick`, on the depth's levels (module docs).
    pub fn frame_at(&mut self, tick: u64, cancel: &CancellationToken) -> Result<Frame, ReelError> {
        let depth = self.options.depth;
        self.frame_unquantised(tick, cancel).map(|f| depth.quantise_frame(f))
    }

    fn frame_unquantised(&mut self, tick: u64, cancel: &CancellationToken) -> Result<Frame, ReelError> {
        let t = i64::try_from(tick).map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?;
        let base = self.scene(t, cancel)?;
        let Some(blur) = self.blur.clone() else {
            let f = self.render_scene(&base, cancel)?;
            self.renders = self.renders.saturating_add(1);
            return Ok(f);
        };
        let pixels = usize::try_from(u64::from(self.width).saturating_mul(u64::from(self.height))).unwrap_or(usize::MAX);
        let mut acc_rgb = vec![0f64; pixels.saturating_mul(3)];
        let mut acc_a = vec![0f64; pixels];
        let ticks = blur.sample_ticks(tick, self.ticks_per_frame);
        for st in &ticks {
            let sample = self.scene(*st, cancel)?;
            let mut composed = sample.clone();
            if !blur.layers.is_empty() {
                composed.layers = sample
                    .layers
                    .iter()
                    .filter_map(|l| {
                        if blur.layers.contains(&l.layer_id) {
                            Some(l.clone())
                        } else {
                            base.layers.iter().find(|b| b.layer_id == l.layer_id).cloned()
                        }
                    })
                    .collect();
                // Unblurred layers present at T but absent at the sample tick keep their place at T.
                for (i, b) in base.layers.iter().enumerate() {
                    if !blur.layers.contains(&b.layer_id) && !composed.layers.iter().any(|c| c.layer_id == b.layer_id) {
                        let at = i.min(composed.layers.len());
                        composed.layers.insert(at, b.clone());
                    }
                }
            }
            let f = self.render_scene(&composed, cancel)?;
            for (i, a) in f.alpha.iter().enumerate() {
                let a = f64::from(*a);
                if let Some(slot) = acc_a.get_mut(i) {
                    *slot += a;
                }
                for c in 0..3 {
                    let j = i.saturating_mul(3).saturating_add(c);
                    if let (Some(slot), Some(v)) = (acc_rgb.get_mut(j), f.rgb.get(j)) {
                        *slot += f64::from(*v) * a;
                    }
                }
            }
        }
        self.renders = self.renders.saturating_add(1);
        let n = ticks.len().max(1) as f64;
        let mut rgb = Vec::with_capacity(acc_rgb.len());
        let mut alpha = Vec::with_capacity(acc_a.len());
        for (i, a) in acc_a.iter().enumerate() {
            let mean_a = a / n;
            alpha.push(mean_a as f32);
            for c in 0..3 {
                let p = acc_rgb.get(i.saturating_mul(3).saturating_add(c)).copied().unwrap_or(0.0) / n;
                rgb.push(if mean_a > 0.0 { (p / mean_a) as f32 } else { 0.0 });
            }
        }
        Ok(Frame { width: self.width, height: self.height, rgb, alpha })
    }
}

/// Frames of one span rendered once and shared by several output modules (module docs).
pub struct SharedFrames<S> {
    inner: Mutex<SharedState<S>>,
}

struct SharedState<S> {
    renderer: FrameRenderer<S>,
    start: u64,
    consumers: u32,
    cache: BTreeMap<u64, (Arc<Frame>, u32)>,
}

impl<S: CompositionScenes> SharedFrames<S> {
    /// `consumers` output modules read frames `0..frames` of the span starting at `start`; the whole
    /// span must fit `max_bytes` (16 bytes per pixel per frame), checked here.
    pub fn new(renderer: FrameRenderer<S>, start: u64, frames: u64, consumers: u32, max_bytes: u64) -> Result<Arc<Self>, MonitorFail> {
        let per = u64::from(renderer.width).saturating_mul(u64::from(renderer.height)).saturating_mul(16);
        if consumers > 1 && per.saturating_mul(frames) > max_bytes {
            return Err(fail("render_once_cache_budget", format!("{frames} frames of {per} bytes exceed {max_bytes}")));
        }
        Ok(Arc::new(Self { inner: Mutex::new(SharedState { renderer, start, consumers: consumers.max(1), cache: BTreeMap::new() }) }))
    }

    /// Frame renders so far (each frame of the span once, whatever the module count).
    pub fn renders(&self) -> u64 {
        self.inner.lock().map_or(0, |s| s.renderer.renders())
    }

    /// Frame `index` of the span (rendered on first request, dropped after the last consumer).
    pub fn frame(&self, index: u64, cancel: &CancellationToken) -> Result<Arc<Frame>, ReelError> {
        let mut s = self.inner.lock().map_err(|_| ReelError::Unsupported { what: "export-shared-frames-poisoned" })?;
        let consumers = s.consumers;
        if let Some((frame, taken)) = s.cache.get_mut(&index) {
            *taken = taken.saturating_add(1);
            let out = frame.clone();
            if *taken >= consumers {
                s.cache.remove(&index);
            }
            return Ok(out);
        }
        let tick = index
            .checked_mul(s.renderer.ticks_per_frame)
            .and_then(|t| t.checked_add(s.start))
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        let frame = Arc::new(s.renderer.frame_at(tick, cancel)?);
        if consumers > 1 {
            s.cache.insert(index, (frame.clone(), 1));
        }
        Ok(frame)
    }
}

/// Output channels of a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputChannels {
    Rgb,
    Alpha,
}

/// STU-MOT-122 post of one output module (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputPost {
    /// `[top, left, bottom, right]`, negative expands.
    pub crop: [i32; 4],
    /// Final size after the crop (`None` = the cropped size).
    pub resize: Option<(u32, u32)>,
    pub channels: OutputChannels,
    /// RGB as premultiplied colour (over black) instead of straight colour.
    pub premultiplied: bool,
}

impl Default for OutputPost {
    fn default() -> Self {
        Self { crop: [0; 4], resize: None, channels: OutputChannels::Rgb, premultiplied: true }
    }
}

impl OutputPost {
    /// Output size for a `width x height` composition.
    pub fn size(&self, width: u32, height: u32) -> Result<(u32, u32), MonitorFail> {
        let [t, l, b, r] = self.crop.map(i64::from);
        let w = i64::from(width).saturating_sub(l).saturating_sub(r);
        let h = i64::from(height).saturating_sub(t).saturating_sub(b);
        if !(1..=30_000).contains(&w) || !(1..=30_000).contains(&h) {
            return Err(fail("output_crop", "the crop leaves no pixel or exceeds 30000"));
        }
        let (w, h) = (u32::try_from(w).unwrap_or(1), u32::try_from(h).unwrap_or(1));
        match self.resize {
            None => Ok((w, h)),
            Some((rw, rh)) if (1..=30_000).contains(&rw) && (1..=30_000).contains(&rh) => Ok((rw, rh)),
            Some(_) => Err(fail("output_resize", "resize width and height 1..=30000")),
        }
    }

    /// Premultiplied RGBA of the cropped frame (transparent outside the source).
    fn crop_premul(&self, f: &Frame) -> (u32, u32, Vec<[f32; 4]>) {
        let [t, l, b, r] = self.crop.map(i64::from);
        let w = i64::from(f.width).saturating_sub(l).saturating_sub(r).max(1);
        let h = i64::from(f.height).saturating_sub(t).saturating_sub(b).max(1);
        let mut out = Vec::with_capacity(usize::try_from(w.saturating_mul(h)).unwrap_or(0));
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x.saturating_add(l), y.saturating_add(t));
                let inside = sx >= 0 && sy >= 0 && sx < i64::from(f.width) && sy < i64::from(f.height);
                let px = if inside {
                    let i = usize::try_from(sy.saturating_mul(i64::from(f.width)).saturating_add(sx)).unwrap_or(0);
                    let a = f.alpha.get(i).copied().unwrap_or(0.0);
                    let c = |k: usize| f.rgb.get(i.saturating_mul(3).saturating_add(k)).copied().unwrap_or(0.0) * a;
                    [c(0), c(1), c(2), a]
                } else {
                    [0.0; 4]
                };
                out.push(px);
            }
        }
        (u32::try_from(w).unwrap_or(1), u32::try_from(h).unwrap_or(1), out)
    }

    /// Premultiplied RGBA of the cropped and resized frame, `out_width * out_height` pixels.
    fn resolved(&self, f: &Frame) -> (u32, u32, Vec<[f32; 4]>) {
        let (w, h, src) = self.crop_premul(f);
        let (ow, oh) = self.resize.unwrap_or((w, h));
        let at = |x: i64, y: i64| -> [f32; 4] {
            let x = x.clamp(0, i64::from(w).saturating_sub(1));
            let y = y.clamp(0, i64::from(h).saturating_sub(1));
            src.get(usize::try_from(y.saturating_mul(i64::from(w)).saturating_add(x)).unwrap_or(0)).copied().unwrap_or([0.0; 4])
        };
        let mut out = Vec::with_capacity(usize::try_from(u64::from(ow).saturating_mul(u64::from(oh))).unwrap_or(0));
        for y in 0..oh {
            for x in 0..ow {
                let p = if (ow, oh) == (w, h) {
                    at(i64::from(x), i64::from(y))
                } else {
                    let fx = (f64::from(x) + 0.5) * f64::from(w) / f64::from(ow) - 0.5;
                    let fy = (f64::from(y) + 0.5) * f64::from(h) / f64::from(oh) - 0.5;
                    let (x0, y0) = (fx.floor(), fy.floor());
                    let (ax, ay) = ((fx - x0) as f32, (fy - y0) as f32);
                    let (x0, y0) = (x0 as i64, y0 as i64);
                    let (p00, p10) = (at(x0, y0), at(x0.saturating_add(1), y0));
                    let (p01, p11) = (at(x0, y0.saturating_add(1)), at(x0.saturating_add(1), y0.saturating_add(1)));
                    let mut q = [0f32; 4];
                    for (k, v) in q.iter_mut().enumerate() {
                        let top = p00[k] + (p10[k] - p00[k]) * ax;
                        let bottom = p01[k] + (p11[k] - p01[k]) * ax;
                        *v = top + (bottom - top) * ay;
                    }
                    q
                };
                out.push(p);
            }
        }
        (ow, oh, out)
    }

    /// The module's `rgb_f32le` frame (module docs).
    pub fn apply(&self, f: &Frame) -> Vec<u8> {
        let (ow, oh, pixels) = self.resolved(f);
        let mut out = Vec::with_capacity(usize::try_from(u64::from(ow).saturating_mul(u64::from(oh)).saturating_mul(12)).unwrap_or(0));
        for p in pixels {
            let rgb = match self.channels {
                OutputChannels::Alpha => [p[3]; 3],
                OutputChannels::Rgb if self.premultiplied => [p[0], p[1], p[2]],
                OutputChannels::Rgb => Self::straight(p),
            };
            for c in rgb {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out
    }

    /// The module's `rgba_f32le` frame (lane SMB-AE-RENDER-QUEUE-2; STU-MOT-122 `channels`
    /// RGB+alpha with `color`): colour as `premultiplied` selects (premultiplied colour is the
    /// straight colour times alpha, straight colour is zero where alpha is zero), then the straight
    /// alpha; `channels` is not consulted. No reel encoder carries alpha yet, so the render queue
    /// still refuses `rgb_alpha` modules (`output_alpha_codec`); this is the frame such an encoder
    /// or a still writer would take.
    pub fn apply_rgba(&self, f: &Frame) -> Vec<u8> {
        let (ow, oh, pixels) = self.resolved(f);
        let mut out = Vec::with_capacity(usize::try_from(u64::from(ow).saturating_mul(u64::from(oh)).saturating_mul(16)).unwrap_or(0));
        for p in pixels {
            let rgb = if self.premultiplied { [p[0], p[1], p[2]] } else { Self::straight(p) };
            for c in rgb.into_iter().chain([p[3]]) {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
        out
    }

    fn straight(p: [f32; 4]) -> [f32; 3] {
        if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] } else { [0.0; 3] }
    }
}

/// One output module's frames as an export video source.
pub struct CompositionVideo<S> {
    shared: Arc<SharedFrames<S>>,
    post: OutputPost,
    ticks_per_frame: u64,
    frames: u64,
    format: VideoFormat,
}

impl<S: CompositionScenes> ExportVideoSource for CompositionVideo<S> {
    fn format(&self) -> VideoFormat {
        self.format
    }
    fn ticks_per_frame(&self) -> u64 {
        self.ticks_per_frame
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn frame(&mut self, index: u64, cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        let frame = self.shared.frame(index, cancel)?;
        rgb_to_planes(&self.post.apply(&frame), &self.format)
    }
}

struct PlacedLayer {
    samples: Arc<Vec<f32>>,
    start: u64,
    gain: f32,
}

/// The composition's audio layers summed over the exported span (stereo).
pub struct CompositionAudio {
    layers: Vec<PlacedLayer>,
    start: u64,
    frames: u64,
    rate: u32,
}

impl ExportAudioSource for CompositionAudio {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u16 {
        2
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn render(&mut self, first: u64, frames: u32, out: &mut Vec<f32>, cancel: &CancellationToken) -> Result<(), ReelError> {
        cancel.check().map_err(|_| ReelError::Canceled)?;
        let base = self.start.checked_add(first).ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        for f in 0..u64::from(frames) {
            let s = base.checked_add(f).ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
            let (mut l, mut r) = (0f32, 0f32);
            for layer in &self.layers {
                let Some(i) = s.checked_sub(layer.start) else { continue };
                let Some(at) = usize::try_from(i).ok().and_then(|i| i.checked_mul(2)) else { continue };
                if let (Some(a), Some(b)) = (layer.samples.get(at), at.checked_add(1).and_then(|j| layer.samples.get(j))) {
                    l += a * layer.gain;
                    r += b * layer.gain;
                }
            }
            out.push(l);
            out.push(r);
        }
        Ok(())
    }
}

/// The span `[in, out)` of an export with its frame duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompositionSpan {
    pub ticks_per_frame: u64,
    pub in_tick: u64,
    pub out_tick: u64,
}

impl CompositionSpan {
    /// Frame count; refuses partial or empty spans.
    pub fn frames(&self) -> Result<u64, MonitorFail> {
        let tpf = self.ticks_per_frame;
        if tpf == 0 {
            return Err(fail("export_range", "zero frame duration"));
        }
        if self.out_tick <= self.in_tick || !self.in_tick.is_multiple_of(tpf) || !self.out_tick.is_multiple_of(tpf) {
            return Err(fail("export_range", "in/out must be whole frames, in < out"));
        }
        Ok(self.out_tick.saturating_sub(self.in_tick).checked_div(tpf).unwrap_or(0))
    }
}

/// What to export from a composition (one output module).
pub struct CompositionExport<S> {
    pub scenes: S,
    pub width: u32,
    pub height: u32,
    /// Composition frame duration (ticks).
    pub ticks_per_frame: u64,
    /// First exported composition tick and the end (exclusive), both whole frames.
    pub in_tick: u64,
    pub out_tick: u64,
    pub recipe: ExportRecipe,
    pub bit_depth: u8,
    pub chroma: ChromaFormat,
    /// Colour tag of the scenes (never converted).
    pub space: ColourTag,
    /// Audio layers and the export sample rate (`None`: no audio track).
    pub audio: Option<(Vec<CompositionAudioLayer>, u32)>,
    pub output: PathBuf,
    pub overwrite: bool,
    pub artifact: String,
}

fn whole_samples(ticks: u64, rate: u32) -> Result<u64, MonitorFail> {
    let num = u128::from(ticks).saturating_mul(u128::from(rate));
    if !num.is_multiple_of(u128::from(TICKS_PER_SECOND)) {
        return Err(fail("export_range", "not a whole audio sample"));
    }
    num.checked_div(u128::from(TICKS_PER_SECOND))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| fail("export_range", "overflow"))
}

/// The audio source of a span (`None`: no audio track).
pub fn composition_audio(span: &CompositionSpan, audio: Option<(Vec<CompositionAudioLayer>, u32)>) -> Result<Option<CompositionAudio>, MonitorFail> {
    let Some((layers, rate)) = audio else { return Ok(None) };
    if layers.len() > MAX_AUDIO_LAYERS {
        return Err(fail("export_audio", "too many audio layers"));
    }
    let limits = ExportLimits::default();
    let start = whole_samples(span.in_tick, rate)?;
    let end = u128::from(span.out_tick)
        .saturating_mul(u128::from(rate))
        .checked_div(u128::from(TICKS_PER_SECOND))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| fail("export_range", "overflow"))?;
    let mut placed = Vec::with_capacity(layers.len());
    for l in layers {
        if !l.samples.len().is_multiple_of(2) {
            return Err(fail("export_audio", "odd interleaved stereo buffer"));
        }
        if (l.samples.len() as u64).checked_div(2).unwrap_or(0) > limits.max_audio_frames || !l.gain.is_finite() {
            return Err(fail("export_audio", "audio layer over budget or non-finite gain"));
        }
        placed.push(PlacedLayer { start: whole_samples(l.start_tick, rate)?, samples: l.samples, gain: l.gain });
    }
    Ok(Some(CompositionAudio { layers: placed, start, frames: end.saturating_sub(start), rate }))
}

/// One module of a render-once item: its recipe, post, output and audio.
pub struct ModuleOutput {
    pub recipe: ExportRecipe,
    pub post: OutputPost,
    pub bit_depth: u8,
    pub chroma: ChromaFormat,
    pub audio: Option<(Vec<CompositionAudioLayer>, u32)>,
    pub output: PathBuf,
    pub overwrite: bool,
    pub artifact: String,
}

/// Reel jobs of several output modules sharing one render of `span` (module docs).
pub fn shared_module_jobs<S: CompositionScenes + 'static>(
    shared: &Arc<SharedFrames<S>>,
    width: u32,
    height: u32,
    span: CompositionSpan,
    modules: Vec<ModuleOutput>,
) -> Result<Vec<ExportJob>, MonitorFail> {
    let frames = span.frames()?;
    let mut jobs = Vec::with_capacity(modules.len());
    for m in modules {
        let (w, h) = m.post.size(width, height)?;
        let format = VideoFormat {
            width: w,
            height: h,
            bit_depth: m.bit_depth,
            chroma: m.chroma,
            color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
        };
        let audio: Option<Box<dyn ExportAudioSource>> = composition_audio(&span, m.audio)?.map(|a| Box::new(a) as Box<dyn ExportAudioSource>);
        let video = CompositionVideo { shared: shared.clone(), post: m.post, ticks_per_frame: span.ticks_per_frame, frames, format };
        jobs.push(ExportJob {
            recipe: m.recipe,
            video: Some(Box::new(video)),
            audio,
            span: SourceSpan { start_ticks: span.in_tick, end_ticks: span.out_tick },
            output: m.output,
            overwrite: m.overwrite,
            artifact: m.artifact,
            limits: ExportLimits::default(),
        });
    }
    Ok(jobs)
}

/// Build the reel export job of a composition span (one module, no blur, default post).
pub fn composition_export_job<S: CompositionScenes + 'static>(c: CompositionExport<S>) -> Result<ExportJob, MonitorFail> {
    let span = CompositionSpan { ticks_per_frame: c.ticks_per_frame, in_tick: c.in_tick, out_tick: c.out_tick };
    let frames = span.frames()?;
    if c.width == 0 || c.height == 0 {
        return Err(fail("export_raster", "zero raster"));
    }
    let renderer = FrameRenderer::new(c.scenes, c.space, c.width, c.height, c.ticks_per_frame, None)?;
    let shared = SharedFrames::new(renderer, c.in_tick, frames, 1, DEFAULT_SHARED_CACHE_BYTES)?;
    let module = ModuleOutput {
        recipe: c.recipe,
        post: OutputPost::default(),
        bit_depth: c.bit_depth,
        chroma: c.chroma,
        audio: c.audio,
        output: c.output,
        overwrite: c.overwrite,
        artifact: c.artifact,
    };
    shared_module_jobs(&shared, c.width, c.height, span, vec![module])?
        .pop()
        .ok_or_else(|| fail("export_range", "no job"))
}

// ---------------------------------------------------------------------------------------------
// Lane SMB-DELIVERY-QUEUE-1: a PNG image sequence sink over the shared frames (STU-MOT-122; motion
// step 84). Not a reel job: each frame goes through `OutputPost` and interop-raster's PNG encoder.
// ---------------------------------------------------------------------------------------------

/// Digits of a PNG sequence frame number: `starting_number`'s hard_max 9999999 (STU-MOT-122) fits, so names
/// sort (lead ruling 2026-11-08).
pub const PNG_FRAME_DIGITS: usize = 7;
/// Largest PNG frame number (STU-MOT-122 `starting_number` hard_max).
pub const PNG_MAX_FRAME_NUMBER: u64 = 9_999_999;

/// Channels of a PNG module (STU-MOT-122 `channels`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngChannels {
    Rgb,
    /// A grey matte of the alpha (one channel).
    Alpha,
    RgbAlpha,
}

/// The transfer a PNG module applies to the rendered linear working-space colour (module docs of `render_queue`;
/// review 44da1364 F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngTransfer {
    /// IEC 61966-2-1 sRGB OETF (the viewer's and the document PNG export's encoding).
    Srgb,
    /// ITU-R BT.709 OETF: `4.5 L` below 0.018, else `1.099 L^0.45 - 0.099`.
    Rec709,
    /// `preserve_rgb`: the rendered samples as they are.
    AsRendered,
}

impl PngTransfer {
    /// `v` clamped to 0..=1 (NaN is 0), then encoded.
    pub fn encode(self, v: f32) -> f32 {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match self {
            Self::Srgb if v <= 0.003_130_8 => v * 12.92,
            Self::Srgb => 1.055 * v.powf(1.0 / 2.4) - 0.055,
            Self::Rec709 if v < 0.018 => v * 4.5,
            Self::Rec709 => 1.099 * v.powf(0.45) - 0.099,
            Self::AsRendered => v,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::Rec709 => "rec709",
            Self::AsRendered => "as_rendered",
        }
    }
}

/// A PNG module's colour (STU-MOT-122 colour group): the transfer, and whether an RGB file carries the matching
/// chunk (`embed_color_space`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PngColour {
    pub transfer: PngTransfer,
    pub embed: bool,
}

impl PngColour {
    /// The colour chunk an RGB or RGBA file of this colour carries: `iCCP` (sRGB v4 profile), `cICP` (BT.709) or none.
    pub fn chunk(self, colour: hsk_studio_interop_raster::Colour) -> Option<&'static str> {
        if !self.embed || colour == hsk_studio_interop_raster::Colour::Gray {
            return None;
        }
        match self.transfer {
            PngTransfer::Srgb => Some("iCCP"),
            PngTransfer::Rec709 => Some("cICP"),
            PngTransfer::AsRendered => None,
        }
    }
}

/// `png` with a `cICP` chunk (BT.709 primaries 1, transfer 1, matrix 0 = RGB, full range) right after `IHDR`, where
/// the PNG specification (third edition) places it: before `PLTE` and `IDAT`.
fn with_cicp_bt709(png: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    // Signature (8) + IHDR: length (4), type (4), 13 data bytes, CRC (4).
    const AFTER_IHDR: usize = 33;
    if png.get(12..16) != Some(b"IHDR") || png.len() < AFTER_IHDR {
        return Err("png_encode_failed");
    }
    let body: [u8; 8] = [b'c', b'I', b'C', b'P', 1, 1, 0, 1];
    let mut out = Vec::with_capacity(png.len().saturating_add(16));
    out.extend_from_slice(&png[..AFTER_IHDR]);
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend_from_slice(&body);
    // The PNG chunk CRC is the ISO 3309 CRC-32 of type and data: the workspace's one CRC-32 (package, D16).
    out.extend_from_slice(&hsk_studio_package::crc32_of(&body).to_be_bytes());
    out.extend_from_slice(&png[AFTER_IHDR..]);
    Ok(out)
}

/// One PNG sequence output module (module docs of `render_queue`): frame `i` of the span is written to
/// `<stem>_<number>.png`, `number` = `first_number + i` with [`PNG_FRAME_DIGITS`] zero-padded digits, where
/// `<stem>` is `pattern` without its `.png` suffix.
#[derive(Clone, Debug, PartialEq)]
pub struct PngModule {
    pub post: OutputPost,
    pub channels: PngChannels,
    pub colour: PngColour,
    /// 8 or 16 bits per sample.
    pub depth: u8,
    pub compression: hsk_studio_interop_raster::PngCompression,
    pub first_number: u64,
    /// The pattern under the output root, and as the record names it (receipts name files relative).
    pub pattern: PathBuf,
    pub relative: String,
    pub overwrite: bool,
}

impl PngModule {
    /// Checks a module of `frames` frames before the first frame: [`ExportLimits::max_frames`]
    /// (`export_frame_limit`), the last number (`output_frame_number_range`), depth 8 or 16 (`output_depth`)
    /// and a `.png` pattern (`output_png_path`).
    pub fn check(&self, frames: u64) -> Result<(), MonitorFail> {
        if frames > ExportLimits::default().max_frames {
            return Err(fail("export_frame_limit", format!("{frames} frames exceed {}", ExportLimits::default().max_frames)));
        }
        let last = frames.checked_sub(1).and_then(|n| self.first_number.checked_add(n));
        if last.is_none_or(|n| n > PNG_MAX_FRAME_NUMBER) {
            return Err(fail("output_frame_number_range", format!("frame numbers from {} for {frames} frames pass {PNG_MAX_FRAME_NUMBER}", self.first_number)));
        }
        if !matches!(self.depth, 8 | 16) {
            return Err(fail("output_depth", "a PNG sequence writes 8 or 16 bits per sample"));
        }
        if png_stem(&self.relative).is_none() {
            return Err(fail("output_png_path", format!("{}: a PNG sequence pattern ends in .png", self.relative)));
        }
        Ok(())
    }

    /// The file of frame number `number`: under the root, and relative.
    pub fn frame_path(&self, number: u64) -> Option<(PathBuf, String)> {
        let stem = png_stem(&self.relative)?;
        let rel = format!("{stem}_{number:0width$}.png", width = PNG_FRAME_DIGITS);
        let abs_stem = png_stem(&self.pattern.to_string_lossy())?.to_owned();
        Some((PathBuf::from(format!("{abs_stem}_{number:0width$}.png", width = PNG_FRAME_DIGITS)), rel))
    }

    /// The PNG bytes of one frame (module docs of `render_queue`: straight colour through the module's transfer,
    /// premultiplied after encoding when `post.premultiplied`; alpha linear; samples clamped to 0..=1 and rounded
    /// to the depth's levels, NaN is 0; the colour chunk of [`PngColour::chunk`]).
    pub fn encode(&self, f: &Frame) -> Result<Vec<u8>, &'static str> {
        use hsk_studio_interop_raster::{Colour, PngEncodeRequest, Samples};
        let (w, h) = self.post.size(f.width, f.height).map_err(|e| e.code)?;
        // Straight colour and alpha per pixel (crop and resize applied), whatever `color` says.
        let straight = OutputPost { premultiplied: false, ..self.post }.apply_rgba(f);
        let floats: Vec<f32> = straight.chunks_exact(4).map(|b| b.try_into().map_or(0.0, f32::from_le_bytes)).collect();
        let t = |v: f32| self.colour.transfer.encode(v);
        let alpha = |a: f32| if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        let premultiplied = self.post.premultiplied;
        let colour = match self.channels {
            PngChannels::RgbAlpha => Colour::Rgba,
            PngChannels::Rgb => Colour::Rgb,
            PngChannels::Alpha => Colour::Gray,
        };
        let mut picked: Vec<f32> = Vec::with_capacity(floats.len());
        for px in floats.chunks_exact(4) {
            let (rgb, a) = ([px[0], px[1], px[2]], alpha(px[3]));
            match self.channels {
                PngChannels::RgbAlpha if premultiplied => picked.extend(rgb.map(|c| t(c) * a).into_iter().chain([a])),
                PngChannels::RgbAlpha => picked.extend(rgb.map(t).into_iter().chain([a])),
                // RGB premultiplied is the composite over black, encoded.
                PngChannels::Rgb if premultiplied => picked.extend(rgb.map(|c| t(c * a))),
                PngChannels::Rgb => picked.extend(rgb.map(t)),
                PngChannels::Alpha => picked.push(a),
            }
        }
        let level = |v: f32, max: f32| if v.is_finite() { (v.clamp(0.0, 1.0) * max + 0.5).floor() } else { 0.0 };
        let samples = if self.depth == 16 {
            Samples::U16(picked.iter().map(|v| level(*v, 65535.0) as u16).collect())
        } else {
            Samples::U8(picked.iter().map(|v| level(*v, 255.0) as u8).collect())
        };
        let chunk = self.colour.chunk(colour);
        let icc = (chunk == Some("iCCP")).then_some(crate::view::develop::SRGB_V4_ICC);
        let png = hsk_studio_interop_raster::encode_png(&PngEncodeRequest { width: w, height: h, colour, samples: &samples, icc, compression: self.compression }).map_err(|_| "png_encode_failed")?;
        if chunk == Some("cICP") { with_cicp_bt709(png) } else { Ok(png) }
    }
}

/// `pattern` without its `.png` suffix (any case); `None` without the suffix or with an empty stem.
fn png_stem(pattern: &str) -> Option<&str> {
    let cut = pattern.len().checked_sub(4)?;
    let (stem, ext) = (pattern.get(..cut)?, pattern.get(cut..)?);
    (ext.eq_ignore_ascii_case(".png") && !stem.is_empty() && !stem.ends_with(['/', '\\'])).then_some(stem)
}

/// The receipt of one PNG sequence (STU-MOT-125: a stopped render names its partial output).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngSequenceReceipt {
    /// `complete`, `canceled` or `failed`.
    pub outcome: &'static str,
    pub code: Option<&'static str>,
    pub detail: Option<String>,
    pub frames_total: u64,
    /// The frame names, relative as the record names them: `<stem>_%07d.png` (review 44da1364 F6: the pattern and
    /// the range, never one entry per frame).
    pub pattern: String,
    /// The number of the first frame, and how many frames were written in order from it (also when canceled or
    /// failed).
    pub first_number: u64,
    pub frames_written: u64,
    pub bytes: u64,
    /// The transfer applied and the colour chunk written (`render_queue` module docs).
    pub transfer: &'static str,
    pub chunk: Option<&'static str>,
}

impl PngSequenceReceipt {
    /// The number of the last frame written, if any.
    pub fn last_number(&self) -> Option<u64> {
        self.frames_written.checked_sub(1).and_then(|n| self.first_number.checked_add(n))
    }

    /// The receipt's artifacts: one range `{pattern, first_number, last_number, frames}`, or none.
    pub fn artifacts_json(&self) -> serde_json::Value {
        match self.last_number() {
            Some(last) => serde_json::json!([{"pattern": self.pattern, "first_number": self.first_number, "last_number": last, "frames": self.frames_written}]),
            None => serde_json::json!([]),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({"format": "png_sequence", "outcome": self.outcome, "code": self.code, "detail": self.detail, "frames_total": self.frames_total,
            "pattern": self.pattern, "first_number": self.first_number, "frames_written": self.frames_written, "last_number": self.last_number(),
            "bytes": self.bytes, "colour": {"transfer": self.transfer, "chunk": self.chunk}})
    }
}

static PNG_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `bytes` at `target` through a sibling temp created with `create_new`: without `overwrite` a hard
/// link publishes only where no entry exists (`export-output-exists`), atomically, with a re-checked rename
/// where the file system has no hard links; with it the rename replaces. The temp never stays.
fn publish_png(target: &Path, bytes: &[u8], overwrite: bool) -> Result<(), &'static str> {
    use std::io::Write;
    let name = target.file_name().ok_or("png_write_failed")?.to_string_lossy().into_owned();
    let n = PNG_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = target.with_file_name(format!(".{name}.partial-{}-{n}", std::process::id()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err("png_write_failed");
    }
    let published = if overwrite {
        std::fs::rename(&temp, target).map_err(|_| "png_write_failed")
    } else {
        match std::fs::hard_link(&temp, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err("export-output-exists"),
            Err(_) if std::fs::symlink_metadata(target).is_ok() => Err("export-output-exists"),
            Err(_) => std::fs::rename(&temp, target).map_err(|_| "png_write_failed"),
        }
    };
    let _ = std::fs::remove_file(&temp);
    published
}

/// Frame `i` of a PNG sequence (the shared render, or a test closure).
pub type PngFrameSource<'a> = &'a dyn Fn(u64, &CancellationToken) -> Result<Arc<Frame>, ReelError>;

/// Writes the PNG sequence of `frames` frames, frame `i` from `frame` (module docs). Checked before the
/// first frame: [`PngModule::check`] and that the pattern's folder exists (`output_directory_missing`). A
/// cancel stops before the next frame; a failed frame or write stops there; the receipt names the frames
/// written either way (pattern and range). `frames` up to [`ExportLimits::max_frames`] (4,000,000) inclusive.
pub fn run_png_sequence(frame: PngFrameSource<'_>, frames: u64, m: &PngModule, cancel: &CancellationToken) -> PngSequenceReceipt {
    let pattern = png_stem(&m.relative).map_or_else(|| m.relative.clone(), |s| format!("{s}_%0{PNG_FRAME_DIGITS}d.png"));
    let colour = if m.channels == PngChannels::Alpha { hsk_studio_interop_raster::Colour::Gray } else { hsk_studio_interop_raster::Colour::Rgba };
    let transfer = if m.channels == PngChannels::Alpha { "linear_alpha" } else { m.colour.transfer.code() };
    let mut r = PngSequenceReceipt {
        outcome: "complete",
        code: None,
        detail: None,
        frames_total: frames,
        pattern,
        first_number: m.first_number,
        frames_written: 0,
        bytes: 0,
        transfer,
        chunk: m.colour.chunk(colour),
    };
    let stop = |mut r: PngSequenceReceipt, outcome: &'static str, code: &'static str, detail: String| {
        r.outcome = outcome;
        r.code = Some(code);
        r.detail = Some(detail);
        r
    };
    if let Err(e) = m.check(frames) {
        return stop(r, "failed", e.code, e.detail);
    }
    if !m.pattern.parent().is_some_and(Path::is_dir) {
        return stop(r, "failed", "output_directory_missing", m.relative.clone());
    }
    for i in 0..frames {
        if cancel.check().is_err() {
            return stop(r, "canceled", "MEDIA_CANCELED", format!("canceled before frame {i}"));
        }
        let Some((path, rel)) = m.first_number.checked_add(i).and_then(|n| m.frame_path(n)) else {
            return stop(r, "failed", "output_frame_number_range", format!("frame {i}"));
        };
        let f = match frame(i, cancel) {
            Ok(f) => f,
            Err(ReelError::Canceled) => return stop(r, "canceled", "MEDIA_CANCELED", format!("canceled at frame {i}")),
            Err(e) => return stop(r, "failed", e.code(), format!("frame {i}: {e}")),
        };
        let bytes = match m.encode(&f) {
            Ok(b) => b,
            Err(code) => return stop(r, "failed", code, format!("frame {i}")),
        };
        if let Err(code) = publish_png(&path, &bytes, m.overwrite) {
            return stop(r, "failed", code, format!("frame {i}: {rel}"));
        }
        r.bytes = r.bytes.saturating_add(bytes.len() as u64);
        r.frames_written = r.frames_written.saturating_add(1);
    }
    r
}

#[cfg(test)]
mod png_sequence_tests {
    //! Lane SMB-DELIVERY-QUEUE-1: the PNG sequence sink alone (its frames from a closure): a cancel names the
    //! frames written, hostile counts refuse before the first frame, 16-bit grey alpha samples.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-png-sequence-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn module(dir: &Path, channels: PngChannels, depth: u8) -> PngModule {
        PngModule {
            post: OutputPost { crop: [0; 4], resize: None, channels: OutputChannels::Rgb, premultiplied: false },
            channels,
            colour: PngColour { transfer: PngTransfer::Srgb, embed: true },
            depth,
            compression: hsk_studio_interop_raster::PngCompression::Fast,
            first_number: 0,
            pattern: dir.join("f.png"),
            relative: "out/f.png".into(),
            overwrite: false,
        }
    }
    fn frame() -> Arc<Frame> {
        Arc::new(Frame { width: 2, height: 2, rgb: vec![0.5; 12], alpha: vec![0.25; 4] })
    }

    #[test]
    fn a_cancel_names_the_frames_written() {
        let dir = scratch("cancel");
        let m = module(&dir, PngChannels::RgbAlpha, 8);
        let cancel = CancellationToken::default();
        let served = AtomicU64::new(0);
        let source = |i: u64, _: &CancellationToken| {
            served.fetch_add(1, Ordering::Relaxed);
            if i == 1 {
                cancel.cancel();
            }
            Ok(frame())
        };
        let r = run_png_sequence(&source, 5, &m, &cancel);
        assert_eq!((r.outcome, r.code), ("canceled", Some("MEDIA_CANCELED")), "{r:?}");
        assert_eq!((r.pattern.as_str(), r.first_number, r.frames_written, r.last_number()), ("out/f_%07d.png", 0, 2, Some(1)), "{r:?}");
        assert_eq!(r.artifacts_json(), serde_json::json!([{"pattern": "out/f_%07d.png", "first_number": 0, "last_number": 1, "frames": 2}]));
        assert_eq!(served.load(Ordering::Relaxed), 2);
        assert!(dir.join("f_0000001.png").is_file() && !dir.join("f_0000002.png").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().contains(".partial-")).collect();
        assert!(leftovers.is_empty(), "no temp stays: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hostile_counts_refuse_before_the_first_frame() {
        let dir = scratch("hostile");
        let served = AtomicU64::new(0);
        let source = |_: u64, _: &CancellationToken| {
            served.fetch_add(1, Ordering::Relaxed);
            Ok(frame())
        };
        let cancel = CancellationToken::default();
        let m = module(&dir, PngChannels::Rgb, 8);
        let r = run_png_sequence(&source, ExportLimits::default().max_frames.saturating_add(1), &m, &cancel);
        assert_eq!(r.code, Some("export_frame_limit"), "{r:?}");
        let mut high = module(&dir, PngChannels::Rgb, 8);
        high.first_number = PNG_MAX_FRAME_NUMBER;
        assert_eq!(run_png_sequence(&source, 2, &high, &cancel).code, Some("output_frame_number_range"));
        assert_eq!(run_png_sequence(&source, 1, &high, &cancel).outcome, "complete", "9999999 itself is a valid number");
        assert!(dir.join("f_9999999.png").is_file());
        assert_eq!(run_png_sequence(&source, 1, &module(&dir, PngChannels::Rgb, 10), &cancel).code, Some("output_depth"));
        assert_eq!(served.load(Ordering::Relaxed), 1, "only the valid sequence asked for a frame");
        let mut missing = module(&dir, PngChannels::Rgb, 8);
        missing.pattern = dir.join("no-such-folder").join("f.png");
        assert_eq!(run_png_sequence(&source, 1, &missing, &cancel).code, Some("output_directory_missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alpha_channel_writes_a_sixteen_bit_grey_matte_and_never_replaces_a_file() {
        let dir = scratch("alpha16");
        let cancel = CancellationToken::default();
        let m = module(&dir, PngChannels::Alpha, 16);
        let source = |_: u64, _: &CancellationToken| Ok(frame());
        let r = run_png_sequence(&source, 1, &m, &cancel);
        assert_eq!(r.outcome, "complete", "{r:?}");
        let bytes = std::fs::read(dir.join("f_0000000.png")).unwrap();
        let img = hsk_studio_interop_raster::decode(&bytes, &hsk_studio_interop_raster::Limits::default(), &cancel).unwrap();
        assert_eq!(img.colour, hsk_studio_interop_raster::Colour::Gray);
        // 0.25 * 65535 + 0.5 = 16384.25 -> 16384.
        assert_eq!(img.samples, hsk_studio_interop_raster::Samples::U16(vec![16384; 4]));
        let again = run_png_sequence(&source, 1, &m, &cancel);
        assert_eq!((again.outcome, again.code, again.frames_written), ("failed", Some("export-output-exists"), 0), "{again:?}");
        assert_eq!(again.artifacts_json(), serde_json::json!([]));
        assert_eq!(std::fs::read(dir.join("f_0000000.png")).unwrap(), bytes, "the existing file is untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The frame bound is inclusive (lead ruling 2026-11-11): exactly `max_frames` (4,000,000) passes the check,
    /// one more is `export_frame_limit`.
    #[test]
    fn exactly_max_frames_is_allowed() {
        let m = module(Path::new("."), PngChannels::Rgb, 8);
        let max = ExportLimits::default().max_frames;
        assert_eq!(max, 4_000_000);
        assert!(m.check(max).is_ok());
        assert_eq!(m.check(max.saturating_add(1)).map_err(|e| e.code), Err("export_frame_limit"));
    }

    /// Review 44da1364 F3: linear mid-grey 0.5 encodes to sRGB 188 / 48192 and BT.709 180 / 46236 (8 / 16 bits);
    /// premultiplied colour is premultiplied after encoding; alpha stays linear; the chunk follows the space.
    #[test]
    fn mid_grey_encodes_through_the_colour_space_transfer() {
        let cancel = CancellationToken::default();
        let grey = Frame { width: 1, height: 1, rgb: vec![0.5; 3], alpha: vec![1.0] };
        // Frames hold straight colour: 0.5 at alpha 0.5.
        let half = Frame { width: 1, height: 1, rgb: vec![0.5; 3], alpha: vec![0.5] };
        let decode = |bytes: &[u8]| hsk_studio_interop_raster::decode(bytes, &hsk_studio_interop_raster::Limits::default(), &cancel).unwrap();
        for (transfer, want8, want16, chunk) in [(PngTransfer::Srgb, 188u8, 48192u16, Some("iCCP")), (PngTransfer::Rec709, 180, 46236, Some("cICP")), (PngTransfer::AsRendered, 128, 32768, None)] {
            let mut m = module(Path::new("."), PngChannels::RgbAlpha, 8);
            m.colour = PngColour { transfer, embed: true };
            let img = decode(&m.encode(&grey).unwrap());
            assert_eq!(img.samples, hsk_studio_interop_raster::Samples::U8(vec![want8, want8, want8, 255]), "{transfer:?}");
            assert_eq!(img.icc.is_some(), chunk == Some("iCCP"), "{transfer:?}");
            assert_eq!(m.colour.chunk(hsk_studio_interop_raster::Colour::Rgba), chunk);
            m.depth = 16;
            assert_eq!(decode(&m.encode(&grey).unwrap()).samples, hsk_studio_interop_raster::Samples::U16(vec![want16, want16, want16, 65535]), "{transfer:?} 16");
            // Straight 0.5 at alpha 0.5, premultiplied after encoding: half the encoded value, alpha 128.
            m.depth = 8;
            m.post.premultiplied = true;
            let img = decode(&m.encode(&half).unwrap());
            let encoded = (transfer.encode(0.5) * 0.5 * 255.0 + 0.5).floor() as u8;
            assert_eq!(img.samples, hsk_studio_interop_raster::Samples::U8(vec![encoded, encoded, encoded, 128]), "{transfer:?} premultiplied");
        }
        let mut m = module(Path::new("."), PngChannels::RgbAlpha, 8);
        m.colour.embed = false;
        let bytes = m.encode(&grey).unwrap();
        assert!(decode(&bytes).icc.is_none() && !bytes.windows(4).any(|w| w == b"cICP"), "embed off: no colour chunk");
        m.channels = PngChannels::Alpha;
        m.colour = PngColour { transfer: PngTransfer::Rec709, embed: true };
        assert_eq!(decode(&m.encode(&half).unwrap()).samples, hsk_studio_interop_raster::Samples::U8(vec![128]), "the alpha matte stays linear, untagged");
        assert_eq!(hsk_studio_package::crc32_of(b"IEND"), 0xAE42_6082, "the PNG IEND chunk's CRC");
    }
}
