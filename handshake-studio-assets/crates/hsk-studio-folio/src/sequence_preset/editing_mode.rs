//! STU-VID-017 / 017a editing modes: a named constraint set over sequence settings, shipped as the
//! 53 rows of the 017a table with `constraints_known = false` and every allow-list empty (declared
//! gap STU-VID-080: the lists are authored deliberately, never inferred from presets; test
//! `editing_mode_constraint_lists_are_a_declared_gap_mt_33699`).
#![deny(clippy::arithmetic_side_effects)]
use super::super::{Budget, Code, Result, SequenceFrameRate, Unresolved, err, key, values::Ctx};
use super::settings::{SequenceFieldOrder, SequenceFrameSize, SequencePixelAspectRatio, StudioSequenceSettings};
use super::shipped::SHIPPED_MODE_ROWS;
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Most editing modes one list may hold (Studio bound; 53 ship).
pub const MAX_EDITING_MODES: usize = 1024;
/// Most entries of one constraint allow-list (Studio bound; the capture declares at most 12 slots).
pub const MAX_EDITING_MODE_ALLOW_LIST: usize = 1024;
/// The mode id of `Custom`, the mode that declares no constraints (STU-VID-017).
pub const CUSTOM_EDITING_MODE: &str = "custom";

/// A platform column value of the STU-VID-017a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditingModePlatform {
    /// `All`.
    All,
    Mac,
    Win,
}

/// The constraint-list STRUCTURE the capture declares per mode: slot counts per axis (STU-VID-017a
/// "188 frame-rate slots and 52 frame-rect slots across the 53 modes"). Every slot decoded empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEditingModeSlots {
    pub frame_rate: u16,
    pub frame_rect: u16,
    pub pixel_aspect: u16,
    pub field_type: u16,
}

/// A mode's constraint lists: an explicit allow-list per axis; an EMPTY allow-list means
/// unconstrained on that axis, never "nothing allowed" (STU-VID-017a; test
/// `editing_mode_constraint_lists_are_a_declared_gap_mt_33699`). Devices are opaque keys of
/// the playback and capture devices that may service the mode (STU-VID-017). Each list is omitted
/// from the wire when empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEditingModeConstraints {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_frame_rates: Vec<SequenceFrameRate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_frame_sizes: Vec<SequenceFrameSize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_pixel_aspect_ratios: Vec<SequencePixelAspectRatio>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_field_orders: Vec<SequenceFieldOrder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playback_devices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_devices: Vec<String>,
}
impl StudioEditingModeConstraints {
    /// No allow-list holds an entry.
    pub fn is_empty(&self) -> bool {
        self.allowed_frame_rates.is_empty()
            && self.allowed_frame_sizes.is_empty()
            && self.allowed_pixel_aspect_ratios.is_empty()
            && self.allowed_field_orders.is_empty()
            && self.playback_devices.is_empty()
            && self.capture_devices.is_empty()
    }
}

/// STU-VID-017 `StudioEditingMode`: a named constraint set over sequence settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEditingMode {
    /// Studio key of the mode (the table name, lower case, non-alphanumerics as `_`; the unnamed
    /// row is `unnamed`). The table's names are the spec's; the ids are Studio's.
    pub mode_id: String,
    /// The table name; explicit null for the unnamed row.
    #[serde(deserialize_with = "super::super::required")]
    #[schemars(required)]
    pub name: Option<String>,
    /// The Windows GUID column; explicit null where the table gives none (two GUIDs are shared by
    /// two modes each, as declared).
    #[serde(deserialize_with = "super::super::required")]
    #[schemars(required)]
    pub windows_guid: Option<String>,
    /// The platform column (`All`, or `Mac` and `Win`); empty for the unnamed row (`--`).
    pub platforms: Vec<EditingModePlatform>,
    pub slots: StudioEditingModeSlots,
    /// False on every shipped mode (STU-VID-017a): nothing is enforced and every allow-list is empty
    /// until the lists are authored deliberately.
    pub constraints_known: bool,
    #[serde(default, skip_serializing_if = "StudioEditingModeConstraints::is_empty")]
    pub constraints: StudioEditingModeConstraints,
}

/// One settings axis an editing mode constrains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EditingModeAxis {
    FrameRate,
    FrameSize,
    PixelAspectRatio,
    FieldOrder,
}
impl EditingModeAxis {
    pub const fn code(self) -> &'static str {
        match self {
            Self::FrameRate => "frame_rate",
            Self::FrameSize => "frame_size",
            Self::PixelAspectRatio => "pixel_aspect_ratio",
            Self::FieldOrder => "field_order",
        }
    }
}

