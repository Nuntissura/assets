//! Audition-style batch processing (lane SMB-AUDIO-IMPORT-2; feature `audio-batch`): a typed job per
//! file runs import (module [`super::audio_import`]: decode, BS.775 downmix), a score effect chain
//! (`hsk_studio_score::Chain` bounced by `Engine::bounce`: latency compensated, cancel checked between
//! blocks) and export (FLAC with STREAMINFO MD5, or AIFF / AIFF-C). The queue follows reel's export
//! queue (`hsk_studio_reel::ExportQueue`): [`AudioBatchQueue::submit`] returns a cloneable, thread-safe
//! [`AudioBatchHandle`] with determinate progress (frames processed plus frames encoded, of twice the
//! file's frames) and cancel (also before the item starts); [`AudioBatchQueue::run`] executes the
//! items in submission order on the caller's thread and returns one [`AudioBatchReceipt`] per item.
//!
//! Every refusable input (output directory, existing output without `overwrite`, output = input, chain
//! length, export format) is checked before the input is read. The output is written to a sibling
//! temp file, re-decoded by reel (frame count, channel count, FLAC MD5 must match) and renamed over the
//! output only when the whole item succeeded; on failure or cancel the temp file is removed and the
//! output path is untouched.
#![deny(clippy::arithmetic_side_effects)]

use super::audio_import::{
    BlockSource, IMPORT_CLIP_ID, ImportLimits, ImportOptions, MixExport, MixFormat, PlanarSource,
    export_from, import_audio_with, read_audio_file, write_atomically,
};
use super::monitor::MonitorFail;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio as folio;
use hsk_studio_reel as reel;
use hsk_studio_score as score;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// One effect of the chain, built for the file's rate and channel count.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectSpec {
    /// `score::Gain` in dB.
    Gain { db: f64 },
    /// `score::Equalizer` sections and master gain.
    Equalizer {
        sections: Vec<score::BiquadSpec>,
        master_gain_db: f64,
    },
    /// `score::Dynamics` (compressor / expander).
    Dynamics(score::DynamicsParams),
    /// `score::LookaheadLimiter`.
    Limiter(score::LimiterParams),
}

impl EffectSpec {
    fn build(&self, rate: u32, channels: usize) -> Result<Box<dyn score::Processor + Send>, score::ScoreError> {
        Ok(match self {
            Self::Gain { db } => Box::new(score::Gain::from_db(*db)?),
            Self::Equalizer {
                sections,
                master_gain_db,
            } => Box::new(score::Equalizer::new(rate, channels, sections, *master_gain_db)?),
            Self::Dynamics(p) => Box::new(score::Dynamics::new(rate, channels, *p)?),
            Self::Limiter(p) => Box::new(score::LookaheadLimiter::new(rate, channels, *p)?),
        })
    }
}

/// One batch item.
#[derive(Clone, Debug)]
pub struct AudioBatchJob {
    pub input: PathBuf,
    /// Output file. Its directory must exist; an existing file is replaced only with `overwrite`.
    pub output: PathBuf,
    pub overwrite: bool,
    /// Artifact label named in the receipt (no directories).
    pub artifact: String,
    pub options: ImportOptions,
    pub limits: ImportLimits,
    /// Ordered effects (empty: import and export only).
    pub chain: Vec<EffectSpec>,
    pub format: MixFormat,
}

/// Item state as seen by a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchState {
    Queued,
    Importing,
    Processing,
    Encoding,
    Complete,
    Failed,
    Canceled,
}

