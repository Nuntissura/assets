//! Thread-shared wide conversions (SMB-PRISM-ENG-7): prepare a plan once, convert disjoint pixel bands
//! from any number of threads, finish once.
//!
//! `WideEngine::prepare_job` (and `AdmittedWideEngine::prepare_job`, which charges the caller's ledger first)
//! resolves the plan exactly like `WideEngine::transform` (cache hit or build) and returns a `WideJob`, which is
//! `Send + Sync`. Callers share `&WideJob` with scoped threads; each band is converted at most once with
//! `convert_band` (separate input and output buffers) or `convert_band_in_place`, then `finish` merges the band
//! records in band order and delivers one terminal observation. The job borrows the engine mutably, so the
//! plan cache (and, on the admitted route, every plan lease) cannot change while a job is alive.
//!
//! * Bit identity: every pixel goes through the same `Plan::pixel` evaluation and clip scan as
//!   `WideEngine::transform`, and a band's input checks are the request checks, so a band's output and its digest
//!   equal those of a serial request holding exactly that band's pixels, whatever the thread count or schedule
//!   (tests/wide_job.rs: job_bands_are_bit_identical_to_the_serial_path_for_every_thread_count).
//! * Allocation: band conversion writes only into caller buffers and uses stack scratch (digest staging, one
//!   pixel copy for in-place work); everything the job owns is allocated by `prepare_job`. The admitted route
//!   charges it all before the job exists, so its ledger trace does not depend on the thread count
//!   (tests/wide_job.rs: admitted_job_charges_everything_before_the_bands_for_every_thread_count).
//! * Chunks (STU-CON-044, "bounded chunks of at most 64 pixels ... accounted lifetime"): a band is evaluated in
//!   chunks of at most `ADMITTED_CHUNK_PIXELS` pixels, one after the other, each with its own cancellation
//!   check, finite-output check and digest update. `WideBandReport::chunks` records the band's chunks and the job
//!   receipt's `WideReceipt::chunks` the merge over all bands; both are functions of the band sizes alone. A chunk
//!   allocates nothing and charges nothing: its Prism-owned buffer is the band's stack digest staging array
//!   (`Stage`, `CHUNK_STAGE_BYTES` = 1024 bytes for every destination channel count; `WideChunkAccount::scratch_bytes`
//!   of a band and of the job receipt is `size_of::<Stage>()`, unlike a request, whose heap buffer is 64 x
//!   destination channels x 4 bytes; an in-place band also copies one 16-byte pixel on the stack, outside that
//!   record), and the ledger was charged in full at prepare (tests/wide_chunks.rs:
//!   chunk_accounting_is_bounded_deterministic_and_allocation_free; src/wide_job/tests.rs:
//!   the_job_chunk_account_reports_the_stage_a_band_really_holds). Output bits and digests do not depend on the
//!   chunk size (the digest is one SHA-256 stream over the band's output).
//! * Failures: a band's outcome depends only on the plan and its pixels; `finish` reports the lowest band that
//!   failed or was never converted (tests/wide_job.rs: band_contract_is_typed_and_finish_reports_the_lowest_failed_band).
//! * Digests: each band reports SHA-256 of its output f32 bits (the serial request digest of that band); the job
//!   receipt carries SHA-256 over the band digests in band order (`JOB_DIGEST_RULE`), independent of thread count.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::wide::{
    CHUNK_PIXELS, CHUNK_STAGE_BYTES, ClipMethod, ClipReport, ForwardSink, MAX_WIDE_PIXELS, Plan, SourceSpace,
    WIDE_ENGINE_NAME, WIDE_ENGINE_VERSION, WideChunkAccount, WideEngine, WideError, WideFailure,
    WideProfileDescriptor, WideReceipt, WideSource, canceled, canceled_at_chunk, engine_build,
};
use crate::{EvaluationOptions, Intent, ProfileInput};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, Observation, Observe, Outcome, SinkPort};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Most bands one job may declare (the band table is the only job allocation that grows with the layout).
pub const MAX_WIDE_JOB_BANDS: usize = 65_536;
/// How `WideJobReceipt::receipt.output_sha256` is formed for a job.
pub const JOB_DIGEST_RULE: &str = "sha256_over_band_output_sha256_in_band_order";

