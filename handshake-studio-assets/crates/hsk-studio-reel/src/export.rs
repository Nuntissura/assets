//! Export queue (lane SMB-REEL-ENCODE; STU-VID-060..064, STU-MOT-120, STU-IO-142/143): frames from a
//! [`ExportVideoSource`] and samples from an [`ExportAudioSource`] go through an encoder (Studio's VC-3
//! encoder, rav1e AV1 with cargo feature `av1-encode`, or, Operator decision D24-A, H.264 / HEVC through the
//! OS encoder registered with [`ExportQueue::with_video_encoder`]: reel-native's `os-encode` worker) into the
//! ISO BMFF muxer, into a sibling temp file that is renamed over the output only when the whole item
//! succeeded.
//!
//! STU-VID-064: an export is submitted to a queue ([`ExportQueue::submit`]), runs without a window,
//! reports determinate progress ([`ExportHandle::progress`], frames done of frames declared, readable
//! from another thread), is cancellable ([`ExportHandle::cancel`], also before it starts), and produces
//! one [`ExportReceipt`] per item naming the recipe, the source span, the output artifact and the
//! outcome. Several items are one queue with per-item receipts ([`ExportQueue::run`]).
//!
//! STU-VID-062: every combination and bound is refused before the first frame is requested from the
//! source (encoder configuration, container combination, sample and byte budgets, output path). An
//! absent encoder is `MEDIA_CODEC_UNAVAILABLE` naming the codec (STU-VID-051 item 3).
//!
//! Exactness: the published file is re-indexed by reel's own demuxer before the rename; its sample
//! counts must equal what was written, else the item fails and nothing is published. On failure or
//! cancel the temp file is removed and the output path is untouched.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{ReelError, check_cancel};
use crate::model::{Limits, VideoFormat};
use crate::model::ChromaFormat;
use crate::mux::{
    Container, MUXER_ID, MUXER_VERSION, MuxLimits, MuxReceipt, MuxSpec, Muxer, NclxColor, PcmSpec,
    VideoEntry, VideoTrackSpec,
};
use crate::os_encode::{
    EncodedUnit, ExternalVideoEncoder, OS_MAX_BITRATE, OsEncodeSpec, OsVideoCodec, OsVideoSettings,
    ParameterSets, VideoEncodeSession, annex_b_to_sample, build_avcc, build_hvcc,
};
use crate::vc3::encode::{VC3_ENCODER_ID, VC3_ENCODER_VERSION, Vc3Encoder, Vc3Profile};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

/// AV1 rate and speed settings (rav1e; STU-IO-143 rate-control facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Av1Settings {
    /// rav1e speed preset 0 (slowest) ..= 10 (fastest).
    pub speed: u8,
    /// Constant-quantizer mode (0..=255) when `bitrate` is `None`; the ceiling quantizer otherwise.
    pub quantizer: u8,
    /// Target bitrate in bits per second (rav1e rate control).
    pub bitrate: Option<u32>,
    /// Largest distance between key frames (frames, >= 1).
    pub max_keyframe_interval: u32,
}

impl Default for Av1Settings {
    fn default() -> Self {
        Self {
            speed: 6,
            quantizer: 100,
            bitrate: None,
            max_keyframe_interval: 240,
        }
    }
}

/// Video codec of a recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecChoice {
    /// VC-3 (DNxHD / DNxHR), Studio's encoder.
    Vc3(Vc3Profile),
    /// AV1 through rav1e (cargo feature `av1-encode`; else `MEDIA_CODEC_UNAVAILABLE av1`).
    Av1(Av1Settings),
    /// H.264 through the OS encoder only (D24-A; `ExportQueue::with_video_encoder`, else
    /// `MEDIA_CODEC_UNAVAILABLE h264`). MP4 or MOV, 8-bit 4:2:0, even sides.
    H264(OsVideoSettings),
    /// HEVC through the OS encoder only (D24-A; the Windows HEVC Video Extensions provide it, else the
    /// encoder's typed `MEDIA_CODEC_UNAVAILABLE hevc`). MP4 or MOV, 8-bit 4:2:0, even sides.
    Hevc(OsVideoSettings),
}

/// Linear PCM of a recipe (16/24/32-bit integer or 32-bit float).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmChoice {
    pub bits: u16,
    pub float: bool,
}

/// A portable export recipe with a stable identity (STU-VID-063: `does_video` = `video.is_some()`,
/// `does_audio` = `audio.is_some()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRecipe {
    /// Stable recipe identity (not a name or a path).
    pub recipe_id: String,
    pub container: Container,
    pub video: Option<VideoCodecChoice>,
    pub audio: Option<PcmChoice>,
}

