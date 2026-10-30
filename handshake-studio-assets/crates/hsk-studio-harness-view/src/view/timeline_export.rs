//! Render-queue export of a pulse timeline (lane SMB-REEL-ENCODE slice 4; feature `media`): the program
//! frames come from the program monitor's composite path ([`ProgramMonitor::render_at`]: pulse decides
//! what plays, composite renders it on render-cpu), the sound from pulse's score mixdown
//! ([`TimelineMixStream`]), and both go into reel's export queue (`hsk_studio_reel::ExportQueue`:
//! encoder, ISO BMFF muxer, receipt). The harness only adapts the two ports (CX-CAP-001: no codec here).
//!
//! Frames: the composite is gamma-encoded R'G'B' over an opaque black background; it is converted to
//! Y'CbCr with the ITU-R BT.709 matrix at video (limited) range for the recipe's bit depth, chroma by a
//! box average of the 2 (4:2:2) or 2x2 (4:2:0) full-resolution values, nearest-integer rounding; the
//! stream is tagged BT.709 (primaries 1, transfer 1, matrix 1). The range is exact: `in`/`out` must be
//! whole frames of the sequence and the audio start a whole sample, else the export is refused before
//! anything renders. A sequence with an enabled nested sequence clip is refused `nest_unresolved`
//! naming the clip (lane SMB-PR-NEST-1: the job holds no document to resolve it).
#![deny(clippy::arithmetic_side_effects)]

/// Render queue from compositions (lane SMB-AE-MOTION-TOOLS slice 4).
pub mod composition;
/// Render queue items from Folio records (lane SMB-AE-MOTION-RECORDS).
pub mod render_queue;

use super::monitor::{DropPolicy, FrameSource, MonitorFail, ProgramMonitor};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::StudioSequence;
use hsk_studio_pulse::{AudioSources, SourceExtents, StreamLimits, TimelineMixStream};
use hsk_studio_reel::{
    ChromaFormat, ColorInfo, ColorOrigin, ExportAudioSource, ExportJob, ExportLimits, ExportRecipe,
    ExportVideoSource, ReelError, SourceSpan, VideoFormat,
};
use std::path::PathBuf;

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

/// BT.709 R'G'B' (0..=1) to limited-range Y'CbCr code values at `bits` (unrounded).
fn ycbcr(rgb: [f32; 3], bits: u8) -> [f64; 3] {
    let [r, g, b] = rgb.map(|v| f64::from(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }));
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / 1.8556;
    let cr = (r - y) / 1.5748;
    let scale = f64::from(1u32 << bits.saturating_sub(8));
    [
        (16.0 + 219.0 * y) * scale,
        (128.0 + 224.0 * cb) * scale,
        (128.0 + 224.0 * cr) * scale,
    ]
}

/// Convert a rendered `rgb_f32le` frame to planes Y, Cb, Cr in `format`'s delivery layout.
pub fn rgb_to_planes(colour: &[u8], format: &VideoFormat) -> Result<Vec<Vec<u8>>, ReelError> {
    let (w, h) = (format.width as usize, format.height as usize);
    let pixels = w.checked_mul(h).ok_or(ReelError::LimitExceeded {
        what: "export-frame-bytes",
    })?;
    if colour.len() != pixels.saturating_mul(12) {
        return Err(ReelError::Unsupported {
            what: "export-render-size",
        });
    }
    let (sx, sy) = match format.chroma {
        ChromaFormat::Yuv420 => (2usize, 2usize),
        ChromaFormat::Yuv422 => (2, 1),
        ChromaFormat::Yuv444 => (1, 1),
        ChromaFormat::Mono => {
            return Err(ReelError::Unsupported {
                what: "export-mono",
            });
        }
    };
    let bits = format.bit_depth;
    let max = f64::from((1u32 << bits).saturating_sub(1));
    let wide = bits > 8;
    let full: Vec<[f64; 3]> = colour
        .chunks_exact(12)
        .map(|p| {
            let c = |i: usize| {
                p.get(i..i.saturating_add(4))
                    .and_then(|b| b.try_into().ok())
                    .map_or(0.0, f32::from_le_bytes)
            };
            ycbcr([c(0), c(4), c(8)], bits)
        })
        .collect();
    let put = |out: &mut Vec<u8>, v: f64| {
        let q = (v + 0.5).floor().clamp(0.0, max);
        if wide {
            out.extend_from_slice(&(q as u16).to_le_bytes());
        } else {
            out.push(q as u8);
        }
    };
    let mut y_plane = Vec::new();
    for v in &full {
        put(&mut y_plane, v[0]);
    }
    let (cw, ch) = (w.div_ceil(sx), h.div_ceil(sy));
    let (mut cb, mut cr) = (Vec::new(), Vec::new());
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut sum_b, mut sum_r, mut n) = (0f64, 0f64, 0f64);
            for dy in 0..sy {
                for dx in 0..sx {
                    let x = cx.saturating_mul(sx).saturating_add(dx);
                    let y = cy.saturating_mul(sy).saturating_add(dy);
                    let at = y.saturating_mul(w).saturating_add(x);
                    if x < w && y < h
                        && let Some(v) = full.get(at)
                    {
                        sum_b += v[1];
                        sum_r += v[2];
                        n += 1.0;
                    }
                }
            }
            put(&mut cb, sum_b / n);
            put(&mut cr, sum_r / n);
        }
    }
    Ok(vec![y_plane, cb, cr])
}