/// Band layout of a job, in pixels. Every band holds 1..=`MAX_WIDE_PIXELS` pixels; 1..=`MAX_WIDE_JOB_BANDS` bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WideBands<'a> {
    /// `pixels` split into consecutive bands of `band_pixels`; the last band holds the remainder.
    Uniform { pixels: usize, band_pixels: usize },
    /// Explicit pixel count of every band, in band order (e.g. one band per image tile).
    Sizes(&'a [usize]),
}
impl WideBands<'_> {
    /// Band count and total pixels, or the typed refusal: `ChannelCount` for an empty layout or an empty band,
    /// `PixelLimit` for a band above `MAX_WIDE_PIXELS` or more than `MAX_WIDE_JOB_BANDS` bands.
    pub fn layout(&self) -> Result<(usize, u64), WideError> {
        match *self {
            Self::Uniform { pixels, band_pixels } => {
                if pixels == 0 || band_pixels == 0 {
                    return Err(WideError::ChannelCount);
                }
                if band_pixels > MAX_WIDE_PIXELS {
                    return Err(WideError::PixelLimit);
                }
                let bands = pixels.div_ceil(band_pixels);
                if bands > MAX_WIDE_JOB_BANDS {
                    return Err(WideError::PixelLimit);
                }
                Ok((bands, pixels as u64))
            }
            Self::Sizes(sizes) => {
                if sizes.is_empty() {
                    return Err(WideError::ChannelCount);
                }
                if sizes.len() > MAX_WIDE_JOB_BANDS {
                    return Err(WideError::PixelLimit);
                }
                let mut total: u64 = 0;
                for &n in sizes {
                    if n == 0 {
                        return Err(WideError::ChannelCount);
                    }
                    if n > MAX_WIDE_PIXELS {
                        return Err(WideError::PixelLimit);
                    }
                    total = total.checked_add(n as u64).ok_or(WideError::PixelLimit)?;
                }
                Ok((sizes.len(), total))
            }
        }
    }
    /// Pixels of band `i` of a layout that `layout` accepted.
    fn band(&self, i: usize) -> usize {
        match *self {
            Self::Uniform { pixels, band_pixels } => {
                let start = i.saturating_mul(band_pixels);
                pixels.saturating_sub(start).min(band_pixels)
            }
            Self::Sizes(sizes) => sizes.get(i).copied().unwrap_or(0),
        }
    }
}

/// Caller-scoped job request: `WideTransformRequest` with a band layout instead of the pixels, which arrive
/// band by band.
pub struct WideJobRequest<'a> {
    pub source: WideSource<'a>,
    pub destination: ProfileInput<'a>,
    pub bands: WideBands<'a>,
    pub intent: Intent,
    pub black_point_compensation: bool,
    pub revision: u64,
    pub expected_revision: u64,
    pub correlation_id: u64,
    pub resource_id: &'a DomainId,
    pub actor: &'a ActorContext,
}

/// What one band conversion produced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WideBandReport {
    pub band: usize,
    pub pixel_count: usize,
    pub clip: ClipReport,
    /// Chunks this band ran (STU-CON-044): at most `ADMITTED_CHUNK_PIXELS` pixels each, a function of the band
    /// size alone.
    pub chunks: WideChunkAccount,
    /// SHA-256 of this band's output as little-endian f32 bits: the `output_sha256` of a serial
    /// `WideEngine::transform` request holding exactly this band's pixels.
    pub output_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct WideJobReceipt {
    /// Identity, engine, intent, BPC/absolute and cache fields as for a serial request; `clip` is merged over the
    /// bands in band order (counts summed, largest overshoot), `pixel_count` is the job total (saturating at
    /// `usize::MAX`) and `output_sha256` follows `digest_rule`.
    pub receipt: WideReceipt,
    pub bands: usize,
    pub total_pixels: u64,
    pub digest_rule: &'static str,
}
#[derive(Debug)]
pub struct WideJobResult {
    pub receipt: WideJobReceipt,
    pub delivery: hsk_studio_observe::Receipt,
    pub diagnostic_state: hsk_studio_observe::State,
}

