//! Admission-ledger route for the wide path (SMB-PRISM-ENG-6 item D13 option b; STU-CON-044 "accounted
//! lifetime" for develop's working -> output conversion). Strictly additive: `WideEngine` is unchanged and
//! this route wraps one, charging the caller's `ProviderAdmission` BEFORE every allocation it causes and
//! holding each reservation exactly as long as the allocation it covers:
//!
//! * engine construction: actor strings, the plan-cache vector and the lease vector (`provider_reserved_bytes`);
//! * plan build (cache miss), charged before any profile is parsed: a transient lease for parsing and
//!   building (released when the call returns) and a plan lease kept with the cached plan until
//!   `try_clear_cache`/`try_retire`;
//! * every request: a transient lease for the per-chunk digest buffer, the Observe frame and the request
//!   strings, and the output lease, which travels with the returned `AdmittedWideTransform`.
//!
//! What is charged, exactly. The byte counts are upper bounds of REQUESTED owned allocations, derived from the ICC
//! tag table before parsing; the unit is the allocator request size (the unit tests/wide_heap.rs measures with a
//! counting global allocator), not allocator bookkeeping, stack frames or RSS. "Stored bytes" of a profile are its
//! length plus the sum of its tag sizes (a tag that shares data with another counts once per referencing tag):
//! * provider lease: exact, not a bound (actor strings, the plan-cache vector, the lease vector);
//! * parse (moxcms `ColorProfile`): every tag decodes to at most `PARSE_FACTOR` times its stored size (u8
//!   and u16 tables keep their width, text widens at most 2x), plus `PARSE_FIXED` per profile;
//! * plan (Prism stages, f64): a stored table value becomes at most one f64 per stored byte (u8 -> 8 bytes),
//!   and a fused pair clones its curves once more, so `PLAN_FACTOR` = 8 per stored byte covers both, plus
//!   `PLAN_FIXED` per profile;
//! * black point compensation may build up to four extra stages while planning: `BPC_STAGES` x the plan
//!   bound, plus the 256-point ramp;
//! * request: the chunk digest staging buffer, the Observe frame bound and the request strings; output: the
//!   output vector (count x channels x 4) and the receipt's profile-id strings (x4).
//!
//! Measured against those charges (tests/wide_heap.rs: leased_bytes_cover_the_measured_heap_per_lease_kind_for_synthetic_families
//! and leased_bytes_cover_the_measured_heap_for_installed_cmyk_profiles): across 10 synthetic profile families
//! (matrix/TRC pair, v2 `curv` gamma, 65 536-entry `curv` tables, 16-bit `mAB `, 16-bit and 8-bit CMYK CLUT tables
//! as source and destination, BPC on, perceptual and relative) and 12 installed CMYK conversions (SWOP, FOGRA39,
//! GRACoL as source and destination), the peak heap of every call and the heap that stays alive (output, plan,
//! job receipt) stayed within the matching lease. Measured / leased: the provider lease 1.00 (exact) and the
//! output vector 1.00 (0.84-0.87 at 100 pixels with the receipt strings charged 4x); the plan lease 0.75 for
//! `curv` tables at the parse limit on both sides, 0.0005-0.24 for the other families; the build transient
//! 0.02-0.32 (0.32 is the `mAB ` case without BPC, the BPC cases charge 4 extra plan stages); the request
//! transient 0.29-0.35. The parse, plan and build leases therefore over-reserve; only the provider and output
//! leases are exact, and no lease claims the exact heap of a call. The measured regions exclude the caller's
//! own ledger and sink.
//!
//! Requests are bounded by `MAX_WIDE_PIXELS`; callers wanting smaller resident output split their frame into
//! several requests, each charged separately, or use a job (`prepare_job`) whose bands live in caller buffers.
//! Whatever the request or band size, the conversion runs in chunks of at most `ADMITTED_CHUNK_PIXELS` (64,
//! STU-CON-044) pixels: the per-request digest staging buffer charged below is one such chunk, and the receipt's
//! `chunks` record names the chunks that ran (tests/wide_chunks.rs:
//! chunk_accounting_is_bounded_deterministic_and_allocation_free).
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::admitted::{actor_bytes, add, reserve, reserve_without_cancellation, supported_target};
use crate::wide::{CHUNK_PIXELS, MAX_WIDE_PIXELS, frame, verify};
use crate::{
    Error, Intent, ProfileInput, ProviderAdmission, ProviderReservation, WideBandReport, WideCacheStats, WideEngine,
    WideError, WideFailure, WideJob, WideJobRequest, WideJobResult, WideProfileDescriptor, WideSource,
    WideTransformRequest, WideTransformResult,
};
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_observe::SinkPort;

