//! Lane SMB-AE-EFFECT-STACK-1: `StudioCompositionLayer::effects` (STU-FX-002 effect stack,
//! STU-FX-103..107 parameter rows, STU-FX-131 records of the implemented distort and generate
//! kinds). Records without a stack serialise byte for byte as before; with one they round trip;
//! values outside a declared hard bound are refused typed (STU-FX-104), values outside only the soft
//! range are legal (STU-FX-105); every other hostile shape is refused typed.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}
fn document() -> StudioDocument {
    let layers = vec![json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR", 1),"name":"l1",
        "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true})];
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn records(layer: Value) -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":"main","name":"main","width":64,"height":48,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},
         "duration":254016000000u64,"layers":[layer]}]})
}
fn layer() -> Value {
    json!({"layer_id":identity("SLYR",1),
        "timing":{"in_point":0,"out_point":254016000000u64,"start_time":0,"stretch":{"numerator":1,"denominator":1}}})
}
fn with_effects(effects: Value) -> Value {
    let mut l = layer();
    l["effects"] = effects;
    records(l)
}
fn check(v: &Value) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let r: StudioCompositionRecords = serde_json::from_value(v.clone()).expect("record shape");
    validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller)
}
fn refusal(v: &Value) -> String {
    let e = check(v).unwrap_err();
    format!("{:?}:{}", e.code, e.target)
}
fn constant(v: Value) -> Value {
    json!({ "static_value": v })
}
fn stack() -> Value {
    json!([
        {"effect_id":"twirl-1","filter_kind":"twirl","values":{"angle":constant(json!([-720.0])),"twirl_radius":constant(json!([50.0]))}},
        {"effect_id":"ramp","filter_kind":"gradient_ramp","enabled":false,"opacity":40.0,
         "values":{"start_color":constant(json!([1.0,0.5,0.0,1.0])),"ramp_shape":constant(json!([2.0]))}},
        {"effect_id":"circle","filter_kind":"circle","solo":true,"values":{
            "radius":{"static_value":[12.0],"keyframes":[
                {"time":0,"value":[12.0],"in_interp":"linear","out_interp":"linear"},
                {"time":127008000000u64,"value":[30.0],"in_interp":"linear","out_interp":"linear"}]},
            "center":constant(json!([0.25,0.75])),"blending_mode":constant(json!([2.0]))}},
        {"effect_id":"sphere","filter_kind":"spherize","values":{"radius":constant(json!([2000.0]))}}
    ])
}

#[test]
fn effect_stack_records_round_trip_and_an_absent_stack_is_byte_identical() {
    // Without a stack: no `effects` key on the wire, bytes unchanged.
    let plain = records(layer());
    let r: StudioCompositionRecords = serde_json::from_value(plain.clone()).unwrap();
    assert!(r.compositions[0].layers[0].effects.is_empty());
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("effects"));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), plain);
    check(&plain).unwrap();
    // With one: valid (a Spherize radius of 2000 lies outside the soft range 0..250 and inside the
    // hard range 0..2500: legal, STU-FX-105), byte-identical round trip, order kept, defaults
    // (`enabled` true, `solo` false, `opacity` 100) not written.
    let with = with_effects(stack());
    check(&with).unwrap();
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    let e = &r.compositions[0].layers[0].effects;
    let ids: Vec<&str> = e.iter().map(|x| x.effect_id.as_str()).collect();
    assert_eq!(ids, ["twirl-1", "ramp", "circle", "sphere"]);
    assert!(e[0].enabled && !e[0].solo && e[0].opacity == 100.0);
    assert!(!e[1].enabled && e[1].opacity == 40.0);
    assert!(e[2].solo);
    assert_eq!(e[0].static_or_default("twirl_center"), Some(vec![0.5, 0.5]));
    assert_eq!(e[1].static_or_default("end_color"), Some(vec![1.0, 1.0, 1.0, 1.0]));
    assert_eq!(e[0].static_or_default("no_such_row"), None);
    let fresh = StudioCompositionEffect::new("x", StudioCompositionEffectKind::Checkerboard);
    let wire = serde_json::to_value(&fresh).unwrap();
    assert_eq!(wire, json!({"effect_id":"x","filter_kind":"checkerboard"}));
}

