//! Plan construction for the wide path: the Prism-evaluated source and destination stages
//! (`icc_eval.rs`, ICC.1:2022 8.10.2 tag precedence) and the Prism PCS stage between them. Black point
//! compensation lives in `bpc.rs`; this file adds absolute colorimetric media-white scaling and, for
//! matrix/TRC and Gray pairs, fuses the two matrices and the PCS map into one affine 3x3 (SMB-PRISM-ENG-6).
//!
//! Absolute colorimetric rule: both stages use the colorimetric tables (relative); the PCS XYZ is then
//! scaled component-wise by `source media white / destination media white` (ICC `wtpt` tags, a Lab
//! source counting as D50). A relative value `x` has absolute value `x * MW / D50`, so the
//! destination relative value is `x_src * MW_src / MW_dst`. The black point compensation flag is
//! ignored for this intent (receipt rule `absolute_ignores_bpc`). A profile without `wtpt` is
//! refused (`MissingMediaWhite`) rather than assumed to be D50.
//!
//! Media-white rule: the `wtpt` tag is used as written for every profile (receipt
//! `absolute.media_white_rule`). LittleCMS 2 reads D50 instead for an ICC v2 display profile; when such a
//! profile with a non-D50 `wtpt` takes part, the receipt names the registered divergence
//! (`absolute.divergence` = `ABS_V2_DISPLAY_WTPT_DIVERGENCE`) so no conversion applies one convention silently.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::Intent;
use crate::bpc::{BpcInputs, compute};
use crate::icc_eval::{self, D50, Fused};
use crate::wide::{
    ABS_V2_DISPLAY_WTPT_DIVERGENCE, AbsoluteReport, BpcReport, DeviceSpace, MEDIA_WHITE_RULE_WTPT_AS_WRITTEN, PcsMap,
    Plan, ProfileKind, WTPT_SCALE_TOLERANCE, WideError, WideProfileDescriptor,
};
use hsk_studio_accord::CancellationToken;
use moxcms::ColorProfile;

/// moxcms applies a constant-black scaling only for perceptual/saturation between two RGB ICC v4
/// profiles of which at least one is table-based (its `detect_black_point` is `None` for CMYK, Gray
/// and v2 profiles, and the scaling lives only in the table paths). Prism runs no moxcms
/// transform on the wide path, so this only feeds the receipt's `engine_builtin_suppressed`.
fn engine_builtin_would_apply(
    intent: Intent,
    source: &WideProfileDescriptor,
    destination: &WideProfileDescriptor,
) -> bool {
    let table = |d: &WideProfileDescriptor| {
        matches!(d.kind, ProfileKind::LegacyLut | ProfileKind::MultiDimensional)
    };
    matches!(intent, Intent::Perceptual | Intent::Saturation)
        && source.device == DeviceSpace::Rgb
        && destination.device == DeviceSpace::Rgb
        && source.version_major >= 4
        && destination.version_major >= 4
        && (table(source) || table(destination))
}

pub(crate) fn build(
    source: Option<(&ColorProfile, &WideProfileDescriptor)>,
    destination: (&ColorProfile, &WideProfileDescriptor),
    intent: Intent,
    black_point_compensation: bool,
    fusion: bool,
    cancel: &CancellationToken,
) -> Result<Plan, WideError> {
    let (dp, dd) = destination;
    let to_pcs = match source {
        Some((sp, sd)) => Some(icc_eval::to_pcs(sp, sd, intent)?),
        None => None,
    };
    cancel.check().map_err(|_| WideError::Canceled)?;
    let from_pcs = icc_eval::from_pcs(dp, dd, intent)?;
    let suppressed = source.is_some_and(|(_, sd)| engine_builtin_would_apply(intent, sd, dd));
    let (map, bpc, absolute) = if intent == Intent::AbsoluteColorimetric {
        let src_mw = match source {
            Some((_, sd)) => sd.media_white.ok_or(WideError::MissingMediaWhite)?,
            None => D50,
        };
        let dst_mw = dd.media_white.ok_or(WideError::MissingMediaWhite)?;
        if src_mw.iter().chain(&dst_mw).any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(WideError::MissingMediaWhite);
        }
        let scale = [src_mw[0] / dst_mw[0], src_mw[1] / dst_mw[1], src_mw[2] / dst_mw[2]];
        // The single place that applies absolute colorimetric: every route converts through this plan, so the
        // rule and the registered divergence are recorded here and nowhere else (tests/wide_r6_plausibles.rs:
        // absolute_receipt_names_the_media_white_rule_and_the_v2_display_divergence_on_every_route).
        // The LittleCMS 2 reading of the same pair: a v2 display profile counts as D50, a Lab source as D50.
        let lcms2_white = |d: &WideProfileDescriptor, written: [f64; 3]| if d.is_v2_display() { D50 } else { written };
        let alt_src = match source {
            Some((_, sd)) => lcms2_white(sd, src_mw),
            None => D50,
        };
        let alt_dst = lcms2_white(dd, dst_mw);
        let alt_scale = [alt_src[0] / alt_dst[0], alt_src[1] / alt_dst[1], alt_src[2] / alt_dst[2]];
        let divergence = scale
            .iter()
            .zip(alt_scale)
            .any(|(written, alt)| (written - alt).abs() > WTPT_SCALE_TOLERANCE)
            .then_some(ABS_V2_DISPLAY_WTPT_DIVERGENCE);
        (
            if scale == [1.0; 3] {
                PcsMap::Identity
            } else {
                PcsMap::Scale(scale)
            },
            BpcReport {
                requested: black_point_compensation,
                rule: if black_point_compensation {
                    "absolute_ignores_bpc"
                } else {
                    "not_requested"
                },
                ..Default::default()
            },
            AbsoluteReport {
                applied: true,
                source_media_white: Some(src_mw),
                destination_media_white: Some(dst_mw),
                scale: Some(scale),
                media_white_rule: MEDIA_WHITE_RULE_WTPT_AS_WRITTEN,
                divergence,
            },
        )
    } else if black_point_compensation {
        let (map, mut report) = compute(
            &BpcInputs {
                source,
                destination: (dp, dd),
                intent,
                to_pcs: to_pcs.as_ref(),
                from_pcs: &from_pcs,
            },
            cancel,
        )?;
        report.engine_builtin_suppressed = suppressed;
        (map, report, AbsoluteReport::default())
    } else {
        (
            PcsMap::Identity,
            BpcReport {
                rule: "not_requested",
                engine_builtin_suppressed: suppressed,
                ..Default::default()
            },
            AbsoluteReport::default(),
        )
    };
    let fused = match (&to_pcs, fusion) {
        (Some(src), true) => {
            let (scale, offset) = map.affine();
            Fused::build(src, scale, offset, &from_pcs)
        }
        _ => None,
    };
    Ok(Plan {
        source: to_pcs,
        dest: from_pcs,
        map,
        fused,
        bpc,
        absolute,
    })
}
