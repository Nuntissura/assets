use std::collections::{BTreeMap, BTreeSet};
use hsscrap::{ports::*, *};
use rss_acquisition::capture::save_url;
use rss_contracts::CaptureIntent;

fn reference(id: &str) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "hsscrap".into(), native_id: id.into() } }
fn instant() -> Instant { Instant::parse("2026-10-23T00:00:00Z").unwrap() }
fn bounds() -> Bounds { Bounds { max_url_bytes: 4096, max_text_bytes: 4096, max_collection_items: 100, max_page_items: 25, max_operation_bytes: 100000 } }
fn command() -> (CommandContext, URLCapture) {
    let operation_id = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let item_id = "00000000-0000-0000-0000-000000000002".parse().unwrap();
    let url = "https://example.org/article";
    let normalized = hsscrap::identity::normalize_url(url, &bounds()).unwrap();
    let context = CommandContext { actor: reference("actor"), scope: ScopeRef("profile-view".into()), operation_id,
        intent_digest: "capture-intent-v1".into(), expected_revisions: vec![], bounds: bounds() };
    let capture = URLCapture { capture_id: "00000000-0000-0000-0000-000000000003".parse().unwrap(), item_id,
        capture_kind: CaptureKind::UserInput, access_partition: context.scope.clone(), captured_url: url.into(), original_url: Some(url.into()),
        identity_url: normalized.identity_url, normalization_version: normalized.normalization_version, captured_at: Some(instant()), created_here_at: instant(),
        actor_ref: Some(context.actor.clone()), operation_id, common: Extensions::default() };
    (context, capture)
}
#[derive(Default)]
struct Owner { calls: usize, recovery_calls: usize, outcomes: BTreeMap<OperationId, OperationOutcome>, captures: Vec<URLCapture>, lose_response: bool, pending: bool, recoverable: bool, hide_capture: bool, canonicalize_guards: bool, invalid_item_owner: bool, deny_inspection: bool }
impl MutationCoordinator for Owner {
    fn commit(&mut self, _: CommandContext, _: Vec<OwnerCommand>) -> Result<OperationOutcome> { panic!("capture uses its owner") }
    fn lookup(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> { self.operation_outcome(context, operation) }
    fn recover(&mut self, context: CommandContext) -> Result<OperationOutcome> {
        self.recovery_calls += 1;
        let outcome = self.outcomes.get_mut(&context.operation_id).ok_or(Error::NotFound)?;
        if outcome.scope != context.scope || outcome.intent_digest != context.intent_digest { return Err(Error::Denied); }
        if self.recoverable { outcome.outcome = OutcomeState::Committed; outcome.committed_at = Some(instant()); }
        Ok(outcome.clone())
    }
}
impl ItemOwner for Owner {
    fn capture(&mut self, command: CaptureCommand) -> Result<OperationOutcome> {
        self.calls += 1;
        self.captures.push(command.capture);
        let outcome = OperationOutcome { operation_id: command.context.operation_id, scope: command.context.scope, intent_digest: command.context.intent_digest,
            expected_revisions: command.context.expected_revisions, outcome: if self.pending { OutcomeState::Pending } else { OutcomeState::Committed },
            result_refs: vec![reference("item")], owner_receipt_refs: vec![reference("receipt")], committed_at: (!self.pending).then(instant), common: Extensions::default() };
        let outcome = if self.canonicalize_guards { serde_json::from_str(&serde_json::to_string(&outcome).unwrap()).unwrap() } else { outcome };
        self.outcomes.insert(outcome.operation_id, outcome.clone());
        if self.lose_response { Err(Error::UnknownOutcome { operation_id: outcome.operation_id }) } else { Ok(outcome) }
    }
    fn inspect(&self, context: &CommandContext, item: ItemId) -> Result<ItemSnapshot> {
        if self.deny_inspection {return Err(Error::Denied);}
        let capture = self.captures.iter().find(|capture| capture.item_id == item).ok_or(Error::NotFound)?;
        if capture.access_partition != context.scope { return Err(Error::Denied); }
        Ok(ItemSnapshot { item: Item { item_id: item, owner_scope: ScopeRef((if self.invalid_item_owner {""} else {"account"}).into()), revision: Revision(1), first_capture_id: capture.capture_id,
            selected_observation_id: None, gallery_observation_id: None, member_order: vec![], title_override: Override::Inherit,
            description_override: Override::Inherit, cover_override: Override::Inherit, created_at: instant(), updated_at: instant(), lifecycle: Lifecycle::Active,
            deleted_at: None, common: Extensions::default() }, captures: if self.hide_capture { vec![] } else { self.captures.clone() }, observations: vec![], members: vec![], streams: vec![] })
    }
    fn lookup_identity(&self, _: &CommandContext, _: IdentityQuery) -> Result<Option<ItemId>> { Ok(None) }
    fn lookup_url(&self, context: &CommandContext, query: UrlIdentityQuery) -> Result<Vec<ItemId>> {
        query.validate(&context.bounds)?;
        if query.access_partition != context.scope { return Err(Error::Denied); }
        Ok(self.captures.iter().filter(|capture| capture.access_partition == context.scope && capture.identity_url == query.identity_url)
            .map(|capture| capture.item_id).collect::<BTreeSet<_>>().into_iter().collect())
    }
    fn operation_outcome(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> {
        let value = self.outcomes.get(&operation).ok_or(Error::NotFound)?;
        if value.scope != context.scope { return Err(Error::Denied); }
        Ok(value.clone())
    }
    fn apply_patch(&mut self, _: CommandContext, _: ItemId, _: ItemPatch) -> Result<OperationOutcome> { panic!("capture must not edit metadata") }
    fn accept_observation(&mut self, _: CommandContext, _: MetadataObservation) -> Result<OperationOutcome> { panic!("capture must not enrich") }
    fn apply_gallery_snapshot(&mut self, _: CommandContext, _: ItemId, _: ObservationId) -> Result<OperationOutcome> { panic!("capture must not enrich galleries") }
    fn tombstone(&mut self, _: CommandContext, _: ItemId) -> Result<OperationOutcome> { panic!("capture must not delete") }
}
fn intent(capture: &URLCapture) -> CaptureIntent { CaptureIntent::SaveUrl { original_url: capture.captured_url.clone() } }

#[test]
fn saved_url_is_durable_before_enrichment_and_lost_response_replays_without_second_capture() {
    let (context, capture) = command(); let mut owner = Owner { lose_response: true, ..Default::default() };
    let outcome = save_url(&mut owner, &intent(&capture), context.clone(), capture.clone(), &BTreeSet::new()).unwrap();
    assert_eq!(outcome.outcome, OutcomeState::Committed);
    save_url(&mut owner, &intent(&capture), context.clone(), capture.clone(), &BTreeSet::new()).unwrap();
    assert_eq!(owner.calls, 1);
    assert_eq!(owner.captures, vec![capture.clone()]);
    let snapshot=owner.inspect(&context,capture.item_id).unwrap();
    assert_eq!(snapshot.item.owner_scope,ScopeRef("account".into()));
    assert_eq!(capture.access_partition,ScopeRef("profile-view".into()));
    owner.deny_inspection=true;
    assert_eq!(save_url(&mut owner,&intent(&capture),context.clone(),capture.clone(),&BTreeSet::new()),Err(rss_contracts::Error::ItemOwner {error:Error::Denied}));
    owner.deny_inspection=false;
    let mut changed = context; changed.intent_digest = "different-intent".into();
    assert_eq!(save_url(&mut owner, &intent(&capture), changed, capture, &BTreeSet::new()), Err(rss_contracts::Error::Conflict));
    assert_eq!(owner.calls, 1);
}

#[test]
fn pending_or_missing_canonical_capture_never_acknowledges_success() {
    for (pending, hide_capture, invalid_item_owner) in [(true, false, false), (false, true, false), (false, false, true)] {
        let (context, capture) = command(); let mut owner = Owner { pending, hide_capture, invalid_item_owner, ..Default::default() };
        let result = save_url(&mut owner, &intent(&capture), context.clone(), capture.clone(), &BTreeSet::new());
        assert_eq!(result, Err(rss_contracts::Error::UnknownOutcome { operation_id: context.operation_id }));
        if pending {
            assert!(save_url(&mut owner, &intent(&capture), context, capture, &BTreeSet::new()).is_err());
            assert_eq!(owner.calls, 1);
        }
    }
}

#[test]
fn save_and_follow_intents_and_scope_are_not_interchangeable() {
    let (context, capture) = command(); let mut owner = Owner::default();
    let follow = CaptureIntent::FollowSource { original_url: capture.captured_url.clone(), provider: rss_contracts::Provider::Syndication,
        partition: rss_contracts::AccessPartition { scope: context.scope.clone(), mode: rss_contracts::AccessMode::PublicFeed, connection_ref: None } };
    assert!(save_url(&mut owner, &follow, context.clone(), capture.clone(), &BTreeSet::new()).is_err());
    let mut denied = context; denied.scope = ScopeRef("other-account".into());
    assert_eq!(save_url(&mut owner, &intent(&capture), denied, capture, &BTreeSet::new()), Err(rss_contracts::Error::Denied));
    assert_eq!(owner.calls, 0);
}

#[test]
fn interrupted_pending_capture_uses_same_operation_recovery_then_canonical_readback() {
    let (context, capture) = command(); let mut owner = Owner { pending: true, recoverable: true, ..Default::default() };
    owner.capture(CaptureCommand { context: context.clone(), capture: capture.clone(), item_id: capture.item_id }).unwrap();
    let outcome = save_url(&mut owner, &intent(&capture), context.clone(), capture.clone(), &BTreeSet::new()).unwrap();
    assert_eq!(outcome.operation_id, context.operation_id);
    assert_eq!(outcome.outcome, OutcomeState::Committed);
    assert_eq!(owner.calls, 1); assert_eq!(owner.recovery_calls, 1);
    assert_eq!(owner.inspect(&context, capture.item_id).unwrap().captures, vec![capture]);
}

#[test]
fn canonical_guard_roundtrip_accepts_reordering_but_rejects_changed_or_duplicate_owners() {
    let (mut context, capture) = command(); context.expected_revisions = vec![
        ExpectedRevision { owner: reference("z-owner"), revision: Revision(3) },
        ExpectedRevision { owner: reference("a-owner"), revision: Revision(1) },
    ];
    let mut owner = Owner { canonicalize_guards: true, ..Default::default() };
    save_url(&mut owner, &intent(&capture), context.clone(), capture.clone(), &BTreeSet::new()).unwrap();
    let stored = &owner.outcomes[&context.operation_id];
    assert_ne!(stored.expected_revisions, context.expected_revisions, "actual owning serializer must reorder this guard list");
    for change_owner in [false, true] {
        let mut changed = context.clone();
        if change_owner { changed.expected_revisions[0].owner = reference("different-owner"); } else { changed.expected_revisions[0].revision = Revision(4); }
        assert_eq!(save_url(&mut owner, &intent(&capture), changed, capture.clone(), &BTreeSet::new()), Err(rss_contracts::Error::Conflict));
    }
    let mut duplicate = context.clone(); duplicate.expected_revisions.push(duplicate.expected_revisions[0].clone());
    assert!(save_url(&mut owner, &intent(&capture), duplicate, capture.clone(), &BTreeSet::new()).is_err());
    let stored = owner.outcomes.get_mut(&context.operation_id).unwrap(); stored.expected_revisions.push(stored.expected_revisions[0].clone());
    assert!(save_url(&mut owner, &intent(&capture), context, capture, &BTreeSet::new()).is_err());
    assert_eq!(owner.calls, 1);
}
