use std::{collections::{BTreeMap, BTreeSet}, future::Future, pin::pin, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, task::{Context, Poll, Wake, Waker}};
use hsscrap::{ports::*, *};
use rss_acquisition::{authority::*, ingestion::*, jobs::*};
use rss_contracts::{AcquisitionPage, AcquisitionState, AccessMode, AccessPartition, AuthorityPort, BatchCommit, CheckpointOwner,
    CommitOutcome, Continuation, Pending, SourceAction, SourceAppearance, SourceRequest, Subscription};

fn run<T>(future: impl Future<Output = T>) -> T {
    struct Noop; impl Wake for Noop { fn wake(self: Arc<Self>) {} }
    let waker = Waker::from(Arc::new(Noop)); let mut context = Context::from_waker(&waker); let mut future = pin!(future);
    match future.as_mut().poll(&mut context) { Poll::Ready(value) => value, Poll::Pending => panic!("fixture unexpectedly requires external runtime") }
}
fn id<T: std::str::FromStr>(value: u64) -> T where T::Err: std::fmt::Debug { format!("00000000-0000-0000-0000-{value:012}").parse().unwrap() }
fn reference(value: impl Into<String>) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "fixture".into(), native_id: value.into() } }
fn at() -> Instant { Instant::parse("2026-10-23T00:00:00Z").unwrap() }
fn bounds() -> Bounds { Bounds { max_url_bytes: 4096, max_text_bytes: 4096, max_collection_items: 100, max_page_items: 25, max_operation_bytes: 100000 } }
fn request(page: u64) -> SourceRequest { SourceRequest {
    subscription: Subscription { subscription_ref: reference("subscription"), source_ref: reference("source"),
        partition: AccessPartition { scope: ScopeRef("account".into()), mode: AccessMode::PublicFeed, connection_ref: None },
        executor_ref: reference("executor"), generation: Revision(1), paused: false, common: Extensions::default() },
    operation_id: id(page), continuation: None, bounds: bounds(),
} }
fn number(request: &SourceRequest) -> u64 { request.operation_id.to_string()[24..].parse().unwrap() }
fn context(request: &SourceRequest, step: u64) -> CommandContext { CommandContext { actor: reference("actor"),
    scope: request.subscription.partition.scope.clone(), operation_id: id(1000 + number(request) * 10 + step),
    intent_digest: format!("page-{}-step-{step}", number(request)), expected_revisions: vec![], bounds: request.bounds.clone() } }
fn query() -> IdentityQuery { IdentityQuery { provider: "syndication".into(), namespace: "posts".into(), provider_item_id: "opaque-post".into(), access_partition: ScopeRef("account".into()) } }

