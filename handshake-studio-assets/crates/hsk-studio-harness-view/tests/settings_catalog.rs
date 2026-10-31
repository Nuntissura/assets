//! Lane SMB-SETTINGS-SCHEMA-1 slice 4 (RF-09 phase 0, asset side of MT-36249 AC-HOST-001): brush
//! entries beside `BrushSettings` and the Studio settings catalog (module tab, feature subtab,
//! route walk, audit through observe). Round unit `["viewer"]` (`.config/round-features.toml`).
#![cfg(feature = "viewer")]
use hsk_studio_accord::{
    ActorContext, BoundSource, DomainId, SettingConstraint, SettingEntry, SettingValue, SettingsSchema,
};
use hsk_studio_harness_view::view::edit::*;
use hsk_studio_harness_view::view::settings_catalog::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, Outcome, SinkPort};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::{BrushDynamics, BrushLimits, BrushTip, Channel, Control, StrokeSample, place_dabs, stroke_coverage};

#[derive(Default)]
struct Capture(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for Capture {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

fn resource() -> DomainId {
    DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap()
}

#[test]
fn catalog_has_one_module_tab_per_module_and_every_entry_has_its_route() {
    let schema = studio_settings_catalog().unwrap();
    let total: usize = CATALOG_GROUPS.iter().map(|g| g.len()).sum();
    assert_eq!((schema.entries().len(), total), (35, 35));
    let tree = schema.tree();
    let tabs: Vec<(&str, Vec<&str>)> = tree.iter().map(|m| (m.module, m.features.iter().map(|f| f.feature).collect())).collect();
    assert_eq!(tabs, vec![("harness-view", vec!["brush"]), ("pigment", vec!["selection"]), ("develop", vec!["export"])]);
    for e in schema.entries() {
        let route = find_route(&tree, e.id).unwrap_or_else(|| panic!("{} has no route", e.id));
        assert_eq!(route, SettingsRoute { module: e.module, feature: e.feature, id: e.id });
    }
    for id in [
        "",
        "studio.harness-view.brush",
        "studio.harness-view.brush.nope",
        "studio.harness-view.selection.diameter",
        "studio.pigment.brush.diameter",
        "studio.Pigment.selection.wand-tolerance",
        "view-defaults.harness-view.brush.diameter",
    ] {
        assert_eq!(find_route(&tree, id), None, "{id:?}");
    }
    let json = schema.to_preference_json();
    assert_eq!(json, studio_settings_catalog().unwrap().to_preference_json(), "deterministic");
    assert!(json.ends_with(r#""namespace":"studio","namespace_status":"spec_delta_pending"}"#), "{json}");
    for e in schema.entries() {
        assert_eq!(json.matches(&format!(r#""preference_id":"{}""#, e.id)).count(), 1, "{}", e.id);
    }
    assert_eq!(schema.validate("studio.harness-view.brush.colour", &SettingValue::Int(0)).unwrap_err().code(), "unknown_setting");
}

#[test]
fn brush_entry_defaults_match_brush_settings_field_for_field() {
    // Exhaustive destructuring: a new BrushSettings field fails to compile here until it is decided.
    let BrushSettings { diameter_px, hardness_pct, spacing_pct, opacity_pct, rgb8: _no_entry_colour } = BrushSettings::default();
    let d = |id: &str| BRUSH_SETTING_ENTRIES.iter().find(|e| e.id == id).unwrap().default;
    assert_eq!(d(BRUSH_DIAMETER), SettingValue::Float(diameter_px));
    assert_eq!(d(BRUSH_HARDNESS), SettingValue::Float(hardness_pct));
    assert_eq!(d(BRUSH_SPACING), SettingValue::Float(spacing_pct));
    assert_eq!(d(BRUSH_OPACITY), SettingValue::Float(opacity_pct));
    let values = brush_values(&BrushSettings::default());
    assert_eq!(values.len(), BRUSH_SETTING_ENTRIES.len());
    for (e, (id, v)) in BRUSH_SETTING_ENTRIES.iter().zip(values) {
        assert_eq!((e.id, e.default), (id, v));
        assert_eq!((e.module, e.feature, e.bound_source), ("harness-view", "brush", BoundSource::StudioDeclared));
    }
    let schema = SettingsSchema::new(&[BRUSH_SETTING_ENTRIES]).unwrap();
    for (id, v, code) in [
        (BRUSH_DIAMETER, SettingValue::Float(0.5), "out_of_range"),
        (BRUSH_DIAMETER, SettingValue::Float(5000.5), "out_of_range"),
        (BRUSH_SPACING, SettingValue::Float(0.0), "out_of_range"),
        (BRUSH_OPACITY, SettingValue::Float(f64::NAN), "not_finite"),
        (BRUSH_HARDNESS, SettingValue::Bool(true), "not_float"),
    ] {
        assert_eq!(schema.validate(id, &v).unwrap_err().code(), code, "{id} {v:?}");
    }
}

#[test]
fn every_brush_bound_value_is_accepted_by_the_pigment_brush() {
    let samples = [
        StrokeSample { x: 10.0, y: 10.0, pressure: None, time_ms: None },
        StrokeSample { x: 40.0, y: 30.0, pressure: Some(0.5), time_ms: None },
    ];
    let canvas = Rect { x: 0, y: 0, width: 64, height: 64 };
    let limits = BrushLimits::default();
    let mut fed = 0;
    for e in BRUSH_SETTING_ENTRIES {
        let SettingConstraint::FloatRange { min, max } = e.constraint else { panic!("{} is not a float range", e.id) };
        for v in [min, max] {
            let mut b = BrushSettings::default();
            match e.id {
                BRUSH_DIAMETER => b.diameter_px = v,
                BRUSH_HARDNESS => b.hardness_pct = v,
                BRUSH_SPACING => b.spacing_pct = v,
                BRUSH_OPACITY => b.opacity_pct = v,
                other => panic!("unmapped brush entry {other}"),
            }
            // The paint path of `Viewer` (edit.rs): tip, pressure-driven size, opacity channel.
            let tip = BrushTip::round(b.diameter_px, b.hardness_pct, b.spacing_pct);
            let dynamics = BrushDynamics {
                size: Channel { base_value: 100.0, minimum: 0.0, jitter: 0.0, control: Control::PenPressure },
                opacity: Channel { base_value: b.opacity_pct, ..Channel::off() },
                flow: Channel::off(),
            };
            let dabs = place_dabs(&tip, &dynamics, &samples, &limits).unwrap_or_else(|err| panic!("{} {v}: {}", e.id, err.code()));
            stroke_coverage(&dabs, tip.hardness_pct, canvas, &limits).unwrap_or_else(|err| panic!("{} {v}: {}", e.id, err.code()));
            fed += 1;
        }
    }
    assert_eq!(fed, 2 * BRUSH_SETTING_ENTRIES.len());
}

#[test]
fn catalog_audit_reports_codes_and_counters_through_observe() {
    let audit = audit_catalog();
    assert_eq!(audit, CatalogAudit { modules: 3, features: 3, entries: 35, found: 35, schema_refused: false, code: None });
    let mut sink = Capture::default();
    let receipt = report_catalog_audit(&audit, 7, 3, resource(), actor(), &mut sink).unwrap();
    assert_eq!((receipt.correlation_id, receipt.revision, receipt.outcome), (7, 3, Outcome::Success));
    assert_eq!(sink.0.len(), 1);
    assert_eq!(sink.0[0].0, DeliveryClass::Terminal);
    // Codes and counters only: no label, id or other text of the catalog reaches the frame.
    let frame = &sink.0[0].1;
    for text in ["Brush", "studio.", "harness-view", "pigment", "export"] {
        assert!(!frame.windows(text.len()).any(|w| w == text.as_bytes()), "{text} in the frame");
    }

    // A broken catalog: the duplicate id is refused typed, reported as a validation failure.
    let dup: &[SettingEntry] = &[BRUSH_SETTING_ENTRIES[0]];
    let bad = audit_groups(&[BRUSH_SETTING_ENTRIES, dup]);
    assert_eq!(bad, CatalogAudit { modules: 0, features: 0, entries: 5, found: 0, schema_refused: true, code: Some("duplicate_id") });
    let mut sink = Capture::default();
    let receipt = report_catalog_audit(&bad, 8, 3, resource(), actor(), &mut sink).unwrap();
    assert!(matches!(receipt.outcome, Outcome::Failure(_)), "{receipt:?}");
    assert_eq!((sink.0.len(), sink.0[0].0), (1, DeliveryClass::Terminal));
    assert_ne!(sink.0[0].1, frame.clone(), "the failure frame differs from the success frame");
    let empty = audit_groups(&[]);
    assert_eq!(empty, CatalogAudit { modules: 0, features: 0, entries: 0, found: 0, schema_refused: false, code: None });
}
