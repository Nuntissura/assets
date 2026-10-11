//! Plan construction for the wide path: intent-table checks, the two moxcms legs, and the PCS
//! stage. Black point compensation lives in `bpc.rs`; absolute colorimetric scaling and the clip
//! report (SMB-PRISM-ENG-2) extend `build` and `clip_scan`.
use crate::bpc::{BpcInputs, compute};
use crate::wide::{
    AbsoluteReport, BpcReport, ClipMethod, ClipReport, DeviceSpace, PcsMap, Plan, ProfileKind,
    WideError, WideProfileDescriptor, intent_table_check, leg,
};
use crate::Intent;
use hsk_studio_accord::CancellationToken;
use moxcms::ColorProfile;

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

pub(crate) fn build(
    companion: &ColorProfile,
    source: (&ColorProfile, &WideProfileDescriptor),
    destination: (&ColorProfile, &WideProfileDescriptor),
    intent: Intent,
    black_point_compensation: bool,
    cancel: &CancellationToken,
) -> Result<Plan, WideError> {
    let ((sp, sd), (dp, dd)) = (source, destination);
    if intent == Intent::AbsoluteColorimetric {
        return Err(WideError::UnsupportedIntent);
    }
    intent_table_check(intent, sd, dd)?;
    let to_pcs = leg(sp, sd.device, companion, DeviceSpace::Rgb, intent)?;
    cancel.check().map_err(|_| WideError::Canceled)?;
    let from_pcs = leg(companion, DeviceSpace::Rgb, dp, dd.device, intent)?;
    let suppressed = engine_builtin_would_apply(intent, sd, dd);
    let (map, bpc) = if black_point_compensation {
        let (map, mut report) = compute(
            &BpcInputs {
                companion,
                source: (sp, sd),
                destination: (dp, dd),
                intent,
                to_pcs: &to_pcs,
                from_pcs: &from_pcs,
            },
            cancel,
        )?;
        report.engine_builtin_suppressed = suppressed;
        (map, report)
    } else {
        (
            PcsMap::Identity,
            BpcReport {
                rule: "not_requested",
                engine_builtin_suppressed: suppressed,
                ..Default::default()
            },
        )
    };
    Ok(Plan {
        to_pcs,
        from_pcs,
        map,
        bpc,
        absolute: AbsoluteReport::default(),
        clip_executor: None,
        clip_method: ClipMethod::NotComputed,
    })
}

/// Accumulate clip statistics for one chunk (ENG-2); a no-op while the method is `NotComputed`.
pub(crate) fn clip_scan(
    _plan: &Plan,
    _pcs_xyz: &[f32],
    _out_channels: usize,
    _clip: &mut ClipReport,
) -> Result<(), WideError> {
    Ok(())
}
