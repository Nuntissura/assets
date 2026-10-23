//! Typed value store bound to a registry, with validation on every write and undo-friendly
//! change events. The store holds control-local values; document commit stays with the host.
//!
//! Gestures (STU-SHL-194): a drag/scrub opens ONE gesture scope on its control. Live updates are
//! visible immediately but are `EventPhase::Live` (not undo steps); release produces ONE
//! `Committed` event spanning press value -> final value (the single undo step); Escape/abort
//! restores the press value and emits an `Aborted` event that is excluded from undo.

use crate::registry::Registry;
use crate::value::{ControlError, Value};
use hsk_studio_accord::ActorContext;
use std::collections::BTreeMap;

/// Which route produced a change (all routes share one validation owner).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSource {
    Pointer,
    Typed,
    Keyboard,
    Model,
    Reset,
    Undo,
}

/// Where an event sits in a gesture. Only `Discrete` and `Committed` events are undo steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventPhase {
    Discrete,
    Live(u64),
    Committed(u64),
    Aborted(u64),
}

/// One accepted change. Carries old and new so a host can build an undo step without asking the
/// store again; `seq` is the store revision after the change (a `Committed` event does not change
/// the value again, so it carries the revision of the last live update).
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeEvent {
    pub seq: u64,
    pub control_id: String,
    pub old: Value,
    pub new: Value,
    pub source: ChangeSource,
    pub actor: Option<ActorContext>,
    pub phase: EventPhase,
}

impl ChangeEvent {
    /// True for events a host should push on its undo stack (one per discrete edit, one per
    /// completed gesture).
    pub fn is_undo_step(&self) -> bool {
        matches!(self.phase, EventPhase::Discrete | EventPhase::Committed(_))
    }
}

/// Gesture marker carried by an [`Interaction`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GesturePhase {
    /// A drag/scrub sample (the first sample opens the gesture).
    Update,
    /// Release / focus loss: finalize the gesture once. A refused `End` (error outcome or illegal
    /// final value) leaves the store untouched, the open gesture included: the value stays at the
    /// last live sample, `set` stays refused, and the caller sends a legal `End` or an `Abort`
    /// (proof `tests/step_effect.rs` `no_change_is_reported_by_route_and_a_refused_end_keeps_the_gesture`).
    End,
    /// Escape / cancel: restore the press value.
    Abort,
}

/// A user/model proposal produced by a control (renderer or agent route) before the store
/// validates and applies it.
#[derive(Clone, Debug, PartialEq)]
pub struct Interaction {
    pub control_id: String,
    pub source: ChangeSource,
    pub outcome: Result<Value, ControlError>,
    pub gesture: Option<GesturePhase>,
}

impl Interaction {
    pub fn discrete(control_id: &str, source: ChangeSource, outcome: Result<Value, ControlError>) -> Self {
        Self { control_id: control_id.to_string(), source, outcome, gesture: None }
    }

    pub fn gesture(control_id: &str, source: ChangeSource, phase: GesturePhase, outcome: Result<Value, ControlError>) -> Self {
        Self { control_id: control_id.to_string(), source, outcome, gesture: Some(phase) }
    }
}

#[derive(Clone, Debug)]
struct ActiveGesture {
    id: u64,
    pre: Value,
}

#[derive(Clone, Debug, Default)]
pub struct ValueStore {
    values: BTreeMap<String, Value>,
    seq: u64,
    active: BTreeMap<String, ActiveGesture>,
    next_gesture: u64,
}

fn gesture_err(id: &str, reason: &'static str) -> ControlError {
    ControlError::Gesture { control_id: id.to_string(), reason }
}

impl ValueStore {
    /// Seeds every registered control with its default.
    pub fn new(registry: &Registry) -> Self {
        let values = registry.specs().iter().map(|s| (s.id.clone(), s.default.clone())).collect();
        Self { values, seq: 0, active: BTreeMap::new(), next_gesture: 1 }
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.values.get(id)
    }

    /// Store revision: number of accepted value changes so far (live updates included).
    pub fn revision(&self) -> u64 {
        self.seq
    }

    /// True while a drag/scrub gesture is open on `id`.
    pub fn gesture_active(&self, id: &str) -> bool {
        self.active.contains_key(id)
    }

    fn current_or_default(&self, registry: &Registry, id: &str) -> Result<Value, ControlError> {
        let spec = registry.get(id).ok_or_else(|| ControlError::UnknownControl { control_id: id.to_string() })?;
        Ok(self.values.get(id).cloned().unwrap_or_else(|| spec.default.clone()))
    }

