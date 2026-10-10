//! Bounded caller-scoped RGB ICCv4 analytical transform; not persisted profile decoding.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, FailureCode, Observation, Observe, Outcome, SinkPort};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ParametricCurve, ParsingOptions, ProfileClass,
    RenderingIntent, ToneReprCurve, TransformF32Executor, TransformOptions,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod admitted;
pub use admitted::*;

pub const ENGINE_NAME: &str = "moxcms";
pub const ENGINE_VERSION: &str = "0.9.1";
pub const MAX_PROFILE_BYTES: usize = 65_536;
pub const MAX_PIXELS: usize = 4096;
pub const MAX_CACHE_ENTRIES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    ProfileLimit,
    PixelLimit,
    InvalidProfileId,
    HashMismatch,
    MalformedProfile,
    UnsupportedProfile,
    InvalidCurve,
    SingularMatrix,
    UnsupportedIntent,
    UnsupportedDepth,
    UnsupportedBpc,
    NonfiniteChannel,
    ChannelRange,
    RevisionMismatch,
    ContextMismatch,
    CacheFull,
    InvalidBudget,
    Canceled,
    EngineFailure,
    NonfiniteOutput,
    DeliveryFailed,
    AdmissionDenied,
    UnsupportedAdmission,
    RetirementUnavailable,
    DiagnosticDelivery(hsk_studio_observe::Error),
}
impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProfileLimit => "profile_limit",
            Self::PixelLimit => "pixel_limit",
            Self::InvalidProfileId => "invalid_profile_id",
            Self::HashMismatch => "hash_mismatch",
            Self::MalformedProfile => "malformed_profile",
            Self::UnsupportedProfile => "unsupported_profile",
            Self::InvalidCurve => "invalid_curve",
            Self::SingularMatrix => "singular_matrix",
            Self::UnsupportedIntent => "unsupported_intent",
            Self::UnsupportedDepth => "unsupported_depth",
            Self::UnsupportedBpc => "unsupported_bpc",
            Self::NonfiniteChannel => "nonfinite_channel",
            Self::ChannelRange => "channel_range",
            Self::RevisionMismatch => "revision_mismatch",
            Self::ContextMismatch => "context_mismatch",
            Self::CacheFull => "cache_full",
            Self::InvalidBudget => "invalid_budget",
            Self::Canceled => "canceled",
            Self::EngineFailure => "engine_failure",
            Self::NonfiniteOutput => "nonfinite_output",
            Self::DeliveryFailed => "delivery_failed",
            Self::AdmissionDenied => "admission_denied",
            Self::UnsupportedAdmission => "unsupported_admission",
            Self::RetirementUnavailable => "retirement_unavailable",
            Self::DiagnosticDelivery(_) => "diagnostic_delivery",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}