const PARSE_FACTOR: u64 = 2;
const PARSE_FIXED: u64 = 16 * 1024;
const PLAN_FACTOR: u64 = 8;
const PLAN_FIXED: u64 = 16 * 1024;
const BPC_STAGES: u64 = 4;
/// 256 Lab points, their device values (up to 4 channels) and L* in f64/f32 work vectors.
const BPC_RAMP_BYTES: u64 = 256 * (8 + 8 + 4 * 4 + 3 * 8);

fn mul(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_mul(b).ok_or(Error::InvalidBudget)
}

/// Refusal of the admitted wide route: the ledger or a request precondition (nothing parsed, nothing
/// delivered), a typed wide refusal found from the framing before the engine ran (nothing parsed, nothing
/// delivered; counted with a recording sink in tests/wide_doc_claims.rs
/// admitted_refusals_found_before_the_engine_ran_deliver_nothing), or the wide engine's own failure with its
/// delivery outcome.
#[derive(Debug)]
pub enum AdmittedWideError {
    Admission(Error),
    Refused(WideError),
    Wide(WideFailure),
}
impl AdmittedWideError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Admission(e) => e.code(),
            Self::Refused(e) => e.code(),
            Self::Wide(f) => f.error.code(),
        }
    }
}
impl From<Error> for AdmittedWideError {
    fn from(e: Error) -> Self {
        Self::Admission(e)
    }
}

/// A wide conversion whose output allocation is charged to the caller's ledger for as long as it lives.
pub struct AdmittedWideTransform<R: ProviderReservation> {
    result: WideTransformResult,
    lease: R,
}
impl<R: ProviderReservation> AdmittedWideTransform<R> {
    pub fn result(&self) -> &WideTransformResult {
        &self.result
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.lease.reserved_bytes()
    }
}

/// Stored ICC tag bytes of an identity- and hash-verified profile (framing only, nothing parsed).
fn stored_tag_bytes(input: ProfileInput<'_>) -> Result<u64, AdmittedWideError> {
    verify(input).map_err(wide_refusal)?;
    let tags = frame(input.bytes).map_err(wide_refusal)?;
    tags.iter()
        .try_fold(input.bytes.len() as u64, |acc, t| acc.checked_add(t.size as u64))
        .ok_or(AdmittedWideError::Admission(Error::InvalidBudget))
}
/// A refusal found before the engine ran (framing, identity, hash, cache capacity): nothing delivered
/// (tests/wide_doc_claims.rs admitted_refusals_found_before_the_engine_ran_deliver_nothing).
fn wide_refusal(error: WideError) -> AdmittedWideError {
    AdmittedWideError::Refused(error)
}
fn channels_of(bytes: &[u8]) -> Option<u64> {
    match bytes.get(16..20)? {
        b"GRAY" => Some(1),
        b"RGB " => Some(3),
        b"CMYK" => Some(4),
        _ => None,
    }
}

pub struct AdmittedWideEngine<A: ProviderAdmission> {
    // Field order matters: allocations drop before the leases that account for them.
    engine: WideEngine,
    plan_leases: Vec<A::Reservation>,
    provider_lease: A::Reservation,
    admission: A,
}

