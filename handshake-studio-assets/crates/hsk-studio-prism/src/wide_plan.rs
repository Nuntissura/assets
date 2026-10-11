//! Plan construction for the wide path: intent-table checks, the two moxcms legs, and the PCS
//! stage. Lane SMB-PRISM-ENG-3 builds the legs only; black point compensation (ENG-1) and
//! absolute colorimetric plus clip reporting (ENG-2) extend `build` and `clip_scan`.
use crate::wide::{
    AbsoluteReport, BpcReport, ClipMethod, ClipReport, PcsMap, Plan, WideError,
    WideProfileDescriptor, intent_table_check, leg,
};
use crate::Intent;
use hsk_studio_accord::CancellationToken;
use moxcms::ColorProfile;

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
    if black_point_compensation {
        return Err(WideError::UnsupportedBpc);
    }
    intent_table_check(intent, sd, dd)?;
    let to_pcs = leg(sp, sd.device, companion, crate::wide::DeviceSpace::Rgb, intent)?;
    cancel.check().map_err(|_| WideError::Canceled)?;
    let from_pcs = leg(companion, crate::wide::DeviceSpace::Rgb, dp, dd.device, intent)?;
    Ok(Plan {
        to_pcs,
        from_pcs,
        map: PcsMap::Identity,
        bpc: BpcReport::default(),
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