/// Rendered frames of the source span, in presentation order.
pub trait ExportVideoSource: Send {
    /// Layout of every frame `frame` returns.
    fn format(&self) -> VideoFormat;
    /// Frame duration in 254016000000-per-second ticks (STU-IO-142).
    fn ticks_per_frame(&self) -> u64;
    fn frame_count(&self) -> u64;
    /// Planes (Y, U, V) of frame `index` in `VideoFormat` delivery layout.
    fn frame(&mut self, index: u64, cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError>;
}

/// Mixed-down audio of the source span (interleaved f32, nominal range -1..=1).
pub trait ExportAudioSource: Send {
    fn rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn frame_count(&self) -> u64;
    /// Append `frames` interleaved frames starting at frame `first` to `out`.
    fn render(
        &mut self,
        first: u64,
        frames: u32,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<(), ReelError>;
}

/// The timeline span an item exports (for the receipt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start_ticks: u64,
    pub end_ticks: u64,
}

/// Bounds of one item, checked before the first frame is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportLimits {
    pub mux: MuxLimits,
    /// Largest number of video frames.
    pub max_frames: u64,
    /// Largest number of audio frames.
    pub max_audio_frames: u64,
}

impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            mux: MuxLimits::default(),
            max_frames: 4_000_000,
            max_audio_frames: 48_000 * 60 * 60 * 24,
        }
    }
}

/// One export item.
pub struct ExportJob {
    pub recipe: ExportRecipe,
    pub video: Option<Box<dyn ExportVideoSource>>,
    pub audio: Option<Box<dyn ExportAudioSource>>,
    pub span: SourceSpan,
    /// Output file. Its directory must exist; an existing file is replaced only with `overwrite`.
    pub output: PathBuf,
    pub overwrite: bool,
    /// Artifact label named in the receipt (no directories).
    pub artifact: String,
    pub limits: ExportLimits,
}

/// Item state as seen by a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportState {
    Queued,
    Running,
    Complete,
    Failed,
    Canceled,
}

impl ExportState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Queued,
            1 => Self::Running,
            2 => Self::Complete,
            3 => Self::Failed,
            _ => Self::Canceled,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Running => 1,
            Self::Complete => 2,
            Self::Failed => 3,
            Self::Canceled => 4,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// Determinate progress: `done` of `total` units (video frames; audio frames for audio-only items).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProgress {
    pub state: ExportState,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug)]
struct Shared {
    state: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
}

/// Cloneable, thread-safe view of one queued item: progress and cancel.
#[derive(Debug, Clone)]
pub struct ExportHandle {
    pub id: u64,
    cancel: CancellationToken,
    shared: Arc<Shared>,
}

impl ExportHandle {
    pub fn progress(&self) -> ExportProgress {
        ExportProgress {
            state: ExportState::from_u8(self.shared.state.load(Ordering::Acquire)),
            done: self.shared.done.load(Ordering::Acquire),
            total: self.shared.total.load(Ordering::Acquire),
        }
    }

    /// Cancel the item: a queued item never starts, a running item stops at the next frame.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Outcome of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    Complete,
    Canceled,
    Failed { code: &'static str, detail: String },
}

/// Per-item receipt (STU-VID-064).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReceipt {
    pub item_id: u64,
    pub recipe_id: String,
    pub artifact: String,
    pub span: SourceSpan,
    pub outcome: ExportOutcome,
    pub container: Container,
    pub video_codec: Option<&'static str>,
    pub encoder_id: Option<&'static str>,
    pub encoder_version: Option<&'static str>,
    pub muxer_id: &'static str,
    pub muxer_version: &'static str,
    pub frames_encoded: u64,
    pub audio_frames: u64,
    /// Audio samples clamped into the PCM range (non-finite samples count here too, written as 0).
    pub audio_clipped: u64,
    pub output_bytes: u64,
    /// Lower-case hex SHA-256 of the published file.
    pub output_sha256: Option<String>,
    /// The published file was re-indexed by reel's demuxer and its sample counts matched.
    pub verified: bool,
    pub mux: Option<MuxReceipt>,
}

