//! No-effect class (R3 + R5): a discrete move must change the value or be refused with a typed
//! error; it may not come back as an unchanged success. R3: number `clean` grid vs `fine_step`.
//! R5: integer step saturating at the `i64` edge, plus the crate-wide audit of every step, nudge,
//! clamp and no-change report path (slider arrows, live samples at a hard stop, stray aborts).

use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect, vec2};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_controls::egui_adapter::{PanelOutcome, edit_id, show_panel_and_apply};
use hsk_studio_controls::report::{ReportContext, report};
use hsk_studio_controls::{
    ChangeSource, ChoiceOption, ControlError, ControlSpec, EventPhase, GesturePhase, GestureRoles, IntegerSpec,
    Interaction, NumberSpec, Registry, ScrubGesture, Value, ValueStore,
};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};

fn num(v: f64) -> Value {
    Value::Number(v)
}

fn all_roles() -> [GestureRoles; 4] {
    [GestureRoles::NONE, roles(true, false), roles(false, true), roles(true, true)]
}

fn no_effect(id: &str) -> ControlError {
    ControlError::StepNoEffect { control_id: id.into() }
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

#[test]
fn integer_and_directionless_steps_at_the_domain_edge_are_refused() {
    // R5 finding (descriptor.rs integer arm): saturating_add at i64::MAX/MIN returned Ok(same).
    let open = ControlSpec::integer("fx.count", "Count", "Unbounded count.", IntegerSpec::field(None, None), 0);
    let registry = registered(open.clone());
    for role in all_roles() {
        assert_eq!(open.step_value(&Value::Int(i64::MAX), 1, role), Err(no_effect("fx.count")), "{role:?}");
        assert_eq!(open.step_value(&Value::Int(i64::MIN), -1, role), Err(no_effect("fx.count")), "{role:?}");
        assert_eq!(open.step_value(&Value::Int(i64::MAX), -1, role), Ok(Value::Int(i64::MAX - 1)));
        assert_eq!(open.step_value(&Value::Int(i64::MIN), 1, role), Ok(Value::Int(i64::MIN + 1)));
    }
    let mut store = ValueStore::new(&registry);
    store.set(&registry, "fx.count", Value::Int(i64::MAX), ChangeSource::Model, None).unwrap();
    let revision = store.revision();
    let step = open.step_value(store.get("fx.count").unwrap(), 1, GestureRoles::NONE);
    let refused = store.apply(&registry, &Interaction::discrete("fx.count", ChangeSource::Keyboard, step), None);
    assert_eq!(refused, Err(no_effect("fx.count")));
    assert_eq!((store.get("fx.count"), store.revision()), (Some(&Value::Int(i64::MAX)), revision), "store untouched");

    // A hard bound at the domain edge: MAX + 1 is not representable, so the move has no effect.
    let edge = ControlSpec::integer("fx.edge", "Edge", "Domain-edge bounds.", IntegerSpec::field(Some(i64::MIN), Some(i64::MAX)), 0);
    assert_eq!(edge.step_value(&Value::Int(i64::MAX), 1, GestureRoles::NONE), Err(no_effect("fx.edge")));
    // Inside the domain a hard bound still rejects with the bound named; nothing is clamped.
    let small = ControlSpec::integer("fx.small", "Small", "Three either way.", IntegerSpec::field(Some(-3), Some(3)), 0);
    assert_eq!(
        small.step_value(&Value::Int(3), 1, GestureRoles::NONE),
        Err(ControlError::OutOfHardBoundsInt { control_id: "fx.small".into(), value: 4, hard_min: Some(-3), hard_max: Some(3) })
    );

    // No direction is no move: integers, an on-grid number, and an off-grid number that noise
    // removal alone would have changed (it came back as Ok(0.123456789) before). At the f64 domain
    // edge every increment is absorbed.
    let gain = ControlSpec::number("fx.gain", "Gain", "Gain.", NumberSpec::field(2, None, None), 0.5);
    for role in all_roles() {
        assert_eq!(open.step_value(&Value::Int(5), 0, role), Err(no_effect("fx.count")));
        assert_eq!(gain.step_value(&num(0.5), 0, role), Err(no_effect("fx.gain")));
        assert_eq!(gain.step_value(&num(0.123_456_789_123_4), 0, role), Err(no_effect("fx.gain")));
        for v in [f64::MAX, -f64::MAX] {
            for dir in [1_i8, -1] {
                assert_eq!(gain.step_value(&num(v), dir, role), Err(no_effect("fx.gain")), "{v} {dir} {role:?}");
            }
        }
    }

    // Kinds without a step route (toggle; choice has no option cycling) refuse with a type error.
    let toggle = ControlSpec::toggle("fx.on", "On", "Enabled.", true);
    let options = vec![ChoiceOption::new("normal", "Normal"), ChoiceOption::new("multiply", "Multiply")];
    let blend = ControlSpec::choice("fx.blend", "Blend", "Mix mode.", options, "normal");
    assert!(matches!(toggle.step_value(&Value::Bool(true), 1, GestureRoles::NONE), Err(ControlError::TypeMismatch { .. })));
    assert!(matches!(
        blend.step_value(&Value::Choice("normal".into()), 1, GestureRoles::NONE),
        Err(ControlError::TypeMismatch { .. })
    ));
}

fn key(k: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers }
}

