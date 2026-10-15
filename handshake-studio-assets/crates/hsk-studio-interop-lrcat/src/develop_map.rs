#![deny(clippy::arithmetic_side_effects)]
//! Develop-settings mapping (lane SMB-CKC-PORTS, lead-approved edge interop-lrcat -> develop):
//! the top-level scalars of a Lightroom develop-settings text map into develop's own typed
//! controls (`BasicTone`, `Presence`, `AbsoluteWhiteBalance`, exposure stops). Key names are the
//! community-known Camera Raw names and are UNVERIFIED against a Lightroom-written catalog; the
//! numeric meaning of develop's controls is develop's own documented mapping, so every mapped row
//! also carries `develop_scale_unverified` (Approximate): equal slider numbers do not mean equal
//! pixels (D5 oracle). Out-of-range or malformed values are typed losses, never clipped.
use crate::extract::{DevelopProposal, Disposition};
use hsk_studio_develop::{AbsoluteWhiteBalance, BasicTone, Presence, white_xy};

/// Count-scale soft range of develop's sliders (develop `COUNT_LIMIT`, STU-RAW-110/111).
const COUNT_LIMIT: f64 = 100.0;
/// Exposure range accepted as stops (Camera Raw `Exposure2012` slider range, UNVERIFIED).
const EXPOSURE_LIMIT: f64 = 5.0;

/// Keys this mapping consumes (ProcessVersion is consumed by the fixed extraction).
pub const MAPPED_DEVELOP_KEYS: &[&str] = &[
    "Exposure2012",
    "Contrast2012",
    "Highlights2012",
    "Shadows2012",
    "Whites2012",
    "Blacks2012",
    "Vibrance",
    "Saturation",
    "Clarity2012",
    "WhiteBalance",
    "Temperature",
    "Tint",
    "ProcessVersion",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WhiteBalanceSource {
    /// `WhiteBalance = "As Shot"` (or absent): develop resolves the as-shot neutral itself.
    AsShot,
    /// A custom kelvin/tint pair that develop's `white_xy` accepts.
    Absolute(AbsoluteWhiteBalance),
    /// Any other preset or an invalid pair (listed as a loss).
    Unmapped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopMapping {
    pub image_local_id: i64,
    pub exposure_stops: Option<f64>,
    pub tone: BasicTone,
    pub presence: Presence,
    pub white_balance: WhiteBalanceSource,
    /// Number of scalar keys that mapped into a typed control.
    pub mapped_keys: u32,
}

/// (field, disposition, code) losses of one row; the caller aggregates them.
pub(crate) type DevelopLoss = (&'static str, Disposition, &'static str);

fn number(scalars: &std::collections::BTreeMap<String, String>, key: &'static str, limit: f64, losses: &mut Vec<DevelopLoss>) -> Option<f64> {
    let raw = scalars.get(key)?;
    match raw.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v.abs() <= limit => Some(v),
        Ok(_) => {
            losses.push((key, Disposition::Unsupported, "develop_value_out_of_range"));
            None
        }
        Err(_) => {
            losses.push((key, Disposition::Unsupported, "develop_value_not_number"));
            None
        }
    }
}

pub(crate) fn map_develop(d: &DevelopProposal) -> (DevelopMapping, Vec<DevelopLoss>) {
    let s = &d.scalars;
    let mut losses: Vec<DevelopLoss> = Vec::new();
    let mut mapped = 0u32;
    let mut take = |key: &'static str, limit: f64, losses: &mut Vec<DevelopLoss>| {
        let v = number(s, key, limit, losses);
        if v.is_some() {
            mapped = mapped.saturating_add(1);
        }
        v
    };
    let exposure_stops = take("Exposure2012", EXPOSURE_LIMIT, &mut losses);
    let tone = BasicTone {
        contrast: take("Contrast2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        highlights: take("Highlights2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        shadows: take("Shadows2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        whites: take("Whites2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        blacks: take("Blacks2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
    };
    let presence = Presence {
        vibrance: take("Vibrance", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        saturation: take("Saturation", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
        clarity: take("Clarity2012", COUNT_LIMIT, &mut losses).unwrap_or(0.0),
    };
    let white_balance = match s.get("WhiteBalance").map(String::as_str) {
        None | Some("As Shot") => WhiteBalanceSource::AsShot,
        Some("Custom") => {
            let kelvin = take("Temperature", 50_000.0, &mut losses);
            let tint = take("Tint", 150.0, &mut losses);
            match (kelvin, tint) {
                (Some(kelvin), Some(tint)) if white_xy(kelvin, tint).is_ok() => {
                    mapped = mapped.saturating_add(1);
                    WhiteBalanceSource::Absolute(AbsoluteWhiteBalance { kelvin, tint })
                }
                _ => {
                    losses.push(("WhiteBalance", Disposition::Unsupported, "develop_white_balance_invalid"));
                    WhiteBalanceSource::Unmapped
                }
            }
        }
        Some(_) => {
            losses.push(("WhiteBalance", Disposition::Approximate, "develop_white_balance_preset_unmapped"));
            WhiteBalanceSource::Unmapped
        }
    };
    let unmapped = s.keys().filter(|k| !MAPPED_DEVELOP_KEYS.contains(&k.as_str())).count();
    for _ in 0..unmapped {
        losses.push(("", Disposition::Opaque, "develop_key_unmapped"));
    }
    if mapped > 0 {
        losses.push(("", Disposition::Approximate, "develop_scale_unverified"));
    }
    (
        DevelopMapping {
            image_local_id: d.image_local_id,
            exposure_stops,
            tone,
            presence,
            white_balance,
            mapped_keys: mapped,
        },
        losses,
    )
}
