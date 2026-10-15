#![deny(clippy::arithmetic_side_effects)]
//! TEST-ONLY in-memory implementation of [`CkcCatalogPort`]. Compiled only under `cfg(test)` or
//! the non-default `ckc-test-double` feature (V4-04 pattern), never in a default build, so it can
//! never ship as a second catalog store (STU-ASSET-004/017). It exists so import mappings (the
//! lrcat library proposal) can be proven end to end against the port contract. Its search
//! semantics (case-insensitive text matching, unrated = 0 stars) are the double's own and are NOT
//! a statement about CKC behaviour.
use crate::ckc::{
    AssetId, AssetMetadata, CatalogField, CatalogOp, CatalogRule, CatalogValue, CkcActor, CkcCatalogPort,
    CkcError, CkcLimits, CkcReceipt, CollectionCommand, CollectionId, CollectionKind, Criterion, Flag,
    KeywordCommand, KeywordPath, MetadataChange, MetadataMutation, Rating, SearchPage, SearchRequest,
    XmpSidecarWrite,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

/// Facts the double needs to answer searches over fields the port does not mutate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetFacts {
    pub file_name: String,
    pub file_format: String,
    pub capture_time: String,
    pub folder: String,
}

#[derive(Clone, Debug)]
struct Asset {
    meta: AssetMetadata,
    facts: AssetFacts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
}

pub struct CkcTestDouble {
    limits: CkcLimits,
    state: Mutex<State>,
}

fn bump(rev: u64) -> Result<u64, CkcError> {
    rev.checked_add(1).ok_or(CkcError::Limit("ckc_revision_overflow"))
}