impl ExportReceipt {
    /// Deterministic JSON (no paths).
    pub fn to_json(&self) -> String {
        let esc = |s: &str| {
            s.chars()
                .flat_map(|c| match c {
                    '"' => "\\\"".chars().collect::<Vec<_>>(),
                    '\\' => "\\\\".chars().collect(),
                    c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
                    c => vec![c],
                })
                .collect::<String>()
        };
        let opt = |o: Option<&str>| o.map_or("null".to_owned(), |s| format!("\"{}\"", esc(s)));
        let (outcome, code, detail) = match &self.outcome {
            ExportOutcome::Complete => ("complete", None, None),
            ExportOutcome::Canceled => ("canceled", Some("MEDIA_CANCELED"), None),
            ExportOutcome::Failed { code, detail } => ("failed", Some(*code), Some(detail.as_str())),
        };
        format!(
            "{{\"item_id\":{},\"recipe_id\":\"{}\",\"artifact\":\"{}\",\"span\":{{\"start_ticks\":{},\"end_ticks\":{}}},\"outcome\":\"{}\",\"code\":{},\"detail\":{},\"container\":\"{}\",\"video_codec\":{},\"encoder\":{{\"id\":{},\"version\":{}}},\"muxer\":{{\"id\":\"{}\",\"version\":\"{}\"}},\"frames_encoded\":{},\"audio_frames\":{},\"audio_clipped\":{},\"output_bytes\":{},\"output_sha256\":{},\"verified\":{}}}",
            self.item_id,
            esc(&self.recipe_id),
            esc(&self.artifact),
            self.span.start_ticks,
            self.span.end_ticks,
            outcome,
            opt(code),
            opt(detail),
            self.container.as_str(),
            opt(self.video_codec),
            opt(self.encoder_id),
            opt(self.encoder_version),
            self.muxer_id,
            self.muxer_version,
            self.frames_encoded,
            self.audio_frames,
            self.audio_clipped,
            self.output_bytes,
            opt(self.output_sha256.as_deref()),
            self.verified,
        )
    }
}

struct Item {
    handle: ExportHandle,
    job: Option<ExportJob>,
}

/// One queue for any number of items; `run` executes them in submission order on the caller's thread
/// (move the queue to a worker thread for background export; handles stay usable from anywhere).
#[derive(Default)]
pub struct ExportQueue {
    items: Vec<Item>,
    next_id: u64,
    receipts: Vec<ExportReceipt>,
    /// The OS encoder for H.264 / HEVC recipes (D24-A); none by default.
    video_encoder: Option<Arc<dyn ExternalVideoEncoder>>,
}

impl ExportQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the external (OS) encoder H.264 and HEVC recipes run through (Operator decision D24-A:
    /// reel-native's `os-encode` worker). Without one those recipes are `MEDIA_CODEC_UNAVAILABLE`.
    pub fn with_video_encoder(mut self, encoder: Arc<dyn ExternalVideoEncoder>) -> Self {
        self.video_encoder = Some(encoder);
        self
    }

    pub fn submit(&mut self, job: ExportJob) -> ExportHandle {
        self.next_id = self.next_id.saturating_add(1);
        let handle = ExportHandle {
            id: self.next_id,
            cancel: CancellationToken::default(),
            shared: Arc::new(Shared {
                state: AtomicU8::new(ExportState::Queued.as_u8()),
                done: AtomicU64::new(0),
                total: AtomicU64::new(0),
            }),
        };
        self.items.push(Item {
            handle: handle.clone(),
            job: Some(job),
        });
        handle
    }

    /// Handles of every item, in submission order.
    pub fn handles(&self) -> Vec<ExportHandle> {
        self.items.iter().map(|i| i.handle.clone()).collect()
    }

    /// Run every item not yet run; returns the receipts of this call.
    pub fn run(&mut self) -> Vec<ExportReceipt> {
        let mut out = Vec::new();
        let external = self.video_encoder.clone();
        for item in &mut self.items {
            let Some(job) = item.job.take() else { continue };
            let receipt = run_item(item.handle.id, job, &item.handle, external.as_deref());
            let state = match receipt.outcome {
                ExportOutcome::Complete => ExportState::Complete,
                ExportOutcome::Canceled => ExportState::Canceled,
                ExportOutcome::Failed { .. } => ExportState::Failed,
            };
            item.handle
                .shared
                .state
                .store(state.as_u8(), Ordering::Release);
            out.push(receipt);
        }
        self.receipts.extend(out.iter().cloned());
        out
    }

    /// Every receipt so far.
    pub fn receipts(&self) -> &[ExportReceipt] {
        &self.receipts
    }
}

/// An OS encoder session (D24-A) and the parameter sets its first units carried.
struct OsSession {
    codec: OsVideoCodec,
    session: Box<dyn VideoEncodeSession>,
    encoder_id: &'static str,
    encoder_version: &'static str,
    sets: ParameterSets,
    units: Vec<EncodedUnit>,
}

enum Encoder {
    Vc3(Box<Vc3Encoder>),
    #[cfg(feature = "av1-encode")]
    Av1(Box<crate::av1_encode::Av1Encoder>),
    Os(Box<OsSession>),
}

impl Encoder {
    fn identity(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Os(os) => (os.codec.as_str(), os.encoder_id, os.encoder_version),
            Self::Vc3(_) => ("vc3", VC3_ENCODER_ID, VC3_ENCODER_VERSION),
            #[cfg(feature = "av1-encode")]
            Self::Av1(_) => (
                "av1",
                crate::av1_encode::AV1_ENCODER_ID,
                crate::av1_encode::AV1_ENCODER_VERSION,
            ),
        }
    }
}

