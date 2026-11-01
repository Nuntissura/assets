//! Lane SMB-ID-TOC-INDEX-1: trap presets (STU-LAY-152; MT-30846, MT-30847). One test per bound-set
//! row of the parameter table (MT-30847 AC-001..AC-007), plus the capture conflict, the enumerations,
//! the per-unit width conversions, hostile values and the Observe report. Oracles are the spec table.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_layout::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};

/// Parameter, name, hard_min, hard_max, unit code, precision; soft bounds and default are unknown
/// for every row.
fn assert_row(p: TrapParameter, name: &str, hard_min: f64, hard_max: f64, unit: &str, precision: Option<u32>) {
    assert_eq!(p.name(), name);
    let b = p.bounds();
    assert_eq!((b.hard_min, b.hard_max), (hard_min, hard_max), "{name} hard bounds");
    assert_eq!((b.soft_min, b.soft_max, b.default), (None, None, None), "{name}: unknown is never mirrored or invented");
    assert_eq!(b.unit.code(), unit, "{name} unit");
    assert_eq!(b.precision, precision, "{name} precision");
    // Hard bounds admit their own values and refuse just past them (a validation error, not a clamp).
    assert_eq!(p.check(hard_min), Ok(hard_min));
    assert_eq!(p.check(hard_max), Ok(hard_max));
    let step = if precision.is_some() { 0.001 } else { 0.5 };
    assert_eq!(p.check(hard_min - step), Err(TrapPresetError::ParameterOutOfBounds(p)));
    assert_eq!(p.check(hard_max + step), Err(TrapPresetError::ParameterOutOfBounds(p)));
    assert_eq!(p.check(f64::NAN), Err(TrapPresetError::ParameterNotFinite(p)));
    assert_eq!(p.check(f64::INFINITY), Err(TrapPresetError::ParameterNotFinite(p)));
    // Storage keeps the value; a refused value leaves the record unchanged.
    let mut v = TrapPresetValues::default();
    assert_eq!(v.get(p), None);
    v.set(p, hard_max).unwrap();
    assert_eq!(v.get(p), Some(hard_max));
    assert!(v.set(p, hard_max + step).is_err());
    assert_eq!(v.get(p), Some(hard_max));
}

#[test]
fn trap_row_default_trap_width() {
    assert_row(TrapParameter::DefaultTrapWidth, "default_trap_width", 0.0, 8.0, "pt", None);
}
#[test]
fn trap_row_black_width() {
    assert_row(TrapParameter::BlackWidth, "black_width", 0.0, 8.0, "pt", None);
}
#[test]
fn trap_row_black_color_threshold() {
    assert_row(TrapParameter::BlackColorThreshold, "black_color_threshold", 0.0, 100.0, "percent", None);
}
/// The table says hard_max 10; the second capture says 12.0 and STU-LAY-103 makes 12.0 normative
/// while recording both.
#[test]
fn trap_row_black_density_with_capture_conflict() {
    assert_row(TrapParameter::BlackDensity, "black_density", 0.001, 12.0, "ratio", Some(3));
    assert_eq!(TrapParameter::BlackDensity.captured_hard_max(), &[10.0, 12.0]);
    assert_eq!(TrapParameter::BlackDensity.check(11.5), Ok(11.5), "between the two captures: admitted by the normative 12.0");
    // Precision 3: three decimal places admitted, four refused.
    assert_eq!(TrapParameter::BlackDensity.check(1.123), Ok(1.123));
    assert_eq!(TrapParameter::BlackDensity.check(0.123), Ok(0.123));
    assert_eq!(TrapParameter::BlackDensity.check(1.1234), Err(TrapPresetError::ParameterPrecision(TrapParameter::BlackDensity)));
    assert_eq!(TrapParameter::BlackDensity.check(0.0), Err(TrapPresetError::ParameterOutOfBounds(TrapParameter::BlackDensity)));
    for p in TrapParameter::ALL.into_iter().filter(|p| *p != TrapParameter::BlackDensity) {
        assert_eq!(p.captured_hard_max(), &[p.bounds().hard_max], "{} has one capture", p.name());
    }
}
#[test]
fn trap_row_sliding_trap_threshold() {
    assert_row(TrapParameter::SlidingTrapThreshold, "sliding_trap_threshold", 0.0, 100.0, "percent", None);
}
#[test]
fn trap_row_step_threshold() {
    assert_row(TrapParameter::StepThreshold, "step_threshold", 1.0, 100.0, "percent", None);
}
#[test]
fn trap_row_color_reduction() {
    assert_row(TrapParameter::ColorReduction, "color_reduction", 0.0, 100.0, "percent", None);
}

#[test]
fn trap_table_has_seven_rows_in_order() {
    let names: Vec<&str> = TrapParameter::ALL.iter().map(|p| p.name()).collect();
    assert_eq!(
        names,
        ["default_trap_width", "black_width", "black_color_threshold", "black_density", "sliding_trap_threshold", "step_threshold", "color_reduction"]
    );
}

