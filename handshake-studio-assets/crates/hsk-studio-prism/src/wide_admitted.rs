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
//! The byte counts are conservative upper bounds of requested owned allocations derived from the ICC tag
//! table before parsing (not allocator bookkeeping or RSS):
//! * parse (moxcms `ColorProfile`): every tag decodes to at most `PARSE_FACTOR` times its stored size (u8
//!   and u16 tables keep their width, text widens at most 2x), plus `PARSE_FIXED` per profile; tags that
//!   share data are counted once per referencing tag;
//! * plan (Prism stages, f64): a stored table value becomes at most one f64 per stored byte (u8 -> 8 bytes),
//!   and a fused pair clones its curves once more, so `PLAN_FACTOR` = 8 per stored byte covers both, plus
//!   `PLAN_FIXED` per profile;
//! * black point compensation may build up to four extra stages while planning: `BPC_STAGES` x the plan
//!   bound, plus the 256-point ramp.
//!
//! Requests are bounded by `MAX_WIDE_PIXELS`; callers wanting smaller resident output (develop: chunks)
//! split their frame into several requests, each charged separately.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::admitted::{actor_bytes, add, reserve, reserve_without_cancellation, supported_target};
use crate::wide::{CHUNK_PIXELS, MAX_WIDE_PIXELS, frame, verify};
use crate::{
    Error, ProfileInput, ProviderAdmission, ProviderReservation, WideCacheStats, WideEngine, WideError, WideFailure,
    WideProfileDescriptor, WideSource, WideTransformRequest, WideTransformResult,
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
/// delivered), or the wide engine's own failure with its delivery outcome.
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
/// A refusal found before the engine ran (framing, identity, hash, cache capacity): nothing delivered.
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
        let miss = !self.engine.is_cached(r);
        let (build, plan) = if miss {
            if self.engine.cache_is_full() {
                return Err(wide_refusal(WideError::CacheFull));
            }
            let mut stored = stored_tag_bytes(r.destination)?;
            if let WideSource::Profile(p) = r.source {
                stored = add(stored, stored_tag_bytes(p)?)?;
            }
            let plan = add(mul(stored, PLAN_FACTOR)?, mul(PLAN_FIXED, 2)?)?;
            let build = add(
                add(mul(stored, PARSE_FACTOR)?, mul(PARSE_FIXED, 2)?)?,
                if r.black_point_compensation { add(mul(plan, BPC_STAGES)?, BPC_RAMP_BYTES)? } else { 0 },
            )?;
            (Some(build), Some(plan))
        } else {
            (None, None)
        };
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
