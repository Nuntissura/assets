//! One multicam step = one undo step (lane SMB-PR-MULTICAM-2; STU-VID-040 "Every operation in this
//! sub-section is one reversible `StudioHistoryEntry`. ... A multicam record pass is one step.";
//! STU-VID-033 "`flatten` ... is one reversible history step").
//!
//! A multicam planner result (Pulse `MulticamProposal`) changes zero or more document sequences
//! (replace an existing one, insert a created multicam source sequence, remove one) AND the Folio
//! multicam record set the session keeps as the document entry `multicam` (module `multicam`). A
//! [`MulticamStep`] carries both: every [`SequenceEdit`] quotes the live value it expects, and
//! `records_expect` quotes the live record set (`None` = no entry stored). The session applies the
//! whole step as ONE revision (`revision + 1`, also when only the records change: declared reading,
//! so a records-only step is a history entry with a receipt), ONE receipt and ONE inverse
//! ([`crate::Applied::multicam_inverse`]: replace swaps, insert <-> remove at the live index,
//! records swapped, edits in reverse order); the undo's own inverse is the redo. Any refusal leaves
//! the document, the entry and the receipts untouched. Removing a sequence a clip still nests is
//! refused by Folio validation (`folio_rejected`); records that do not validate against the
//! resulting document are refused `folio_rejected` (`budget_exceeded` / `canceled` keep their codes).
//!
//! The step shares the submit pipeline (grant, idempotency key and receipt, admission, commit
//! authority, publish) with provider submits. A commit authority receives the successor DOCUMENT;
//! the record set travels in the binding hash (the whole step is bound), and the session stores the
//! entry only when it publishes that successor (declared reading: the entry is session-held state).
use crate::error::SessionError;
use crate::timeline::valid_command_id;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    self as folio, MAX_SEQUENCES, Resolver, Snapshot, StudioMulticamRecords, StudioSequence,
    validate_multicam_records,
};

/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const MULTICAM_STEP_PROVIDER: &str = "multicam.step";
/// Most sequence edits one step carries (every sequence of a full document removed and as many
/// created); more is `budget_exceeded` before any work.
pub const MAX_MULTICAM_STEP_EDITS: usize = MAX_SEQUENCES.saturating_mul(2);

/// One sequence value change inside a [`MulticamStep`].
#[derive(Clone, Debug, PartialEq)]
pub enum SequenceEdit {
    /// The live sequence `expect` (by id) becomes `write` (same id).
    Replace { expect: StudioSequence, write: StudioSequence },
    /// `write` (an id the document does not hold) is inserted at index `at` (at most the count).
    Insert { at: usize, write: StudioSequence },
    /// The live sequence `expect` is removed.
    Remove { expect: StudioSequence },
}

impl SequenceEdit {
    fn sequence_id(&self) -> &str {
        match self {
            Self::Replace { expect, .. } | Self::Remove { expect } => &expect.sequence_id,
            Self::Insert { write, .. } => &write.sequence_id,
        }
    }
}

/// A multicam step (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct MulticamStep {
    pub command_id: String,
    pub correlation_id: u64,
    /// Applied in order; each sequence id appears in at most one edit.
    pub sequences: Vec<SequenceEdit>,
    /// The record set the session holds now (`None` = no `multicam` entry).
    pub records_expect: Option<StudioMulticamRecords>,
    /// The record set after the step (`None` = the entry is removed).
    pub records_write: Option<StudioMulticamRecords>,
}

impl MulticamStep {
    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let invalid = |_| SessionError::Provider("invalid_input");
        let mut out = format!("hsk-studio-multicam-step-1\0{}\0{}\0", self.command_id, self.correlation_id).into_bytes();
        for edit in &self.sequences {
            let (tag, at, a, b) = match edit {
                SequenceEdit::Replace { expect, write } => ("replace", 0, Some(expect), Some(write)),
                SequenceEdit::Insert { at, write } => ("insert", *at, None, Some(write)),
                SequenceEdit::Remove { expect } => ("remove", 0, Some(expect), None),
            };
            out.extend_from_slice(format!("{tag}\0{at}\0").as_bytes());
            out.extend_from_slice(&serde_json::to_vec(&a).map_err(invalid)?);
            out.push(0);
            out.extend_from_slice(&serde_json::to_vec(&b).map_err(invalid)?);
            out.push(0);
        }
        out.extend_from_slice(&serde_json::to_vec(&self.records_expect).map_err(invalid)?);
        out.push(0);
        out.extend_from_slice(&serde_json::to_vec(&self.records_write).map_err(invalid)?);
        Ok(out)
    }
}

