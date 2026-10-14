use crate::{Diagnostic, Placement, SelectedWindow};
use serde::{Deserialize, Serialize};

/// Durable recovery contains no native handle, PID, or automatically actionable identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    pub schema: u32,
    pub semantic_reference: String,
    pub original: Placement,
    #[serde(default)]
    pub pending: Option<Placement>,
}
impl RecoveryRecord {
    pub fn from_selection<L>(selected: &SelectedWindow<L>) -> Self {
        Self { schema: 1, semantic_reference: selected.semantic_reference().into(), original: selected.original(), pending: selected.pending() }
    }
    /// Rebinding is explicit: this record never discovers or controls another process.
    pub fn apply_to<L>(&self, selected: &mut SelectedWindow<L>) -> Result<(), Diagnostic> {
        if self.schema != 1 || self.semantic_reference != selected.semantic_reference() {
            return Err(Diagnostic::new("recovery_mismatch", "Explicit selection does not match the recovery reference"));
        }
        self.original.bounds.validate()?;
        if let Some(pending) = self.pending { pending.bounds.validate()?; }
        if self.original.state != crate::WindowState::Normal || self.pending.is_some_and(|placement| placement.state != crate::WindowState::Normal) {
            return Err(Diagnostic::new("unsupported_recovery_state", "Recovery requires quietly restorable normal placements"));
        }
        if selected.pending().is_some() && selected.pending() != self.pending {
            return Err(Diagnostic::new("pending_recovery_conflict", "Recovery record cannot erase or replace an unsettled live dispatch"));
        }
        selected.original = self.original;
        selected.pending.set(self.pending);
        Ok(())
    }
}