/// One encoded sample on its way to the muxer.
struct Sample {
    data: Vec<u8>,
    pts: u64,
    sync: bool,
}

/// Frame rate of `ticks_per_frame` as a reduced rational that fits u32 (STU-IO-142 rates do).
fn frame_rate(ticks_per_frame: u64) -> Result<(u32, u32), ReelError> {
    let (mut a, mut b) = (TICKS_PER_SECOND, ticks_per_frame);
    while b != 0 {
        let r = a.checked_rem(b).unwrap_or(0);
        a = b;
        b = r;
    }
    let num = TICKS_PER_SECOND.checked_div(a).and_then(|n| u32::try_from(n).ok());
    let den = ticks_per_frame.checked_div(a).and_then(|d| u32::try_from(d).ok());
    match (num, den) {
        (Some(num), Some(den)) if num > 0 && den > 0 => Ok((num, den)),
        _ => Err(unsupported("os-encode-frame-rate")),
    }
}

/// Opens the OS encoder session of an H.264 / HEVC recipe (D24-A) after reel's own checks.
fn open_os(
    codec: OsVideoCodec,
    settings: OsVideoSettings,
    format: VideoFormat,
    ticks_per_frame: u64,
    external: Option<&dyn ExternalVideoEncoder>,
    cancel: &CancellationToken,
) -> Result<Encoder, ReelError> {
    let Some(external) = external.filter(|e| e.supports(codec)) else {
        return Err(ReelError::CodecUnavailable {
            codec: codec.as_str().to_owned(),
        });
    };
    if format.bit_depth != 8 || format.chroma != ChromaFormat::Yuv420 {
        return Err(unsupported("os-encode-pixel-format"));
    }
    if format.width == 0
        || format.height == 0
        || !format.width.is_multiple_of(2)
        || !format.height.is_multiple_of(2)
    {
        return Err(unsupported("os-encode-dimensions"));
    }
    if settings.bitrate == 0 || settings.bitrate > OS_MAX_BITRATE {
        return Err(limit("os-encode-bitrate"));
    }
    let (fps_num, fps_den) = frame_rate(ticks_per_frame)?;
    let spec = OsEncodeSpec {
        codec,
        width: format.width,
        height: format.height,
        fps_num,
        fps_den,
        bitrate: settings.bitrate,
        profile: match codec {
            OsVideoCodec::H264 => settings.profile.profile_idc(),
            OsVideoCodec::Hevc => 1,
        },
    };
    let session = external.open(&spec, cancel)?;
    let identity = external.identity(codec);
    Ok(Encoder::Os(Box::new(OsSession {
        codec,
        session,
        encoder_id: identity.id,
        encoder_version: identity.version,
        sets: ParameterSets::default(),
        units: Vec::new(),
    })))
}

/// A provisional sample entry for the muxer's combination check before the first unit (the real record is
/// built from the first key unit's parameter sets): placeholder parameter sets of one byte each.
fn provisional_entry(codec: OsVideoCodec) -> Result<VideoEntry, ReelError> {
    Ok(match codec {
        OsVideoCodec::H264 => VideoEntry::H264Remux {
            avcc: build_avcc(&ParameterSets {
                vps: None,
                sps: Some(vec![0x67, 100, 0, 40]),
                pps: Some(vec![0x68]),
            })?,
        },
        OsVideoCodec::Hevc => {
            let mut hvcc = vec![
                1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5D, 0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0,
                0, 0x0F, 3,
            ];
            for (kind, nal) in [(0xA0u8, 0x40u8), (0xA1, 0x42), (0xA2, 0x44)] {
                hvcc.extend_from_slice(&[kind, 0, 1, 0, 1, nal]);
            }
            VideoEntry::Hevc { hvcc }
        }
    })
}

fn make_encoder(
    choice: VideoCodecChoice,
    format: VideoFormat,
    ticks_per_frame: u64,
    external: Option<&dyn ExternalVideoEncoder>,
    cancel: &CancellationToken,
) -> Result<Encoder, ReelError> {
    match choice {
        VideoCodecChoice::H264(settings) => open_os(
            OsVideoCodec::H264,
            settings,
            format,
            ticks_per_frame,
            external,
            cancel,
        ),
        VideoCodecChoice::Hevc(settings) => open_os(
            OsVideoCodec::Hevc,
            settings,
            format,
            ticks_per_frame,
            external,
            cancel,
        ),
        VideoCodecChoice::Vc3(profile) => Ok(Encoder::Vc3(Box::new(Vc3Encoder::new(profile, format)?))),
        #[cfg(feature = "av1-encode")]
        VideoCodecChoice::Av1(settings) => Ok(Encoder::Av1(Box::new(
            crate::av1_encode::Av1Encoder::new(format, ticks_per_frame, settings)?,
        ))),
        #[cfg(not(feature = "av1-encode"))]
        VideoCodecChoice::Av1(_) => {
            let _ = ticks_per_frame;
            Err(ReelError::CodecUnavailable {
                codec: "av1".to_owned(),
            })
        }
    }
}

