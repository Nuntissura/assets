//! Render queue from COMPOSITIONS (lane SMB-AE-MOTION-TOOLS slice 4; feature `media`;
//! STU-MOT-120/124/125: a composition render queue item and a sequence export recipe are two entry
//! points to the ONE export pipeline 14.13 owns, here reel's `ExportQueue`). The harness only adapts
//! ports (CX-CAP-001: no codec and no compositor here):
//!
//! * video: a caller [`CompositionScenes`] evaluates the composition at each frame's composition
//!   tick (composite's `CompositionTemporalSource::scene_at` is such an evaluator; tests build scenes
//!   directly), composite's `render_scene` renders it on render-cpu, the straight-alpha frame is
//!   flattened over opaque black (`rgb * alpha`) and converted by [`super::rgb_to_planes`] (BT.709
//!   limited range, the timeline export's conversion);
//! * audio: the composition's audio layers ([`CompositionAudioLayer`]: interleaved stereo f32 at the
//!   export rate placed at a composition tick, with a linear gain) summed sample-exactly over the
//!   exported span (no clamping here; reel counts clipped samples in the receipt). Folio has no audio
//!   layer record for compositions yet (SPEC/record GAP): the caller resolves the audio layers.
//!
//! Refused before anything renders (STU-VID-062): a span that is not whole frames or is empty, an
//! audio layer start that is not a whole sample, an odd interleaved buffer, a raster the scene
//! does not match (checked per frame: a scene of another size fails the item, never a scaled frame),
//! and audio layer counts / lengths above [`MAX_AUDIO_LAYERS`] / reel's audio budget.
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
use std::path::PathBuf;
use std::sync::Arc;

/// Most audio layers of one composition export.
pub const MAX_AUDIO_LAYERS: usize = 256;

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// Evaluates a composition at composition ticks.
pub trait CompositionScenes: Send {
    /// The composition's frame at `tick`; an `Err` code fails the export item.
    fn scene_at(&mut self, tick: u64, cancel: &CancellationToken) -> Result<SceneComposition, String>;
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

/// Rendered composition frames of `[in, out)` as an export video source.
pub struct CompositionVideo<S> {
    scenes: S,
    renderer: Renderer,
    space: ColourTag,
    start: u64,
    ticks_per_frame: u64,
    frames: u64,
    format: VideoFormat,
}

/// `rgb * alpha` (over opaque black) of a straight `rgb_f32le` + `f32le` frame.
fn over_black(colour: &[u8], alpha: &[u8]) -> Result<Vec<u8>, ReelError> {
    let pixels = alpha.len().checked_div(4).unwrap_or(0);
    if colour.len() != pixels.saturating_mul(12) {
        return Err(ReelError::Unsupported { what: "export-render-size" });
    }
    let mut out = Vec::with_capacity(colour.len());
    for (c, a) in colour.chunks_exact(12).zip(alpha.chunks_exact(4)) {
        let a = a.try_into().map_or(0.0, f32::from_le_bytes);
        for k in c.chunks_exact(4) {
            let v = k.try_into().map_or(0.0, f32::from_le_bytes);
            out.extend_from_slice(&(v * a).to_le_bytes());
        }
    }
    Ok(out)
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
        let tick = index
            .checked_mul(self.ticks_per_frame)
            .and_then(|t| t.checked_add(self.start))
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        let canceled = |cancel: &CancellationToken| cancel.check().is_err();
        let scene = self.scenes.scene_at(tick, cancel).map_err(|_| {
            if canceled(cancel) { ReelError::Canceled } else { ReelError::Unsupported { what: "export-composition-unavailable" } }
        })?;
        if scene.width != self.format.width || scene.height != self.format.height {
            return Err(ReelError::Unsupported { what: "export-composition-size" });
        }
        let ctx = SceneContext {
            backend: &self.renderer,
            pixels: self.scenes.pixels(),
            request: ExecuteRequest {
                expected_revision: 0,
                min_exactness: Exactness::Approximate,
                deadline: None,
                max_working_bytes: 1 << 30,
            },
            space: self.space,
            limits: SceneLimits::default(),
        };
        let rendered = render_scene(&scene, &ctx, cancel).map_err(|_| {
            if canceled(cancel) { ReelError::Canceled } else { ReelError::Unsupported { what: "export-render-failed" } }
        })?;
        rgb_to_planes(&over_black(&rendered.colour, &rendered.alpha)?, &self.format)
    }
}

struct PlacedLayer {
    samples: Arc<Vec<f32>>,
    /// First composition sample of the layer.
    start: u64,
    gain: f32,
}

/// The composition's audio layers summed over the exported span (stereo).
pub struct CompositionAudio {
    layers: Vec<PlacedLayer>,
    /// Composition sample of export frame 0.
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

/// What to export from a composition.
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

/// Build the reel export job of a composition span (module docs).
pub fn composition_export_job<S: CompositionScenes + 'static>(c: CompositionExport<S>) -> Result<ExportJob, MonitorFail> {
    let tpf = c.ticks_per_frame;
    if tpf == 0 {
        return Err(fail("export_range", "zero frame duration"));
    }
    if c.out_tick <= c.in_tick || !c.in_tick.is_multiple_of(tpf) || !c.out_tick.is_multiple_of(tpf) {
        return Err(fail("export_range", "in/out must be whole frames, in < out"));
    }
    if c.width == 0 || c.height == 0 {
        return Err(fail("export_raster", "zero raster"));
    }
    let frames = c.out_tick.saturating_sub(c.in_tick).checked_div(tpf).unwrap_or(0);
    let limits = ExportLimits::default();
    let format = VideoFormat {
        width: c.width,
        height: c.height,
        bit_depth: c.bit_depth,
        chroma: c.chroma,
        color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
    };
    let renderer = Renderer::new(64).map_err(|e| fail("renderer", format!("{e:?}")))?;
    let audio: Option<Box<dyn ExportAudioSource>> = match c.audio {
        None => None,
        Some((layers, rate)) => {
            if layers.len() > MAX_AUDIO_LAYERS {
                return Err(fail("export_audio", "too many audio layers"));
            }
            let start = whole_samples(c.in_tick, rate)?;
            let end = u128::from(c.out_tick)
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
            Some(Box::new(CompositionAudio { layers: placed, start, frames: end.saturating_sub(start), rate }))
        }
    };
    let video = CompositionVideo { scenes: c.scenes, renderer, space: c.space, start: c.in_tick, ticks_per_frame: tpf, frames, format };
    Ok(ExportJob {
        recipe: c.recipe,
        video: Some(Box::new(video)),
        audio,
        span: SourceSpan { start_ticks: c.in_tick, end_ticks: c.out_tick },
        output: c.output,
        overwrite: c.overwrite,
        artifact: c.artifact,
        limits,
    })
}