#[derive(Default)]
struct Store {
    captures: Vec<URLCapture>, observations: Vec<MetadataObservation>, streams: Vec<ObservationStream>, identities: Vec<SourceIdentity>,
    outcomes: BTreeMap<OperationId, OperationOutcome>, appearances: BTreeMap<String, SourceAppearance>, mutations: usize,
    outcome_refs: BTreeMap<HostRef, OperationId>,
}
#[derive(Clone, Default)]
struct Owner(Arc<Mutex<Store>>);
impl Owner {
    fn record(store: &mut Store, context: CommandContext) -> OperationOutcome {
        let outcome = OperationOutcome { operation_id: context.operation_id, scope: context.scope, intent_digest: context.intent_digest,
            expected_revisions: context.expected_revisions, outcome: OutcomeState::Committed, result_refs: vec![],
            owner_receipt_refs: vec![reference(context.operation_id.to_string())], committed_at: Some(at()), common: Extensions::default() };
        store.outcome_refs.insert(reference(format!("canonical-operation-{}", outcome.operation_id)), outcome.operation_id);
        store.outcomes.insert(outcome.operation_id, outcome.clone()); store.mutations += 1; outcome
    }
}
impl ItemOwner for Owner {
    fn capture(&mut self, command: CaptureCommand) -> Result<OperationOutcome> {
        let mut store = self.0.lock().unwrap(); store.captures.push(command.capture); Ok(Self::record(&mut store, command.context))
    }
    fn inspect(&self, context: &CommandContext, item: ItemId) -> Result<ItemSnapshot> {
        let store = self.0.lock().unwrap(); let first = store.captures.iter().find(|capture| capture.item_id == item).ok_or(Error::NotFound)?;
        if first.access_partition != context.scope { return Err(Error::Denied); }
        Ok(ItemSnapshot { item: Item { item_id: item, owner_scope: context.scope.clone(), revision: Revision(1), first_capture_id: first.capture_id,
            selected_observation_id: None, gallery_observation_id: None, member_order: vec![], title_override: Override::Inherit,
            description_override: Override::Inherit, cover_override: Override::Inherit, created_at: at(), updated_at: at(), lifecycle: Lifecycle::Active,
            deleted_at: None, common: Extensions::default() }, captures: store.captures.iter().filter(|value| value.item_id == item).cloned().collect(),
            observations: store.observations.iter().filter(|value| value.item_id == item).cloned().collect(), members: vec![],
            streams: store.streams.iter().filter(|value| value.item_id == item).cloned().collect() })
    }
    fn lookup_identity(&self, context: &CommandContext, query: IdentityQuery) -> Result<Option<ItemId>> {
        if query.access_partition != context.scope { return Err(Error::Denied); }
        Ok(self.0.lock().unwrap().identities.iter().find(|value| value.provider == query.provider && value.namespace == query.namespace
            && value.provider_item_id == query.provider_item_id && value.access_partition == context.scope && value.retired_at.is_none()).map(|value| value.item_id))
    }
    fn lookup_url(&self, context: &CommandContext, query: UrlIdentityQuery) -> Result<Vec<ItemId>> {
        query.validate(&context.bounds)?; if query.access_partition != context.scope { return Err(Error::Denied); }
        Ok(self.0.lock().unwrap().captures.iter().filter(|value| value.identity_url == query.identity_url && value.access_partition == context.scope)
            .map(|value| value.item_id).collect::<BTreeSet<_>>().into_iter().collect())
    }
    fn accept_observation(&mut self, context: CommandContext, observation: MetadataObservation) -> Result<OperationOutcome> {
        let mut store = self.0.lock().unwrap(); let stream = store.streams.iter_mut().find(|stream| stream.stream_id == observation.stream_id).unwrap();
        stream.last_accepted_sequence = Some(observation.sequence); stream.latest_observation_id = Some(observation.observation_id);
        store.observations.push(observation); Ok(Self::record(&mut store, context))
    }
    fn assert_source_identity(&mut self, context: CommandContext, identity: SourceIdentity) -> Result<OperationOutcome> {
        let mut store = self.0.lock().unwrap(); store.identities.push(identity); Ok(Self::record(&mut store, context))
    }
    fn inspect_source_identity(&self, context: &CommandContext, identity: IdentityId) -> Result<SourceIdentity> {
        self.0.lock().unwrap().identities.iter().find(|value| value.identity_id == identity && value.access_partition == context.scope).cloned().ok_or(Error::NotFound)
    }
    fn operation_outcome(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> {
        let store = self.0.lock().unwrap(); let outcome = store.outcomes.get(&operation).ok_or(Error::NotFound)?;
        if outcome.scope != context.scope { return Err(Error::Denied); } Ok(outcome.clone())
    }
    fn apply_patch(&mut self, _: CommandContext, _: ItemId, _: ItemPatch) -> Result<OperationOutcome> { panic!("ingestion must preserve overrides") }
    fn apply_gallery_snapshot(&mut self, _: CommandContext, _: ItemId, _: ObservationId) -> Result<OperationOutcome> { panic!("fixture has no gallery") }
    fn tombstone(&mut self, _: CommandContext, _: ItemId) -> Result<OperationOutcome> { panic!("provider failure must preserve items") }
}
impl MutationCoordinator for Owner {
    fn commit(&mut self, _: CommandContext, _: Vec<OwnerCommand>) -> Result<OperationOutcome> { panic!("use declared item owner methods") }
    fn lookup(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> { self.operation_outcome(context, operation) }
    fn recover(&mut self, context: CommandContext) -> Result<OperationOutcome> { self.operation_outcome(&context, context.operation_id) }
}
struct Admission { current: Mutex<Subscription>, operation: Mutex<OperationId>, active: AtomicBool, allowed: AtomicBool }
impl Admission { fn new(request: &SourceRequest) -> Self { Self { current: Mutex::new(request.subscription.clone()), operation: Mutex::new(request.operation_id), active: AtomicBool::new(true), allowed: AtomicBool::new(true) } } }
impl AuthorityPort for Admission {
    fn authorize<'a>(&'a self, _: &'a ScopeRef, _: &'a HostRef, _: SourceAction) -> Pending<'a, ()> {
        Box::pin(async move { if self.allowed.load(Ordering::SeqCst) { Ok(()) } else { Err(rss_contracts::Error::Denied) } })
    }
}
impl AcquisitionOwner for Admission {
    fn admit<'a>(&'a self, request: &'a SourceRequest, execution_ref: &'a HostRef) -> Pending<'a, Subscription> {
        Box::pin(async move { if !self.active.load(Ordering::SeqCst) || execution_ref != &reference("attempt") || request.operation_id != *self.operation.lock().unwrap() { return Err(rss_contracts::Error::Cancelled); }
            Ok(self.current.lock().unwrap().clone()) })
    }
}
struct Allocator(Owner);
impl EntryAllocator for Allocator {
    fn allocate_capture<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str, resolved: Option<ItemId>, url: hsscrap::identity::NormalizedCaptureUrl) -> Pending<'a, CaptureAllocation> {
        Box::pin(async move { let context = context(request, 1); Ok(CaptureAllocation { capture: URLCapture {
            capture_id: id(100 + number(request)), item_id: resolved.unwrap_or_else(|| id(500)), capture_kind: CaptureKind::SourceIngest,
            access_partition: context.scope.clone(), captured_url: url.original.clone(), original_url: Some(url.original), identity_url: url.identity_url,
            normalization_version: url.normalization_version, captured_at: Some(at()), created_here_at: at(), actor_ref: Some(context.actor.clone()),
            operation_id: context.operation_id, common: Extensions::default() }, context, appearance_ref: reference("appearance") }) })
    }
    fn reserve_observation<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str, snapshot: ItemSnapshot, identity: Option<IdentityQuery>) -> Pending<'a, ObservationAllocation> {
        Box::pin(async move {
            let capture_id = id(100 + number(request)); let observation_id = id(200 + number(request)); let stream_id = id(300);
            let mut store = self.0.0.lock().unwrap();
            assert!(store.captures.iter().any(|value| value.capture_id == capture_id), "capture must commit before stream reservation/enrichment");
            if !store.streams.iter().any(|stream| stream.stream_id == stream_id) { store.streams.push(ObservationStream { stream_id,
                item_id: snapshot.item.item_id, producer: "fixture".into(), provider_namespace: identity.as_ref().map(|query| query.namespace.clone()).unwrap_or_else(|| "url-only".into()),
                source_ref: request.subscription.source_ref.clone(), access_partition: request.subscription.partition.scope.clone(), last_issued_sequence: Revision(number(request)),
                last_accepted_sequence: None, latest_observation_id: None, revision: Revision(1), common: Extensions::default() }); }
            let stream = store.streams.iter_mut().find(|stream| stream.stream_id == stream_id).unwrap();
            stream.last_issued_sequence = Revision(number(request));
            let baseline = if number(request) > 1 { Some(id(200 + number(request) - 1)) } else { None };
            Ok(ObservationAllocation { context: context(request, 2), observation: MetadataObservation { observation_id, item_id: snapshot.item.item_id,
                stream_id, sequence: Revision(number(request)), baseline_observation_id: baseline, capture_id: Some(capture_id), appearance_ref: Some(reference("appearance")),
                producer: "fixture".into(), access_partition: request.subscription.partition.scope.clone(), observed_at: at(), source_revision: None,
                canonical_url_assertion: None, provider: identity.as_ref().map(|query| query.provider.clone()), host_site: "example.org".into(), title: None,
                description: None, author: None, published_at: None, provider_labels: vec![], gallery_snapshot: None, source_payload_digest: None,
                status: Availability { state: AvailabilityState::Available, checked_at: Some(at()), reason_code: None, retry_after: None }, common: Extensions::default() } })
        })
    }
    fn reserve_identity<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str,
        current: ItemSnapshot, observation: &'a MetadataObservation, query: IdentityQuery) -> Pending<'a, IdentityAllocation> {
        Box::pin(async move {
            assert!(current.observations.iter().any(|stored| stored == observation), "identity guard must follow observation commit/readback");
            Ok(IdentityAllocation { context: context(request, 3), identity: SourceIdentity { identity_id: id(400 + number(request)),
                item_id: current.item.item_id, provider: query.provider, namespace: query.namespace, provider_item_id: query.provider_item_id,
                access_partition: query.access_partition, verified_by_observation_id: observation.observation_id, retired_at: None, common: Extensions::default() } })
        })
    }
    fn inspect_appearance<'a>(&'a self, _: &'a SourceRequest, key: &'a str) -> Pending<'a, Option<SourceAppearance>> {
        Box::pin(async move { Ok(self.0.0.lock().unwrap().appearances.get(key).cloned()) })
    }
    fn outcome_reference<'a>(&'a self, context: &'a CommandContext) -> Pending<'a, HostRef> {
        Box::pin(async move { self.0.0.lock().unwrap().outcome_refs.iter().find(|(_, operation)| **operation == context.operation_id)
            .map(|(reference, _)| reference.clone()).ok_or(rss_contracts::Error::NotFound) })
    }
    fn inspect_outcome<'a>(&'a self, scope: &'a ScopeRef, reference: &'a HostRef) -> Pending<'a, OperationOutcome> {
        Box::pin(async move { let store = self.0.0.lock().unwrap(); let operation = store.outcome_refs.get(reference).ok_or(rss_contracts::Error::NotFound)?;
            let outcome = store.outcomes.get(operation).ok_or(rss_contracts::Error::NotFound)?;
            if &outcome.scope != scope { return Err(rss_contracts::Error::Denied); } Ok(outcome.clone()) })
    }
}
#[derive(Default)]
struct CheckpointState { batches: BTreeMap<OperationId, (ScopeRef, BatchCommit, CommitOutcome)>, calls: usize, lookups: usize, lose_next: bool, pending: bool, wrong_generation: bool }
struct Checkpoints { state: Mutex<CheckpointState>, owner: Owner }
impl Checkpoints { fn new(owner: Owner) -> Self { Self { state: Mutex::new(CheckpointState::default()), owner } } }
impl CheckpointOwner for Checkpoints {
    fn lookup<'a>(&'a self, scope: &'a ScopeRef, operation: OperationId) -> Pending<'a, CommitOutcome> {
        Box::pin(async move { let mut state = self.state.lock().unwrap(); state.lookups += 1; let (stored_scope, _, result) = state.batches.get(&operation).ok_or(rss_contracts::Error::NotFound)?;
            if stored_scope != scope { return Err(rss_contracts::Error::Denied); } Ok(result.clone()) })
    }
    fn commit<'a>(&'a self, scope: &'a ScopeRef, batch: BatchCommit) -> Pending<'a, CommitOutcome> {
        Box::pin(async move { let mut state = self.state.lock().unwrap(); state.calls += 1;
            if let Some((stored_scope, stored, _)) = state.batches.get(&batch.operation_id) { if stored_scope != scope || stored != &batch { return Ok(CommitOutcome::Conflict); } }
            { let store = self.owner.0.lock().unwrap(); for reference in &batch.item_owner_outcome_refs {
                let operation = store.outcome_refs.get(reference).ok_or(rss_contracts::Error::NotFound)?;
                let outcome = store.outcomes.get(operation).ok_or(rss_contracts::Error::NotFound)?;
                if &outcome.scope != scope || outcome.outcome != OutcomeState::Committed { return Err(rss_contracts::Error::Denied); }
            } }
            let result = if state.pending { CommitOutcome::Pending { operation_id: batch.operation_id } } else { CommitOutcome::Committed {
                receipt_ref: reference(format!("checkpoint-{}", batch.operation_id)), generation: if state.wrong_generation { Revision(99) } else { batch.expected_generation } } };
            if matches!(result, CommitOutcome::Committed { .. }) { let mut store = self.owner.0.lock().unwrap(); for appearance in &batch.appearances { store.appearances.insert(appearance.provider_entry_key.clone(), appearance.clone()); } }
            state.batches.insert(batch.operation_id, (scope.clone(), batch.clone(), result.clone()));
            if state.lose_next { state.lose_next = false; return Err(rss_contracts::Error::UnknownOutcome { operation_id: batch.operation_id }); } Ok(result)
        })
    }
}
fn prepared(owner: &mut Owner, admission: &Admission, request: &SourceRequest, identity: Option<IdentityQuery>) -> PreparedEntry {
    let allocator = Allocator(owner.clone());
    run(prepare_entry(owner, admission, admission, &allocator, &reference("attempt"), request, &context(request, 0), "opaque-post", "https://example.org/post", identity,
        |observation| observation.title = Some("Actual parsed title".into()), &BTreeSet::new())).unwrap()
}
fn page(request: &SourceRequest, prepared: &PreparedEntry, more: bool) -> AcquisitionPage { AcquisitionPage {
    subscription_ref: request.subscription.subscription_ref.clone(), source_ref: request.subscription.source_ref.clone(), partition: request.subscription.partition.clone(),
    executor_ref: request.subscription.executor_ref.clone(), generation: request.subscription.generation, operation_id: request.operation_id,
    entries: vec![prepared.entry().clone()], state: if more { AcquisitionState::Partial { observed_at: at(), reason_code: "more_pages".into() } } else { AcquisitionState::Ready { last_success_at: at() } },
    coverage: if more { Coverage::Partial } else { Coverage::Complete }, continuation: more.then(|| Continuation { source_ref: request.subscription.source_ref.clone(),
        subscription_ref: request.subscription.subscription_ref.clone(), partition: request.subscription.partition.clone(), executor_ref: request.subscription.executor_ref.clone(),
        generation: request.subscription.generation, token: "page-2".into() })
} }

