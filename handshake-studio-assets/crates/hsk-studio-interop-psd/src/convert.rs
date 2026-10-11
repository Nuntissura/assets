//! Display conversion of embedded ICC profiles through Prism. The importer only asks two things of
//! a converter: whether it accepts a profile (`inspect`) and RGB-to-working-space conversion
//! (`convert_rgb`). A refusal is a typed loss, never a silent fallback; nothing here works around
//! Prism limits.
//!
//! Current Prism admission (read from `hsk-studio-prism`): ICC v4, RGB data space, display class,
//! matrix plus identical parametric curves, at most 64 KiB, 4096 pixels per request, three
//! channels. CMYK, Lab and gray (`kTRC`) profiles, LUT profiles and v2 profiles are refused. Those
//! needs are tracked as a dependency on lane SMB-PRISM-ENG-3.

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::SinkPort;
use hsk_studio_prism::{ColorEngine, Intent, MAX_PIXELS, Prism, ProfileInput, TransformRequest, profile_hash};

/// Why a converter did not take a profile: the converter's own fixed error code (for Prism, the
/// `hsk_studio_prism::Error::code()` string, for example `profile_limit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IccRefusal(pub &'static str);

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
}

/// Host-supplied identities and destination for [`PrismConverter`].
pub struct PrismRoute {
    pub actor: ActorContext,
    /// `SCPF-...` the host assigns to the document's embedded profile.
    pub source_profile_id: DomainId,
    /// `SCPF-...` of the destination profile.
    pub destination_profile_id: DomainId,
    /// Destination profile bytes: a linear-light profile Prism admits.
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
}

impl<'a> PrismConverter<'a> {
    pub fn new(prism: Prism, route: PrismRoute, sink: &'a mut dyn SinkPort) -> Self {
        Self { prism, route, sink }
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
}

/// Closed set of loss codes for a refusal, so reports never carry converter text.
pub fn refusal_code(refusal: IccRefusal) -> &'static str {
    match refusal.0 {
        "profile_limit" => "icc_refused_profile_limit",
        "unsupported_profile" => "icc_refused_unsupported_profile",
        "malformed_profile" => "icc_refused_malformed_profile",
        _ => "icc_refused_other",
    }
}
