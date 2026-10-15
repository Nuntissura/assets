//! Decode hook of the program monitor (lane SMB-PULSE-PLAYBACK-2, feature `media`): reel's
//! decoders behind pulse's [`FrameDecoder`] port, a bounded frame cache with prefetch
//! ([`FramePipeline`]) and the monitor's [`FrameSource`] port over it ([`MediaFrames`]).
//!
//! What decodes where (reel routing policy, unchanged): VC-3/DNxHR in process in every build,
//! ProRes in process (reel feature `prores`, enabled by `media`), Y4M raw; H.264 and AV1 of an
//! untrusted source only through a registered external decoder (reel-native's Media Foundation or
//! AV1 worker, feature `media-native`, [`ReelFrameDecoder::with_external`]); otherwise reel's
//! `MEDIA_CODEC_UNAVAILABLE`, which the monitor shows as the typed refusal `codec_unavailable`
//! (never an empty frame).
//!
//! Admission before allocation (STU-ARC-008): the container's declared picture size is probed
//! first; more than [`MAX_MONITOR_PIXELS`](super::monitor::MAX_MONITOR_PIXELS) pixels, or a frame
//! over the cache budget, is `limit_exceeded` before any decode; a decoded picture that does not
//! match the declared size is `corrupt`. A seek past the source is `out_of_range`.
//!
//! Colour: decoded Y'CbCr planes become encoded (non-linear) R'G'B' in `0..=1`, alpha 1, by the
//! H.273 matrix of the stream (1 = BT.709, 5/6 = BT.601, 9/10 = BT.2020 non-constant luminance,
//! 0 = identity G/B/R planes) and its range flag; an unspecified matrix (2, or a raw Y4M) is
//! assumed BT.709 from 720 lines up and BT.601 below, recorded in [`DecodeNote::matrix_assumed`].
//! Chroma is upsampled by sample replication (nearest). Exactness: Approximate (f32).
#![deny(clippy::arithmetic_side_effects)]

use super::monitor::{FrameSource, MAX_MONITOR_PIXELS, MonitorFail, SourceFrame};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::ClipSource;
use hsk_studio_pulse::{
    DecodeRefusal, DecodedFrame, FrameCacheLimits, FrameCost, FrameDecoder, FrameLayer,
    FramePipeline, TickRange, source_key,
};
use hsk_studio_reel as reel;
use reel::{ChromaFormat, ReelError, VideoFormat};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

/// Largest in-memory media item.
pub const MAX_MEDIA_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// Most registered media items.
pub const MAX_MEDIA_ITEMS: usize = 4096;
/// Bytes of one monitor frame per pixel (`rgb_f32le` + `f32le` alpha).
pub const FRAME_BYTES_PER_PIXEL: u64 = 16;

impl FrameCost for SourceFrame {
    fn cost_bytes(&self) -> u64 {
        u64::try_from(self.colour.len().saturating_add(self.alpha.len())).unwrap_or(u64::MAX)
    }
}

/// One registered source: its bytes and the grant the decoder reads them under.
struct Item {
    bytes: Arc<[u8]>,
    content_id: reel::ContentId,
    revision: u64,
    trust: reel::SourceTrust,
}

/// What the last decode did (for state projections and tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeNote {
    pub provider: String,
    pub codec: String,
    pub decoder_id: String,
    pub matrix: u16,
    pub matrix_assumed: bool,
    pub full_range: bool,
    pub frames: u64,
    /// `(pts ticks, SHA-256 hex of the decoded planes as reel delivered them)` per frame of the
    /// run, comparable with an external decoder's frame hashes (ffmpeg `framehash`).
    pub planes: Vec<(u64, String)>,
}

/// reel decoders as a pulse [`FrameDecoder`] producing monitor [`SourceFrame`]s.
pub struct ReelFrameDecoder {
    registry: reel::Registry,
    limits: reel::Limits,
    items: BTreeMap<String, Item>,
    note: Mutex<DecodeNote>,
}

