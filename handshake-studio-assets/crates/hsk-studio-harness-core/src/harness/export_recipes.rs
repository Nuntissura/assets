//! The storage step of `export.recipe_save` and `export.recipe_delete` (lane SMB-DELIVERY-QUEUE-1;
//! STU-VID-063, STU-IO-110): the user export recipes of the open document live in the session document entry
//! [`EXPORT_RECIPES_ENTRY`], saved with the package and restored on reopen. Harness-core has no reel edge, so
//! the recipes are parsed and checked by their producer (harness-view `timeline_export::user_recipes`, every
//! value through reel's `StudioExportRecipe::set_parameter`) before this step stores the bytes. A change is one
//! entry-only undo step (no revision, like every document entry) with the caller's attribution, one op record
//! and one Observe frame of codes and counters; a refusal changes nothing. Not recordable: a recording
//! reports it typed ([STU-AUT-109]).
#![deny(clippy::arithmetic_side_effects)]

use super::{EntryRestore, HistoryStep, Run};
pub use hsk_studio_session::EXPORT_RECIPES_ENTRY;
use serde_json::{Value, json};

/// Largest entry body a save stores: the producer's one bound for save and read (harness-view
/// `timeline_export::MAX_USER_RECIPES_BYTES`, review 44da1364 F2); the package limits apply as well. A delete
/// that shrinks an entry already over it (another writer's) is stored, so the user can repair the document.
pub const MAX_EXPORT_RECIPES_BYTES: usize = 8 << 20;

impl Run<'_> {
    /// Stores `bytes` (`None`: removes the entry) as the user export recipes after `export.recipe_save`
    /// (`delete` false) or `export.recipe_delete` (`delete` true) of `recipe_id`; `recipes` is the count the
    /// new body holds. Equal bytes are no step. Refusals: `not_open`, `canceled`, `export_recipes_too_large` (over
    /// [`MAX_EXPORT_RECIPES_BYTES`], unless a delete shrinks the entry), the session's entry codes. One record `set_export_recipes`, detail `{command, recipe_id, recipes,
    /// changed}`.
    pub fn set_export_recipes(&mut self, delete: bool, recipe_id: &str, bytes: Option<Vec<u8>>, recipes: usize) -> bool {
        const OP: &str = "set_export_recipes";
        let command: &'static str = if delete { "export.recipe_delete" } else { "export.recipe_save" };
        self.note_unrecordable(command);
        let before = self.frames.count();
        if self.session.is_none() {
            return self.push(OP, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(command, None);
        if self.cancel.check().is_err() {
            return self.own_op(OP, before, Err("canceled"));
        }
        let cancel = self.cancel;
        let previous = self.session.as_ref().and_then(|s| s.document_entry(EXPORT_RECIPES_ENTRY)).map(<[u8]>::to_vec);
        // Over the bound: refused, except a delete that shrinks the entry (module docs, the repair path).
        let shrinks = |b: &Vec<u8>| delete && previous.as_ref().is_some_and(|p| b.len() < p.len());
        if bytes.as_ref().is_some_and(|b| b.len() > MAX_EXPORT_RECIPES_BYTES && !shrinks(b)) {
            return self.own_op(OP, before, Err("export_recipes_too_large"));
        }
        let Some(session) = self.session.as_mut() else {
            return self.push(OP, before, Err("not_open"), None, None);
        };
        let changed = previous != bytes;
        if changed {
            if let Err(e) = session.set_document_entry(EXPORT_RECIPES_ENTRY, bytes, cancel) {
                return self.own_op(OP, before, Err(e.code()));
            }
            let revision = session.document_revision();
            let restore = EntryRestore { token: EXPORT_RECIPES_ENTRY.to_string(), bytes: previous };
            self.undo_stack.push(HistoryStep { label: format!("{command} {recipe_id}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, containers: None, layout: None, ctx, revision });
            self.redo_stack.clear();
        }
        self.own_op(OP, before, Ok(json!({"command": command, "recipe_id": recipe_id, "recipes": recipes, "changed": changed})))
    }
}