#[derive(Clone, Copy)]
struct BandRecord {
    clip: ClipReport,
    chunks: WideChunkAccount,
    sha256: [u8; 32],
}
struct Slot {
    pixels: usize,
    claimed: AtomicBool,
    done: OnceLock<Result<BandRecord, WideError>>,
}

/// A prepared conversion shared by reference across threads (see the module docs).
pub struct WideJob<'e> {
    engine: &'e mut WideEngine,
    plan: Arc<Plan>,
    source_desc: Option<WideProfileDescriptor>,
    destination_desc: WideProfileDescriptor,
    source_id: Option<DomainId>,
    destination_id: DomainId,
    resource_id: DomainId,
    cache_hit: bool,
    lab: bool,
    in_ch: usize,
    out_ch: usize,
    clip_method: ClipMethod,
    clip_channels: usize,
    intent: Intent,
    revision: u64,
    correlation_id: u64,
    total_pixels: u64,
    slots: Vec<Slot>,
}

/// `WideJob` is shared by reference with worker threads.
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<WideJob<'static>>();
};

/// Merge one band's clip report (counts are sums, the overshoot a maximum: order-independent).
fn merge_clip(total: &mut ClipReport, part: &ClipReport) {
    total.pixels_clipped = total.pixels_clipped.saturating_add(part.pixels_clipped);
    for (sum, add) in total.channels_clipped.iter_mut().zip(part.channels_clipped) {
        *sum = sum.saturating_add(add);
    }
    total.max_overshoot = total.max_overshoot.max(part.max_overshoot);
}

/// The band's digest staging buffer: a stack array of `CHUNK_STAGE_BYTES` (one chunk of CMYK values, 1024 bytes),
/// the same size for every destination channel count.
type Stage = [u8; CHUNK_STAGE_BYTES];
/// The chunk account of a job band and of the job receipt: its `scratch_bytes` is the size of the stage a band
/// really holds (`size_of::<Stage>()`), not the 64 x channels x 4 formula of a request.
fn band_account() -> WideChunkAccount {
    WideChunkAccount::with_scratch(std::mem::size_of::<Stage>())
}
/// Refuse non-finite output, then feed the chunk's f32 bits (little-endian) to the digest through the band's stack
/// staging buffer (`Stage`).
fn seal_chunk(out: &[f32], stage: &mut Stage, hasher: &mut Sha256) -> Result<(), WideError> {
    if out.iter().any(|v| !v.is_finite()) {
        return Err(WideError::NonfiniteOutput);
    }
    for group in out.chunks(CHUNK_STAGE_BYTES / 4) {
        let mut used = 0usize;
        for (dst, v) in stage.chunks_exact_mut(4).zip(group) {
            dst.copy_from_slice(&v.to_le_bytes());
            used = used.saturating_add(4);
        }
        hasher.update(&stage[..used]);
    }
    Ok(())
}

