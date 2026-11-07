//! The seed of [STU-SHL-240]'s `UiDescriptor` (RF-05, lane SMB-COMMAND-SURFACE-1): one record per
//! addressable element, keyed by an `author_id` of the [STU-MDL-100] grammar, carrying the `command_id`
//! it runs and the ids it was known by before (`aliases`, [STU-MDL-103] item 4). [`UiCatalog`] is the one
//! lookup a panel binds to: it refuses a malformed id, a duplicate id and an id that is also someone's
//! alias ([STU-SHL-243]).
//!
//! Seed subset of the record: no `shortcut_id`, `manual_anchor`, `provenance` beyond its extraction
//! method, `availability`, `a11y`, menu position or ParamSpec block yet; the menu keeps its own
//! availability evaluation until the descriptor set carries it.

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The closed `kind` vocabulary of [STU-SHL-240].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiKind {
    Tool,
    ToolGroup,
    Command,
    MenuItem,
    Panel,
    Control,
    Param,
    Preset,
    TaskScope,
    LayoutPreset,
}

impl UiKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::ToolGroup => "tool_group",
            Self::Command => "command",
            Self::MenuItem => "menu_item",
            Self::Panel => "panel",
            Self::Control => "control",
            Self::Param => "param",
            Self::Preset => "preset",
            Self::TaskScope => "task_scope",
            Self::LayoutPreset => "layout_preset",
        }
    }
}

/// One addressable element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiDescriptor {
    pub author_id: String,
    pub kind: UiKind,
    pub display_name: String,
    /// One sentence, or `None`: no text was recovered, reported and never faked.
    pub summary: Option<String>,
    /// The command the element runs; a panel calls this id, never a private method.
    pub command_id: Option<String>,
    pub domain: String,
    pub menu_only: bool,
    pub aliases: Vec<String>,
}

impl UiDescriptor {
    pub fn new(author_id: String, kind: UiKind, display_name: &str, domain: &str) -> Self {
        Self { author_id, kind, display_name: display_name.to_string(), summary: None, command_id: None, domain: domain.to_string(), menu_only: false, aliases: Vec::new() }
    }

    pub fn with_command(mut self, command_id: &str) -> Self {
        self.command_id = Some(command_id.to_string());
        self
    }

    pub fn with_alias(mut self, alias: &str) -> Self {
        self.aliases.push(alias.to_string());
        self
    }

    pub fn json(&self) -> Value {
        json!({
            "author_id": self.author_id, "stable_id": stable_id(&self.author_id), "kind": self.kind.token(), "display_name": self.display_name, "summary": self.summary,
            "command_id": self.command_id, "domain": self.domain, "menu_only": self.menu_only, "aliases": self.aliases, "extraction_method": "handshake_authored",
        })
    }
}

/// Why a descriptor was not admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiCatalogError {
    InvalidAuthorId(String),
    DuplicateAuthorId(String),
    /// One id, live or alias, already resolves to another element.
    AliasCollision { id: String, holder: String },
}

impl UiCatalogError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidAuthorId(_) => "ui_author_id_invalid",
            Self::DuplicateAuthorId(_) => "ui_author_id_duplicate",
            Self::AliasCollision { .. } => "ui_alias_collision",
        }
    }
}

/// `snake_case` to the kebab-case of the grammar, dots kept.
pub fn kebab(id: &str) -> String {
    id.replace('_', "-")
}

/// `hs-` plus the id with dots replaced by hyphens ([STU-MDL-103] item 2).
pub fn stable_id(author_id: &str) -> String {
    format!("hs-{}", author_id.replace('.', "-"))
}

/// `studio.<surface>.<group>.<element>[.<sub>][#<instance>]`, charset `[a-z0-9-.]`. A flat surface such
/// as `studio.tab.layers` has its two parts, as the clause's own examples do.
pub fn valid_author_id(id: &str) -> bool {
    let body = id.split_once('#').map_or(id, |(body, instance)| if !instance.is_empty() && instance.bytes().all(|b| b.is_ascii_digit()) { body } else { "" });
    let parts: Vec<&str> = body.split('.').collect();
    parts.len() >= 3 && parts[0] == "studio" && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}

/// The descriptor set, in insertion order, looked up by `author_id`, alias or `command_id`.
#[derive(Clone, Debug, Default)]
pub struct UiCatalog {
    rows: Vec<UiDescriptor>,
    /// `author_id` or alias -> row.
    index: BTreeMap<String, usize>,
    /// `command_id` -> its first row.
    by_command: BTreeMap<String, usize>,
}

impl UiCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits one descriptor, whole or not at all.
    pub fn insert(&mut self, descriptor: UiDescriptor) -> Result<(), UiCatalogError> {
        if !valid_author_id(&descriptor.author_id) {
            return Err(UiCatalogError::InvalidAuthorId(descriptor.author_id));
        }
        if self.index.contains_key(&descriptor.author_id) {
            return Err(UiCatalogError::DuplicateAuthorId(descriptor.author_id));
        }
        for (position, alias) in descriptor.aliases.iter().enumerate() {
            if let Some(&row) = self.index.get(alias) {
                return Err(UiCatalogError::AliasCollision { id: alias.clone(), holder: self.rows[row].author_id.clone() });
            }
            if *alias == descriptor.author_id || descriptor.aliases[..position].contains(alias) {
                return Err(UiCatalogError::AliasCollision { id: alias.clone(), holder: descriptor.author_id.clone() });
            }
        }
        let row = self.rows.len();
        self.index.insert(descriptor.author_id.clone(), row);
        for alias in &descriptor.aliases {
            self.index.insert(alias.clone(), row);
        }
        if let Some(command) = &descriptor.command_id {
            self.by_command.entry(command.clone()).or_insert(row);
        }
        self.rows.push(descriptor);
        Ok(())
    }

    /// The element an id names: its `author_id`, a former id, or the `command_id` it runs.
    pub fn resolve(&self, id: &str) -> Option<&UiDescriptor> {
        let row = self.index.get(id).or_else(|| self.by_command.get(id))?;
        self.rows.get(*row)
    }

    pub fn rows(&self) -> &[UiDescriptor] {
        &self.rows
    }

    pub fn json(&self) -> Value {
        json!({"count": self.rows.len(), "descriptors": self.rows.iter().map(UiDescriptor::json).collect::<Vec<_>>()})
    }
}
