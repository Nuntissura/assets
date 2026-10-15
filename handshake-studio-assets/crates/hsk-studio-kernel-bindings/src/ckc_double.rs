#![deny(clippy::arithmetic_side_effects)]
//! TEST-ONLY in-memory implementation of [`CkcCatalogPort`]. Compiled only under `cfg(test)` or
//! the non-default `ckc-test-double` feature (V4-04 pattern), never in a default build, so it can
//! never ship as a second catalog store (STU-ASSET-004/017). It exists so import mappings (the
//! lrcat library proposal) can be proven end to end against the port contract. It follows the
//! port's revision semantics (STU-SDB-004: compare-and-set on the expected revision, a duplicate
//! idempotency key replays the prior receipt with no second mutation or event; MT-014: a batch is
//! validated against every target before any mutation and yields one receipt). Its search
//! semantics (case-insensitive text matching) are the double's own and are NOT CKC behaviour.
use crate::ckc::{
    AssetId, AssetMetadata, CatalogField, CatalogOp, CatalogRule, CatalogValue, CkcActor, CkcCatalogPort, CkcError,
    CkcLimits, CkcReceipt, CollectionCommand, CollectionId, CollectionKind, Criterion, Flag, KeywordCommand, KeywordPath,
    MetadataBatch, MetadataChange, MetadataMutation, Rating, SearchPage, SearchRequest, XmpSidecarWrite,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

/// Facts the double needs to answer searches over fields the port does not mutate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetFacts {
    pub original_filename: String,
    pub file_format: String,
    pub capture_time: String,
    pub folder: String,
}

