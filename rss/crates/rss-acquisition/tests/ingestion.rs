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
        partition: AccessPartition { scope: ScopeRef("profile-view".into()), mode: AccessMode::PublicFeed, connection_ref: None },
        executor_ref: reference("executor"), generation: Revision(1), paused: false, common: Extensions::default() },
    operation_id: id(page), continuation: None, bounds: bounds(),
} }
fn number(request: &SourceRequest) -> u64 { request.operation_id.to_string()[24..].parse().unwrap() }
fn context(request: &SourceRequest, step: u64) -> CommandContext { CommandContext { actor: reference("actor"),
    scope: request.subscription.partition.scope.clone(), operation_id: id(1000 + number(request) * 10 + step),
    intent_digest: format!("page-{}-step-{step}", number(request)), expected_revisions: vec![], bounds: request.bounds.clone() } }
fn query() -> IdentityQuery { IdentityQuery { provider: "syndication".into(), namespace: "posts".into(), provider_item_id: "opaque-post".into(), access_partition: ScopeRef("profile-view".into()) } }

#[derive(Default)]
struct Store {
    invalid_item_owner: bool,
    identity_read_override: Option<SourceIdentity>, missing_identity_read: bool,
    denied_identity_read: bool, missing_identity_lookup: bool, identity_reservations: usize,
    captures: Vec<URLCapture>, observations: Vec<MetadataObservation>, streams: Vec<ObservationStream>, identities: Vec<SourceIdentity>,
    outcomes: BTreeMap<OperationId, OperationOutcome>, appearances: BTreeMap<String, SourceAppearance>, mutations: usize,
    outcome_refs: BTreeMap<HostRef, OperationId>,
    items: BTreeMap<ItemId, Item>,
    capture_allocations: BTreeMap<OperationId,(CommandContext,URLCapture)>,
    observation_allocations: BTreeMap<OperationId,(CommandContext,MetadataObservation,Option<InitialObservation>)>,
    identity_allocations: BTreeMap<OperationId,(CommandContext,SourceIdentity)>,
    initial_choices: BTreeMap<OperationId,Option<InitialObservation>>,
    initial_proposals: Vec<Option<InitialObservation>>,
    plans: BTreeMap<OperationId,hsscrap::operations::OperationPlan>,
    receipts: BTreeMap<OperationId,hsscrap::operations::OwnerStepReceipt>,
    clear_at: Option<u8>, lose_initial_reply: bool,
}
#[derive(Clone, Default)]
struct Owner(Arc<Mutex<Store>>);
impl Owner {
    fn planned(mut context:CommandContext,command:OwnerCommand)->CommandContext {
        context.intent_digest.clear();
        hsscrap::operations::plan_operation(context,vec![command],&BTreeSet::new()).unwrap().context
    }
    fn expected(context:&CommandContext,owner:HostRef)->Revision {
        context.expected_revisions.iter().find(|value|value.owner==owner).expect("native owner revision guard").revision
    }
    fn record(store: &mut Store, context: CommandContext, command:OwnerCommand) -> Result<OperationOutcome> {
        use hsscrap::operations::*;
        let plan=plan_operation(context,vec![command],&BTreeSet::new())?;
        assert_eq!(decide_operation(&plan,store.outcomes.get(&plan.context.operation_id),true)?,OperationDecision::Begin);
        let receipt=OwnerStepReceipt {operation_id:plan.context.operation_id,intent_digest:plan.context.intent_digest.clone(),
            command_index:0,owner:reference("item-owner"),receipt:reference(plan.context.operation_id.to_string())};
        let outcome=finish_operation(&plan,std::slice::from_ref(&receipt),std::slice::from_ref(&receipt.owner),vec![],at())?;
        store.plans.insert(outcome.operation_id,plan);store.receipts.insert(outcome.operation_id,receipt);
        store.outcome_refs.insert(reference(format!("canonical-operation-{}", outcome.operation_id)), outcome.operation_id);
        store.outcomes.insert(outcome.operation_id, outcome.clone()); store.mutations += 1; Ok(outcome)
    }
    fn snapshot(store:&Store,item:ItemId)->Result<ItemSnapshot> {
        let mut value=store.items.get(&item).cloned().ok_or(Error::NotFound)?;
        if store.invalid_item_owner {value.owner_scope=ScopeRef(String::new());}
        Ok(ItemSnapshot {item:value,captures:store.captures.iter().filter(|v|v.item_id==item).cloned().collect(),
            observations:store.observations.iter().filter(|v|v.item_id==item).cloned().collect(),members:vec![],
            streams:store.streams.iter().filter(|v|v.item_id==item).cloned().collect()})
    }
    fn edit_selection(&mut self,item:ItemId,selection:Patch<ObservationId>) {
        let revision=self.0.lock().unwrap().items[&item].revision;
        let mut ctx=context(&request(90),revision.0);ctx.expected_revisions=vec![ExpectedRevision{owner:reference(format!("item-{item}")),revision}];
        let patch=ItemPatch {title:Patch::Keep,description:Patch::Keep,cover:Patch::Keep,selected_observation:selection,gallery_observation:Patch::Keep};
        let ctx=Self::planned(ctx,OwnerCommand::ItemPatch{item,patch:patch.clone()});
        self.apply_patch(ctx,item,patch).unwrap();
    }
}
impl ItemOwner for Owner {
    fn capture(&mut self, command: CaptureCommand) -> Result<OperationOutcome> {
        let mut store = self.0.lock().unwrap();
        let capture=command.capture;let item=capture.item_id;
        hsscrap::operations::plan_operation(command.context.clone(),vec![OwnerCommand::Capture{item,capture:capture.clone()}],&BTreeSet::new())?;
        if let Some(old)=store.items.get_mut(&item) {
            if Self::expected(&command.context,reference(format!("item-{item}")))!=old.revision {return Err(Error::Conflict{field:"revision".into()});}
            old.revision=old.revision.next()?;old.updated_at=capture.created_here_at.clone();}
        else {assert!(command.context.expected_revisions.is_empty());store.items.insert(item,Item {item_id:item,owner_scope:ScopeRef("account".into()),revision:Revision(1),first_capture_id:capture.capture_id,
            selected_observation_id:None,gallery_observation_id:None,member_order:vec![],title_override:Override::Inherit,
            description_override:Override::Inherit,cover_override:Override::Inherit,created_at:capture.created_here_at.clone(),updated_at:capture.created_here_at.clone(),
            lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()});}
        store.captures.push(capture.clone());
        let result=Self::record(&mut store,command.context,OwnerCommand::Capture{item,capture});
        let clear=store.clear_at==Some(0);if clear {store.clear_at=None;}drop(store);
        if clear {self.edit_selection(item,Patch::Clear);}result
    }
    fn inspect(&self, context: &CommandContext, item: ItemId) -> Result<ItemSnapshot> {
        let store = self.0.lock().unwrap(); let first = store.captures.iter().find(|capture| capture.item_id == item).ok_or(Error::NotFound)?;
        if first.access_partition != context.scope { return Err(Error::Denied); }
        Self::snapshot(&store,item)
    }
    fn lookup_identity(&self, context: &CommandContext, query: IdentityQuery) -> Result<Option<ItemId>> {
        if query.access_partition != context.scope { return Err(Error::Denied); }
        let store = self.0.lock().unwrap();
        if store.missing_identity_lookup { return Ok(None); }
        Ok(store.identities.iter().find(|value| value.provider == query.provider && value.namespace == query.namespace
            && value.provider_item_id == query.provider_item_id && value.access_partition == context.scope && value.retired_at.is_none()).map(|value| value.item_id))
    }
    fn lookup_url(&self, context: &CommandContext, query: UrlIdentityQuery) -> Result<Vec<ItemId>> {
        query.validate(&context.bounds)?; if query.access_partition != context.scope { return Err(Error::Denied); }
        Ok(self.0.lock().unwrap().captures.iter().filter(|value| value.identity_url == query.identity_url && value.access_partition == context.scope)
            .map(|value| value.item_id).collect::<BTreeSet<_>>().into_iter().collect())
    }
    fn accept_observation(&mut self, context: CommandContext, observation: MetadataObservation) -> Result<OperationOutcome> {
        let mut store=self.0.lock().unwrap();
        hsscrap::operations::plan_operation(context.clone(),vec![OwnerCommand::AcceptObservation{observation:observation.clone()}],&BTreeSet::new())?;
        let stream=store.streams.iter().find(|v|v.stream_id==observation.stream_id).unwrap();
        let baseline=observation.baseline_observation_id.and_then(|id|store.observations.iter().find(|v|v.observation_id==id));
        let accepted=hsscrap::metadata::accept_observation(&store.items[&observation.item_id],stream,observation.clone(),baseline,
            Self::expected(&context,reference(format!("item-{}",observation.item_id))),
            Self::expected(&context,reference(format!("stream-{}",stream.stream_id))),&context.bounds,&BTreeSet::new())?;
        store.items.insert(accepted.item.item_id,accepted.item);
        let stream_id=accepted.stream.stream_id;
        *store.streams.iter_mut().find(|v|v.stream_id==stream_id).unwrap()=accepted.stream;
        store.observations.push(accepted.observation);
        Self::record(&mut store,context,OwnerCommand::AcceptObservation{observation})
    }
    fn accept_initial_observation(&mut self,context:CommandContext,observation:MetadataObservation,initial:InitialObservation)->Result<OperationOutcome> {
        let mut store=self.0.lock().unwrap();let snapshot=Self::snapshot(&store,observation.item_id)?;
        hsscrap::operations::plan_operation(context.clone(),vec![OwnerCommand::AcceptInitialObservation{observation:observation.clone(),initial:initial.clone()}],&BTreeSet::new())?;
        let accepted=hsscrap::metadata::accept_initial_observation(&snapshot,observation.clone(),&initial,
            Self::expected(&context,reference(format!("item-{}",observation.item_id))),
            Self::expected(&context,reference(format!("stream-{}",observation.stream_id))),&context.bounds,&BTreeSet::new())?;
        store.items.insert(accepted.item.item_id,accepted.item);
        let stream_id=accepted.stream.stream_id;
        *store.streams.iter_mut().find(|v|v.stream_id==stream_id).unwrap()=accepted.stream;
        store.observations.push(accepted.observation);
        let outcome=Self::record(&mut store,context,OwnerCommand::AcceptInitialObservation{observation,initial})?;
        if store.lose_initial_reply {store.lose_initial_reply=false;
            let pending=hsscrap::operations::pending_outcome(&store.plans[&outcome.operation_id]);
            store.outcomes.insert(outcome.operation_id,pending);return Err(Error::UnknownOutcome{operation_id:outcome.operation_id});}
        Ok(outcome)
    }
    fn assert_source_identity(&mut self, context: CommandContext, identity: SourceIdentity) -> Result<OperationOutcome> {
        let mut store = self.0.lock().unwrap();
        if identity.access_partition != context.scope || identity.retired_at.is_some()
            || store.identities.iter().any(|value| value.identity_id == identity.identity_id
                || value.retired_at.is_none() && value.provider == identity.provider && value.namespace == identity.namespace
                    && value.provider_item_id == identity.provider_item_id && value.access_partition == identity.access_partition)
            || !store.observations.iter().any(|observation| observation.observation_id == identity.verified_by_observation_id
                && observation.item_id == identity.item_id && observation.access_partition == identity.access_partition
                && observation.provider.as_ref() == Some(&identity.provider) && observation.status.state == AvailabilityState::Available
                && store.streams.iter().any(|stream| stream.stream_id == observation.stream_id && stream.provider_namespace == identity.namespace)) {
            return Err(Error::Conflict { field: "source_identity".into() });
        }
        hsscrap::operations::plan_operation(context.clone(),vec![OwnerCommand::AssertSourceIdentity(identity.clone())],&BTreeSet::new())?;
        if Self::expected(&context,reference(format!("item-{}",identity.item_id)))!=store.items[&identity.item_id].revision {return Err(Error::Conflict{field:"revision".into()});}
        let revision=store.items[&identity.item_id].revision.next()?;
        store.items.get_mut(&identity.item_id).unwrap().revision=revision;
        store.identities.push(identity.clone()); Self::record(&mut store, context,OwnerCommand::AssertSourceIdentity(identity))
    }
    fn inspect_source_identity(&self, context: &CommandContext, identity: IdentityId) -> Result<SourceIdentity> {
        let store = self.0.lock().unwrap();
        if store.denied_identity_read { return Err(Error::Denied); }
        store.identities.iter().find(|value| value.identity_id == identity && value.access_partition == context.scope).cloned().ok_or(Error::NotFound)
    }
    fn operation_outcome(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> {
        let store = self.0.lock().unwrap(); let outcome = store.outcomes.get(&operation).ok_or(Error::NotFound)?;
        if outcome.scope != context.scope { return Err(Error::Denied); } Ok(outcome.clone())
    }
    fn apply_patch(&mut self, context: CommandContext, item: ItemId, patch: ItemPatch) -> Result<OperationOutcome> {
        let mut store=self.0.lock().unwrap();let old=store.items.get_mut(&item).unwrap();
        hsscrap::operations::plan_operation(context.clone(),vec![OwnerCommand::ItemPatch{item,patch:patch.clone()}],&BTreeSet::new())?;
        assert_eq!(context.expected_revisions[0].revision,old.revision);
        match &patch.selected_observation {Patch::Keep=>{},Patch::Clear=>old.selected_observation_id=None,Patch::Set(id)=>old.selected_observation_id=Some(*id)}
        old.revision=old.revision.next()?;old.updated_at=at();
        Self::record(&mut store,context,OwnerCommand::ItemPatch{item,patch})
    }
    fn apply_gallery_snapshot(&mut self, _: CommandContext, _: ItemId, _: ObservationId) -> Result<OperationOutcome> { panic!("fixture has no gallery") }
    fn tombstone(&mut self, _: CommandContext, _: ItemId) -> Result<OperationOutcome> { panic!("provider failure must preserve items") }
}
impl MutationCoordinator for Owner {
    fn commit(&mut self, _: CommandContext, _: Vec<OwnerCommand>) -> Result<OperationOutcome> { panic!("use declared item owner methods") }
    fn lookup(&self, context: &CommandContext, operation: OperationId) -> Result<OperationOutcome> { self.operation_outcome(context, operation) }
    fn recover(&mut self, context: CommandContext) -> Result<OperationOutcome> {
        let mut store=self.0.lock().unwrap();let plan=&store.plans[&context.operation_id];
        assert!(matches!(hsscrap::operations::decide_operation(plan,store.outcomes.get(&context.operation_id),true)?,hsscrap::operations::OperationDecision::Recover(_)));
        let receipt=&store.receipts[&context.operation_id];
        let outcome=hsscrap::operations::finish_operation(plan,std::slice::from_ref(receipt),std::slice::from_ref(&receipt.owner),vec![],at())?;
        store.outcomes.insert(context.operation_id,outcome.clone());Ok(outcome)
    }
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
        Box::pin(async move { let mut store=self.0.0.lock().unwrap();let operation=context(request,1).operation_id;
            if let Some((context,capture))=store.capture_allocations.get(&operation) {return Ok(CaptureAllocation{context:context.clone(),capture:capture.clone(),appearance_ref:reference("appearance")});}
            let mut context = context(request, 1);let capture=URLCapture {
            capture_id: id(100 + number(request)), item_id: resolved.unwrap_or_else(|| id(500)), capture_kind: CaptureKind::SourceIngest,
            access_partition: context.scope.clone(), captured_url: url.original.clone(), original_url: Some(url.original), identity_url: url.identity_url,
            normalization_version: url.normalization_version, captured_at: Some(at()), created_here_at: at(), actor_ref: Some(context.actor.clone()),
            operation_id: context.operation_id, common: Extensions::default() };
            if let Some(item)=store.items.get(&capture.item_id) {context.expected_revisions=vec![ExpectedRevision{owner:reference(format!("item-{}",capture.item_id)),revision:item.revision}];}
            let context=Owner::planned(context,OwnerCommand::Capture{item:capture.item_id,capture:capture.clone()});
            store.capture_allocations.insert(operation,(context.clone(),capture.clone()));
            Ok(CaptureAllocation{capture,context,appearance_ref:reference("appearance")}) })
    }
    fn reserve_observation<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str, snapshot: ItemSnapshot, identity: Option<IdentityQuery>, initial:Option<InitialObservation>) -> Pending<'a, ObservationAllocation> {
        Box::pin(async move {
            let capture_id = id(100 + number(request)); let observation_id = id(200 + number(request)); let stream_id = id(300);
            let mut store = self.0.0.lock().unwrap();
            let operation=context(request,2).operation_id;
            store.initial_proposals.push(initial.clone());
            if let Some((context,observation,initial))=store.observation_allocations.get(&operation) {
                return Ok(ObservationAllocation{context:context.clone(),observation:observation.clone(),initial:initial.clone()});
            }
            let initial=store.initial_choices.entry(operation).or_insert(initial).clone();
            let clear=store.clear_at==Some(1);if clear {store.clear_at=None;}drop(store);
            if clear {self.0.clone().edit_selection(snapshot.item.item_id,Patch::Clear);}
            let mut store=self.0.0.lock().unwrap();
            assert!(store.captures.iter().any(|value| value.capture_id == capture_id), "capture must commit before stream reservation/enrichment");
            if !store.streams.iter().any(|stream| stream.stream_id == stream_id) { store.streams.push(ObservationStream { stream_id,
                item_id: snapshot.item.item_id, producer: "fixture".into(), provider_namespace: identity.as_ref().map(|query| query.namespace.clone()).unwrap_or_else(|| "url-only".into()),
                source_ref: request.subscription.source_ref.clone(), access_partition: request.subscription.partition.scope.clone(), last_issued_sequence: Revision(1),
                last_accepted_sequence: None, latest_observation_id: None, revision: Revision(1), common: Extensions::default() }); }
            else {let stream=store.streams.iter_mut().find(|stream|stream.stream_id==stream_id).unwrap();
                *stream=hsscrap::metadata::reserve_sequence(stream,stream.revision).unwrap();}
            let stream = store.streams.iter_mut().find(|stream| stream.stream_id == stream_id).unwrap();
            let sequence=stream.last_issued_sequence;let baseline=stream.latest_observation_id;
            let mut context=context(request,2);context.expected_revisions=vec![ExpectedRevision{owner:reference(format!("item-{}",snapshot.item.item_id)),revision:snapshot.item.revision},
                ExpectedRevision{owner:reference(format!("stream-{stream_id}")),revision:stream.revision}];
            let observation=MetadataObservation { observation_id, item_id: snapshot.item.item_id,
                stream_id, sequence, baseline_observation_id: baseline, capture_id: Some(capture_id), appearance_ref: Some(reference("appearance")),
                producer: "fixture".into(), access_partition: request.subscription.partition.scope.clone(), observed_at: at(), source_revision: None,
                canonical_url_assertion: None, provider: identity.as_ref().map(|query| query.provider.clone()), host_site: "example.org".into(), title: None,
                description: None, author: None, published_at: None, provider_labels: vec![], gallery_snapshot: None, source_payload_digest: None,
                status: Availability { state: AvailabilityState::Available, checked_at: Some(at()), reason_code: None, retry_after: None }, common: Extensions::default() };
            // This fixture producer already knows its parsed title before reserving native intent.
            let mut populated=observation.clone();populated.title=Some("Actual parsed title".into());
            let command=match &initial {Some(initial)=>OwnerCommand::AcceptInitialObservation{observation:populated,initial:initial.clone()},
                None=>OwnerCommand::AcceptObservation{observation:populated}};
            let context=Owner::planned(context,command);
            store.observation_allocations.insert(operation,(context.clone(),observation.clone(),initial.clone()));
            let clear=store.clear_at==Some(2);if clear {store.clear_at=None;}drop(store);
            if clear {self.0.clone().edit_selection(snapshot.item.item_id,Patch::Clear);}
            Ok(ObservationAllocation{context,observation,initial})
        })
    }
    fn reserve_identity<'a>(&'a self, request: &'a SourceRequest, _: &'a HostRef, _: &'a str,
        current: ItemSnapshot, observation: &'a MetadataObservation, query: IdentityQuery) -> Pending<'a, IdentityAllocation> {
        Box::pin(async move {
            assert!(current.observations.iter().any(|stored| stored == observation), "identity guard must follow observation commit/readback");
            let mut store=self.0.0.lock().unwrap();let operation=context(request,3).operation_id;
            if let Some((context,identity))=store.identity_allocations.get(&operation) {return Ok(IdentityAllocation{context:context.clone(),identity:identity.clone()});}
            store.identity_reservations += 1;
            let identity=SourceIdentity { identity_id: id(400 + number(request)),
                item_id: current.item.item_id, provider: query.provider, namespace: query.namespace, provider_item_id: query.provider_item_id,
                access_partition: query.access_partition, verified_by_observation_id: observation.observation_id, retired_at: None, common: Extensions::default() };
            let mut context=context(request,3);context.expected_revisions=vec![ExpectedRevision{owner:reference(format!("item-{}",current.item.item_id)),revision:current.item.revision}];
            let context=Owner::planned(context,OwnerCommand::AssertSourceIdentity(identity.clone()));
            store.identity_allocations.insert(operation,(context.clone(),identity.clone()));
            Ok(IdentityAllocation{context,identity})
        })
    }
    fn inspect_identity<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, query: &'a IdentityQuery) -> Pending<'a, Option<SourceIdentity>> {
        Box::pin(async move {
            if query.access_partition != request.subscription.partition.scope || execution != &reference("attempt") { return Err(rss_contracts::Error::Denied); }
            let store = self.0.0.lock().unwrap();
            if store.denied_identity_read { return Err(rss_contracts::Error::Denied); }
            if store.missing_identity_read { return Ok(None); }
            if let Some(value) = &store.identity_read_override { return Ok(Some(value.clone())); }
            let mut matching = store.identities.iter().filter(|value| value.provider == query.provider && value.namespace == query.namespace
                && value.provider_item_id == query.provider_item_id && value.access_partition == query.access_partition && value.retired_at.is_none());
            let result = matching.next().cloned();
            if matching.next().is_some() { return Err(rss_contracts::Error::Conflict); }
            Ok(result)
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
    let snapshot=owner.inspect(&context(&request,0),first.entry().capture.item_id).unwrap();
    assert_eq!(snapshot.item.owner_scope,ScopeRef("account".into()));
    assert_eq!(first.entry().capture.access_partition,ScopeRef("profile-view".into()));
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
    let immutable_identity = owner.0.lock().unwrap().identities[0].clone();
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
    { let store = owner.0.lock().unwrap();
        assert_eq!(store.identities, vec![immutable_identity.clone()]);
        assert_eq!(store.observations.len(), 2);
        assert_eq!(store.identity_reservations, 1, "refresh must not allocate or reassert an immutable identity");
    }
    assert_eq!(checkpoints.state.lock().unwrap().batches.len(), 2);
    assert!(checkpoints.state.lock().unwrap().batches.values().all(|(_, batch, _)| batch.expected_generation == Revision(1)));
    { let state = checkpoints.state.lock().unwrap();
        assert_eq!(state.batches[&first_request.operation_id].1.item_owner_outcome_refs.len(), 3);
        assert_eq!(state.batches[&second_request.operation_id].1.item_owner_outcome_refs.len(), 2);
    }
    let mutations = owner.0.lock().unwrap().mutations;
    for changed_verification in [false, true] {
        let mut duplicate = immutable_identity.clone();
        if changed_verification { duplicate.verified_by_observation_id = id(202); }
        assert!(owner.assert_source_identity(context(&second_request, 9), duplicate).is_err());
    }
    let mut duplicate = immutable_identity; duplicate.identity_id = id(999);
    assert!(owner.assert_source_identity(context(&second_request, 9), duplicate).is_err());
    assert_eq!(owner.0.lock().unwrap().mutations, mutations);
}

#[test]
fn same_page_identity_retry_preserves_exact_lost_checkpoint_batch() {
    let request = request(1); let admission = Admission::new(&request); let mut owner = Owner::default();
    let checkpoints = Checkpoints::new(owner.clone()); let allocator = Allocator(owner.clone());
    let first = prepared(&mut owner, &admission, &request, Some(query())); let source_page = page(&request, &first, false);
    checkpoints.state.lock().unwrap().lose_next = true;
    assert!(matches!(run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &request,
        &source_page, &[first], &BTreeSet::new())), Err(rss_contracts::Error::UnknownOutcome { .. })));
    let batch_before = serde_json::to_vec(&checkpoints.state.lock().unwrap().batches[&request.operation_id].1).unwrap();
    let identity_before = owner.0.lock().unwrap().identities.clone();
    let replay = prepared(&mut owner, &admission, &request, Some(query()));
    let replay_page = page(&request, &replay, false);
    assert_eq!(source_page, replay_page);
    run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &request,
        &replay_page, &[replay], &BTreeSet::new())).unwrap();
    let state = checkpoints.state.lock().unwrap();
    assert_eq!(serde_json::to_vec(&state.batches[&request.operation_id].1).unwrap(), batch_before);
    assert_eq!(state.batches[&request.operation_id].1.item_owner_outcome_refs.len(), 3);
    let store = owner.0.lock().unwrap();
    assert_eq!(store.identities, identity_before); assert_eq!(store.observations.len(), 1); assert_eq!(store.mutations, 3);
}

