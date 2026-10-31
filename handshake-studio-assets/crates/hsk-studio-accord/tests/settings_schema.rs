//! Lane SMB-SETTINGS-SCHEMA-1 (RF-09 phase 0): the typed settings schema (SET-REC-001..004,
//! GUI-DESKTOP-014). Handshake reference (read-only): `PreferenceSchemaEntry::validate` codes and
//! the registry invariants (unique ids, every default validates its own constraint).
use hsk_studio_accord::{
    BoundSource, MAX_SETTINGS_ENTRIES, Redaction, SETTINGS_NAMESPACE_STATUS, SchemaRefusal, SettingConstraint, SettingEntry,
    SettingId, SettingScope, SettingValue, SettingsError, SettingsSchema,
};

const MEMBERS: &[&str] = &["four", "eight"];

const fn entry(id: &'static str, module: &'static str, feature: &'static str, constraint: SettingConstraint, default: SettingValue<'static>) -> SettingEntry {
    SettingEntry {
        id,
        module,
        feature,
        label: "Label",
        unit: None,
        constraint,
        default,
        scope: SettingScope::Workspace,
        redaction: Redaction::Public,
        bound_source: BoundSource::StudioDeclared,
    }
}

const FLAG: SettingEntry = entry("studio.pigment.selection.flag", "pigment", "selection", SettingConstraint::Bool, SettingValue::Bool(true));
const COUNT: SettingEntry =
    entry("studio.pigment.selection.count", "pigment", "selection", SettingConstraint::IntRange { min: 1, max: 9 }, SettingValue::Int(3));
const LEVEL: SettingEntry =
    entry("studio.develop.export.level", "develop", "export", SettingConstraint::FloatRange { min: 0.0, max: 2.5 }, SettingValue::Float(0.5));
const MODE: SettingEntry = entry("studio.pigment.brush.mode-2", "pigment", "brush", SettingConstraint::Enum(MEMBERS), SettingValue::Str("four"));

fn refusal(groups: &[&[SettingEntry]]) -> SchemaRefusal {
    SettingsSchema::new(groups).unwrap_err()
}

#[test]
fn setting_id_grammar_refuses_every_malformed_id() {
    let id = SettingId::parse("studio.harness-view.brush.diameter-2").unwrap();
    assert_eq!((id.module(), id.feature(), id.key()), ("harness-view", "brush", "diameter-2"));
    let long = format!("studio.a.b.{}", "k".repeat(118));
    assert_eq!(long.len(), 129);
    for bad in [
        "",
        "studio",
        "studio.a.b",
        "studio.a.b.c.d",
        "view-defaults.a.b.c",
        "Studio.a.b.c",
        "studio.A.b.c",
        "studio..b.c",
        "studio.a.b.",
        "studio.a-.b.c",
        "studio.-a.b.c",
        "studio.1a.b.c",
        "studio.a_b.c.d",
        "studio.a.b.c d",
        "studio.a.b.c\u{0}",
        "studio.a.b.\u{e9}",
        long.as_str(),
    ] {
        assert_eq!(SettingId::parse(bad).unwrap_err(), SettingsError::InvalidId, "{bad:?}");
    }
    assert!(SettingId::parse(&long[..128]).is_ok(), "128 bytes is inside the accord id bound");
}

