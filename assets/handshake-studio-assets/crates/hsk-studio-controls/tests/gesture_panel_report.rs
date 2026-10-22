//! Slice 2 focused tests (<= 4): spec-aligned gesture semantics + undo coalescing, headless egui
//! gestures and collapsible sections, section projection/audit, one Observe frame per operation.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_controls::egui_adapter::{PanelOutcome, show_panel_and_apply};
use hsk_studio_controls::projection::{agent_json, manual_topics, registry_from_json};
use hsk_studio_controls::report::{ReportContext, report};
use hsk_studio_controls::{
    ChangeSource, ControlError, ControlSpec, EventPhase, GesturePhase, GestureRoles, IntegerSpec, Interaction, NumberSpec,
    Registry, ScrubGesture, SectionSpec, SoftState, Value, ValueStore,
};
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};

fn num(v: f64) -> Value {
    Value::Number(v)
}

fn opacity() -> ControlSpec {
    ControlSpec::number(
        "layer.opacity",
        "Opacity",
        "How much of the layer shows through.",
        NumberSpec::slider(2, (0.0, 1.0), (0.2, 0.8)),
        0.5,
    )
}

fn fps() -> ControlSpec {
    ControlSpec::integer("adv.fps", "Frame rate", "Frames per second.", IntegerSpec::slider((1, 120), (12, 60)), 24)
        .with_unit("fps")
        .in_group(&["adv"])
}

fn panel() -> Registry {
    let mut r = Registry::new();
    r.register(opacity()).unwrap();
    r.register_section(SectionSpec::new("adv", "Advanced", "Rarely used timing options.")).unwrap();
    r.register_section(SectionSpec::new("extra", "Extra", "Naming options.").collapsed()).unwrap();
    r.register(fps()).unwrap();
    r.register(ControlSpec::text("extra.name", "Name", "Display name.", 32, false, "Layer 1").in_group(&["extra"])).unwrap();
    for id in ["layer.opacity", "adv.fps", "extra.name"] {
        r.register_consumer(id, "test.consumer").unwrap();
    }
    r
}