#[test]
fn existing_identity_requires_exact_canonical_tuple_and_original_verification() {
    for scenario in 0..9 {
        let first_request = request(1); let admission = Admission::new(&first_request); let mut owner = Owner::default();
        let first = prepared(&mut owner, &admission, &first_request, Some(query()));
        let original = owner.0.lock().unwrap().identities[0].clone();
        { let mut store = owner.0.lock().unwrap(); let mut changed = original.clone();
            match scenario {
                0 => changed.retired_at = Some(at()),
                1 => changed.verified_by_observation_id = id(999),
                2 => changed.item_id = id(999),
                3 => store.missing_identity_read = true,
                4 => store.missing_identity_lookup = true,
                5 => store.denied_identity_read = true,
                6 => changed.access_partition = ScopeRef("foreign".into()),
                7 => changed.namespace = "different-namespace".into(),
                _ => store.observations[0].status.state = AvailabilityState::Unavailable,
            }
            if matches!(scenario, 0 | 1 | 2 | 6 | 7) { store.identity_read_override = Some(changed); }
        }
        let second_request = request(2); *admission.operation.lock().unwrap() = second_request.operation_id;
        let allocator = Allocator(owner.clone());
        assert!(run(prepare_entry(&mut owner, &admission, &admission, &allocator, &reference("attempt"),
            &second_request, &context(&second_request, 0), "opaque-post", "https://example.org/post", Some(query()), |value|value.title=Some("Actual parsed title".into()), &BTreeSet::new())).is_err(), "scenario {scenario}");
        let store = owner.0.lock().unwrap();
        assert_eq!(store.identities, vec![original]); assert_eq!(store.identity_reservations, 1);
        assert_eq!(first.entry().capture.item_id, store.captures[0].item_id);
    }
}

