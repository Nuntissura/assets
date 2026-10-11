//! R3 regression (descriptor.rs: `clean` grid vs `fine_step`): a discrete number step must change
//! the value or be refused with a typed error; it may never come back as an unchanged success.

use hsk_studio_controls::{
    ChangeSource, ControlError, ControlSpec, GestureRoles, NumberSpec, Registry, ScrubGesture, Value, ValueStore,
};

fn num(v: f64) -> Value {
    Value::Number(v)
}

fn registered(spec: ControlSpec) -> Registry {
    let mut registry = Registry::new();
    registry.register(spec).expect("precision 9 stays a legal descriptor");
    registry
}

fn roles(coarse: bool, fine: bool) -> GestureRoles {
    GestureRoles { coarse, fine }
}

#[test]
fn fine_step_at_precision_9_changes_the_value_through_every_route() {
    let spec = ControlSpec::number("fx.gain", "Gain", "Precision 9 gain.", NumberSpec::field(9, None, None), 0.5);
    let registry = registered(spec.clone());
    let fine = roles(false, true);

    // R3 finding: clean() rounded to a 1e-9 grid, so fine_step = 1e-10 returned Ok(0.5) and
    // store.set answered Ok(None) (already current) - a silent no-op.
    let up = spec.step_value(&num(0.5), 1, fine).unwrap();
    assert_eq!(up, num(0.500_000_000_1));
    assert_eq!(spec.step_value(&num(0.5), -1, fine).unwrap(), num(0.499_999_999_9));
    let mut store = ValueStore::new(&registry);
    let event = store.set(&registry, "fx.gain", up, ChangeSource::Keyboard, None).unwrap();
    assert!(event.is_some(), "the fine step is a real change in the store");
    assert_eq!(store.get("fx.gain"), Some(&num(0.500_000_000_1)));

    // The pointer route has the same fine role: frozen step 1e-9 (soft range 2e-7 / 200), fine x0.1.
    let slider = ControlSpec::number(
        "fx.trim",
        "Trim",
        "Precision 9 trim.",
        NumberSpec::slider(9, (-1.0, 1.0), (0.0, 2.0e-7)),
        0.5,
    );
    let mut gesture = ScrubGesture::begin(&slider, &num(0.5)).unwrap();
    assert!((gesture.step() - 1.0e-9).abs() < 1.0e-24, "{}", gesture.step());
    assert_eq!(gesture.drag(&slider, 1.0, fine).unwrap(), num(0.500_000_000_1));
}

#[test]
fn every_declared_precision_steps_by_its_derived_increments() {
    for precision in 0..=9_u8 {
        let spec = ControlSpec::number("fx.any", "Any", "Any precision.", NumberSpec::field(precision, None, None), 0.5);
        spec.validate().unwrap();
        let n = spec.number_spec().unwrap();
        let cases = [
            (GestureRoles::NONE, n.step()),
            (roles(true, false), n.coarse_step()),
            (roles(false, true), n.fine_step()),
            (roles(true, true), n.step() * 100.0),
        ];
        for (role, amount) in cases {
            for dir in [1_i8, -1] {
                let next = match spec.step_value(&num(0.5), dir, role).unwrap() {
                    Value::Number(v) => v,
                    other => panic!("{other:?}"),
                };
                let expected = 0.5 + f64::from(dir) * amount;
                assert!(next != 0.5, "precision {precision} {role:?} dir {dir}: unchanged");
                assert!((next - expected).abs() < 1.0e-15, "precision {precision} {role:?} dir {dir}: {next} vs {expected}");
            }
        }
    }
}

#[test]
fn a_step_below_f64_resolution_is_refused_not_reported_as_success() {
    let spec = ControlSpec::number("fx.big", "Big", "Large magnitude.", NumberSpec::field(9, None, None), 1.0e7);
    let registry = registered(spec.clone());
    // 1e7 + 1e-10 == 1e7 in binary64: the old code returned Ok(1e7).
    let error = spec.step_value(&num(1.0e7), 1, roles(false, true)).unwrap_err();
    assert_eq!(error, ControlError::StepNoEffect { control_id: "fx.big".into() });
    assert_eq!(error.code(), "step_no_effect");
    assert!(error.to_string().contains("fx.big"));
    let store = ValueStore::new(&registry);
    assert_eq!(store.get("fx.big"), Some(&num(1.0e7)), "a refused step leaves the store alone");

    // At every magnitude and role the answer is a changed value or the typed refusal, never Ok(same).
    for magnitude in [0.5, 123.456, 1.0e4, 1.0e5, 1.0e6, 4.0e6, 1.0e7, 1.0e9] {
        for role in [GestureRoles::NONE, roles(true, false), roles(false, true), roles(true, true)] {
            for dir in [1_i8, -1] {
                match spec.step_value(&num(magnitude), dir, role) {
                    Ok(Value::Number(v)) => assert!(v != magnitude, "{magnitude} {role:?} {dir}: unchanged success"),
                    Ok(other) => panic!("{other:?}"),
                    Err(ControlError::StepNoEffect { .. }) => {}
                    Err(other) => panic!("{magnitude} {role:?} {dir}: {other:?}"),
                }
            }
        }
    }
}