#[test]
fn gesture_semantics_integer_domain_and_undo_coalescing() {
    let reg = panel();
    let spec = reg.get("layer.opacity").unwrap();

    // STU-SHL-190 roles: x1, coarse x10, fine /10, both x100.
    let roles = |coarse, fine| GestureRoles { coarse, fine };
    assert_eq!(
        [roles(false, false), roles(true, false), roles(false, true), roles(true, true)].map(GestureRoles::factor),
        [1.0, 10.0, 0.1, 100.0]
    );

    // STU-SHL-191 sensitivity: max(q, soft_range/200) = max(0.01, 0.003); else anchor based, frozen.
    let mut g = ScrubGesture::begin(spec, &num(0.5)).unwrap();
    assert_eq!(g.step(), 0.01);
    assert_eq!(g.drag(spec, 30.0, GestureRoles::NONE).unwrap(), num(0.8));
    // Crosses the soft bound, clamps at the hard bound only; dragging back returns immediately.
    let top = g.drag(spec, 30.0, GestureRoles::NONE).unwrap();
    assert_eq!((top.clone(), spec.soft_state(&top)), (num(1.0), SoftState::OutOfSoft));
    assert_eq!(g.drag(spec, -10.0, GestureRoles::NONE).unwrap(), num(0.9));
    assert_eq!(g.drag(spec, -1.0, roles(true, false)).unwrap(), num(0.8), "coarse applies once");
    assert_eq!(g.drag(spec, 1.0, roles(false, true)).unwrap(), num(0.801), "fine applies once");
    assert!(matches!(g.drag(spec, f64::NAN, GestureRoles::NONE), Err(ControlError::NonFinite { .. })));
    // No usable soft range: max(q, max(|anchor|, 1)/200), frozen at press.
    let blur = ControlSpec::number("blur", "Blur", "h", NumberSpec::field(1, Some(0.0), None), 50.0);
    assert_eq!(ScrubGesture::begin(&blur, &num(50.0)).unwrap().step(), 0.25);

    // Integer domain: step = coarse = fine = 1, modifiers never skip; typed/pointer rules hold.
    let fps = reg.get("adv.fps").unwrap();
    let both = roles(true, true);
    assert_eq!(fps.step_value(&Value::Int(24), 1, both).unwrap(), Value::Int(25));
    assert_eq!(fps.step_value(&Value::Int(24), -1, roles(true, false)).unwrap(), Value::Int(23));
    assert_eq!(
        fps.step_value(&Value::Int(120), 1, GestureRoles::NONE).unwrap_err(),
        ControlError::OutOfHardBoundsInt { control_id: "adv.fps".into(), value: 121, hard_min: Some(1), hard_max: Some(120) }
    );
    assert_eq!(fps.parse_text("30 fps").unwrap(), Value::Int(30));
    assert!(matches!(fps.parse_text("3.5"), Err(ControlError::InvalidText { .. })));
    assert!(matches!(fps.parse_text("12+3"), Err(ControlError::InvalidText { .. })));
    assert!(matches!(fps.parse_text("200"), Err(ControlError::OutOfHardBoundsInt { .. })));
    assert_eq!(fps.pointer_value(200.4).unwrap(), Value::Int(120));
    assert_eq!(fps.soft_state(&Value::Int(90)), SoftState::OutOfSoft);
    let mut gi = ScrubGesture::begin(fps, &Value::Int(24)).unwrap();
    assert_eq!(gi.step(), 0.24, "integer: no declared precision, soft range 48 / 200");
    assert_eq!(gi.drag(fps, 10.0, GestureRoles::NONE).unwrap(), Value::Int(26));

    // Store: a gesture is many live updates and ONE undo step; abort restores and is not undoable.
    let mut store = ValueStore::new(&reg);
    let id = "layer.opacity";
    let upd = |v: f64| Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::Update, Ok(num(v)));
    let ev1 = store.apply(&reg, &upd(0.6), None).unwrap().unwrap();
    let ev2 = store.apply(&reg, &upd(0.7), None).unwrap().unwrap();
    assert!(matches!((ev1.phase, ev2.phase), (EventPhase::Live(a), EventPhase::Live(b)) if a == b));
    assert!(!ev1.is_undo_step() && !ev2.is_undo_step() && store.gesture_active(id));
    assert!(matches!(store.set(&reg, id, num(0.1), ChangeSource::Model, None), Err(ControlError::Gesture { .. })));
    assert!(matches!(store.undo(&reg, &ev2, None), Err(ControlError::Gesture { .. })), "live samples are not undo steps");
    let end = Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::End, Ok(num(0.7)));
    let done = store.apply(&reg, &end, None).unwrap().unwrap();
    assert!(matches!(done.phase, EventPhase::Committed(_)) && done.is_undo_step());
    assert_eq!((done.old.clone(), done.new.clone(), store.gesture_active(id)), (num(0.5), num(0.7), false));
    store.undo(&reg, &done, None).unwrap();
    assert_eq!(store.get(id), Some(&num(0.5)), "one undo reverts the whole gesture");

    store.apply(&reg, &upd(0.9), None).unwrap();
    let abort = Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::Abort, Ok(num(0.9)));
    let aborted = store.apply(&reg, &abort, None).unwrap().unwrap();
    assert!(matches!(aborted.phase, EventPhase::Aborted(_)) && !aborted.is_undo_step());
    assert_eq!(store.get(id), Some(&num(0.5)));
    // A gesture that ends where it began leaves no undo step.
    store.apply(&reg, &upd(0.6), None).unwrap();
    store.apply(&reg, &upd(0.5), None).unwrap();
    let net_zero = Interaction::gesture(id, ChangeSource::Pointer, GesturePhase::End, Ok(num(0.5)));
    assert_eq!(store.apply(&reg, &net_zero, None).unwrap(), None);
}

fn key(k: Key, pressed: bool) -> Event {
    Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
}

struct Harness {
    ctx: egui::Context,
    t: f64,
    origin: Pos2,
    reg: Registry,
    store: ValueStore,
    all: Vec<hsk_studio_controls::ChangeEvent>,
}

impl Harness {
    fn new() -> Self {
        let reg = panel();
        let store = ValueStore::new(&reg);
        Self { ctx: egui::Context::default(), t: 0.0, origin: Pos2::ZERO, reg, store, all: Vec::new() }
    }

    fn frame(&mut self, events: Vec<Event>) -> PanelOutcome {
        self.t += 0.1;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))),
            time: Some(self.t),
            events,
            ..Default::default()
        };
        let (reg, store, origin) = (&self.reg, &mut self.store, &mut self.origin);
        let mut outcome = PanelOutcome::default();
        let _ = self.ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                *origin = ui.next_widget_position();
                outcome = show_panel_and_apply(ui, reg, store, None);
            });
        });
        self.all.extend(outcome.changes.clone());
        outcome
    }

    fn settle(&mut self, n: usize) -> PanelOutcome {
        let mut last = PanelOutcome::default();
        for _ in 0..n {
            last = self.frame(vec![]);
        }
        last
    }
}

