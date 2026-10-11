//! Working -> output profile conversion (STU-CON-044). Both profiles are inspected through Prism's
//! ADMITTED route first (verified linear-light descriptors under the caller's admission ledger),
//! then pixels convert through a Prism admitted wide JOB (`AdmittedWideEngine::prepare_job`) with
//! relative-colorimetric / BPC off. A job resolves the transform plan once (each profile is parsed
//! at most once per cached plan), returns a `Send + Sync` handle, and converts disjoint pixel bands
//! in place from any number of threads; develop's scoped tile workers call it between their stage A
//! and stage C. Prism's range refusal outside 0..1 surfaces as a typed error with no tile delivered
//! (`tests/develop_profile.rs`: `profile_transform_admitted`); the output is not clamped and
//! out-of-gamut destination values are counted in a clip report that `finish` merges in band order
//! (`wide_requests_are_chunk_invariant_and_report_plan_cache_and_clip`). The pixels equal the
//! serial per-tile wide transform bit for bit on 1, 2, 4, 8, 16 and 32 threads
//! (`job_conversion_equals_the_serial_wide_transform_for_every_thread_count`). There is no fallback
//! to Prism's legacy unbudgeted route or to the unaccounted `WideEngine`.
//!
//! Accounting (STU-CON-044 "accounted lifetime"): the admitted wide engine charges the caller's
//! ledger in full when the job is prepared, BEFORE any allocation it causes (provider lease at
//! construction; work, receipt, plan and transient build leases at `prepare`) and band conversion
//! neither allocates nor charges. A denial surfaces as `DevelopError::Prism("admission_denied")`
//! before any parse or conversion work (`tests/develop_profile.rs`:
//! `short_admission_budget_is_refused_before_any_work`). The 64-pixel chunk size of the spec text is
//! still not used (a band is one develop tile, split only above [`BandLimits::band_pixels`]).
//!
//! One terminal Prism observation is delivered per finished job (not per tile). A run that ends in
//! an error or a cancellation drops its job without finishing it: no observation, every lease
//! released, the develop error carries the code.
use crate::DevelopError;
use crate::recipe::ProfileBinding;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::SinkPort;
use hsk_studio_prism::{
    AdmittedPrism, AdmittedWideEngine, AdmittedWideError, AdmittedWideJob, ClipReport, Intent,
    MAX_WIDE_JOB_BANDS, MAX_WIDE_PIXELS, ProfileInput, ProviderAdmission, ProviderReservation,
    Transfer, WideBands, WideCacheStats, WideError, WideJobRequest, WideSource,
};

/// Default pixels per band: a develop tile is one band unless it is larger than this. Hard limit:
/// `hsk_studio_prism::MAX_WIDE_PIXELS` (2,097,152).
pub const WIDE_CHUNK_PIXELS: usize = 1 << 18;

/// What a converter did since it was created: plan-cache use (a job resolves its plan once, so a
/// frame is one hit or one miss per job; a miss parses each profile once) and the destination clip
/// report summed over its finished jobs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ConversionStats {
    /// Jobs served by a cached plan (profile identity and hash re-proved, nothing parsed).
    pub plan_hits: u64,
    /// Jobs that built a plan.
    pub plan_misses: u64,
    /// Profile parses performed (a hit parses nothing).
    pub plan_parses: u64,
    /// Destination values outside 0..1 that a display clamp would clip. The converted pixels
    /// themselves are not clamped (`wide_requests_are_chunk_invariant_and_report_plan_cache_and_clip`);
    /// `ClipMethod::NotComputed` means no job finished.
    pub clip: ClipReport,
}

/// How a converter wants a frame cut into bands: the develop pipeline splits a tile above
/// `band_pixels` into several bands and starts a new job when one would exceed `max_bands`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandLimits {
    /// Most pixels in one band (at least 1).
    pub band_pixels: usize,
    /// Most bands in one job (at least 1).
    pub max_bands: usize,
}

impl Default for BandLimits {
    fn default() -> Self {
        Self {
            band_pixels: WIDE_CHUNK_PIXELS,
            max_bands: MAX_WIDE_JOB_BANDS,
        }
    }
}