#[derive(Clone, Debug)]
struct Asset {
    meta: AssetMetadata,
    facts: AssetFacts,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DoubleCollection {
    Plain { name: String, parent: Option<CollectionId>, kind: CollectionKind, members: BTreeSet<AssetId>, revision: u64 },
    Smart { name: String, parent: Option<CollectionId>, rule: CatalogRule, revision: u64 },
}

#[derive(Default)]
struct State {
    assets: BTreeMap<AssetId, Asset>,
    collections: BTreeMap<CollectionId, DoubleCollection>,
    synonyms: BTreeMap<KeywordPath, Vec<String>>,
    receipts: HashMap<String, CkcReceipt>,
    sidecars: Vec<XmpSidecarWrite>,
    denied: BTreeSet<String>,
    denials: u64,
    events: u64,
}

pub struct CkcTestDouble {
    limits: CkcLimits,
    state: Mutex<State>,
}

fn bump(rev: u64) -> Result<u64, CkcError> {
    rev.checked_add(1).ok_or(CkcError::Limit("ckc_revision_overflow"))
}

fn apply_changes(meta: &AssetMetadata, changes: &[MetadataChange]) -> Result<AssetMetadata, CkcError> {
    let mut next = meta.clone();
    for change in changes {
        match change {
            MetadataChange::SetRating(r) => next.rating = *r,
            MetadataChange::SetFlag(f) => next.flag = *f,
            MetadataChange::SetColorLabel(l) => next.color_label = *l,
            MetadataChange::AddKeyword(k) => {
                if !next.keywords.contains(k) {
                    next.keywords.push(k.clone());
                }
            }
            MetadataChange::RemoveKeyword(k) => next.keywords.retain(|x| x != k),
        }
    }
    next.revision = bump(next.revision)?;
    Ok(next)
}

impl CkcTestDouble {
    pub fn new(limits: CkcLimits) -> Self {
        Self { limits, state: Mutex::new(State::default()) }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, CkcError> {
        self.state.lock().map_err(|_| CkcError::Unavailable)
    }

    pub fn seed_asset(&self, asset_id: AssetId, facts: AssetFacts) -> Result<(), CkcError> {
        let meta = AssetMetadata {
            asset_id: asset_id.clone(),
            revision: 1,
            rating: Rating::UNRATED,
            flag: Flag::Unflagged,
            color_label: None,
            keywords: Vec::new(),
            collections: Vec::new(),
        };
        self.lock()?.assets.insert(asset_id, Asset { meta, facts });
        Ok(())
    }

    /// Simulates a ResourceBroker denial for a principal.
    pub fn deny(&self, principal_id: &str) -> Result<(), CkcError> {
        self.lock()?.denied.insert(principal_id.to_owned());
        Ok(())
    }

    /// Independent inspection (not part of the port).
    pub fn collection_state(&self, id: &CollectionId) -> Option<DoubleCollection> {
        self.lock().ok()?.collections.get(id).cloned()
    }
    pub fn synonyms(&self, keyword: &KeywordPath) -> Vec<String> {
        self.lock().ok().and_then(|s| s.synonyms.get(keyword).cloned()).unwrap_or_default()
    }
    pub fn sidecar_requests(&self) -> Vec<XmpSidecarWrite> {
        self.lock().map(|s| s.sidecars.clone()).unwrap_or_default()
    }
    pub fn denials(&self) -> u64 {
        self.lock().map(|s| s.denials).unwrap_or(0)
    }
    /// Number of EventLedger events the double recorded (one per accepted, non-replayed command).
    pub fn events(&self) -> u64 {
        self.lock().map(|s| s.events).unwrap_or(0)
    }

    fn authorize(state: &mut State, actor: &CkcActor) -> Result<(), CkcError> {
        if state.denied.contains(&actor.principal_id) {
            state.denials = state.denials.saturating_add(1);
            return Err(CkcError::Unauthorized);
        }
        Ok(())
    }

    fn replay(state: &State, key: &str) -> Option<CkcReceipt> {
        state.receipts.get(key).cloned()
    }

    fn record(state: &mut State, key: &str, command_id: &'static str, target: &str, revision: u64, actor: &CkcActor) -> CkcReceipt {
        state.events = state.events.saturating_add(1);
        let r = CkcReceipt {
            idempotency_key: key.to_owned(),
            command_id,
            target: target.to_owned(),
            revision,
            event_ledger_event_id: format!("EVT-{:08}", state.events),
            actor: actor.clone(),
        };
        state.receipts.insert(key.to_owned(), r.clone());
        r
    }
}

fn text_of(state: &State, a: &Asset, field: CatalogField) -> Vec<String> {
    match field {
        CatalogField::Keywords => a.meta.keywords.iter().flat_map(|k| k.segments().iter().cloned()).collect(),
        CatalogField::OriginalFilename => vec![a.facts.original_filename.clone()],
        CatalogField::FileFormat => vec![a.facts.file_format.clone()],
        CatalogField::Folder => vec![a.facts.folder.clone()],
        CatalogField::Collections => state
            .collections
            .iter()
            .filter(|(_, c)| matches!(c, DoubleCollection::Plain { members, .. } if members.contains(&a.meta.asset_id)))
            .map(|(_, c)| match c {
                DoubleCollection::Plain { name, .. } | DoubleCollection::Smart { name, .. } => name.clone(),
            })
            .collect(),
        CatalogField::AnyText => {
            let mut v = text_of(state, a, CatalogField::Keywords);
            v.push(a.facts.original_filename.clone());
            v
        }
        _ => Vec::new(),
    }
}

fn ord(a: &CatalogValue, b: &CatalogValue) -> Option<Ordering> {
    match (a, b) {
        (CatalogValue::Int(x), CatalogValue::Int(y)) => Some(x.cmp(y)),
        (CatalogValue::Text(x), CatalogValue::Text(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn as_ordinal(v: &CatalogValue) -> CatalogValue {
    match v {
        CatalogValue::Rating(r) => CatalogValue::Int(i64::from(r.value())),
        other => other.clone(),
    }
}

fn matches(state: &State, a: &Asset, c: &Criterion) -> bool {
    use CatalogOp::*;
    let actual = match c.field {
        CatalogField::Rating => Some(CatalogValue::Int(i64::from(a.meta.rating.value()))),
        CatalogField::CaptureTime => Some(CatalogValue::Text(a.facts.capture_time.clone())),
        CatalogField::Flag => Some(CatalogValue::Flag(a.meta.flag)),
        CatalogField::ColorLabel => a.meta.color_label.map(CatalogValue::ColorLabel),
        _ => None,
    };
    if let Some(actual) = actual {
        let want = as_ordinal(&c.value);
        return match c.op {
            Eq => actual == want,
            Ne => actual != want,
            Gt => ord(&actual, &want) == Some(Ordering::Greater),
            Ge => matches!(ord(&actual, &want), Some(Ordering::Greater | Ordering::Equal)),
            Lt => ord(&actual, &want) == Some(Ordering::Less),
            Le => matches!(ord(&actual, &want), Some(Ordering::Less | Ordering::Equal)),
            InRange => {
                let hi = c.value2.as_ref().map(as_ordinal);
                matches!(ord(&actual, &want), Some(Ordering::Greater | Ordering::Equal))
                    && hi.is_some_and(|h| matches!(ord(&actual, &h), Some(Ordering::Less | Ordering::Equal)))
            }
            _ => false,
        };
    }
    if c.field == CatalogField::ColorLabel {
        // No label: only "color_label != X" holds.
        return c.op == Ne;
    }
    let CatalogValue::Text(want) = &c.value else { return false };
    let want = want.to_lowercase();
    let have: Vec<String> = text_of(state, a, c.field).iter().map(|s| s.to_lowercase()).collect();
    let words: Vec<&str> = want.split_whitespace().collect();
    match c.op {
        Eq => have.contains(&want),
        Ne => have.iter().all(|h| *h != want),
        ContainsAny => words.iter().any(|w| have.iter().any(|h| h.contains(w))),
        ContainsAll | ContainsWords => words.iter().all(|w| have.iter().any(|h| h.contains(w))),
        ContainsNone => !words.iter().any(|w| have.iter().any(|h| h.contains(w))),
        BeginsWith => have.iter().any(|h| h.starts_with(&want)),
        EndsWith => have.iter().any(|h| h.ends_with(&want)),
        _ => false,
    }
}

fn eval(state: &State, a: &Asset, rule: &CatalogRule) -> bool {
    match rule {
        CatalogRule::Match(c) => matches(state, a, c),
        CatalogRule::All(v) => v.iter().all(|r| eval(state, a, r)),
        CatalogRule::Any(v) => v.iter().any(|r| eval(state, a, r)),
        CatalogRule::NoneOf(v) => !v.iter().any(|r| eval(state, a, r)),
    }
}

fn sort_value(a: &Asset, f: CatalogField) -> (i64, String) {
    match f {
        CatalogField::Rating => (i64::from(a.meta.rating.value()), String::new()),
        CatalogField::CaptureTime => (0, a.facts.capture_time.clone()),
        CatalogField::OriginalFilename => (0, a.facts.original_filename.clone()),
        _ => (0, String::new()),
    }
}

fn parent_ok(state: &State, id: &CollectionId, parent: &Option<CollectionId>) -> Result<(), CkcError> {
    let mut cur = parent.clone();
    let mut steps = 0usize;
    while let Some(p) = cur {
        if &p == id {
            return Err(CkcError::ParentCycle);
        }
        steps = steps.saturating_add(1);
        if steps > state.collections.len() {
            return Err(CkcError::ParentCycle);
        }
        match state.collections.get(&p) {
            Some(DoubleCollection::Plain { kind: CollectionKind::Set, parent, .. }) => cur = parent.clone(),
            Some(_) => return Err(CkcError::ParentNotSet),
            None => return Err(CkcError::NotFound),
        }
    }
    Ok(())
}

impl CkcCatalogPort for CkcTestDouble {
    fn get(&self, actor: &CkcActor, asset_id: &AssetId) -> Result<AssetMetadata, CkcError> {
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        let asset = s.assets.get(asset_id).ok_or(CkcError::NotFound)?;
        let mut meta = asset.meta.clone();
        meta.collections = s
            .collections
            .iter()
            .filter(|(_, c)| matches!(c, DoubleCollection::Plain { members, .. } if members.contains(asset_id)))
            .map(|(id, _)| id.clone())
            .collect();
        Ok(meta)
    }

    fn search(&self, actor: &CkcActor, request: &SearchRequest) -> Result<SearchPage, CkcError> {
        request.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        let mut hits: Vec<&Asset> = s
            .assets
            .values()
            .filter(|a| request.rule.as_ref().is_none_or(|r| eval(&s, a, r)))
            .collect();
        hits.sort_by(|x, y| {
            for k in &request.sort {
                let o = sort_value(x, k.field).cmp(&sort_value(y, k.field));
                let o = if k.descending { o.reverse() } else { o };
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.meta.asset_id.cmp(&y.meta.asset_id)
        });
        let total = hits.len() as u64;
        let start = usize::try_from(request.offset).unwrap_or(usize::MAX).min(hits.len());
        let end = start.saturating_add(request.limit as usize).min(hits.len());
        let assets: Vec<AssetMetadata> = hits[start..end].iter().map(|a| a.meta.clone()).collect();
        let next_offset = (end < hits.len()).then_some(end as u64);
        Ok(SearchPage { assets, total, next_offset })
    }

    fn update_library_metadata(&self, actor: &CkcActor, m: &MetadataMutation) -> Result<CkcReceipt, CkcError> {
        m.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, &m.idempotency_key) {
            return Ok(r);
        }
        let asset = s.assets.get_mut(&m.asset_id).ok_or(CkcError::NotFound)?;
        if asset.meta.revision != m.expected_revision {
            return Err(CkcError::StaleRevision { current: asset.meta.revision });
        }
        let next = apply_changes(&asset.meta, &m.changes)?;
        let revision = next.revision;
        asset.meta = next;
        Ok(Self::record(&mut s, &m.idempotency_key, "asset.update_library_metadata", m.asset_id.as_str(), revision, actor))
    }

    fn update_library_metadata_batch(&self, actor: &CkcActor, batch: &MetadataBatch) -> Result<CkcReceipt, CkcError> {
        batch.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, &batch.idempotency_key) {
            return Ok(r);
        }
        // All targets are validated (existence, revision, change application) before any mutation.
        let mut staged = Vec::with_capacity(batch.targets.len());
        for t in &batch.targets {
            let asset = s.assets.get(&t.asset_id).ok_or(CkcError::NotFound)?;
            if asset.meta.revision != t.expected_revision {
                return Err(CkcError::StaleRevision { current: asset.meta.revision });
            }
            staged.push((t.asset_id.clone(), apply_changes(&asset.meta, &t.changes)?));
        }
        let count = staged.len() as u64;
        for (id, next) in staged {
            if let Some(asset) = s.assets.get_mut(&id) {
                asset.meta = next;
            }
        }
        Ok(Self::record(&mut s, &batch.idempotency_key, "asset.update_library_metadata_batch", "batch", count, actor))
    }

    fn keyword(&self, actor: &CkcActor, command: &KeywordCommand) -> Result<CkcReceipt, CkcError> {
        command.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        let KeywordCommand::DefineSynonyms { idempotency_key, keyword, synonyms } = command;
        if let Some(r) = Self::replay(&s, idempotency_key) {
            return Ok(r);
        }
        s.synonyms.insert(keyword.clone(), synonyms.clone());
        Ok(Self::record(&mut s, idempotency_key, "catalog.keyword.define_synonyms", keyword.leaf(), 1, actor))
    }

    fn collection(&self, actor: &CkcActor, command: &CollectionCommand) -> Result<CkcReceipt, CkcError> {
        command.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, command.idempotency_key()) {
            return Ok(r);
        }
        let (id, revision) = match command {
            CollectionCommand::Create { collection_id, name, parent, kind, .. } => {
                if s.collections.contains_key(collection_id) {
                    return Err(CkcError::Conflict("ckc_collection_exists"));
                }
                parent_ok(&s, collection_id, parent)?;
                let c = DoubleCollection::Plain { name: name.clone(), parent: parent.clone(), kind: *kind, members: BTreeSet::new(), revision: 1 };
                s.collections.insert(collection_id.clone(), c);
                (collection_id, 1)
            }
            CollectionCommand::CreateSmart { collection_id, name, parent, rule, .. } => {
                if s.collections.contains_key(collection_id) {
                    return Err(CkcError::Conflict("ckc_collection_exists"));
                }
                parent_ok(&s, collection_id, parent)?;
                let c = DoubleCollection::Smart { name: name.clone(), parent: parent.clone(), rule: rule.clone(), revision: 1 };
                s.collections.insert(collection_id.clone(), c);
                (collection_id, 1)
            }
            CollectionCommand::AddMembers { collection_id, asset_ids, .. }
            | CollectionCommand::RemoveMembers { collection_id, asset_ids, .. } => {
                if asset_ids.iter().any(|a| !s.assets.contains_key(a)) {
                    return Err(CkcError::NotFound);
                }
                let add = matches!(command, CollectionCommand::AddMembers { .. });
                match s.collections.get_mut(collection_id) {
                    Some(DoubleCollection::Plain { kind: CollectionKind::Manual, members, revision, .. }) => {
                        for a in asset_ids {
                            if add {
                                members.insert(a.clone());
                            } else {
                                members.remove(a);
                            }
                        }
                        *revision = bump(*revision)?;
                        (collection_id, *revision)
                    }
                    Some(_) => return Err(CkcError::Conflict("ckc_members_need_manual_collection")),
                    None => return Err(CkcError::NotFound),
                }
            }
            CollectionCommand::SetRule { collection_id, expected_revision, rule, .. } => match s.collections.get_mut(collection_id) {
                Some(DoubleCollection::Smart { rule: r, revision, .. }) => {
                    if *revision != *expected_revision {
                        return Err(CkcError::StaleRevision { current: *revision });
                    }
                    *r = rule.clone();
                    *revision = bump(*revision)?;
                    (collection_id, *revision)
                }
                Some(_) => return Err(CkcError::Conflict("ckc_rule_needs_smart_collection")),
                None => return Err(CkcError::NotFound),
            },
            CollectionCommand::Delete { collection_id, expected_revision, .. } => {
                let current = match s.collections.get(collection_id) {
                    Some(DoubleCollection::Plain { revision, .. } | DoubleCollection::Smart { revision, .. }) => *revision,
                    None => return Err(CkcError::NotFound),
                };
                if current != *expected_revision {
                    return Err(CkcError::StaleRevision { current });
                }
                let has_children = s.collections.values().any(|c| match c {
                    DoubleCollection::Plain { parent, .. } | DoubleCollection::Smart { parent, .. } => parent.as_ref() == Some(collection_id),
                });
                if has_children {
                    return Err(CkcError::Conflict("ckc_set_not_empty"));
                }
                s.collections.remove(collection_id);
                (collection_id, current)
            }
        };
        let target = id.as_str().to_owned();
        Ok(Self::record(&mut s, command.idempotency_key(), command.command_id(), &target, revision, actor))
    }

    fn request_xmp_sidecar(&self, actor: &CkcActor, request: &XmpSidecarWrite) -> Result<CkcReceipt, CkcError> {
        request.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, &request.idempotency_key) {
            return Ok(r);
        }
        let current = s.assets.get(&request.asset_id).ok_or(CkcError::NotFound)?.meta.revision;
        if current != request.expected_revision {
            return Err(CkcError::StaleRevision { current });
        }
        s.sidecars.push(request.clone());
        Ok(Self::record(&mut s, &request.idempotency_key, "asset.write_xmp_sidecar", request.asset_id.as_str(), current, actor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ckc::{ActorKind, BatchTarget, ColorLabel, SidecarPlacement, SortKey, XmpRatingValue};

    fn l() -> CkcLimits {
        CkcLimits::default()
    }
    fn op() -> CkcActor {
        CkcActor { kind: ActorKind::Operator, principal_id: "op-1".into() }
    }
    fn aid(n: u8) -> AssetId {
        let mut b = [0u8; 16];
        b[15] = n;
        AssetId::from_bytes(b)
    }
    fn cid(n: u8) -> CollectionId {
        let mut b = [0u8; 16];
        b[0] = 0xc0;
        b[15] = n;
        CollectionId::from_bytes(b)
    }
    fn kw(p: &[&str]) -> KeywordPath {
        KeywordPath::new(p, &l()).unwrap()
    }
    fn crit(field: CatalogField, op: CatalogOp, value: CatalogValue) -> CatalogRule {
        CatalogRule::Match(Criterion { field, op, value, value2: None })
    }

    #[test]
    fn port_requests_are_validated_and_bounded() {
        let l = l();
        // Ids are canonical UUIDs (Spec 2.2.3.1 asset_id: UUID, 2.2.3.2 collections: UUID[]).
        assert_eq!(aid(7).as_str(), "00000000-0000-0000-0000-000000000007");
        for bad in ["", "a1", "00000000-0000-0000-0000-00000000000G", "00000000-0000-0000-0000-00000000000A", "00000000_0000-0000-0000-000000000000"] {
            assert_eq!(AssetId::new(bad).unwrap_err().code(), "ckc_uuid", "{bad}");
        }
        // Wire names of the PhotoAsset library metadata (Spec 2.2.3.2).
        assert_eq!([Flag::Pick, Flag::Unflagged, Flag::Reject].map(Flag::wire), ["pick", "none", "reject"]);
        assert_eq!(Flag::from_wire("none"), Some(Flag::Unflagged));
        assert_eq!(ColorLabel::from_wire("purple"), Some(ColorLabel::Purple));
        assert_eq!(ColorLabel::from_wire("Red"), None, "wire names are lowercase");
        assert_eq!(Rating::new(6).unwrap_err().code(), "ckc_rating_range");
        assert_eq!(CatalogField::ColorLabel.ckc_field(), Some("PhotoAsset.color_label"));
        assert_eq!(CatalogField::Folder.ckc_field(), None, "proposed CKC addition");
        assert_eq!(KeywordPath::new(&["a|b"], &l).unwrap_err().code(), "ckc_keyword_segment");
        assert_eq!(KeywordPath::new(&vec!["x"; 33], &l).unwrap_err().code(), "ckc_keyword_segments");
        assert_eq!(KeywordPath::new(&["a\u{0}"], &l).unwrap_err().code(), "ckc_text_control_char");
        // Operator/field/value compatibility and range operands.
        let bad = [
            crit(CatalogField::Flag, CatalogOp::Gt, CatalogValue::Flag(Flag::Pick)),
            crit(CatalogField::Rating, CatalogOp::Eq, CatalogValue::Int(9)),
            crit(CatalogField::Keywords, CatalogOp::Eq, CatalogValue::Int(1)),
            crit(CatalogField::Rating, CatalogOp::InRange, CatalogValue::Int(1)),
            CatalogRule::All(vec![]),
            crit(CatalogField::Aperture, CatalogOp::Ge, CatalogValue::Real(f64::NAN)),
        ];
        let codes: Vec<&str> = bad.iter().map(|r| r.validate(&l).unwrap_err().code()).collect();
        assert_eq!(
            codes,
            [
                "ckc_rule_op_for_field",
                "ckc_rule_value_for_field",
                "ckc_rule_value_for_field",
                "ckc_rule_range_operand",
                "ckc_rule_empty_group",
                "ckc_rule_value_for_field"
            ]
        );
        // Rule bombs: depth and node count refused by an explicit-stack walk (no recursion).
        let mut deep = crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(1));
        for _ in 0..100_000 {
            deep = CatalogRule::All(vec![deep]);
        }
        assert_eq!(deep.validate(&l).unwrap_err().code(), "ckc_rule_depth");
        std::mem::forget(deep); // dropping a 100k-deep tree would recurse in Drop; the test leaks it
        let wide = CatalogRule::Any(vec![crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(1)); 5000]);
        assert_eq!(wide.validate(&l).unwrap_err().code(), "ckc_rule_nodes");
        let long = crit(CatalogField::OriginalFilename, CatalogOp::BeginsWith, CatalogValue::Text("x".repeat(4097)));
        assert_eq!(long.validate(&l).unwrap_err().code(), "ckc_rule_text_length");
        let page = SearchRequest { rule: None, sort: vec![], offset: 0, limit: 1001 };
        assert_eq!(page.validate(&l).unwrap_err().code(), "ckc_page");
        let target = |n| BatchTarget { asset_id: aid(n), expected_revision: 1, changes: vec![MetadataChange::SetRating(Rating::UNRATED)] };
        let repeated = MetadataBatch { idempotency_key: "b".into(), targets: vec![target(1), target(1)] };
        assert_eq!(repeated.validate(&l).unwrap_err().code(), "ckc_batch_repeated_asset");
        let huge = MetadataBatch { idempotency_key: "b".into(), targets: (0..=255u8).cycle().take(1001).map(target).collect() };
        assert_eq!(huge.validate(&l).unwrap_err().code(), "ckc_batch");
        let sidecar = XmpSidecarWrite {
            idempotency_key: "r".into(),
            asset_id: aid(1),
            expected_revision: 1,
            rating: Some(XmpRatingValue::Stars(7)),
            label: None,
            keywords: vec![],
            hierarchical_keywords: vec![],
            placement: SidecarPlacement::BesideOriginal,
        };
        assert_eq!(sidecar.validate(&l).unwrap_err().code(), "ckc_rating_range");
    }

    #[test]
    fn double_enforces_revision_idempotency_authorization_and_parents() {
        let d = CkcTestDouble::new(l());
        for n in 1..=3 {
            d.seed_asset(aid(n), AssetFacts::default()).unwrap();
        }
        let m = MetadataMutation {
            idempotency_key: "m1".into(),
            asset_id: aid(1),
            expected_revision: 1,
            changes: vec![MetadataChange::SetRating(Rating::new(4).unwrap()), MetadataChange::AddKeyword(kw(&["Places", "Rome"]))],
        };
        let r = d.update_library_metadata(&op(), &m).unwrap();
        assert_eq!((r.revision, r.command_id, r.event_ledger_event_id.as_str()), (2, "asset.update_library_metadata", "EVT-00000001"));
        assert_eq!(d.update_library_metadata(&op(), &m).unwrap(), r, "replayed, not re-applied");
        assert_eq!(d.events(), 1, "a replay records no second event (STU-SDB-004)");
        let stale = MetadataMutation { idempotency_key: "m2".into(), ..m.clone() };
        assert_eq!(d.update_library_metadata(&op(), &stale).unwrap_err(), CkcError::StaleRevision { current: 2 });
        let model = CkcActor { kind: ActorKind::Model, principal_id: "lane-7".into() };
        let r3 = d
            .update_library_metadata(&model, &MetadataMutation { idempotency_key: "m3".into(), expected_revision: 2, changes: vec![MetadataChange::SetColorLabel(Some(ColorLabel::Red))], ..m })
            .unwrap();
        assert_eq!(r3.actor.kind, ActorKind::Model, "model-lane attribution");
        // Batch: one stale target refuses the whole batch with no mutation; a valid batch is one receipt.
        let t = |n, rev| BatchTarget { asset_id: aid(n), expected_revision: rev, changes: vec![MetadataChange::SetFlag(Flag::Reject)] };
        let bad = MetadataBatch { idempotency_key: "b1".into(), targets: vec![t(2, 1), t(1, 1)] };
        assert_eq!(d.update_library_metadata_batch(&op(), &bad).unwrap_err(), CkcError::StaleRevision { current: 3 });
        assert_eq!(d.get(&op(), &aid(2)).unwrap().flag, Flag::Unflagged, "nothing applied");
        let good = MetadataBatch { idempotency_key: "b2".into(), targets: vec![t(2, 1), t(3, 1)] };
        let rb = d.update_library_metadata_batch(&op(), &good).unwrap();
        assert_eq!((rb.revision, d.get(&op(), &aid(3)).unwrap().flag), (2, Flag::Reject));
        d.deny("lane-7").unwrap();
        assert_eq!(d.get(&model, &aid(1)).unwrap_err(), CkcError::Unauthorized);
        assert_eq!(d.get(&model, &aid(99)).unwrap_err(), CkcError::Unauthorized, "denial does not reveal existence");
        assert_eq!(d.denials(), 2);
        let create = |key: &str, id: CollectionId, parent: Option<CollectionId>, kind| CollectionCommand::Create {
            idempotency_key: key.into(),
            collection_id: id.clone(),
            name: id.as_str().into(),
            parent,
            kind,
        };
        d.collection(&op(), &create("c1", cid(1), None, CollectionKind::Set)).unwrap();
        d.collection(&op(), &create("c2", cid(2), Some(cid(1)), CollectionKind::Manual)).unwrap();
        assert_eq!(d.collection(&op(), &create("c3", cid(3), Some(cid(2)), CollectionKind::Manual)).unwrap_err(), CkcError::ParentNotSet);
        assert_eq!(d.collection(&op(), &create("c4", cid(4), Some(cid(4)), CollectionKind::Set)).unwrap_err(), CkcError::ParentCycle);
        let add = CollectionCommand::AddMembers { idempotency_key: "c6".into(), collection_id: cid(2), asset_ids: vec![aid(1)] };
        d.collection(&op(), &add).unwrap();
        assert_eq!(d.get(&op(), &aid(1)).unwrap().collections, [cid(2)], "PhotoAsset.collections projection");
        let del = CollectionCommand::Delete { idempotency_key: "c5".into(), collection_id: cid(1), expected_revision: 1 };
        assert_eq!(d.collection(&op(), &del).unwrap_err(), CkcError::Conflict("ckc_set_not_empty"));
        let search = SearchRequest {
            rule: Some(CatalogRule::All(vec![
                crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(4)),
                crit(CatalogField::Keywords, CatalogOp::ContainsAny, CatalogValue::Text("rome paris".into())),
            ])),
            sort: vec![SortKey { field: CatalogField::Rating, descending: true }],
            offset: 0,
            limit: 10,
        };
        let page = d.search(&op(), &search).unwrap();
        assert_eq!((page.total, page.assets[0].color_label), (1, Some(ColorLabel::Red)));
    }
}