#[test]
fn identity_resolution_reuses_saved_url_and_rejects_conflicting_provider_identity() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default();
    let first = prepared(&mut owner, &admission, &request, None);
    let current = owner.0.lock().unwrap().mutations;
    assert_eq!(rss_acquisition::identity::resolve_capture_identity(&owner, &context(&request, 0), "https://example.org/post", Some(&query())).unwrap(), Some(first.entry().capture.item_id));
    assert_eq!(owner.0.lock().unwrap().mutations, current);
    owner.0.lock().unwrap().identities.push(SourceIdentity { identity_id: id(999), item_id: id(501), provider: query().provider, namespace: query().namespace,
        provider_item_id: query().provider_item_id, access_partition: request.subscription.partition.scope.clone(), verified_by_observation_id: first.entry().observation.observation_id,
        retired_at: None, common: Extensions::default() });
    assert!(matches!(rss_acquisition::identity::resolve_capture_identity(&owner, &context(&request, 0), "https://example.org/post", Some(&query())), Err(rss_contracts::Error::ItemOwner { error: Error::Conflict { .. } })));
    assert_eq!(owner.0.lock().unwrap().mutations, current);
}

#[test]
fn lost_checkpoint_reply_requires_exact_full_batch_retry_before_settlement() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default(); let checkpoints = Checkpoints::new(owner.clone());
    let prepared = prepared(&mut owner, &admission, &request, Some(query())); let mut page = page(&request, &prepared, true);
    let mut queue = JobQueue::new(JobLimits { max_jobs: 1, max_running: 1, max_attempts: 3, timeout_ticks: 10, initial_backoff_ticks: 1, max_backoff_ticks: 4 }).unwrap();
    queue.enqueue(request.clone(), 0).unwrap(); queue.start(request.operation_id, &reference("executor"), 0).unwrap(); queue.begin_commit(request.operation_id, &reference("executor"), 1).unwrap();
    checkpoints.state.lock().unwrap().lose_next = true;
    let prepared = [prepared];
    assert!(matches!(run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &prepared, &BTreeSet::new())), Err(rss_contracts::Error::UnknownOutcome { .. })));
    assert_eq!(checkpoints.state.lock().unwrap().lookups, 0); assert_eq!(queue.jobs()[0].state, JobState::AwaitingRecovery);
    let ack = run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &prepared, &BTreeSet::new())).unwrap();
    let before_advance = serde_json::to_vec(&queue).unwrap();
    assert!(queue.acknowledge(&ack).is_err());
    assert_eq!(serde_json::to_vec(&queue).unwrap(), before_advance);
    queue.advance(&ack, id(2), 2).unwrap(); assert!(matches!(queue.jobs()[0].state, JobState::Queued { .. }));
    page.continuation.as_mut().unwrap().token = "changed-intent".into();
    assert!(matches!(run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &prepared, &BTreeSet::new())), Err(rss_contracts::Error::Conflict)));
    assert_eq!(checkpoints.state.lock().unwrap().lookups, 1);
}

