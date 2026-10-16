//! Lane SMB-LR-LIBRARY-WRITE slice 1: library edits, smart-rule preview and hostile input, proven
//! against the test-only CKC double. Lightroom Classic parity is UNVERIFIED (D5): expectations are
//! the documented Studio semantics, computed by hand.
use crate::ckc::*;
use crate::ckc_double::{AssetFacts, CkcTestDouble, DoubleCollection};
use crate::ckc_library::*;
use crate::ckc_rules::*;
use std::cell::Cell;

fn op() -> CkcActor {
    CkcActor { kind: ActorKind::Operator, principal_id: "op-1".into() }
}
fn aid(n: u16) -> AssetId {
    let mut b = [0u8; 16];
    b[14..].copy_from_slice(&n.to_be_bytes());
    AssetId::from_bytes(b)
}
fn cid(n: u8) -> CollectionId {
    let mut b = [0u8; 16];
    b[0] = 0xc0;
    b[15] = n;
    CollectionId::from_bytes(b)
}
fn kw(p: &[&str]) -> KeywordPath {
    KeywordPath::new(p, &CkcLimits::default()).unwrap()
}
fn crit(field: CatalogField, op: CatalogOp, value: CatalogValue) -> CatalogRule {
    CatalogRule::Match(Criterion { field, op, value, value2: None })
}
fn double(n: u16, limits: CkcLimits) -> CkcTestDouble {
    let d = CkcTestDouble::new(limits);
    for i in 1..=n {
        d.seed_asset(aid(i), AssetFacts::default()).unwrap();
    }
    d
}

/// Wraps the double and moves one asset's revision just before the `nth` batch reaches it
/// (another actor editing concurrently).
struct Racing<'a> {
    inner: &'a CkcTestDouble,
    nth: usize,
    victim: AssetId,
    seen: Cell<usize>,
}

impl CkcCatalogPort for Racing<'_> {
    fn get(&self, a: &CkcActor, id: &AssetId) -> Result<AssetMetadata, CkcError> {
        self.inner.get(a, id)
    }
    fn search(&self, a: &CkcActor, r: &SearchRequest) -> Result<SearchPage, CkcError> {
        self.inner.search(a, r)
    }
    fn update_library_metadata(&self, a: &CkcActor, m: &MetadataMutation) -> Result<CkcReceipt, CkcError> {
        self.inner.update_library_metadata(a, m)
    }
    fn update_library_metadata_batch(&self, a: &CkcActor, b: &MetadataBatch) -> Result<CkcReceipt, CkcError> {
        let n = self.seen.get() + 1;
        self.seen.set(n);
        if n == self.nth {
            let cur = self.inner.get(a, &self.victim)?.revision;
            let other = CkcActor { kind: ActorKind::Model, principal_id: "lane-9".into() };
            let m = MetadataMutation {
                idempotency_key: format!("race-{n}-{cur}"),
                asset_id: self.victim.clone(),
                expected_revision: cur,
                changes: vec![MetadataChange::SetRating(Rating::new(1).unwrap())],
            };
            self.inner.update_library_metadata(&other, &m)?;
        }
        self.inner.update_library_metadata_batch(a, b)
    }
    fn keyword(&self, a: &CkcActor, c: &KeywordCommand) -> Result<CkcReceipt, CkcError> {
        self.inner.keyword(a, c)
    }
    fn collection(&self, a: &CkcActor, c: &CollectionCommand) -> Result<CkcReceipt, CkcError> {
        self.inner.collection(a, c)
    }
    fn request_xmp_sidecar(&self, a: &CkcActor, r: &XmpSidecarWrite) -> Result<CkcReceipt, CkcError> {
        self.inner.request_xmp_sidecar(a, r)
    }
}

