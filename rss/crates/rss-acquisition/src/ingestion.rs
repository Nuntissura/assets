use std::collections::{BTreeMap, BTreeSet};
use hsscrap::{identity::NormalizedCaptureUrl, ports::{CommandContext, IdentityQuery, ItemOwner, ItemSnapshot, MutationCoordinator},
    CaptureKind, MetadataObservation, OperationOutcome, SourceIdentity, URLCapture, Validate};
use rss_contracts::*;
use crate::{authority::{authorize_current, AcquisitionOwner}, capture::{committed, persist_capture, reconcile, same_outcome, validate_context}, invalid};

pub struct CaptureAllocation {
    pub context: CommandContext,
    pub capture: URLCapture,
    pub appearance_ref: HostRef,
}
pub struct IdentityAllocation { pub context: CommandContext, pub identity: SourceIdentity }
pub struct ObservationAllocation {
    pub context: CommandContext,
    pub observation: MetadataObservation,
}

/// The host durably binds child operation/record allocations to (page operation, entry key).
/// Retries return the same complete allocation payload and contexts, including reserved stream sequence.
/// Allocate no provider-derived UUIDs.
/// Observation reservation occurs only after the supplied capture snapshot has committed.
/// Each actual owner mutation must atomically recheck the parent attempt, current grants and generation.
pub trait EntryAllocator: Send + Sync {
    fn allocate_capture<'a>(&'a self, request: &'a SourceRequest, execution_ref: &'a HostRef,
        entry_key: &'a str, resolved_item: Option<ItemId>, url: NormalizedCaptureUrl) -> Pending<'a, CaptureAllocation>;
    fn reserve_observation<'a>(&'a self, request: &'a SourceRequest, execution_ref: &'a HostRef,
        entry_key: &'a str, capture: ItemSnapshot, identity: Option<IdentityQuery>) -> Pending<'a, ObservationAllocation>;
    /// Resolve the identity assertion's revision guard from the snapshot after observation commitment.
    fn reserve_identity<'a>(&'a self, request: &'a SourceRequest, execution_ref: &'a HostRef, entry_key: &'a str,
        current: ItemSnapshot, observation: &'a MetadataObservation, identity: IdentityQuery) -> Pending<'a, IdentityAllocation>;
    /// Read the currently authorized canonical appearance, including unknown historical timestamps.
    fn inspect_appearance<'a>(&'a self, request: &'a SourceRequest, entry_key: &'a str) -> Pending<'a, Option<SourceAppearance>>;
    /// Resolve the canonical operation journal record, not an individual owner-step receipt.
    fn outcome_reference<'a>(&'a self, context: &'a CommandContext) -> Pending<'a, HostRef>;
    fn inspect_outcome<'a>(&'a self, scope: &'a ScopeRef, reference: &'a HostRef) -> Pending<'a, OperationOutcome>;
}

/// Constructed only after canonical owner readbacks. This is not a persistence or authorization token.
pub struct PreparedEntry {
    entry: ObservedEntry,
    appearance: SourceAppearance,
    capture_context: CommandContext,
    observation_context: CommandContext,
    identity: Option<IdentityAllocation>,
    receipts: Vec<HostRef>,
    outcome_refs: Vec<HostRef>,
}
impl PreparedEntry {
    pub fn entry(&self) -> &ObservedEntry { &self.entry }
    pub fn appearance(&self) -> &SourceAppearance { &self.appearance }
}

fn context_for(context: &CommandContext, request: &SourceRequest, actor: &HostRef) -> Result<()> {
    validate_context(context, &request.subscription.partition.scope)?;
    if context.bounds != request.bounds || &context.actor != actor || context.operation_id == request.operation_id {
        return Err(invalid("allocation.context"));
    }
    Ok(())
}