impl<A: ProviderAdmission> AdmittedWideEngine<A> {
    pub fn new(actor: &ActorContext, cache_entries: usize, admission: A) -> Result<Self, Error> {
        supported_target()?;
        if cache_entries == 0 || cache_entries > crate::MAX_CACHE_ENTRIES {
            return Err(Error::InvalidBudget);
        }
        let cost = add(
            actor_bytes(actor),
            add(
                WideEngine::entry_bytes(cache_entries) as u64,
                mul(cache_entries as u64, std::mem::size_of::<A::Reservation>() as u64)?,
            )?,
        )?;
        let provider_lease = reserve_without_cancellation(&admission, cost)?;
        let engine = WideEngine::new(actor.clone(), cache_entries).map_err(|_| Error::InvalidBudget)?;
        Ok(Self {
            engine,
            plan_leases: Vec::with_capacity(cache_entries),
            provider_lease,
            admission,
        })
    }
    pub fn cache_stats(&self) -> WideCacheStats {
        self.engine.cache_stats()
    }
    pub fn provider_reserved_bytes(&self) -> u64 {
        self.provider_lease.reserved_bytes()
    }
    /// Bytes held by the leases of the cached plans.
    pub fn plan_reserved_bytes(&self) -> u64 {
        self.plan_leases.iter().map(ProviderReservation::reserved_bytes).sum()
    }
    pub fn matrix_fusion(&self) -> bool {
        self.engine.matrix_fusion()
    }
    /// Toggle the fused matrix plan; a change clears the cache, so every plan lease must be retirable.
    pub fn set_matrix_fusion(&mut self, enabled: bool) -> Result<(), Error> {
        if enabled != self.engine.matrix_fusion() {
            self.try_clear_cache()?;
            self.engine.set_matrix_fusion(enabled);
        }
        Ok(())
    }
    pub fn try_clear_cache(&mut self) -> Result<(), Error> {
        for lease in &self.plan_leases {
            lease.retirement_ready()?;
        }
        self.engine.clear_cache();
        self.plan_leases.clear();
        Ok(())
    }
    /// `None` means retired; `Some` returns the complete engine when a lease cannot retire yet.
    pub fn try_retire(self) -> Option<Self> {
        if self.provider_lease.retirement_ready().is_err()
            || self.plan_leases.iter().any(|l| l.retirement_ready().is_err())
        {
            return Some(self);
        }
        drop(self);
        None
    }

    /// Wide admission of one profile; its parse is charged (transiently) before it runs.
    pub fn inspect_profile(
        &self,
        input: ProfileInput<'_>,
        t: &CancellationToken,
    ) -> Result<WideProfileDescriptor, AdmittedWideError> {
        let stored = stored_tag_bytes(input)?;
        let _parse = reserve(&self.admission, add(mul(stored, PARSE_FACTOR)?, PARSE_FIXED)?, t)?;
        self.engine.inspect_profile(input, t).map_err(wide_refusal)
    }

    /// Transient build bytes and kept plan bytes of a cache miss (`None`, `None` for a hit), from the ICC tag
    /// tables before anything is parsed; a full cache is refused here.
    fn miss_charges(
        &self,
        source: WideSource<'_>,
        destination: ProfileInput<'_>,
        intent: Intent,
        bpc: bool,
    ) -> Result<(Option<u64>, Option<u64>), AdmittedWideError> {
        if self.engine.is_cached_parts(source, destination, intent, bpc) {
            return Ok((None, None));
        }
        if self.engine.cache_is_full() {
            return Err(wide_refusal(WideError::CacheFull));
        }
        let mut stored = stored_tag_bytes(destination)?;
        if let WideSource::Profile(p) = source {
            stored = add(stored, stored_tag_bytes(p)?)?;
        }
        let plan = add(mul(stored, PLAN_FACTOR)?, mul(PLAN_FIXED, 2)?)?;
        let build = add(
            add(mul(stored, PARSE_FACTOR)?, mul(PARSE_FIXED, 2)?)?,
            if bpc { add(mul(plan, BPC_STAGES)?, BPC_RAMP_BYTES)? } else { 0 },
        )?;
        Ok((Some(build), Some(plan)))
    }

