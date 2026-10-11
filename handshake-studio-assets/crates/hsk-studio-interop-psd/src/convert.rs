//! Display conversion of embedded ICC profiles through Prism. The importer asks two kinds of
//! thing of a converter. The wide n-channel path (`inspect_wide` / `convert_wide`, `WideEngine`:
//! Gray, RGB and CMYK device profiles of ICC v2/v4 as matrix, `kTRC`, `mAB`, `mft1/mft2`, up to
//! 2 MiB, plus a profile-free Lab PCS source) carries the configured rendering intent and black
//! point compensation to Prism; the importer uses it for every embedded profile when the converter
//! has it. The three-channel legacy path (`inspect` / `convert_rgb`, legacy Prism: ICC v4 matrix
//! profiles up to 64 KiB, relative colorimetric, no black point compensation) serves only
//! converters without a wide path, and the importer then records that the intent and compensation
//! were not applied (`rendering_intent_not_applied_legacy_rgb`,
//! `black_point_compensation_not_applied_legacy_rgb`; test
//! `rgb_intent_and_black_point_compensation_reach_prism_or_are_recorded`). A refusal is a typed
//! loss, not a silent fallback (test `embedded_profiles_go_through_prism_and_refusals_are_typed_losses`);
//! nothing here works around a Prism limit, and no default CMYK profile is invented (test
//! `cmyk_without_a_usable_profile_is_a_typed_unmapped_layer_never_defaulted`).
//!
//! Wide output is the converter's working RGB (linear light) and is UNCLAMPED: out-of-gamut values
//! lie below 0 or above 1. Clamping is the caller's job and is recorded (see `proposal`).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::SinkPort;
use hsk_studio_prism::{
    ClipMethod, ColorEngine, DeviceSpace, Intent, MAX_CACHE_ENTRIES, MAX_PIXELS, MAX_WIDE_PIXELS, Prism, ProfileInput,
    TransformRequest, WideEngine, WideSource, WideTransformRequest, profile_hash,
};

/// Why a converter did not take a profile: the converter's own fixed error code (for Prism, the
/// `hsk_studio_prism::Error::code()` string, for example `profile_limit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IccRefusal(pub &'static str);

/// Refusal code of the default [`IccConverter::inspect_wide`] / [`IccConverter::convert_wide`]: the
/// converter has no n-channel path at all (not a refusal of a particular profile).
pub const WIDE_UNAVAILABLE: &str = "wide_conversion_unavailable";

/// Device colour space of an embedded profile as the converter reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileDevice {
    Gray,
    Rgb,
    Cmyk,
}

impl ProfileDevice {
    pub fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Rgb => "rgb",
            Self::Cmyk => "cmyk",
        }
    }
}

/// What the wide path learned about an embedded profile without converting anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileFacts {
    pub device: ProfileDevice,
    /// Whether the device-to-PCS direction can serve each intent, indexed by the
    /// [`RenderingIntent`] discriminant (absolute colorimetric also needs the media white). A
    /// missing table is not substituted by another intent (test
    /// `cmyk_without_a_usable_profile_is_a_typed_unmapped_layer_never_defaulted`).
    pub intents: [bool; 4],
}

impl ProfileFacts {
    pub fn supports(&self, intent: RenderingIntent) -> bool {
        self.intents[intent as usize]
    }
}

/// Rendering intent with Studio's own encoding (STU-COL-135): the discriminants are the ICC intent
/// numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    Perceptual = 0,
    RelativeColorimetric = 1,
    Saturation = 2,
    AbsoluteColorimetric = 3,
}

impl RenderingIntent {
    pub fn code(self) -> &'static str {
        match self {
            Self::Perceptual => "perceptual",
            Self::RelativeColorimetric => "relative_colorimetric",
            Self::Saturation => "saturation",
            Self::AbsoluteColorimetric => "absolute_colorimetric",
        }
    }

    pub fn icc_number(self) -> u8 {
        self as u8
    }

    fn prism(self) -> Intent {
        match self {
            Self::Perceptual => Intent::Perceptual,
            Self::RelativeColorimetric => Intent::RelativeColorimetric,
            Self::Saturation => Intent::Saturation,
            Self::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
        }
    }
}

