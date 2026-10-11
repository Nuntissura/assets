//! Capability registry: the ordered set of control descriptors and section descriptors plus the
//! consumers that read each control's value, and the dead-control audit that joins them.
//!
//! Sections are declared groups (STU-SHL-165, STU-IO-109): first-class nodes with a parent chain,
//! a collapsed-by-default flag and manual text. A control names its place by `group_path` (root
//! first), which must be a declared chain. Registration order is display order.

use crate::descriptor::{ControlSpec, valid_id};
use crate::value::ControlError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A collapsible group of controls (and child sections).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSpec {
    pub id: String,
    pub label: String,
    /// UserManual text. Required.
    pub help: String,
    /// Declared parent section (registered earlier); `None` = panel root.
    #[serde(default)]
    pub parent: Option<String>,
    /// The disclosure starts closed (STU-SHL-165 item 2).
    #[serde(default)]
    pub collapsed_by_default: bool,
}

impl SectionSpec {
    pub fn new(id: &str, label: &str, help: &str) -> Self {
        Self { id: id.to_string(), label: label.to_string(), help: help.to_string(), parent: None, collapsed_by_default: false }
    }

    pub fn within(mut self, parent: &str) -> Self {
        self.parent = Some(parent.to_string());
        self
    }

    pub fn collapsed(mut self) -> Self {
        self.collapsed_by_default = true;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    specs: Vec<ControlSpec>,
    sections: Vec<SectionSpec>,
    consumers: BTreeMap<String, BTreeSet<String>>,
}

/// Result of [`Registry::audit`]. A control with no registered consumer is a dead control: the
/// operator can edit it but nothing reads it (Craft shipped 55 of 141 such settings). A section
/// with no control anywhere in its subtree is an empty section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    pub total: usize,
    pub consumed: usize,
    /// Control ids with zero registered consumers, in registration order.
    pub orphans: Vec<String>,
    /// Section ids whose subtree holds no control, in registration order.
    pub empty_sections: Vec<String>,
}

impl AuditReport {
    pub fn is_clean(&self) -> bool {
        self.orphans.is_empty() && self.empty_sections.is_empty()
    }
}

impl fmt::Display for AuditReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{} controls consumed; orphans: [{}]; empty sections: [{}]",
            self.consumed,
            self.total,
            self.orphans.join(", "),
            self.empty_sections.join(", ")
        )
    }
}

fn invalid(id: &str, reason: impl Into<String>) -> ControlError {
    ControlError::InvalidDescriptor { control_id: id.to_string(), reason: reason.into() }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn id_taken(&self, id: &str) -> bool {
        self.specs.iter().any(|s| s.id == id) || self.sections.iter().any(|s| s.id == id)
    }

    /// Registers a section; ids are unique across controls AND sections (they share the manual
    /// and agent address space). The parent must already be registered.
    pub fn register_section(&mut self, section: SectionSpec) -> Result<(), ControlError> {
        let id = section.id.as_str();
        if !valid_id(id) {
            return Err(invalid(id, "section id must be 1-96 chars of [a-z0-9._-] starting with a letter"));
        }
        if section.label.trim().is_empty() || section.label.chars().count() > 128 || section.label.chars().any(char::is_control) {
            return Err(invalid(id, "section label must be 1-128 printable chars"));
        }
        if section.help.trim().is_empty() || section.help.chars().count() > 1024 {
            return Err(invalid(id, "section help (UserManual text) must be 1-1024 chars"));
        }
        if self.id_taken(id) {
            return Err(ControlError::DuplicateControl { control_id: section.id });
        }
        if let Some(p) = &section.parent
            && self.section(p).is_none()
        {
            return Err(invalid(id, format!("parent section `{p}` is not registered")));
        }
        self.sections.push(section);
        Ok(())
    }

    /// Registers a descriptor after full descriptor validation; ids are unique and its
    /// `group_path` must be a declared parent chain.
    pub fn register(&mut self, spec: ControlSpec) -> Result<(), ControlError> {
        spec.validate()?;
        if self.id_taken(&spec.id) {
            return Err(ControlError::DuplicateControl { control_id: spec.id });
        }
        let mut parent: Option<&str> = None;
        for g in &spec.group_path {
            match self.section(g) {
                Some(s) if s.parent.as_deref() == parent => parent = Some(g.as_str()),
                Some(_) => return Err(invalid(&spec.id, format!("group_path is not a parent chain at `{g}`"))),
                None => return Err(invalid(&spec.id, format!("group_path section `{g}` is not registered"))),
            }
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
            return Err(invalid(control_id, "consumer name must be 1-128 printable chars"));
        }
        self.consumers.entry(control_id.to_string()).or_default().insert(consumer.to_string());
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&ControlSpec> {
        self.specs.iter().find(|s| s.id == id)
    }

    pub fn section(&self, id: &str) -> Option<&SectionSpec> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// Descriptors in registration order.
    pub fn specs(&self) -> &[ControlSpec] {
        &self.specs
    }

    /// Sections in registration order (parents always precede children).
    pub fn sections(&self) -> &[SectionSpec] {
        &self.sections
    }

    /// Root-first chain of section ids ending at `id`.
    pub fn section_path(&self, id: &str) -> Option<Vec<String>> {
        let mut chain = vec![self.section(id)?.id.clone()];
        while let Some(p) = self.section(chain.last()?)?.parent.clone() {
            chain.push(p);
            if chain.len() > 64 {
                return None;
            }
        }
        chain.reverse();
        Some(chain)
    }

    /// Child sections of `parent` (`None` = root), in registration order.
    pub fn children_of(&self, parent: Option<&str>) -> Vec<&SectionSpec> {
        self.sections.iter().filter(|s| s.parent.as_deref() == parent).collect()
    }

    /// Controls placed directly in `section` (`None` = panel root), in registration order.
    pub fn controls_in(&self, section: Option<&str>) -> Vec<&ControlSpec> {
        self.specs.iter().filter(|c| c.group_path.last().map(String::as_str) == section).collect()
    }

    /// Consumers of `id`, sorted.
    pub fn consumers_of(&self, id: &str) -> Vec<&str> {
        self.consumers.get(id).map(|s| s.iter().map(String::as_str).collect()).unwrap_or_default()
    }

    /// Dead-control audit: every descriptor must have at least one registered consumer, and every
    /// section must contain at least one control somewhere in its subtree.
    pub fn audit(&self) -> AuditReport {
        let orphans: Vec<String> =
            self.specs.iter().filter(|s| self.consumers_of(&s.id).is_empty()).map(|s| s.id.clone()).collect();
        let empty_sections = self
            .sections
            .iter()
            .filter(|sec| !self.specs.iter().any(|c| c.group_path.contains(&sec.id)))
            .map(|sec| sec.id.clone())
            .collect();
        AuditReport { total: self.specs.len(), consumed: self.specs.len() - orphans.len(), orphans, empty_sections }
    }
}