    /// `WideEngine::prepare_job` with every allocation the job causes charged first, all before the job exists:
    /// a work lease (band table, request strings, Observe frame; released by `finish` or drop), a receipt lease
    /// (profile ids of the descriptors and the receipt; travels with the finished result) and, on a cache miss,
    /// the plan lease (kept with the cached plan) and a transient build lease. Band conversion allocates nothing
    /// and charges nothing (tests/wide_chunks.rs chunk_accounting_is_bounded_deterministic_and_allocation_free,
    /// admitted_transform_and_job_report_their_chunks), so the ledger trace is the same for every thread count
    /// (tests/wide_job.rs admitted_job_charges_everything_before_the_bands_for_every_thread_count). Refusals before the engine ran
    /// (layout, revision, framing, capacity, plan errors) are `Refused` and deliver nothing; `finish` delivers.
    pub fn prepare_job(
        &mut self,
        r: &WideJobRequest<'_>,
        t: &CancellationToken,
    ) -> Result<AdmittedWideJob<'_, A::Reservation>, AdmittedWideError> {
        t.check().map_err(|_| Error::Canceled)?;
        if r.actor != self.engine.actor() {
            return Err(Error::ContextMismatch.into());
        }
        if r.revision != r.expected_revision {
            return Err(wide_refusal(WideError::RevisionMismatch));
        }
        let (bands, _) = r.bands.layout().map_err(wide_refusal)?;
        let ids = add(
            r.destination.profile_id.as_str().len() as u64,
            match r.source {
                WideSource::Profile(p) => p.profile_id.as_str().len() as u64,
                WideSource::Lab => 0,
            },
        )?;
        let receipt = mul(ids, 4)?;
        let work = add(
            mul(bands as u64, WideJob::slot_bytes() as u64)?,
            add(
                hsk_studio_observe::MAX_FRAME_BYTES as u64,
                add(actor_bytes(self.engine.actor()), r.resource_id.as_str().len() as u64)?,
            )?,
        )?;
        let (build, plan) = self.miss_charges(r.source, r.destination, r.intent, r.black_point_compensation)?;
        let work_lease = reserve(&self.admission, work, t)?;
        let receipt_lease = reserve(&self.admission, receipt, t)?;
        let plan_lease = match plan {
            Some(bytes) => Some(reserve(&self.admission, bytes, t)?),
            None => None,
        };
        let _build_lease = match build {
            Some(bytes) => Some(reserve(&self.admission, bytes, t)?),
            None => None,
        };
        // `WideEngine::prepare_job` has no fallible step after the plan is cached: a refusal caches nothing.
        let job = self.engine.prepare_job(r, t).map_err(wide_refusal)?;
        if !job.cache_hit()
            && let Some(lease) = plan_lease
        {
            self.plan_leases.push(lease);
        }
        Ok(AdmittedWideJob {
            job,
            work_lease,
            receipt_lease,
        })
    }

    /// `WideEngine::transform` with every allocation it causes charged first (see the module docs).
    pub fn transform(
        &mut self,
        r: &WideTransformRequest<'_>,
        t: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<AdmittedWideTransform<A::Reservation>, AdmittedWideError> {
        t.check().map_err(|_| Error::Canceled)?;
        if r.actor != self.engine.actor() {
            return Err(Error::ContextMismatch.into());
        }
        // Pixel bound and output size before anything is charged or parsed.
        let (in_ch, out_ch) = (
            match r.source {
                WideSource::Profile(p) => channels_of(p.bytes).unwrap_or(1),
                WideSource::Lab => 3,
            },
            channels_of(r.destination.bytes).unwrap_or(4),
        );
        let count = (r.pixels.len() as u64).checked_div(in_ch).ok_or(Error::InvalidBudget)?;
        if count > MAX_WIDE_PIXELS as u64 {
            return Err(Error::PixelLimit.into());
        }
        let ids = add(
            r.destination.profile_id.as_str().len() as u64,
            match r.source {
                WideSource::Profile(p) => p.profile_id.as_str().len() as u64,
                WideSource::Lab => 0,
            },
        )?;
        let output = add(mul(mul(count, out_ch)?, 4)?, mul(ids, 4)?)?;
        let request = add(
            mul(mul(CHUNK_PIXELS as u64, out_ch)?, 4)?,
            add(
                hsk_studio_observe::MAX_FRAME_BYTES as u64,
                add(actor_bytes(self.engine.actor()), r.resource_id.as_str().len() as u64)?,
            )?,
        )?;
        let (build, plan) = self.miss_charges(r.source, r.destination, r.intent, r.black_point_compensation)?;
        let _request_lease = reserve(&self.admission, request, t)?;
        let output_lease = reserve(&self.admission, output, t)?;
        let plan_lease = match plan {
            Some(bytes) => Some(reserve(&self.admission, bytes, t)?),
            None => None,
        };
        let _build_lease = match build {
            Some(bytes) => Some(reserve(&self.admission, bytes, t)?),
            None => None,
        };
        let entries_before = self.engine.cache_stats().entries;
        let result = self.engine.transform(r, t, sink);
        // A plan cached by this call keeps its lease whether or not the conversion itself succeeded.
        if self.engine.cache_stats().entries > entries_before
            && let Some(lease) = plan_lease
        {
            self.plan_leases.push(lease);
        }
        let result = result.map_err(AdmittedWideError::Wide)?;
        Ok(AdmittedWideTransform {
            result,
            lease: output_lease,
        })
    }
}

