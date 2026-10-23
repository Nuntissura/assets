//! Bounded acquisition decisions. Hosts persist state, authorize effects and enforce deadlines.
pub mod capture;
pub mod jobs;

use rss_contracts::{Error, Result, SourceRequest};

pub(crate) fn invalid(field: &str) -> Error { Error::Invalid { field: field.into() } }

pub fn validate_request(request: &SourceRequest) -> Result<()> {
    request.bounds.validate()?;
    request.subscription.partition.validate(&request.bounds)?;
    request.subscription.generation.validate()?;
    request.subscription.common.validate_bounded(&request.bounds, &Default::default())?;
    for reference in [&request.subscription.subscription_ref, &request.subscription.source_ref, &request.subscription.executor_ref] {
        for value in [&reference.host_instance_id, &reference.owner_kind, &reference.native_id] {
            if value.is_empty() { return Err(invalid("request.reference")); }
            if value.len() > request.bounds.max_text_bytes { return Err(Error::LimitExceeded { field: "request.reference".into() }); }
        }
    }
    if let Some(continuation) = &request.continuation { continuation.validate_for(&request.subscription, &request.bounds)?; }
    let bytes = serde_json::to_vec(request).map_err(|_| invalid("request"))?;
    if bytes.len() > request.bounds.max_operation_bytes { return Err(Error::LimitExceeded { field: "request".into() }); }
    Ok(())
}

pub async fn acquire(provider: &impl rss_contracts::ProviderPort, request: SourceRequest,
    supported: &std::collections::BTreeSet<String>) -> Result<rss_contracts::AcquisitionPage> {
    validate_request(&request)?;
    if request.subscription.paused { return Err(Error::Cancelled); }
    let page = provider.acquire(request.clone()).await?;
    page.validate(&request.subscription, &request.bounds, supported)?;
    if page.operation_id != request.operation_id { return Err(Error::StaleResult); }
    Ok(page)
}
