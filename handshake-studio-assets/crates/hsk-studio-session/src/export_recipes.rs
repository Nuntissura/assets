//! User export recipes in the session (lane SMB-DELIVERY-QUEUE-1; STU-VID-063 "a user recipe is the same
//! record as a shipped one", STU-IO-110 recipes are portable): a saved document carries its user
//! `StudioExportRecipe` records (reel) as the document entry `studio/export_recipes.<sha256>`, so a recipe
//! travels with the project. STU-IO-166 places recipes in SurrealDB tables; this asset binds no store, so the
//! document entry is the asset-local home (lead ruling 2026-11-08, proposed as an SD item); a host store can
//! import the entry later.
//!
//! The bytes belong to their producer, like `raster_records`: the harness viewer writes
//! `{"schema_id":"hsk.studio.export_recipes@1","recipes":[<StudioExportRecipe::to_json>, ...]}` (recipes in
//! `recipe_id` byte order) and parses every recipe back through reel's checked setters on every set and read
//! (`hsk-studio-harness-view` `timeline_export::user_recipes`). The session holds and saves them under the
//! common document-entry rules (`Session::set_document_entry`: the package limits, not a revision, not
//! undoable by itself). The entry names no sequence, track or clip. Tests: hsk-studio-harness
//! tests/delivery_queue.rs.

/// Document-entry token of the user export recipes.
pub const EXPORT_RECIPES_ENTRY: &str = "export_recipes";
