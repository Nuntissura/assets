//! Design files in the window (lane SMB-FIG-EDIT-1): FILE > Open turns a design file (`.fig`) into
//! an editable native document ([`fig_import`]), and the Properties panel shows the instance
//! inspector when the selected layer draws a component instance ([`instance`]).
//!
//! Import ([STU-IO-006] row 35, I and EP): interop-fig `import_fig` + `decode_document` + `to_native`
//! make the native proposal; the window's adapter writes it as a native package beside the source
//! (`<stem>.handshake`, never over an existing file without overwrite) whose layers, artboards and
//! paths edit with the vector, layer and properties surfaces (session entry `vector`), whose text
//! layers edit through the session entry `text`, whose effects ride as the package records entry,
//! and whose receipt lists every loss ([STU-IO-005]: mapping and decode losses of the importer plus
//! the adapter's own) and carries the design records the entries cannot hold (components, sets,
//! instances, variables, reactions, auto layout) with the instance bindings.
//!
//! Instances ([STU-DS-003/008/009/016/017], bound contextually per [STU-DS-160]): see [`instance`].
#![deny(clippy::arithmetic_side_effects)]

pub mod fig_import;
pub mod instance;

pub use fig_import::{FigImportReport, is_design_file};
pub use instance::{DesignCommand, DesignState};
