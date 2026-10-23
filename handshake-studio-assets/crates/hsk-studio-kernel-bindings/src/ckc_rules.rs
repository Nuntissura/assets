#![deny(clippy::arithmetic_side_effects)]
//! Smart-collection rule preview (lane SMB-LR-LIBRARY-WRITE): pure, stateless evaluation of a
//! [`CatalogRule`] over records the CKC port returned (`asset.get` / `asset.search` pages and
//! `catalog.collection.list`). It answers "would this record match this rule" for a rule being
//! edited before `SetRule` is sent, and checks a rule for reference cycles. It is NOT a second
//! smart-collection mechanism (STU-ASSET-004): it stores no membership, indexes nothing, and CKC
//! remains the authority that executes `asset.search` and owns smart collections
//! (STU-ASSET-010/019). Lightroom Classic matching semantics are UNVERIFIED (D5): the semantics
//! below are Studio's own, documented per operator.
//!
//! Three-valued (Kleene) logic: a field the record does not carry (a technical fact the port did
//! not return) is `Unknown`, never silently false. `All` is `NoMatch` if any child is, else
//! `Unknown` if any child is, else `Match`; `Any` mirrors it; `NoneOf` is the negation of `Any`.
//!
//! Collection criteria (field `Collections`, text value): `Eq`/`Ne` with a canonical UUID test
//! membership in that collection (manual: `AssetMetadata.collections`; a set contains what its
//! children contain; a smart collection's membership is its rule, evaluated recursively). Every
//! other text operator matches the names of the manual collections the record is in and of the
//! sets above them; smart collections are never matched by name, so a name criterion cannot form
//! a cycle (a smart collection that names itself would otherwise always be one).
//! Reference chains are bounded by `max_reference_depth`, a cycle is refused as
//! `ckc_smart_collection_cycle`, and all criterion evaluations of one record share the
//! `max_eval_steps` budget.
use crate::ckc::{
    AssetMetadata, CatalogField, CatalogOp, CatalogRule, CatalogValue, CkcError, CkcLimits, CollectionId,
    CollectionSummary, Criterion, ListedKind,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// Technical facts a record may carry besides its library metadata (Spec 2.2.3.1/2.2.3.2 field
/// names; `None` = the port did not return the field, so criteria on it are `Unknown`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordFacts {
    pub original_filename: Option<String>,
    /// ISO 8601 text; ordered comparison is lexicographic, so callers pass one canonical form.
    pub capture_time: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub iso: Option<i64>,
    pub file_format: Option<String>,
    pub folder: Option<String>,
    pub has_adjustments: Option<bool>,
}

/// A port-provided record: CKC library metadata plus the facts the port returned.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryRecord {
    pub metadata: AssetMetadata,
    pub facts: RecordFacts,
}

