//! The window half of the [`UiCatalog`] seed: tools, panels and menu leaves, from their own enumerations.
//! The command half is `action::ui_seed` in harness-core. The old id of each element stays an alias.
//! A tool's family is not in `Tool` yet, so tools sit in one group until the tool catalogue lands.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::edit::Tool;
use super::super::shell::Panel;
use hsk_studio_controls::menu::MenuNodeKind;
use hsk_studio_controls::{UiCatalog, UiCatalogError, UiDescriptor, UiKind, kebab};
use hsk_studio_harness_core::action;
use serde_json::{Value, json};

use super::CommandRefusal;
use super::decode::Fields;

impl Viewer {
    /// The one descriptor catalog of this build: every command, tool, panel and menu leaf, with the id
    /// each was known by before. A descriptor set that breaks an id rule is a refusal, never a partial
    /// catalog.
    pub fn ui_catalog(&self) -> Result<UiCatalog, UiCatalogError> {
        let mut catalog = action::ui_seed()?;
        for tool in Tool::ALL {
            let author_id = format!("studio.tool.unfiled.{}", kebab(tool.key()));
            catalog.insert(UiDescriptor::new(author_id, UiKind::Tool, tool.label(), "tool").with_command("viewer.set_tool").with_alias(&format!("tool.{}", tool.key())))?;
        }
        for panel in Panel::ALL {
            let author_id = format!("studio.tab.{}", kebab(panel.key()));
            catalog.insert(UiDescriptor::new(author_id, UiKind::Panel, panel.label(), "panel").with_alias(&format!("panel.{}", panel.key())))?;
        }
        let menu = self.menu_bar();
        for walked in menu.walk() {
            let node = walked.node;
            if node.kind != MenuNodeKind::Item || node.data_label {
                continue;
            }
            let author_id = format!("studio.menu.{}", kebab(node.id.strip_prefix("studio.").unwrap_or(&node.id)));
            let mut descriptor = UiDescriptor::new(author_id, UiKind::MenuItem, &node.label, "menu").with_command(&node.id).with_alias(&node.id);
            descriptor.menu_only = node.menu_only;
            if !node.help.is_empty() {
                descriptor.summary = Some(node.help.clone());
            }
            catalog.insert(descriptor)?;
        }
        Ok(catalog)
    }
}

/// `catalog.list` and `catalog.resolve`.
pub(super) fn run(viewer: &Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    let catalog = viewer.ui_catalog().map_err(|e| CommandRefusal::new("catalog_invalid", json!({"reason": e.code(), "detail": format!("{e:?}")})))?;
    if id == "catalog.list" {
        return Ok(catalog.json());
    }
    let wanted = f.text("id")?;
    catalog.resolve(&wanted).map(hsk_studio_controls::UiDescriptor::json).ok_or_else(|| CommandRefusal::new("unknown_command", json!({"id": wanted})))
}