fn nclx(format: &VideoFormat) -> Option<NclxColor> {
    let c = format.color;
    (c.primaries != 2 || c.transfer != 2 || c.matrix != 2).then_some(NclxColor {
        primaries: c.primaries,
        transfer: c.transfer,
        matrix: c.matrix,
        full_range: c.full_range,
    })
}

/// Everything decided before the first frame is requested.
struct Plan {
    encoder: Option<Encoder>,
    spec: MuxSpec,
    frames: u64,
    ticks_per_frame: u64,
    audio_frames: u64,
}

fn plan(
    job: &ExportJob,
    external: Option<&dyn ExternalVideoEncoder>,
    cancel: &CancellationToken,
) -> Result<Plan, ReelError> {
    let recipe = &job.recipe;
    if recipe.video.is_none() && recipe.audio.is_none() {
        return Err(unsupported("export-recipe-empty"));
    }
    if recipe.video.is_some() != job.video.is_some() {
        return Err(unsupported("export-video-source-mismatch"));
    }
    if recipe.audio.is_some() != job.audio.is_some() {
        return Err(unsupported("export-audio-source-mismatch"));
    }
    if job.output.file_name().is_none() {
        return Err(unsupported("export-output-path"));
    }
    if !job.overwrite && job.output.exists() {
        return Err(unsupported("export-output-exists"));
    }
    let mut encoder = None;
    let mut video_spec = None;
    let (mut frames, mut tpf) = (0u64, 0u64);
    if let (Some(choice), Some(source)) = (recipe.video, job.video.as_ref()) {
        let format = source.format();
        tpf = source.ticks_per_frame();
        frames = source.frame_count();
        if frames == 0 {
            return Err(unsupported("export-no-frames"));
        }
        if frames > job.limits.max_frames
            || frames > u64::try_from(job.limits.mux.max_video_samples).unwrap_or(u64::MAX)
        {
            return Err(limit("export-frames"));
        }
        if format.frame_bytes().is_none() {
            return Err(limit("export-frame-bytes"));
        }
        let enc = make_encoder(choice, format, tpf, external, cancel)?;
        let entry = match &enc {
            Encoder::Os(os) => provisional_entry(os.codec)?,
            Encoder::Vc3(e) => VideoEntry::Vc3 {
                dnxhr: e.profile().is_dnxhr(),
            },
            // Provisional record (container header only) for the combination check; the real one with
            // the sequence header OBU is built from the first temporal unit.
            #[cfg(feature = "av1-encode")]
            Encoder::Av1(e) => VideoEntry::Av1 {
                av1c: e.config_record(&[]),
            },
        };
        video_spec = Some(VideoTrackSpec {
            entry,
            width: format.width,
            height: format.height,
            ticks_per_frame: tpf,
            color: nclx(&format),
        });
        encoder = Some(enc);
    }
    let mut audio_spec = None;
    let mut audio_frames = 0u64;
    if let (Some(choice), Some(source)) = (recipe.audio, job.audio.as_ref()) {
        audio_frames = source.frame_count();
        if audio_frames > job.limits.max_audio_frames {
            return Err(limit("export-audio-frames"));
        }
        if audio_frames == 0 {
            return Err(unsupported("export-no-audio-frames"));
        }
        audio_spec = Some(PcmSpec {
            bits: choice.bits,
            float: choice.float,
            rate: source.rate(),
            channels: source.channels(),
        });
    }
    let spec = MuxSpec {
        container: recipe.container,
        video: video_spec,
        audio: audio_spec,
    };
    // Combination and bound check of the muxer, on a sink that keeps nothing.
    Muxer::new(std::io::Cursor::new(Vec::new()), spec.clone(), job.limits.mux)?;
    Ok(Plan {
        encoder,
        spec,
        frames,
        ticks_per_frame: tpf,
        audio_frames,
    })
}

/// Audio frames that end at or before the end of video frame `index` (exact integer arithmetic).
fn audio_end_for_frame(index: u64, tpf: u64, rate: u32, total: u64) -> u64 {
    let end_ticks = u128::from(index.saturating_add(1)).saturating_mul(u128::from(tpf));
    let frames = end_ticks
        .saturating_mul(u128::from(rate))
        .checked_div(u128::from(TICKS_PER_SECOND))
        .unwrap_or(0);
    u64::try_from(frames).unwrap_or(u64::MAX).min(total)
}

