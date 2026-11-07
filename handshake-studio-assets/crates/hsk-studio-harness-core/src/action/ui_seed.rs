//! The command half of the descriptor catalog (RF-05 seed, lane SMB-COMMAND-SURFACE-1): one
//! `UiDescriptor` per corpus row, timeline row and viewer row, from those enumerations; the window half
//! is `Viewer::ui_catalog`. A former id stays an alias: `viewer.<method>` of a viewer row, the spec
//! command ids a timeline row names.

use super::commands::RUN_COMMANDS;
use crate::manual::{DRIVE_TIMELINE, DRIVE_VIEWER, viewer_command_id};
use hsk_studio_controls::{UiCatalog, UiCatalogError, UiDescriptor, UiKind, kebab};

fn author_id(command_id: &str) -> String {
    format!("studio.command.{}", kebab(command_id))
}

fn domain_of(command_id: &str) -> &str {
    command_id.split('.').next().unwrap_or_default()
}

/// Every command of this build as a descriptor. Refused whole when an id rule breaks.
pub fn ui_seed() -> Result<UiCatalog, UiCatalogError> {
    let mut catalog = UiCatalog::new();
    for row in RUN_COMMANDS {
        catalog.insert(UiDescriptor::new(author_id(row.command_id), UiKind::Command, row.display_name, domain_of(row.command_id)).with_command(row.command_id))?;
    }
    for row in DRIVE_TIMELINE {
        let id = format!("timeline.{}", row.key);
        let mut descriptor = UiDescriptor::new(author_id(&id), UiKind::Command, row.key, "timeline").with_command(&id);
        for spec in row.spec_commands {
            descriptor = descriptor.with_alias(spec);
        }
        catalog.insert(descriptor)?;
    }
    for row in DRIVE_VIEWER {
        let id = viewer_command_id(row.method);
        catalog.insert(UiDescriptor::new(author_id(&id), UiKind::Command, row.method, domain_of(&id)).with_command(&id).with_alias(&format!("viewer.{}", row.method)))?;
    }
    Ok(catalog)
}