#[test]
fn parameter_rows_keep_hard_and_soft_bounds_apart_and_defaults_are_legal() {
    // STU-FX-131 rows reproduced (spot checks of the recovered tables) and STU-FX-107: four
    // separate bound fields.
    let k = StudioCompositionEffectKind::Spherize;
    let r = k.param("radius").unwrap();
    assert_eq!((r.hard_min, r.hard_max, r.soft_min, r.soft_max), (Some(0.0), Some(2500.0), Some(0.0), Some(250.0)));
    assert_eq!((r.param_index, r.precision, r.bound_state), (1, Some(1), StudioBoundState::DeclaredBoth));
    let c = StudioCompositionEffectKind::Circle.param("radius").unwrap();
    assert_eq!((c.hard_min, c.hard_max, c.soft_min, c.soft_max, c.default), (Some(-10000.0), Some(10000.0), Some(0.0), Some(600.0), &[75.0][..]));
    let b = StudioCompositionEffectKind::Checkerboard.param("blending_mode").unwrap();
    assert_eq!(b.options.len(), 19);
    assert!(b.options.iter().all(|(i, _)| ![4, 11, 14, 19].contains(i)), "source gaps kept");
    let a = StudioCompositionEffectKind::Twirl.param("angle").unwrap();
    assert_eq!((a.hard_min, a.hard_max, a.bound_state, a.unit), (None, None, StudioBoundState::UnboundedInSource, StudioUnitToken::Degrees));
    let p = StudioCompositionEffectKind::GradientRamp.param("start_of_ramp").unwrap();
    assert_eq!((p.kind, p.default), (StudioParameterKind::Point2d, &[0.5, 0.0][..]));
    for kind in StudioCompositionEffectKind::ALL {
        assert!(!kind.import_key().is_empty() && !kind.wire().is_empty());
        for row in kind.params() {
            assert!(row.accepts(row.default), "{} {}", kind.wire(), row.param_key);
            if row.kind == StudioParameterKind::Enumeration {
                assert!(!row.interpolable && !row.options.is_empty());
            }
        }
    }
    assert_eq!(StudioCompositionEffectKind::Twirl.category(), StudioEffectCategory::Distort);
    assert_eq!(StudioCompositionEffectKind::Checkerboard.category(), StudioEffectCategory::Generate);
}