/// The per-unit width ranges of the spec are derived from the point bound: 0.1111 in, 2.822 mm,
/// 0.2822 cm, 0p8 picas. The capture states 0c7.507 ciceros; the shared converter (cicero = 12
/// Didot points of 0.376065 mm) derives 0c7.505, which is recorded as a spec gap in the lane notes,
/// not hidden by a tolerance.
#[test]
fn trap_widths_convert_from_canonical_points() {
    let round = |v: f64, places: i32| (v * 10f64.powi(places)).round() / 10f64.powi(places);
    for p in [TrapParameter::DefaultTrapWidth, TrapParameter::BlackWidth] {
        let at = |s| p.hard_bounds_in(s).unwrap();
        assert_eq!(at(MeasureSystem::Points), (0.0, 8.0));
        assert_eq!(round(at(MeasureSystem::Inches).1, 4), 0.1111);
        assert_eq!(round(at(MeasureSystem::Millimeters).1, 3), 2.822);
        assert_eq!(round(at(MeasureSystem::Centimeters).1, 4), 0.2822);
        assert_eq!(round(at(MeasureSystem::Picas).1 * 12.0, 6), 8.0, "0p8: eight points of a pica");
        let didots = at(MeasureSystem::Ciceros).1 * 12.0;
        assert_eq!(round(didots, 3), 7.505, "derived from 0.376065 mm Didot points");
        assert!(at(MeasureSystem::Ciceros).0 == 0.0);
    }
    assert_eq!(TrapParameter::BlackDensity.hard_bounds_in(MeasureSystem::Millimeters), None, "not a length");
    assert_eq!(TrapParameter::BlackWidth.hard_bounds_in(MeasureSystem::Custom(0.0)), None);
    assert_eq!(TrapParameter::BlackWidth.hard_bounds_in(MeasureSystem::Custom(f64::NAN)), None);
}

#[test]
fn trap_enumerations_round_trip_exact_codes() {
    let ends: Vec<&str> = TrapEndType::ALL.iter().map(|m| m.code()).collect();
    assert_eq!(ends, ["trap_end.miter", "trap_end.overlap"]);
    let images: Vec<&str> = TrapImagePlacement::ALL.iter().map(|m| m.code()).collect();
    assert_eq!(
        images,
        ["trap_image.center_edges", "trap_image.choke", "trap_image.image_neutral_density", "trap_image.images_over_spread"]
    );
    for m in TrapEndType::ALL {
        assert_eq!(TrapEndType::from_code(m.code()), Ok(m));
    }
    for m in TrapImagePlacement::ALL {
        assert_eq!(TrapImagePlacement::from_code(m.code()), Ok(m));
    }
    for bad in ["TRAP_END.MITER", " trap_end.miter", "trap_end.round", "miter", ""] {
        assert_eq!(TrapEndType::from_code(bad), Err(TrapPresetError::UnknownMember("trap_end_type")), "{bad:?}");
    }
    assert_eq!(TrapImagePlacement::from_code("trap_image.spread"), Err(TrapPresetError::UnknownMember("trap_image_placement")));
}

#[test]
fn trap_preset_record_validates_name_and_values() {
    let mut values = TrapPresetValues::default();
    values.set(TrapParameter::DefaultTrapWidth, 0.25).unwrap();
    values.set(TrapParameter::StepThreshold, 10.0).unwrap();
    values.trap_end_type = Some(TrapEndType::Overlap);
    values.trap_image_placement = Some(TrapImagePlacement::Choke);
    let preset = TrapPreset::new("Newsprint", values.clone()).unwrap();
    assert_eq!(preset.values, values);
    assert_eq!(preset.values.black_width, None, "unset stays unset: no default supplied");
    assert_eq!(TrapPreset::new("  ", values.clone()), Err(TrapPresetError::NameEmpty));
    assert_eq!(TrapPreset::new(&"n".repeat(MAX_TRAP_PRESET_NAME_BYTES + 1), values.clone()), Err(TrapPresetError::NameTooLong));
    let mut hostile = values;
    hostile.step_threshold = Some(0.5);
    assert_eq!(TrapPreset::new("x", hostile.clone()), Err(TrapPresetError::ParameterOutOfBounds(TrapParameter::StepThreshold)));
    hostile.step_threshold = None;
    hostile.black_density = Some(f64::NEG_INFINITY);
    assert_eq!(hostile.validate(), Err(TrapPresetError::ParameterNotFinite(TrapParameter::BlackDensity)));
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn trap_refusals_report_through_observe_codes_only() {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    let mut observe = Observe::new(1, 3, resource, actor, Budget::new(0, 0).unwrap());
    let refused: Result<TrapPreset, LayoutError> =
        TrapPreset::new("", TrapPresetValues::default()).map_err(TrapPresetError::layout_error);
    let mut sink = Collect(Vec::new());
    let receipt = report(&refused, 1, 3, &CancellationToken::default(), &mut observe, &mut sink).unwrap();
    assert_eq!(receipt.outcome, Outcome::Failure(FailureCode::Validation));
    assert_eq!(TrapPresetError::NameTooLong.layout_error(), LayoutError::BudgetExceeded);
}
