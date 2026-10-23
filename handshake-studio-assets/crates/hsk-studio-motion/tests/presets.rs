//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 3: animation presets on the property tree
//! (STU-MOT-110 values, keyframes and expressions written into named property paths; effect
//! instances as property groups, STU-MOT-017; STU-MOT-111 path-based application that fails loudly
//! with PRESET_TARGET_INCOMPATIBLE naming the unmatched paths and applies nothing). Apply-with-time-
//! offset shifts every key by the application time. After Effects `.ffx` semantics UNVERIFIED (D5).
use hsk_studio_accord::Ticks;
use hsk_studio_motion::*;

fn p(s: &str) -> PropertyPath {
    PropertyPath::new(s).unwrap()
}

/// A source layer: an eased opacity fade 0 -> 100 (keys at ticks 1000 and 3000), a static
/// position with an expression, and a blur effect instance with a keyed radius.
fn source() -> PropertyStore {
    let mut s = PropertyStore::new();
    let fade = Property::new(
        "opacity",
        vec![100.0],
        vec![Keyframe::eased(Ticks::new(1000), vec![0.0]), Keyframe::new(Ticks::new(3000), vec![100.0])],
    )
    .unwrap();
    s.insert(p("c/src/transform/opacity"), fade).unwrap();
    s.insert(p("c/src/transform/position"), Property::new("position", vec![5.0, 6.0], vec![]).unwrap().with_expression("wiggle(2,10)").unwrap()).unwrap();
    s.insert(
        p("c/src/effects/blur/radius"),
        Property::new("radius", vec![4.0], vec![Keyframe::new(Ticks::new(2000), vec![4.0]), Keyframe::new(Ticks::new(4000), vec![12.0])]).unwrap(),
    )
    .unwrap();
    s
}

fn target() -> PropertyStore {
    let mut t = PropertyStore::new();
    t.insert(p("c/dst/transform/opacity"), Property::new("opacity", vec![50.0], vec![]).unwrap()).unwrap();
    t.insert(p("c/dst/transform/position"), Property::new("position", vec![0.0, 0.0], vec![]).unwrap()).unwrap();
    // The target already carries a `blur` instance: the preset's blur is applied as `blur_2`.
    t.insert(p("c/dst/effects/blur/radius"), Property::new("radius", vec![1.0], vec![]).unwrap()).unwrap();
    t
}

fn blur() -> Vec<PresetEffect> {
    vec![PresetEffect { instance: "blur".into(), effect_kind: "gaussian_blur".into() }]
}

#[test]
fn saved_preset_applies_values_keyframes_expressions_and_effects_at_a_time_offset() {
    let src = source();
    let preset = capture_preset(&src, "c/src", "Fade and blur", &blur(), &["transform/opacity", "transform/position", "effects/blur/radius"], None).unwrap();
    // Origin = the earliest captured key (1000): keys become 0 / 2000 and 1000 / 3000.
    let op = &preset.entries[0];
    assert_eq!(op.kind, PresetEntryKind::Write);
    assert_eq!(op.keyframes.iter().map(|k| k.time.value()).collect::<Vec<_>>(), vec![0, 2000]);
    assert_eq!(op.keyframes[0].in_interp, Interpolation::Bezier, "ease kept");
    assert_eq!(preset.entries[1].expression.as_deref(), Some("wiggle(2,10)"));
    assert_eq!(preset.entries[2].kind, PresetEntryKind::Insert);
    assert_eq!(preset.entries[2].keyframes.iter().map(|k| k.time.value()).collect::<Vec<_>>(), vec![1000, 3000]);

    let mut dst = target();
    let applied = apply_preset(&mut dst, "c/dst", &preset, Ticks::new(10_000)).unwrap();
    assert_eq!(applied.instances.get("blur").map(String::as_str), Some("blur_2"));
    assert_eq!(applied.written, vec![p("c/dst/transform/opacity"), p("c/dst/transform/position")]);
    assert_eq!(applied.inserted, vec![p("c/dst/effects/blur_2/radius")]);
    let o = dst.get(&p("c/dst/transform/opacity")).unwrap();
    assert_eq!(o.keyframes().iter().map(|k| k.time.value()).collect::<Vec<_>>(), vec![10_000, 12_000]);
    assert_eq!(o.keyframes()[0].value, vec![0.0]);
    assert_eq!(o.static_value(), &[100.0]);
    assert_eq!(dst.get(&p("c/dst/transform/position")).unwrap().expression(), Some("wiggle(2,10)"));
    let r = dst.get(&p("c/dst/effects/blur_2/radius")).unwrap();
    assert_eq!(r.keyframes().iter().map(|k| k.time.value()).collect::<Vec<_>>(), vec![11_000, 13_000]);
    assert_eq!(dst.get(&p("c/dst/effects/blur/radius")).unwrap().static_value(), &[1.0], "existing instance untouched");
    // Applying again creates blur_3; an explicit origin keeps absolute times.
    let again = apply_preset(&mut dst, "c/dst", &preset, Ticks::new(0)).unwrap();
    assert_eq!(again.instances["blur"], "blur_3");
    let abs = capture_preset(&src, "c/src", "abs", &[], &["transform/opacity"], Some(Ticks::new(0))).unwrap();
    assert_eq!(abs.entries[0].keyframes[1].time.value(), 3000);
}