#[test]
fn hostile_effect_records_are_refused_typed() {
    let one = |e: Value| with_effects(json!([e]));
    let t = identity("SLYR", 1);
    // Outside a declared hard bound: refused, never clamped (STU-FX-104).
    let over = one(json!({"effect_id":"t","filter_kind":"twirl","values":{"twirl_radius":constant(json!([100.5]))}}));
    assert_eq!(refusal(&over), format!("InvalidField:{t}/effects/t/twirl_radius/effect_param_out_of_range"));
    let key_over = one(json!({"effect_id":"s","filter_kind":"spherize","values":{"radius":{"static_value":[1.0],"keyframes":[
        {"time":0,"value":[1.0],"in_interp":"linear","out_interp":"linear"},
        {"time":10,"value":[2500.5],"in_interp":"linear","out_interp":"linear"}]}}}));
    assert_eq!(refusal(&key_over), format!("InvalidField:{t}/effects/s/radius/effect_param_out_of_range"));
    let low = one(json!({"effect_id":"c","filter_kind":"checkerboard","values":{"width":constant(json!([0.5]))}}));
    assert_eq!(refusal(&low), format!("InvalidField:{t}/effects/c/width/effect_param_out_of_range"));
    // An enumeration index that is a gap in the source numbering, or fractional.
    let gap = one(json!({"effect_id":"c","filter_kind":"circle","values":{"blending_mode":constant(json!([4.0]))}}));
    assert_eq!(refusal(&gap), format!("InvalidField:{t}/effects/c/blending_mode/effect_param_out_of_range"));
    let frac = one(json!({"effect_id":"p","filter_kind":"polar_coordinates","values":{"type_of_conversion":constant(json!([1.5]))}}));
    assert_eq!(refusal(&frac), format!("InvalidField:{t}/effects/p/type_of_conversion/effect_param_out_of_range"));
    // An enumeration keyed with a non-hold interpolation.
    let lerped = one(json!({"effect_id":"p","filter_kind":"polar_coordinates","values":{"type_of_conversion":{"static_value":[1.0],"keyframes":[
        {"time":0,"value":[1.0],"in_interp":"linear","out_interp":"linear"},
        {"time":10,"value":[2.0],"in_interp":"linear","out_interp":"linear"}]}}}));
    assert_eq!(refusal(&lerped), format!("InvalidField:{t}/effects/p/type_of_conversion/keyframes/interpolation"));
    // Shapes: unknown row, wrong component count, bad entry opacity, bad and duplicate ids.
    let unknown = one(json!({"effect_id":"t","filter_kind":"twirl","values":{"radius":constant(json!([1.0]))}}));
    assert_eq!(refusal(&unknown), format!("InvalidField:{t}/effects/t/values"));
    let shape = one(json!({"effect_id":"g","filter_kind":"gradient_ramp","values":{"start_color":constant(json!([1.0,0.0,0.0]))}}));
    assert_eq!(refusal(&shape), format!("InvalidField:{t}/effects/g/start_color/static_value"));
    let opacity = one(json!({"effect_id":"t","filter_kind":"twirl","opacity":100.5}));
    assert_eq!(refusal(&opacity), format!("InvalidField:{t}/effects/t/opacity"));
    let bad_id = one(json!({"effect_id":"has space","filter_kind":"twirl"}));
    assert_eq!(refusal(&bad_id), format!("InvalidField:{t}/effects/effect_id"));
    let long_id = one(json!({"effect_id":"x".repeat(65),"filter_kind":"twirl"}));
    assert_eq!(refusal(&long_id), format!("InvalidField:{t}/effects/effect_id"));
    let dup = with_effects(json!([{"effect_id":"a","filter_kind":"twirl"},{"effect_id":"a","filter_kind":"offset"}]));
    assert_eq!(refusal(&dup), format!("DuplicateIdentity:{t}/effects/a"));
    // Count ceiling, checked before any entry is examined.
    let many: Vec<Value> = (0..=MAX_LAYER_EFFECTS).map(|i| json!({"effect_id":format!("e{i}"),"filter_kind":"offset"})).collect();
    assert_eq!(refusal(&with_effects(Value::Array(many))), format!("BudgetExceeded:{t}/effects"));
    // An unknown kind or field is a shape refusal at decode.
    for bad in [json!({"effect_id":"a","filter_kind":"wave_warp"}), json!({"effect_id":"a","filter_kind":"twirl","render_scope":"below"})] {
        assert!(serde_json::from_value::<StudioCompositionRecords>(with_effects(json!([bad]))).is_err());
    }
}

