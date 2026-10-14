//! Colour on import (SMB-CONVERT-COLOUR): every importer hands interop-psd's `PrismConverter` (the
//! existing adapter over `hsk-studio-prism`'s inspect/transform API) to `to_proposal` /
//! `raster_proposal`, so an embedded ICC profile (PSD resource 1039, TIFF tag 34675) is converted by
//! Prism into the document working space; the harness adds no colour math (CX-CAP-001). The working
//! space is linear-light sRGB: the destination is the 480-byte CC0 ICC v4 profile in
//! `profiles/sRGB-linear-v4.icc` (Prism's own test derivative, sha256 4813c25a...), the same
//! linear sRGB the importer assumes without a profile and that develop's camera solution outputs.
//!
//! What interop-psd decides stays typed (tests in `tests/import_colour.rs`
//! `embedded_profiles_convert_through_prism_into_the_working_space` and
//! `missing_or_unusable_profiles_are_typed_losses`): no profile is the recorded sRGB assumption
//! (`profile_assumed_srgb`), a profile Prism refuses is a loss entry with Prism's code, CMYK without
//! a usable profile is a typed unmapped layer. The profile bytes stay in the import's original entry;
//! the receipt key `colour` records the route, the profile digests and interop-psd's conversion report.
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::SourceMint;
use super::{ImportError, SourceFormat};
use hsk_studio_accord::DomainId;
use hsk_studio_interop_psd as psd;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::{Prism, hash_hex, profile_hash};
use serde_json::{Value, json};

/// Destination profile of every conversion: linear-light sRGB, ICC v4 matrix/TRC (CC0).
pub const WORKING_PROFILE: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");

/// Prism contexts cache this many transforms per import.
const PRISM_CACHE_ENTRIES: usize = 4;

/// Counts the Observe frames Prism delivers during one import (recorded in the receipt).
#[derive(Default)]
pub(crate) struct FrameCount(pub u64);

impl SinkPort for FrameCount {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        self.0 = self.0.saturating_add(1);
        Ok(())
    }
}

/// A Prism-backed converter for one import. Profile and resource ids come from reserved counters of
/// the source's deterministic mint, so they never collide with the minted document identities.
pub(crate) fn converter<'s>(
    format: SourceFormat,
    mint: &SourceMint,
    sink: &'s mut FrameCount,
) -> Result<psd::PrismConverter<'s>, ImportError> {
    let id = |prefix: &str, k: u32| DomainId::parse(&mint.reserved(prefix, k)).map_err(|_| ImportError::Importer { format, code: "colour_id" });
    let actor = crate::default_actor();
    let prism = Prism::new(actor.clone(), PRISM_CACHE_ENTRIES).map_err(|e| ImportError::Importer { format, code: e.code() })?;
    let route = psd::PrismRoute {
        actor,
        source_profile_id: id("SCPF", 1)?,
        destination_profile_id: id("SCPF", 2)?,
        destination_icc: WORKING_PROFILE.to_vec(),
        resource: id("SDOC", 3)?,
        correlation_id: 1,
        revision: 1,
    };
    Ok(psd::PrismConverter::new(prism, route, sink))
}

/// Receipt key `colour`: what happened to the source's colour on the way into the working space.
pub(crate) fn summary(proposal: &psd::ImportProposal, frames: u64, route: &str) -> Value {
    let p = &proposal.provenance;
    let status = if p.icc_converted || p.display.is_some() {
        "converted"
    } else if p.assumed_profile {
        "assumed"
    } else if p.icc_sha256_hex.is_some() {
        "not_applied"
    } else {
        "unmapped_or_not_applicable"
    };
    let losses: Vec<Value> = proposal
        .loss
        .entries
        .iter()
        .filter(|e| e.path.starts_with("document/") || e.code.contains("profile") || e.code.contains("icc"))
        .map(|e| json!({"path": e.path, "kind": e.kind.as_str(), "code": e.code}))
        .collect();
    let transform = p.display.as_ref().map(|d| {
        json!({
            "source_space": d.source_space, "intent": format!("{:?}", d.intent), "intent_assumed": d.intent_assumed,
            "black_point_compensation": d.black_point_compensation,
            "black_point_compensation_assumed": d.black_point_compensation_assumed,
            "pixels_converted": d.pixels_converted, "pixels_clamped": d.pixels_clamped,
            "pixels_out_of_gamut": d.pixels_out_of_gamut, "channels_clamped": d.channels_clamped,
            "max_overshoot": d.max_overshoot, "input_range_clamped": d.input_range_clamped,
            "bpc": {"requested": d.converter_bpc.requested, "applied": d.converter_bpc.applied, "rule": d.converter_bpc.rule},
            "absolute": {"applied": d.converter_absolute.applied, "divergence": d.converter_absolute.divergence},
            "clip": {"pixels_clipped": d.converter_clip.pixels_clipped, "max_overshoot": d.converter_clip.max_overshoot},
            "engine": {"name": d.converter_engine.name, "version": d.converter_engine.version,
                       "simd": d.converter_engine.simd, "fma_contraction": d.converter_engine.fma_contraction},
        })
    });
    json!({
        "status": status,
        "route": route,
        "working_space": "linear-light sRGB",
        "working_profile_sha256": hash_hex(&profile_hash(WORKING_PROFILE)),
        "converter": "hsk-studio-prism through hsk-studio-interop-psd PrismConverter",
        "embedded_profile_sha256": p.icc_sha256_hex,
        "embedded_profile_bytes": "kept in the original import entry",
        "icc_converted": p.icc_converted,
        "assumed_profile": p.assumed_profile,
        "prism_frames": frames,
        "transform": transform,
        "losses": losses,
    })
}