/// Where the pixels of a wide conversion come from.
#[derive(Clone, Copy, Debug)]
pub enum WideConvertSource<'a> {
    /// Embedded device profile bytes (Gray, RGB or CMYK); `pixels` carry that many channels.
    Profile(&'a [u8]),
    /// ICC PCS Lab (D50) triples: `L` 0..100, `a` and `b` -128..128; no profile involved.
    Lab,
}

/// One n-channel conversion into the converter's working RGB.
#[derive(Clone, Copy, Debug)]
pub struct WideConvertRequest<'a> {
    pub source: WideConvertSource<'a>,
    /// Interleaved `f32`: device values `0.0..=1.0` (CMYK: `1.0` is full ink, the ICC convention)
    /// or Lab triples.
    pub pixels: &'a [f32],
    pub intent: RenderingIntent,
    pub black_point_compensation: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipMethodKind {
    /// The converter did not compute a clip report.
    #[default]
    NotComputed,
    /// Exact: linear destination values checked against `0..=1` before any clamping.
    LinearDevice,
    /// The destination is table-based; its output cannot reveal clipping.
    Unavailable,
}

impl ClipMethodKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotComputed => "not_computed",
            Self::LinearDevice => "linear_device",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The converter's own clip report (Prism `ClipReport`), summed over all calls of an import. It
/// counts every converted pixel, including fully transparent ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipSummary {
    pub method: ClipMethodKind,
    pub pixels_clipped: u64,
    pub channels_clipped: [u64; 4],
    pub max_overshoot: f64,
}

impl ClipSummary {
    pub(crate) fn absorb(&mut self, other: &ClipSummary, first: bool) {
        if first {
            self.method = other.method;
        }
        self.pixels_clipped = self.pixels_clipped.saturating_add(other.pixels_clipped);
        for (a, b) in self.channels_clipped.iter_mut().zip(other.channels_clipped) {
            *a = a.saturating_add(b);
        }
        self.max_overshoot = self.max_overshoot.max(other.max_overshoot);
    }
}

/// Result of [`IccConverter::convert_wide`]: working-RGB (linear light) triples, one per input
/// pixel, NOT clamped.
#[derive(Clone, Debug, PartialEq)]
pub struct WideConverted {
    pub pixels: Vec<[f32; 3]>,
    pub clip: ClipSummary,
}

pub trait IccConverter {
    /// `SCPF-...` of the working space the converted values are expressed in (linear light).
    fn destination_profile_id(&self) -> String;
    /// Validates `icc` without converting anything.
    fn inspect(&mut self, icc: &[u8], cancel: &CancellationToken) -> Result<(), IccRefusal>;
    /// Converts encoded RGB triples in `0.0..=1.0` from `icc` to the destination working space.
    fn convert_rgb(
        &mut self,
        icc: &[u8],
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, IccRefusal>;

    /// Whether this converter has an n-channel path (CMYK, Lab, gray profiles). Default: no.
    fn wide_available(&self) -> bool {
        false
    }
    /// Validates a Gray, RGB or CMYK profile on the n-channel path and reports its device space.
    fn inspect_wide(&mut self, _icc: &[u8], _cancel: &CancellationToken) -> Result<ProfileFacts, IccRefusal> {
        Err(IccRefusal(WIDE_UNAVAILABLE))
    }
    /// Converts n-channel device pixels or Lab to the destination working RGB, unclamped.
    fn convert_wide(
        &mut self,
        _request: &WideConvertRequest<'_>,
        _cancel: &CancellationToken,
    ) -> Result<WideConverted, IccRefusal> {
        Err(IccRefusal(WIDE_UNAVAILABLE))
    }
}

/// Host-supplied identities and destination for [`PrismConverter`].
pub struct PrismRoute {
    pub actor: ActorContext,
    /// `SCPF-...` the host assigns to the document's embedded profile.
    pub source_profile_id: DomainId,
    /// `SCPF-...` of the destination profile.
    pub destination_profile_id: DomainId,
    /// Destination profile bytes: a linear-light RGB profile Prism admits (the wide path also
    /// admits it; it must be an RGB device, whose output the wide path leaves unclamped).
    pub destination_icc: Vec<u8>,
    /// `SDOC-...` the conversion is attributed to.
    pub resource: DomainId,
    pub correlation_id: u64,
    pub revision: u64,
}

/// [`IccConverter`] over a caller-owned Prism context and diagnostics sink.
pub struct PrismConverter<'a> {
    prism: Prism,
    route: PrismRoute,
    sink: &'a mut dyn SinkPort,
    destination_sha256: [u8; 32],
    /// Created on first wide use; plans (LUT bakes) are cached inside it per profile pair.
    wide: Option<WideEngine>,
    /// Device space per inspected wide profile hash, so a conversion does not re-parse a profile
    /// just to learn its channel count.
    devices: Vec<([u8; 32], ProfileFacts)>,
}