#[test]
fn library_edits_resolve_against_fresh_reads_with_receipts_and_conflicts() {
    let l = CkcLimits { max_batch: 2, ..CkcLimits::default() };
    let d = double(5, l);
    let all = Selection::Assets((1..=5).map(aid).collect());
    // Step rating (clamped), toggle pick, toggle label, set keywords; one batch receipt per 2 assets.
    let edits = [
        LibraryEdit::StepRating(4),
        LibraryEdit::ToggleFlag(Flag::Pick),
        LibraryEdit::ToggleLabel(ColorLabel::Red),
        LibraryEdit::SetKeywords(vec![kw(&["Places", "Rome"]), kw(&["Trip"])]),
    ];
    let r = apply_library_edit(&d, &op(), &all, &edits, "e1", ConflictPolicy::Fail, &l).unwrap();
    assert_eq!((r.receipts.len(), r.changed.len(), r.unchanged.len(), d.events()), (3, 5, 0, 3));
    assert!(r.receipts.iter().all(|x| x.command_id == "asset.update_library_metadata_batch"));
    let m = d.get(&op(), &aid(3)).unwrap();
    assert_eq!((m.rating.value(), m.flag, m.color_label, m.revision), (4, Flag::Pick, Some(ColorLabel::Red), 2));
    assert_eq!(m.keywords, [kw(&["Places", "Rome"]), kw(&["Trip"])]);
    // Same edit again: rating 4 -> 5 (clamped from 8), pick and red toggle off, keywords unchanged.
    let r2 = apply_library_edit(&d, &op(), &all, &edits, "e2", ConflictPolicy::Fail, &l).unwrap();
    let m = d.get(&op(), &aid(3)).unwrap();
    assert_eq!((m.rating.value(), m.flag, m.color_label, r2.changed.len()), (5, Flag::Unflagged, None, 5));
    // An edit that changes nothing sends nothing.
    let events = d.events();
    let same = apply_library_edit(&d, &op(), &all, &[LibraryEdit::SetRating(Rating::new(5).unwrap())], "e3", ConflictPolicy::Fail, &l).unwrap();
    assert_eq!((same.receipts.len(), same.unchanged.len(), d.events()), (0, 5, events));
    // Replaying a prefix replays the receipts (no second event).
    let again = apply_library_edit(&d, &op(), &all, &edits, "e2", ConflictPolicy::Fail, &l);
    assert_eq!((again.map(|r| r.receipts), d.events()), (Ok(r2.receipts.clone()), events));
    // A concurrent edit lands on asset 4 just before the 2nd batch (assets 3, 4): batch 1 is
    // accepted and returned, the conflict names asset 4 with its revisions, nothing else applied.
    let racing = Racing { inner: &d, nth: 2, victim: aid(4), seen: Cell::new(0) };
    let err = apply_library_edit(&racing, &op(), &all, &[LibraryEdit::StepRating(-1)], "e4", ConflictPolicy::Fail, &l).unwrap_err();
    let LibraryWriteError::Conflict { conflicts, applied } = &err else { panic!("{err:?}") };
    assert_eq!(err.code(), "ckc_library_conflict");
    assert_eq!(applied.len(), 1, "accepted batch reported, not hidden");
    let rev4 = d.get(&op(), &aid(4)).unwrap().revision;
    assert_eq!(conflicts.as_slice(), [RevisionConflict { asset_id: aid(4), expected: rev4 - 1, current: rev4 }]);
    assert_eq!(d.get(&op(), &aid(3)).unwrap().rating.value(), 5, "the stale batch applied nothing");
    // RebaseOnce re-resolves the intent on fresh state: asset 4 was set to 1 by the other actor,
    // so "one star less" gives 0 there and 4 elsewhere.
    let racing = Racing { inner: &d, nth: 2, victim: aid(4), seen: Cell::new(0) };
    let ok = apply_library_edit(&racing, &op(), &all, &[LibraryEdit::StepRating(-1)], "e5", ConflictPolicy::RebaseOnce, &l).unwrap();
    assert_eq!(ok.rebased, 1);
    let ratings: Vec<u8> = (1..=5).map(|i| d.get(&op(), &aid(i)).unwrap().rating.value()).collect();
    assert_eq!(ratings, [3, 3, 4, 0, 4], "assets 1-2 took e4 and e5, 3 and 5 only e5, 4 rebased from 1");
    // Collection membership over a rule selection (every search page, canonical set).
    d.collection(&op(), &CollectionCommand::Create { idempotency_key: "c1".into(), collection_id: cid(1), name: "Picks".into(), parent: None, kind: CollectionKind::Manual }).unwrap();
    let sel = Selection::Rule(crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(3)));
    let small_pages = CkcLimits { max_page: 1, max_members: 2, ..l };
    let rs = set_collection_membership(&d, &op(), &cid(1), &sel, true, "m1", &small_pages).unwrap();
    assert_eq!(rs.len(), 2, "4 assets in member chunks of 2");
    let Some(DoubleCollection::Plain { members, .. }) = d.collection_state(&cid(1)) else { panic!() };
    assert_eq!(members.into_iter().collect::<Vec<_>>(), [aid(1), aid(2), aid(3), aid(5)]);
}

