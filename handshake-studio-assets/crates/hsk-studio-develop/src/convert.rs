//! Working -> output profile conversion (STU-CON-044). Both profiles are inspected through Prism's
//! ADMITTED route first (verified linear-light descriptors under the caller's admission ledger),
//! then pixels convert through Prism's [`AdmittedWideEngine`] in requests of at most
//! [`WIDE_CHUNK_PIXELS`] with relative-colorimetric / BPC off. The engine caches its transform plan
//! per (profile hashes, intent, BPC), so a frame parses each profile once instead of once per call.
//! Prism's range refusal outside 0..1 surfaces as a typed error and is never clipped; the output is
//! not clamped and out-of-gamut destination values are counted in a clip report. There is no
//! fallback to Prism's legacy unbudgeted route or to the unaccounted `WideEngine`.
//!
//! Accounting (STU-CON-044 "accounted lifetime"): the admitted wide engine charges the caller's
//! ledger BEFORE every allocation it causes (provider lease at construction, plan and transient
//! build leases on a cache miss, request and output leases per request) and a denial surfaces as
//! `DevelopError::Prism("admission_denied")` before any parse or conversion work of that request
//! (`tests/develop_profile.rs`: `short_admission_budget_is_refused_before_any_work`). The 64-pixel
//! chunk size of the spec text is still not used (requests are up to [`WIDE_CHUNK_PIXELS`] pixels).
use crate::DevelopError;
use crate::recipe::ProfileBinding;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::SinkPort;
use hsk_studio_prism::{
    AdmittedPrism, AdmittedWideEngine, AdmittedWideError, ClipReport, Intent, MAX_WIDE_PIXELS,
    ProfileInput, ProviderAdmission, Transfer, WideCacheStats, WideError, WideSource,
    WideTransformRequest,
};

/// Default pixels per wide Prism request: 3 MiB of f32 RGB in and 3 MiB out. Hard limit:
/// `hsk_studio_prism::MAX_WIDE_PIXELS` (2,097,152). A develop tile is converted in one request
/// unless it is larger than this.
pub const WIDE_CHUNK_PIXELS: usize = 1 << 18;

/// What a converter did since it was created: plan-cache use (the wide path parses each profile
/// once per cached plan) and the destination clip report summed over its requests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ConversionStats {
    /// Requests served by a cached plan (profile identity and hash re-proved, nothing parsed).
    pub plan_hits: u64,
    /// Requests that built a plan.
    pub plan_misses: u64,
    /// Profile parses performed (a hit parses nothing).
    pub plan_parses: u64,
    /// Destination values outside 0..1 that a display clamp would clip. The converted pixels
    /// themselves are never clamped; `ClipMethod::NotComputed` means no request ran.
    pub clip: ClipReport,
}

/// Port used by [`crate::develop_with`]; implemented by [`PrismConverter`].
pub trait ProfileConverter {
    /// Identity + hash of the source (working) profile this converter was built for.
    fn source(&self) -> &ProfileBinding;
    /// Identity + hash of the destination (output) profile.
    fn destination(&self) -> &ProfileBinding;
    /// Admitted inspection of both profiles (verified linear-light descriptors).
    fn inspect(&mut self, cancel: &CancellationToken) -> Result<(), DevelopError>;
    /// Convert working-profile linear RGB to output-profile linear RGB.
    fn convert(
        &mut self,
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, DevelopError>;
    /// Transform requests made so far.
    fn calls(&self) -> u32;
    /// Plan-cache use and clip report so far; converters without them report the default.
    fn stats(&self) -> ConversionStats {
        ConversionStats::default()
    }
}

/// Exact ICC bytes of one profile with its recipe binding.
#[derive(Clone, Copy)]
pub struct IccProfile<'a> {
    pub binding: &'a ProfileBinding,
    pub icc: &'a [u8],
}

/// Request-scoped Prism context (revision is a local precondition, equal on both sides). The
/// actor must equal the one the [`AdmittedWideEngine`] was created for.
#[derive(Clone, Copy)]
pub struct PrismContext<'a> {
    pub actor: &'a ActorContext,
    pub resource_id: &'a DomainId,
    pub correlation_id: u64,
    pub revision: u64,
}

pub struct PrismConverter<'a, A: ProviderAdmission> {
    /// Admitted inspection only (linear-light proof, caller-owned ledger).
    prism: &'a AdmittedPrism<A>,
    /// The transform: plan cache plus two-leg conversion, every allocation charged to the caller's
    /// ledger first; reuse one engine across frames.
    wide: &'a mut AdmittedWideEngine<A>,
    source: IccProfile<'a>,
    destination: IccProfile<'a>,
    context: PrismContext<'a>,
    /// Each Prism request emits one terminal observation here: the sink must accept many.
    sink: &'a mut dyn SinkPort,
    chunk_pixels: usize,
    calls: u32,
    cache_at_start: WideCacheStats,
    clip: ClipReport,
}

