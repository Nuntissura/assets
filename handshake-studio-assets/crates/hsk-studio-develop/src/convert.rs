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
//! neither allocates nor charges (Prism: `tests/wide_chunks.rs`
//! `chunk_accounting_is_bounded_deterministic_and_allocation_free` counts no heap allocation inside
//! a band, `tests/wide_job.rs` `admitted_job_charges_everything_before_the_bands_for_every_thread_count`
//! shows no charge during bands). A denial surfaces as `DevelopError::Prism("admission_denied")`
//! before any parse or conversion work (`tests/develop_profile.rs`:
//! `short_admission_budget_is_refused_before_any_work`).
//!
//! Chunks (STU-CON-044 "bounded chunks of at most 64 pixels"; the reading here is provisional: the
//! Operator may require the literal one or enrich the spec). The 64-pixel chunking is Prism's
//! (test names below checked against Prism 3312f72): every band passed to `convert_band_in_place`
//! runs inside Prism in chunks of at most `hsk_studio_prism::ADMITTED_CHUNK_PIXELS` (64) pixels,
//! each chunk starting with a cancellation check (in Prism's code it ends with a finite-output
//! check and a digest update), and Prism records them (`WideBandReport::chunks`, merged over the
//! job in `WideReceipt::chunks`). Prism's `tests/wide_chunks.rs`
//! `chunk_accounting_is_bounded_deterministic_and_allocation_free` asserts the chunk account (chunk
//! counts and the 64-pixel bound per band and per job) with bands of 1 to 2049 pixels, output bits
//! equal to one whole-frame request however the pixels are cut into bands, and no allocation inside
//! a band (the in-place entry point included for the RGB case). Cancellation is Prism's too:
//! `a_canceled_token_stops_a_band_before_its_first_chunk` (tests/wide_chunks.rs, both entry
//! points) and, in src/wide_job/tests.rs, `a_band_canceled_at_a_chunk_edge_leaves_exactly_the_chunks_before_it`
//! and `an_in_place_band_canceled_mid_band_is_partly_overwritten` (a token tripped at an exact chunk
//! edge leaves a whole number of converted chunks; an in-place band is converted prefix plus
//! untouched rest).
//!
//! A develop band is NOT 64 pixels: it is one whole develop tile, split only above
//! [`BandLimits::band_pixels`] (default 262144), and develop makes one band call per tile, not one
//! admitted call per 64 pixels (the literal reading, not adopted; Prism prices it in its ignored
//! bench `bench_literal_64_pixel_admitted_calls_against_internal_chunks`). The ledger is charged
//! once at prepare, not per chunk.
//!
//! What develop itself sees of the chunks: nothing. `convert_band` drops the band report and
//! `finish` reads only the merged clip report, so `DevelopReceipt` carries no chunk count. Develop's
//! tests run bands through the chunked code (in
//! `job_conversion_equals_the_serial_wide_transform_for_every_thread_count` a 32 x 32 tile is a
//! 1024-pixel band = 16 Prism chunks; the serial `AdmittedWideEngine::transform` oracle is chunked
//! too, so that test shows the two routes agree, not a chunk count) and an already cancelled token
//! stops a real Prism band (`job_contract_violations_keep_their_prism_codes`). No develop test
//! cancels between the chunks of a real Prism band: `cancellation_returns_canceled_and_no_partial_output`
//! drives a stand-in converter. A band canceled mid-band leaves develop's per-tile scratch partly
//! converted; the run then ends `Canceled` and delivers no tile (code path, not covered by a
//! develop test with a real Prism band).
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
/// `hsk_studio_prism::MAX_WIDE_PIXELS` (2,097,152). Despite the name this is a develop BAND, not
/// the STU-CON-044 chunk: Prism cuts every band into chunks of at most
/// `hsk_studio_prism::ADMITTED_CHUNK_PIXELS` (64) pixels itself (module docs above).
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