#[test]
fn smart_rules_evaluate_three_valued_over_port_records() {
    let l = CkcLimits::default();
    let d = double(2, l);
    d.update_library_metadata(&op(), &MetadataMutation {
        idempotency_key: "k".into(),
        asset_id: aid(1),
        expected_revision: 1,
        changes: vec![MetadataChange::SetRating(Rating::new(4).unwrap()), MetadataChange::SetKeywords(vec![kw(&["Places", "Rome"])])],
    })
    .unwrap();
    let ctx = RuleContext::new(d.list_collections(&op()).unwrap(), &l).unwrap();
    let rec = LibraryRecord::from_metadata(d.get(&op(), &aid(1)).unwrap());
    let r4 = crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(4));
    let iso = crit(CatalogField::Iso, CatalogOp::Gt, CatalogValue::Int(800));
    let red = crit(CatalogField::ColorLabel, CatalogOp::Eq, CatalogValue::ColorLabel(ColorLabel::Red));
    let not_red = crit(CatalogField::ColorLabel, CatalogOp::Ne, CatalogValue::ColorLabel(ColorLabel::Red));
    let rome = crit(CatalogField::Keywords, CatalogOp::ContainsWords, CatalogValue::Text("ROME".into()));
    let rom = crit(CatalogField::Keywords, CatalogOp::ContainsWords, CatalogValue::Text("rom".into()));
    let ev = |r: &CatalogRule| ctx.evaluate(r, &rec).unwrap();
    use Verdict::*;
    // Facts the port did not return are Unknown, never false; a missing label is a known absence.
    assert_eq!([ev(&r4), ev(&iso), ev(&red), ev(&not_red), ev(&rome), ev(&rom)], [Match, Unknown, NoMatch, Match, Match, NoMatch]);
    // Kleene: All(Match, Unknown) = Unknown, All(NoMatch, Unknown) = NoMatch, Any(Match, Unknown) = Match,
    // NoneOf(Unknown) = Unknown.
    let all = |v: Vec<CatalogRule>| CatalogRule::All(v);
    assert_eq!(ev(&all(vec![r4.clone(), iso.clone()])), Unknown);
    assert_eq!(ev(&all(vec![red.clone(), iso.clone()])), NoMatch);
    assert_eq!(ev(&CatalogRule::Any(vec![iso.clone(), r4.clone()])), Match);
    assert_eq!(ev(&CatalogRule::NoneOf(vec![iso.clone()])), Unknown);
    // With the fact supplied the verdict is decided; aperture compares Int and Real operands.
    let full = LibraryRecord { facts: RecordFacts { iso: Some(1600), aperture: Some(2.8), ..RecordFacts::default() }, ..rec.clone() };
    let f28 = CatalogRule::Match(Criterion { field: CatalogField::Aperture, op: CatalogOp::InRange, value: CatalogValue::Int(2), value2: Some(CatalogValue::Real(2.8)) });
    assert_eq!([ctx.evaluate(&iso, &full).unwrap(), ctx.evaluate(&f28, &full).unwrap()], [Match, Match]);
    // Preview over a page keeps Unknown records apart; it agrees with the double's own search on
    // the decided rule.
    let other = LibraryRecord::from_metadata(d.get(&op(), &aid(2)).unwrap());
    let page = [rec.clone(), other.clone(), full.clone()];
    let (hits, unknown) = ctx.preview(&all(vec![r4.clone(), iso.clone()]), &page).unwrap();
    assert_eq!((hits.len(), unknown.len()), (1, 1));
    let search = d.search(&op(), &SearchRequest { rule: Some(r4.clone()), sort: vec![], offset: 0, limit: 10 }).unwrap();
    let two = [rec, other];
    let (h, _) = ctx.preview(&r4, &two).unwrap();
    assert_eq!(search.assets.iter().map(|a| &a.asset_id).collect::<Vec<_>>(), h.iter().map(|r| &r.metadata.asset_id).collect::<Vec<_>>());
    // An invalid rule is refused before evaluation.
    assert_eq!(ctx.evaluate(&CatalogRule::All(vec![]), &full).unwrap_err().code(), "ckc_rule_empty_group");
}