struct Panel {
    ctx: egui::Context,
    t: f64,
    reg: Registry,
    store: ValueStore,
}

impl Panel {
    fn new() -> Self {
        let mut reg = Registry::new();
        let opacity = NumberSpec::slider(2, (0.0, 1.0), (0.2, 0.8));
        reg.register(ControlSpec::number("layer.opacity", "Opacity", "Layer opacity.", opacity, 0.5)).unwrap();
        let fps = IntegerSpec::slider((1, 120), (12, 60));
        reg.register(ControlSpec::integer("adv.fps", "Frame rate", "Frames per second.", fps, 24)).unwrap();
        let store = ValueStore::new(&reg);
        Self { ctx: egui::Context::default(), t: 0.0, reg, store }
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) -> PanelOutcome {
        self.t += 0.1;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))),
            time: Some(self.t),
            modifiers,
            events,
            ..Default::default()
        };
        let (reg, store) = (&self.reg, &mut self.store);
        let mut outcome = PanelOutcome::default();
        let _ = self.ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| outcome = show_panel_and_apply(ui, reg, store, None));
        });
        outcome
    }

    /// One key press (down + up in one frame) and one settling frame; returns the press frame.
    fn press(&mut self, k: Key, modifiers: Modifiers) -> PanelOutcome {
        let out = self.frame(vec![key(k, true, modifiers), key(k, false, modifiers)], modifiers);
        self.frame(vec![], Modifiers::NONE);
        out
    }

    fn put(&mut self, id: &str, value: Value) {
        self.store.set(&self.reg, id, value, ChangeSource::Model, None).unwrap();
        self.frame(vec![], Modifiers::NONE);
    }
}

fn moves(out: &PanelOutcome) -> Vec<(ChangeSource, EventPhase, Value, Value)> {
    out.changes.iter().map(|e| (e.source, e.phase, e.old.clone(), e.new.clone())).collect()
}