/// Convert interleaved f32 to the recipe's PCM layout; counts clamped samples.
fn pcm_bytes(samples: &[f32], choice: PcmChoice, out: &mut Vec<u8>, clipped: &mut u64) {
    for &s in samples {
        let finite = if s.is_finite() { s } else { 0.0 };
        if !s.is_finite() || !(-1.0..=1.0).contains(&s) {
            *clipped = clipped.saturating_add(1);
        }
        let v = f64::from(finite.clamp(-1.0, 1.0));
        match (choice.float, choice.bits) {
            (true, _) => out.extend_from_slice(&(v as f32).to_le_bytes()),
            (false, 16) => out.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
            (false, 24) => out.extend_from_slice(&((v * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
            _ => out.extend_from_slice(&((v * 2_147_483_647.0).round() as i32).to_le_bytes()),
        }
    }
}

struct Run<'a> {
    handle: &'a ExportHandle,
    clipped: u64,
    audio_written: u64,
    frames_encoded: u64,
}

fn temp_path(output: &Path, id: u64) -> PathBuf {
    let name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!(".{name}.partial-{}-{id}", std::process::id()))
}

fn run_item(
    id: u64,
    mut job: ExportJob,
    handle: &ExportHandle,
    external: Option<&dyn ExternalVideoEncoder>,
) -> ExportReceipt {
    let mut receipt = ExportReceipt {
        item_id: id,
        recipe_id: job.recipe.recipe_id.clone(),
        artifact: job.artifact.clone(),
        span: job.span,
        outcome: ExportOutcome::Canceled,
        container: job.recipe.container,
        video_codec: None,
        encoder_id: None,
        encoder_version: None,
        muxer_id: MUXER_ID,
        muxer_version: MUXER_VERSION,
        frames_encoded: 0,
        audio_frames: 0,
        audio_clipped: 0,
        output_bytes: 0,
        output_sha256: None,
        verified: false,
        mux: None,
    };
    if handle.cancel.check().is_err() {
        return receipt;
    }
    handle
        .shared
        .state
        .store(ExportState::Running.as_u8(), Ordering::Release);
    let temp = temp_path(&job.output, id);
    let mut run = Run {
        handle,
        clipped: 0,
        audio_written: 0,
        frames_encoded: 0,
    };
    let result = plan(&job, external, &handle.cancel).and_then(|plan| {
        if let Some(enc) = &plan.encoder {
            let (codec, eid, ever) = enc.identity();
            receipt.video_codec = Some(codec);
            receipt.encoder_id = Some(eid);
            receipt.encoder_version = Some(ever);
        }
        execute(&mut job, plan, &temp, &mut run)
    });
    receipt.frames_encoded = run.frames_encoded;
    receipt.audio_frames = run.audio_written;
    receipt.audio_clipped = run.clipped;
    match result {
        Ok((mux, bytes, sha, verified)) => {
            receipt.outcome = ExportOutcome::Complete;
            receipt.verified = verified;
            receipt.mux = Some(mux);
            receipt.output_bytes = bytes;
            receipt.output_sha256 = Some(sha);
        }
        Err(error) => {
            // Our own temp file only; the output path is untouched.
            let _ = std::fs::remove_file(&temp);
            receipt.outcome = match error {
                ReelError::Canceled => ExportOutcome::Canceled,
                e => ExportOutcome::Failed {
                    code: e.code(),
                    detail: e.to_string(),
                },
            };
        }
    }
    receipt
}

/// Lazily created muxer: the AV1 record needs the first temporal unit.
struct Sink {
    writer: Option<BufWriter<File>>,
    muxer: Option<Muxer<BufWriter<File>>>,
    spec: MuxSpec,
    limits: MuxLimits,
}

impl Sink {
    fn muxer(&mut self, entry: Option<VideoEntry>) -> Result<&mut Muxer<BufWriter<File>>, ReelError> {
        if self.muxer.is_none() {
            let mut spec = self.spec.clone();
            if let (Some(entry), Some(v)) = (entry, spec.video.as_mut()) {
                v.entry = entry;
            }
            let w = self.writer.take().ok_or(unsupported("export-internal"))?;
            self.muxer = Some(Muxer::new(w, spec, self.limits)?);
        }
        self.muxer.as_mut().ok_or(unsupported("export-internal"))
    }
}

/// The sample entry built from the first sample (`av1C` for AV1; `avcC` / `hvcC` from the parameter sets
/// of the OS encoder's first units); `None` keeps the planned entry.
fn record_for(encoder: &Encoder, first: Option<&Sample>) -> Result<Option<VideoEntry>, ReelError> {
    Ok(match encoder {
        Encoder::Vc3(_) => {
            let _ = first;
            None
        }
        #[cfg(feature = "av1-encode")]
        Encoder::Av1(e) => first.map(|s| VideoEntry::Av1 {
            av1c: e.config_record(&s.data),
        }),
        Encoder::Os(os) if first.is_some() => Some(match os.codec {
            OsVideoCodec::H264 => VideoEntry::H264Remux {
                avcc: build_avcc(&os.sets)?,
            },
            OsVideoCodec::Hevc => VideoEntry::Hevc {
                hvcc: build_hvcc(&os.sets)?,
            },
        }),
        Encoder::Os(_) => None,
    })
}

/// The OS encoder's units as muxer samples, checked: a unit presents a frame already sent (never a later
/// one) and the first unit is a key unit.
fn os_samples(os: &mut OsSession, sent_up_to: u64, samples: &mut Vec<Sample>) -> Result<(), ReelError> {
    for unit in os.units.drain(..) {
        if unit.frame_index > sent_up_to || (os.sets.sps.is_none() && !unit.keyframe) {
            return Err(ReelError::DecoderFault {
                fault: crate::error::DecoderFault::Protocol,
            });
        }
        let data = annex_b_to_sample(os.codec, &unit.annex_b, &mut os.sets)?;
        samples.push(Sample {
            data,
            pts: unit.frame_index,
            sync: unit.keyframe,
        });
    }
    Ok(())
}

/// Audio delivery into the muxer in bounded chunks.
struct AudioOut {
    choice: Option<PcmChoice>,
    channels: u16,
    buf: Vec<f32>,
    pcm: Vec<u8>,
}

impl AudioOut {
    fn write_until(
        &mut self,
        muxer: &mut Muxer<BufWriter<File>>,
        end: u64,
        run: &mut Run<'_>,
        source: &mut Option<Box<dyn ExportAudioSource>>,
        cancel: &CancellationToken,
    ) -> Result<(), ReelError> {
        let (Some(source), Some(choice)) = (source.as_mut(), self.choice) else {
            return Ok(());
        };
        while run.audio_written < end {
            check_cancel(cancel)?;
            let n = u32::try_from(end.saturating_sub(run.audio_written).min(48_000))
                .unwrap_or(48_000);
            self.buf.clear();
            source.render(run.audio_written, n, &mut self.buf, cancel)?;
            let expected = usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_mul(usize::from(self.channels)))
                .ok_or(limit("export-audio-frames"))?;
            if self.buf.len() != expected {
                return Err(ReelError::Incomplete {
                    requested: u64::from(n),
                    delivered: u64::try_from(self.buf.len()).unwrap_or(0),
                });
            }
            self.pcm.clear();
            pcm_bytes(&self.buf, choice, &mut self.pcm, &mut run.clipped);
            muxer.write_audio(&self.pcm)?;
            run.audio_written = run.audio_written.saturating_add(u64::from(n));
        }
        Ok(())
    }
}