#[test]
fn smart_collection_references_cycles_and_budgets_are_bounded() {
    let l = CkcLimits::default();
    let d = double(1, l);
    let mk = |n: u8, name: &str, parent: Option<CollectionId>, kind| {
        d.collection(&op(), &CollectionCommand::Create { idempotency_key: format!("c{n}"), collection_id: cid(n), name: name.into(), parent, kind }).unwrap();
    };
    mk(1, "Italy", None, CollectionKind::Set);
    mk(2, "Rome 2026", Some(cid(1)), CollectionKind::Manual);
    d.collection(&op(), &CollectionCommand::AddMembers { idempotency_key: "a".into(), collection_id: cid(2), asset_ids: vec![aid(1)] }).unwrap();
    let in_coll = |n: u8| crit(CatalogField::Collections, CatalogOp::Eq, CatalogValue::Text(cid(n).as_str().into()));
    let smart = |n: u8, rule: CatalogRule| {
        d.collection(&op(), &CollectionCommand::CreateSmart { idempotency_key: format!("s{n}"), collection_id: cid(n), name: format!("smart {n}"), parent: None, rule }).unwrap();
    };
    smart(3, in_coll(1)); // via the set
    smart(4, crit(CatalogField::Collections, CatalogOp::ContainsAll, CatalogValue::Text("rome".into()))); // by name
    smart(5, in_coll(6)); // 5 -> 6 -> 5
    smart(6, in_coll(5));
    smart(7, crit(CatalogField::Collections, CatalogOp::BeginsWith, CatalogValue::Text("smart".into()))); // names only manual/sets
    let ctx = RuleContext::new(d.list_collections(&op()).unwrap(), &l).unwrap();
    let rec = LibraryRecord::from_metadata(d.get(&op(), &aid(1)).unwrap());
    use Verdict::*;
    assert_eq!(ctx.member_of(&cid(3), &rec).unwrap(), Match, "smart -> set -> manual");
    assert_eq!(ctx.member_of(&cid(4), &rec).unwrap(), Match, "name of the manual collection");
    assert_eq!(ctx.member_of(&cid(7), &rec).unwrap(), NoMatch, "smart collections are not matched by name");
    assert_eq!(ctx.member_of(&cid(5), &rec).unwrap_err().code(), "ckc_smart_collection_cycle");
    // Static check before SetRule: self reference, two-step and set-mediated cycles.
    assert_eq!(ctx.check_acyclic(&cid(3), &in_coll(3)).unwrap_err().code(), "ckc_smart_collection_cycle");
    assert_eq!(ctx.check_acyclic(&cid(6), &in_coll(5)).unwrap_err().code(), "ckc_smart_collection_cycle");
    assert!(ctx.check_acyclic(&cid(4), &in_coll(3)).is_ok());
    // A 200-long chain of smart references exceeds max_reference_depth (32) instead of recursing.
    let chain: Vec<CollectionSummary> = (0..200u8)
        .map(|i| {
            let next = if i < 199 { in_coll(i + 1) } else { crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(0)) };
            CollectionSummary { collection_id: cid(i), name: format!("c{i}"), parent: None, kind: ListedKind::Smart(next), revision: 1 }
        })
        .collect();
    let cctx = RuleContext::new(chain, &l).unwrap();
    assert_eq!(cctx.member_of(&cid(0), &rec).unwrap_err().code(), "ckc_rule_reference_depth");
    assert_eq!(cctx.check_acyclic(&cid(250), &in_coll(0)).unwrap_err().code(), "ckc_rule_reference_depth");
    // Fan-out: 8 references to a collection whose rule has 4000 criteria; memoised per record, so
    // within budget; with a tight budget the evaluation stops typed.
    let wide = CatalogRule::Any(vec![crit(CatalogField::Iso, CatalogOp::Gt, CatalogValue::Int(1)); 4000]);
    let fan = vec![
        CollectionSummary { collection_id: cid(1), name: "w".into(), parent: None, kind: ListedKind::Smart(wide), revision: 1 },
        CollectionSummary { collection_id: cid(2), name: "f".into(), parent: None, kind: ListedKind::Smart(CatalogRule::All(vec![in_coll(1); 8])), revision: 1 },
    ];
    let fctx = RuleContext::new(fan.clone(), &l).unwrap();
    assert_eq!(fctx.member_of(&cid(2), &rec).unwrap(), Unknown);
    let tight = CkcLimits { max_eval_steps: 1000, ..l };
    assert_eq!(RuleContext::new(fan, &tight).unwrap().member_of(&cid(2), &rec).unwrap_err().code(), "ckc_rule_eval_budget");
    // Context bounds: repeated ids and too many collections are refused.
    let one = CollectionSummary { collection_id: cid(1), name: "x".into(), parent: None, kind: ListedKind::Manual, revision: 1 };
    assert_eq!(RuleContext::new(vec![one.clone(), one.clone()], &l).unwrap_err().code(), "ckc_collection_repeated");
    assert_eq!(RuleContext::new(vec![one; 3], &CkcLimits { max_collections: 2, ..l }).unwrap_err().code(), "ckc_collections");
}