#[test]
fn headless_egui_label_scrub_is_one_gesture_and_sections_collapse() {
    let mut h = Harness::new();
    let last = h.settle(3);
    // adv is open, extra starts collapsed: its control is not rendered.
    assert_eq!(last.rendered, ["layer.opacity", "adv.fps"], "{:?}", last.sections);
    assert_eq!(last.sections.iter().map(|s| (s.id.as_str(), s.open)).collect::<Vec<_>>(), [("adv", true), ("extra", false)]);

    // Clicking the collapsed section header opens it.
    let p = last.sections[1].header.center();
    h.frame(vec![Event::PointerMoved(p)]);
    h.frame(vec![button(p, true)]);
    h.frame(vec![button(p, false)]);
    let last = h.settle(6);
    assert!(last.rendered.contains(&"extra.name".to_string()), "{:?} {:?}", last.rendered, last.sections);

    // Scrub the opacity label: press, drag right, release = live samples then ONE undo step.
    let before = h.store.get("layer.opacity").cloned();
    let p = h.origin + vec2(6.0, 8.0);
    h.frame(vec![Event::PointerMoved(p)]);
    h.frame(vec![button(p, true)]);
    h.frame(vec![Event::PointerMoved(p + vec2(20.0, 0.0))]);
    h.frame(vec![Event::PointerMoved(p + vec2(40.0, 0.0))]);
    h.frame(vec![Event::PointerMoved(p + vec2(40.0, 0.0))]);
    h.frame(vec![button(p + vec2(40.0, 0.0), false)]);
    h.settle(2);
    let live = h.all.iter().filter(|e| e.control_id == "layer.opacity" && matches!(e.phase, EventPhase::Live(_))).count();
    let steps: Vec<_> = h.all.iter().filter(|e| e.control_id == "layer.opacity" && e.is_undo_step()).collect();
    assert!(live >= 1, "{:?}", h.all);
    assert_eq!(steps.len(), 1, "{:?}", h.all);
    assert!(matches!(steps[0].phase, EventPhase::Committed(_)));
    assert_eq!(Some(&steps[0].old), before.as_ref());
    assert_eq!(h.store.get("layer.opacity"), Some(&steps[0].new));
    assert!(steps[0].new != steps[0].old && !h.store.gesture_active("layer.opacity"));

    // Escape during a drag aborts: value restored, aborted event, no undo step.
    let pre = h.store.get("layer.opacity").cloned();
    let n_before = h.all.len();
    h.frame(vec![Event::PointerMoved(p)]);
    h.frame(vec![button(p, true)]);
    h.frame(vec![Event::PointerMoved(p - vec2(20.0, 0.0))]);
    h.frame(vec![Event::PointerMoved(p - vec2(40.0, 0.0))]);
    h.frame(vec![key(Key::Escape, true), key(Key::Escape, false)]);
    h.frame(vec![button(p - vec2(40.0, 0.0), false)]);
    h.settle(2);
    let new_events = &h.all[n_before..];
    assert!(new_events.iter().any(|e| matches!(e.phase, EventPhase::Aborted(_))), "{new_events:?}");
    assert!(new_events.iter().all(|e| !e.is_undo_step()), "{new_events:?}");
    assert_eq!(h.store.get("layer.opacity"), pre.as_ref());
    assert!(!h.store.gesture_active("layer.opacity"));
}

#[test]
fn sections_project_to_json_and_manual_and_are_audited() {
    let reg = panel();
    let store = ValueStore::new(&reg);
    let json = agent_json(&reg, Some(&store));
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let sections = doc["sections"].as_array().unwrap();
    assert_eq!(sections.iter().map(|s| s["id"].as_str().unwrap()).collect::<Vec<_>>(), ["adv", "extra"]);
    assert!(sections[1]["collapsed_by_default"] == true && sections[0]["collapsed_by_default"] == false);
    assert!(sections[0]["controls"][0] == "adv.fps" && sections[1]["controls"][0] == "extra.name");
    let fps = doc["controls"].as_array().unwrap().iter().find(|c| c["id"] == "adv.fps").unwrap();
    assert!(fps["group_path"][0] == "adv" && fps["is_default"] == true, "{fps}");
    assert!(fps["kind"]["step"] == 1 && fps["kind"]["fine_step"] == 1 && fps["kind"]["coarse_step"] == 1, "{fps}");
    assert!(fps["kind"]["hard_min"] == 1 && fps["kind"]["soft_max"] == 60, "{fps}");

    // Same descriptors round-trip byte-identically, sections and group paths included.
    let again = registry_from_json(&json).unwrap();
    assert_eq!((again.specs(), again.sections()), (reg.specs(), reg.sections()));
    assert_eq!(agent_json(&again, Some(&ValueStore::new(&again))), json);

    // Manual topics come from the same descriptors: section topics list members, controls show the path.
    let topics = manual_topics(&reg);
    assert_eq!(topics.len(), 5);
    let adv = topics.iter().find(|t| t.id == "adv").unwrap();
    assert!(adv.body.contains("Frame rate (`adv.fps`)") && adv.body.contains("Starts open."), "{}", adv.body);
    assert!(topics.iter().find(|t| t.id == "extra").unwrap().body.contains("Starts collapsed."));
    let t = topics.iter().find(|t| t.id == "adv.fps").unwrap();
    assert!(t.body.contains("Section: adv.") && t.body.contains("never skip"), "{}", t.body);

    // Audit: an empty section is reported; group paths must be declared parent chains; ids are shared.
    let mut reg = reg;
    assert!(reg.audit().is_clean(), "{}", reg.audit());
    reg.register_section(SectionSpec::new("ghost", "Ghost", "Nothing inside.")).unwrap();
    assert_eq!(reg.audit().empty_sections, ["ghost"]);
    let toggle = |id: &str| ControlSpec::toggle(id, "T", "h", true);
    assert!(matches!(reg.register(toggle("t1").in_group(&["nope"])), Err(ControlError::InvalidDescriptor { .. })));
    assert!(matches!(reg.register_section(SectionSpec::new("kid", "Kid", "h").within("nope")), Err(ControlError::InvalidDescriptor { .. })));
    reg.register_section(SectionSpec::new("kid", "Kid", "h").within("adv")).unwrap();
    assert!(matches!(reg.register(toggle("t2").in_group(&["extra", "kid"])), Err(ControlError::InvalidDescriptor { .. })), "kid is not under extra");
    assert!(reg.register(toggle("t3").in_group(&["adv", "kid"])).is_ok());
    assert!(matches!(reg.register(toggle("adv")), Err(ControlError::DuplicateControl { .. })), "control id must not shadow a section id");
}