#[test]
fn checkpoint_rechecks_reused_identity_without_an_old_operation_receipt() {
    for scenario in 0..8 {
        let first_request = request(1); let admission = Admission::new(&first_request); let mut owner = Owner::default();
        let _first = prepared(&mut owner, &admission, &first_request, Some(query()));
        let second_request = request(2); *admission.operation.lock().unwrap() = second_request.operation_id;
        let second = prepared(&mut owner, &admission, &second_request, Some(query())); let source_page = page(&second_request, &second, false);
        { let mut store = owner.0.lock().unwrap();
            // An old identity journal is unnecessary for a new page's read-only identity proof.
            store.outcomes.remove(&context(&first_request, 3).operation_id);
            match scenario {
                0 => store.identities[0].retired_at = Some(at()),
                1 => store.identities[0].verified_by_observation_id = second.entry().observation.observation_id,
                2 => store.identities[0].item_id = id(999),
                3 => store.denied_identity_read = true,
                4 => store.missing_identity_lookup = true,
                5 => { store.observations.remove(0); },
                6 => store.observations[0].status.state = AvailabilityState::Unavailable,
                _ => {},
            }
        }
        let checkpoints = Checkpoints::new(owner.clone());
        let result = run(commit_page(&owner, &admission, &admission, &Allocator(owner.clone()), &checkpoints, &reference("attempt"),
            &second_request, &source_page, &[second], &BTreeSet::new()));
        if scenario == 7 {
            assert!(result.is_ok());
            assert_eq!(checkpoints.state.lock().unwrap().batches[&second_request.operation_id].1.item_owner_outcome_refs.len(), 2);
        } else {
            assert!(result.is_err(), "scenario {scenario}");
            assert_eq!(checkpoints.state.lock().unwrap().calls, 0);
        }
    }
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
    owner.0.lock().unwrap().invalid_item_owner=true;
    assert!(matches!(run(commit_page(&owner,&admission,&admission,&Allocator(owner.clone()),&checkpoints,&reference("attempt"),&request,&page,std::slice::from_ref(&prepared),&BTreeSet::new())),Err(rss_contracts::Error::UnknownOutcome {..})));
    assert_eq!(checkpoints.state.lock().unwrap().calls,0);
    owner.0.lock().unwrap().invalid_item_owner=false;
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

#[test]
fn authority_changes_during_provider_fetch_reject_the_returned_page() {
    struct Provider<'a> { admission: &'a Admission, scenario: u8, called: AtomicBool }
    impl rss_contracts::ProviderPort for Provider<'_> {
        fn capabilities(&self) -> Vec<rss_contracts::Capability> { vec![] }
        fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage> {
            Box::pin(async move {
                self.called.store(true, Ordering::SeqCst);
                match self.scenario {
                    0 => self.admission.allowed.store(false, Ordering::SeqCst),
                    1 => self.admission.active.store(false, Ordering::SeqCst),
                    2 => self.admission.current.lock().unwrap().generation = Revision(2),
                    _ => self.admission.current.lock().unwrap().executor_ref = reference("new-executor"),
                }
                Ok(AcquisitionPage { subscription_ref: request.subscription.subscription_ref, source_ref: request.subscription.source_ref,
                    partition: request.subscription.partition, executor_ref: request.subscription.executor_ref, generation: request.subscription.generation,
                    operation_id: request.operation_id, entries: vec![], state: AcquisitionState::VerifiedEmpty { observed_at: at(),
                        selected_source_identity: "https://example.org/feed".into() }, coverage: Coverage::Complete, continuation: None })
            })
        }
    }
    for scenario in 0..4 {
        let request = request(1); let admission = Admission::new(&request);
        let provider = Provider { admission: &admission, scenario, called: AtomicBool::new(false) };
        let result = run(rss_acquisition::acquire(&provider, &admission, &admission, &reference("attempt"), request, &BTreeSet::new()));
        assert!(provider.called.load(Ordering::SeqCst));
        let expected = match scenario { 0 => rss_contracts::Error::Denied, 1 => rss_contracts::Error::Cancelled, _ => rss_contracts::Error::StaleResult };
        assert_eq!(result, Err(expected));
    }
}