/// Lane SMB-R43-FOLLOWUPS-1 (round finding F-R43-4, STU-FX-118 "Every `color` parameter carries a
/// required `StudioColorProfile` reference"): `StudioCompositionEffect::colour_profiles` maps a
/// colour row to its reference in the `StudioColor` shape. Additive: records without it keep their
/// bytes; a colour parameter without a reference loads (the renderer refuses it when the effect is
/// applied); a reference on a row that is not a colour, a bad id or a bad shape is refused typed.
#[test]
fn colour_parameters_carry_a_profile_reference_additively() {
    let one = |e: Value| with_effects(json!([e]));
    let t = identity("SLYR", 1);
    // No member: the colour value loads and the bytes round trip unchanged (no `colour_profiles` key).
    let bare = one(json!({"effect_id":"t","filter_kind":"tint","values":{"map_black_to":constant(json!([0.2,0.1,0.0,1.0]))}}));
    check(&bare).unwrap();
    let r: StudioCompositionRecords = serde_json::from_value(bare.clone()).unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("colour_profiles"));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), bare);
    // With references (an SCPF id, explicit null = the document's RGB working space): valid, byte-identical round trip.
    let p2 = identity("SCPF", 2);
    let with = one(json!({"effect_id":"t","filter_kind":"tint","values":{"map_black_to":constant(json!([0.2,0.1,0.0,1.0]))},
        "colour_profiles":{"map_black_to":{"colour_profile_id":p2},"map_white_to":{"colour_profile_id":null}}}));
    assert_eq!(check(&with).unwrap(), Vec::<ResolutionIssue>::new());
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let e = &r.compositions[0].layers[0].effects[0];
    assert_eq!(e.colour_profiles["map_black_to"].colour_profile_id, ProfileBinding::Assigned(p2.clone()));
    assert_eq!(e.colour_profiles["map_white_to"].colour_profile_id, ProfileBinding::Unassigned(()));
    assert!(StudioCompositionEffect::new("x", StudioCompositionEffectKind::Tint).colour_profiles.is_empty());
    let first = serde_json::to_vec(&r).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    // Every colour row of every kind takes one (Tritone, Glow A & B, the generators).
    let glow = one(json!({"effect_id":"g","filter_kind":"glow","colour_profiles":{
        "color_a":{"colour_profile_id":p2},"color_b":{"colour_profile_id":p2}}}));
    check(&glow).unwrap();
    for (kind, key) in [("tritone", "midtones"), ("circle", "color"), ("checkerboard", "color"), ("gradient_ramp", "end_color")] {
        check(&one(json!({"effect_id":"k","filter_kind":kind,"colour_profiles":{key:{"colour_profile_id":p2}}}))).unwrap();
    }
    // An unavailable profile is a resolution issue at the parameter's target, like every binding.
    struct NoProfiles;
    impl Resolver for NoProfiles {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Absent
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            true
        }
    }
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let issues = validate_composition_records(&r, &document(), Budget::default(), &CancellationToken::default(), &NoProfiles).unwrap();
    assert_eq!(issues, vec![ResolutionIssue { target: format!("{t}/effects/t/colour_profiles/map_black_to/colour_profile"), disposition: Resolution::Absent }]);
    // Hostile: a key that is not a colour row of the kind, an unknown key, an id of another kind.
    let scalar = one(json!({"effect_id":"t","filter_kind":"tint","colour_profiles":{"amount_to_tint":{"colour_profile_id":p2}}}));
    assert_eq!(refusal(&scalar), format!("InvalidField:{t}/effects/t/colour_profiles"));
    let unknown = one(json!({"effect_id":"t","filter_kind":"tint","colour_profiles":{"no_such_row":{"colour_profile_id":p2}}}));
    assert_eq!(refusal(&unknown), format!("InvalidField:{t}/effects/t/colour_profiles"));
    let wrong = one(json!({"effect_id":"t","filter_kind":"tint","colour_profiles":{"map_black_to":{"colour_profile_id":identity("SLYR",9)}}}));
    assert_eq!(refusal(&wrong), format!("InvalidId:{t}/effects/t/colour_profiles/map_black_to"));
    // Shapes refused at decode: a missing member (null must be explicit), an unknown member, a bare string.
    for bad in [json!({}), json!({"colour_profile_id":p2,"intent":"perceptual"}), json!(p2)] {
        let v = one(json!({"effect_id":"t","filter_kind":"tint","colour_profiles":{"map_black_to":bad}}));
        assert!(serde_json::from_value::<StudioCompositionRecords>(v).is_err(), "{bad}");
    }
}

// ---- Lane SMB-FOLIO-ANGLE-UNBOUNDED-1 (v02.228 SD-26, STU-FX-131a): recovered ANGLE rows ----
//
// An `ANGLE` row whose bound cells held numbers came from a binary parameter-definition record read
// at the wrong offsets, so those numbers were never source declarations. Every such row is
// `unbounded_in_source` with all four bounds `None` and an angle stores the full signed value in
// degrees, whole extra revolutions included (STU-FX-112): it is never wrapped or clamped on store.
// Defaults and precision are provisional (UNVERIFIED until the capture is re-decoded). Vendor
// behaviour is UNVERIFIED (D5).

/// Every angle row of the catalogue: `kind == angle` or `unit == degrees`.
fn angle_rows() -> Vec<(StudioCompositionEffectKind, &'static StudioEffectParamSpec)> {
    let mut rows = Vec::new();
    for kind in StudioCompositionEffectKind::ALL {
        for row in kind.params() {
            if row.kind == StudioParameterKind::Angle || row.unit == StudioUnitToken::Degrees {
                rows.push((kind, row));
            }
        }
    }
    rows
}

