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
use hsk_studio_composite::{SceneComposition, SceneContext, SceneLimits, ScenePixels, render_scene};
use hsk_studio_pigment::blend_math::Exactness;
use hsk_studio_pigment::render_plan::{ColourTag, ExecuteRequest};
use hsk_studio_reel::{
    ChromaFormat, ColorInfo, ColorOrigin, ExportAudioSource, ExportJob, ExportLimits, ExportRecipe,
    ExportVideoSource, ReelError, SourceSpan, VideoFormat,
};
use hsk_studio_render_cpu::Renderer;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
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

/// Renders frames of one composition (with optional motion blur).
pub struct FrameRenderer<S> {
    scenes: S,
    renderer: Renderer,
    space: ColourTag,
    width: u32,
    height: u32,
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
        Ok(Self { scenes, renderer, space, width, height, ticks_per_frame, blur, renders: 0 })
    }

    /// Frame renders so far (motion-blur samples count once per frame).
    pub fn renders(&self) -> u64 {
        self.renders
    }

    fn render_scene(&mut self, scene: &SceneComposition, cancel: &CancellationToken) -> Result<Frame, ReelError> {
        if scene.width != self.width || scene.height != self.height {
            return Err(ReelError::Unsupported { what: "export-composition-size" });
        }
        let ctx = SceneContext {
            backend: &self.renderer,
            pixels: self.scenes.pixels(),
            request: ExecuteRequest { expected_revision: 0, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 },
            space: self.space,
            limits: SceneLimits::default(),
        };
        let r = render_scene(scene, &ctx, cancel).map_err(|_| {
            if cancel.check().is_err() { ReelError::Canceled } else { ReelError::Unsupported { what: "export-render-failed" } }
        })?;
        Ok(Frame { width: r.width, height: r.height, rgb: decode_f32(&r.colour), alpha: decode_f32(&r.alpha) })
    }

    fn scene(&mut self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, ReelError> {
        self.scenes.scene_at(tick, cancel).map_err(|_| {
            if cancel.check().is_err() { ReelError::Canceled } else { ReelError::Unsupported { what: "export-composition-unavailable" } }
        })
    }

    /// The frame at composition tick `tick`.
    pub fn frame_at(&mut self, tick: u64, cancel: &CancellationToken) -> Result<Frame, ReelError> {
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

    /// The module's `rgb_f32le` frame (module docs).
    pub fn apply(&self, f: &Frame) -> Vec<u8> {
        let (w, h, src) = self.crop_premul(f);
        let (ow, oh) = self.resize.unwrap_or((w, h));
        let at = |x: i64, y: i64| -> [f32; 4] {
            let x = x.clamp(0, i64::from(w).saturating_sub(1));
            let y = y.clamp(0, i64::from(h).saturating_sub(1));
            src.get(usize::try_from(y.saturating_mul(i64::from(w)).saturating_add(x)).unwrap_or(0)).copied().unwrap_or([0.0; 4])
        };
        let mut out = Vec::with_capacity(usize::try_from(u64::from(ow).saturating_mul(u64::from(oh)).saturating_mul(12)).unwrap_or(0));
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
                let rgb = match self.channels {
                    OutputChannels::Alpha => [p[3]; 3],
                    OutputChannels::Rgb if self.premultiplied => [p[0], p[1], p[2]],
                    OutputChannels::Rgb => {
                        if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] } else { [0.0; 3] }
                    }
                };
                for c in rgb {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
        out
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