#[test]
fn two_source_pages_keep_the_same_subscription_fence_and_reuse_canonical_item() {
    let first_request = request(1); let admission = Admission::new(&first_request); let mut owner = Owner::default(); let checkpoints = Checkpoints::new(owner.clone());
    let mut queue = JobQueue::new(JobLimits { max_jobs: 1, max_running: 1, max_attempts: 3, timeout_ticks: 10, initial_backoff_ticks: 1, max_backoff_ticks: 4 }).unwrap();
    queue.enqueue(first_request.clone(), 0).unwrap();
    queue.start(first_request.operation_id, &reference("executor"), 0).unwrap();
    queue.begin_commit(first_request.operation_id, &reference("executor"), 1).unwrap();
    let first = prepared(&mut owner, &admission, &first_request, Some(query())); let first_page = page(&first_request, &first, true);
    let first_ack = run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &first_request, &first_page, &[first], &BTreeSet::new())).unwrap();
    let before_advance = serde_json::to_vec(&queue).unwrap(); let mut queue: JobQueue = serde_json::from_slice(&before_advance).unwrap();
    assert_eq!(queue.advance(&first_ack, first_request.operation_id, 4), Err(rss_contracts::Error::Conflict));
    assert_eq!(serde_json::to_vec(&queue).unwrap(), before_advance, "rejected transition must preserve old operation and continuation");
    let second_request = queue.advance(&first_ack, id(2), 4).unwrap();
    second_request.continuation.as_ref().unwrap().validate_for(&second_request.subscription, &second_request.bounds).unwrap();
    let persisted = serde_json::to_vec(&queue).unwrap(); let mut queue: JobQueue = serde_json::from_slice(&persisted).unwrap();
    let second_request = queue.start(second_request.operation_id, &reference("executor"), 5).unwrap();
    assert_eq!(second_request.continuation.as_ref().unwrap().token, "page-2");
    *admission.operation.lock().unwrap() = second_request.operation_id;
    queue.begin_commit(second_request.operation_id, &reference("executor"), 6).unwrap();
    owner.0.lock().unwrap().appearances.get_mut("opaque-post").unwrap().first_seen_at = None;
    let second = prepared(&mut owner, &admission, &second_request, Some(query())); let second_page = page(&second_request, &second, false);
    assert_eq!(second.appearance().first_seen_at, None);
    let second_ack = run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &second_request, &second_page, &[second], &BTreeSet::new())).unwrap();
    queue.acknowledge(&second_ack).unwrap(); assert_eq!(queue.jobs()[0].state, JobState::Completed); assert!(second_ack.continuation().is_none());
    assert_eq!(owner.0.lock().unwrap().captures.iter().map(|capture| capture.item_id).collect::<BTreeSet<_>>().len(), 1);
    assert_eq!(checkpoints.state.lock().unwrap().batches.len(), 2);
    assert!(checkpoints.state.lock().unwrap().batches.values().all(|(_, batch, _)| batch.expected_generation == Revision(1)));
}