#[test]
fn incompatible_targets_and_malformed_presets_apply_nothing() {
    let preset = capture_preset(&source(), "c/src", "Fade and blur", &blur(), &["transform/opacity", "transform/position", "effects/blur/radius"], None).unwrap();
    // A text-only target: no opacity, a 1-component "position" -> both unmatched, store unchanged.
    let mut t = PropertyStore::new();
    t.insert(p("c/txt/transform/position"), Property::new("position", vec![0.0], vec![]).unwrap()).unwrap();
    t.insert(p("c/txt/text/animator/offset"), Property::new("offset", vec![0.0], vec![]).unwrap()).unwrap();
    let rev = t.revision();
    let e = apply_preset(&mut t, "c/txt", &preset, Ticks::new(0)).unwrap_err();
    assert_eq!(e.code(), PRESET_TARGET_INCOMPATIBLE);
    assert_eq!(e, PresetError::TargetIncompatible { unmatched: vec!["transform/opacity".into(), "transform/position".into()] });
    assert_eq!(e.to_string(), "PRESET_TARGET_INCOMPATIBLE:transform/opacity,transform/position");
    assert_eq!((t.revision(), t.len()), (rev, 2));
    assert!(t.get(&p("c/txt/effects/blur/radius")).is_none(), "no half-rigged layer");
    // A key pushed past u64 by the offset: refused before any mutation.
    let mut dst = target();
    let rev = dst.revision();
    assert_eq!(apply_preset(&mut dst, "c/dst", &preset, Ticks::new(u64::MAX - 10)).unwrap_err(), PresetError::Motion(MotionError::TickOverflow));
    assert_eq!(dst.revision(), rev);
    // A NaN value in a preset entry is refused by the property model, nothing applied.
    let mut bad = preset.clone();
    bad.entries[0].static_value = vec![f64::NAN];
    assert_eq!(apply_preset(&mut dst, "c/dst", &bad, Ticks::new(0)).unwrap_err(), PresetError::Motion(MotionError::NonFinite));
    assert_eq!(dst.revision(), rev);
    // Malformed presets: an Insert entry outside a declared instance, a duplicate path, a bad path,
    // an empty preset, a duplicate instance.
    let mut m = preset.clone();
    m.entries[0].kind = PresetEntryKind::Insert;
    assert_eq!(validate_preset(&m), Err(PresetError::Invalid("transform/opacity".into())));
    let mut m = preset.clone();
    m.entries[1].path = "transform/opacity".into();
    assert!(matches!(validate_preset(&m), Err(PresetError::Invalid(_))));
    let mut m = preset.clone();
    m.entries[1].path = "transform/../x".into();
    assert!(matches!(validate_preset(&m), Err(PresetError::Invalid(_))));
    let mut m = preset.clone();
    m.entries.clear();
    assert_eq!(validate_preset(&m), Err(PresetError::Invalid("entries".into())));
    let mut m = preset.clone();
    m.effects.push(m.effects[0].clone());
    assert!(matches!(validate_preset(&m), Err(PresetError::Invalid(_))));
    assert_eq!(dst.revision(), rev);
}