fn refusal(e: &ReelError) -> DecodeRefusal {
    let code = match e {
        ReelError::CodecUnavailable { .. } => "codec_unavailable",
        ReelError::ImportUnsupported { .. } => "import_unsupported",
        ReelError::Corrupt { .. } => "corrupt",
        ReelError::Truncated { .. } => "truncated",
        ReelError::LimitExceeded { .. } => "limit_exceeded",
        ReelError::Canceled => "canceled",
        ReelError::StaleGrant => "stale_grant",
        ReelError::Unsupported { .. } => "unsupported",
        ReelError::OutOfRange { .. } => "out_of_range",
        ReelError::DecoderFault { .. } => "decoder_fault",
        ReelError::Io(_) => "io",
        _ => "media_error",
    };
    DecodeRefusal::new(code, e.to_string())
}

impl Default for ReelFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ReelFrameDecoder {
    /// reel's standard providers, no external decoder, default limits.
    pub fn new() -> Self {
        Self {
            registry: reel::Registry::standard(),
            limits: reel::Limits::default(),
            items: BTreeMap::new(),
            note: Mutex::new(DecodeNote::default()),
        }
    }
    /// Routes untrusted compressed video (H.264, AV1) to `decoder` (an isolated worker).
    pub fn with_external(mut self, decoder: Arc<dyn reel::ExternalVideoDecoder>) -> Self {
        self.registry = self.registry.with_video_decoder(decoder);
        self
    }
    pub fn with_limits(mut self, limits: reel::Limits) -> Self {
        self.limits = limits;
        self
    }
    /// Registers the bytes of `source` (revision 1, untrusted). `invalid_input` for a source
    /// without a cache key, `limit_exceeded` above [`MAX_MEDIA_BYTES`] or [`MAX_MEDIA_ITEMS`].
    pub fn register(&mut self, source: &ClipSource, bytes: Arc<[u8]>) -> Result<(), DecodeRefusal> {
        let key = source_key(source)
            .ok_or_else(|| DecodeRefusal::new("invalid_input", "source key too long"))?;
        if bytes.len() > MAX_MEDIA_BYTES
            || (self.items.len() >= MAX_MEDIA_ITEMS && !self.items.contains_key(&key))
        {
            return Err(DecodeRefusal::new("limit_exceeded", "media registry"));
        }
        let id = match source {
            ClipSource::Media { content_digest, .. } => {
                let tail: String = content_digest.digest.chars().take(64).collect();
                format!("media-{tail}")
            }
            _ => "media".to_owned(),
        };
        let content_id = reel::ContentId::parse(&id)
            .or_else(|_| reel::ContentId::parse("media"))
            .map_err(|e| refusal(&e))?;
        self.items.insert(
            key,
            Item {
                bytes,
                content_id,
                revision: 1,
                trust: reel::SourceTrust::Untrusted,
            },
        );
        Ok(())
    }
    /// The last decode's note.
    pub fn note(&self) -> DecodeNote {
        self.note.lock().map(|n| n.clone()).unwrap_or_default()
    }

    fn item(&self, source: &ClipSource) -> Result<&Item, DecodeRefusal> {
        source_key(source)
            .and_then(|k| self.items.get(&k))
            .ok_or_else(|| DecodeRefusal::new("media_unregistered", "no bytes registered for the source"))
    }

    fn probe(
        &self,
        item: &Item,
        cancel: &CancellationToken,
    ) -> Result<(String, reel::StreamLayout), DecodeRefusal> {
        let mut src = Cursor::new(&item.bytes[..]);
        let provider = self.registry.detect(&mut src).map_err(|e| refusal(&e))?;
        let layout = provider
            .probe(&mut src, &self.limits, cancel)
            .map_err(|e| refusal(&e))?;
        if layout.kind != reel::MediaKind::Video {
            return Err(DecodeRefusal::new("not_video", format!("{:?}", layout.kind)));
        }
        Ok((provider.descriptor().id.to_string(), layout))
    }
}

fn declared_pixels(width: u32, height: u32) -> Result<u64, DecodeRefusal> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| DecodeRefusal::new("limit_exceeded", "picture size overflows"))?;
    if pixels == 0 {
        return Err(DecodeRefusal::new("corrupt", "empty picture"));
    }
    if pixels > MAX_MONITOR_PIXELS {
        return Err(DecodeRefusal::new(
            "limit_exceeded",
            format!("{width}x{height} over the monitor pixel limit"),
        ));
    }
    Ok(pixels)
}