#[test]
fn schema_build_refuses_typed_before_admitting_an_entry() {
    let ok = SettingsSchema::new(&[&[FLAG, COUNT], &[LEVEL, MODE]]).unwrap();
    assert_eq!(ok.entries().len(), 4);
    let code = |groups: &[&[SettingEntry]]| refusal(groups).error.code();
    // Entry count is checked first, before any entry (even an invalid one) is read.
    let bad_id = SettingEntry { id: "studio.BAD.x.y", ..FLAG };
    assert_eq!(
        SettingsSchema::with_limit(&[&[FLAG, COUNT], &[bad_id]], 2).unwrap_err(),
        SchemaRefusal { id: None, error: SettingsError::TooManyEntries }
    );
    assert_eq!(SettingsSchema::with_limit(&[&[FLAG, COUNT]], 2).unwrap().entries().len(), 2);
    assert_eq!(SettingsSchema::with_limit(&[&[FLAG]], usize::MAX).unwrap().entries().len(), 1, "a limit above the max is the max");
    let many: Vec<SettingEntry> = (0..=MAX_SETTINGS_ENTRIES)
        .map(|i| SettingEntry { id: Box::leak(format!("studio.m.f.k{i}").into_boxed_str()), module: "m", feature: "f", ..FLAG })
        .collect();
    assert_eq!(code(&[&many]), "too_many_entries");
    assert!(SettingsSchema::new(&[&many[..MAX_SETTINGS_ENTRIES]]).is_ok());
    assert_eq!(code(&[&[bad_id]]), "invalid_id");
    assert_eq!(code(&[&[SettingEntry { module: "develop", ..FLAG }]]), "id_mismatch");
    assert_eq!(code(&[&[SettingEntry { feature: "export", ..FLAG }]]), "id_mismatch");
    assert_eq!(refusal(&[&[FLAG], &[COUNT, FLAG]]), SchemaRefusal { id: Some(FLAG.id), error: SettingsError::DuplicateId });
    assert_eq!(code(&[&[SettingEntry { label: "", ..FLAG }]]), "invalid_label");
    assert_eq!(code(&[&[SettingEntry { label: "a\nb", ..FLAG }]]), "invalid_label");
    assert_eq!(code(&[&[SettingEntry { unit: Some(""), ..FLAG }]]), "invalid_unit");
    assert_eq!(code(&[&[SettingEntry { constraint: SettingConstraint::IntRange { min: 9, max: 1 }, ..COUNT }]]), "invalid_range");
    for (min, max) in [(2.0, 1.0), (f64::NAN, 1.0), (0.0, f64::INFINITY), (f64::NEG_INFINITY, 0.0)] {
        assert_eq!(code(&[&[SettingEntry { constraint: SettingConstraint::FloatRange { min, max }, ..LEVEL }]]), "invalid_range", "{min} {max}");
    }
    assert_eq!(code(&[&[SettingEntry { constraint: SettingConstraint::Enum(&[]), ..MODE }]]), "empty_enum");
    assert_eq!(code(&[&[SettingEntry { constraint: SettingConstraint::Enum(&["four", ""]), ..MODE }]]), "invalid_enum_member");
    assert_eq!(code(&[&[SettingEntry { constraint: SettingConstraint::Enum(&["four", "eight", "four"]), ..MODE }]]), "duplicate_enum_member");
    // Every default validates its own constraint, with exactly the constraint's kind.
    for e in [
        SettingEntry { default: SettingValue::Int(10), ..COUNT },
        SettingEntry { default: SettingValue::Int(0), ..COUNT },
        SettingEntry { default: SettingValue::Float(3.0), ..COUNT },
        SettingEntry { default: SettingValue::Int(1), ..LEVEL },
        SettingEntry { default: SettingValue::Float(f64::NAN), ..LEVEL },
        SettingEntry { default: SettingValue::Float(2.6), ..LEVEL },
        SettingEntry { default: SettingValue::Str("three"), ..MODE },
        SettingEntry { default: SettingValue::Str("Four"), ..MODE },
        SettingEntry { default: SettingValue::Int(1), ..FLAG },
    ] {
        assert_eq!(refusal(&[&[e]]), SchemaRefusal { id: Some(e.id), error: SettingsError::InvalidDefault }, "{e:?}");
    }
}

#[test]
fn validate_answers_handshake_codes_and_never_coerces() {
    let s = SettingsSchema::new(&[&[FLAG, COUNT, LEVEL, MODE]]).unwrap();
    let code = |id: &str, v: SettingValue<'_>| s.validate(id, &v).map_err(|r| (r.id.to_owned(), r.code()));
    let fail = |id: &str, v: SettingValue<'_>| code(id, v).unwrap_err().1;
    assert_eq!(code("studio.pigment.selection.nope", SettingValue::Bool(true)), Err(("studio.pigment.selection.nope".into(), "unknown_setting")));
    assert_eq!(fail("not-an-id", SettingValue::Bool(true)), "unknown_setting");
    assert_eq!(code(FLAG.id, SettingValue::Bool(false)), Ok(()));
    assert_eq!(fail(FLAG.id, SettingValue::Int(1)), "not_bool");
    assert_eq!(fail(FLAG.id, SettingValue::Str("true")), "not_bool");
    assert_eq!(code(COUNT.id, SettingValue::Int(1)), Ok(()));
    assert_eq!(code(COUNT.id, SettingValue::Int(9)), Ok(()));
    assert_eq!(fail(COUNT.id, SettingValue::Int(0)), "out_of_range");
    assert_eq!(fail(COUNT.id, SettingValue::Int(10)), "out_of_range");
    assert_eq!(fail(COUNT.id, SettingValue::Int(i64::MIN)), "out_of_range");
    assert_eq!(fail(COUNT.id, SettingValue::Float(3.0)), "not_int", "a float is never an integer value");
    assert_eq!(code(LEVEL.id, SettingValue::Float(0.0)), Ok(()));
    assert_eq!(code(LEVEL.id, SettingValue::Float(2.5)), Ok(()));
    assert_eq!(code(LEVEL.id, SettingValue::Int(2)), Ok(()), "an integer is a float value (Handshake as_f64)");
    assert_eq!(fail(LEVEL.id, SettingValue::Int(3)), "out_of_range");
    assert_eq!(fail(LEVEL.id, SettingValue::Float(-0.000_001)), "out_of_range");
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(fail(LEVEL.id, SettingValue::Float(v)), "not_finite");
    }
    assert_eq!(fail(LEVEL.id, SettingValue::Bool(true)), "not_float");
    assert_eq!(fail(LEVEL.id, SettingValue::Str("1")), "not_float");
    assert_eq!(code(MODE.id, SettingValue::Str("eight")), Ok(()));
    for m in ["Eight", " eight", "eight ", ""] {
        assert_eq!(fail(MODE.id, SettingValue::Str(m)), "unknown_enum_member", "{m:?}");
    }
    assert_eq!(fail(MODE.id, SettingValue::Int(0)), "not_string");
}