impl<'a> PrismConverter<'a> {
    pub fn new(prism: Prism, route: PrismRoute, sink: &'a mut dyn SinkPort) -> Self {
        let destination_sha256 = profile_hash(&route.destination_icc);
        Self { prism, route, sink, destination_sha256, wide: None, devices: Vec::new() }
    }

    fn facts_of(&mut self, icc: &[u8], cancel: &CancellationToken) -> Result<ProfileFacts, IccRefusal> {
        let hash = profile_hash(icc);
        if let Some((_, facts)) = self.devices.iter().find(|(h, _)| *h == hash) {
            return Ok(*facts);
        }
        let input = ProfileInput { profile_id: &self.route.source_profile_id, bytes: icc, expected_sha256: hash };
        let descriptor = self.prism.inspect_wide_profile(input, cancel).map_err(|e| IccRefusal(e.code()))?;
        let device = match descriptor.device {
            DeviceSpace::Gray => ProfileDevice::Gray,
            DeviceSpace::Rgb => ProfileDevice::Rgb,
            DeviceSpace::Cmyk => ProfileDevice::Cmyk,
        };
        // Absolute colorimetric also needs the media white (`wtpt`) for the media-white scaling.
        let intents = [
            Intent::Perceptual,
            Intent::RelativeColorimetric,
            Intent::Saturation,
            Intent::AbsoluteColorimetric,
        ]
        .map(|i| descriptor.device_to_pcs.has(i) && (i != Intent::AbsoluteColorimetric || descriptor.media_white.is_some()));
        let facts = ProfileFacts { device, intents };
        if self.devices.len() >= MAX_CACHE_ENTRIES {
            self.devices.remove(0);
        }
        self.devices.push((hash, facts));
        Ok(facts)
    }
}