#[test]
fn no_angle_row_of_the_catalogue_carries_numeric_bounds() {
    let rows = angle_rows();
    let mut keys: Vec<String> = rows.iter().map(|(k, r)| format!("{}/{}", k.wire(), r.param_key)).collect();
    keys.sort();
    // The five STU-FX-131a ANGLE rows of the implemented kinds: nothing else is an angle, nothing is missing.
    assert_eq!(
        keys,
        ["directional_blur/direction", "glow/color_phase", "hue_saturation/colorize_hue", "hue_saturation/master_hue", "twirl/angle"],
        "the angle rows of the catalogue"
    );
    for (kind, row) in rows {
        let at = format!("{}/{}", kind.wire(), row.param_key);
        assert_eq!(row.kind, StudioParameterKind::Angle, "{at}: a degrees row is an angle");
        assert_eq!(row.unit, StudioUnitToken::Degrees, "{at}");
        assert_eq!([row.hard_min, row.hard_max, row.soft_min, row.soft_max], [None; 4], "{at}: no numeric bound");
        assert_eq!(row.bound_state, StudioBoundState::UnboundedInSource, "{at}");
        // Full signed degrees, revolutions included: stored as given, none of it refused.
        for v in [0.0, 5.0, 360.0, 725.0, -725.0, 1440.5, -0.003906, MAX_STREAM_ABS_VALUE, -MAX_STREAM_ABS_VALUE] {
            assert!(row.accepts(&[v]), "{at}: {v}");
        }
        // Only the Folio ceiling and non-finite values stay refused.
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, MAX_STREAM_ABS_VALUE.next_up(), (-MAX_STREAM_ABS_VALUE).next_down()] {
            assert!(!row.accepts(&[v]), "{at}: {v}");
        }
        assert!(!row.accepts(&[]) && !row.accepts(&[0.0, 0.0]), "{at}: one component");
        assert!(row.accepts(row.default), "{at}: the default is legal");
    }
}

#[test]
fn glow_color_phase_and_directional_blur_direction_rows_change_from_numeric_pairs_to_unbounded() {
    // Old -> new. Glow Color Phase: hard 0.003906..0.496094, soft 0.003906..0.039062, declared_both
    // -> all four None, unbounded_in_source. Directional Blur Direction: hard None, soft 0..7936,
    // declared_soft_only -> all four None, unbounded_in_source. Defaults, unit and precision stay.
    let p = StudioCompositionEffectKind::Glow.param("color_phase").unwrap();
    assert_eq!((p.param_index, p.kind, p.unit, p.precision, p.default), (10, StudioParameterKind::Angle, StudioUnitToken::Degrees, None, &[0.003906][..]));
    assert_eq!((p.hard_min, p.hard_max, p.soft_min, p.soft_max, p.bound_state), (None, None, None, None, StudioBoundState::UnboundedInSource));
    assert!(p.animatable && p.interpolable);
    let d = StudioCompositionEffectKind::DirectionalBlur.param("direction").unwrap();
    assert_eq!((d.param_index, d.kind, d.unit, d.precision, d.default), (1, StudioParameterKind::Angle, StudioUnitToken::Degrees, None, &[0.0][..]));
    assert_eq!((d.hard_min, d.hard_max, d.soft_min, d.soft_max, d.bound_state), (None, None, None, None, StudioBoundState::UnboundedInSource));
    assert!(d.animatable && d.interpolable);
    // Rows that were already unbounded keep their recorded default and precision.
    let t = StudioCompositionEffectKind::Twirl.param("angle").unwrap();
    assert_eq!((t.default, t.precision), (&[0.0][..], Some(0)));
    for key in ["master_hue", "colorize_hue"] {
        let h = StudioCompositionEffectKind::HueSaturation.param(key).unwrap();
        assert_eq!((h.default, h.precision, h.unit), (&[0.0][..], Some(0), StudioUnitToken::Degrees), "{key}");
    }
    // Neighbouring numeric rows are untouched: Glow `A & B Midpoint` keeps its hard pair and
    // Directional Blur `Blur Length` its hard and soft pairs.
    let m = StudioCompositionEffectKind::Glow.param("a_b_midpoint").unwrap();
    assert_eq!((m.hard_min, m.hard_max), (Some(0.009995), Some(0.990005)));
    let l = StudioCompositionEffectKind::DirectionalBlur.param("blur_length").unwrap();
    assert_eq!((l.hard_min, l.hard_max, l.soft_min, l.soft_max), (Some(0.0), Some(1000.0), Some(0.0), Some(20.0)));
}