impl LibraryRecord {
    pub fn from_metadata(metadata: AssetMetadata) -> Self {
        Self { metadata, facts: RecordFacts::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Match,
    NoMatch,
    Unknown,
}

impl Verdict {
    fn not(self) -> Self {
        match self {
            Self::Match => Self::NoMatch,
            Self::NoMatch => Self::Match,
            Self::Unknown => Self::Unknown,
        }
    }
    fn from_bool(b: bool) -> Self {
        if b { Self::Match } else { Self::NoMatch }
    }
}

/// The collections a rule may reference, as `catalog.collection.list` returned them; validated
/// once (count, duplicate ids, every smart rule).
#[derive(Clone, Debug)]
pub struct RuleContext {
    limits: CkcLimits,
    collections: BTreeMap<CollectionId, CollectionSummary>,
    children: BTreeMap<CollectionId, Vec<CollectionId>>,
}

fn is_uuid(s: &str) -> bool {
    CollectionId::new(s).is_ok()
}

impl RuleContext {
    pub fn new(list: Vec<CollectionSummary>, limits: &CkcLimits) -> Result<Self, CkcError> {
        if list.len() > limits.max_collections {
            return Err(CkcError::Limit("ckc_collections"));
        }
        let mut collections = BTreeMap::new();
        let mut children: BTreeMap<CollectionId, Vec<CollectionId>> = BTreeMap::new();
        for c in list {
            if let ListedKind::Smart(rule) = &c.kind {
                rule.validate(limits)?;
            }
            if let Some(p) = &c.parent {
                children.entry(p.clone()).or_default().push(c.collection_id.clone());
            }
            let id = c.collection_id.clone();
            if collections.insert(id, c).is_some() {
                return Err(CkcError::Invalid("ckc_collection_repeated"));
            }
        }
        Ok(Self { limits: *limits, collections, children })
    }

    pub fn get(&self, id: &CollectionId) -> Option<&CollectionSummary> {
        self.collections.get(id)
    }

    /// Whether `rule`, installed as the rule of smart collection `owner`, would create a reference
    /// cycle through UUID references, directly or through sets (static check before `SetRule` /
    /// `CreateSmart`).
    pub fn check_acyclic(&self, owner: &CollectionId, rule: &CatalogRule) -> Result<(), CkcError> {
        rule.validate(&self.limits)?;
        // Depth-first over the reference graph with an explicit stack; `owner` uses `rule`.
        let mut stack: Vec<(CollectionId, usize)> = Vec::new();
        let mut done: BTreeSet<CollectionId> = BTreeSet::new();
        for r in self.references(rule) {
            if &r == owner {
                return Err(CkcError::Invalid("ckc_smart_collection_cycle"));
            }
            stack.push((r, 1));
        }
        let mut steps = 0usize;
        while let Some((id, depth)) = stack.pop() {
            steps = steps.saturating_add(1);
            if steps > self.limits.max_eval_steps {
                return Err(CkcError::Limit("ckc_rule_eval_budget"));
            }
            if depth > self.limits.max_reference_depth {
                return Err(CkcError::Limit("ckc_rule_reference_depth"));
            }
            if !done.insert(id.clone()) {
                continue;
            }
            let next: Vec<CollectionId> = match self.collections.get(&id).map(|c| &c.kind) {
                Some(ListedKind::Smart(r)) => self.references(r),
                Some(ListedKind::Set) => self.children.get(&id).cloned().unwrap_or_default(),
                _ => Vec::new(),
            };
            for n in next {
                if &n == owner {
                    return Err(CkcError::Invalid("ckc_smart_collection_cycle"));
                }
                stack.push((n, depth.saturating_add(1)));
            }
        }
        Ok(())
    }

    /// Collections a rule references by UUID (one step).
    fn references(&self, rule: &CatalogRule) -> Vec<CollectionId> {
        let mut out = Vec::new();
        let mut stack = vec![rule];
        while let Some(r) = stack.pop() {
            match r {
                CatalogRule::Match(c) if c.field == CatalogField::Collections => {
                    let CatalogValue::Text(t) = &c.value else { continue };
                    if matches!(c.op, CatalogOp::Eq | CatalogOp::Ne)
                        && let Ok(id) = CollectionId::new(t)
                    {
                        out.push(id);
                    }
                    // Name criteria only match manual collections and sets: no reference.
                }
                CatalogRule::Match(_) => {}
                CatalogRule::All(v) | CatalogRule::Any(v) | CatalogRule::NoneOf(v) => stack.extend(v.iter()),
            }
        }
        out
    }

    /// Evaluates `rule` for one record (validated first).
    pub fn evaluate(&self, rule: &CatalogRule, record: &LibraryRecord) -> Result<Verdict, CkcError> {
        rule.validate(&self.limits)?;
        let mut run = Run { ctx: self, record, steps: 0, memo: BTreeMap::new(), active: BTreeSet::new() };
        run.rule(rule, 0)
    }

    /// Membership of `record` in collection `id` (manual, set or smart).
    pub fn member_of(&self, id: &CollectionId, record: &LibraryRecord) -> Result<Verdict, CkcError> {
        let mut run = Run { ctx: self, record, steps: 0, memo: BTreeMap::new(), active: BTreeSet::new() };
        run.membership(id, 0)
    }

    /// The records of `page` matching `rule` (preview of a smart collection over one page the
    /// port returned); `Unknown` records are returned separately, never folded into either side.
    pub fn preview<'r>(
        &self,
        rule: &CatalogRule,
        page: &'r [LibraryRecord],
    ) -> Result<(Vec<&'r LibraryRecord>, Vec<&'r LibraryRecord>), CkcError> {
        let (mut hits, mut unknown) = (Vec::new(), Vec::new());
        for r in page {
            match self.evaluate(rule, r)? {
                Verdict::Match => hits.push(r),
                Verdict::Unknown => unknown.push(r),
                Verdict::NoMatch => {}
            }
        }
        Ok((hits, unknown))
    }
}

struct Run<'a> {
    ctx: &'a RuleContext,
    record: &'a LibraryRecord,
    steps: usize,
    memo: BTreeMap<CollectionId, Verdict>,
    active: BTreeSet<CollectionId>,
}