impl<'a, A: ProviderAdmission> PrismConverter<'a, A> {
    pub fn new(
        prism: &'a AdmittedPrism<A>,
        wide: &'a mut AdmittedWideEngine<A>,
        source: IccProfile<'a>,
        destination: IccProfile<'a>,
        context: PrismContext<'a>,
        sink: &'a mut dyn SinkPort,
    ) -> Self {
        let cache_at_start = wide.cache_stats();
        Self {
            prism,
            wide,
            source,
            destination,
            context,
            sink,
            chunk_pixels: WIDE_CHUNK_PIXELS,
            calls: 0,
            cache_at_start,
            clip: ClipReport::default(),
        }
    }

    /// Cap pixels per wide request (1..=`MAX_WIDE_PIXELS`); smaller caps bound memory further.
    pub fn with_chunk_pixels(mut self, pixels: usize) -> Result<Self, DevelopError> {
        if pixels == 0 || pixels > MAX_WIDE_PIXELS {
            return Err(DevelopError::InvalidLimits);
        }
        self.chunk_pixels = pixels;
        Ok(self)
    }
}

fn input<'a>(profile: &IccProfile<'a>) -> ProfileInput<'a> {
    ProfileInput {
        profile_id: &profile.binding.profile_id,
        bytes: profile.icc,
        expected_sha256: profile.binding.sha256,
    }
}

fn map(error: hsk_studio_prism::Error) -> DevelopError {
    match error {
        hsk_studio_prism::Error::Canceled => DevelopError::Canceled,
        other => DevelopError::Prism(other.code()),
    }
}

fn map_wide(error: WideError) -> DevelopError {
    match error {
        WideError::Canceled => DevelopError::Canceled,
        other => DevelopError::Prism(other.code()),
    }
}

/// Admission denials keep Prism's `admission_denied` code (outcome: unavailable); cancellation
/// stays `Canceled` whichever layer saw it.
fn map_admitted(error: AdmittedWideError) -> DevelopError {
    match error {
        AdmittedWideError::Admission(error) => map(error),
        AdmittedWideError::Refused(error) => map_wide(error),
        AdmittedWideError::Wide(failure) => map_wide(failure.error),
    }
}

/// Fold one request's clip report into the running total.
fn merge_clip(total: &mut ClipReport, part: &ClipReport) {
    total.method = part.method;
    total.pixels_clipped = total.pixels_clipped.saturating_add(part.pixels_clipped);
    for (sum, add) in total.channels_clipped.iter_mut().zip(part.channels_clipped) {
        *sum = sum.saturating_add(add);
    }
    total.max_overshoot = total.max_overshoot.max(part.max_overshoot);
}

impl<A: ProviderAdmission> ProfileConverter for PrismConverter<'_, A> {
    fn source(&self) -> &ProfileBinding {
        self.source.binding
    }

    fn destination(&self) -> &ProfileBinding {
        self.destination.binding
    }

    fn inspect(&mut self, cancel: &CancellationToken) -> Result<(), DevelopError> {
        for profile in [&self.source, &self.destination] {
            let request = input(profile);
            let admitted = self.prism.inspect_profile(request, cancel).map_err(map)?;
            let descriptor = admitted.descriptor();
            if descriptor.transfer() != Transfer::LinearLight || !descriptor.matches(request) {
                return Err(DevelopError::InvalidProfile);
            }
            // The wide path builds its plan from the same bytes: refuse here, before any pixel.
            self.wide
                .inspect_profile(request, cancel)
                .map_err(map_admitted)?;
        }
        Ok(())
    }

    fn convert(
        &mut self,
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, DevelopError> {
        let mut out = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks(self.chunk_pixels) {
            cancel.check()?;
            let request = WideTransformRequest {
                source: WideSource::Profile(input(&self.source)),
                destination: input(&self.destination),
                pixels: chunk.as_flattened(),
                intent: Intent::RelativeColorimetric,
                black_point_compensation: false,
                revision: self.context.revision,
                expected_revision: self.context.revision,
                correlation_id: self.context.correlation_id,
                resource_id: self.context.resource_id,
                actor: self.context.actor,
            };
            // The output lease lives with `admitted` until the pixels are copied out below.
            let admitted = self
                .wide
                .transform(&request, cancel, &mut *self.sink)
                .map_err(map_admitted)?;
            self.calls += 1;
            let result = admitted.result();
            merge_clip(&mut self.clip, &result.receipt.clip);
            out.extend(result.pixels.chunks_exact(3).map(|p| [p[0], p[1], p[2]]));
        }
        Ok(out)
    }

    fn calls(&self) -> u32 {
        self.calls
    }

    fn stats(&self) -> ConversionStats {
        let now = self.wide.cache_stats();
        ConversionStats {
            plan_hits: now.hits.saturating_sub(self.cache_at_start.hits),
            plan_misses: now.misses.saturating_sub(self.cache_at_start.misses),
            plan_parses: now.parses.saturating_sub(self.cache_at_start.parses),
            clip: self.clip,
        }
    }
}