#[test]
fn hostile_keyword_sets_and_selections_are_refused_before_any_command() {
    let l = CkcLimits::default();
    let d = double(3, l);
    let huge: Vec<KeywordPath> = (0..100_000).map(|i| kw(&[&format!("k{i}")])).collect();
    let sel = Selection::Assets(vec![aid(1)]);
    let err = apply_library_edit(&d, &op(), &sel, &[LibraryEdit::SetKeywords(huge.clone())], "h1", ConflictPolicy::Fail, &l).unwrap_err();
    assert_eq!(err, LibraryWriteError::Invalid(CkcError::Limit("ckc_keyword_set")));
    // The port contract refuses the same set directly, and a repeated keyword.
    let m = |changes| MetadataMutation { idempotency_key: "h2".into(), asset_id: aid(1), expected_revision: 1, changes };
    assert_eq!(d.update_library_metadata(&op(), &m(vec![MetadataChange::SetKeywords(huge)])).unwrap_err().code(), "ckc_keyword_set");
    let rep = vec![kw(&["a"]), kw(&["b"]), kw(&["a"])];
    assert_eq!(d.update_library_metadata(&op(), &m(vec![MetadataChange::SetKeywords(rep.clone())])).unwrap_err().code(), "ckc_keyword_repeated");
    // Adding keywords that would push an asset past the per-asset bound is refused at resolve time.
    let small = CkcLimits { max_keywords_per_asset: 3, ..l };
    let four: Vec<KeywordPath> = ["a", "b", "c", "d"].iter().map(|k| kw(&[k])).collect();
    let e = apply_library_edit(&d, &op(), &sel, &[LibraryEdit::AddKeywords(four[..2].to_vec()), LibraryEdit::AddKeywords(four[2..].to_vec())], "h3", ConflictPolicy::Fail, &small).unwrap_err();
    assert_eq!(e.code(), "ckc_keyword_set");
    // Selections: repeated asset, empty, over max_selection (explicit and through search total).
    let codes: Vec<&str> = [
        Selection::Assets(vec![aid(2), aid(2)]),
        Selection::Assets(vec![]),
        Selection::Assets((1..=4).map(aid).collect()),
        Selection::Rule(crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(0))),
    ]
    .iter()
    .map(|s| resolve_selection(&d, &op(), s, &CkcLimits { max_selection: 2, ..l }).unwrap_err().code())
    .collect();
    assert_eq!(codes, ["ckc_selection_repeated_asset", "ckc_selection_empty", "ckc_selection", "ckc_selection"]);
    // Out-of-range rating step and an idempotency prefix that cannot form valid keys.
    let bad_step = apply_library_edit(&d, &op(), &sel, &[LibraryEdit::StepRating(9)], "h4", ConflictPolicy::Fail, &l).unwrap_err();
    let bad_key = apply_library_edit(&d, &op(), &sel, &[LibraryEdit::StepRating(1)], "bad key", ConflictPolicy::Fail, &l).unwrap_err();
    assert_eq!([bad_step.code(), bad_key.code()], ["ckc_rating_step", "ckc_key_charset"]);
    assert_eq!(d.events(), 0, "no hostile request reached the port as a command");
    // A rule nested past max_rule_depth in a listed collection is refused when the context is built.
    let mut deep = crit(CatalogField::Rating, CatalogOp::Ge, CatalogValue::Int(1));
    for _ in 0..100 {
        deep = CatalogRule::All(vec![deep]);
    }
    let bomb = CollectionSummary { collection_id: cid(9), name: "bomb".into(), parent: None, kind: ListedKind::Smart(deep), revision: 1 };
    assert_eq!(RuleContext::new(vec![bomb], &l).unwrap_err().code(), "ckc_rule_depth");
}