#[test]
fn terminal_empty_document_settles_partial_after_prior_items_without_clearing_them() {
    let first_request = request(1); let admission = Admission::new(&first_request); let mut owner = Owner::default();
    let checkpoints = Checkpoints::new(owner.clone()); let allocator = Allocator(owner.clone());
    let mut queue = JobQueue::new(JobLimits { max_jobs: 1, max_running: 1, max_attempts: 3, timeout_ticks: 10, initial_backoff_ticks: 1, max_backoff_ticks: 4 }).unwrap();
    queue.enqueue(first_request.clone(), 0).unwrap(); queue.start(first_request.operation_id, &reference("executor"), 0).unwrap();
    queue.begin_commit(first_request.operation_id, &reference("executor"), 1).unwrap();
    let first = prepared(&mut owner, &admission, &first_request, None); let first_page = page(&first_request, &first, true);
    let first_ack = run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &first_request, &first_page, &[first], &BTreeSet::new())).unwrap();
    let second_request = queue.advance(&first_ack, id(2), 2).unwrap(); *admission.operation.lock().unwrap() = second_request.operation_id;
    queue.start(second_request.operation_id, &reference("executor"), 3).unwrap(); queue.begin_commit(second_request.operation_id, &reference("executor"), 4).unwrap();
    let preserved = { let store = owner.0.lock().unwrap(); (store.captures.clone(), store.observations.clone(), store.appearances.clone(), store.mutations) };
    let mut terminal = first_page; terminal.operation_id = second_request.operation_id; terminal.entries.clear(); terminal.continuation = None;
    terminal.state = AcquisitionState::Partial { observed_at: at(), reason_code: rss_contracts::TERMINAL_SOURCE_DOCUMENT.into() };
    terminal.coverage = Coverage::Partial;
    let mut fresh = second_request.clone(); fresh.continuation = None;
    assert!(run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &fresh, &terminal, &[], &BTreeSet::new())).is_err());
    let mut wrong = terminal.clone(); wrong.coverage = Coverage::Complete;
    assert!(run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &second_request, &wrong, &[], &BTreeSet::new())).is_err());
    let mut wrong = terminal.clone(); wrong.continuation = second_request.continuation.clone();
    assert!(run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &second_request, &wrong, &[], &BTreeSet::new())).is_err());
    let mut wrong = terminal.clone(); wrong.state = AcquisitionState::Partial { observed_at: at(), reason_code: "challenge".into() };
    assert!(run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &second_request, &wrong, &[], &BTreeSet::new())).is_err());
    assert_eq!(checkpoints.state.lock().unwrap().calls, 1);
    let acknowledgement = run(commit_page(&owner, &admission, &admission, &allocator, &checkpoints, &reference("attempt"), &second_request, &terminal, &[], &BTreeSet::new())).unwrap();
    queue.acknowledge(&acknowledgement).unwrap(); assert_eq!(queue.jobs()[0].state, JobState::Completed);
    let store = owner.0.lock().unwrap(); assert_eq!((store.captures.clone(), store.observations.clone(), store.appearances.clone(), store.mutations), preserved);
    let state = checkpoints.state.lock().unwrap(); let (_, batch, _) = state.batches.get(&second_request.operation_id).unwrap();
    assert!(batch.appearances.is_empty()); assert!(batch.item_owner_outcome_refs.is_empty()); assert!(batch.continuation.is_none());
    assert!(matches!(&batch.state, AcquisitionState::Partial { reason_code, .. } if reason_code == rss_contracts::TERMINAL_SOURCE_DOCUMENT));
}