impl IccConverter for PrismConverter<'_> {
    fn destination_profile_id(&self) -> String {
        self.route.destination_profile_id.as_str().to_owned()
    }

    fn inspect(&mut self, icc: &[u8], cancel: &CancellationToken) -> Result<(), IccRefusal> {
        let input = ProfileInput {
            profile_id: &self.route.source_profile_id,
            bytes: icc,
            expected_sha256: profile_hash(icc),
        };
        self.prism.inspect_profile(input, cancel).map(|_| ()).map_err(|e| IccRefusal(e.code()))
    }

    fn convert_rgb(
        &mut self,
        icc: &[u8],
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, IccRefusal> {
        let route = &self.route;
        let mut out = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks(MAX_PIXELS) {
            let request = TransformRequest {
                source: ProfileInput {
                    profile_id: &route.source_profile_id,
                    bytes: icc,
                    expected_sha256: profile_hash(icc),
                },
                destination: ProfileInput {
                    profile_id: &route.destination_profile_id,
                    bytes: &route.destination_icc,
                    expected_sha256: profile_hash(&route.destination_icc),
                },
                pixels: chunk,
                intent: Intent::RelativeColorimetric,
                bit_depth: 32,
                black_point_compensation: false,
                revision: route.revision,
                expected_revision: route.revision,
                correlation_id: route.correlation_id,
                resource_id: &route.resource,
                actor: &route.actor,
                private_project_text: None,
            };
            match self.prism.transform(&request, cancel, &mut *self.sink) {
                Ok(result) => out.extend(result.pixels),
                Err(failure) => return Err(IccRefusal(failure.error.code())),
            }
        }
        Ok(out)
    }

    fn wide_available(&self) -> bool {
        true
    }

    fn inspect_wide(&mut self, icc: &[u8], cancel: &CancellationToken) -> Result<ProfileFacts, IccRefusal> {
        self.facts_of(icc, cancel)
    }

    fn convert_wide(
        &mut self,
        request: &WideConvertRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<WideConverted, IccRefusal> {
        let channels = match request.source {
            WideConvertSource::Lab => 3,
            WideConvertSource::Profile(icc) => self.facts_of(icc, cancel)?.device.channels(),
        };
        if request.pixels.is_empty() || !request.pixels.len().is_multiple_of(channels) {
            return Err(IccRefusal("channel_count"));
        }
        if self.wide.is_none() {
            self.wide = Some(
                WideEngine::new(self.route.actor.clone(), MAX_CACHE_ENTRIES).map_err(|e| IccRefusal(e.code()))?,
            );
        }
        let Some(engine) = self.wide.as_mut() else {
            return Err(IccRefusal("engine_failure"));
        };
        let route = &self.route;
        let destination =
            ProfileInput { profile_id: &route.destination_profile_id, bytes: &route.destination_icc, expected_sha256: self.destination_sha256 };
        let source_sha256 = match request.source {
            WideConvertSource::Profile(icc) => Some(profile_hash(icc)),
            WideConvertSource::Lab => None,
        };
        let mut out = Vec::with_capacity(request.pixels.len().checked_div(channels).unwrap_or(0));
        let mut clip = ClipSummary::default();
        for (n, chunk) in request.pixels.chunks(MAX_WIDE_PIXELS.saturating_mul(channels)).enumerate() {
            let source = match (request.source, source_sha256) {
                (WideConvertSource::Profile(icc), Some(hash)) => WideSource::Profile(ProfileInput {
                    profile_id: &route.source_profile_id,
                    bytes: icc,
                    expected_sha256: hash,
                }),
                _ => WideSource::Lab,
            };
            let wide_request = WideTransformRequest {
                source,
                destination,
                pixels: chunk,
                intent: request.intent.prism(),
                black_point_compensation: request.black_point_compensation,
                revision: route.revision,
                expected_revision: route.revision,
                correlation_id: route.correlation_id,
                resource_id: &route.resource,
                actor: &route.actor,
            };
            let result = match engine.transform(&wide_request, cancel, &mut *self.sink) {
                Ok(result) => result,
                Err(failure) => return Err(IccRefusal(failure.error.code())),
            };
            if result.receipt.destination_space != DeviceSpace::Rgb {
                return Err(IccRefusal("destination_not_rgb"));
            }
            out.extend(result.pixels.chunks_exact(3).map(|p| [p[0], p[1], p[2]]));
            let report = &result.receipt.clip;
            clip.absorb(
                &ClipSummary {
                    method: match report.method {
                        ClipMethod::NotComputed => ClipMethodKind::NotComputed,
                        ClipMethod::LinearDevice => ClipMethodKind::LinearDevice,
                        ClipMethod::Unavailable => ClipMethodKind::Unavailable,
                    },
                    pixels_clipped: report.pixels_clipped,
                    channels_clipped: report.channels_clipped,
                    max_overshoot: report.max_overshoot,
                },
                n == 0,
            );
        }
        Ok(WideConverted { pixels: out, clip })
    }
}

/// Closed set of loss codes for a refusal: the result is a `&'static str`, so a report carries a
/// fixed code and no converter text.
pub fn refusal_code(refusal: IccRefusal) -> &'static str {
    match refusal.0 {
        "profile_limit" => "icc_refused_profile_limit",
        "unsupported_profile" => "icc_refused_unsupported_profile",
        "malformed_profile" => "icc_refused_malformed_profile",
        "unsupported_version" => "icc_refused_unsupported_version",
        "unsupported_class" => "icc_refused_unsupported_class",
        "unsupported_color_space" => "icc_refused_unsupported_color_space",
        "unsupported_pcs" => "icc_refused_unsupported_pcs",
        "unsupported_tag_type" => "icc_refused_unsupported_tag_type",
        "missing_intent_table" => "icc_refused_missing_intent_table",
        "missing_media_white" => "icc_refused_missing_media_white",
        WIDE_UNAVAILABLE => "icc_refused_wide_conversion_unavailable",
        _ => "icc_refused_other",
    }
}