fn angle_stack() -> Value {
    let key = |t: u64, v: f64| json!({"time":t,"value":[v],"in_interp":"linear","out_interp":"linear"});
    json!([
        {"effect_id":"glow","filter_kind":"glow","values":{
            "color_phase":{"static_value":[725.0],"keyframes":[key(0, 725.0), key(127008000000u64, -1080.5)]}}},
        {"effect_id":"dir","filter_kind":"directional_blur","values":{
            "direction":{"static_value":[-725.0],"keyframes":[key(0, -725.0), key(127008000000u64, 8000.25)]}}},
        {"effect_id":"hue","filter_kind":"hue_saturation","values":{
            "master_hue":constant(json!([-1080.0])),"colorize_hue":constant(json!([725.0]))}},
        {"effect_id":"twirl","filter_kind":"twirl","values":{"angle":constant(json!([-1440.0]))}}
    ])
}

#[test]
fn multi_turn_and_negative_angles_are_stored_and_round_trip_unchanged() {
    let with = with_effects(angle_stack());
    // Valid (nothing refused for its size), then a byte-identical round trip through the record
    // wire form: no wrap into 0..360 or -180..180, no clamp, no rounding.
    check(&with).unwrap();
    let r: StudioCompositionRecords = serde_json::from_value(with.clone()).unwrap();
    let first = serde_json::to_vec(&r).unwrap();
    let back: StudioCompositionRecords = serde_json::from_slice(&first).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), with);
    check(&serde_json::from_slice::<Value>(&first).unwrap()).unwrap();
    let e = &back.compositions[0].layers[0].effects;
    assert_eq!(e[0].static_or_default("color_phase"), Some(vec![725.0]));
    assert_eq!(e[1].static_or_default("direction"), Some(vec![-725.0]));
    assert_eq!(e[2].static_or_default("master_hue"), Some(vec![-1080.0]));
    assert_eq!(e[2].static_or_default("colorize_hue"), Some(vec![725.0]));
    assert_eq!(e[3].static_or_default("angle"), Some(vec![-1440.0]));
    // Keyframe values too (725 = two turns plus 5 degrees, stored as 725).
    let glow_keys: Vec<f64> = e[0].values["color_phase"].keyframes.iter().map(|k| k.value[0]).collect();
    assert_eq!(glow_keys, [725.0, -1080.5]);
    let dir_keys: Vec<f64> = e[1].values["direction"].keyframes.iter().map(|k| k.value[0]).collect();
    assert_eq!(dir_keys, [-725.0, 8000.25]);
    // An unset angle reads its recorded default, never a wrapped value.
    let fresh = StudioCompositionEffect::new("g", StudioCompositionEffectKind::Glow);
    assert_eq!(fresh.static_or_default("color_phase"), Some(vec![0.003906]));
}

#[test]
fn only_the_folio_ceiling_refuses_an_angle() {
    let t = identity("SLYR", 1);
    let one = |e: Value| with_effects(json!([e]));
    // Glow Color Phase above the old junk pair (about 0.5 degrees) is legal.
    check(&one(json!({"effect_id":"g","filter_kind":"glow","values":{"color_phase":constant(json!([0.5]))}}))).unwrap();
    check(&one(json!({"effect_id":"g","filter_kind":"glow","values":{"color_phase":constant(json!([360.0]))}}))).unwrap();
    // A value past the Folio ceiling (not a bound of the row) is still a typed refusal, static or
    // keyframed, by the stream's own finiteness and ceiling check before any row bound is read.
    let over = 1.0e13;
    for (kind, key) in [("glow", "color_phase"), ("directional_blur", "direction"), ("twirl", "angle")] {
        let s = one(json!({"effect_id":"e","filter_kind":kind,"values":{key:constant(json!([over]))}}));
        assert_eq!(refusal(&s), format!("InvalidField:{t}/effects/e/{key}/static_value"), "{kind}");
        let k = one(json!({"effect_id":"e","filter_kind":kind,"values":{key:{"static_value":[1.0],"keyframes":[
            {"time":0,"value":[1.0],"in_interp":"linear","out_interp":"linear"},
            {"time":10,"value":[-over],"in_interp":"linear","out_interp":"linear"}]}}}));
        assert_eq!(refusal(&k), format!("InvalidField:{t}/effects/e/{key}/keyframes/value"), "{kind} keyframe");
    }
}