#[test]
fn wrong_checkpoint_generation_and_pending_never_acknowledge() {
    for pending in [false, true] {
        let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default(); let checkpoints = Checkpoints::new(owner.clone());
        let prepared = prepared(&mut owner, &admission, &request, None); let page = page(&request, &prepared, false);
        { let mut state = checkpoints.state.lock().unwrap(); state.pending = pending; state.wrong_generation = !pending; }
        let result = run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &[prepared], &BTreeSet::new()));
        assert!(matches!(result, Err(rss_contracts::Error::UnknownOutcome { .. }) | Err(rss_contracts::Error::StaleResult)));
        assert_eq!(checkpoints.state.lock().unwrap().lookups, 0);
    }
}

#[test]
fn revocation_expired_attempt_and_stale_executor_prevent_item_effects() {
    for scenario in 0..3 {
        let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default(); let allocator = Allocator(owner.clone());
        match scenario { 0 => admission.allowed.store(false, Ordering::SeqCst), 1 => admission.active.store(false, Ordering::SeqCst), _ => admission.current.lock().unwrap().executor_ref = reference("new-executor") }
        assert!(run(prepare_entry(&mut owner, &admission, &admission, &allocator, &reference("attempt"), &request, &context(&request, 0), "opaque-post", "https://example.org/post", None, |_| {}, &BTreeSet::new())).is_err());
        assert_eq!(owner.0.lock().unwrap().mutations, 0);
    }
}

