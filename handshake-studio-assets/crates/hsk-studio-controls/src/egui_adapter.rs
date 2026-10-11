//! The one egui renderer: every widget is chosen and configured ONLY by a `ControlSpec`.
//! It never mutates values; it returns `Interaction`s (already routed through the descriptor's
//! legality checks) that the host applies to a `ValueStore`. Headless-safe: no window, no eframe.

use crate::descriptor::{ControlKind, ControlSpec, NumberSpec, NumberWidget, SoftState, StepModifier};
use crate::registry::Registry;
use crate::store::{ChangeEvent, ChangeSource, Interaction, ValueStore};
use crate::value::Value;
use egui::{CursorIcon, Id, Key, Sense, TextEdit, Ui};
use hsk_studio_accord::ActorContext;

/// Stable egui id source of a control (`control_id` is the only input).
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

fn set_error(ui: &Ui, spec: &ControlSpec, outcome: &Result<Value, crate::value::ControlError>) {
    let id = err_id(&spec.id);
    ui.data_mut(|d| match outcome {
        Ok(_) => d.remove::<String>(id),
        Err(e) => d.insert_temp(id, e.to_string()),
    });
}

fn interaction(ui: &Ui, spec: &ControlSpec, source: ChangeSource, outcome: Result<Value, crate::value::ControlError>) -> Interaction {
    set_error(ui, spec, &outcome);
    Interaction { control_id: spec.id.clone(), source, outcome }
}

enum FieldEvent {
    None,
    Commit(String),
    Step(i8, StepModifier),
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
                // Modifier defaults (STU-FX-149) are UNVERIFIED here: shift = coarse, alt = fine, applied once.
                let modifier = if mods.shift {
                    StepModifier::Coarse
                } else if mods.alt {
                    StepModifier::Fine
                } else {
                    StepModifier::None
                };
                ui.data_mut(|d| d.remove::<String>(bid));
                event = FieldEvent::Step(if up { 1 } else { -1 }, modifier);
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

fn number(ui: &mut Ui, spec: &ControlSpec, n: &NumberSpec, v: f64, current: &Value) -> Option<Interaction> {
    let mut out = None;
    // The label is the scrub handle (drag to change); pointer scrubbing clamps hard bounds only.
    let label = ui
        .add(egui::Label::new(spec.label.as_str()).sense(Sense::drag()))
        .on_hover_cursor(CursorIcon::ResizeHorizontal)
        .on_hover_text(spec.help.as_str());
    if label.dragged() {
        let dx = f64::from(label.drag_delta().x);
        if dx != 0.0 {
            let mods = ui.input(|i| i.modifiers);
            let per_px = if mods.shift {
                n.coarse_step()
            } else if mods.alt {
                n.fine_step()
            } else {
                n.step()
            };
            out = Some(interaction(ui, spec, ChangeSource::Pointer, spec.pointer_value(v + dx * per_px)));
        }
    }
    if n.widget == NumberWidget::Slider
        && let Some((lo, hi)) = n.track()
    {
        let mut sv = v;
        let r = ui.add(egui::Slider::new(&mut sv, lo..=hi).show_value(false).clamping(egui::SliderClamping::Never));
        if r.changed() {
            out = Some(interaction(ui, spec, ChangeSource::Pointer, spec.pointer_value(sv)));
        }
    }
    match text_field(ui, spec, &spec.format_value(current), false, true) {
        FieldEvent::Commit(text) => out = Some(interaction(ui, spec, ChangeSource::Typed, spec.parse_text(&text))),
        FieldEvent::Step(dir, modifier) => {
            out = Some(interaction(ui, spec, ChangeSource::Keyboard, spec.step_value(current, dir, modifier)));
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
            (ControlKind::Number(n), Value::Number(v)) => number(ui, spec, n, *v, current),
            (ControlKind::Toggle, Value::Bool(b)) => {
                let mut checked = *b;
                let r = ui.checkbox(&mut checked, spec.label.as_str()).on_hover_text(spec.help.as_str());
                r.changed().then(|| interaction(ui, spec, ChangeSource::Pointer, spec.validate_value(&Value::Bool(checked))))
            }
            (ControlKind::Choice { options }, Value::Choice(selected)) => {
                let mut out = None;
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                egui::ComboBox::from_id_salt(widget_id(&spec.id).with("combo")).selected_text(spec.format_value(current)).show_ui(ui, |ui| {
                    for o in options {
                        if ui.selectable_label(&o.id == selected, o.label.as_str()).clicked() && &o.id != selected {
                            out = Some(interaction(ui, spec, ChangeSource::Pointer, spec.validate_value(&Value::Choice(o.id.clone()))));
                        }
                    }
                });
                out
            }
            (ControlKind::Color, Value::Color(rgba)) => {
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                let mut c = *rgba;
                let r = ui.color_edit_button_srgba_unmultiplied(&mut c);
                r.changed().then(|| interaction(ui, spec, ChangeSource::Pointer, spec.validate_value(&Value::Color(c))))
            }
            (ControlKind::Text { multiline, .. }, Value::Text(t)) => {
                ui.label(spec.label.as_str()).on_hover_text(spec.help.as_str());
                let out = match text_field(ui, spec, t, *multiline, false) {
                    FieldEvent::Commit(text) => Some(interaction(ui, spec, ChangeSource::Typed, spec.parse_text(&text))),
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

/// Renders every registered control in registration order; returns this frame's interactions.
pub fn show_panel(ui: &mut Ui, registry: &Registry, store: &ValueStore) -> Vec<Interaction> {
    let mut out = Vec::new();
    for spec in registry.specs() {
        if let Some(v) = store.get(&spec.id) {
            out.extend(show(ui, spec, v));
        }
    }
    out
}

/// Renders the panel and applies this frame's interactions to `store`; returns accepted changes.
/// Rejected interactions leave the store untouched (their error text stays visible next to the
/// control until the next accepted edit or Escape).
pub fn show_panel_and_apply(ui: &mut Ui, registry: &Registry, store: &mut ValueStore, actor: Option<&ActorContext>) -> Vec<ChangeEvent> {
    let interactions = show_panel(ui, registry, store);
    interactions.iter().filter_map(|i| store.apply(registry, i, actor).ok().flatten()).collect()
}