impl BatchState {
    const ALL: [Self; 7] = [
        Self::Queued,
        Self::Importing,
        Self::Processing,
        Self::Encoding,
        Self::Complete,
        Self::Failed,
        Self::Canceled,
    ];
    fn as_u8(self) -> u8 {
        Self::ALL.iter().position(|s| *s == self).and_then(|i| u8::try_from(i).ok()).unwrap_or(0)
    }
    fn from_u8(v: u8) -> Self {
        Self::ALL.get(usize::from(v)).copied().unwrap_or(Self::Failed)
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Importing => "importing",
            Self::Processing => "processing",
            Self::Encoding => "encoding",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// Determinate progress: `done` of `total` (frames processed + frames encoded; `total` is 0 until the
/// file is imported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchProgress {
    pub state: BatchState,
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
pub struct AudioBatchHandle {
    pub id: u64,
    cancel: CancellationToken,
    shared: Arc<Shared>,
}

impl AudioBatchHandle {
    pub fn progress(&self) -> BatchProgress {
        BatchProgress {
            state: BatchState::from_u8(self.shared.state.load(Ordering::Acquire)),
            done: self.shared.done.load(Ordering::Acquire),
            total: self.shared.total.load(Ordering::Acquire),
        }
    }

    /// Cancel the item: a queued item never starts, a running item stops at the next block.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    fn set(&self, state: BatchState) {
        self.shared.state.store(state.as_u8(), Ordering::Release);
    }
}

/// Outcome of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Complete,
    Canceled,
    Failed { code: &'static str, detail: String },
}

/// Per-item receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBatchReceipt {
    pub item_id: u64,
    pub artifact: String,
    pub outcome: BatchOutcome,
    /// The import's structured projection (`ImportedAudio::to_json`), when the file was imported.
    pub import: Option<Value>,
    /// Score chain identity (`BounceReceipt::chain_identity_hex`), bounce sample digest and the latency
    /// frames the bounce compensated; `None` for an empty chain.
    pub chain_identity: Option<String>,
    pub bounce_digest: Option<String>,
    pub latency_dropped_frames: u32,
    pub chain: Vec<String>,
    pub export: Option<MixExport>,
    /// Lower-case hex SHA-256 of the published file.
    pub output_sha256: Option<String>,
    /// The published file was re-decoded by reel: same frames and channels, and the FLAC MD5 matched.
    pub verified: bool,
}

impl AudioBatchReceipt {
    /// Deterministic JSON (no paths).
    pub fn to_json(&self) -> Value {
        let (outcome, code, detail) = match &self.outcome {
            BatchOutcome::Complete => ("complete", None, None),
            BatchOutcome::Canceled => ("canceled", Some("canceled"), None),
            BatchOutcome::Failed { code, detail } => ("failed", Some(*code), Some(detail.clone())),
        };
        json!({
            "item_id": self.item_id,
            "artifact": self.artifact,
            "outcome": outcome,
            "code": code,
            "detail": detail,
            "import": self.import,
            "chain": self.chain,
            "chain_identity": self.chain_identity,
            "bounce_digest": self.bounce_digest,
            "latency_dropped_frames": self.latency_dropped_frames,
            "export": self.export.as_ref().map(MixExport::to_json),
            "output_sha256": self.output_sha256,
            "verified": self.verified,
        })
    }
}

struct Item {
    handle: AudioBatchHandle,
    job: Option<AudioBatchJob>,
}

/// One queue for any number of files; `run` executes them in submission order on the caller's thread
/// (move the queue to a worker thread for background work; handles stay usable from anywhere).
pub struct AudioBatchQueue {
    actor: ActorContext,
    items: Vec<Item>,
    next_id: u64,
    receipts: Vec<AudioBatchReceipt>,
}

const BLOCK: usize = 4096;

impl AudioBatchQueue {
    /// `actor` is named on every score request (score refuses a request without one).
    pub fn new(actor: ActorContext) -> Self {
        Self {
            actor,
            items: Vec::new(),
            next_id: 0,
            receipts: Vec::new(),
        }
    }

