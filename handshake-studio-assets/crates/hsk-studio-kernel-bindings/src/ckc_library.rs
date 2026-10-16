#![deny(clippy::arithmetic_side_effects)]
//! Library edits over the CKC port (lane SMB-LR-LIBRARY-WRITE; Lightroom Classic Library module
//! ratings, flags, colour labels and keywords on a selection, LR-28/LR-29). Studio resolves an
//! operator- or model-level edit ("one star more", "toggle pick", "toggle red", "replace the
//! keywords") against FRESH port reads into absolute [`MetadataChange`]s and sends them as
//! all-or-nothing batches with the read revisions as preconditions (STU-ASSET-023, STU-SDB-004).
//! Nothing is cached or stored here (STU-ASSET-004/017): every call reads CKC, writes CKC, and
//! returns CKC's receipts.
//!
//! Selection is the canonical set (as STU-RAW-017 requires for develop multi-image apply): an
//! explicit asset list, or a rule resolved by paging `asset.search` to its end, never a visible
//! or loaded subset. A selection larger than `max_batch` is sent as several batches, each
//! atomic; a failure after some batches were accepted returns those receipts in the error
//! (`applied`), so a partial outcome is never silent. A stale precondition is reported per asset
//! (expected vs current revision, found by re-reading) as [`LibraryWriteError::Conflict`]; with
//! [`ConflictPolicy::RebaseOnce`] the edit is re-resolved once against the fresh state (edits are
//! intents, so "one star more" applies to the current rating) under a new idempotency key.
//! Lightroom Classic behaviour (rating step clamping, pick toggling) is UNVERIFIED (D5).
use crate::ckc::{
    AssetId, AssetMetadata, BatchTarget, CatalogRule, CollectionCommand, CollectionId, CkcActor, CkcCatalogPort, CkcError, CkcLimits, CkcReceipt,
    ColorLabel, Flag, KeywordPath, MetadataBatch, MetadataChange, Rating, SearchRequest, check_key, check_keyword_set,
};
use std::collections::BTreeSet;

/// An edit as the operator or a model lane states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryEdit {
    SetRating(Rating),
    /// Adds `delta` stars, clamped to 0..=5.
    StepRating(i8),
    SetFlag(Flag),
    /// Sets `flag`, or `Unflagged` when the asset already has it.
    ToggleFlag(Flag),
    SetLabel(Option<ColorLabel>),
    /// Sets the label, or clears it when the asset already has it.
    ToggleLabel(ColorLabel),
    AddKeywords(Vec<KeywordPath>),
    RemoveKeywords(Vec<KeywordPath>),
    SetKeywords(Vec<KeywordPath>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    Assets(Vec<AssetId>),
    /// Every asset `asset.search` returns for the rule, all pages.
    Rule(CatalogRule),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    Fail,
    RebaseOnce,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionConflict {
    pub asset_id: AssetId,
    pub expected: u64,
    pub current: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryWriteReport {
    /// One receipt per accepted batch, in send order.
    pub receipts: Vec<CkcReceipt>,
    /// Assets whose metadata changed.
    pub changed: Vec<AssetId>,
    /// Assets the edit left as they were (no command sent for them).
    pub unchanged: Vec<AssetId>,
    /// Batches re-resolved after a conflict.
    pub rebased: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryWriteError {
    /// Refused before any command was sent.
    Invalid(CkcError),
    /// Preconditions moved; `applied` holds the receipts of batches already accepted.
    Conflict { conflicts: Vec<RevisionConflict>, applied: Vec<CkcReceipt> },
    /// The port refused; `applied` as above.
    Port { error: CkcError, applied: Vec<CkcReceipt> },
}

impl LibraryWriteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(e) | Self::Port { error: e, .. } => e.code(),
            Self::Conflict { .. } => "ckc_library_conflict",
        }
    }
}

fn check_edits(edits: &[LibraryEdit], l: &CkcLimits) -> Result<(), CkcError> {
    if edits.is_empty() {
        return Err(CkcError::Invalid("ckc_no_changes"));
    }
    if edits.len() > l.max_changes {
        return Err(CkcError::Limit("ckc_changes"));
    }
    for e in edits {
        match e {
            LibraryEdit::AddKeywords(k) | LibraryEdit::RemoveKeywords(k) | LibraryEdit::SetKeywords(k) => {
                check_keyword_set(k, l)?;
            }
            LibraryEdit::StepRating(d) if !(-5..=5).contains(d) => return Err(CkcError::Invalid("ckc_rating_step")),
            _ => {}
        }
    }
    Ok(())
}