struct Collect {
    frames: Vec<(i64, u64, Vec<u8>)>,
    budget: usize,
}
impl reel::ChunkSink for Collect {
    fn accept(&mut self, c: reel::Chunk<'_>) -> Result<(), ReelError> {
        if c.kind != reel::ChunkKind::VideoFrame {
            return Ok(());
        }
        self.budget = self
            .budget
            .checked_sub(c.bytes.len())
            .ok_or(ReelError::LimitExceeded { what: "monitor-run-bytes" })?;
        self.frames.push((c.pts, c.duration.value(), c.bytes.to_vec()));
        Ok(())
    }
}

/// Y4M carries no picture format in its receipt: 8-bit planes inferred from the frame length.
fn raw_format(width: u32, height: u32, len: usize) -> Option<VideoFormat> {
    let colour = reel::ColorInfo {
        primaries: 2,
        transfer: 2,
        matrix: 2,
        full_range: false,
        origin: reel::ColorOrigin::Untagged,
    };
    [ChromaFormat::Yuv420, ChromaFormat::Yuv422, ChromaFormat::Yuv444, ChromaFormat::Mono]
        .into_iter()
        .map(|chroma| VideoFormat {
            width,
            height,
            bit_depth: 8,
            chroma,
            color: colour,
        })
        .find(|f| f.frame_bytes() == Some(len))
}

/// (Kr, Kb) of an H.273 matrix; `None` for identity (0). Unspecified -> by height (assumed).
fn coefficients(matrix: u16, height: u32) -> (Option<(f64, f64)>, u16, bool) {
    match matrix {
        0 => (None, 0, false),
        1 => (Some((0.2126, 0.0722)), 1, false),
        5 | 6 => (Some((0.299, 0.114)), matrix, false),
        9 | 10 => (Some((0.2627, 0.0593)), matrix, false),
        _ if height >= 720 => (Some((0.2126, 0.0722)), 1, true),
        _ => (Some((0.299, 0.114)), 6, true),
    }
}

