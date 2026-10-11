//! Slice 1 focused tests (<= 5): numeric legality + store/undo, headless egui interaction,
//! projection round-trip, dead-control audit, descriptor refusal.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use hsk_studio_controls::egui_adapter::{edit_id, show_panel_and_apply};
use hsk_studio_controls::projection::{agent_json, manual_topics, registry_from_json};
use hsk_studio_controls::{
    ChangeEvent, ChangeSource, ChoiceOption, ControlError, ControlSpec, NumberSpec, Registry, SoftState, StepModifier,
    Value, ValueStore,
};

fn opacity() -> ControlSpec {
    // MT-35553 fixture: precision 2, hard [0,1], soft [0.2,0.8], initial 0.50.
    ControlSpec::number(
        "layer.opacity",
        "Opacity",
        "How much of the layer shows through. Drag the label to scrub.",
        NumberSpec::slider(2, (0.0, 1.0), (0.2, 0.8)),
        0.5,
    )
}

fn fixture() -> Registry {
    let mut r = Registry::new();
    r.register(ControlSpec::toggle("layer.visible", "Visible", "Shows or hides the layer.", true)).unwrap();
    r.register(opacity()).unwrap();
    r.register(
        ControlSpec::number("layer.blur", "Blur", "Gaussian blur radius.", NumberSpec::field(1, Some(0.0), None), 0.0)
            .with_unit("px"),
    )
    .unwrap();
    r.register(ControlSpec::choice(
        "layer.blend",
        "Blend",
        "How the layer mixes with the layers below.",
        vec![ChoiceOption::new("normal", "Normal"), ChoiceOption::new("multiply", "Multiply")],
        "normal",
    ))
    .unwrap();
    r.register(ControlSpec::color("layer.tint", "Tint", "Colour overlay.", [255, 128, 0, 255])).unwrap();
    r.register(ControlSpec::text("layer.name", "Name", "Layer display name.", 32, false, "Layer 1")).unwrap();
    for id in ["layer.visible", "layer.opacity", "layer.blur", "layer.blend", "layer.tint", "layer.name"] {
        r.register_consumer(id, "test.renderer").unwrap();
    }
    r
}

fn num(v: f64) -> Value {
    Value::Number(v)
}

#[test]
fn fixture_numeric_legality_store_and_undo() {
    let reg = fixture();
    let spec = reg.get("layer.opacity").unwrap();
    let ns = spec.number_spec().unwrap();
    assert_eq!((ns.step(), ns.fine_step(), ns.coarse_step()), (0.01, 0.001, 0.1));

    // Typed: exact value ok; beyond soft but inside hard is legal and flagged.
    assert_eq!(spec.parse_text("0.75").unwrap(), num(0.75));
    let beyond = spec.parse_text("0.9").unwrap();
    assert_eq!((beyond.clone(), spec.soft_state(&beyond)), (num(0.9), SoftState::OutOfSoft));
    assert_eq!(spec.soft_state(&num(0.75)), SoftState::InRange);
    // Display rounds to precision; the stored value keeps full precision.
    assert_eq!(spec.parse_text("0.7549").unwrap(), num(0.7549));
    assert_eq!(spec.format_value(&num(0.7549)), "0.75");

    // Hard violation is an error naming the control, the value and both bounds, never a clamp.
    let err = spec.parse_text("1.25").unwrap_err();
    assert_eq!(
        err,
        ControlError::OutOfHardBounds { control_id: "layer.opacity".into(), value: 1.25, hard_min: Some(0.0), hard_max: Some(1.0) }
    );
    let msg = err.to_string();
    assert!(msg.contains("layer.opacity") && msg.contains("1.25") && msg.contains('0') && msg.contains('1'), "{msg}");

    // Strict parser: no arithmetic, no junk stripping.
    assert!(matches!(spec.parse_text("NaN"), Err(ControlError::NonFinite { .. })));
    assert!(matches!(spec.parse_text("-inf"), Err(ControlError::NonFinite { .. })));
    for junk in ["0.75abc", "12+3", "", "  ", "0..5", "1e", ".", "0x10"] {
        assert!(matches!(spec.parse_text(junk), Err(ControlError::InvalidText { .. })), "{junk:?}");
    }
    // Unit suffix must match the descriptor exactly.
    let blur = reg.get("layer.blur").unwrap();
    assert_eq!(blur.parse_text("2.5 px").unwrap(), num(2.5));
    assert!(blur.parse_text("2.5em").is_err());

    // Pointer clamps ONLY hard bounds and crosses soft; keyboard steps once per modifier.
    assert_eq!(spec.pointer_value(1.7).unwrap(), num(1.0));
    assert_eq!(spec.pointer_value(0.9).unwrap(), num(0.9));
    assert_eq!(spec.step_value(&num(0.5), 1, StepModifier::None).unwrap(), num(0.51));
    assert_eq!(spec.step_value(&num(0.5), 1, StepModifier::Coarse).unwrap(), num(0.6));
    assert_eq!(spec.step_value(&num(0.5), -1, StepModifier::Fine).unwrap(), num(0.499));
    assert!(matches!(spec.step_value(&num(1.0), 1, StepModifier::None), Err(ControlError::OutOfHardBounds { .. })));

    // Store: rejected writes leave value and revision untouched; accepted writes emit undo events.
    let mut store = ValueStore::new(&reg);
    assert_eq!((store.get("layer.opacity"), store.revision()), (Some(&num(0.5)), 0));
    assert!(store.set(&reg, "layer.opacity", num(1.25), ChangeSource::Model, None).is_err());
    assert!(store.set(&reg, "layer.opacity", Value::Bool(true), ChangeSource::Model, None).is_err());
    assert!(store.set(&reg, "nope", num(1.0), ChangeSource::Model, None).is_err());
    assert_eq!((store.get("layer.opacity"), store.revision()), (Some(&num(0.5)), 0));
    assert_eq!(store.set(&reg, "layer.opacity", num(0.5), ChangeSource::Typed, None).unwrap(), None);
    let ev = store.set(&reg, "layer.opacity", num(0.75), ChangeSource::Typed, None).unwrap().unwrap();
    assert_eq!((ev.old.clone(), ev.new.clone(), ev.seq), (num(0.5), num(0.75), 1));
    let other = store.set(&reg, "layer.opacity", num(0.9), ChangeSource::Keyboard, None).unwrap().unwrap();
    assert!(matches!(store.undo(&reg, &ev, None), Err(ControlError::Stale { .. })), "undo of a superseded event is stale");
    store.undo(&reg, &other, None).unwrap();
    store.undo(&reg, &ev, None).unwrap();
    assert_eq!(store.get("layer.opacity"), Some(&num(0.5)));
}