/// Encode one frame into `samples`.
fn encode_frame(
    encoder: &mut Encoder,
    planes: &[&[u8]],
    index: u64,
    samples: &mut Vec<Sample>,
    cancel: &CancellationToken,
) -> Result<(), ReelError> {
    match encoder {
        Encoder::Os(os) => {
            os.session.encode(planes, index, cancel, &mut os.units)?;
            os_samples(os, index, samples)?;
        }
        Encoder::Vc3(e) => {
            let data = e.encode(planes, cancel)?;
            samples.push(Sample {
                data,
                pts: index,
                sync: true,
            });
        }
        #[cfg(feature = "av1-encode")]
        Encoder::Av1(e) => {
            let mut out = Vec::new();
            e.encode(planes, &mut out)?;
            samples.extend(out.into_iter().map(|p| Sample {
                data: p.data,
                pts: p.frame_index,
                sync: p.keyframe,
            }));
        }
    }
    Ok(())
}

/// Drain the encoder at the end of the stream.
fn finish_encoder(
    encoder: &mut Encoder,
    samples: &mut Vec<Sample>,
    frames: u64,
    cancel: &CancellationToken,
) -> Result<(), ReelError> {
    match encoder {
        Encoder::Os(os) => {
            os.session.finish(cancel, &mut os.units)?;
            os_samples(os, frames.saturating_sub(1), samples)?;
        }
        Encoder::Vc3(_) => {
            let _ = samples;
        }
        #[cfg(feature = "av1-encode")]
        Encoder::Av1(e) => {
            let mut out = Vec::new();
            e.finish(&mut out)?;
            samples.extend(out.into_iter().map(|p| Sample {
                data: p.data,
                pts: p.frame_index,
                sync: p.keyframe,
            }));
        }
    }
    Ok(())
}