    pub fn submit(&mut self, job: AudioBatchJob) -> AudioBatchHandle {
        self.next_id = self.next_id.saturating_add(1);
        let handle = AudioBatchHandle {
            id: self.next_id,
            cancel: CancellationToken::default(),
            shared: Arc::new(Shared {
                state: AtomicU8::new(BatchState::Queued.as_u8()),
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
    pub fn handles(&self) -> Vec<AudioBatchHandle> {
        self.items.iter().map(|i| i.handle.clone()).collect()
    }

    /// Run every item not yet run; returns the receipts of this call.
    pub fn run(&mut self) -> Vec<AudioBatchReceipt> {
        let mut out = Vec::new();
        for item in &mut self.items {
            let Some(job) = item.job.take() else { continue };
            let mut receipt = AudioBatchReceipt {
                item_id: item.handle.id,
                artifact: job.artifact.clone(),
                outcome: BatchOutcome::Complete,
                import: None,
                chain_identity: None,
                bounce_digest: None,
                latency_dropped_frames: 0,
                chain: Vec::new(),
                export: None,
                output_sha256: None,
                verified: false,
            };
            let result = run_item(&self.actor, &job, &item.handle, &mut receipt);
            receipt.outcome = match result {
                Ok(()) => BatchOutcome::Complete,
                Err(e) if e.code == "canceled" || item.handle.cancel.check().is_err() => BatchOutcome::Canceled,
                Err(e) => BatchOutcome::Failed {
                    code: e.code,
                    detail: e.detail,
                },
            };
            if receipt.outcome != BatchOutcome::Complete {
                receipt.export = None;
                receipt.output_sha256 = None;
                receipt.verified = false;
            }
            item.handle.set(match receipt.outcome {
                BatchOutcome::Complete => BatchState::Complete,
                BatchOutcome::Canceled => BatchState::Canceled,
                BatchOutcome::Failed { .. } => BatchState::Failed,
            });
            out.push(receipt);
        }
        self.receipts.extend(out.iter().cloned());
        out
    }

    /// Every receipt so far.
    pub fn receipts(&self) -> &[AudioBatchReceipt] {
        &self.receipts
    }
}

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

fn score_fail(e: score::ScoreError) -> MonitorFail {
    if e == score::ScoreError::Canceled {
        fail("canceled", e.code())
    } else {
        fail("effect_chain", e.code())
    }
}

fn run_item(
    actor: &ActorContext,
    job: &AudioBatchJob,
    handle: &AudioBatchHandle,
    receipt: &mut AudioBatchReceipt,
) -> Result<(), MonitorFail> {
    let cancel = &handle.cancel;
    cancel.check().map_err(|_| fail("canceled", "before start"))?;
    // Refusable inputs, before the input is read.
    if job.chain.len() > score::MAX_CHAIN_STAGES {
        return Err(fail("effect_chain", format!("{} stages exceeds {}", job.chain.len(), score::MAX_CHAIN_STAGES)));
    }
    if !matches!(job.format, MixFormat::Flac { bits: 16 | 24 } | MixFormat::Aiff { bits: 16 | 24 } | MixFormat::AiffFloat) {
        return Err(fail("export_format", format!("{:?}", job.format)));
    }
    let dir = job.output.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    if !dir.is_dir() {
        return Err(fail("output_path", "output directory does not exist"));
    }
    let same = |a: &std::path::Path, b: &std::path::Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(&job.input, &job.output) {
        return Err(fail("output_path", "output is the input"));
    }
    if job.output.exists() && !job.overwrite {
        return Err(fail("output_exists", job.artifact.clone()));
    }

    handle.set(BatchState::Importing);
    let bytes = read_audio_file(&job.input, &job.limits)?;
    let audio = import_audio_with(&mut Cursor::new(bytes), &job.limits, &job.options, cancel)?;
    receipt.import = Some(audio.to_json());
    let frames = audio.frames;
    handle.shared.total.store(frames.saturating_mul(2), Ordering::Release);

    handle.set(BatchState::Processing);
    let channels = audio.planar.len();
    let processed: Vec<Vec<f32>> = if job.chain.is_empty() {
        handle.shared.done.fetch_add(frames, Ordering::AcqRel);
        audio.planar.as_ref().clone()
    } else {
        let layout = if channels == 2 {
            score::ChannelLayout::stereo()
        } else {
            score::ChannelLayout::mono(score::STEREO_LABELS[0])
        };
        let mut stages = Vec::with_capacity(job.chain.len());
        for spec in &job.chain {
            let processor = spec.build(audio.sample_rate, channels).map_err(score_fail)?;
            receipt.chain.push(processor.identity());
            stages.push(score::ChainStage {
                processor,
                output: layout.clone(),
            });
        }
        let mut chain = score::Chain::new(layout.clone(), BLOCK, stages).map_err(score_fail)?;
        let resource = DomainId::parse(IMPORT_CLIP_ID).map_err(|_| fail("effect_chain", "resource id"))?;
        let engine = score::Engine::new(resource.clone(), 1);
        let ctx = score::RequestContext {
            resource,
            actor: Some(actor.clone()),
            expected_revision: 1,
        };
        let inputs: Vec<&[f32]> = audio.planar.iter().map(Vec::as_slice).collect();
        let request = score::BounceRequest {
            sample_rate_hz: audio.sample_rate,
            layout: &layout,
            start_sample: 0,
            channels: &inputs,
            block_frames: BLOCK,
        };
        let mut block = score::OutputBlock::with_capacity(layout.clone(), BLOCK).map_err(score_fail)?;
        let out = RefCell::new(vec![Vec::with_capacity(audio.planar[0].len()); channels]);
        let mut sink = |chunk: score::BounceChunk<'_>| {
            let mut out = out.borrow_mut();
            for (dst, src) in out.iter_mut().zip(chunk.channels) {
                dst.extend_from_slice(src);
            }
            let n = chunk.channels.first().map_or(0, |c| c.len() as u64);
            handle.shared.done.fetch_add(n, Ordering::AcqRel);
            Ok(())
        };
        let bounce = engine
            .bounce(&ctx, &request, &mut chain, &mut block, &mut sink, cancel)
            .map_err(score_fail)?;
        receipt.chain_identity = Some(bounce.chain_identity_hex());
        receipt.bounce_digest = Some(bounce.digest_hex());
        receipt.latency_dropped_frames = bounce.latency_dropped_frames;
        out.into_inner()
    };

    handle.set(BatchState::Encoding);
    let export = write_atomically(&job.output, job.overwrite, |w| {
        let mut src = PlanarSource {
            planes: &processed,
            rate: audio.sample_rate,
            position: 0,
        };
        let src: &mut dyn BlockSource = &mut src;
        let export = export_from(src, BLOCK, job.format, w, Some(&handle.shared.done), cancel)?;
        std::io::Write::flush(w).map_err(|e| fail("export_write", e.to_string()))?;
        // Re-decode the written bytes before publishing them.
        let file = w.get_mut();
        let mut bytes = Vec::new();
        std::io::Seek::rewind(file).map_err(|e| fail("export_write", e.to_string()))?;
        std::io::Read::read_to_end(file, &mut bytes).map_err(|e| fail("export_write", e.to_string()))?;
        let back = reel::decode_audio_file(&mut Cursor::new(&bytes), &reel::Limits::default(), frames.saturating_add(1), cancel)
            .map_err(|e| fail("export_verify", e.to_string()))?;
        let md5_ok = !matches!(job.format, MixFormat::Flac { .. }) || back.checksum_verified == Some(true);
        if back.frames() as u64 != frames || usize::from(back.channels) != channels || !md5_ok {
            return Err(fail(
                "export_verify",
                format!("{} frames x {} ch, md5 {:?}", back.frames(), back.channels, back.checksum_verified),
            ));
        }
        let sha = reel::full_digest(&mut Cursor::new(&bytes), 1 << 20, cancel).map_err(|e| fail("export_verify", e.to_string()))?;
        Ok((export, reel::hex(&sha)))
    })?;
    receipt.export = Some(export.0);
    receipt.output_sha256 = Some(export.1);
    receipt.verified = true;
    Ok(())
}

// ----- effect presets as stored records -------------------------------------------------------

/// `filter_kind` keys of the presets this module applies: score's processor identity tags.
pub const PRESET_GAIN: &str = "gain";
pub const PRESET_EQUALIZER: &str = "equalizer";
pub const PRESET_DYNAMICS: &str = "dynamics";
pub const PRESET_LIMITER: &str = "lookahead_limiter";
/// Most equalizer sections a stored preset may describe (score's `MAX_EQ_SECTIONS`).
const PRESET_MAX_EQ_SECTIONS: usize = score::MAX_EQ_SECTIONS;

const BIQUAD_KINDS: [score::BiquadKind; 5] = [
    score::BiquadKind::Peaking,
    score::BiquadKind::LowShelf,
    score::BiquadKind::HighShelf,
    score::BiquadKind::LowPass,
    score::BiquadKind::HighPass,
];
const MODES: [score::DynamicsMode; 2] = [score::DynamicsMode::Compressor, score::DynamicsMode::Expander];
const DETECTORS: [score::Detector; 2] = [score::Detector::Peak, score::Detector::Rms];

fn index_of<T: PartialEq>(all: &[T], v: &T) -> f64 {
    all.iter().position(|x| x == v).unwrap_or(0) as f64
}

fn param(key: impl Into<String>, value: f64) -> folio::StudioAudioPresetParam {
    folio::StudioAudioPresetParam { key: key.into(), value }
}

/// Element `v` of `all` when `v` is a whole non-negative index.
fn pick<T: Copy>(all: &[T], v: f64) -> Option<T> {
    (v.fract() == 0.0 && v >= 0.0 && v < all.len() as f64).then(|| all.get(v as usize).copied()).flatten()
}

impl EffectSpec {
    /// The stored-record form of this effect (parameters named after score's parameter fields;
    /// enumerations as their declaration index: biquad kind Peaking 0 .. HighPass 4, dynamics mode
    /// Compressor 0 / Expander 1, detector Peak 0 / Rms 1, booleans 0 / 1; the equalizer stores its
    /// section count as `sections` and each section as `section_<i>_kind|frequency_hz|gain_db|q`).
    pub fn to_preset_instance(&self, instance_id: impl Into<String>) -> folio::StudioAudioEffectInstance {
        let (kind, params) = match self {
            Self::Gain { db } => (PRESET_GAIN, vec![param("db", *db)]),
            Self::Equalizer { sections, master_gain_db } => {
                let mut p = vec![param("master_gain_db", *master_gain_db), param("sections", sections.len() as f64)];
                for (i, s) in sections.iter().enumerate() {
                    p.push(param(format!("section_{i}_kind"), index_of(&BIQUAD_KINDS, &s.kind)));
                    p.push(param(format!("section_{i}_frequency_hz"), s.frequency_hz));
                    p.push(param(format!("section_{i}_gain_db"), s.gain_db));
                    p.push(param(format!("section_{i}_q"), s.q));
                }
                (PRESET_EQUALIZER, p)
            }
            Self::Dynamics(d) => (
                PRESET_DYNAMICS,
                vec![
                    param("mode", index_of(&MODES, &d.mode)),
                    param("threshold_db", d.threshold_db),
                    param("ratio", d.ratio),
                    param("knee_db", d.knee_db),
                    param("attack_ms", d.attack_ms),
                    param("release_ms", d.release_ms),
                    param("hold_ms", d.hold_ms),
                    param("range_db", d.range_db),
                    param("makeup_db", d.makeup_db),
                    param("detector", index_of(&DETECTORS, &d.detector)),
                    param("rms_window_ms", d.rms_window_ms),
                    param("link", f64::from(u8::from(d.link))),
                ],
            ),
            Self::Limiter(l) => (
                PRESET_LIMITER,
                vec![
                    param("ceiling_db", l.ceiling_db),
                    param("lookahead_ms", l.lookahead_ms),
                    param("release_ms", l.release_ms),
                    param("true_peak", f64::from(u8::from(l.true_peak))),
                ],
            ),
        };
        folio::StudioAudioEffectInstance {
            instance_id: instance_id.into(),
            filter_kind: kind.into(),
            enabled: true,
            params,
            keyframes: Vec::new(),
        }
    }

    /// The effect a stored instance describes; `None` for a disabled instance. Refusals
    /// (`preset_effect`): an unknown `filter_kind`, a missing or unknown parameter, an enumerated
    /// parameter that is not one of its indices, or keyframes (a batch chain applies static values).
    /// Parameter ranges are checked by score when the chain is built (`effect_chain`).
    pub fn from_preset_instance(e: &folio::StudioAudioEffectInstance) -> Result<Option<Self>, MonitorFail> {
        if !e.enabled {
            return Ok(None);
        }
        let target = |what: &str| format!("{}/{}: {what}", e.instance_id, e.filter_kind);
        if !e.keyframes.is_empty() {
            return Err(fail("preset_effect", target("keyframes are not applied by the batch chain")));
        }
        let used = RefCell::new(std::collections::BTreeSet::new());
        let get = |key: &str| -> Result<f64, MonitorFail> {
            used.borrow_mut().insert(key.to_owned());
            e.params
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.value)
                .ok_or_else(|| fail("preset_effect", target(&format!("missing {key}"))))
        };
        let enumerated = |what: &str, v: Option<()>| v.ok_or_else(|| fail("preset_effect", target(what)));
        let spec = match e.filter_kind.as_str() {
            PRESET_GAIN => Self::Gain { db: get("db")? },
            PRESET_EQUALIZER => {
                let count: Vec<usize> = (0..=PRESET_MAX_EQ_SECTIONS).collect();
                let n = pick(&count, get("sections")?).ok_or_else(|| fail("preset_effect", target("sections")))?;
                let mut sections = Vec::with_capacity(n);
                for i in 0..n {
                    let kind = pick(&BIQUAD_KINDS, get(&format!("section_{i}_kind"))?);
                    enumerated("section kind", kind.map(|_| ()))?;
                    sections.push(score::BiquadSpec {
                        kind: kind.unwrap_or(score::BiquadKind::Peaking),
                        frequency_hz: get(&format!("section_{i}_frequency_hz"))?,
                        gain_db: get(&format!("section_{i}_gain_db"))?,
                        q: get(&format!("section_{i}_q"))?,
                    });
                }
                Self::Equalizer { sections, master_gain_db: get("master_gain_db")? }
            }
            PRESET_DYNAMICS => {
                let mode = pick(&MODES, get("mode")?);
                let detector = pick(&DETECTORS, get("detector")?);
                let link = pick(&[false, true], get("link")?);
                enumerated("mode", mode.map(|_| ()))?;
                enumerated("detector", detector.map(|_| ()))?;
                enumerated("link", link.map(|_| ()))?;
                Self::Dynamics(score::DynamicsParams {
                    mode: mode.unwrap_or(score::DynamicsMode::Compressor),
                    threshold_db: get("threshold_db")?,
                    ratio: get("ratio")?,
                    knee_db: get("knee_db")?,
                    attack_ms: get("attack_ms")?,
                    release_ms: get("release_ms")?,
                    hold_ms: get("hold_ms")?,
                    range_db: get("range_db")?,
                    makeup_db: get("makeup_db")?,
                    detector: detector.unwrap_or(score::Detector::Peak),
                    rms_window_ms: get("rms_window_ms")?,
                    link: link.unwrap_or(false),
                })
            }
            PRESET_LIMITER => {
                let true_peak = pick(&[false, true], get("true_peak")?);
                enumerated("true_peak", true_peak.map(|_| ()))?;
                Self::Limiter(score::LimiterParams {
                    ceiling_db: get("ceiling_db")?,
                    lookahead_ms: get("lookahead_ms")?,
                    release_ms: get("release_ms")?,
                    true_peak: true_peak.unwrap_or(true),
                })
            }
            other => return Err(fail("preset_effect", target(&format!("unknown filter_kind {other}")))),
        };
        let used = used.into_inner();
        if let Some(extra) = e.params.iter().find(|p| !used.contains(&p.key)) {
            return Err(fail("preset_effect", target(&format!("unknown parameter {}", extra.key))));
        }
        Ok(Some(spec))
    }
}

/// A stored preset of `chain`: instances `fx-0`, `fx-1`, ... in order.
pub fn preset_from_chain(preset_id: &str, name: &str, category_path: &[&str], chain: &[EffectSpec]) -> folio::StudioAudioEffectPreset {
    folio::StudioAudioEffectPreset {
        preset_id: preset_id.into(),
        name: name.into(),
        category_path: category_path.iter().map(|s| (*s).to_owned()).collect(),
        effects: chain.iter().enumerate().map(|(i, e)| e.to_preset_instance(format!("fx-{i}"))).collect(),
    }
}

/// The batch chain of a stored preset (disabled instances skipped; see
/// [`EffectSpec::from_preset_instance`]).
pub fn chain_from_preset(preset: &folio::StudioAudioEffectPreset) -> Result<Vec<EffectSpec>, MonitorFail> {
    let mut chain = Vec::with_capacity(preset.effects.len());
    for e in &preset.effects {
        if let Some(spec) = EffectSpec::from_preset_instance(e)? {
            chain.push(spec);
        }
    }
    Ok(chain)
}

/// Writes a validated preset set as JSON (sibling temp file renamed on success; `output_exists`
/// without `overwrite`); `preset_invalid` carries Folio's diagnostic.
pub fn save_preset_set(path: &std::path::Path, set: &folio::StudioAudioEffectPresetSet, overwrite: bool) -> Result<u64, MonitorFail> {
    folio::validate_audio_effect_presets(set, folio::Budget::default(), &CancellationToken::default())
        .map_err(|d| fail("preset_invalid", format!("{d:?}")))?;
    let bytes = serde_json::to_vec_pretty(set).map_err(|e| fail("preset_invalid", e.to_string()))?;
    write_atomically(path, overwrite, |w| {
        std::io::Write::write_all(w, &bytes).map_err(|e| fail("export_write", e.to_string()))?;
        Ok(bytes.len() as u64)
    })
}

/// Reads and validates a stored preset set (`audio_read` for I/O; `preset_invalid` for Folio's
/// refusal of the JSON, size checked from the metadata against `Budget::input_bytes` first).
pub fn load_preset_set(path: &std::path::Path) -> Result<folio::StudioAudioEffectPresetSet, MonitorFail> {
    let budget = folio::Budget::default();
    let len = std::fs::metadata(path).map_err(|e| fail("audio_read", e.to_string()))?.len();
    if len > budget.input_bytes as u64 {
        return Err(fail("preset_invalid", format!("{len} bytes exceeds {}", budget.input_bytes)));
    }
    let bytes = std::fs::read(path).map_err(|e| fail("audio_read", e.to_string()))?;
    folio::parse_audio_effect_presets(&bytes, budget, &CancellationToken::default())
        .map_err(|d| fail("preset_invalid", format!("{d:?}")))
}
