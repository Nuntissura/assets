//! The one egui renderer: every widget is chosen and configured ONLY by a `ControlSpec`.
//! It never mutates values; it returns `Interaction`s (already routed through the descriptor's
//! legality checks) that the host applies to a `ValueStore`. Headless-safe: no window, no eframe.
//!
//! Gesture mapping (STU-SHL-188..194, STU-FX-149): dragging a numeric control's label scrubs
//! (`dx - dy` pixels, frozen sensitivity, Shift = coarse x10, Ctrl/Command = fine /10, both x100,
//! Alt unused); press..release is ONE gesture (`Update`* then `End`), Escape during a drag aborts
//! it (`Abort`). Typed entry commits on Enter/blur, Escape discards; arrow keys step.

use crate::descriptor::{ControlKind, ControlSpec, GestureRoles, NumberWidget, ScrubGesture, SoftState};
use crate::registry::Registry;
use crate::store::{ChangeEvent, ChangeSource, GesturePhase, Interaction, ValueStore};
use crate::value::{ControlError, Value};
use egui::{CursorIcon, Id, Key, Rect, Sense, TextEdit, Ui};
use hsk_studio_accord::ActorContext;

/// Stable egui id source of a control or section (the descriptor id is the only input).
pub fn widget_id(control_id: &str) -> Id {
    Id::new(("hsk-studio-control", control_id))
}

/// Id of the control's text field (number/text controls); tests and focus requests use this.
pub fn edit_id(control_id: &str) -> Id {
    widget_id(control_id).with("edit")
}

fn buf_id(control_id: &str) -> Id {
    widget_id(control_id).with("buf")
}

fn err_id(control_id: &str) -> Id {
    widget_id(control_id).with("err")
}

/// Default physical mapping of the gesture roles (STU-SHL-190): Shift = coarse; `command`
/// (Ctrl on Windows/Linux, Command on macOS) = fine. Alt has no numeric role.
pub fn roles_from(mods: egui::Modifiers) -> GestureRoles {
    GestureRoles { coarse: mods.shift, fine: mods.command }
}

fn set_error(ui: &Ui, spec: &ControlSpec, outcome: &Result<Value, ControlError>) {
    let id = err_id(&spec.id);
    ui.data_mut(|d| match outcome {
        Ok(_) => d.remove::<String>(id),
        Err(e) => d.insert_temp(id, e.to_string()),
    });
}

fn interaction(
    ui: &Ui,
    spec: &ControlSpec,
    source: ChangeSource,
    gesture: Option<GesturePhase>,
    outcome: Result<Value, ControlError>,
) -> Interaction {
    set_error(ui, spec, &outcome);
    Interaction { control_id: spec.id.clone(), source, outcome, gesture }
}

enum FieldEvent {
    None,
    Commit(String),
    Step(i8, GestureRoles),
}

/// Text field with commit-on-Enter/blur, Escape discard, and (for numbers) arrow-key steps.
fn text_field(ui: &mut Ui, spec: &ControlSpec, formatted: &str, multiline: bool, steppable: bool) -> FieldEvent {
    let eid = edit_id(&spec.id);
    let bid = buf_id(&spec.id);
    let focused = ui.ctx().memory(|m| m.has_focus(eid));
    let mut buf = if focused {
        ui.data(|d| d.get_temp::<String>(bid)).unwrap_or_else(|| formatted.to_string())
    } else {
        formatted.to_string()
    };
    let edit = if multiline { TextEdit::multiline(&mut buf).desired_width(240.0) } else { TextEdit::singleline(&mut buf).desired_width(90.0) };
    let resp = ui.add(edit.id(eid));
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let mut event = FieldEvent::None;
    if escape && (resp.has_focus() || resp.lost_focus()) {
        ui.data_mut(|d| {
            d.remove::<String>(bid);
            d.remove::<String>(err_id(&spec.id));
        });
        resp.surrender_focus();
    } else if resp.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(bid));
        if buf != formatted {
            event = FieldEvent::Commit(buf);
        }
    } else if resp.has_focus() {
        ui.data_mut(|d| d.insert_temp(bid, buf));
        if steppable {
            let (up, down, mods) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.modifiers));
            if up != down {
                ui.data_mut(|d| d.remove::<String>(bid));
                event = FieldEvent::Step(if up { 1 } else { -1 }, roles_from(mods));
            }
        }
    }
    event
}