fn frame(ctx: &egui::Context, t: &mut f64, events: Vec<Event>, reg: &Registry, store: &mut ValueStore, origin: &mut Pos2) -> Vec<ChangeEvent> {
    *t += 0.1;
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))),
        time: Some(*t),
        events,
        ..Default::default()
    };
    let mut changes = Vec::new();
    let _ = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            *origin = ui.next_widget_position();
            changes.extend(show_panel_and_apply(ui, reg, store, None));
        });
    });
    changes
}

fn key(k: Key, pressed: bool) -> Event {
    Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn headless_egui_frames_change_values_through_the_descriptor() {
    let reg = fixture();
    let mut store = ValueStore::new(&reg);
    let ctx = egui::Context::default();
    let (mut t, mut origin) = (0.0, Pos2::ZERO);
    let mut run = |events: Vec<Event>, store: &mut ValueStore, origin: &mut Pos2| frame(&ctx, &mut t, events, &reg, store, origin);

    run(vec![], &mut store, &mut origin); // layout warm-up: widget rects register
    run(vec![], &mut store, &mut origin);

    // Click the first control (toggle): press + release over its checkbox.
    let p = origin + vec2(10.0, 9.0);
    run(vec![Event::PointerMoved(p)], &mut store, &mut origin);
    run(vec![button(p, true)], &mut store, &mut origin);
    let changes = run(vec![button(p, false)], &mut store, &mut origin);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].control_id.as_str(), &changes[0].new, changes[0].source), ("layer.visible", &Value::Bool(false), ChangeSource::Pointer));
    assert_eq!(store.get("layer.visible"), Some(&Value::Bool(false)));

    // Typed entry on the opacity field: focus, select all, type, Enter.
    let eid = edit_id("layer.opacity");
    ctx.memory_mut(|m| m.request_focus(eid));
    run(vec![], &mut store, &mut origin);
    let mut st = egui::text_edit::TextEditState::load(&ctx, eid).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(4))));
    st.store(&ctx, eid);
    run(vec![Event::Text("0.75".into())], &mut store, &mut origin);
    let changes = run(vec![key(Key::Enter, true), key(Key::Enter, false)], &mut store, &mut origin);
    assert_eq!(store.get("layer.opacity"), Some(&num(0.75)), "{changes:?}");
    assert_eq!((changes[0].source, &changes[0].old), (ChangeSource::Typed, &num(0.5)));

    // Out-of-hard-range typed entry is rejected: value unchanged, no change event.
    ctx.memory_mut(|m| m.request_focus(eid));
    run(vec![], &mut store, &mut origin);
    let mut st = egui::text_edit::TextEditState::load(&ctx, eid).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(4))));
    st.store(&ctx, eid);
    run(vec![Event::Text("1.25".into())], &mut store, &mut origin);
    let changes = run(vec![key(Key::Enter, true), key(Key::Enter, false)], &mut store, &mut origin);
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(store.get("layer.opacity"), Some(&num(0.75)));

    // Escape discards an in-progress edit.
    ctx.memory_mut(|m| m.request_focus(eid));
    run(vec![], &mut store, &mut origin);
    run(vec![Event::Text("9".into())], &mut store, &mut origin);
    let changes = run(vec![key(Key::Escape, true), key(Key::Escape, false)], &mut store, &mut origin);
    assert!(changes.is_empty(), "{changes:?}");
    run(vec![], &mut store, &mut origin);
    assert_eq!(store.get("layer.opacity"), Some(&num(0.75)));
}