#[test]
fn initial_source_selection_uses_native_receipts_and_page85_replay() {
    let mut request=request(1);request.bounds.max_page_items=85;
    let admission=Admission::new(&request);let mut owner=Owner::default();
    let first=prepared(&mut owner,&admission,&request,Some(query()));
    let item=first.entry().capture.item_id;let observation=first.entry().observation.observation_id;
    let before=owner.inspect(&context(&request,0),item).unwrap();
    assert_eq!(before.item.selected_observation_id,Some(observation));
    assert_eq!(before.item.revision,Revision(3));
    {let store=owner.0.lock().unwrap();
        assert!(store.initial_choices[&context(&request,2).operation_id].is_some());
        assert!(matches!(store.plans[&context(&request,2).operation_id].commands.as_slice(),[OwnerCommand::AcceptInitialObservation{..}]));
        assert_eq!(store.outcomes.len(),3);
        for (operation,plan) in &store.plans {assert_eq!(plan.context.bounds.max_page_items,85);
            let receipt=&store.receipts[operation];
            assert_eq!(hsscrap::operations::finish_operation(plan,std::slice::from_ref(receipt),std::slice::from_ref(&receipt.owner),vec![],at()).unwrap(),store.outcomes[operation]);}
    }
    let checkpoints=Checkpoints::new(owner.clone());let source_page=page(&request,&first,false);
    run(commit_page(&owner,&admission,&admission,&Allocator(owner.clone()),&checkpoints,&reference("attempt"),&request,&source_page,&[first],&BTreeSet::new())).unwrap();
    let replay=prepared(&mut owner,&admission,&request,Some(query()));
    assert_eq!(page(&request,&replay,false),source_page);
    assert_eq!(owner.inspect(&context(&request,0),item).unwrap(),before);
    run(commit_page(&owner,&admission,&admission,&Allocator(owner.clone()),&checkpoints,&reference("attempt"),&request,&source_page,&[replay],&BTreeSet::new())).unwrap();
    let state=checkpoints.state.lock().unwrap();assert_eq!(state.batches.len(),1);
    assert_eq!(state.batches[&request.operation_id].1.item_owner_outcome_refs.len(),3);
    assert_eq!(owner.0.lock().unwrap().mutations,3);
    drop(state);
    owner.edit_selection(item,Patch::Clear);
    let cleared=owner.inspect(&context(&request,0),item).unwrap();let mutations=owner.0.lock().unwrap().mutations;
    let replay=prepared(&mut owner,&admission,&request,Some(query()));
    run(commit_page(&owner,&admission,&admission,&Allocator(owner.clone()),&checkpoints,&reference("attempt"),&request,&source_page,&[replay],&BTreeSet::new())).unwrap();
    assert_eq!(owner.inspect(&context(&request,0),item).unwrap(),cleared);
    assert_eq!(cleared.item.selected_observation_id,None);
    assert_eq!(owner.0.lock().unwrap().mutations,mutations);
    assert_eq!(checkpoints.state.lock().unwrap().batches[&request.operation_id].1.item_owner_outcome_refs.len(),3);
}