/// Converts packed planar Y'CbCr (reel's `VideoFormat` layout) to a monitor frame.
pub fn ycbcr_to_frame(format: &VideoFormat, bytes: &[u8]) -> Result<(SourceFrame, u16, bool), DecodeRefusal> {
    let corrupt = |what: &str| DecodeRefusal::new("corrupt", what.to_owned());
    if format.frame_bytes() != Some(bytes.len()) || format.bit_depth == 0 || format.bit_depth > 16 {
        return Err(corrupt("decoded picture does not match its format"));
    }
    let (w, h) = (format.width, format.height);
    let pixels = usize::try_from(declared_pixels(w, h)?).map_err(|_| corrupt("size"))?;
    let bps = format.bytes_per_sample();
    let dims = format.plane_dims();
    let mut offsets = Vec::with_capacity(dims.len());
    let mut at = 0usize;
    for (pw, ph) in &dims {
        offsets.push(at);
        let n = usize::try_from(u64::from(*pw).saturating_mul(u64::from(*ph))).map_err(|_| corrupt("plane"))?;
        at = at
            .checked_add(n.checked_mul(bps).ok_or_else(|| corrupt("plane"))?)
            .ok_or_else(|| corrupt("plane"))?;
    }
    let sample = |plane: usize, x: u32, y: u32| -> f64 {
        let (pw, ph) = dims[plane];
        let (sx, sy) = (
            u64::from(x).saturating_mul(u64::from(pw)).checked_div(u64::from(w)).unwrap_or(0),
            u64::from(y).saturating_mul(u64::from(ph)).checked_div(u64::from(h)).unwrap_or(0),
        );
        let index = usize::try_from(sy.saturating_mul(u64::from(pw)).saturating_add(sx)).unwrap_or(0);
        let at = offsets[plane].saturating_add(index.saturating_mul(bps));
        if bps == 2 {
            f64::from(u16::from_le_bytes([bytes[at], bytes[at.saturating_add(1)]]))
        } else {
            f64::from(bytes[at])
        }
    };
    let depth = i32::from(format.bit_depth);
    let scale = f64::from(1u32 << (depth.saturating_sub(8)).clamp(0, 8));
    let max = f64::from((1u32 << depth.clamp(1, 16)).saturating_sub(1));
    let half = f64::from(1u32 << depth.saturating_sub(1).clamp(0, 15));
    let full = format.color.full_range;
    let (k, matrix, assumed) = coefficients(format.color.matrix, h);
    let mono = format.chroma == ChromaFormat::Mono;
    let mut colour = Vec::with_capacity(pixels.saturating_mul(12));
    let mut alpha = Vec::with_capacity(pixels.saturating_mul(4));
    for y in 0..h {
        for x in 0..w {
            let yy = sample(0, x, y);
            let (cb, cr) = if mono { (half, half) } else { (sample(1, x, y), sample(2, x, y)) };
            let rgb = match k {
                None => {
                    // Identity matrix: planes are G, B, R.
                    let n = |v: f64| if full { v / max } else { (v - 16.0 * scale) / (219.0 * scale) };
                    [n(cr), n(yy), n(cb)]
                }
                Some((kr, kb)) => {
                    let (l, pb, pr) = if full {
                        (yy / max, (cb - half) / max, (cr - half) / max)
                    } else {
                        (
                            (yy - 16.0 * scale) / (219.0 * scale),
                            (cb - half) / (224.0 * scale),
                            (cr - half) / (224.0 * scale),
                        )
                    };
                    let kg = 1.0 - kr - kb;
                    let r = l + 2.0 * (1.0 - kr) * pr;
                    let b = l + 2.0 * (1.0 - kb) * pb;
                    let g = (l - kr * r - kb * b) / kg;
                    [r, g, b]
                }
            };
            for c in rgb {
                colour.extend_from_slice(&(c.clamp(0.0, 1.0) as f32).to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
    }
    let frame = SourceFrame {
        width: w,
        height: h,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    };
    Ok((frame, matrix, assumed))
}

impl FrameDecoder for ReelFrameDecoder {
    type Frame = SourceFrame;

    fn declared_bytes(&self, source: &ClipSource, cancel: &CancellationToken) -> Result<u64, DecodeRefusal> {
        let item = self.item(source)?;
        let (_, layout) = self.probe(item, cancel)?;
        declared_pixels(layout.width, layout.height)?
            .checked_mul(FRAME_BYTES_PER_PIXEL)
            .ok_or_else(|| DecodeRefusal::new("limit_exceeded", "frame bytes overflow"))
    }

    fn decode_run(
        &self,
        source: &ClipSource,
        source_tick: Ticks,
        max_frames: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<DecodedFrame<SourceFrame>>, DecodeRefusal> {
        let item = self.item(source)?;
        let (provider, layout) = self.probe(item, cancel)?;
        let out_of_range = || {
            DecodeRefusal::new("out_of_range", format!("source tick {} past the media", source_tick.value()))
        };
        let index = source_tick
            .value()
            .checked_div(layout.ticks_per_unit)
            .ok_or_else(|| DecodeRefusal::new("corrupt", "zero frame duration"))?;
        let remaining = layout.units.checked_sub(index).filter(|r| *r > 0).ok_or_else(out_of_range)?;
        let count = remaining.min(u64::from(max_frames.max(1)));
        let picture = usize::try_from(declared_pixels(layout.width, layout.height)?)
            .map_err(|_| DecodeRefusal::new("limit_exceeded", "size"))?;
        // Raw planes are at most 3 samples of 2 bytes per pixel.
        let budget = picture
            .checked_mul(6)
            .and_then(|b| b.checked_mul(usize::try_from(count).ok()?))
            .ok_or_else(|| DecodeRefusal::new("limit_exceeded", "run bytes"))?;
        let request = reel::SegmentRequest {
            codec: provider.clone(),
            start_pts: source_tick,
            count,
            track: None,
            output: reel::OutputMode::Decoded,
            prefer: reel::SourcePreference::Original,
            purpose: reel::Purpose::Preview,
            grant: reel::Grant {
                content_id: item.content_id.clone(),
                revision: item.revision,
                expected_len: u64::try_from(item.bytes.len()).unwrap_or(u64::MAX),
            },
            expected_revision: item.revision,
        };
        let mut sink = Collect {
            frames: Vec::new(),
            budget,
        };
        let receipt = self
            .registry
            .decode_segment_routed(
                &mut Cursor::new(&item.bytes[..]),
                None,
                &request,
                &self.limits,
                cancel,
                &mut sink,
                item.trust,
            )
            .map_err(|e| refusal(&e))?;
        let mut out = Vec::with_capacity(sink.frames.len());
        let mut note = DecodeNote {
            provider,
            codec: receipt.codec.clone(),
            decoder_id: receipt.decoder_id.to_owned(),
            ..DecodeNote::default()
        };
        for (pts, duration, bytes) in sink.frames {
            let format = match receipt.video {
                Some(f) => f,
                None => raw_format(layout.width, layout.height, bytes.len())
                    .ok_or_else(|| DecodeRefusal::new("corrupt", "raw frame length"))?,
            };
            if (format.width, format.height) != (layout.width, layout.height) {
                return Err(DecodeRefusal::new("corrupt", "decoded picture differs from the declared size"));
            }
            let (frame, matrix, assumed) = ycbcr_to_frame(&format, &bytes)?;
            let start = u64::try_from(pts).map_err(|_| DecodeRefusal::new("corrupt", "negative pts"))?;
            let digest: String = hsk_studio_prism::profile_hash(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            note.planes.push((start, digest));
            let end = start.checked_add(duration.max(1)).ok_or_else(out_of_range)?;
            let span = TickRange::new(Ticks::new(start), Ticks::new(end))
                .map_err(|e| DecodeRefusal::new(e.code(), "span"))?;
            note.matrix = matrix;
            note.matrix_assumed = assumed;
            note.full_range = format.color.full_range;
            note.frames = note.frames.saturating_add(1);
            out.push(DecodedFrame { span, frame });
        }
        if let Ok(mut n) = self.note.lock() {
            *n = note;
        }
        Ok(out)
    }
}

/// The monitor's [`FrameSource`] over a [`FramePipeline`] of [`ReelFrameDecoder`].
pub struct MediaFrames {
    pipeline: RefCell<FramePipeline<ReelFrameDecoder>>,
    cancel: CancellationToken,
    /// Decoder calls one prefetch may make.
    pub prefetch_decodes: u32,
}

impl MediaFrames {
    /// `run_frames` frames are decoded per cache miss (a run serves the next frames as well).
    pub fn new(decoder: ReelFrameDecoder, limits: FrameCacheLimits, run_frames: u32) -> Result<Self, MonitorFail> {
        let pipeline = FramePipeline::new(decoder, limits, run_frames).map_err(|e| MonitorFail {
            code: e.code(),
            detail: "frame pipeline".into(),
        })?;
        Ok(Self {
            pipeline: RefCell::new(pipeline),
            cancel: CancellationToken::default(),
            prefetch_decodes: 2,
        })
    }
    /// Read access to the pipeline (cache statistics, refusals, decoder note).
    pub fn with_pipeline<R>(&self, f: impl FnOnce(&FramePipeline<ReelFrameDecoder>) -> R) -> R {
        f(&self.pipeline.borrow())
    }
}

impl FrameSource for MediaFrames {
    fn frame(&self, source: &ClipSource, source_tick: Ticks) -> Option<Arc<SourceFrame>> {
        self.frame_result(source, source_tick).ok()
    }
    fn frame_result(&self, source: &ClipSource, source_tick: Ticks) -> Result<Arc<SourceFrame>, MonitorFail> {
        self.pipeline
            .borrow_mut()
            .frame(source, source_tick, &self.cancel)
            .map_err(|r| MonitorFail {
                code: r.code,
                detail: r.detail,
            })
    }
    fn prefetch(&self, layers: &[FrameLayer], cancel: &CancellationToken) -> u64 {
        self.pipeline
            .borrow_mut()
            .prefetch(layers, self.prefetch_decodes, cancel)
    }
}