fn show_error(ui: &mut Ui, spec: &ControlSpec) {
    if let Some(msg) = ui.data(|d| d.get_temp::<String>(err_id(&spec.id))) {
        let color = ui.visuals().error_fg_color;
        ui.colored_label(color, msg);
    }
}

/// Slider gesture bookkeeping: 0 = idle, 1 = open, 2 = aborted until release.
///
/// Arrow keys on a focused slider are a discrete move and go through the descriptor's one step
/// owner (`ControlSpec::step_value`). egui's own slider arrows are ignored: they move one track
/// point per press, stop silently at the track ends and, with the value beyond the track (out of
/// soft range), jump back to the track end against the pressed direction; routed through the
/// clamping pointer route they also turned a step at a hard bound into an unchanged success
/// (R5 audit; proof `tests/step_effect.rs` `slider_arrow_keys_use_the_descriptor_step_at_every_position`).
fn slider(ui: &mut Ui, spec: &ControlSpec, current: &Value, lo: f64, hi: f64, integer: bool) -> Option<Interaction> {
    let sid = widget_id(&spec.id).with("slider-state");
    let state = ui.data(|d| d.get_temp::<u8>(sid)).unwrap_or(0);
    let mut sv = spec.numeric_value(current).unwrap_or(lo);
    let mut widget = egui::Slider::new(&mut sv, lo..=hi).show_value(false).clamping(egui::SliderClamping::Never);
    if integer {
        widget = widget.step_by(1.0);
    }
    let r = ui.add(widget);
    let down = r.is_pointer_button_down_on();
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let (mut next, mut out) = (state, None);
    if state == 2 {
        if !down {
            next = 0;
        }
    } else if state == 1 && escape {
        next = 2;
        out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::Abort), Ok(current.clone())));
    } else {
        let (right, left, mods) = if r.has_focus() && !down {
            ui.input(|i| (i.key_pressed(Key::ArrowRight), i.key_pressed(Key::ArrowLeft), i.modifiers))
        } else {
            (false, false, egui::Modifiers::NONE)
        };
        if right != left {
            let step = spec.step_value(current, if right { 1 } else { -1 }, roles_from(mods));
            out = Some(interaction(ui, spec, ChangeSource::Keyboard, None, step));
        } else if r.changed() {
            if down {
                next = 1;
                out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::Update), spec.pointer_value(sv)));
            } else {
                // Press and release in one frame (a click on the track): one discrete pointer target.
                out = Some(interaction(ui, spec, ChangeSource::Pointer, None, spec.pointer_value(sv)));
            }
        }
        if state == 1 && !down {
            next = 0;
            out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::End), Ok(current.clone())));
        }
    }
    if next != state {
        ui.data_mut(|d| d.insert_temp(sid, next));
    }
    out
}