#[test]
fn explicit_clear_before_guard_stream_and_stage_never_reselects() {
    for position in 0..3 {
        let request=request(1);let admission=Admission::new(&request);let mut owner=Owner::default();
        owner.0.lock().unwrap().clear_at=Some(position);
        let allocator=Allocator(owner.clone());
        let result=run(prepare_entry(&mut owner,&admission,&admission,&allocator,&reference("attempt"),&request,&context(&request,0),
            "opaque-post","https://example.org/post",Some(query()),|value|value.title=Some("Actual parsed title".into()),&BTreeSet::new()));
        if position==0 {assert!(result.is_ok());} else {assert!(result.is_err());}
        let snapshot=owner.inspect(&context(&request,0),id(500)).unwrap();
        assert_eq!(snapshot.item.selected_observation_id,None);
        let choice=owner.0.lock().unwrap().initial_choices[&context(&request,2).operation_id].clone();
        assert_eq!(choice.is_some(),position!=0);
        if position!=0 {
            assert!(snapshot.observations.is_empty());
            let mutations=owner.0.lock().unwrap().mutations;
            assert!(run(prepare_entry(&mut owner,&admission,&admission,&allocator,&reference("attempt"),&request,&context(&request,0),
                "opaque-post","https://example.org/post",Some(query()),|value|value.title=Some("Actual parsed title".into()),&BTreeSet::new())).is_err());
            assert_eq!(owner.0.lock().unwrap().mutations,mutations);
            assert_eq!(owner.inspect(&context(&request,0),id(500)).unwrap().item.selected_observation_id,None);
            assert_eq!(owner.0.lock().unwrap().initial_choices[&context(&request,2).operation_id],choice);
        }
    }
}