#[test]
fn slider_arrow_keys_use_the_descriptor_step_at_every_position() {
    // R5 audit (egui_adapter.rs slider): egui's own slider arrows were fed to the clamping pointer
    // route: one track point per press, silent stop at the track end, a jump back to the track end
    // from beyond it, and an unchanged "success" at the hard bound.
    use ChangeSource::Keyboard;
    use EventPhase::Discrete;
    let none = Modifiers::NONE;
    let mut p = Panel::new();
    p.frame(vec![], none);
    // Tab order: opacity label (scrub handle, focusable), opacity slider.
    p.press(Key::Tab, none);
    p.press(Key::Tab, none);
    let focused = p.ctx.memory(|m| m.focused());
    assert!(focused.is_some() && focused != Some(edit_id("layer.opacity")), "{focused:?}");

    assert_eq!(moves(&p.press(Key::ArrowRight, none)), [(Keyboard, Discrete, num(0.5), num(0.51))], "descriptor step");
    let coarse = p.press(Key::ArrowRight, Modifiers::SHIFT);
    assert_eq!(moves(&coarse), [(Keyboard, Discrete, num(0.51), num(0.61))], "coarse role x10");
    p.put("layer.opacity", num(0.8));
    assert_eq!(moves(&p.press(Key::ArrowRight, none)), [(Keyboard, Discrete, num(0.8), num(0.81))], "past the track end");
    p.put("layer.opacity", num(0.95));
    assert_eq!(moves(&p.press(Key::ArrowRight, none)), [(Keyboard, Discrete, num(0.95), num(0.96))], "beyond the track");

    // At the hard bound the step is a typed refusal, store untouched; the other direction moves.
    p.put("layer.opacity", num(1.0));
    let revision = p.store.revision();
    let out = p.press(Key::ArrowRight, none);
    assert!(out.changes.is_empty(), "{:?}", out.changes);
    let over = ControlError::OutOfHardBounds { control_id: "layer.opacity".into(), value: 1.01, hard_min: Some(0.0), hard_max: Some(1.0) };
    assert_eq!(out.rejected, [over]);
    assert_eq!((p.store.get("layer.opacity"), p.store.revision()), (Some(&num(1.0)), revision));
    assert_eq!(moves(&p.press(Key::ArrowLeft, none)), [(Keyboard, Discrete, num(1.0), num(0.99))]);

    // Integer slider: opacity text field, fps label, fps slider.
    for _ in 0..3 {
        p.press(Key::Tab, none);
    }
    p.put("adv.fps", Value::Int(120));
    let out = p.press(Key::ArrowRight, none);
    assert!(out.changes.is_empty(), "{:?}", out.changes);
    let over = ControlError::OutOfHardBoundsInt { control_id: "adv.fps".into(), value: 121, hard_min: Some(1), hard_max: Some(120) };
    assert_eq!(out.rejected, [over]);
    let left = p.press(Key::ArrowLeft, Modifiers::SHIFT);
    assert_eq!(moves(&left), [(Keyboard, Discrete, Value::Int(120), Value::Int(119))], "integer: modifiers do not skip");
}

#[derive(Default)]
struct Frames(usize);

impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        assert!(matches!(class, DeliveryClass::Terminal));
        self.0 += 1;
        Ok(())
    }
}