impl Run<'_> {
    fn spend(&mut self) -> Result<(), CkcError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.ctx.limits.max_eval_steps {
            return Err(CkcError::Limit("ckc_rule_eval_budget"));
        }
        Ok(())
    }

    /// `depth` counts collection references followed; rule nesting is bounded by validation.
    fn rule(&mut self, rule: &CatalogRule, depth: usize) -> Result<Verdict, CkcError> {
        self.spend()?;
        Ok(match rule {
            CatalogRule::Match(c) => self.criterion(c, depth)?,
            CatalogRule::All(v) => {
                let mut acc = Verdict::Match;
                for r in v {
                    match self.rule(r, depth)? {
                        Verdict::NoMatch => return Ok(Verdict::NoMatch),
                        Verdict::Unknown => acc = Verdict::Unknown,
                        Verdict::Match => {}
                    }
                }
                acc
            }
            CatalogRule::Any(v) => self.any(v, depth)?,
            CatalogRule::NoneOf(v) => self.any(v, depth)?.not(),
        })
    }

    fn any(&mut self, v: &[CatalogRule], depth: usize) -> Result<Verdict, CkcError> {
        let mut acc = Verdict::NoMatch;
        for r in v {
            match self.rule(r, depth)? {
                Verdict::Match => return Ok(Verdict::Match),
                Verdict::Unknown => acc = Verdict::Unknown,
                Verdict::NoMatch => {}
            }
        }
        Ok(acc)
    }

    fn membership(&mut self, id: &CollectionId, depth: usize) -> Result<Verdict, CkcError> {
        self.spend()?;
        if let Some(v) = self.memo.get(id) {
            return Ok(*v);
        }
        if depth > self.ctx.limits.max_reference_depth {
            return Err(CkcError::Limit("ckc_rule_reference_depth"));
        }
        let Some(c) = self.ctx.collections.get(id) else {
            // Not listed: only the record's own manual membership can answer.
            return Ok(Verdict::from_bool(self.record.metadata.collections.contains(id)));
        };
        if !self.active.insert(id.clone()) {
            return Err(CkcError::Invalid("ckc_smart_collection_cycle"));
        }
        let v = match &c.kind {
            ListedKind::Manual => Verdict::from_bool(self.record.metadata.collections.contains(id)),
            ListedKind::Smart(rule) => self.rule(rule, depth.saturating_add(1))?,
            ListedKind::Set => {
                let kids = self.ctx.children.get(id).cloned().unwrap_or_default();
                let mut acc = Verdict::NoMatch;
                for k in &kids {
                    match self.membership(k, depth.saturating_add(1))? {
                        Verdict::Match => {
                            acc = Verdict::Match;
                            break;
                        }
                        Verdict::Unknown => acc = Verdict::Unknown,
                        Verdict::NoMatch => {}
                    }
                }
                acc
            }
        };
        self.active.remove(id);
        self.memo.insert(id.clone(), v);
        Ok(v)
    }

    fn criterion(&mut self, c: &Criterion, depth: usize) -> Result<Verdict, CkcError> {
        use CatalogField as F;
        let m = &self.record.metadata;
        let f = &self.record.facts;
        let num = |x: Option<f64>| x.map(CatalogValue::Real);
        Ok(match c.field {
            F::Rating => ordinal(Some(CatalogValue::Int(i64::from(m.rating.value()))), c),
            F::Iso => ordinal(f.iso.map(CatalogValue::Int), c),
            F::FocalLength => ordinal(num(f.focal_length), c),
            F::Aperture => ordinal(num(f.aperture), c),
            F::CaptureTime => ordinal(f.capture_time.clone().map(CatalogValue::Text), c),
            F::Flag => exact(Some(CatalogValue::Flag(m.flag)), c),
            // The label is library metadata CKC returned: absence is known, not unknown.
            F::ColorLabel => match m.color_label {
                Some(l) => exact(Some(CatalogValue::ColorLabel(l)), c),
                None => Verdict::from_bool(c.op == CatalogOp::Ne),
            },
            F::HasAdjustments => exact(f.has_adjustments.map(CatalogValue::Bool), c),
            F::Keywords => {
                let vals: Vec<String> = m.keywords.iter().flat_map(|k| k.segments().iter().cloned()).collect();
                text(Some(vals), c)
            }
            F::OriginalFilename => text(f.original_filename.clone().map(|s| vec![s]), c),
            F::CameraMake => text(f.camera_make.clone().map(|s| vec![s]), c),
            F::CameraModel => text(f.camera_model.clone().map(|s| vec![s]), c),
            F::LensModel => text(f.lens_model.clone().map(|s| vec![s]), c),
            F::FileFormat => text(f.file_format.clone().map(|s| vec![s]), c),
            F::Folder => text(f.folder.clone().map(|s| vec![s]), c),
            F::AnyText => {
                let mut vals: Vec<String> = m.keywords.iter().flat_map(|k| k.segments().iter().cloned()).collect();
                let known = [&f.original_filename, &f.camera_make, &f.camera_model, &f.lens_model, &f.folder];
                vals.extend(known.iter().filter_map(|x| (*x).clone()));
                // Missing facts may hold the text: a negative answer is only Unknown then.
                let complete = known.iter().all(|x| x.is_some());
                match text(Some(vals), c) {
                    Verdict::NoMatch if !complete => Verdict::Unknown,
                    v => v,
                }
            }
            F::Collections => self.collections(c, depth)?,
        })
    }

    fn collections(&mut self, c: &Criterion, depth: usize) -> Result<Verdict, CkcError> {
        let CatalogValue::Text(t) = &c.value else { return Ok(Verdict::Unknown) };
        if matches!(c.op, CatalogOp::Eq | CatalogOp::Ne) && is_uuid(t) {
            let id = CollectionId::new(t)?;
            let v = self.membership(&id, depth)?;
            return Ok(if c.op == CatalogOp::Ne { v.not() } else { v });
        }
        // Name matching: manual collections the record is in plus every set above them.
        let mut names = Vec::new();
        let mut seen: BTreeSet<&CollectionId> = BTreeSet::new();
        for id in &self.record.metadata.collections {
            let mut cur = Some(id);
            let mut hops = 0usize;
            while let Some(cid) = cur {
                self.spend()?;
                hops = hops.saturating_add(1);
                if hops > self.ctx.limits.max_reference_depth || !seen.insert(cid) {
                    break;
                }
                let Some(listed) = self.ctx.collections.get(cid) else { break };
                if matches!(listed.kind, ListedKind::Smart(_)) {
                    break;
                }
                names.push(listed.name.clone());
                cur = listed.parent.as_ref();
            }
        }
        Ok(text(Some(names), c))
    }
}