/// Encode, mux, verify and publish. Returns the mux receipt, the file size and its SHA-256.
fn execute(
    job: &mut ExportJob,
    mut plan: Plan,
    temp: &Path,
    run: &mut Run<'_>,
) -> Result<(MuxReceipt, u64, String, bool), ReelError> {
    let cancel = run.handle.cancel.clone();
    let total = if plan.frames > 0 {
        plan.frames
    } else {
        plan.audio_frames
    };
    run.handle.shared.total.store(total, Ordering::Release);
    let file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    let mut sink = Sink {
        writer: Some(BufWriter::new(file)),
        muxer: None,
        spec: plan.spec.clone(),
        limits: job.limits.mux,
    };
    let mut audio = AudioOut {
        choice: job.recipe.audio,
        channels: plan.spec.audio.map_or(0, |a| a.channels),
        buf: Vec::new(),
        pcm: Vec::new(),
    };
    let rate = plan.spec.audio.map_or(0, |a| a.rate);
    let mut samples: Vec<Sample> = Vec::new();
    if let (Some(source), Some(encoder)) = (job.video.as_mut(), plan.encoder.as_mut()) {
        for index in 0..plan.frames {
            check_cancel(&cancel)?;
            let planes = source.frame(index, &cancel)?;
            let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
            encode_frame(encoder, &refs, index, &mut samples, &cancel)?;
            run.frames_encoded = run.frames_encoded.saturating_add(1);
            if !samples.is_empty() {
                let m = sink.muxer(record_for(encoder, samples.first())?)?;
                for s in samples.drain(..) {
                    m.write_video(&s.data, s.pts, s.sync)?;
                }
                let end = audio_end_for_frame(index, plan.ticks_per_frame, rate, plan.audio_frames);
                audio.write_until(m, end, run, &mut job.audio, &cancel)?;
            }
            run.handle
                .shared
                .done
                .store(run.frames_encoded, Ordering::Release);
        }
        finish_encoder(encoder, &mut samples, plan.frames, &cancel)?;
        let m = sink.muxer(record_for(encoder, samples.first())?)?;
        for s in samples.drain(..) {
            m.write_video(&s.data, s.pts, s.sync)?;
        }
    }
    let m = sink.muxer(None)?;
    // Remaining audio after the last video frame (all of it for audio-only items), with progress.
    while run.audio_written < plan.audio_frames {
        let end = run.audio_written.saturating_add(48_000).min(plan.audio_frames);
        audio.write_until(m, end, run, &mut job.audio, &cancel)?;
        if plan.frames == 0 {
            run.handle
                .shared
                .done
                .store(run.audio_written, Ordering::Release);
        }
    }
    check_cancel(&cancel)?;
    let muxer = sink.muxer.take().ok_or(unsupported("export-internal"))?;
    let (writer, mux) = muxer.finish()?;
    let file = writer
        .into_inner()
        .map_err(|e| ReelError::Io(e.error().kind()))?;
    file.sync_all()?;
    drop(file);
    let (bytes, sha, verified) = verify(temp, &mux, &cancel)?;
    if job.overwrite && job.output.exists() {
        std::fs::remove_file(&job.output)?;
    }
    std::fs::rename(temp, &job.output)?;
    Ok((mux, bytes, sha, verified))
}

/// Most samples a post-write re-index may expand (MP4 ipcm/fpcm audio is one table entry per audio
/// frame, because reel's compact PCM index does not know those sample entries).
const MAX_VERIFY_SAMPLES: u64 = 8_000_000;

/// Re-index the written file with reel's demuxer: track sample counts must equal what was written.
/// Returns `(bytes, sha256, verified)`; `verified` is false only for an MP4 PCM item whose audio table
/// exceeds `MAX_VERIFY_SAMPLES` (the file is then published with that fact in the receipt).
fn verify(
    path: &Path,
    mux: &MuxReceipt,
    cancel: &CancellationToken,
) -> Result<(u64, String, bool), ReelError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let expanded = mux.video_samples.saturating_add(if mux.container == Container::Mp4 {
        mux.audio_frames
    } else {
        0
    });
    let verified = expanded <= MAX_VERIFY_SAMPLES;
    if verified {
        let limits = Limits {
            max_samples: usize::try_from(expanded.saturating_add(16))
                .unwrap_or(usize::MAX)
                .max(Limits::default().max_samples),
            ..Limits::default()
        };
        let index = crate::isobmff::build_index(&mut file, len, &limits, cancel)?;
        let mut tracks = index.tracks.iter();
        if mux.video_samples > 0 {
            let t = tracks.next().ok_or(unsupported("export-verify-video-track"))?;
            if u64::try_from(t.samples().len()).ok() != Some(mux.video_samples) {
                return Err(unsupported("export-verify-video-samples"));
            }
        }
        if mux.audio_frames > 0 {
            let t = tracks.next().ok_or(unsupported("export-verify-audio-track"))?;
            let frames = t
                .pcm()
                .map(|p| p.total_frames)
                .or_else(|| u64::try_from(t.samples().len()).ok());
            if frames != Some(mux.audio_frames) {
                return Err(unsupported("export-verify-audio-frames"));
            }
        }
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(buf.get(..n).unwrap_or_default());
    }
    Ok((len, crate::identity::hex(&hash.finalize()), verified))
}