fn numeric(ui: &mut Ui, spec: &ControlSpec, current: &Value) -> Option<Interaction> {
    let mut out = None;
    let gid = widget_id(&spec.id).with("scrub");
    // The label is the scrub handle: press-drag changes the value, one gesture per drag.
    let label = ui
        .add(egui::Label::new(spec.label.as_str()).sense(Sense::drag()))
        .on_hover_cursor(CursorIcon::ResizeHorizontal)
        .on_hover_text(spec.help.as_str());
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    if label.drag_started() {
        match ScrubGesture::begin(spec, current) {
            Ok(g) => ui.data_mut(|d| d.insert_temp(gid, g)),
            Err(e) => out = Some(interaction(ui, spec, ChangeSource::Pointer, None, Err(e))),
        }
    }
    // The gesture state exists exactly while the gesture is open, so Escape aborts it even if the
    // backend already ended the pointer drag.
    let open_gesture = ui.data(|d| d.get_temp::<ScrubGesture>(gid));
    if escape && open_gesture.is_some() {
        ui.data_mut(|d| d.remove::<ScrubGesture>(gid));
        out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::Abort), Ok(current.clone())));
    } else if label.dragged()
        && let Some(mut g) = open_gesture
    {
        let delta = label.drag_delta();
        let px = f64::from(delta.x - delta.y);
        if px != 0.0 {
            let outcome = g.drag(spec, px, roles_from(ui.input(|i| i.modifiers)));
            ui.data_mut(|d| d.insert_temp(gid, g));
            out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::Update), outcome));
        }
    }
    if label.drag_stopped() && ui.data(|d| d.get_temp::<ScrubGesture>(gid)).is_some() {
        ui.data_mut(|d| d.remove::<ScrubGesture>(gid));
        out = Some(interaction(ui, spec, ChangeSource::Pointer, Some(GesturePhase::End), Ok(current.clone())));
    }
    if spec.numeric_widget() == Some(NumberWidget::Slider)
        && let Some((lo, hi)) = spec.track()
        && let Some(i) = slider(ui, spec, current, lo, hi, matches!(spec.kind, ControlKind::Integer(_)))
    {
        out = Some(i);
    }
    match text_field(ui, spec, &spec.format_value(current), false, true) {
        FieldEvent::Commit(text) => out = Some(interaction(ui, spec, ChangeSource::Typed, None, spec.parse_text(&text))),
        FieldEvent::Step(dir, roles) => {
            out = Some(interaction(ui, spec, ChangeSource::Keyboard, None, spec.step_value(current, dir, roles)));
        }
        FieldEvent::None => {}
    }
    if let Some(u) = &spec.unit {
        ui.weak(u.as_str());
    }
    if spec.soft_state(current) == SoftState::OutOfSoft {
        ui.weak("(beyond soft range)");
    }
    show_error(ui, spec);
    out
}

/// Renders one control. Returns the interaction produced this frame, if any; the value shown is
/// always `current` (the store owns state), so rejected input never sticks.
pub fn show(ui: &mut Ui, spec: &ControlSpec, current: &Value) -> Option<Interaction> {
    ui.push_id(widget_id(&spec.id), |ui| {
        ui.horizontal(|ui| match (&spec.kind, current) {
            (ControlKind::Number(_), Value::Number(_)) | (ControlKind::Integer(_), Value::Int(_)) => numeric(ui, spec, current),
            (ControlKind::Toggle, Value::Bool(b)) => {
                let mut checked = *b;
                let r = ui.checkbox(&mut checked, spec.label.as_str()).on_hover_text(spec.help.as_str());
                r.changed().then(|| interaction(ui, spec, ChangeSource::Pointer, None, spec.validate_value(&Value::Bool(checked))))
            }
            (ControlKind::Choice { options }, Value::Choice(selected)) => {
                let mut out = None;
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                egui::ComboBox::from_id_salt(widget_id(&spec.id).with("combo")).selected_text(spec.format_value(current)).show_ui(ui, |ui| {
                    for o in options {
                        if ui.selectable_label(&o.id == selected, o.label.as_str()).clicked() && &o.id != selected {
                            out = Some(interaction(ui, spec, ChangeSource::Pointer, None, spec.validate_value(&Value::Choice(o.id.clone()))));
                        }
                    }
                });
                out
            }
            (ControlKind::Color, Value::Color(rgba)) => {
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                let mut c = *rgba;
                let r = ui.color_edit_button_srgba_unmultiplied(&mut c);
                r.changed().then(|| interaction(ui, spec, ChangeSource::Pointer, None, spec.validate_value(&Value::Color(c))))
            }
            (ControlKind::Text { multiline, .. }, Value::Text(t)) => {
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                let out = match text_field(ui, spec, t, *multiline, false) {
                    FieldEvent::Commit(text) => Some(interaction(ui, spec, ChangeSource::Typed, None, spec.parse_text(&text))),
                    _ => None,
                };
                show_error(ui, spec);
                out
            }
            _ => {
                let color = ui.visuals().error_fg_color;
                ui.colored_label(color, format!("control `{}`: stored value does not match its descriptor", spec.id));
                None
            }
        })
        .inner
    })
    .inner
}

