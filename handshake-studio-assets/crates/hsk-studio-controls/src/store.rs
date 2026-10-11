//! Typed value store bound to a registry, with validation on every write and undo-friendly
//! change events. The store holds control-local values; document commit stays with the host.

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

/// One accepted change. Carries old and new so a host can build an undo step without asking the
/// store again; `seq` is the store revision after the change.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeEvent {
    pub seq: u64,
    pub control_id: String,
    pub old: Value,
    pub new: Value,
    pub source: ChangeSource,
    pub actor: Option<ActorContext>,
}

/// A user/model proposal produced by a control (renderer or agent route) before the store
/// validates and applies it.
#[derive(Clone, Debug, PartialEq)]
pub struct Interaction {
    pub control_id: String,
    pub source: ChangeSource,
    pub outcome: Result<Value, ControlError>,
}

#[derive(Clone, Debug, Default)]
pub struct ValueStore {
    values: BTreeMap<String, Value>,
    seq: u64,
}

impl ValueStore {
    /// Seeds every registered control with its default.
    pub fn new(registry: &Registry) -> Self {
        let values = registry.specs().iter().map(|s| (s.id.clone(), s.default.clone())).collect();
        Self { values, seq: 0 }
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.values.get(id)
    }

    /// Store revision: number of accepted changes so far.
    pub fn revision(&self) -> u64 {
        self.seq
    }

    /// Validates through the descriptor (the one legality owner) and applies. `Ok(None)` = the
    /// value was already current (no event, no revision bump).
    pub fn set(
        &mut self,
        registry: &Registry,
        id: &str,
        value: Value,
        source: ChangeSource,
        actor: Option<&ActorContext>,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        let spec = registry.get(id).ok_or_else(|| ControlError::UnknownControl { control_id: id.to_string() })?;
        let checked = spec.validate_value(&value)?;
        let old = self.values.get(id).cloned().unwrap_or_else(|| spec.default.clone());
        if old == checked {
            return Ok(None);
        }
        self.seq += 1;
        self.values.insert(id.to_string(), checked.clone());
        Ok(Some(ChangeEvent {
            seq: self.seq,
            control_id: id.to_string(),
            old,
            new: checked,
            source,
            actor: actor.cloned(),
        }))
    }

    /// Applies a control interaction; a rejected outcome is returned as the error, store untouched.
    pub fn apply(
        &mut self,
        registry: &Registry,
        interaction: &Interaction,
        actor: Option<&ActorContext>,
    ) -> Result<Option<ChangeEvent>, ControlError> {
        match &interaction.outcome {
            Ok(v) => self.set(registry, &interaction.control_id, v.clone(), interaction.source, actor),
            Err(e) => Err(e.clone()),
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

    /// Reverts an earlier event. Refused with `Stale` when the control no longer holds the value
    /// the event produced (something else changed it since).
    pub fn undo(
        &mut self,
        registry: &Registry,
        event: &ChangeEvent,
        actor: Option<&ActorContext>,
    ) -> Result<ChangeEvent, ControlError> {
        let current = self
            .values
            .get(&event.control_id)
            .cloned()
            .ok_or_else(|| ControlError::UnknownControl { control_id: event.control_id.clone() })?;
        if current != event.new {
            return Err(ControlError::Stale { control_id: event.control_id.clone(), expected: event.new.clone(), current });
        }
        self.set(registry, &event.control_id, event.old.clone(), ChangeSource::Undo, actor)?
            .ok_or_else(|| ControlError::Stale { control_id: event.control_id.clone(), expected: event.new.clone(), current })
    }
}