/// A prepared conversion of one run of bands. Shared by reference across the scoped workers of one
/// develop call, so it is `Sync`: every band is converted at most once, from any thread.
pub trait ConversionJob: Sync {
    /// Convert band `band` in place (working-profile linear RGB in, output-profile linear RGB out).
    /// `rgb` holds exactly the band's declared pixel count.
    fn convert_band(
        &self,
        band: usize,
        rgb: &mut [[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<(), DevelopError>;
    /// Merge the band records in band order and deliver the job's single terminal observation.
    /// Fails when a band was never converted or failed.
    fn finish(self: Box<Self>, cancel: &CancellationToken) -> Result<(), DevelopError>;
}

/// Port used by [`crate::develop_with`]; implemented by [`PrismConverter`].
pub trait ProfileConverter {
    /// Identity + hash of the source (working) profile this converter was built for.
    fn source(&self) -> &ProfileBinding;
    /// Identity + hash of the destination (output) profile.
    fn destination(&self) -> &ProfileBinding;
    /// Admitted inspection of both profiles (verified linear-light descriptors).
    fn inspect(&mut self, cancel: &CancellationToken) -> Result<(), DevelopError>;
    /// Band cut this converter accepts.
    fn band_limits(&self) -> BandLimits {
        BandLimits::default()
    }
    /// Prepare a job whose bands hold `bands[i]` pixels each (every entry at least 1, within
    /// [`Self::band_limits`]). Everything the job needs is allocated and charged here.
    fn prepare<'a>(
        &'a mut self,
        bands: &[usize],
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ConversionJob + 'a>, DevelopError>;
    /// Bands converted by finished jobs so far.
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
    /// The transform: plan cache plus job route, every allocation charged to the caller's ledger
    /// first; reuse one engine across frames.
    wide: &'a mut AdmittedWideEngine<A>,
    source: IccProfile<'a>,
    destination: IccProfile<'a>,
    context: PrismContext<'a>,
    /// Each finished job emits one terminal observation here.
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

    /// Cap pixels per band (1..=`MAX_WIDE_PIXELS`); a tile above the cap becomes several bands.
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

/// Wide refusals keep Prism's stable code, including the job codes `band_index`, `band_claimed`,
/// `band_length` and `job_incomplete` (`tests/develop_profile.rs`:
/// `job_contract_violations_keep_their_prism_codes`); cancellation stays `Canceled`.
fn map_wide(error: WideError) -> DevelopError {
    match error {
        WideError::Canceled => DevelopError::Canceled,
        other => DevelopError::Prism(other.code()),
    }
}

/// A layout the wide job cannot take (a band above `MAX_WIDE_PIXELS` or more than
/// `MAX_WIDE_JOB_BANDS` bands) is Prism's `pixel_limit`.
pub(crate) fn pixel_limit() -> DevelopError {
    map_wide(WideError::PixelLimit)
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

/// Fold one job's merged clip report into the running total.
fn merge_clip(total: &mut ClipReport, part: &ClipReport) {
    total.method = part.method;
    total.pixels_clipped = total.pixels_clipped.saturating_add(part.pixels_clipped);
    for (sum, add) in total.channels_clipped.iter_mut().zip(part.channels_clipped) {
        *sum = sum.saturating_add(add);
    }
    total.max_overshoot = total.max_overshoot.max(part.max_overshoot);
}

/// The Prism job plus the converter state its `finish` updates. `Sync` because the admitted job is
/// (compile-time asserted in Prism), `dyn SinkPort` is, and the two `&mut` fields are only used by
/// `finish`, which owns the box.
struct PrismJob<'a, R: ProviderReservation> {
    job: AdmittedWideJob<'a, R>,
    sink: &'a mut dyn SinkPort,
    clip: &'a mut ClipReport,
    calls: &'a mut u32,
    bands: u32,
}

impl<R: ProviderReservation> ConversionJob for PrismJob<'_, R> {
    fn convert_band(
        &self,
        band: usize,
        rgb: &mut [[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<(), DevelopError> {
        // The band report (clip, digest) is merged by `finish`; nothing to keep per band.
        self.job
            .convert_band_in_place(band, rgb.as_flattened_mut(), cancel)
            .map(|_report| ())
            .map_err(map_wide)
    }

    fn finish(self: Box<Self>, cancel: &CancellationToken) -> Result<(), DevelopError> {
        let PrismJob {
            job,
            sink,
            clip,
            calls,
            bands,
        } = *self;
        let done = job.finish(cancel, sink).map_err(map_admitted)?;
        merge_clip(clip, &done.result().receipt.receipt.clip);
        *calls = calls.saturating_add(bands);
        // `done` drops here: its receipt lease is released with the job.
        Ok(())
    }
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

    fn band_limits(&self) -> BandLimits {
        BandLimits {
            band_pixels: self.chunk_pixels,
            max_bands: MAX_WIDE_JOB_BANDS,
        }
    }

    fn prepare<'a>(
        &'a mut self,
        bands: &[usize],
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ConversionJob + 'a>, DevelopError> {
        let request = WideJobRequest {
            source: WideSource::Profile(input(&self.source)),
            destination: input(&self.destination),
            bands: WideBands::Sizes(bands),
            intent: Intent::RelativeColorimetric,
            black_point_compensation: false,
            revision: self.context.revision,
            expected_revision: self.context.revision,
            correlation_id: self.context.correlation_id,
            resource_id: self.context.resource_id,
            actor: self.context.actor,
        };
        let job = self
            .wide
            .prepare_job(&request, cancel)
            .map_err(map_admitted)?;
        Ok(Box::new(PrismJob {
            job,
            sink: &mut *self.sink,
            clip: &mut self.clip,
            calls: &mut self.calls,
            bands: u32::try_from(bands.len()).unwrap_or(u32::MAX),
        }))
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