fn same_observation_binding(before: &MetadataObservation, after: &MetadataObservation) -> bool {
    before.observation_id == after.observation_id && before.item_id == after.item_id
        && before.stream_id == after.stream_id && before.sequence == after.sequence
        && before.baseline_observation_id == after.baseline_observation_id && before.capture_id == after.capture_id
        && before.appearance_ref == after.appearance_ref && before.producer == after.producer
        && before.access_partition == after.access_partition && before.observed_at == after.observed_at
}

fn read_observation(owner: &impl ItemOwner, context: &CommandContext, request: &SourceRequest,
    entry: &ObservedEntry, namespace: Option<&str>, supported: &BTreeSet<String>) -> Result<()> {
    let snapshot = owner.inspect(context, entry.capture.item_id)?;
    snapshot.item.validate(&request.bounds, supported)?;
    if snapshot.item.item_id != entry.capture.item_id || snapshot.item.owner_scope.0.is_empty()
        || snapshot.item.lifecycle != hsscrap::Lifecycle::Active
        || !snapshot.captures.iter().any(|value| value == &entry.capture)
        || !snapshot.observations.iter().any(|value| value == &entry.observation) {
        return Err(Error::UnknownOutcome { operation_id: context.operation_id });
    }
    let stream = snapshot.streams.iter().find(|stream| stream.stream_id == entry.observation.stream_id).ok_or(Error::StaleResult)?;
    stream.validate(&request.bounds, supported)?;
    if stream.item_id != entry.capture.item_id || stream.access_partition != request.subscription.partition.scope
        || stream.source_ref != request.subscription.source_ref || stream.producer != entry.observation.producer
        || namespace.is_some_and(|namespace| stream.provider_namespace != namespace)
        || stream.last_accepted_sequence != Some(entry.observation.sequence)
        || stream.latest_observation_id != Some(entry.observation.observation_id) { return Err(Error::StaleResult); }
    Ok(())
}

fn read_identity(owner: &impl ItemOwner, allocation: &IdentityAllocation, supported: &BTreeSet<String>) -> Result<()> {
    committed(owner, &allocation.context)?;
    let stored = owner.inspect_source_identity(&allocation.context, allocation.identity.identity_id)?;
    stored.validate(&allocation.context.bounds, supported)?;
    let query = IdentityQuery { provider: stored.provider.clone(), namespace: stored.namespace.clone(),
        provider_item_id: stored.provider_item_id.clone(), access_partition: stored.access_partition.clone() };
    if stored != allocation.identity || owner.lookup_identity(&allocation.context, query)? != Some(stored.item_id) {
        return Err(Error::UnknownOutcome { operation_id: allocation.context.operation_id });
    }
    Ok(())
}

fn outcome_receipts(outcomes: &[OperationOutcome], bounds: &Bounds) -> Result<Vec<HostRef>> {
    let receipts: BTreeSet<_> = outcomes.iter().flat_map(|outcome| outcome.owner_receipt_refs.iter().cloned()).collect();
    if receipts.is_empty() || receipts.len() > bounds.max_collection_items { return Err(Error::LimitExceeded { field: "owner_receipts".into() }); }
    Ok(receipts.into_iter().collect())
}

async fn canonical_outcome_ref(allocator: &impl EntryAllocator, context: &CommandContext, expected: &OperationOutcome) -> Result<HostRef> {
    let reference = allocator.outcome_reference(context).await?;
    for value in [&reference.host_instance_id, &reference.owner_kind, &reference.native_id] {
        if value.is_empty() { return Err(invalid("owner_outcome_ref")); }
        context.bounds.text("owner_outcome_ref", value, context.bounds.max_text_bytes)?;
    }
    let actual = allocator.inspect_outcome(&context.scope, &reference).await?;
    if !same_outcome(&actual, expected, &context.bounds)? { return Err(Error::UnknownOutcome { operation_id: context.operation_id }); }
    Ok(reference)
}