impl CkcTestDouble {
    pub fn new(limits: CkcLimits) -> Self {
        Self { limits, state: Mutex::new(State::default()) }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, CkcError> {
        self.state.lock().map_err(|_| CkcError::Unavailable)
    }

    pub fn seed_asset(&self, asset: AssetId, facts: AssetFacts) -> Result<(), CkcError> {
        let meta = AssetMetadata {
            asset: asset.clone(),
            revision: 1,
            rating: Rating::Unrated,
            flag: Flag::Unflagged,
            label: None,
            keywords: Vec::new(),
        };
        self.lock()?.assets.insert(asset, Asset { meta, facts });
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

    fn authorize(state: &mut State, actor: &CkcActor) -> Result<(), CkcError> {
        if state.denied.contains(&actor.principal_id) {
            state.denials = state.denials.saturating_add(1);
            return Err(CkcError::Unauthorized);
        }
        Ok(())
    }

    fn replay(state: &State, request_id: &str) -> Option<CkcReceipt> {
        state.receipts.get(request_id).cloned()
    }

    fn record(state: &mut State, request_id: &str, target: &str, revision: u64, actor: &CkcActor) -> CkcReceipt {
        let r = CkcReceipt { request_id: request_id.to_owned(), target: target.to_owned(), revision, actor: actor.clone() };
        state.receipts.insert(request_id.to_owned(), r.clone());
        r
    }
}

fn text_of(state: &State, a: &Asset, field: CatalogField) -> Vec<String> {
    match field {
        CatalogField::Keyword => a.meta.keywords.iter().flat_map(|k| k.segments().iter().cloned()).collect(),
        CatalogField::FileName => vec![a.facts.file_name.clone()],
        CatalogField::FileFormat => vec![a.facts.file_format.clone()],
        CatalogField::Folder => vec![a.facts.folder.clone()],
        CatalogField::Collection => state
            .collections
            .iter()
            .filter(|(_, c)| matches!(c, DoubleCollection::Plain { members, .. } if members.contains(&a.meta.asset)))
            .map(|(_, c)| match c {
                DoubleCollection::Plain { name, .. } | DoubleCollection::Smart { name, .. } => name.clone(),
            })
            .collect(),
        CatalogField::AnyText => {
            let mut v = text_of(state, a, CatalogField::Keyword);
            v.push(a.facts.file_name.clone());
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
        CatalogValue::Rating(r) => CatalogValue::Int(i64::from(r.stars())),
        other => other.clone(),
    }
}

fn matches(state: &State, a: &Asset, c: &Criterion) -> bool {
    use CatalogOp::*;
    let actual = match c.field {
        CatalogField::Rating => Some(CatalogValue::Int(i64::from(a.meta.rating.stars()))),
        CatalogField::CaptureTime => Some(CatalogValue::Text(a.facts.capture_time.clone())),
        CatalogField::Flag => Some(CatalogValue::Flag(a.meta.flag)),
        CatalogField::Label => a.meta.label.map(CatalogValue::Label),
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
    if c.field == CatalogField::Label {
        // No label: only "label != X" holds.
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
        CatalogField::Rating => (i64::from(a.meta.rating.stars()), String::new()),
        CatalogField::CaptureTime => (0, a.facts.capture_time.clone()),
        CatalogField::FileName => (0, a.facts.file_name.clone()),
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
    fn get(&self, actor: &CkcActor, asset: &AssetId) -> Result<AssetMetadata, CkcError> {
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        s.assets.get(asset).map(|a| a.meta.clone()).ok_or(CkcError::NotFound)
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
            x.meta.asset.cmp(&y.meta.asset)
        });
        let total = hits.len() as u64;
        let start = usize::try_from(request.offset).unwrap_or(usize::MAX).min(hits.len());
        let end = start.saturating_add(request.limit as usize).min(hits.len());
        let assets: Vec<AssetMetadata> = hits[start..end].iter().map(|a| a.meta.clone()).collect();
        let next_offset = (end < hits.len()).then_some(end as u64);
        Ok(SearchPage { assets, total, next_offset })
    }

    fn mutate_metadata(&self, actor: &CkcActor, m: &MetadataMutation) -> Result<CkcReceipt, CkcError> {
        m.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, &m.request_id) {
            return Ok(r);
        }
        let asset = s.assets.get_mut(&m.asset).ok_or(CkcError::NotFound)?;
        if asset.meta.revision != m.expected_revision {
            return Err(CkcError::StaleRevision { current: asset.meta.revision });
        }
        let mut next = asset.meta.clone();
        for change in &m.changes {
            match change {
                MetadataChange::SetRating(r) => next.rating = *r,
                MetadataChange::SetFlag(f) => next.flag = *f,
                MetadataChange::SetLabel(l) => next.label = *l,
                MetadataChange::AddKeyword(k) => {
                    if !next.keywords.contains(k) {
                        next.keywords.push(k.clone());
                    }
                }
                MetadataChange::RemoveKeyword(k) => next.keywords.retain(|x| x != k),
            }
        }
        next.revision = bump(next.revision)?;
        let revision = next.revision;
        asset.meta = next;
        Ok(Self::record(&mut s, &m.request_id, m.asset.as_str(), revision, actor))
    }

    fn keyword(&self, actor: &CkcActor, command: &KeywordCommand) -> Result<CkcReceipt, CkcError> {
        command.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        let KeywordCommand::DefineSynonyms { request_id, keyword, synonyms } = command;
        if let Some(r) = Self::replay(&s, request_id) {
            return Ok(r);
        }
        s.synonyms.insert(keyword.clone(), synonyms.clone());
        Ok(Self::record(&mut s, request_id, keyword.leaf(), 1, actor))
    }

    fn collection(&self, actor: &CkcActor, command: &CollectionCommand) -> Result<CkcReceipt, CkcError> {
        command.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, command.request_id()) {
            return Ok(r);
        }
        let (id, revision) = match command {
            CollectionCommand::Create { id, name, parent, kind, .. } => {
                if s.collections.contains_key(id) {
                    return Err(CkcError::Conflict("ckc_collection_exists"));
                }
                parent_ok(&s, id, parent)?;
                let c = DoubleCollection::Plain { name: name.clone(), parent: parent.clone(), kind: *kind, members: BTreeSet::new(), revision: 1 };
                s.collections.insert(id.clone(), c);
                (id, 1)
            }
            CollectionCommand::CreateSmart { id, name, parent, rule, .. } => {
                if s.collections.contains_key(id) {
                    return Err(CkcError::Conflict("ckc_collection_exists"));
                }
                parent_ok(&s, id, parent)?;
                let c = DoubleCollection::Smart { name: name.clone(), parent: parent.clone(), rule: rule.clone(), revision: 1 };
                s.collections.insert(id.clone(), c);
                (id, 1)
            }
            CollectionCommand::AddMembers { collection, assets, .. } | CollectionCommand::RemoveMembers { collection, assets, .. } => {
                if assets.iter().any(|a| !s.assets.contains_key(a)) {
                    return Err(CkcError::NotFound);
                }
                let add = matches!(command, CollectionCommand::AddMembers { .. });
                match s.collections.get_mut(collection) {
                    Some(DoubleCollection::Plain { kind: CollectionKind::Manual, members, revision, .. }) => {
                        for a in assets {
                            if add {
                                members.insert(a.clone());
                            } else {
                                members.remove(a);
                            }
                        }
                        *revision = bump(*revision)?;
                        (collection, *revision)
                    }
                    Some(_) => return Err(CkcError::Conflict("ckc_members_need_manual_collection")),
                    None => return Err(CkcError::NotFound),
                }
            }
            CollectionCommand::SetRule { collection, expected_revision, rule, .. } => match s.collections.get_mut(collection) {
                Some(DoubleCollection::Smart { rule: r, revision, .. }) => {
                    if *revision != *expected_revision {
                        return Err(CkcError::StaleRevision { current: *revision });
                    }
                    *r = rule.clone();
                    *revision = bump(*revision)?;
                    (collection, *revision)
                }
                Some(_) => return Err(CkcError::Conflict("ckc_rule_needs_smart_collection")),
                None => return Err(CkcError::NotFound),
            },
            CollectionCommand::Delete { collection, expected_revision, .. } => {
                let current = match s.collections.get(collection) {
                    Some(DoubleCollection::Plain { revision, .. } | DoubleCollection::Smart { revision, .. }) => *revision,
                    None => return Err(CkcError::NotFound),
                };
                if current != *expected_revision {
                    return Err(CkcError::StaleRevision { current });
                }
                let has_children = s.collections.values().any(|c| match c {
                    DoubleCollection::Plain { parent, .. } | DoubleCollection::Smart { parent, .. } => parent.as_ref() == Some(collection),
                });
                if has_children {
                    return Err(CkcError::Conflict("ckc_set_not_empty"));
                }
                s.collections.remove(collection);
                (collection, current)
            }
        };
        let target = id.as_str().to_owned();
        Ok(Self::record(&mut s, command.request_id(), &target, revision, actor))
    }

    fn request_xmp_sidecar(&self, actor: &CkcActor, request: &XmpSidecarWrite) -> Result<CkcReceipt, CkcError> {
        request.validate(&self.limits)?;
        let mut s = self.lock()?;
        Self::authorize(&mut s, actor)?;
        if let Some(r) = Self::replay(&s, &request.request_id) {
            return Ok(r);
        }
        let current = s.assets.get(&request.asset).ok_or(CkcError::NotFound)?.meta.revision;
        if current != request.expected_revision {
            return Err(CkcError::StaleRevision { current });
        }
        s.sidecars.push(request.clone());
        Ok(Self::record(&mut s, &request.request_id, request.asset.as_str(), current, actor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ckc::{ActorKind, ColorLabel, SortKey, XmpRatingValue, SidecarPlacement};

    fn l() -> CkcLimits {
        CkcLimits::default()
    }
    fn op() -> CkcActor {
        CkcActor { kind: ActorKind::Operator, principal_id: "op-1".into() }
    }
    fn aid(s: &str) -> AssetId {
        AssetId::new(s, &l()).unwrap()
    }
    fn cid(s: &str) -> CollectionId {
        CollectionId::new(s, &l()).unwrap()
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
        assert_eq!(AssetId::new("", &l).unwrap_err().code(), "ckc_id_length");
        assert_eq!(AssetId::new(&"a".repeat(129), &l).unwrap_err().code(), "ckc_id_length");
        assert_eq!(AssetId::new("a/../b", &l).unwrap_err().code(), "ckc_id_charset");
        assert_eq!(Rating::from_stars(6).unwrap_err().code(), "ckc_rating_range");
        assert_eq!(KeywordPath::new(&["a|b"], &l).unwrap_err().code(), "ckc_keyword_segment");
        assert_eq!(KeywordPath::new(&vec!["x"; 33], &l).unwrap_err().code(), "ckc_keyword_segments");
        assert_eq!(KeywordPath::new(&["a\u{0}"], &l).unwrap_err().code(), "ckc_text_control_char");
        // Operator/field/value compatibility and range operands.
        let bad = [
            crit(CatalogField::Flag, CatalogOp::Gt, CatalogValue::Flag(Flag::Flagged)),
            crit(CatalogField::Rating, CatalogOp::Eq, CatalogValue::Int(9)),
            crit(CatalogField::Keyword, CatalogOp::Eq, CatalogValue::Int(1)),
            crit(CatalogField::Rating, CatalogOp::InRange, CatalogValue::Int(1)),
            CatalogRule::All(vec![]),
        ];
        let codes: Vec<&str> = bad.iter().map(|r| r.validate(&l).unwrap_err().code()).collect();
        assert_eq!(
            codes,
            ["ckc_rule_op_for_field", "ckc_rule_value_for_field", "ckc_rule_value_for_field", "ckc_rule_range_operand", "ckc_rule_empty_group"]
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
        let long = crit(CatalogField::FileName, CatalogOp::BeginsWith, CatalogValue::Text("x".repeat(4097)));
        assert_eq!(long.validate(&l).unwrap_err().code(), "ckc_rule_text_length");
        let page = SearchRequest { rule: None, sort: vec![], offset: 0, limit: 1001 };
        assert_eq!(page.validate(&l).unwrap_err().code(), "ckc_page");
        let sidecar = XmpSidecarWrite {
            request_id: "r".into(),
            asset: aid("a"),
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
        d.seed_asset(aid("a1"), AssetFacts::default()).unwrap();
        let m = MetadataMutation {
            request_id: "m1".into(),
            asset: aid("a1"),
            expected_revision: 1,
            changes: vec![MetadataChange::SetRating(Rating::Stars(4)), MetadataChange::AddKeyword(kw(&["Places", "Rome"]))],
        };
        let r = d.mutate_metadata(&op(), &m).unwrap();
        assert_eq!(r.revision, 2);
        assert_eq!(d.mutate_metadata(&op(), &m).unwrap(), r, "replayed, not re-applied");
        let stale = MetadataMutation { request_id: "m2".into(), ..m.clone() };
        assert_eq!(d.mutate_metadata(&op(), &stale).unwrap_err(), CkcError::StaleRevision { current: 2 });
        let model = CkcActor { kind: ActorKind::Model, principal_id: "lane-7".into() };
        let r3 = d.mutate_metadata(&model, &MetadataMutation { request_id: "m3".into(), expected_revision: 2, changes: vec![MetadataChange::SetLabel(Some(ColorLabel::Red))], ..m }).unwrap();
        assert_eq!(r3.actor.kind, ActorKind::Model, "model-lane attribution");
        d.deny("lane-7").unwrap();
        assert_eq!(d.get(&model, &aid("a1")).unwrap_err(), CkcError::Unauthorized);
        assert_eq!(d.get(&model, &aid("missing")).unwrap_err(), CkcError::Unauthorized, "denial does not reveal existence");
        assert_eq!(d.denials(), 2);
        let create = |rid: &str, id: &str, parent: Option<&str>, kind| CollectionCommand::Create {
            request_id: rid.into(),
            id: cid(id),
            name: id.into(),
            parent: parent.map(cid),
            kind,
        };
        d.collection(&op(), &create("c1", "set", None, CollectionKind::Set)).unwrap();
        d.collection(&op(), &create("c2", "manual", Some("set"), CollectionKind::Manual)).unwrap();
        assert_eq!(d.collection(&op(), &create("c3", "under", Some("manual"), CollectionKind::Manual)).unwrap_err(), CkcError::ParentNotSet);
        assert_eq!(d.collection(&op(), &create("c4", "self", Some("self"), CollectionKind::Set)).unwrap_err(), CkcError::ParentCycle);
        let del = CollectionCommand::Delete { request_id: "c5".into(), collection: cid("set"), expected_revision: 1 };
        assert_eq!(d.collection(&op(), &del).unwrap_err(), CkcError::Conflict("ckc_set_not_empty"));
        let search = SearchRequest {
            rule: Some(CatalogRule::All(vec![
                crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(4)),
                crit(CatalogField::Keyword, CatalogOp::ContainsAny, CatalogValue::Text("rome paris".into())),
            ])),
            sort: vec![SortKey { field: CatalogField::Rating, descending: true }],
            offset: 0,
            limit: 10,
        };
        let page = d.search(&op(), &search).unwrap();
        assert_eq!((page.total, page.assets[0].label), (1, Some(ColorLabel::Red)));
    }
}