/// Hash exact bytes, not path/name or ICC's MD5 profile ID.
pub fn profile_hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub fn hash_hex(hash: &[u8; 32]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for b in hash {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}
/// Existing SCPF identity plus caller artifact bytes; no CAS/grant authority.
#[derive(Clone, Copy)]
pub struct ProfileInput<'a> {
    pub profile_id: &'a DomainId,
    pub bytes: &'a [u8],
    pub expected_sha256: [u8; 32],
}
/// Validated transfer classification, not a caller label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    LinearLight,
    Encoded,
}
impl Transfer {
    pub const fn code(self) -> &'static str {
        match self {
            Self::LinearLight => "linear_light",
            Self::Encoded => "encoded",
        }
    }
}
/// Only the validated engine inspection port can construct this descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDescriptor {
    profile_id: DomainId,
    sha256: [u8; 32],
    transfer: Transfer,
}
impl ProfileDescriptor {
    pub fn profile_id(&self) -> &DomainId {
        &self.profile_id
    }
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
    pub const fn transfer(&self) -> Transfer {
        self.transfer
    }
    pub const fn engine_name(&self) -> &'static str {
        ENGINE_NAME
    }
    pub const fn engine_version(&self) -> &'static str {
        ENGINE_VERSION
    }
    /// Match both identity and exact profile content before using this classification.
    pub fn matches(&self, input: ProfileInput<'_>) -> bool {
        input.bytes.len() <= MAX_PROFILE_BYTES
            && self.profile_id == *input.profile_id
            && self.sha256 == input.expected_sha256
            && self.sha256 == profile_hash(input.bytes)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    RelativeColorimetric,
    Perceptual,
    Saturation,
    AbsoluteColorimetric,
}
impl Intent {
    pub const fn code(self) -> &'static str {
        match self {
            Self::RelativeColorimetric => "relative_colorimetric",
            Self::Perceptual => "perceptual",
            Self::Saturation => "saturation",
            Self::AbsoluteColorimetric => "absolute_colorimetric",
        }
    }
}
/// Revision is a local precondition. Private labels never enter telemetry.
pub struct TransformRequest<'a> {
    pub source: ProfileInput<'a>,
    pub destination: ProfileInput<'a>,
    pub pixels: &'a [[f32; 3]],
    pub intent: Intent,
    pub bit_depth: u8,
    pub black_point_compensation: bool,
    pub revision: u64,
    pub expected_revision: u64,
    pub correlation_id: u64,
    pub resource_id: &'a DomainId,
    pub actor: &'a ActorContext,
    pub private_project_text: Option<&'a str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationOptions {
    pub allow_use_cicp_transfer: bool,
    pub prefer_fixed_point: bool,
    pub allow_extended_range_rgb_xyz: bool,
    pub precision_bits: u8,
    pub clamped_output: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformReceipt {
    pub source_profile_id: DomainId,
    pub destination_profile_id: DomainId,
    pub source_sha256: [u8; 32],
    pub destination_sha256: [u8; 32],
    pub engine_name: &'static str,
    pub engine_version: &'static str,
    pub icc_version: &'static str,
    pub intent: Intent,
    pub bit_depth: u8,
    pub black_point_compensation: bool,
    pub analytical: bool,
    pub options: EvaluationOptions,
    pub cache_hit: bool,
    pub revision: u64,
    pub correlation_id: u64,
    pub pixel_count: usize,
}
#[derive(Debug)]
pub struct TransformResult {
    pub pixels: Vec<[f32; 3]>,
    pub receipt: TransformReceipt,
    pub delivery: hsk_studio_observe::Receipt,
    pub diagnostic_state: hsk_studio_observe::State,
}
#[derive(Debug)]
pub struct Failure {
    pub error: Error,
    /// Retain compute failure if delivery also fails; withhold numeric output.
    pub operation_error: Option<Error>,
    pub delivery: Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error>,
    pub diagnostic_state: hsk_studio_observe::State,
}
pub trait ColorEngine: Send + Sync {
    /// Pure bounded validation; no transform, cache admission or telemetry publication.
    fn inspect_profile(
        &self,
        input: ProfileInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProfileDescriptor, Error>;
    fn transform(
        &mut self,
        request: &TransformRequest<'_>,
        cancel: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<TransformResult, Failure>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub rejected: u64,
}
struct CacheEntry {
    source: [u8; 32],
    destination: [u8; 32],
    executor: Arc<TransformF32Executor>,
}
/// One cache per caller context. No globals, eviction, disk cache or scheduler.
/// Every entry fixes engine/version/options/precision; hashes select profiles.
pub struct Prism {
    actor: ActorContext,
    capacity: usize,
    cache: Vec<CacheEntry>,
    stats: CacheStats,
}
impl Prism {
    pub fn new(actor: ActorContext, cache_entries: usize) -> Result<Self, Error> {
        if cache_entries == 0 || cache_entries > MAX_CACHE_ENTRIES {
            return Err(Error::InvalidBudget);
        }
        Ok(Self {
            actor,
            capacity: cache_entries,
            cache: Vec::with_capacity(cache_entries),
            stats: CacheStats {
                entries: 0,
                hits: 0,
                misses: 0,
                rejected: 0,
            },
        })
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.stats
    }
    /// Explicit recovery after saturation; profile/request bytes remain unchanged.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.stats.entries = 0;
    }
    fn compute(
        &mut self,
        r: &TransformRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<(Vec<[f32; 3]>, bool), Error> {
        canceled(cancel)?;
        if r.actor != &self.actor {
            return Err(Error::ContextMismatch);
        }
        if r.revision != r.expected_revision {
            return Err(Error::RevisionMismatch);
        }
        if r.intent != Intent::RelativeColorimetric {
            return Err(Error::UnsupportedIntent);
        }
        if r.black_point_compensation {
            return Err(Error::UnsupportedBpc);
        }
        if r.bit_depth != 32 {
            return Err(Error::UnsupportedDepth);
        }
        if r.pixels.is_empty() || r.pixels.len() > MAX_PIXELS {
            return Err(Error::PixelLimit);
        }
        for pixel in r.pixels {
            for channel in pixel {
                if !channel.is_finite() {
                    return Err(Error::NonfiniteChannel);
                }
                if !(0.0..=1.0).contains(channel) {
                    return Err(Error::ChannelRange);
                }
            }
        }
        let source = parse_profile(r.source)?;
        canceled(cancel)?;
        let destination = parse_profile(r.destination)?;
        canceled(cancel)?;
        let hit = self.cache.iter().position(|e| {
            e.source == r.source.expected_sha256 && e.destination == r.destination.expected_sha256
        });
        let executor = if let Some(i) = hit {
            self.stats.hits = self.stats.hits.saturating_add(1);
            self.cache[i].executor.clone()
        } else {
            if self.cache.len() >= self.capacity {
                return Err(Error::CacheFull);
            }
            let options = TransformOptions {
                rendering_intent: RenderingIntent::RelativeColorimetric,
                allow_use_cicp_transfer: false,
                prefer_fixed_point: false,
                allow_extended_range_rgb_xyz: true,
                ..Default::default()
            };
            let executor = source
                .create_transform_f32(Layout::Rgb, &destination, Layout::Rgb, options)
                .map_err(|_| Error::EngineFailure)?;
            canceled(cancel)?;
            self.cache.push(CacheEntry {
                source: r.source.expected_sha256,
                destination: r.destination.expected_sha256,
                executor: executor.clone(),
            });
            self.stats.entries = self.cache.len();
            self.stats.misses = self.stats.misses.saturating_add(1);
            executor
        };
        let mut output = Vec::with_capacity(r.pixels.len());
        for chunk in r.pixels.chunks(64) {
            canceled(cancel)?;
            let flat: Vec<f32> = chunk.iter().flatten().copied().collect();
            let mut transformed = vec![0.0; flat.len()];
            executor
                .transform(&flat, &mut transformed)
                .map_err(|_| Error::EngineFailure)?;
            if transformed.iter().any(|v| !v.is_finite()) {
                return Err(Error::NonfiniteOutput);
            }
            for p in transformed.chunks_exact(3) {
                output.push([p[0], p[1], p[2]]);
            }
        }
        canceled(cancel)?;
        Ok((output, hit.is_some()))
    }
}
fn canceled(token: &CancellationToken) -> Result<(), Error> {
    token.check().map_err(|_| Error::Canceled)
}
fn parse_profile(input: ProfileInput<'_>) -> Result<ColorProfile, Error> {
    if input.profile_id.prefix() != "SCPF" {
        return Err(Error::InvalidProfileId);
    }
    if input.bytes.len() > MAX_PROFILE_BYTES {
        return Err(Error::ProfileLimit);
    }
    if profile_hash(input.bytes) != input.expected_sha256 {
        return Err(Error::HashMismatch);
    }
    // Outer framing only. All ICC tag interpretation belongs to moxcms.
    if input.bytes.len() < 132
        || u32::from_be_bytes(input.bytes[..4].try_into().unwrap()) as usize != input.bytes.len()
    {
        return Err(Error::MalformedProfile);
    }
    // Admission gate for this narrow leaf, not ICC value interpretation. Some engine
    // parsers ignore unsupported tags; their presence must never silently change meaning.
    let count = u32::from_be_bytes(input.bytes[128..132].try_into().unwrap()) as usize;
    let end = count
        .checked_mul(12)
        .and_then(|n| n.checked_add(132))
        .filter(|end| *end <= input.bytes.len())
        .ok_or(Error::MalformedProfile)?;
    let allowed = [
        b"desc", b"cprt", b"wtpt", b"chad", b"rXYZ", b"gXYZ", b"bXYZ", b"rTRC", b"gTRC", b"bTRC",
    ];
    let mut seen = [false; 10];
    for entry in input.bytes[132..end].chunks_exact(12) {
        let Some(index) = allowed.iter().position(|name| entry[..4] == name[..]) else {
            return Err(Error::UnsupportedProfile);
        };
        if seen[index] {
            return Err(Error::MalformedProfile);
        }
        seen[index] = true;
    }
    let p = ColorProfile::new_from_slice_with_options(
        input.bytes,
        ParsingOptions {
            max_profile_size: MAX_PROFILE_BYTES + 1,
            max_allowed_clut_size: 4096,
            max_allowed_trc_size: 16,
        },
    )
    .map_err(|_| Error::MalformedProfile)?;
    if input.bytes[8] != 4
        || p.color_space != DataColorSpace::Rgb
        || p.pcs != DataColorSpace::Xyz
        || p.profile_class != ProfileClass::DisplayDevice
        || p.cicp.is_some()
        || p.lut_a_to_b_perceptual.is_some()
        || p.lut_a_to_b_colorimetric.is_some()
        || p.lut_a_to_b_saturation.is_some()
        || p.lut_b_to_a_perceptual.is_some()
        || p.lut_b_to_a_colorimetric.is_some()
        || p.lut_b_to_a_saturation.is_some()
        || p.gamut.is_some()
    {
        return Err(Error::UnsupportedProfile);
    }
    let (
        Some(ToneReprCurve::Parametric(red)),
        Some(ToneReprCurve::Parametric(green)),
        Some(ToneReprCurve::Parametric(blue)),
    ) = (&p.red_trc, &p.green_trc, &p.blue_trc)
    else {
        return Err(Error::UnsupportedProfile);
    };
    if red != green
        || red != blue
        || ![1, 3, 4, 5, 7].contains(&red.len())
        || red.iter().any(|x| !x.is_finite())
    {
        return Err(Error::InvalidCurve);
    }
    let curve = ParametricCurve::new(red).ok_or(Error::InvalidCurve)?;
    if curve.g <= 0.0 || curve.a <= 0.0 || curve.invert().is_none() {
        return Err(Error::InvalidCurve);
    }
    let matrix = p.colorant_matrix();
    if matrix.v.iter().flatten().any(|x| !x.is_finite())
        || matrix
            .determinant()
            .is_none_or(|d| !d.is_finite() || d.abs() < 1e-10)
    {
        return Err(Error::SingularMatrix);
    }
    let wp = p.white_point;
    if [wp.x, wp.y, wp.z]
        .iter()
        .any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err(Error::UnsupportedProfile);
    }
    Ok(p)
}
struct ForwardSink<'a>(&'a mut dyn SinkPort);
impl SinkPort for ForwardSink<'_> {
    fn try_send(
        &mut self,
        class: hsk_studio_observe::DeliveryClass,
        bytes: &[u8],
    ) -> Result<(), hsk_studio_observe::DeliveryError> {
        self.0.try_send(class, bytes)
    }
}
impl ColorEngine for Prism {
    fn inspect_profile(
        &self,
        input: ProfileInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProfileDescriptor, Error> {
        canceled(cancel)?;
        let profile = parse_profile(input)?;
        canceled(cancel)?;
        let transfer = match &profile.red_trc {
            Some(ToneReprCurve::Parametric(values)) if values.as_slice() == [1.0] => {
                Transfer::LinearLight
            }
            _ => Transfer::Encoded,
        };
        Ok(ProfileDescriptor {
            profile_id: input.profile_id.clone(),
            sha256: profile_hash(input.bytes),
            transfer,
        })
    }

    fn transform(
        &mut self,
        r: &TransformRequest<'_>,
        cancel: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<TransformResult, Failure> {
        let compute = self.compute(r, cancel);
        let operation_error = compute.as_ref().err().copied();
        let outcome = match operation_error {
            None => Outcome::Success,
            Some(Error::Canceled) => Outcome::Canceled,
            Some(
                Error::UnsupportedProfile
                | Error::UnsupportedIntent
                | Error::UnsupportedDepth
                | Error::UnsupportedBpc,
            ) => Outcome::Failure(FailureCode::Unsupported),
            Some(Error::CacheFull | Error::EngineFailure) => {
                Outcome::Failure(FailureCode::Unavailable)
            }
            Some(_) => Outcome::Failure(FailureCode::Validation),
        };
        // Context is bound to this engine, including rejected cross-context requests.
        let mut observe = Observe::new(
            r.correlation_id,
            r.revision,
            r.resource_id.clone(),
            self.actor.clone(),
            Budget::new(0, 0).expect("valid zero progress budget"),
        );
        let delivery = observe.emit(
            Observation {
                correlation_id: r.correlation_id,
                revision: r.revision,
                outcome,
                progress: None,
                private_project_text: None,
            },
            cancel,
            &mut ForwardSink(sink),
        );
        let diagnostic_state = observe.state();
        if delivery.is_err() {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(Failure {
                error: Error::DeliveryFailed,
                operation_error,
                delivery,
                diagnostic_state,
            });
        }
        let delivery = delivery.expect("checked delivery");
        if let Some(error) = operation_error {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(Failure {
                error,
                operation_error: Some(error),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        if delivery.outcome == Outcome::Canceled {
            return Err(Failure {
                error: Error::Canceled,
                operation_error: Some(Error::Canceled),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        let (pixels, cache_hit) = compute.expect("checked compute");
        Ok(TransformResult {
            pixels,
            receipt: TransformReceipt {
                source_profile_id: r.source.profile_id.clone(),
                destination_profile_id: r.destination.profile_id.clone(),
                source_sha256: r.source.expected_sha256,
                destination_sha256: r.destination.expected_sha256,
                engine_name: ENGINE_NAME,
                engine_version: ENGINE_VERSION,
                icc_version: "v4",
                intent: r.intent,
                bit_depth: r.bit_depth,
                black_point_compensation: false,
                analytical: true,
                options: EvaluationOptions {
                    allow_use_cicp_transfer: false,
                    prefer_fixed_point: false,
                    allow_extended_range_rgb_xyz: true,
                    precision_bits: 32,
                    clamped_output: false,
                },
                cache_hit,
                revision: r.revision,
                correlation_id: r.correlation_id,
                pixel_count: r.pixels.len(),
            },
            delivery,
            diagnostic_state,
        })
    }
}
/// Same owned descriptor feeds CLI help, models, manual and Argus adapters.
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PRISM","descriptor_version":1,"command":"prism-consumer","api":"ColorEngine: Send + Sync; inspect_profile returns opaque validated ID/hash/transfer descriptor; immutable TransformRequest and caller SinkPort","admitted_api":"AdmittedPrism<ProviderAdmission> reserves source-proven requested owned allocation layouts before parsing/executor/cache/output; metadata-free type0-linear matrix ICCv4 only; legacy APIs unchanged. Supported x86_64-pc-windows-msvc Rust1.97.1(8bab26f4f), moxcms0.9.1 extended_range; other targets/admission owners rejected. Inline tokens retain caller ledger and guarantee synchronous infallible Drop; unsupported/indeterminate-retirement owners refused before reserve. Explicit try_clear_cache/try_retire precheck every token and retain intact provider on refusal. Cache/provider/result/descriptor lifetime reservations do not double charge shared cached executors; borrowed result()/descriptor() accessors cannot detach allocations from reservation. Unit excludes allocator bookkeeping/RSS; reserve includes requested capacities, strings, Arc control/padding and transient overlapping owners; external sink/admission ledger internals belong to separately admitted caller. Text metadata is rejected before parser, no runtime stripping or legacy fallback. Cost/source derivation in src/admitted.rs; independently scoped allocation observations required; no full ICC/host proof","purpose":"Display-class RGB ICCv4 matrix/analytical matching parametric-TRC relative-colorimetric f32 conversion; only desc,cprt,wtpt,chad,rXYZ,gXYZ,bXYZ,rTRC,gTRC,bTRC tags admitted","inspection":"--inspect --source FILE --source-hash SHA256 --source-id SCPF-UUIDv7; optional --cancel; pure validation, no telemetry/cache; linear_light only matching type0 gamma1, otherwise admitted profile encoded; errors exit2","input":"--source FILE --destination FILE --source-hash SHA256 --destination-hash SHA256 --pixels FILE --depth 32 --intent relative_colorimetric --revision U64 --expected-revision U64; whitespace RGB triples; also required --source-id SCPF-UUIDv7 --destination-id SCPF-UUIDv7 --resource-id DOMAIN-UUIDv7 --correlation U64 --account ID --principal ID --owner-account ID --owner-principal ID --access-space ID --session ID; optional --cancel --reject-sink","output":"Local JSON numeric RGB output, profile IDs/hashes, engine/version/options, revision/correlation/cache-hit and Observe delivery state; exit2 failure; no numeric success after delivery failure","bounds":{"profile_bytes":65536,"pixels":4096,"cache_entries":8,"test_case_seconds":30},"options":{"engine":"moxcms","version":"0.9.1","default_features":false,"features":["extended_range"],"cicp":false,"fixed_point":false,"extended_range_rgb_xyz":true,"bpc":false,"input_channels":"finite0..1","output_channels":"finite unclamped floats"},"reject":"malformed/hash/profile-id/unsupported profile,intent,BPC,depth/nonfinite/range/stale/context/cache-limit/cancel/engine/delivery","recovery":"Correct input/revision; select supported profiles; clear caller cache or create bounded engine; reconcile indeterminate telemetry through owning sink before retry","privacy":"Profile bytes, paths, pixels, labels and private text never enter Observe telemetry; caller owns grants","pending":["full StudioColorProfile wire decoding","render materialization","cross-host bit-identical promotion","LUT","OCIO","softproof","gamut","BPC","host authorization and embedding"],"manual_route":"same descriptor","argus_route":"same descriptor plus receipt/cache_stats/diagnostic_state"}"#;