/// Program frames of `[in, out)` as an export video source.
pub struct TimelineVideo<S, E> {
    monitor: ProgramMonitor,
    sources: S,
    extents: E,
    start: u64,
    ticks_per_frame: u64,
    frames: u64,
    format: VideoFormat,
}

impl<S: FrameSource + Send, E: SourceExtents + Send> ExportVideoSource for TimelineVideo<S, E> {
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
        let scene = self
            .monitor
            .render_at(Ticks::new(tick), &self.sources, &self.extents, cancel)
            .map_err(|e| {
                if cancel.check().is_err() {
                    ReelError::Canceled
                } else {
                    // A monitor refusal (missing media, render failure) fails the export, never a
                    // silently black frame.
                    ReelError::Unsupported {
                        what: match e.code {
                            "frame_unavailable" => "export-frame-unavailable",
                            "codec_unavailable" => "export-codec-unavailable",
                            _ => "export-render-failed",
                        },
                    }
                }
            })?;
        rgb_to_planes(&scene.colour, &self.format)
    }
}

/// Score mixdown of `[in, out)` (stereo) as an export audio source.
pub struct TimelineAudio<S: AudioSources> {
    mix: TimelineMixStream<S>,
    start: u64,
    frames: u64,
    rate: u32,
    block: usize,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl<S: AudioSources + Send> ExportAudioSource for TimelineAudio<S> {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u16 {
        2
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn render(
        &mut self,
        first: u64,
        frames: u32,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<(), ReelError> {
        let at = self
            .start
            .checked_add(first)
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        if self.mix.position() != at {
            self.mix.seek(at);
        }
        let mut left = u64::from(frames);
        while left > 0 {
            let n = usize::try_from(left).unwrap_or(usize::MAX).min(self.block);
            self.left.resize(n, 0.0);
            self.right.resize(n, 0.0);
            self.mix
                .render(&mut self.left, &mut self.right, cancel)
                .map_err(|e| match e {
                    hsk_studio_pulse::PulseError::Canceled => ReelError::Canceled,
                    _ => ReelError::Unsupported {
                        what: "export-mixdown-failed",
                    },
                })?;
            for (l, r) in self.left.iter().zip(&self.right) {
                out.push(*l);
                out.push(*r);
            }
            left = left.saturating_sub(n as u64);
        }
        Ok(())
    }
}

/// What to export from a sequence.
pub struct TimelineExport<VS, AS, E> {
    pub sequence: StudioSequence,
    /// First exported tick (a frame boundary) and the end tick (exclusive, a frame boundary).
    pub in_tick: u64,
    pub out_tick: u64,
    pub recipe: ExportRecipe,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub chroma: ChromaFormat,
    pub video_sources: VS,
    pub video_extents: E,
    /// Mixdown sources and sample rate (`None`: no audio track in the file).
    pub audio: Option<(AS, u32)>,
    pub audio_extents: E,
    pub output: PathBuf,
    pub overwrite: bool,
    pub artifact: String,
}

/// Build the reel export job of a timeline range. Refused here (before anything renders): a range that
/// is not whole frames, an audio start that is not a whole sample, a monitor raster the monitor refuses,
/// a mixdown pulse cannot plan.
pub fn timeline_export_job<VS, AS, E>(
    t: TimelineExport<VS, AS, E>,
    cancel: &CancellationToken,
) -> Result<ExportJob, MonitorFail>
where
    VS: FrameSource + Send + 'static,
    AS: AudioSources + Send + 'static,
    E: SourceExtents + Send + 'static,
{
    let tpf = t.sequence.frame_rate.ticks_per_frame;
    if tpf == 0 {
        return Err(fail("export_range", "zero frame duration"));
    }
    if t.out_tick <= t.in_tick
        || !t.in_tick.is_multiple_of(tpf)
        || !t.out_tick.is_multiple_of(tpf)
    {
        return Err(fail("export_range", "in/out must be whole frames, in < out"));
    }
    // Lane SMB-PR-NEST-1: the export holds no document to resolve a nested sequence clip, so it is
    // refused naming the clip instead of rendering it black or silent (V4-03).
    let nested = t.sequence.tracks.iter().filter(|tr| tr.enabled).flat_map(|tr| tr.clips.iter()).filter(|c| c.enabled).find_map(|c| match &c.source_ref {
        hsk_studio_folio::ClipSource::Sequence { sequence_id } => Some((c.clip_id.as_str(), sequence_id.as_str())),
        _ => None,
    });
    if let Some((clip, sequence)) = nested {
        return Err(fail("nest_unresolved", format!("{clip}: nested sequence {sequence} (export does not resolve nested sequences)")));
    }
    let frames = t
        .out_tick
        .saturating_sub(t.in_tick)
        .checked_div(tpf)
        .unwrap_or(0);
    let monitor = ProgramMonitor::new(t.sequence.clone(), t.width, t.height, DropPolicy::PresentEveryFrame)?;
    let format = VideoFormat {
        width: t.width,
        height: t.height,
        bit_depth: t.bit_depth,
        chroma: t.chroma,
        color: ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            origin: ColorOrigin::SequenceHeader,
        },
    };
    let video = TimelineVideo {
        monitor,
        sources: t.video_sources,
        extents: t.video_extents,
        start: t.in_tick,
        ticks_per_frame: tpf,
        frames,
        format,
    };
    let audio: Option<Box<dyn ExportAudioSource>> = match t.audio {
        None => None,
        Some((sources, rate)) => {
            let to_samples = |ticks: u64| -> Result<u64, MonitorFail> {
                let num = u128::from(ticks).saturating_mul(u128::from(rate));
                if !num.is_multiple_of(u128::from(TICKS_PER_SECOND)) {
                    return Err(fail("export_range", "audio start is not a whole sample"));
                }
                num.checked_div(u128::from(TICKS_PER_SECOND))
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| fail("export_range", "overflow"))
            };
            let start = to_samples(t.in_tick)?;
            let end_samples = u128::from(t.out_tick)
                .saturating_mul(u128::from(rate))
                .checked_div(u128::from(TICKS_PER_SECOND))
                .unwrap_or(0);
            let end = u64::try_from(end_samples).map_err(|_| fail("export_range", "overflow"))?;
            let block = 4800usize;
            let mix = TimelineMixStream::new(
                &t.sequence,
                rate,
                block,
                sources,
                &t.audio_extents,
                StreamLimits::default(),
                cancel,
            )
            .map_err(|e| fail("mixdown", e.code()))?;
            Some(Box::new(TimelineAudio {
                mix,
                start,
                frames: end.saturating_sub(start),
                rate,
                block,
                left: Vec::new(),
                right: Vec::new(),
            }))
        }
    };
    Ok(ExportJob {
        recipe: t.recipe,
        video: Some(Box::new(video)),
        audio,
        span: SourceSpan {
            start_ticks: t.in_tick,
            end_ticks: t.out_tick,
        },
        output: t.output,
        overwrite: t.overwrite,
        artifact: t.artifact,
        limits: ExportLimits::default(),
    })
}

// ---------------------------------------------------------------------------------------------
// Lane SMB-PR-SOURCE-MONITOR slice 4: caption streams and export presets for timeline exports.
// ---------------------------------------------------------------------------------------------

/// ISO 639-2/T (terminology) code of a BCP 47 tag's primary language subtag: a three-letter subtag as
/// written (lower-cased), the two-letter ISO 639-1 codes of this table mapped, anything else `und`
/// (Studio table; a caption track's language is BCP 47 in Folio, `mdhd` carries ISO 639-2/T).
pub fn iso639_2(tag: &str) -> String {
    let primary = tag.split('-').next().unwrap_or("").to_ascii_lowercase();
    if primary.len() == 3 && primary.bytes().all(|b| b.is_ascii_lowercase()) {
        return primary;
    }
    const TABLE: &[(&str, &str)] = &[
        ("ar", "ara"), ("cs", "ces"), ("da", "dan"), ("de", "deu"), ("el", "ell"), ("en", "eng"), ("es", "spa"),
        ("fi", "fin"), ("fr", "fra"), ("he", "heb"), ("hi", "hin"), ("hu", "hun"), ("id", "ind"), ("it", "ita"),
        ("ja", "jpn"), ("ko", "kor"), ("nb", "nob"), ("nl", "nld"), ("no", "nor"), ("pl", "pol"), ("pt", "por"),
        ("ro", "ron"), ("ru", "rus"), ("sv", "swe"), ("th", "tha"), ("tr", "tur"), ("uk", "ukr"), ("vi", "vie"),
        ("zh", "zho"),
    ];
    TABLE.iter().find(|(two, _)| *two == primary).map_or_else(|| "und".to_owned(), |(_, three)| (*three).to_owned())
}

/// The caption stream of a Folio caption track (STU-VID-037) for an export of `in_tick .. out_tick`:
/// segments clipped to the span and shifted to span-relative ticks (a segment outside the span is left
/// out), the language mapped by [`iso639_2`]; reel checks the cues before the first frame.
pub fn export_captions(
    track: &hsk_studio_folio::StudioCaptionTrack,
    in_tick: u64,
    out_tick: u64,
    embed: bool,
    sidecar: bool,
) -> hsk_studio_reel::captions::ExportCaptions {
    let cues = track
        .segments
        .iter()
        .filter_map(|s| {
            let start = s.start.max(in_tick);
            let end = s.end.min(out_tick);
            (end > start).then(|| hsk_studio_reel::captions::CaptionCue {
                start_ticks: start.saturating_sub(in_tick),
                end_ticks: end.saturating_sub(in_tick),
                text: s.text.clone(),
            })
        })
        .collect();
    hsk_studio_reel::captions::ExportCaptions {
        language: iso639_2(&track.language),
        cues,
        embed,
        sidecar,
        limits: hsk_studio_reel::captions::CaptionLimits::default(),
    }
}

/// An export queue and whether it carries the OS encoder (D24-A): with feature `os-encode` reel-native's
/// `WorkerMfEncoder` (the `reel-native-mf-worker` beside the executable, or `worker` when given) is
/// registered; without the feature, or when the worker cannot be configured, the queue has none and
/// H.264 / HEVC presets are refused by [`preset_recipe`].
pub fn export_queue(worker: Option<std::path::PathBuf>) -> (hsk_studio_reel::ExportQueue, bool) {
    #[cfg(feature = "os-encode")]
    {
        let mut options = hsk_studio_reel_native::MfWorkerOptions::default();
        if let Some(w) = worker {
            options = options.with_worker_program(w);
        }
        if let Ok(encoder) = hsk_studio_reel_native::WorkerMfEncoder::new(options) {
            return (hsk_studio_reel::ExportQueue::new().with_video_encoder(std::sync::Arc::new(encoder)), true);
        }
    }
    #[cfg(not(feature = "os-encode"))]
    let _ = worker;
    (hsk_studio_reel::ExportQueue::new(), false)
}

/// The recipe of Studio preset `preset_id` (reel's preset table) for a queue that does (`os_encoder`)
/// or does not carry the OS encoder: `unknown_preset`, or reel's typed `codec_unavailable` refusal
/// for an H.264 / HEVC preset without the OS encoder (D24-A) and an AV1 preset without `av1-encode`.
pub fn preset_recipe(preset_id: &str, os_encoder: bool) -> Result<ExportRecipe, MonitorFail> {
    let preset = hsk_studio_reel::presets::export_preset(preset_id).ok_or_else(|| fail("unknown_preset", preset_id))?;
    preset.recipe(os_encoder).map_err(|e| fail("codec_unavailable", e.to_string()))
}
