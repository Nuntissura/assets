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