impl WideEngine {
    /// Resolve the plan for a job (cache hit or build, as `transform`) and return the shareable job. Nothing is
    /// delivered to a sink here; `WideJob::finish` delivers the job's single terminal observation. Every
    /// fallible step runs before the plan is resolved, so a refusal never leaves a newly cached plan behind
    /// unless the plan itself was built (the admitted route relies on this).
    pub fn prepare_job(&mut self, r: &WideJobRequest<'_>, cancel: &CancellationToken) -> Result<WideJob<'_>, WideError> {
        let prepared = self.prepare_parts(r, cancel);
        if prepared.is_err() {
            self.note_rejected();
        }
        let (plan, source_desc, destination_desc, cache_hit, (bands, total_pixels)) = prepared?;
        let in_ch = source_desc.as_ref().map_or(3, |d| d.device.channels());
        let out_ch = destination_desc.device.channels();
        let clip_method = plan.clip_method();
        let slots = (0..bands)
            .map(|i| Slot {
                pixels: r.bands.band(i),
                claimed: AtomicBool::new(false),
                done: OnceLock::new(),
            })
            .collect();
        Ok(WideJob {
            lab: source_desc.is_none(),
            source_id: match r.source {
                WideSource::Profile(p) => Some(p.profile_id.clone()),
                WideSource::Lab => None,
            },
            destination_id: r.destination.profile_id.clone(),
            resource_id: r.resource_id.clone(),
            engine: self,
            plan,
            source_desc,
            destination_desc,
            cache_hit,
            in_ch,
            out_ch,
            clip_method,
            clip_channels: if clip_method == ClipMethod::LinearDevice { out_ch } else { 0 },
            intent: r.intent,
            revision: r.revision,
            correlation_id: r.correlation_id,
            total_pixels,
            slots,
        })
    }
    #[allow(clippy::type_complexity)]
    fn prepare_parts(
        &mut self,
        r: &WideJobRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<(Arc<Plan>, Option<WideProfileDescriptor>, WideProfileDescriptor, bool, (usize, u64)), WideError> {
        canceled(cancel)?;
        if r.actor != self.actor() {
            return Err(WideError::ContextMismatch);
        }
        if r.revision != r.expected_revision {
            return Err(WideError::RevisionMismatch);
        }
        let layout = r.bands.layout()?;
        let (plan, source_desc, destination_desc, cache_hit) =
            self.plan_parts(r.source, r.destination, r.intent, r.black_point_compensation, cancel)?;
        Ok((plan, source_desc, destination_desc, cache_hit, layout))
    }
}

impl WideJob<'_> {
    /// Bytes of the job's band table entry, for ledger charging.
    pub(crate) const fn slot_bytes() -> usize {
        std::mem::size_of::<Slot>()
    }
    pub fn bands(&self) -> usize {
        self.slots.len()
    }
    /// Pixels of band `band` (`None` outside the layout).
    pub fn band_pixels(&self, band: usize) -> Option<usize> {
        self.slots.get(band).map(|s| s.pixels)
    }
    pub fn total_pixels(&self) -> u64 {
        self.total_pixels
    }
    /// Channels per input pixel (source device channels, 3 for a Lab source).
    pub fn input_channels(&self) -> usize {
        self.in_ch
    }
    /// Channels per output pixel (destination device channels).
    pub fn output_channels(&self) -> usize {
        self.out_ch
    }
    /// Whether the plan came from the engine's cache.
    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }

    fn slot(&self, band: usize) -> Result<&Slot, WideError> {
        self.slots.get(band).ok_or(WideError::BandIndex)
    }
    fn claim(slot: &Slot) -> Result<(), WideError> {
        if slot.claimed.swap(true, Ordering::AcqRel) { Err(WideError::BandClaimed) } else { Ok(()) }
    }
    fn record(band: usize, slot: &Slot, outcome: Result<BandRecord, WideError>) -> Result<WideBandReport, WideError> {
        // The band was claimed exclusively above, so its cell is still empty.
        let _ = slot.done.set(outcome);
        outcome.map(|r| WideBandReport {
            band,
            pixel_count: slot.pixels,
            clip: r.clip,
            chunks: r.chunks,
            output_sha256: r.sha256,
        })
    }
    /// The request input checks of `WideEngine::transform`: finite, device values 0..1, Lab L 0..100 and a/b -128..128.
    fn check_pixels(&self, pixels: &[f32]) -> Result<(), WideError> {
        for (i, v) in pixels.iter().enumerate() {
            if !v.is_finite() {
                return Err(WideError::NonfiniteChannel);
            }
            let range = if !self.lab {
                0.0..=1.0
            } else if i % 3 == 0 {
                0.0..=100.0
            } else {
                -128.0..=128.0
            };
            if !range.contains(v) {
                return Err(WideError::ChannelRange);
            }
        }
        Ok(())
    }
    fn new_clip(&self) -> ClipReport {
        ClipReport {
            method: self.clip_method,
            ..Default::default()
        }
    }

