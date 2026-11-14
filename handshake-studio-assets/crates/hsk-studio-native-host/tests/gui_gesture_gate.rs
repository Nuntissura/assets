//! SMB-GUI-GESTURE-GATE-1: the STU-SHL-109 arbitration acceptance tests #1 to #6 and #7a on the GUI frame, driven by
//! typed HostEvent steps only (no winit, no OS cursor, never OS input), plus the host clock and the caller's actor.
//! The field is the selected layer's opacity, `studio.panel.properties.opacity` in the Properties body (lead ruling P),
//! committed once per gesture through `Viewer::set_opacity` (catalog id `layer.set_opacity`). #7b (the untimed paths
//! reachable from the menu and the palette) waits for the palette lane (STU-SHL-041..044): NOT_PROVEN here, and the
//! spine GATE_CHECK stays blocked on it. Feature `document`, CPU frame (no device), except the quiet-window clock proof,
//! which runs a window on the real device (GPU test group).
#![cfg(feature = "document")]
mod common;

use common::{Scratch, fixture};
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_native_host::cli::Caller;
use hsk_studio_native_host::diag::HostRun;
use hsk_studio_native_host::event::{CursorRequest, HostEvent, Key, Mods};
use hsk_studio_native_host::gui::{self, GESTURE_ID, GuiFrame, OPACITY_ID, Outcome, PaneSink, Untimed, row_id};
use hsk_studio_native_host::scene::{GuiArgs, Scene};
use hsk_studio_native_host::script::Step;
use hsk_studio_native_host::window::{self, WindowArgs};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_vulkan::Refusal;
use std::time::Duration;

const PLAIN: Mods = Mods { ctrl: false, shift: false, alt: false };
const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false };
const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false };
const SHIFT_CTRL: Mods = Mods { ctrl: true, shift: true, alt: false };
const ALT: Mods = Mods { ctrl: false, shift: false, alt: true };
/// Wheel idle window of STU-SHL-107.
const IDLE_MS: u64 = 400;