#[test]
fn pending_initial_receipt_recovery_after_user_clear_keeps_original_guard_and_intent() {
    let request=request(1);let admission=Admission::new(&request);let mut owner=Owner::default();let allocator=Allocator(owner.clone());
    owner.0.lock().unwrap().lose_initial_reply=true;
    assert!(run(prepare_entry(&mut owner,&admission,&admission,&allocator,&reference("attempt"),&request,&context(&request,0),
        "opaque-post","https://example.org/post",Some(query()),|value|value.title=Some("Actual parsed title".into()),&BTreeSet::new())).is_err());
    let operation=context(&request,2).operation_id;
    let initial={let store=owner.0.lock().unwrap();
        assert_eq!(store.outcomes[&operation].outcome,OutcomeState::Pending);
        assert_eq!(store.items[&id(500)].selected_observation_id,Some(id(201)));
        assert_eq!(store.observations.len(),1);assert!(store.receipts.contains_key(&operation));
        store.observation_allocations[&operation].clone()};
    owner.edit_selection(id(500),Patch::Clear);
    let replay=prepared(&mut owner,&admission,&request,Some(query()));
    let snapshot=owner.inspect(&context(&request,0),id(500)).unwrap();
    assert_eq!(snapshot.item.selected_observation_id,None);
    assert_eq!(snapshot.observations.len(),1);
    {let store=owner.0.lock().unwrap();
        assert_eq!(store.observation_allocations[&operation],initial);
        assert!(store.initial_proposals.first().unwrap().is_some());
        assert!(store.initial_proposals.last().unwrap().is_none());
        assert_eq!(store.outcomes[&operation].outcome,OutcomeState::Committed);
    }
    let checkpoints=Checkpoints::new(owner.clone());let source_page=page(&request,&replay,false);
    run(commit_page(&owner,&admission,&admission,&allocator,&checkpoints,&reference("attempt"),&request,&source_page,&[replay],&BTreeSet::new())).unwrap();
    let mutations=owner.0.lock().unwrap().mutations;
    let replay=prepared(&mut owner,&admission,&request,Some(query()));
    run(commit_page(&owner,&admission,&admission,&allocator,&checkpoints,&reference("attempt"),&request,&source_page,&[replay],&BTreeSet::new())).unwrap();
    assert_eq!(owner.0.lock().unwrap().mutations,mutations);
    assert_eq!(owner.inspect(&context(&request,0),id(500)).unwrap(),snapshot);
    assert_eq!(checkpoints.state.lock().unwrap().batches[&request.operation_id].1.item_owner_outcome_refs.len(),3);
}