    /// Convert band `band`: `pixels` holds exactly its pixels (input channels each) and `out` receives them
    /// (output channels each). Thread-safe; each band converts at most once (`BandClaimed` afterwards, even
    /// after a failure). Index and length refusals do not claim the band.
    pub fn convert_band(
        &self,
        band: usize,
        pixels: &[f32],
        out: &mut [f32],
        cancel: &CancellationToken,
    ) -> Result<WideBandReport, WideError> {
        let slot = self.slot(band)?;
        if Some(pixels.len()) != slot.pixels.checked_mul(self.in_ch) || Some(out.len()) != slot.pixels.checked_mul(self.out_ch)
        {
            return Err(WideError::BandLength);
        }
        Self::claim(slot)?;
        let outcome = self.check_pixels(pixels).and_then(|()| {
            let mut clip = self.new_clip();
            let mut hasher = Sha256::new();
            let mut stage: Stage = [0u8; CHUNK_STAGE_BYTES];
            let mut chunks = band_account();
            let (chunk_in, chunk_out) = (CHUNK_PIXELS.saturating_mul(self.in_ch), CHUNK_PIXELS.saturating_mul(self.out_ch));
            for (px_chunk, out_chunk) in pixels.chunks(chunk_in).zip(out.chunks_mut(chunk_out)) {
                canceled_at_chunk(cancel, chunks.chunks)?;
                chunks.begin(px_chunk.len().checked_div(self.in_ch).unwrap_or(0));
                for (px, o) in px_chunk.chunks_exact(self.in_ch).zip(out_chunk.chunks_exact_mut(self.out_ch)) {
                    if let Some(lin) = self.plan.pixel(px, self.lab, o) {
                        Plan::scan_clip(lin, self.clip_channels, &mut clip);
                    }
                }
                seal_chunk(out_chunk, &mut stage, &mut hasher)?;
            }
            canceled_at_chunk(cancel, chunks.chunks)?;
            Ok(BandRecord {
                clip,
                chunks,
                sha256: hasher.finalize().into(),
            })
        });
        Self::record(band, slot, outcome)
    }

    /// `convert_band` writing the output over the input (`ChannelCount` unless input and output channel counts
    /// are equal). Each pixel is copied to the stack before it is overwritten: same arithmetic, same bits.
    pub fn convert_band_in_place(
        &self,
        band: usize,
        buffer: &mut [f32],
        cancel: &CancellationToken,
    ) -> Result<WideBandReport, WideError> {
        let slot = self.slot(band)?;
        if self.in_ch != self.out_ch {
            return Err(WideError::ChannelCount);
        }
        if Some(buffer.len()) != slot.pixels.checked_mul(self.in_ch) {
            return Err(WideError::BandLength);
        }
        Self::claim(slot)?;
        let ch = self.in_ch;
        let outcome = self.check_pixels(buffer).and_then(|()| {
            let mut clip = self.new_clip();
            let mut hasher = Sha256::new();
            let mut stage: Stage = [0u8; CHUNK_STAGE_BYTES];
            let mut chunks = band_account();
            for chunk in buffer.chunks_mut(CHUNK_PIXELS.saturating_mul(ch)) {
                canceled_at_chunk(cancel, chunks.chunks)?;
                chunks.begin(chunk.len().checked_div(ch).unwrap_or(0));
                for px in chunk.chunks_exact_mut(ch) {
                    let mut src = [0.0f32; 4];
                    src[..ch].copy_from_slice(px);
                    if let Some(lin) = self.plan.pixel(&src[..ch], self.lab, px) {
                        Plan::scan_clip(lin, self.clip_channels, &mut clip);
                    }
                }
                seal_chunk(chunk, &mut stage, &mut hasher)?;
            }
            canceled_at_chunk(cancel, chunks.chunks)?;
            Ok(BandRecord {
                clip,
                chunks,
                sha256: hasher.finalize().into(),
            })
        });
        Self::record(band, slot, outcome)
    }