/// Absolute changes `edits` make to `meta` (empty = nothing changes). Keyword edits fold into one
/// `SetKeywords` so the result does not depend on the host's add/remove ordering.
pub fn resolve_edits(meta: &AssetMetadata, edits: &[LibraryEdit], l: &CkcLimits) -> Result<Vec<MetadataChange>, CkcError> {
    let (mut rating, mut flag, mut label, mut keywords) = (meta.rating, meta.flag, meta.color_label, meta.keywords.clone());
    for e in edits {
        match e {
            LibraryEdit::SetRating(r) => rating = *r,
            LibraryEdit::StepRating(d) => {
                let v = i16::from(rating.value()).saturating_add(i16::from(*d)).clamp(0, 5);
                rating = Rating::new(u8::try_from(v).map_err(|_| CkcError::Invalid("ckc_rating_range"))?)?;
            }
            LibraryEdit::SetFlag(f) => flag = *f,
            LibraryEdit::ToggleFlag(f) => flag = if flag == *f { Flag::Unflagged } else { *f },
            LibraryEdit::SetLabel(x) => label = *x,
            LibraryEdit::ToggleLabel(x) => label = if label == Some(*x) { None } else { Some(*x) },
            LibraryEdit::AddKeywords(ks) => {
                for k in ks {
                    if !keywords.contains(k) {
                        keywords.push(k.clone());
                    }
                }
            }
            LibraryEdit::RemoveKeywords(ks) => keywords.retain(|k| !ks.contains(k)),
            LibraryEdit::SetKeywords(ks) => keywords.clone_from(ks),
        }
    }
    check_keyword_set(&keywords, l)?;
    let mut out = Vec::new();
    if rating != meta.rating {
        out.push(MetadataChange::SetRating(rating));
    }
    if flag != meta.flag {
        out.push(MetadataChange::SetFlag(flag));
    }
    if label != meta.color_label {
        out.push(MetadataChange::SetColorLabel(label));
    }
    if keywords != meta.keywords {
        out.push(MetadataChange::SetKeywords(keywords));
    }
    Ok(out)
}

/// Resolves a selection to its canonical asset list (explicit list, or every search page).
pub fn resolve_selection<P: CkcCatalogPort + ?Sized>(
    port: &P,
    actor: &CkcActor,
    selection: &Selection,
    l: &CkcLimits,
) -> Result<Vec<AssetId>, CkcError> {
    let ids = match selection {
        Selection::Assets(ids) => {
            if ids.len() > l.max_selection {
                return Err(CkcError::Limit("ckc_selection"));
            }
            ids.clone()
        }
        Selection::Rule(rule) => {
            let mut out = Vec::new();
            let mut offset = 0u64;
            loop {
                let req = SearchRequest { rule: Some(rule.clone()), sort: Vec::new(), offset, limit: l.max_page };
                req.validate(l)?;
                let page = port.search(actor, &req)?;
                if page.total > l.max_selection as u64 || out.len().saturating_add(page.assets.len()) > l.max_selection {
                    return Err(CkcError::Limit("ckc_selection"));
                }
                out.extend(page.assets.into_iter().map(|a| a.asset_id));
                match page.next_offset {
                    // A host must advance; a non-advancing cursor would loop forever.
                    Some(n) if n > offset => offset = n,
                    Some(_) => return Err(CkcError::Invalid("ckc_search_cursor")),
                    None => break,
                }
            }
            out
        }
    };
    if ids.is_empty() {
        return Err(CkcError::Invalid("ckc_selection_empty"));
    }
    let mut seen = BTreeSet::new();
    if !ids.iter().all(|a| seen.insert(a)) {
        return Err(CkcError::Invalid("ckc_selection_repeated_asset"));
    }
    Ok(ids)
}

fn key(prefix: &str, chunk: usize, attempt: u32) -> String {
    format!("{prefix}.{chunk}.{attempt}")
}