/// The checked outcome of a step: the successor document (`revision + 1`), the entry bytes to store
/// (`Some(None)` = remove the entry, `None` = entry unchanged) and the inverse step.
pub(crate) struct Checked {
    pub successor: Snapshot,
    pub entry: Option<Option<Vec<u8>>>,
    pub inverse: MulticamStep,
}

fn folio_code(d: folio::Diagnostic) -> SessionError {
    match d.code {
        folio::Code::Canceled => SessionError::Canceled,
        folio::Code::BudgetExceeded => SessionError::Provider("budget_exceeded"),
        _ => SessionError::Provider("folio_rejected"),
    }
}

/// Checks `step` against the live document and the live `multicam` entry bytes; `Ok(None)` when it
/// changes nothing. Nothing is published here.
pub(crate) fn successor(
    snapshot: &Snapshot,
    live_entry: Option<&[u8]>,
    step: &MulticamStep,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Checked>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    if step.sequences.len() > MAX_MULTICAM_STEP_EDITS {
        return Err(SessionError::Provider("budget_exceeded"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for edit in &step.sequences {
        if !ids.insert(edit.sequence_id()) {
            return Err(SessionError::Provider("invalid_input"));
        }
    }
    let document = snapshot.document();
    let mut sequences = document.sequences.clone();
    let mut inverse = Vec::with_capacity(step.sequences.len());
    for edit in &step.sequences {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let position = |id: &str, list: &[StudioSequence]| list.iter().position(|s| s.sequence_id == id);
        match edit {
            SequenceEdit::Replace { expect, write } => {
                if write.sequence_id != expect.sequence_id {
                    return Err(SessionError::Provider("invalid_input"));
                }
                let i = position(&expect.sequence_id, &sequences).ok_or(SessionError::Provider("invalid_target"))?;
                if sequences[i] != *expect {
                    return Err(SessionError::Provider("value_conflict"));
                }
                sequences[i] = write.clone();
                inverse.push(SequenceEdit::Replace { expect: write.clone(), write: expect.clone() });
            }
            SequenceEdit::Insert { at, write } => {
                if position(&write.sequence_id, &sequences).is_some() {
                    return Err(SessionError::Provider("value_conflict"));
                }
                if *at > sequences.len() {
                    return Err(SessionError::Provider("invalid_input"));
                }
                sequences.insert(*at, write.clone());
                inverse.push(SequenceEdit::Remove { expect: write.clone() });
            }
            SequenceEdit::Remove { expect } => {
                let i = position(&expect.sequence_id, &sequences).ok_or(SessionError::Provider("invalid_target"))?;
                if sequences[i] != *expect {
                    return Err(SessionError::Provider("value_conflict"));
                }
                sequences.remove(i);
                inverse.push(SequenceEdit::Insert { at: i, write: expect.clone() });
            }
        }
    }
    inverse.reverse();
    // The live record set: bytes that do not parse never equal an expectation.
    let live: Option<StudioMulticamRecords> = match live_entry {
        None => None,
        Some(bytes) => Some(serde_json::from_slice(bytes).map_err(|_| SessionError::Provider("value_conflict"))?),
    };
    if live != step.records_expect {
        return Err(SessionError::Provider("value_conflict"));
    }
    let sequences_changed = sequences != document.sequences;
    let records_changed = step.records_write != step.records_expect;
    if !sequences_changed && !records_changed {
        return Ok(None);
    }
    let mut next = document.clone();
    next.sequences = sequences;
    next.revision = document.revision.checked_add(1).ok_or(SessionError::Provider("revision_overflow"))?;
    let successor = folio::validate_document(next, budget, cancel, resolver).map_err(folio_code)?;
    if let Some(records) = &step.records_write {
        validate_multicam_records(records, successor.document(), budget, cancel).map_err(folio_code)?;
    }
    let entry = if records_changed {
        Some(match &step.records_write {
            None => None,
            Some(records) => Some(serde_json::to_vec(records).map_err(|_| SessionError::Provider("invalid_input"))?),
        })
    } else {
        None
    };
    Ok(Some(Checked {
        successor,
        entry,
        inverse: MulticamStep {
            command_id: step.command_id.clone(),
            correlation_id: step.correlation_id,
            sequences: inverse,
            records_expect: step.records_write.clone(),
            records_write: step.records_expect.clone(),
        },
    }))
}