    /// Writes an already validated value; `Ok(None)` when it equals the current value.
    fn write(
        &mut self,
        registry: &Registry,
        id: &str,
        new: Value,
        source: ChangeSource,
        actor: Option<&ActorContext>,
        phase: EventPhase,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        let old = self.current_or_default(registry, id)?;
        if old == new {
            return Ok(None);
        }
        self.seq += 1;
        self.values.insert(id.to_string(), new.clone());
        Ok(Some(ChangeEvent { seq: self.seq, control_id: id.to_string(), old, new, source, actor: actor.cloned(), phase }))
    }

    fn validated(registry: &Registry, id: &str, value: &Value) -> Result<Value, ControlError> {
        registry
            .get(id)
            .ok_or_else(|| ControlError::UnknownControl { control_id: id.to_string() })?
            .validate_value(value)
    }

    /// Validates through the descriptor (the one legality owner) and applies. `Ok(None)` = the
    /// value was already current (no event, no revision bump). Refused while a gesture is open
    /// on the control.
    pub fn set(
        &mut self,
        registry: &Registry,
        id: &str,
        value: Value,
        source: ChangeSource,
        actor: Option<&ActorContext>,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        let checked = Self::validated(registry, id, &value)?;
        if self.active.contains_key(id) {
            return Err(gesture_err(id, "a gesture is in progress on this control"));
        }
        self.write(registry, id, checked, source, actor, EventPhase::Discrete)
    }

    /// Applies a control interaction; a rejected outcome is returned as the error, store
    /// untouched (for a refused `End` the gesture stays open, see [`GesturePhase::End`]). Gesture
    /// interactions follow the open/update/commit-or-abort protocol above.
    pub fn apply(
        &mut self,
        registry: &Registry,
        interaction: &Interaction,
        actor: Option<&ActorContext>,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        let id = interaction.control_id.as_str();
        let Some(phase) = interaction.gesture else {
            return match &interaction.outcome {
                Ok(v) => self.set(registry, id, v.clone(), interaction.source, actor),
                Err(e) => Err(e.clone()),
            };
        };
        match phase {
            GesturePhase::Update => {
                let checked = Self::validated(registry, id, interaction.outcome.as_ref().map_err(Clone::clone)?)?;
                let gid = match self.active.get(id) {
                    Some(a) => a.id,
                    None => {
                        let pre = self.current_or_default(registry, id)?;
                        let gid = self.next_gesture;
                        self.next_gesture += 1;
                        self.active.insert(id.to_string(), ActiveGesture { id: gid, pre });
                        gid
                    }
                };
                self.write(registry, id, checked, interaction.source, actor, EventPhase::Live(gid))
            }
            GesturePhase::End => {
                let Some(active) = self.active.get(id).cloned() else { return Ok(None) };
                let last = Self::validated(registry, id, interaction.outcome.as_ref().map_err(Clone::clone)?)?;
                self.write(registry, id, last, interaction.source, actor, EventPhase::Live(active.id))?;
                self.active.remove(id);
                let current = self.current_or_default(registry, id)?;
                if current == active.pre {
                    return Ok(None);
                }
                Ok(Some(ChangeEvent {
                    seq: self.seq,
                    control_id: id.to_string(),
                    old: active.pre,
                    new: current,
                    source: interaction.source,
                    actor: actor.cloned(),
                    phase: EventPhase::Committed(active.id),
                }))
            }
            GesturePhase::Abort => {
                let Some(active) = self.active.remove(id) else { return Ok(None) };
                self.write(registry, id, active.pre, interaction.source, actor, EventPhase::Aborted(active.id))
            }
        }
    }

    /// Restores the default value of one control.
    pub fn reset(
        &mut self,
        registry: &Registry,
        id: &str,
        actor: Option<&ActorContext>,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        let default = registry
            .get(id)
            .ok_or_else(|| ControlError::UnknownControl { control_id: id.to_string() })?
            .default
            .clone();
        self.set(registry, id, default, ChangeSource::Reset, actor)
    }

    /// Reverts an earlier undo step. Refused with `Stale` when the control no longer holds the
    /// value the event produced, and with `Gesture` for live/aborted events (not undo steps).
    pub fn undo(
        &mut self,
        registry: &Registry,
        event: &ChangeEvent,
        actor: Option<&ActorContext>,
    ) -> Result<ChangeEvent, ControlError> {
        if !event.is_undo_step() {
            return Err(gesture_err(&event.control_id, "event is not an undo step"));
        }
        let current = self.current_or_default(registry, &event.control_id)?;
        if current != event.new {
            return Err(ControlError::Stale { control_id: event.control_id.clone(), expected: event.new.clone(), current });
        }
        self.set(registry, &event.control_id, event.old.clone(), ChangeSource::Undo, actor)?
            .ok_or_else(|| ControlError::Stale { control_id: event.control_id.clone(), expected: event.new.clone(), current })
    }
}