#[test]
fn normalizer_cannot_retarget_reserved_ids_and_checkpoint_rereads_canonical_records() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default(); let allocator = Allocator(owner.clone());
    assert!(run(prepare_entry(&mut owner, &admission, &admission, &allocator, &reference("attempt"), &request, &context(&request, 0), "opaque-post", "https://example.org/post", None,
        |observation| observation.item_id = id(999), &BTreeSet::new())).is_err());
    assert_eq!(owner.0.lock().unwrap().captures.len(), 1); assert!(owner.0.lock().unwrap().observations.is_empty());
    let prepared = prepared(&mut owner, &admission, &request, None); let page = page(&request, &prepared, false); let checkpoints = Checkpoints::new(owner.clone());
    owner.0.lock().unwrap().observations.clear();
    assert!(run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &[prepared], &BTreeSet::new())).is_err());
    assert_eq!(checkpoints.state.lock().unwrap().calls, 0);
}

#[test]
fn expired_provider_page_preserves_captures_and_cannot_write_a_checkpoint() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default();
    let prepared = prepared(&mut owner, &admission, &request, None); let mut failed = page(&request, &prepared, false);
    failed.entries.clear(); failed.state = AcquisitionState::Expired; failed.coverage = Coverage::Partial;
    let mutations = owner.0.lock().unwrap().mutations; let captures = owner.0.lock().unwrap().captures.clone();
    let checkpoints = Checkpoints::new(owner.clone());
    assert!(run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &failed, &[], &BTreeSet::new())).is_err());
    assert_eq!(owner.0.lock().unwrap().mutations, mutations); assert_eq!(owner.0.lock().unwrap().captures, captures);
    assert_eq!(checkpoints.state.lock().unwrap().calls, 0);
}

