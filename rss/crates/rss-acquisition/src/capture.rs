use std::collections::{BTreeMap, BTreeSet};
use hsscrap::{ports::{CaptureCommand, CommandContext, ItemOwner, MutationCoordinator}, OperationOutcome, OutcomeState, URLCapture, Validate};
use rss_contracts::{CaptureIntent, Error, Result};
use crate::invalid;

fn guard_map(values: &[hsscrap::ExpectedRevision]) -> Result<BTreeMap<&hsscrap::HostRef, hsscrap::Revision>> {
    let mut guards = BTreeMap::new();
    for value in values {
        value.revision.validate()?;
        if guards.insert(&value.owner, value.revision).is_some() { return Err(invalid("expected_revisions.duplicate_owner")); }
    }
    Ok(guards)
}

pub(crate) fn same_outcome(left: &OperationOutcome, right: &OperationOutcome, bounds: &hsscrap::Bounds) -> Result<bool> {
    left.validate(bounds, &BTreeSet::new())?;
    right.validate(bounds, &BTreeSet::new())?;
    // The owning serializer sorts revision-map entries and rejects duplicate owners.
    Ok(serde_json::to_value(left).map_err(|_| invalid("operation_outcome"))?
        == serde_json::to_value(right).map_err(|_| invalid("operation_outcome"))?)
}

pub(crate) fn committed(owner: &impl ItemOwner, context: &CommandContext) -> Result<OperationOutcome> {
    let outcome = owner.operation_outcome(context, context.operation_id)?;
    outcome.validate(&context.bounds, &BTreeSet::new())?;
    if outcome.operation_id != context.operation_id || outcome.scope != context.scope
        || outcome.intent_digest != context.intent_digest || guard_map(&outcome.expected_revisions)? != guard_map(&context.expected_revisions)? { return Err(Error::Conflict); }
    if outcome.outcome != OutcomeState::Committed || outcome.committed_at.is_none() || outcome.owner_receipt_refs.is_empty() {
        return Err(Error::UnknownOutcome { operation_id: context.operation_id });
    }
    Ok(outcome)
}

/// Resolve a lost response through the canonical owner before issuing another mutation.
pub(crate) fn reconcile(owner: &mut (impl ItemOwner + MutationCoordinator), context: &CommandContext,
    mutation: impl FnOnce(&mut dyn ItemOwner) -> hsscrap::Result<OperationOutcome>) -> Result<OperationOutcome> {
    match owner.operation_outcome(context, context.operation_id) {
        Ok(existing) => {
            if existing.operation_id != context.operation_id || existing.scope != context.scope
                || existing.intent_digest != context.intent_digest { return Err(Error::Conflict); }
            if existing.outcome == OutcomeState::Pending {
                let response = MutationCoordinator::recover(owner, context.clone());
                return match committed(owner, context) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) => match response { Err(failure) => Err(failure.into()), Ok(_) => Err(error) },
                };
            }
            committed(owner, context)
        }
        Err(hsscrap::Error::NotFound) => {
            let response = mutation(owner);
            match committed(owner, context) {
                Ok(outcome) => Ok(outcome),
                Err(Error::ItemOwner { error: hsscrap::Error::NotFound }) => match response {
                    Err(failure) => Err(failure.into()),
                    Ok(_) => Err(Error::UnknownOutcome { operation_id: context.operation_id }),
                },
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_context(context: &CommandContext, scope: &hsscrap::ScopeRef) -> Result<()> {
    context.bounds.validate()?;
    if &context.scope != scope || context.scope.0.is_empty() || context.intent_digest.is_empty() { return Err(Error::Denied); }
    for value in [&context.scope.0, &context.intent_digest, &context.actor.host_instance_id, &context.actor.owner_kind, &context.actor.native_id] {
        if value.is_empty() { return Err(invalid("command_context")); }
        context.bounds.text("command_context", value, context.bounds.max_text_bytes)?;
    }
    if context.expected_revisions.len() > context.bounds.max_collection_items { return Err(Error::LimitExceeded { field: "expected_revisions".into() }); }
    let mut owners = BTreeSet::new();
    for expected in &context.expected_revisions {
        expected.revision.validate()?;
        if !owners.insert(&expected.owner) { return Err(invalid("expected_revisions.duplicate_owner")); }
        for value in [&expected.owner.host_instance_id, &expected.owner.owner_kind, &expected.owner.native_id] {
            if value.is_empty() { return Err(invalid("expected_revisions.owner")); }
            context.bounds.text("expected_revisions.owner", value, context.bounds.max_text_bytes)?;
        }
    }
    Ok(())
}

/// A successful return proves the exact URLCapture is stored. Enrichment is a separate operation.
pub fn save_url(owner: &mut (impl ItemOwner + MutationCoordinator), intent: &CaptureIntent, context: CommandContext,
    capture: URLCapture, supported: &BTreeSet<String>) -> Result<OperationOutcome> {
    let CaptureIntent::SaveUrl { original_url } = intent else { return Err(invalid("intent.save_url")); };
    validate_context(&context, &capture.access_partition)?;
    capture.validate(&context.bounds, supported)?;
    let normalized = hsscrap::identity::normalize_url(original_url, &context.bounds)?;
    if capture.capture_kind != hsscrap::CaptureKind::UserInput || capture.original_url.as_ref() != Some(original_url)
        || capture.captured_url != *original_url || capture.identity_url != normalized.identity_url
        || capture.normalization_version != normalized.normalization_version || capture.operation_id != context.operation_id
        || capture.actor_ref.as_ref() != Some(&context.actor) { return Err(invalid("capture.intent")); }
    if crate::identity::resolve_capture_identity(owner, &context, original_url, None)?.is_some_and(|item| item != capture.item_id) {
        return Err(Error::Conflict);
    }
    persist_capture(owner, context, capture, supported)
}

pub(crate) fn persist_capture(owner: &mut (impl ItemOwner + MutationCoordinator), context: CommandContext,
    capture: URLCapture, supported: &BTreeSet<String>) -> Result<OperationOutcome> {
    validate_context(&context, &capture.access_partition)?;
    capture.validate(&context.bounds, supported)?;
    if capture.operation_id != context.operation_id { return Err(invalid("capture.operation_id")); }
    let command = CaptureCommand { context: context.clone(), capture: capture.clone(), item_id: capture.item_id };
    if serde_json::to_vec(&command).map_err(|_| invalid("capture.command"))?.len() > context.bounds.max_operation_bytes {
        return Err(Error::LimitExceeded { field: "capture.command".into() });
    }
    let outcome = reconcile(owner, &context, |owner| owner.capture(command))?;
    let snapshot = owner.inspect(&context, capture.item_id)?;
    snapshot.item.validate(&context.bounds, supported).map_err(|_|Error::UnknownOutcome {operation_id:context.operation_id})?;
    if snapshot.item.item_id != capture.item_id || snapshot.item.owner_scope.0.is_empty()
        || snapshot.item.lifecycle != hsscrap::Lifecycle::Active
        || !snapshot.captures.iter().any(|stored| stored == &capture) { return Err(Error::UnknownOutcome { operation_id: context.operation_id }); }
    Ok(outcome)
}