/// The axes on which `settings` falls outside `mode`'s allow-lists. A mode whose
/// `constraints_known` is false enforces nothing (STU-VID-017a), and an empty allow-list leaves its
/// axis unconstrained, so every shipped mode yields none.
pub fn editing_mode_violations(mode: &StudioEditingMode, settings: &StudioSequenceSettings) -> Vec<EditingModeAxis> {
    if !mode.constraints_known {
        return Vec::new();
    }
    let c = &mode.constraints;
    let outside = |empty: bool, contains: bool| !empty && !contains;
    let mut out = Vec::new();
    if outside(c.allowed_frame_rates.is_empty(), c.allowed_frame_rates.contains(&settings.frame_rate)) {
        out.push(EditingModeAxis::FrameRate);
    }
    if outside(c.allowed_frame_sizes.is_empty(), c.allowed_frame_sizes.contains(&settings.frame_size)) {
        out.push(EditingModeAxis::FrameSize);
    }
    if outside(c.allowed_pixel_aspect_ratios.is_empty(), c.allowed_pixel_aspect_ratios.contains(&settings.pixel_aspect_ratio)) {
        out.push(EditingModeAxis::PixelAspectRatio);
    }
    if outside(c.allowed_field_orders.is_empty(), c.allowed_field_orders.contains(&settings.field_order)) {
        out.push(EditingModeAxis::FieldOrder);
    }
    out
}

/// The 53 shipped editing modes, in STU-VID-017a table order.
pub fn shipped_editing_modes() -> Vec<StudioEditingMode> {
    SHIPPED_MODE_ROWS.iter().map(super::ShippedModeRow::mode).collect()
}

/// The shipped editing mode `mode_id`.
pub fn shipped_editing_mode(mode_id: &str) -> Option<StudioEditingMode> {
    SHIPPED_MODE_ROWS.iter().find(|r| r.0 == mode_id).map(super::ShippedModeRow::mode)
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(err(Code::InvalidField, format!("{target}/{field}"))) }
}

/// Validates a list of editing modes (shipped or authored): at most `MAX_EDITING_MODES` (counted
/// before any is walked, then charged against `Budget::nodes`), unique key ids (`InvalidId` /
/// `DuplicateIdentity`), names and GUIDs of at most 128 bytes without control characters,
/// platforms listed once, each allow-list at most `MAX_EDITING_MODE_ALLOW_LIST` entries
/// (`BudgetExceeded`, charged against `Budget::nodes`) with valid members and no repeats, device
/// keys in the key grammar; and a mode with `constraints_known` false carries no allow-list entry
/// (`InvalidField` at `<mode>/constraints`: lists are never stored for a mode whose constraints are
/// not known; test `editing_mode_constraint_lists_are_a_declared_gap_mt_33699`).
pub fn validate_editing_modes(modes: &[StudioEditingMode], b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    if modes.len() > MAX_EDITING_MODES {
        return Err(err(Code::BudgetExceeded, "editing_modes"));
    }
    cx.nodes(modes.len(), "editing_modes")?;
    let mut ids = BTreeSet::new();
    for m in modes {
        cx.tick()?;
        let t = m.mode_id.as_str();
        if !key(t) {
            return Err(err(Code::InvalidId, "editing_modes/mode_id"));
        }
        if !ids.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        for text in [&m.name, &m.windows_guid].into_iter().flatten() {
            check(super::super::values::label(text), t, "name")?;
            cx.text(text, t)?;
        }
        let platforms: BTreeSet<_> = m.platforms.iter().collect();
        check(platforms.len() == m.platforms.len(), t, "platforms")?;
        let c = &m.constraints;
        let lengths = [
            c.allowed_frame_rates.len(),
            c.allowed_frame_sizes.len(),
            c.allowed_pixel_aspect_ratios.len(),
            c.allowed_field_orders.len(),
            c.playback_devices.len(),
            c.capture_devices.len(),
        ];
        if lengths.iter().any(|n| *n > MAX_EDITING_MODE_ALLOW_LIST) {
            return Err(err(Code::BudgetExceeded, format!("{t}/constraints")));
        }
        for n in lengths {
            cx.nodes(n, t)?;
        }
        check(m.constraints_known || c.is_empty(), t, "constraints")?;
        check(c.allowed_frame_rates.iter().all(|r| r.accord().is_some()), t, "constraints")?;
        check(c.allowed_frame_sizes.iter().all(SequenceFrameSize::valid), t, "constraints")?;
        check(c.allowed_pixel_aspect_ratios.iter().all(SequencePixelAspectRatio::valid), t, "constraints")?;
        let unique = |n: usize, set: usize| n == set;
        check(unique(c.allowed_frame_rates.len(), c.allowed_frame_rates.iter().map(|r| r.ticks_per_frame).collect::<BTreeSet<_>>().len()), t, "constraints")?;
        check(unique(c.allowed_frame_sizes.len(), c.allowed_frame_sizes.iter().map(|s| (s.width, s.height)).collect::<BTreeSet<_>>().len()), t, "constraints")?;
        check(unique(c.allowed_pixel_aspect_ratios.len(), c.allowed_pixel_aspect_ratios.iter().map(|p| (p.numerator, p.denominator)).collect::<BTreeSet<_>>().len()), t, "constraints")?;
        check(unique(c.allowed_field_orders.len(), c.allowed_field_orders.iter().collect::<BTreeSet<_>>().len()), t, "constraints")?;
        for device in c.playback_devices.iter().chain(&c.capture_devices) {
            check(key(device), t, "constraints")?;
            cx.text(device, t)?;
        }
        for devices in [&c.playback_devices, &c.capture_devices] {
            check(unique(devices.len(), devices.iter().collect::<BTreeSet<_>>().len()), t, "constraints")?;
        }
    }
    Ok(())
}