#[test]
fn legacy_item_and_reused_other_stream_or_clear_keep_their_selection() {
    for legacy in [true,false] {
        for other_stream in [false,true] {
            let first_request=request(1);let admission=Admission::new(&first_request);let mut owner=Owner::default();
            let expected=if legacy {
                let allocation=run(Allocator(owner.clone()).allocate_capture(&request(8),&reference("attempt"),"legacy",None,
                    hsscrap::identity::normalize_url("https://example.org/post",&bounds()).unwrap())).unwrap();
                let mut capture=allocation.capture;capture.capture_kind=CaptureKind::LegacyMigration;
                let context=Owner::planned(allocation.context,OwnerCommand::Capture{item:capture.item_id,capture:capture.clone()});
                owner.capture(CaptureCommand{context,capture,item_id:id(500)}).unwrap();None
            } else {
                let first=prepared(&mut owner,&admission,&first_request,Some(query()));
                if other_stream {
                    let mut observation=first.entry().observation.clone();observation.observation_id=id(777);observation.stream_id=id(777);
                    observation.sequence=Revision(1);observation.baseline_observation_id=None;
                    let mut stream=owner.0.lock().unwrap().streams[0].clone();stream.stream_id=id(777);stream.last_issued_sequence=Revision(1);
                    stream.last_accepted_sequence=None;stream.latest_observation_id=None;stream.revision=Revision(1);
                    owner.0.lock().unwrap().streams.push(stream);
                    let revision=owner.0.lock().unwrap().items[&id(500)].revision;
                    let mut context=context(&request(80),0);context.expected_revisions=vec![ExpectedRevision{owner:reference(format!("item-{}",id::<ItemId>(500))),revision},
                        ExpectedRevision{owner:reference(format!("stream-{}",id::<StreamId>(777))),revision:Revision(1)}];
                    let context=Owner::planned(context,OwnerCommand::AcceptObservation{observation:observation.clone()});
                    owner.accept_observation(context,observation).unwrap();
                    owner.edit_selection(id(500),Patch::Set(id(777)));Some(id(777))
                } else {owner.edit_selection(id(500),Patch::Clear);None}
            };
            let next_request=if legacy {first_request}else{request(2)};
            *admission.operation.lock().unwrap()=next_request.operation_id;
            let entry=prepared(&mut owner,&admission,&next_request,Some(query()));
            assert_eq!(owner.inspect(&context(&next_request,0),entry.entry().capture.item_id).unwrap().item.selected_observation_id,expected);
            assert!(owner.0.lock().unwrap().initial_choices[&context(&next_request,2).operation_id].is_none());
        }
    }
}

#[test]
fn versioned_appearance_keys_share_provider_identity_and_preserve_capture_history() {
    fn prepare_version(owner: &mut Owner, request: &SourceRequest, entry: &str, url: &str,
        identity: IdentityQuery) -> rss_contracts::Result<PreparedEntry> {
        let admission = Admission::new(request);
        let allocator = Allocator(owner.clone());
        run(prepare_entry(owner, &admission, &admission, &allocator,
            &reference("attempt"), request, &context(request, 0), entry, url, Some(identity),
            |observation| observation.title = Some("Actual parsed title".into()), &BTreeSet::new()))
    }
    let mut owner = Owner::default();
    let identity = IdentityQuery { provider: "arxiv".into(), namespace: "paper".into(),
        provider_item_id: "2408.12345".into(), access_partition: ScopeRef("profile-view".into()) };
    let first_request = request(1);
    let first = prepare_version(&mut owner, &first_request, "http://arxiv.org/abs/2408.12345v1",
        "https://example.org/paper/v1", identity.clone()).unwrap();
    let mutations = owner.0.lock().unwrap().mutations;
    let replay = prepare_version(&mut owner, &first_request, "http://arxiv.org/abs/2408.12345v1",
        "https://example.org/paper/v1", identity.clone()).unwrap();
    assert_eq!(replay.entry(), first.entry());
    assert_eq!(owner.0.lock().unwrap().mutations, mutations);
    let second = prepare_version(&mut owner, &request(2), "http://arxiv.org/abs/2408.12345v2",
        "https://example.org/paper/v2", identity.clone()).unwrap();
    assert_eq!(first.entry().capture.item_id, second.entry().capture.item_id);
    assert_eq!(first.entry().provider_entry_key, "http://arxiv.org/abs/2408.12345v1");
    assert_eq!(second.entry().provider_entry_key, "http://arxiv.org/abs/2408.12345v2");
    assert_ne!(first.entry().observation.observation_id, second.entry().observation.observation_id);
    {
        let stored = owner.0.lock().unwrap();
        assert_eq!(stored.items.len(), 1);
        assert_eq!(stored.captures.len(), 2);
        assert_eq!(stored.observations.len(), 2);
        assert_eq!(stored.identities.len(), 1);
        assert_eq!(stored.identities[0].provider_item_id, "2408.12345");
        assert!(stored.captures.contains(&first.entry().capture));
        assert!(stored.captures.contains(&second.entry().capture));
        assert!(stored.observations.contains(&first.entry().observation));
        assert!(stored.observations.contains(&second.entry().observation));
    }
    let mutations = owner.0.lock().unwrap().mutations;
    let mut foreign = identity.clone();
    foreign.access_partition = ScopeRef("another-profile".into());
    assert!(matches!(prepare_version(&mut owner, &request(3), "version-three",
        "https://example.org/paper/v3", foreign), Err(rss_contracts::Error::Denied)));
    let mut empty = identity;
    empty.provider_item_id.clear();
    assert!(prepare_version(&mut owner, &request(3), "version-three",
        "https://example.org/paper/v3", empty).is_err());
    assert_eq!(owner.0.lock().unwrap().mutations, mutations);
}