#[derive(Default)]
struct Collect(Vec<(bool, Vec<u8>)>);

impl SinkPort for Collect {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((matches!(class, DeliveryClass::Terminal), bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn one_observe_terminal_frame_per_operation_with_redacted_values() {
    let reg = panel();
    let mut store = ValueStore::new(&reg);
    let ctx = ReportContext {
        correlation_id: 7,
        resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap(),
        actor: ActorContext::new("acct", "princ", "acct", "princ", "space", "sess").unwrap(),
    };
    let live = CancellationToken::default();
    let mut sink = Collect::default();
    let id = "layer.opacity";
    let run = |store: &mut ValueStore, i: Interaction, sink: &mut Collect, token: &CancellationToken| {
        let result = store.apply(&reg, &i, None);
        report(&ctx, store.revision(), i.gesture, &result, token, sink).unwrap().map(|r| r.outcome)
    };

    // Discrete accepted edit -> Success; unchanged value -> Success; exactly one terminal frame each.
    let set = |v: f64| Interaction::discrete(id, ChangeSource::Typed, Ok(num(v)));
    assert_eq!(run(&mut store, set(0.75), &mut sink, &live), Some(Outcome::Success));
    assert_eq!(run(&mut store, set(0.75), &mut sink, &live), Some(Outcome::Success));
    assert_eq!(sink.0.len(), 2);
    assert!(sink.0.iter().all(|(terminal, _)| *terminal));

    // Live gesture samples emit nothing; the committed gesture emits one Success frame.
    let g = |p, v: f64| Interaction::gesture(id, ChangeSource::Pointer, p, Ok(num(v)));
    assert_eq!(run(&mut store, g(GesturePhase::Update, 0.8), &mut sink, &live), None);
    assert_eq!(run(&mut store, g(GesturePhase::Update, 0.9), &mut sink, &live), None);
    assert_eq!(sink.0.len(), 2);
    assert_eq!(run(&mut store, g(GesturePhase::End, 0.9), &mut sink, &live), Some(Outcome::Success));
    assert_eq!(sink.0.len(), 3);

    // Abort -> Canceled.
    run(&mut store, g(GesturePhase::Update, 0.3), &mut sink, &live);
    assert_eq!(run(&mut store, g(GesturePhase::Abort, 0.3), &mut sink, &live), Some(Outcome::Canceled));

    // Rejections -> Failure class from the closed vocabulary; the value and control id never reach the sink.
    let before = sink.0.len();
    let bad = Interaction::discrete(id, ChangeSource::Typed, reg.get(id).unwrap().parse_text("1.2345"));
    assert_eq!(run(&mut store, bad, &mut sink, &live), Some(Outcome::Failure(FailureCode::Validation)));
    let ghost = Interaction::discrete("ghost.control", ChangeSource::Model, Ok(num(1.0)));
    assert_eq!(run(&mut store, ghost, &mut sink, &live), Some(Outcome::Failure(FailureCode::Unsupported)));
    assert_eq!(sink.0.len(), before + 2);
    for (_, bytes) in &sink.0 {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("1.2345") && !text.contains("layer.opacity") && !text.contains("ghost"), "{text}");
    }

    // A cancelled token makes Observe record Canceled whatever the result.
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(run(&mut store, set(0.2), &mut sink, &cancelled), Some(Outcome::Canceled));
}