/// A prepared wide job whose allocations are charged to the caller's ledger (see `prepare_job`). `Send + Sync`:
/// share it by reference with worker threads; band calls neither allocate nor charge.
pub struct AdmittedWideJob<'e, R: ProviderReservation> {
    // Field order matters: the job's allocations drop before the leases that account for them.
    job: WideJob<'e>,
    work_lease: R,
    receipt_lease: R,
}
impl<'e, R: ProviderReservation> AdmittedWideJob<'e, R> {
    /// Layout and channel queries (`bands`, `band_pixels`, `input_channels`, ...).
    pub fn job(&self) -> &WideJob<'e> {
        &self.job
    }
    /// Bytes held by the job's own leases (work + receipt; plan leases stay with the engine).
    pub fn reserved_bytes(&self) -> u64 {
        self.work_lease.reserved_bytes().saturating_add(self.receipt_lease.reserved_bytes())
    }
    /// `WideJob::convert_band`.
    pub fn convert_band(
        &self,
        band: usize,
        pixels: &[f32],
        out: &mut [f32],
        t: &CancellationToken,
    ) -> Result<WideBandReport, WideError> {
        self.job.convert_band(band, pixels, out, t)
    }
    /// `WideJob::convert_band_in_place`.
    pub fn convert_band_in_place(
        &self,
        band: usize,
        buffer: &mut [f32],
        t: &CancellationToken,
    ) -> Result<WideBandReport, WideError> {
        self.job.convert_band_in_place(band, buffer, t)
    }
    /// `WideJob::finish`; the work lease is released, the receipt lease travels with the result.
    pub fn finish(
        self,
        t: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<AdmittedWideJobResult<R>, AdmittedWideError> {
        let Self {
            job,
            work_lease,
            receipt_lease,
        } = self;
        let result = job.finish(t, sink);
        drop(work_lease);
        Ok(AdmittedWideJobResult {
            result: result.map_err(AdmittedWideError::Wide)?,
            lease: receipt_lease,
        })
    }
}

/// A finished job whose receipt allocations stay charged for as long as it lives.
pub struct AdmittedWideJobResult<R: ProviderReservation> {
    result: WideJobResult,
    lease: R,
}
impl<R: ProviderReservation> AdmittedWideJobResult<R> {
    pub fn result(&self) -> &WideJobResult {
        &self.result
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.lease.reserved_bytes()
    }
}