#[test]
fn agent_projection_round_trips_and_matches_manual() {
    let reg = fixture();
    let store = ValueStore::new(&reg);
    let json = agent_json(&reg, Some(&store));
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let opacity = doc["controls"].as_array().unwrap().iter().find(|c| c["id"] == "layer.opacity").unwrap();
    // STU-FX-107: four separate bound keys, plus the derived step trio and the current value.
    let kind = &opacity["kind"];
    assert!(kind["hard_min"] == 0.0 && kind["hard_max"] == 1.0 && kind["soft_min"] == 0.2 && kind["soft_max"] == 0.8, "{kind}");
    assert!(kind["step"] == 0.01 && kind["fine_step"] == 0.001 && kind["coarse_step"] == 0.1, "{kind}");
    assert!(opacity["value"]["value"] == 0.5);
    assert_eq!(opacity["consumers"][0], "test.renderer");

    // Round trip: descriptors and consumers survive; the second projection is byte-identical.
    let again = registry_from_json(&json).unwrap();
    assert_eq!(again.specs(), reg.specs());
    assert_eq!(agent_json(&again, Some(&ValueStore::new(&again))), json);

    // Manual topics come from the same descriptors (id/label/help/range/default).
    let topics = manual_topics(&reg);
    assert_eq!(topics.len(), reg.specs().len());
    let t = topics.iter().find(|t| t.id == "layer.opacity").unwrap();
    assert!(t.title == "Opacity" && t.body.contains("Hard range: [0, 1]") && t.body.contains("Soft range: [0.2, 0.8]"), "{}", t.body);
    assert!(t.body.contains("Default: 0.50."), "{}", t.body);

    // A tampered projection is refused by descriptor validation (hard max below soft max).
    let bad = json.replace("\"hard_max\": 1.0", "\"hard_max\": 0.5");
    assert!(registry_from_json(&bad).is_err());
}

#[test]
fn audit_catches_orphan_controls() {
    let mut reg = fixture();
    assert!(reg.audit().is_clean(), "{}", reg.audit());
    reg.register(ControlSpec::toggle("layer.dead", "Dead switch", "Nothing reads this.", false)).unwrap();
    let report = reg.audit();
    assert!(!report.is_clean());
    assert_eq!((report.total, report.consumed, report.orphans.clone()), (7, 6, vec!["layer.dead".to_string()]));
    reg.register_consumer("layer.dead", "render.dead-switch").unwrap();
    assert!(reg.audit().is_clean());
    // A consumer for a control that is not registered cannot be declared.
    assert!(matches!(reg.register_consumer("layer.ghost", "x"), Err(ControlError::UnknownControl { .. })));
}

#[test]
fn registry_refuses_invalid_descriptors() {
    let mut reg = fixture();
    assert!(matches!(reg.register(opacity()), Err(ControlError::DuplicateControl { .. })));
    let invalid = |s: ControlSpec| matches!(Registry::new().register(s), Err(ControlError::InvalidDescriptor { .. }));
    assert!(invalid(ControlSpec::toggle("Bad Id", "x", "h", true)));
    assert!(invalid(ControlSpec::toggle("ok.id", "x", "  ", true)), "help text is required");
    // Soft range outside hard range; slider without a complete range; default outside hard range.
    let mut n = NumberSpec::slider(2, (0.0, 1.0), (0.2, 1.5));
    assert!(invalid(ControlSpec::number("a", "A", "h", n.clone(), 0.5)));
    n = NumberSpec::field(2, Some(0.0), Some(1.0));
    n.widget = hsk_studio_controls::NumberWidget::Slider;
    n.hard_max = None;
    assert!(invalid(ControlSpec::number("b", "B", "h", n, 0.5)));
    assert!(matches!(
        Registry::new().register(ControlSpec::number("c", "C", "h", NumberSpec::field(2, Some(0.0), Some(1.0)), 3.0)),
        Err(ControlError::OutOfHardBounds { .. })
    ));
    assert!(invalid(ControlSpec::choice("d", "D", "h", vec![ChoiceOption::new("a", "A"), ChoiceOption::new("a", "B")], "a")));
}
