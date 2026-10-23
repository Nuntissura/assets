use rss_contracts::{AuthorityPort, Error, HostRef, Pending, Result, SourceAction, SourceRequest, Subscription};

/// Canonical host admission for an opaque attempt lease, not a grant supplied by the caller.
/// Implementations recheck current subscription, executor, cancellation and deadline for this exact
/// attempt. Each item/checkpoint owner must repeat these fences atomically at its mutation boundary.
pub trait AcquisitionOwner: Send + Sync {
    fn admit<'a>(&'a self, request: &'a SourceRequest, execution_ref: &'a HostRef) -> Pending<'a, Subscription>;
}

pub async fn authorize_current(authority: &impl AuthorityPort, execution: &impl AcquisitionOwner,
    request: &SourceRequest, execution_ref: &HostRef) -> Result<()> {
    crate::validate_request(request)?;
    for value in [&execution_ref.host_instance_id, &execution_ref.owner_kind, &execution_ref.native_id] {
        if value.is_empty() { return Err(crate::invalid("execution_ref")); }
        request.bounds.text("execution_ref", value, request.bounds.max_text_bytes)?;
    }
    let scope = &request.subscription.partition.scope;
    authority.authorize(scope, &request.subscription.subscription_ref, SourceAction::Refresh).await?;
    authority.authorize(scope, &request.subscription.source_ref, SourceAction::Refresh).await?;
    let current = execution.admit(request, execution_ref).await?;
    if current != request.subscription { return Err(Error::StaleResult); }
    if current.paused { return Err(Error::Cancelled); }
    Ok(())
}