#[test]
fn canonical_outcome_references_must_resolve_the_accepted_operations() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default();
    let prepared = prepared(&mut owner, &admission, &request, None); let page = page(&request, &prepared, false);
    let capture_context = context(&request, 1); let observation_context = context(&request, 2);
    let canonical_reference = run(Allocator(owner.clone()).outcome_reference(&capture_context)).unwrap();
    owner.0.lock().unwrap().outcome_refs.insert(canonical_reference, observation_context.operation_id);
    let checkpoints = Checkpoints::new(owner.clone());
    assert!(matches!(run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"), &request, &page, &[prepared], &BTreeSet::new())), Err(rss_contracts::Error::UnknownOutcome { .. })));
    assert_eq!(checkpoints.state.lock().unwrap().calls, 0);
}

#[test]
fn current_authority_is_checked_before_provider_effects() {
    struct Provider;
    impl rss_contracts::ProviderPort for Provider {
        fn capabilities(&self) -> Vec<rss_contracts::Capability> { vec![] }
        fn acquire<'a>(&'a self, _: SourceRequest) -> Pending<'a, AcquisitionPage> { panic!("revoked authority must prevent provider invocation") }
    }
    let request = request(1); let admission = Admission::new(&request); admission.allowed.store(false, Ordering::SeqCst);
    assert!(matches!(run(rss_acquisition::acquire(&Provider, &admission, &admission, &reference("attempt"), request, &BTreeSet::new())), Err(rss_contracts::Error::Denied)));
}
