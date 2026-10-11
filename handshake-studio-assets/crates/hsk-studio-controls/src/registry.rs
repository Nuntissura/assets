//! Capability registry: the ordered set of control descriptors plus the consumers that read each
//! control's value, and the dead-control audit that joins them.

use crate::descriptor::ControlSpec;
use crate::value::ControlError;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Debug, Default)]
pub struct Registry {
    specs: Vec<ControlSpec>,
    consumers: BTreeMap<String, BTreeSet<String>>,
}

/// Result of [`Registry::audit`]. A control with no registered consumer is a dead control: the
/// operator can edit it but nothing reads it (Craft shipped 55 of 141 such settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    pub total: usize,
    pub consumed: usize,
    /// Control ids with zero registered consumers, in registration order.
    pub orphans: Vec<String>,
}

impl AuditReport {
    pub fn is_clean(&self) -> bool {
        self.orphans.is_empty()
    }
}

impl fmt::Display for AuditReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{} controls consumed; orphans: [{}]", self.consumed, self.total, self.orphans.join(", "))
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a descriptor after full descriptor validation; ids are unique.
    pub fn register(&mut self, spec: ControlSpec) -> Result<(), ControlError> {
        spec.validate()?;
        if self.get(&spec.id).is_some() {
            return Err(ControlError::DuplicateControl { control_id: spec.id });
        }
        self.specs.push(spec);
        Ok(())
    }

    /// Declares that `consumer` (a stable name for the code that reads the control's value, e.g.
    /// `render.layer-opacity`) reads `control_id`. The control must already be registered.
    pub fn register_consumer(&mut self, control_id: &str, consumer: &str) -> Result<(), ControlError> {
        if self.get(control_id).is_none() {
            return Err(ControlError::UnknownControl { control_id: control_id.to_string() });
        }
        if consumer.trim().is_empty() || consumer.chars().any(char::is_control) || consumer.len() > 128 {
            return Err(ControlError::InvalidDescriptor {
                control_id: control_id.to_string(),
                reason: "consumer name must be 1-128 printable chars".to_string(),
            });
        }
        self.consumers.entry(control_id.to_string()).or_default().insert(consumer.to_string());
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ControlSpec> {
        self.specs.iter().find(|s| s.id == id)
    }

    /// Descriptors in registration order.
    pub fn specs(&self) -> &[ControlSpec] {
        &self.specs
    }

    /// Consumers of `id`, sorted.
    pub fn consumers_of(&self, id: &str) -> Vec<&str> {
        self.consumers.get(id).map(|s| s.iter().map(String::as_str).collect()).unwrap_or_default()
    }

    /// Dead-control audit: every descriptor must have at least one registered consumer.
    pub fn audit(&self) -> AuditReport {
        let orphans: Vec<String> =
            self.specs.iter().filter(|s| self.consumers_of(&s.id).is_empty()).map(|s| s.id.clone()).collect();
        AuditReport { total: self.specs.len(), consumed: self.specs.len() - orphans.len(), orphans }
    }
}