/// Where one section header was drawn this frame (visual-debug / agent surface).
#[derive(Clone, Debug, PartialEq)]
pub struct SectionFrame {
    pub id: String,
    pub header: Rect,
    pub open: bool,
}

/// What a panel frame rendered and produced.
#[derive(Clone, Debug, Default)]
pub struct PanelFrame {
    pub interactions: Vec<Interaction>,
    /// Control ids drawn this frame, in order (controls inside collapsed sections are absent).
    pub rendered: Vec<String>,
    pub sections: Vec<SectionFrame>,
}

fn show_level(ui: &mut Ui, registry: &Registry, store: &ValueStore, section: Option<&str>, frame: &mut PanelFrame) {
    for spec in registry.controls_in(section) {
        if let Some(v) = store.get(&spec.id) {
            frame.rendered.push(spec.id.clone());
            frame.interactions.extend(show(ui, spec, v));
        }
    }
    for sec in registry.children_of(section) {
        let response = egui::CollapsingHeader::new(sec.label.as_str())
            .id_salt(widget_id(&sec.id))
            .default_open(!sec.collapsed_by_default)
            .show(ui, |ui| show_level(ui, registry, store, Some(&sec.id), frame));
        response.header_response.clone().on_hover_text(sec.help.as_str());
        frame.sections.push(SectionFrame { id: sec.id.clone(), header: response.header_response.rect, open: response.body_response.is_some() });
    }
}

/// Renders the panel tree from the registry: root controls, then root sections (collapsible,
/// `collapsed_by_default` honoured), each with its controls then child sections.
pub fn show_panel(ui: &mut Ui, registry: &Registry, store: &ValueStore) -> PanelFrame {
    let mut frame = PanelFrame::default();
    show_level(ui, registry, store, None, &mut frame);
    frame
}

/// Result of one panel frame applied to a store.
#[derive(Clone, Debug, Default)]
pub struct PanelOutcome {
    /// Accepted changes (live gesture samples, discrete edits, committed/aborted gestures).
    pub changes: Vec<ChangeEvent>,
    pub rendered: Vec<String>,
    pub sections: Vec<SectionFrame>,
    /// Interactions the store refused (their error text is also shown next to the control).
    pub rejected: Vec<ControlError>,
}

impl PanelOutcome {
    /// Events a host pushes on its undo stack: one per discrete edit and one per finished gesture.
    pub fn undo_steps(&self) -> impl Iterator<Item = &ChangeEvent> {
        self.changes.iter().filter(|e| e.is_undo_step())
    }
}

/// Renders the panel and applies this frame's interactions to `store`.
pub fn show_panel_and_apply(ui: &mut Ui, registry: &Registry, store: &mut ValueStore, actor: Option<&ActorContext>) -> PanelOutcome {
    let frame = show_panel(ui, registry, store);
    let mut out = PanelOutcome { rendered: frame.rendered, sections: frame.sections, ..PanelOutcome::default() };
    for i in &frame.interactions {
        match store.apply(registry, i, actor) {
            Ok(Some(ev)) => out.changes.push(ev),
            Ok(None) => {}
            Err(e) => out.rejected.push(e),
        }
    }
    out
}
