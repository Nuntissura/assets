//! Plan construction for the wide path: intent-table checks, the two moxcms legs, and the Prism PCS
//! stage between them. Black point compensation lives in `bpc.rs`; this file adds absolute
//! colorimetric media-white scaling and picks the clip-detection method (SMB-PRISM-ENG-2).
//!
//! Absolute colorimetric rule: both legs use the colorimetric tables (relative); the PCS XYZ is then
//! scaled component-wise by `source media white / destination media white` (ICC `wtpt` tags, a Lab
//! source counting as D50). A relative value `x` has absolute value `x * MW / D50`, so the
//! destination relative value is `x_src * MW_src / MW_dst`. The black point compensation flag is
//! ignored for this intent (receipt rule `absolute_ignores_bpc`). A profile without `wtpt` is
//! refused (`MissingMediaWhite`) rather than assumed to be D50.
use crate::Intent;
use crate::bpc::{BpcInputs, compute};
use crate::wide::{
    AbsoluteReport, BpcReport, ClipPlan, DeviceSpace, PcsMap, Plan, ProfileKind, WideError,
    WideProfileDescriptor, intent_table_check, leg,
};
use hsk_studio_accord::CancellationToken;
use moxcms::ColorProfile;

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// moxcms applies a constant-black scaling only for perceptual/saturation between two RGB ICC v4
/// profiles of which at least one is table-based (its `detect_black_point` is `None` for CMYK, Gray
/// and v2 profiles, and the scaling lives only in the table paths).
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

fn inverse(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let c = |r: usize, k: usize| {
        let (r1, r2, k1, k2) = ((r + 1) % 3, (r + 2) % 3, (k + 1) % 3, (k + 2) % 3);
        m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    // Inverse = adjugate / det; the adjugate is the transposed cofactor matrix.
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            *v = c(k, r) / det;
        }
    }
    Some(out)
}

fn clip_plan(destination: (&ColorProfile, &WideProfileDescriptor)) -> ClipPlan {
    let (profile, desc) = destination;
    match desc.kind {
        ProfileKind::MatrixTrc => inverse(profile.colorant_matrix().v).map_or(ClipPlan::Unavailable, |mut m| {
            // The legs carry XYZ / D50, so fold D50 into the columns.
            for row in &mut m {
                for (v, white) in row.iter_mut().zip(D50) {
                    *v *= white;
                }
            }
            ClipPlan::Rgb(m)
        }),
        ProfileKind::GrayTrc => ClipPlan::Gray,
        ProfileKind::LegacyLut | ProfileKind::MultiDimensional => ClipPlan::Unavailable,
    }
}

pub(crate) fn build(
    companion: &ColorProfile,
    source: Option<(&ColorProfile, &WideProfileDescriptor)>,
    destination: (&ColorProfile, &WideProfileDescriptor),
    intent: Intent,
    black_point_compensation: bool,
    cancel: &CancellationToken,
) -> Result<Plan, WideError> {
    let (dp, dd) = destination;
    intent_table_check(intent, source.map(|(_, d)| d), dd)?;
    let to_pcs = match source {
        Some((sp, sd)) => Some(leg(sp, sd.device, companion, DeviceSpace::Rgb, intent)?),
        None => None,
    };
    cancel.check().map_err(|_| WideError::Canceled)?;
    let from_pcs = leg(companion, DeviceSpace::Rgb, dp, dd.device, intent)?;
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
            },
        )
    } else if black_point_compensation {
        let (map, mut report) = compute(
            &BpcInputs {
                companion,
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
    Ok(Plan {
        to_pcs,
        from_pcs,
        map,
        bpc,
        absolute,
        clip: clip_plan((dp, dd)),
    })
}