/// Applies `edits` to every asset of `selection` through `port`. `key_prefix` makes the batch
/// idempotency keys (`<prefix>.<batch>.<attempt>`); reusing a prefix replays prior receipts.
pub fn apply_library_edit<P: CkcCatalogPort + ?Sized>(
    port: &P,
    actor: &CkcActor,
    selection: &Selection,
    edits: &[LibraryEdit],
    key_prefix: &str,
    policy: ConflictPolicy,
    l: &CkcLimits,
) -> Result<LibraryWriteReport, LibraryWriteError> {
    check_edits(edits, l).map_err(LibraryWriteError::Invalid)?;
    // The longest key this call can build must be a valid key (checked before any port call).
    check_key(&key(key_prefix, usize::MAX, 1), l).map_err(LibraryWriteError::Invalid)?;
    let ids = resolve_selection(port, actor, selection, l).map_err(LibraryWriteError::Invalid)?;
    let mut report = LibraryWriteReport { receipts: Vec::new(), changed: Vec::new(), unchanged: Vec::new(), rebased: 0 };
    let chunk_size = l.max_batch.max(1);
    for (chunk_index, chunk) in ids.chunks(chunk_size).enumerate() {
        let mut attempt = 0u32;
        loop {
            let port_err = |error: CkcError, report: &LibraryWriteReport| LibraryWriteError::Port { error, applied: report.receipts.clone() };
            let mut targets = Vec::new();
            let mut unchanged = Vec::new();
            for id in chunk {
                let meta = port.get(actor, id).map_err(|e| port_err(e, &report))?;
                // A refused resolution (e.g. keywords past the bound) sends nothing for this batch.
                let changes = resolve_edits(&meta, edits, l).map_err(|e| {
                    if report.receipts.is_empty() { LibraryWriteError::Invalid(e) } else { port_err(e, &report) }
                })?;
                if changes.is_empty() {
                    unchanged.push(id.clone());
                } else {
                    targets.push(BatchTarget { asset_id: id.clone(), expected_revision: meta.revision, changes });
                }
            }
            if targets.is_empty() {
                report.unchanged.extend(unchanged);
                break;
            }
            let batch = MetadataBatch { idempotency_key: key(key_prefix, chunk_index, attempt), targets };
            match port.update_library_metadata_batch(actor, &batch) {
                Ok(receipt) => {
                    report.receipts.push(receipt);
                    report.changed.extend(batch.targets.iter().map(|t| t.asset_id.clone()));
                    report.unchanged.extend(unchanged);
                    break;
                }
                Err(CkcError::StaleRevision { .. }) => {
                    let mut conflicts = Vec::new();
                    for t in &batch.targets {
                        let now = port.get(actor, &t.asset_id).map_err(|e| port_err(e, &report))?;
                        if now.revision != t.expected_revision {
                            conflicts.push(RevisionConflict { asset_id: t.asset_id.clone(), expected: t.expected_revision, current: now.revision });
                        }
                    }
                    if policy == ConflictPolicy::RebaseOnce && attempt == 0 {
                        attempt = 1;
                        report.rebased = report.rebased.saturating_add(1);
                        continue;
                    }
                    return Err(LibraryWriteError::Conflict { conflicts, applied: report.receipts });
                }
                Err(e) => return Err(LibraryWriteError::Port { error: e, applied: report.receipts }),
            }
        }
    }
    Ok(report)
}

/// Adds (`add`) or removes the canonical `selection` to/from manual collection `collection_id`,
/// in `max_members` chunks (`<prefix>.<chunk>` keys). Accepted chunks are returned on failure.
pub fn set_collection_membership<P: CkcCatalogPort + ?Sized>(
    port: &P,
    actor: &CkcActor,
    collection_id: &CollectionId,
    selection: &Selection,
    add: bool,
    key_prefix: &str,
    l: &CkcLimits,
) -> Result<Vec<CkcReceipt>, LibraryWriteError> {
    check_key(&format!("{key_prefix}.{}", usize::MAX), l).map_err(LibraryWriteError::Invalid)?;
    let ids = resolve_selection(port, actor, selection, l).map_err(LibraryWriteError::Invalid)?;
    let mut applied = Vec::new();
    for (i, chunk) in ids.chunks(l.max_members.max(1)).enumerate() {
        let (idempotency_key, collection_id, asset_ids) = (format!("{key_prefix}.{i}"), collection_id.clone(), chunk.to_vec());
        let cmd = if add {
            CollectionCommand::AddMembers { idempotency_key, collection_id, asset_ids }
        } else {
            CollectionCommand::RemoveMembers { idempotency_key, collection_id, asset_ids }
        };
        match port.collection(actor, &cmd) {
            Ok(r) => applied.push(r),
            Err(error) => return Err(LibraryWriteError::Port { error, applied }),
        }
    }
    Ok(applied)
}
