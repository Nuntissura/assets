//! Volatile host-owned description drafts. Tokens are crate-generated, never raw binary ingress.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use yrs::updates::decoder::Decode;
use yrs::{
    ClientID, Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, Transact, Update,
};

const ROOT: &str = "description";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    workspace: String,
    record: String,
    generation: String,
}

impl Domain {
    pub fn new(workspace: &str, record: &str, generation: &str) -> Result<Self, Error> {
        if [workspace, record, generation]
            .iter()
            .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            workspace: workspace.into(),
            record: record.into(),
            generation: generation.into(),
        })
    }
}

/// Host-selected byte budgets; no implicit production defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    text: usize,
    update: usize,
    state: usize,
}

impl Limits {
    pub fn new(text: usize, update: usize, state: usize) -> Result<Self, Error> {
        if [text, update, state]
            .iter()
            .any(|n| *n == 0 || *n > u32::MAX as usize)
        {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            text,
            update,
            state,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    InvalidRange,
    DomainMismatch,
    LimitsMismatch,
    ReplicaReuse,
    BudgetExceeded,
    Unavailable,
    Internal,
}

/// Immutable provenance token; private bytes cannot be imported or deserialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftUpdate {
    domain: Domain,
    limits: Limits,
    replica: u64,
    bytes: Vec<u8>,
}

/// Volatile checkpoint; cloning retains immutable provenance, not a live replica identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftCheckpoint {
    domain: Domain,
    limits: Limits,
    replicas: BTreeSet<u64>,
    bytes: Vec<u8>,
}

struct State {
    doc: Doc,
    replicas: BTreeSet<u64>,
}

/// Cloned handles share one synchronized replica. Hosts allocate unique IDs for distinct replicas.
#[derive(Clone)]
pub struct Draft {
    domain: Domain,
    limits: Limits,
    replica: u64,
    state: Arc<Mutex<State>>,
}

fn document(replica: u64) -> Doc {
    let doc = Doc::with_options(Options {
        offset_kind: OffsetKind::Bytes,
        ..Options::with_client_id(ClientID::new(replica))
    });
    doc.get_or_insert_text(ROOT);
    doc
}

fn apply(doc: &Doc, bytes: &[u8]) -> Result<(), Error> {
    // Only bytes encoded by this crate's plain-text operations can reach this decoder.
    let update = Update::decode_v1(bytes).map_err(|_| Error::Internal)?;
    doc.try_transact_mut()
        .map_err(|_| Error::Unavailable)?
        .apply_update(update)
        .map_err(|_| Error::Internal)
}

fn snapshot(doc: &Doc) -> Result<Vec<u8>, Error> {
    Ok(doc
        .try_transact()
        .map_err(|_| Error::Unavailable)?
        .encode_state_as_update_v1(&StateVector::default()))
}

fn within_state_budget(doc: &Doc, replicas: usize, limit: usize) -> Result<bool, Error> {
    // Include replica-provenance metadata, including no-op writers, in the state budget.
    let bytes = snapshot(doc)?.len();
    Ok(replicas
        .checked_mul(8)
        .and_then(|n| n.checked_add(bytes))
        .is_some_and(|n| n <= limit))
}

fn text(doc: &Doc) -> Result<String, Error> {
    let root = doc.get_or_insert_text(ROOT);
    let txn = doc.try_transact().map_err(|_| Error::Unavailable)?;
    Ok(root.get_string(&txn))
}

impl Draft {
    pub fn new(domain: Domain, replica: u64, limits: Limits) -> Result<Self, Error> {
        if replica == 0 || replica >= (1u64 << 53) {
            return Err(Error::InvalidConfig);
        }
        let doc = document(replica);
        if !within_state_budget(&doc, 1, limits.state)? {
            return Err(Error::BudgetExceeded);
        }
        Ok(Self {
            domain,
            limits,
            replica,
            state: Arc::new(Mutex::new(State {
                doc,
                replicas: BTreeSet::from([replica]),
            })),
        })
    }

    pub fn fork(checkpoint: &DraftCheckpoint, replica: u64) -> Result<Self, Error> {
        if checkpoint.replicas.contains(&replica) {
            return Err(Error::ReplicaReuse);
        }
        let draft = Self::new(checkpoint.domain.clone(), replica, checkpoint.limits)?;
        {
            let mut state = draft.state.lock().map_err(|_| Error::Unavailable)?;
            apply(&state.doc, &checkpoint.bytes)?;
            state.replicas.extend(&checkpoint.replicas);
            if !within_state_budget(&state.doc, state.replicas.len(), draft.limits.state)? {
                return Err(Error::BudgetExceeded);
            }
        }
        Ok(draft)
    }