#[test]
fn no_change_is_reported_by_route_and_a_refused_end_keeps_the_gesture() {
    let spec = ControlSpec::number("layer.opacity", "Opacity", "Layer opacity.", NumberSpec::slider(2, (0.0, 1.0), (0.2, 0.8)), 0.5);
    let count = ControlSpec::integer("fx.count", "Count", "Unbounded count.", IntegerSpec::field(None, None), i64::MAX);
    let mut reg = Registry::new();
    reg.register(spec.clone()).unwrap();
    reg.register(count.clone()).unwrap();
    let ctx = ReportContext {
        correlation_id: 9,
        resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap(),
        actor: ActorContext::new("acct", "princ", "acct", "princ", "space", "sess").unwrap(),
    };
    let live = CancellationToken::default();
    let run = |store: &mut ValueStore, i: Interaction, sink: &mut Frames| {
        let result = store.apply(&reg, &i, None);
        report(&ctx, store.revision(), i.gesture, &result, &live, sink).unwrap().map(|r| r.outcome)
    };
    let id = "layer.opacity";
    let upd = |o: Result<Value, ControlError>| Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::Update, o);
    let end = |o: Result<Value, ControlError>| Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::End, o);
    let mut store = ValueStore::new(&reg);
    let mut sink = Frames::default();

    // Scrub past the hard stop: the second sample is clamped to the same value. A live sample is
    // not terminal even when it changes nothing (it was reported as a terminal Success before).
    let mut g = ScrubGesture::begin(&spec, &num(0.5)).unwrap();
    let first = g.drag(&spec, 100.0, GestureRoles::NONE);
    let again = g.drag(&spec, 10.0, GestureRoles::NONE);
    assert_eq!((&first, &again), (&Ok(num(1.0)), &Ok(num(1.0))));
    assert_eq!(run(&mut store, upd(first), &mut sink), None);
    assert_eq!(run(&mut store, upd(again), &mut sink), None, "clamped sample at the hard stop");
    assert_eq!(sink.0, 0);
    assert_eq!(run(&mut store, end(Ok(num(1.0))), &mut sink), Some(Outcome::Success));
    assert_eq!(sink.0, 1);

    // Abort with no open gesture (Escape before the first sample): Canceled, not Success.
    let abort = Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::Abort, Ok(num(1.0)));
    assert_eq!(run(&mut store, abort, &mut sink), Some(Outcome::Canceled));
    // End with no gesture opened (press and release without movement): no change, no undo step.
    assert_eq!(store.apply(&reg, &end(Ok(num(1.0))), None), Ok(None));
    // Absolute route whose requested value already holds: idempotent Success.
    let same = Interaction::discrete(id, ChangeSource::Model, Ok(num(1.0)));
    assert_eq!(run(&mut store, same, &mut sink), Some(Outcome::Success));
    // Relative moves without effect arrive as refusals: past the hard max, and at the i64 edge.
    let step = spec.step_value(store.get(id).unwrap(), 1, GestureRoles::NONE);
    let validation = Some(Outcome::Failure(FailureCode::Validation));
    assert_eq!(run(&mut store, Interaction::discrete(id, ChangeSource::Keyboard, step), &mut sink), validation);
    let edge = count.step_value(store.get("fx.count").unwrap(), 1, GestureRoles::NONE);
    assert_eq!(run(&mut store, Interaction::discrete("fx.count", ChangeSource::Keyboard, edge), &mut sink), validation);
    assert_eq!(sink.0, 5);

    // R5 PLAUSIBLE (store.rs End): a refused End leaves the store untouched, the open gesture
    // included; set stays refused until a legal End or an Abort.
    let mut store = ValueStore::new(&reg);
    store.apply(&reg, &upd(Ok(num(0.6))), None).unwrap();
    let non_finite = ControlError::NonFinite { control_id: id.into() };
    assert_eq!(store.apply(&reg, &end(Err(non_finite.clone())), None), Err(non_finite));
    let over = ControlError::OutOfHardBounds { control_id: id.into(), value: 1.5, hard_min: Some(0.0), hard_max: Some(1.0) };
    assert_eq!(store.apply(&reg, &end(Ok(num(1.5))), None), Err(over));
    assert!(store.gesture_active(id));
    assert_eq!(store.get(id), Some(&num(0.6)));
    assert!(matches!(store.set(&reg, id, num(0.7), ChangeSource::Model, None), Err(ControlError::Gesture { .. })));
    let done = store.apply(&reg, &end(Ok(num(0.7))), None).unwrap().unwrap();
    assert!(matches!(done.phase, EventPhase::Committed(_)), "{done:?}");
    assert_eq!((done.old, done.new, store.gesture_active(id)), (num(0.5), num(0.7), false));
    // Same after a refused End, but closed by Abort: the press value comes back.
    store.apply(&reg, &upd(Ok(num(0.2))), None).unwrap();
    assert!(store.apply(&reg, &end(Ok(num(f64::NAN))), None).is_err());
    let abort = Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::Abort, Ok(num(0.2)));
    let aborted = store.apply(&reg, &abort, None).unwrap().unwrap();
    assert!(matches!(aborted.phase, EventPhase::Aborted(_)) && !store.gesture_active(id));
    assert_eq!(store.get(id), Some(&num(0.7)));
}

#[test]
fn typed_text_grammar_is_unchanged_by_the_overflow_safe_parser() {
    // V4-01 rewrite of strict_decimal / parse_hex_color without index arithmetic: same grammar.
    let free = ControlSpec::number("fx.free", "Free", "Unbounded.", NumberSpec::field(3, None, None), 0.0);
    for (text, v) in [("+0.5", 0.5), ("-.5", -0.5), ("5.", 5.0), (".5", 0.5), ("1e-1", 0.1), ("1E+2", 100.0), ("-0", 0.0), ("007", 7.0)] {
        assert_eq!(free.parse_text(text), Ok(num(v)), "{text:?}");
    }
    for junk in ["1e5e3", "+-1", "--1", "1.2.3", "e5", ".e5", "1e+", "1e-", ".", "+", "", "1 2", "1_000", "\u{661}"] {
        assert!(matches!(free.parse_text(junk), Err(ControlError::InvalidText { .. })), "{junk:?}");
    }
    let tint = ControlSpec::color("fx.tint", "Tint", "Overlay.", [0, 0, 0, 255]);
    assert_eq!(tint.parse_text("#ff8000"), Ok(Value::Color([255, 128, 0, 255])));
    assert_eq!(tint.parse_text("#FF800080"), Ok(Value::Color([255, 128, 0, 128])));
    for junk in ["ff8000", "#ff80", "#ff8000f", "#gg8000", "#ff8000801", "#+f8000"] {
        assert!(matches!(tint.parse_text(junk), Err(ControlError::InvalidText { .. })), "{junk:?}");
    }
}