    /// Merge the band records in band order and deliver one terminal observation. Failure precedence: the
    /// lowest band whose conversion failed (its error) or that was never converted (`JobIncomplete`), then
    /// cancellation. Numeric results are withheld when delivery fails, as for `transform`.
    pub fn finish(self, cancel: &CancellationToken, sink: &mut dyn SinkPort) -> Result<WideJobResult, WideFailure> {
        let WideJob {
            engine,
            plan,
            source_desc,
            destination_desc,
            source_id,
            destination_id,
            resource_id,
            cache_hit,
            intent,
            revision,
            correlation_id,
            total_pixels,
            slots,
            clip_method,
            ..
        } = self;
        let bands = slots.len();
        let mut clip = ClipReport {
            method: clip_method,
            ..Default::default()
        };
        let mut chunks = band_account();
        let mut digest = Sha256::new();
        let mut verdict: Result<(), WideError> = Ok(());
        for slot in &slots {
            match slot.done.get() {
                Some(Ok(record)) => {
                    merge_clip(&mut clip, &record.clip);
                    chunks.merge(&record.chunks);
                    digest.update(record.sha256);
                }
                Some(Err(error)) => {
                    verdict = Err(*error);
                    break;
                }
                None => {
                    verdict = Err(WideError::JobIncomplete);
                    break;
                }
            }
        }
        drop(slots);
        let verdict = verdict.and_then(|()| canceled(cancel));
        let operation_error = verdict.err();
        let outcome = operation_error.map_or(Outcome::Success, WideError::outcome);
        let mut observe = Observe::new(
            correlation_id,
            revision,
            resource_id,
            engine.actor().clone(),
            Budget::new(0, 0).expect("valid zero progress budget"),
        );
        let delivery = observe.emit(
            Observation {
                correlation_id,
                revision,
                outcome,
                progress: None,
                private_project_text: None,
            },
            cancel,
            &mut ForwardSink(sink),
        );
        let diagnostic_state = observe.state();
        let delivery = match delivery {
            Ok(d) => d,
            Err(e) => {
                engine.note_rejected();
                return Err(WideFailure {
                    error: WideError::DeliveryFailed,
                    operation_error,
                    delivery: Err(e),
                    diagnostic_state,
                });
            }
        };
        if let Some(error) = operation_error {
            engine.note_rejected();
            return Err(WideFailure {
                error,
                operation_error: Some(error),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        if delivery.outcome == Outcome::Canceled {
            return Err(WideFailure {
                error: WideError::Canceled,
                operation_error: Some(WideError::Canceled),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        let receipt = WideReceipt {
            source_profile_id: source_id,
            source_sha256: source_desc.as_ref().map(|d| d.sha256),
            source_kind: source_desc.as_ref().map(|d| d.kind),
            source_space: source_desc
                .as_ref()
                .map_or(SourceSpace::Lab, |d| SourceSpace::Device(d.device)),
            destination_profile_id: destination_id,
            destination_sha256: destination_desc.sha256,
            destination_kind: destination_desc.kind,
            destination_space: destination_desc.device,
            engine_name: WIDE_ENGINE_NAME,
            engine_version: WIDE_ENGINE_VERSION,
            engine_build: engine_build(),
            intent,
            options: EvaluationOptions {
                allow_use_cicp_transfer: false,
                prefer_fixed_point: false,
                allow_extended_range_rgb_xyz: true,
                precision_bits: 32,
                clamped_output: false,
            },
            bpc: plan.bpc,
            absolute: plan.absolute,
            clip,
            cache_hit,
            revision,
            correlation_id,
            pixel_count: usize::try_from(total_pixels).unwrap_or(usize::MAX),
            chunks,
            output_sha256: digest.finalize().into(),
        };
        Ok(WideJobResult {
            receipt: WideJobReceipt {
                receipt,
                bands,
                total_pixels,
                digest_rule: JOB_DIGEST_RULE,
            },
            delivery,
            diagnostic_state,
        })
    }
}

#[cfg(test)]
mod tests;