    pub fn text(&self) -> Result<String, Error> {
        text(&self.state.lock().map_err(|_| Error::Unavailable)?.doc)
    }

    /// Reports missing causal predecessors; this is not durable acknowledgment.
    pub fn has_missing_updates(&self) -> Result<bool, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let txn = state.doc.try_transact().map_err(|_| Error::Unavailable)?;
        Ok(txn.has_missing_updates())
    }

    pub fn checkpoint(&self) -> Result<DraftCheckpoint, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        Ok(DraftCheckpoint {
            domain: self.domain.clone(),
            limits: self.limits,
            replicas: state.replicas.clone(),
            bytes: snapshot(&state.doc)?,
        })
    }

    /// Splice byte offsets, rejecting partial UTF-8 scalars. Mutation is atomic on every error.
    pub fn splice_utf8(
        &self,
        index: usize,
        delete: usize,
        insert: &str,
    ) -> Result<DraftUpdate, Error> {
        self.splice(index, delete, insert, false)
    }

    /// Splice UTF-16 code-unit offsets, rejecting partial surrogate pairs.
    pub fn splice_utf16(
        &self,
        index: usize,
        delete: usize,
        insert: &str,
    ) -> Result<DraftUpdate, Error> {
        self.splice(index, delete, insert, true)
    }

    fn splice(
        &self,
        index: usize,
        delete: usize,
        insert: &str,
        utf16: bool,
    ) -> Result<DraftUpdate, Error> {
        if insert.len() > self.limits.text || insert.len() > self.limits.update {
            return Err(Error::BudgetExceeded);
        }
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let old = text(&state.doc)?;
        let end = index.checked_add(delete).ok_or(Error::InvalidRange)?;
        let (index, end) = if utf16 {
            (byte_offset(&old, index)?, byte_offset(&old, end)?)
        } else {
            (index, end)
        };
        if end > old.len() || !old.is_char_boundary(index) || !old.is_char_boundary(end) {
            return Err(Error::InvalidRange);
        }
        if old
            .len()
            .checked_sub(end - index)
            .and_then(|n| n.checked_add(insert.len()))
            .ok_or(Error::BudgetExceeded)?
            > self.limits.text
        {
            return Err(Error::BudgetExceeded);
        }
        let clock = state
            .doc
            .try_transact()
            .map_err(|_| Error::Unavailable)?
            .state_vector()
            .get(&ClientID::new(self.replica));
        clock
            .checked_add(insert.len() as u32)
            .ok_or(Error::BudgetExceeded)?;
        let candidate = document(self.replica);
        apply(&candidate, &snapshot(&state.doc)?)?;
        let root = candidate.get_or_insert_text(ROOT);
        let bytes = {
            let mut txn = candidate
                .try_transact_mut()
                .map_err(|_| Error::Unavailable)?;
            if end > index {
                root.remove_range(&mut txn, index as u32, (end - index) as u32);
            }
            root.insert(&mut txn, index as u32, insert);
            txn.encode_update_v1()
        };
        if bytes.len() > self.limits.update
            || !within_state_budget(&candidate, state.replicas.len(), self.limits.state)?
        {
            return Err(Error::BudgetExceeded);
        }
        state.doc = candidate;
        Ok(DraftUpdate {
            domain: self.domain.clone(),
            limits: self.limits,
            replica: self.replica,
            bytes,
        })
    }

    pub fn merge(&self, update: &DraftUpdate) -> Result<(), Error> {
        if self.domain != update.domain {
            return Err(Error::DomainMismatch);
        }
        if self.limits != update.limits {
            return Err(Error::LimitsMismatch);
        }
        if update.bytes.len() > self.limits.update {
            return Err(Error::BudgetExceeded);
        }
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        let candidate = document(self.replica);
        apply(&candidate, &snapshot(&state.doc)?)?;
        apply(&candidate, &update.bytes)?;
        let replicas =
            state.replicas.len() + usize::from(!state.replicas.contains(&update.replica));
        if text(&candidate)?.len() > self.limits.text
            || !within_state_budget(&candidate, replicas, self.limits.state)?
        {
            return Err(Error::BudgetExceeded);
        }
        state.doc = candidate;
        state.replicas.insert(update.replica);
        Ok(())
    }
}

fn byte_offset(text: &str, target: usize) -> Result<usize, Error> {
    let mut units = 0;
    for (offset, ch) in text.char_indices() {
        if units == target {
            return Ok(offset);
        }
        units += ch.len_utf16();
    }
    if units == target {
        Ok(text.len())
    } else {
        Err(Error::InvalidRange)
    }
}