#[test]
fn tree_groups_module_tabs_and_feature_subtabs_in_declaration_order() {
    let s = SettingsSchema::new(&[&[FLAG, LEVEL, MODE, COUNT]]).unwrap();
    let tree = s.tree();
    type Tabs<'a> = Vec<(&'a str, Vec<(&'a str, Vec<&'a str>)>)>;
    let shape: Tabs<'_> =
        tree.iter().map(|m| (m.module, m.features.iter().map(|f| (f.feature, f.entries.clone())).collect())).collect();
    assert_eq!(
        shape,
        vec![
            ("pigment", vec![("selection", vec![FLAG.id, COUNT.id]), ("brush", vec![MODE.id])]),
            ("develop", vec![("export", vec![LEVEL.id])]),
        ]
    );
    let walked: usize = tree.iter().flat_map(|m| &m.features).map(|f| f.entries.len()).sum();
    assert_eq!(walked, s.entries().len());
    assert!(SettingsSchema::new(&[]).unwrap().tree().is_empty());
}

#[test]
fn preference_projection_is_deterministic_with_handshake_field_names() {
    let unit = SettingEntry { unit: Some("px"), label: "Quote \" and \\ slash", bound_source: BoundSource::Spec, ..COUNT };
    let a = SettingsSchema::new(&[&[unit, LEVEL, MODE, FLAG]]).unwrap();
    let b = SettingsSchema::new(&[&[unit], &[LEVEL, MODE], &[FLAG]]).unwrap();
    assert_eq!(a.to_preference_json(), b.to_preference_json());
    assert_eq!(SETTINGS_NAMESPACE_STATUS, "spec_delta_pending");
    let expected = concat!(
        r#"{"entries":["#,
        r#"{"bound_source":"spec","constraint":{"kind":"int-range","max":9,"min":1},"default_value":3,"feature":"selection","#,
        r#""label":"Quote \" and \\ slash","module":"pigment","namespace":"studio","preference_id":"studio.pigment.selection.count","#,
        r#""redaction_class":"public","scope_kind":"workspace","unit":"px","value_type":"int"},"#,
        r#"{"bound_source":"studio_declared","constraint":{"kind":"float-range","max":2.5,"min":0},"default_value":0.5,"feature":"export","#,
        r#""label":"Label","module":"develop","namespace":"studio","preference_id":"studio.develop.export.level","#,
        r#""redaction_class":"public","scope_kind":"workspace","unit":null,"value_type":"float"},"#,
        r#"{"bound_source":"studio_declared","constraint":{"kind":"enum","members":["four","eight"]},"default_value":"four","feature":"brush","#,
        r#""label":"Label","module":"pigment","namespace":"studio","preference_id":"studio.pigment.brush.mode-2","#,
        r#""redaction_class":"public","scope_kind":"workspace","unit":null,"value_type":"enum"},"#,
        r#"{"bound_source":"studio_declared","constraint":{"kind":"bool"},"default_value":true,"feature":"selection","#,
        r#""label":"Label","module":"pigment","namespace":"studio","preference_id":"studio.pigment.selection.flag","#,
        r#""redaction_class":"public","scope_kind":"workspace","unit":null,"value_type":"bool"}"#,
        r#"],"namespace":"studio","namespace_status":"spec_delta_pending"}"#,
    );
    assert_eq!(a.to_preference_json(), expected);
    let surface = SettingEntry { scope: SettingScope::Surface, redaction: Redaction::NonPublic, ..FLAG };
    let p = SettingsSchema::new(&[&[surface]]).unwrap().to_preference_json();
    assert!(p.contains(r#""redaction_class":"non-public""#) && p.contains(r#""scope_kind":"surface""#), "{p}");
    assert_eq!(SettingsSchema::new(&[]).unwrap().to_preference_json(), r#"{"entries":[],"namespace":"studio","namespace_status":"spec_delta_pending"}"#);
}