fn open_at(dir: &std::path::Path, w: u32, h: u32) -> GuiFrame {
    let mut f = GuiFrame::open(&fixture(dir), w, h, Some(1000)).unwrap_or_else(|e| panic!("gui open: {e}"));
    f.frame().expect("first frame");
    f
}
fn open(dir: &std::path::Path) -> GuiFrame {
    open_at(dir, 1920, 1080)
}
fn layer(f: &GuiFrame, name: &str) -> String {
    f.rows().iter().find(|r| r.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id.clone()
}
/// Selects layer `name` (view state, no history step) and returns its id.
fn select(f: &mut GuiFrame, name: &str) -> String {
    let id = layer(f, name);
    f.invoke(&row_id(&id)).expect("select");
    f.frame().unwrap();
    assert_eq!(f.selected(), Some(id.as_str()));
    id
}
/// Applies one event, then paints what changed.
fn ev(f: &mut GuiFrame, e: HostEvent) -> Option<Outcome> {
    let out = f.apply(e);
    f.frame().unwrap();
    out
}
fn to(f: &mut GuiFrame, x: i32, y: i32) -> Option<Outcome> {
    ev(f, HostEvent::PointerMoved { x, y })
}
fn press(f: &mut GuiFrame) -> Option<Outcome> {
    ev(f, HostEvent::Button { button: hsk_studio_native_host::event::Button::Left, pressed: true })
}
fn release(f: &mut GuiFrame) -> Option<Outcome> {
    ev(f, HostEvent::Button { button: hsk_studio_native_host::event::Button::Left, pressed: false })
}
fn mods(f: &mut GuiFrame, m: Mods) {
    ev(f, HostEvent::Modifiers(m));
}
fn key(f: &mut GuiFrame, key: Key, mods: Mods) -> Option<Outcome> {
    ev(f, HostEvent::Key { key, pressed: true, mods })
}
fn notch(f: &mut GuiFrame, down: bool) {
    ev(f, HostEvent::Wheel { x: 0, y: if down { -1000 } else { 1000 }, pixels: false });
}
/// The field's centre, physical px (scale 1 in these tests).
fn field(f: &GuiFrame) -> (i32, i32) {
    f.centre(OPACITY_ID).expect("the opacity field shows in the Properties body while a layer is selected")
}
/// The value the field shows (its node value, `81.5%`).
fn shown(f: &GuiFrame) -> f64 {
    let v = f.node(OPACITY_ID).and_then(|n| n.value.clone()).expect("the field carries its value");
    v.trim_end_matches('%').parse().unwrap_or_else(|_| panic!("field value {v}"))
}
/// The document's opacity of `layer`, percent.
fn opacity(f: &GuiFrame, layer: &str) -> f64 {
    f.rows().iter().find(|r| r.layer_id == layer).map(|r| r.opacity).expect("row")
}
fn gesture(f: &GuiFrame) -> String {
    f.node(GESTURE_ID).and_then(|n| n.value.clone()).expect("studio.gesture.active exists")
}
/// The history entries of the viewer's run.
fn entries(f: &GuiFrame) -> usize {
    f.viewer().run().history()["undo_steps"].as_array().map_or(0, Vec::len)
}
/// The newest history entry: (command id, actor account, client session).
fn newest(f: &GuiFrame) -> (String, String, String) {
    let h = f.viewer().run().history();
    let s = h["undo_steps"].as_array().and_then(|a| a.last()).cloned().expect("a history entry");
    (
        s["command_id"].as_str().unwrap_or_default().to_owned(),
        s["actor"]["account_id"].as_str().unwrap_or_default().to_owned(),
        s["client_session"].as_str().unwrap_or_default().to_owned(),
    )
}
/// The receipt's gesture counters as rendered JSON.
fn counters(f: &GuiFrame) -> String {
    f.receipt().get("frame").and_then(|fr| fr.get("gesture")).map(|g| g.render()).expect("frame.gesture in the receipt")
}

/// STU-SHL-109 #1 (STU-SHL-101, -104, -108): a scrub that begins in the field crosses the Colour group's tab strip and
/// the Inspect Dock's edge (where its splitter sits) and is released over a tab: studio.dock.state is byte-identical,
/// studio.gesture.active reads the field's author_id and `scrub` at every sample, no other control shows hover, no tip
/// fires, the tab under the release is not selected, and the gesture is one history entry.
#[test]
fn t1_a_scrub_across_a_tab_strip_and_the_dock_edge_moves_no_dock_state() {
    let s = Scratch::new("gate-1");
    let mut f = open(&s.0);
    select(&mut f, "Green");
    let dock = f.dock_state();
    let dock_node = f.node("studio.dock.state").and_then(|n| n.value.clone());
    let (fx, fy) = field(&f);
    let (tx, ty) = f.centre("studio.tab.swatches").expect("the Colour group's Swatches tab");
    let edge = f.scale().snap(f.regions().dock).x0;
    assert_eq!(gesture(&f), "none");
    let before = entries(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    let want = format!("{OPACITY_ID} scrub");
    assert_eq!(gesture(&f), want);
    let mut path: Vec<(i32, i32)> = (1..=10).map(|i| (fx + (tx - fx) * i / 10, fy + (ty - fy) * i / 10)).collect();
    path.extend([(edge + 2, ty), (edge, ty), (edge - 1, ty), (edge - 12, ty), (edge - 40, ty + 20)]);
    path.extend((1..=5).map(|i| (edge - 40 + (tx - edge + 40) * i / 5, ty + 20 - 20 * i / 5)));
    for (x, y) in path {
        to(&mut f, x, y);
        assert_eq!(gesture(&f), want, "at ({x}, {y})");
        assert!(!f.node("studio.tab.swatches").unwrap().flags.hovered, "no other control shows hover (STU-SHL-104)");
    }
    f.advance(1000);
    f.frame().unwrap();
    assert_eq!(f.tip(), None, "no tooltip fires during an owned gesture (STU-SHL-104)");
    let out = release(&mut f);
    assert_eq!(out.map(|o| o.command), Some("layer.set_opacity"));
    assert_eq!(f.active_tab("studio.group.colour"), Some("colour"), "the release over a tab selects nothing");
    assert_eq!(f.dock_state(), dock, "studio.dock.state byte-identical");
    assert_eq!(f.node("studio.dock.state").and_then(|n| n.value.clone()), dock_node);
    assert_eq!(gesture(&f), "none");
    assert_eq!(entries(&f), before + 1, "one history entry for the gesture");
}

/// STU-SHL-109 #2 (STU-SHL-104, -106, -192): past the window edge the value keeps changing; the cursor request is
/// Hidden at every sample of the drag and shows at the press point on release; no request ever moves the pointer.
/// Modifiers are sampled continuously in value space (no jump), the clamp holds the clamped value so dragging back
/// returns at once, and hostile coordinates stay bounded.
#[test]
fn t2_past_the_window_edge_the_value_keeps_changing_and_the_cursor_is_hidden_then_shown_at_the_press() {
    let s = Scratch::new("gate-2");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    assert_eq!(f.cursor(), CursorRequest::ColumnResize, "column-resize over the field (STU-SHL-186)");
    press(&mut f);
    mods(&mut f, SHIFT);
    to(&mut f, fx - 10, fy);
    assert_eq!(shown(&f), 50.0, "Shift x10: 10 px left at 0.5 %/px is -50");
    assert_eq!(f.cursor(), CursorRequest::Hidden);
    mods(&mut f, CTRL);
    assert_eq!(shown(&f), 50.0, "a modifier change alone never jumps the value");
    let width = i32::try_from(f.extent().0).unwrap();
    let mut last = shown(&f);
    for x in [width - 10, width, width + 30, width + 80, width + 280, width + 480] {
        to(&mut f, x, fy);
        assert_eq!(f.cursor(), CursorRequest::Hidden, "hidden at every sample of the drag, never a pointer move");
        let v = shown(&f);
        assert!(v > last, "the value keeps changing at x {x} (edge {width}): {v} after {last}");
        last = v;
    }
    // Hostile coordinates: bounded, clamped at the hard limits, no panic.
    to(&mut f, i32::MAX, i32::MIN);
    assert_eq!(shown(&f), 100.0);
    to(&mut f, i32::MIN, i32::MAX);
    assert_eq!(shown(&f), 0.0);
    // Dragging back returns at once from the clamp (STU-SHL-192): Ctrl /10, 10 px right is +0.5.
    to(&mut f, i32::MIN + 10, i32::MAX);
    assert_eq!(shown(&f), 0.5);
    assert_eq!(f.cursor(), CursorRequest::Hidden);
    release(&mut f);
    assert_eq!(f.cursor(), CursorRequest::ShowAt { x: fx, y: fy }, "the cursor shows again at the press point");
    assert_eq!(opacity(&f, &green), 0.5);
    mods(&mut f, PLAIN);
}

/// STU-SHL-106: Shift x10, Ctrl /10, Shift+Ctrl x100, Alt is never a magnitude.
#[test]
fn modifiers_scale_the_step_and_alt_is_not_a_magnitude() {
    let s = Scratch::new("gate-mods");
    let mut f = open(&s.0);
    select(&mut f, "Green");
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    mods(&mut f, ALT);
    to(&mut f, fx - 10, fy);
    assert_eq!(shown(&f), 95.0, "Alt: the plain 0.5 %/px");
    mods(&mut f, SHIFT_CTRL);
    to(&mut f, fx - 11, fy);
    assert_eq!(shown(&f), 45.0, "Shift+Ctrl x100: 1 px is 50");
    mods(&mut f, CTRL);
    to(&mut f, fx - 21, fy);
    assert_eq!(shown(&f), 44.5, "Ctrl /10: 10 px is 0.5");
    mods(&mut f, PLAIN);
    to(&mut f, fx - 21, fy + 4);
    assert_eq!(shown(&f), 42.5, "delta = dx - dy: 4 px down is -2 (STU-SHL-188)");
    key(&mut f, Key::Escape, PLAIN);
    release(&mut f);
}

/// STU-SHL-109 #3 (STU-SHL-104, -194): press, drag, Escape: the value is the pre-press value and zero history entries
/// were written, through the release.
#[test]
fn t3_escape_restores_the_pre_press_value_and_writes_no_entry() {
    let s = Scratch::new("gate-3");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let before = entries(&f);
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    to(&mut f, fx - 20, fy);
    assert_eq!(shown(&f), 90.0);
    key(&mut f, Key::Escape, PLAIN);
    assert_eq!(shown(&f), 100.0, "Escape restores the pre-press value");
    to(&mut f, fx - 60, fy);
    assert_eq!(shown(&f), 100.0, "an aborted gesture changes nothing more");
    assert_eq!(release(&mut f), None);
    assert_eq!(opacity(&f, &green), 100.0);
    assert_eq!(entries(&f), before, "zero history entries");
    let c = counters(&f);
    assert!(c.contains(r#""aborts":1"#) && c.contains(r#""commits":0"#), "{c}");
}

/// STU-SHL-109 #4 (STU-SHL-107): 500 pointer samples are exactly one history entry, named by the catalog id.
#[test]
fn t4_five_hundred_samples_are_one_entry() {
    let s = Scratch::new("gate-4");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let before = entries(&f);
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    for i in 0..500 {
        to(&mut f, fx - (i % 50), fy + (i % 7));
    }
    let want = shown(&f);
    let out = release(&mut f);
    assert_eq!(out, Some(Outcome { command: "layer.set_opacity", code: "ok".into() }));
    assert_eq!(entries(&f), before + 1, "exactly one history entry");
    assert_eq!(newest(&f).0, "layer.set_opacity", "the entry carries the catalog id");
    assert_eq!(opacity(&f, &green), want);
    assert!(counters(&f).contains(r#""commits":1"#), "{}", counters(&f));
}

/// STU-SHL-109 #5 (STU-SHL-105, -107, -189): five wheel notches inside the idle window, then the clock past 400 ms:
/// exactly one history entry; none before the idle window closes.
#[test]
fn t5_five_wheel_notches_then_the_idle_window_are_one_entry() {
    let s = Scratch::new("gate-5");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let before = entries(&f);
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    for _ in 0..5 {
        notch(&mut f, true);
        f.advance(100);
    }
    assert_eq!(shown(&f), 97.5, "each notch steps 0.5 (STU-SHL-189, -191)");
    assert_eq!(entries(&f), before, "no entry inside the idle window");
    f.advance(IDLE_MS - 100 - 1);
    assert_eq!(entries(&f), before, "still inside the idle window");
    f.advance(2);
    f.frame().unwrap();
    assert_eq!(entries(&f), before + 1, "exactly one history entry");
    assert_eq!(opacity(&f, &green), 97.5);
    assert_eq!(f.next_timer_ms(), None, "no timer left");
}

/// STU-SHL-109 #6 (STU-SHL-107, -192): the accessibility SetValue writes one entry and gives the identical clamped
/// result as the drag; hostile values are typed refusals with no entry.
#[test]
fn t6_set_value_is_one_entry_and_the_same_clamped_result_as_the_drag() {
    let s = Scratch::new("gate-6");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let (fx, fy) = field(&f);
    let undo = |f: &mut GuiFrame| key(f, Key::Char('z'), CTRL);
    for (dx, set) in [(37, 81.5), (37, 81.54), (1000, -250.0)] {
        to(&mut f, fx, fy);
        press(&mut f);
        to(&mut f, fx - dx, fy);
        release(&mut f);
        let dragged = opacity(&f, &green);
        undo(&mut f);
        assert_eq!(opacity(&f, &green), 100.0);
        let n = entries(&f);
        assert_eq!(f.set_value(OPACITY_ID, set), Ok(Some(Outcome { command: "layer.set_opacity", code: "ok".into() })));
        assert_eq!(entries(&f), n + 1, "SetValue is one entry");
        assert_eq!(opacity(&f, &green), dragged, "SetValue {set} equals the drag of {dx} px");
        undo(&mut f);
    }
    let n = entries(&f);
    assert_eq!(f.set_value(OPACITY_ID, f64::NAN), Err("value_not_finite"));
    assert_eq!(f.set_value(OPACITY_ID, f64::INFINITY), Err("value_not_finite"));
    assert_eq!(f.set_value("studio.panel.properties.bogus", 10.0), Err("unknown_author_id"));
    assert_eq!(f.set_value(OPACITY_ID, 100.0), Ok(None), "an unchanged value writes nothing");
    assert_eq!(entries(&f), n);
}

/// STU-SHL-109 #7a: the frame lists its timed gestures (the list itself is enumerated, not a hand list); each has an
/// untimed path the inspector drives now, with no clock. #7b (menu and palette) is NOT_PROVEN here.
#[test]
fn t7a_every_timed_gesture_has_an_untimed_path() {
    let timed = gui::timed_gestures();
    for id in ["tooltip.l0", "tooltip.l1", "scrub.wheel_idle"] {
        assert!(timed.iter().any(|t| t.id == id), "{id} in {timed:?}");
    }
    let s = Scratch::new("gate-7");
    for t in timed {
        assert!(t.after_ms > 0, "{t:?}");
        let mut f = open(&s.0);
        let green = select(&mut f, "Green");
        match t.untimed {
            Untimed::ShowTip { level } => {
                let r = f.show_tip(OPACITY_ID, level).unwrap_or_else(|e| panic!("{}: {e}", t.id));
                assert_eq!((r.level, r.name.as_str()), (level, "Opacity"), "{}", t.id);
                assert_eq!(f.tip().map(|r| r.level), Some(level), "{}: shown without the clock", t.id);
            }
            Untimed::CloseTransaction => {
                let (fx, fy) = field(&f);
                for close in [HostEvent::PointerLeft, HostEvent::Focus { gained: false }] {
                    let n = entries(&f);
                    to(&mut f, fx, fy);
                    notch(&mut f, true);
                    notch(&mut f, true);
                    assert_eq!(entries(&f), n);
                    ev(&mut f, close);
                    assert_eq!(entries(&f), n + 1, "{}: {close:?} closes the transaction at once", t.id);
                }
                assert_eq!(opacity(&f, &green), 98.0);
            }
        }
    }
}

/// Focus loss finalizes an open drag once (STU-SHL-194); the later release writes nothing more.
#[test]
fn focus_loss_finalizes_a_drag_once() {
    let s = Scratch::new("gate-focus");
    let mut f = open(&s.0);
    let green = select(&mut f, "Green");
    let n = entries(&f);
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    to(&mut f, fx - 4, fy);
    ev(&mut f, HostEvent::Focus { gained: false });
    assert_eq!(entries(&f), n + 1);
    assert_eq!(gesture(&f), "none");
    assert_eq!(release(&mut f), None);
    assert_eq!(entries(&f), n + 1);
    assert_eq!(opacity(&f, &green), 98.0);
}

/// Host clock, agent mode (STU-SHL-235 rule 5, lead ruling 3): an agent-driven frame shows no dwell tip after the
/// clock passes the dwell, while the same clock still closes the wheel idle window; the Operator's frame shows the tip.
#[test]
fn agent_mode_has_no_dwell_tip_but_its_clock_closes_the_wheel_idle() {
    let s = Scratch::new("gate-agent");
    let mut operator = open(&s.0);
    let (tx, ty) = operator.centre("studio.tab.swatches").unwrap();
    to(&mut operator, tx, ty);
    operator.advance(1000);
    assert!(operator.tip().is_some(), "the Operator's frame shows the dwell tip");

    let mut f = open(&s.0);
    f.set_agent_driven(true);
    to(&mut f, tx, ty);
    f.advance(1000);
    assert_eq!(f.tip(), None, "no dwell tip in an agent-driven frame");
    let green = select(&mut f, "Green");
    let n = entries(&f);
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    notch(&mut f, true);
    assert_eq!(f.next_timer_ms(), Some(IDLE_MS));
    f.advance(150);
    assert_eq!(f.next_timer_ms(), Some(IDLE_MS - 150));
    f.advance(IDLE_MS);
    assert_eq!(entries(&f), n + 1);
    assert_eq!(opacity(&f, &green), 99.5);
    assert_eq!(f.next_timer_ms(), None);
}

/// Offscreen drive settles pending gesture timers at the end of its script (lead ruling 3): a script ending in wheel
/// notches is one entry without a clock.
#[test]
fn drive_settles_the_wheel_idle_at_the_end_of_the_script() {
    struct Sink;
    impl PaneSink for Sink {
        fn upload(&mut self, _: &[u8]) -> Result<(), Refusal> {
            Ok(())
        }
    }
    let s = Scratch::new("gate-drive");
    let mut probe = open(&s.0);
    let green = select(&mut probe, "Green");
    let (fx, fy) = field(&probe);
    let mut f = open(&s.0);
    let n = entries(&f);
    let mut steps = vec![Step::Invoke(row_id(&green)), Step::Event(HostEvent::PointerMoved { x: fx, y: fy })];
    steps.extend((0..5).map(|_| Step::Event(HostEvent::Wheel { x: 0, y: -1000, pixels: false })));
    gui::drive(&mut f, &mut Sink, &steps).expect("drive");
    assert_eq!(entries(&f), n + 1);
    assert_eq!(opacity(&f, &green), 97.5);
}

/// HOST INTEGRATION (4): the gesture's history entry carries the host run's actor (GuiFrame::open_as from the caller),
/// never the asset-local test host's; the eye toggle on the same viewer does too (V4-06 sibling).
#[test]
fn the_gesture_entry_carries_the_host_runs_actor() {
    let s = Scratch::new("gate-actor");
    let actor = ActorContext::new("gate-actor", "gate-actor", "gate-owner", "gate-owner", "gate-space", "gate-session").unwrap();
    let (cancel, mut frames) = (CancellationToken::default(), Frames::default());
    let host = HostRun::new(actor, Caller::asset_local().resource, 7, &cancel, &mut frames).expect("host run");
    let mut f = GuiFrame::open_as(&fixture(&s.0), 1920, 1080, Some(1000), host.actor()).expect("open as the caller");
    f.frame().unwrap();
    let green = select(&mut f, "Green");
    let (fx, fy) = field(&f);
    to(&mut f, fx, fy);
    press(&mut f);
    to(&mut f, fx - 2, fy);
    release(&mut f);
    assert_eq!(newest(&f), ("layer.set_opacity".to_owned(), "gate-actor".to_owned(), "gate-session".to_owned()));
    assert_ne!(Caller::asset_local().actor.account_id(), "gate-actor");
    f.invoke(&format!("{}.visibility", row_id(&green))).unwrap();
    assert_eq!(newest(&f), ("layer.panel_set".to_owned(), "gate-actor".to_owned(), "gate-session".to_owned()));
}

/// The frames a host run sent.
#[derive(Default)]
struct Frames(Vec<DeliveryClass>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(class);
        Ok(())
    }
}

/// The host clock reaches GuiFrame::advance in a window run (candidate 2026-11-13T09:27:50Z; lead ruling 3's proof): a
/// quiet window run whose script ends in five wheel notches over the field commits exactly once, which only the
/// window's clock can do (the run does not end while the gesture timer is pending), and the revision moves by one.
/// Dwell in the Operator's window is UNVERIFIED by run (STU-SHL-235 rule 5: no tip in an agent run).
#[test]
fn a_quiet_window_run_feeds_the_clock_so_the_wheel_idle_commits_once() {
    let s = Scratch::new("gate-window");
    let doc = fixture(&s.0);
    let (w, h) = (1280, 720);
    let mut probe = GuiFrame::open(&doc, w, h, Some(1000)).unwrap();
    probe.frame().unwrap();
    let green = layer(&probe, "Green");
    probe.invoke(&row_id(&green)).unwrap();
    probe.frame().unwrap();
    let (fx, fy) = field(&probe);
    let rev = probe.revision();
    let mut steps = vec![Step::Invoke(row_id(&green)), Step::Event(HostEvent::PointerMoved { x: fx, y: fy })];
    steps.extend((0..5).map(|_| Step::Event(HostEvent::Wheel { x: 0, y: -1000, pixels: false })));
    let (cancel, mut frames) = (CancellationToken::default(), Frames::default());
    let caller = Caller::asset_local();
    let mut host = HostRun::new(caller.actor, caller.resource, 1, &cancel, &mut frames).expect("host run");
    let win = window::run(
        &WindowArgs {
            width: w,
            height: h,
            scene: Scene::Gui(doc.clone()),
            quiet: true,
            frames: 3,
            exercise: false,
            capture: None,
            receipt: None,
            allow_overlays: false,
            deadline: Duration::from_secs(120),
            gui: GuiArgs { steps, scale_milli: Some(1000), ..GuiArgs::default() },
        },
        &mut host,
    )
    .expect("quiet window");
    drop(host);
    assert_eq!(frames.0, [DeliveryClass::Terminal]);
    assert_eq!(win.focus.focus_changes, 0, "{:?}", win.focus);
    let g = win.receipt.get("gui").expect("gui member").render();
    assert!(g.contains(r#""commits":1"#), "{g}");
    assert!(g.contains(&format!(r#""revision":{}"#, rev + 1)), "{g}");
}