/// Provider constructors inject the allocator and normalizer; ProviderPort still returns AcquisitionPage.
/// Fill only metadata on the reserved observation. Changing canonical identity or sequence is rejected.
pub async fn prepare_entry<O: ItemOwner + MutationCoordinator>(owner: &mut O, authority: &impl AuthorityPort,
    execution: &impl AcquisitionOwner, allocator: &impl EntryAllocator, execution_ref: &HostRef,
    request: &SourceRequest, lookup_context: &CommandContext, entry_key: &str, original_url: &str,
    provider_identity: Option<IdentityQuery>, populate: impl FnOnce(&mut MetadataObservation),
    supported: &BTreeSet<String>) -> Result<PreparedEntry> {
    authorize_current(authority, execution, request, execution_ref).await?;
    validate_context(lookup_context, &request.subscription.partition.scope)?;
    if lookup_context.bounds != request.bounds || entry_key.is_empty() { return Err(invalid("entry.allocation")); }
    request.bounds.text("entry_key", entry_key, request.bounds.max_text_bytes)?;
    if provider_identity.as_ref().is_some_and(|query| query.provider_item_id != entry_key) { return Err(invalid("entry.identity_key")); }
    let resolved = crate::identity::resolve_capture_identity(owner, lookup_context, original_url, provider_identity.as_ref())?;
    let normalized = hsscrap::identity::normalize_url(original_url, &request.bounds)?;
    let allocation = allocator.allocate_capture(request, execution_ref, entry_key, resolved, normalized.clone()).await?;
    context_for(&allocation.context, request, &lookup_context.actor)?;
    let capture = allocation.capture;
    if capture.capture_kind != CaptureKind::SourceIngest || capture.captured_url != original_url
        || capture.identity_url != normalized.identity_url || capture.normalization_version != normalized.normalization_version
        || resolved.is_some_and(|item| item != capture.item_id) { return Err(invalid("capture.allocation")); }
    authorize_current(authority, execution, request, execution_ref).await?;
    if crate::identity::resolve_capture_identity(owner, lookup_context, original_url, provider_identity.as_ref())?.is_some_and(|item| item != capture.item_id) {
        return Err(Error::Conflict);
    }
    let capture_outcome = persist_capture(owner, allocation.context.clone(), capture.clone(), supported)?;
    let snapshot = owner.inspect(&allocation.context, capture.item_id)?;
    authorize_current(authority, execution, request, execution_ref).await?;
    let mut reserved = allocator.reserve_observation(request, execution_ref, entry_key, snapshot, provider_identity.clone()).await?;
    context_for(&reserved.context, request, &lookup_context.actor)?;
    if reserved.context.operation_id == allocation.context.operation_id { return Err(invalid("entry.operation_reuse")); }
    let before = reserved.observation.clone();
    populate(&mut reserved.observation);
    if !same_observation_binding(&before, &reserved.observation) || reserved.observation.item_id != capture.item_id
        || reserved.observation.capture_id != Some(capture.capture_id)
        || reserved.observation.appearance_ref.as_ref() != Some(&allocation.appearance_ref) { return Err(invalid("observation.reservation")); }
    let entry = ObservedEntry { provider_entry_key: entry_key.into(), capture, observation: reserved.observation };
    entry.validate(&request.bounds, supported)?;
    if provider_identity.as_ref().is_some_and(|query| entry.observation.provider.as_ref() != Some(&query.provider)) {
        return Err(invalid("observation.provider"));
    }
    authorize_current(authority, execution, request, execution_ref).await?;
    let observation_outcome = reconcile(owner, &reserved.context, |owner| owner.accept_observation(reserved.context.clone(), entry.observation.clone()))?;
    read_observation(owner, &reserved.context, request, &entry, provider_identity.as_ref().map(|query| query.namespace.as_str()), supported)?;
    let mut outcomes = vec![capture_outcome, observation_outcome];
    let identity = if let Some(query) = provider_identity {
        authorize_current(authority, execution, request, execution_ref).await?;
        let current = owner.inspect(&reserved.context, entry.capture.item_id)?;
        let identity = allocator.reserve_identity(request, execution_ref, entry_key, current, &entry.observation, query.clone()).await?;
        context_for(&identity.context, request, &lookup_context.actor)?;
        identity.identity.validate(&request.bounds, supported)?;
        if identity.context.operation_id == reserved.context.operation_id || identity.context.operation_id == allocation.context.operation_id
            || identity.identity.item_id != entry.capture.item_id || identity.identity.access_partition != request.subscription.partition.scope
            || identity.identity.verified_by_observation_id != entry.observation.observation_id || identity.identity.retired_at.is_some()
            || identity.identity.provider != query.provider || identity.identity.namespace != query.namespace
            || identity.identity.provider_item_id != query.provider_item_id {
            return Err(invalid("identity.reservation"));
        }
        authorize_current(authority, execution, request, execution_ref).await?;
        outcomes.push(reconcile(owner, &identity.context, |owner| owner.assert_source_identity(identity.context.clone(), identity.identity.clone()))?);
        read_identity(owner, &identity, supported)?;
        Some(identity)
    } else { None };
    let previous = allocator.inspect_appearance(request, entry_key).await?;
    let appearance = if let Some(mut previous) = previous {
        previous.validate(&request.bounds, supported)?;
        if previous.appearance_ref != allocation.appearance_ref || previous.item_id != entry.capture.item_id
            || previous.source_ref != request.subscription.source_ref || previous.access_partition != request.subscription.partition.scope
            || previous.provider_entry_key != entry_key { return Err(Error::Conflict); }
        previous.last_seen_at = Some(entry.observation.observed_at.clone());
        previous.latest_observation_id = Some(entry.observation.observation_id);
        previous.source_state = SourceState::Active;
        previous
    } else {
        SourceAppearance { appearance_ref: allocation.appearance_ref, item_id: entry.capture.item_id,
            source_ref: request.subscription.source_ref.clone(), access_partition: request.subscription.partition.scope.clone(),
            provider_entry_key: entry_key.into(), first_seen_at: Some(entry.observation.observed_at.clone()),
            last_seen_at: Some(entry.observation.observed_at.clone()), latest_observation_id: Some(entry.observation.observation_id),
            source_state: SourceState::Active, common: Extensions::default() }
    };
    appearance.validate(&request.bounds, supported)?;
    let mut outcome_refs = vec![canonical_outcome_ref(allocator, &allocation.context, &outcomes[0]).await?,
        canonical_outcome_ref(allocator, &reserved.context, &outcomes[1]).await?];
    if let Some(identity) = &identity { outcome_refs.push(canonical_outcome_ref(allocator, &identity.context, &outcomes[2]).await?); }
    Ok(PreparedEntry { entry, appearance, capture_context: allocation.context, observation_context: reserved.context,
        identity, receipts: outcome_receipts(&outcomes, &request.bounds)?, outcome_refs })
}