fn real(v: &CatalogValue) -> Option<f64> {
    match v {
        CatalogValue::Int(i) => Some(*i as f64),
        CatalogValue::Real(r) => Some(*r),
        CatalogValue::Rating(r) => Some(f64::from(r.value())),
        _ => None,
    }
}

fn cmp(a: &CatalogValue, b: &CatalogValue) -> Option<Ordering> {
    match (a, b) {
        (CatalogValue::Text(x), CatalogValue::Text(y)) => Some(x.cmp(y)),
        _ => real(a)?.partial_cmp(&real(b)?),
    }
}

fn ordinal(actual: Option<CatalogValue>, c: &Criterion) -> Verdict {
    use CatalogOp::*;
    let Some(a) = actual else { return Verdict::Unknown };
    let Some(o) = cmp(&a, &c.value) else { return Verdict::Unknown };
    Verdict::from_bool(match c.op {
        Eq => o == Ordering::Equal,
        Ne => o != Ordering::Equal,
        Gt => o == Ordering::Greater,
        Ge => o != Ordering::Less,
        Lt => o == Ordering::Less,
        Le => o != Ordering::Greater,
        InRange => {
            let Some(hi) = c.value2.as_ref().and_then(|h| cmp(&a, h)) else { return Verdict::Unknown };
            o != Ordering::Less && hi != Ordering::Greater
        }
        _ => return Verdict::Unknown,
    })
}

fn exact(actual: Option<CatalogValue>, c: &Criterion) -> Verdict {
    let Some(a) = actual else { return Verdict::Unknown };
    match c.op {
        CatalogOp::Eq => Verdict::from_bool(a == c.value),
        CatalogOp::Ne => Verdict::from_bool(a != c.value),
        _ => Verdict::Unknown,
    }
}

/// Case-insensitive text semantics (Studio's, UNVERIFIED against Lightroom): `Eq`/`Ne` compare
/// whole values; `ContainsAny`/`ContainsAll`/`ContainsNone` test each whitespace-separated word as
/// a substring of any value; `ContainsWords` needs every word to equal a whole word of a value;
/// `BeginsWith`/`EndsWith` test any value.
fn text(values: Option<Vec<String>>, c: &Criterion) -> Verdict {
    use CatalogOp::*;
    let Some(values) = values else { return Verdict::Unknown };
    let CatalogValue::Text(want) = &c.value else { return Verdict::Unknown };
    let want = want.to_lowercase();
    let have: Vec<String> = values.iter().map(|s| s.to_lowercase()).collect();
    let words: Vec<&str> = want.split_whitespace().collect();
    let sub = |w: &str| have.iter().any(|h| h.contains(w));
    Verdict::from_bool(match c.op {
        Eq => have.contains(&want),
        Ne => !have.contains(&want),
        ContainsAny => words.iter().any(|w| sub(w)),
        ContainsAll => !words.is_empty() && words.iter().all(|w| sub(w)),
        ContainsNone => !words.iter().any(|w| sub(w)),
        ContainsWords => {
            !words.is_empty() && words.iter().all(|w| have.iter().any(|h| h.split_whitespace().any(|t| t == *w)))
        }
        BeginsWith => have.iter().any(|h| h.starts_with(&want)),
        EndsWith => have.iter().any(|h| h.ends_with(&want)),
        _ => return Verdict::Unknown,
    })
}