pub struct AcknowledgedPage { request: SourceRequest, receipt_ref: HostRef, continuation: Option<Continuation> }
impl AcknowledgedPage {
    pub fn receipt_ref(&self) -> &HostRef { &self.receipt_ref }
    pub fn request(&self) -> &SourceRequest { &self.request }
    pub fn continuation(&self) -> Option<&Continuation> { self.continuation.as_ref() }
}

/// Acknowledgement requires a successful exact full-batch commit AND matching canonical receipt lookup.
/// A lost/error reply or Pending never authorizes lookup-only acknowledgement; retry the same full batch.
pub async fn commit_page(owner: &impl ItemOwner, authority: &impl AuthorityPort, execution: &impl AcquisitionOwner,
    allocator: &impl EntryAllocator, checkpoints: &impl CheckpointOwner, execution_ref: &HostRef, request: &SourceRequest,
    page: &AcquisitionPage, prepared: &[PreparedEntry], supported: &BTreeSet<String>) -> Result<AcknowledgedPage> {
    authorize_current(authority, execution, request, execution_ref).await?;
    page.validate(&request.subscription, &request.bounds, supported)?;
    if page.operation_id != request.operation_id || prepared.len() != page.entries.len() { return Err(Error::StaleResult); }
    let terminal_document = page.is_terminal_partial_document() && request.continuation.is_some();
    if !matches!(page.state, AcquisitionState::Ready { .. } | AcquisitionState::Partial { .. } | AcquisitionState::VerifiedEmpty { .. })
        || page.entries.is_empty() && !matches!(page.state, AcquisitionState::VerifiedEmpty { .. }) && !terminal_document {
        return Err(invalid("page.unverified_progress"));
    }
    let entries: BTreeMap<_, _> = prepared.iter().map(|prepared| (&prepared.entry.provider_entry_key, prepared)).collect();
    if entries.len() != prepared.len() { return Err(invalid("prepared.duplicate_key")); }
    let mut outcome_refs = BTreeSet::new();
    let mut appearances = Vec::with_capacity(prepared.len());
    for entry in &page.entries {
        let prepared = entries.get(&entry.provider_entry_key).ok_or_else(|| invalid("prepared.missing"))?;
        if &prepared.entry != entry { return Err(Error::Conflict); }
        let mut outcomes = vec![committed(owner, &prepared.capture_context)?, committed(owner, &prepared.observation_context)?];
        read_observation(owner, &prepared.observation_context, request, entry, prepared.identity.as_ref().map(|identity| identity.identity.namespace.as_str()), supported)?;
        if let Some(identity) = &prepared.identity {
            read_identity(owner, identity, supported)?;
            outcomes.push(committed(owner, &identity.context)?);
        }
        if outcome_receipts(&outcomes, &request.bounds)? != prepared.receipts { return Err(Error::Conflict); }
        if outcomes.len() != prepared.outcome_refs.len() { return Err(Error::Conflict); }
        for (outcome, reference) in outcomes.iter().zip(&prepared.outcome_refs) {
            let actual = allocator.inspect_outcome(&request.subscription.partition.scope, reference).await?;
            if !same_outcome(&actual, outcome, &request.bounds)? {
                return Err(Error::UnknownOutcome { operation_id: outcome.operation_id });
            }
        }
        outcome_refs.extend(prepared.outcome_refs.iter().cloned());
        appearances.push(prepared.appearance.clone());
    }
    if outcome_refs.len() > request.bounds.max_collection_items { return Err(Error::LimitExceeded { field: "batch.owner_outcomes".into() }); }
    let batch = BatchCommit { operation_id: request.operation_id, subscription_ref: request.subscription.subscription_ref.clone(),
        expected_generation: request.subscription.generation, item_owner_outcome_refs: outcome_refs.into_iter().collect(),
        appearances, continuation: page.continuation.clone(), state: page.state.clone() };
    if serde_json::to_vec(&batch).map_err(|_| invalid("batch"))?.len() > request.bounds.max_operation_bytes {
        return Err(Error::LimitExceeded { field: "batch".into() });
    }
    authorize_current(authority, execution, request, execution_ref).await?;
    let scope = &request.subscription.partition.scope;
    let committed = checkpoints.commit(scope, batch).await?;
    let CommitOutcome::Committed { receipt_ref, generation } = &committed else {
        return Err(match committed { CommitOutcome::Conflict => Error::Conflict, _ => Error::UnknownOutcome { operation_id: request.operation_id } });
    };
    if *generation != request.subscription.generation { return Err(Error::StaleResult); }
    for value in [&receipt_ref.host_instance_id, &receipt_ref.owner_kind, &receipt_ref.native_id] {
        if value.is_empty() { return Err(invalid("checkpoint.receipt")); }
        request.bounds.text("checkpoint.receipt", value, request.bounds.max_text_bytes)?;
    }
    if checkpoints.lookup(scope, request.operation_id).await? != committed { return Err(Error::UnknownOutcome { operation_id: request.operation_id }); }
    Ok(AcknowledgedPage { request: request.clone(), receipt_ref: receipt_ref.clone(), continuation: page.continuation.clone() })
}
