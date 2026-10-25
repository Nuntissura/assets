use crate::{
    invalid,
    public::{PageFetch, Traversal},
    CanonicalEntryPort, FetchRequest, FetchResponse, ProviderLimits,
};
use rss_contracts::{
    AccessMode, AcquisitionPage, Bounds, Capability, CapabilityStatus, Error, HostRef, Pending,
    Provider, ProviderPort, Revision, ScopeRef, Source, SourceRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Nonsecret attempt provenance. The host owns the actual lease, secrets and expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedContext {
    pub connection_ref: HostRef,
    pub owner_scope: ScopeRef,
    pub external_identity: String,
    pub lease_generation: Revision,
}
impl AuthorizedContext {
    pub(crate) fn selection(&self) -> AuthorizedSelection {
        AuthorizedSelection {
            connection_ref: self.connection_ref.clone(),
            owner_scope: self.owner_scope.clone(),
            external_identity: self.external_identity.clone(),
        }
    }
    pub fn validate(&self, source: &Source, request: &SourceRequest) -> rss_contracts::Result<()> {
        let bounds = &request.bounds;
        bounds.validate()?;
        self.lease_generation.validate()?;
        if self.owner_scope != source.owner_scope
            || self.owner_scope.0.is_empty()
            || request.subscription.partition.connection_ref.as_ref() != Some(&self.connection_ref)
            || !crate::public::numeric_id(&self.external_identity)
        {
            return Err(Error::Denied);
        }
        for value in [
            &self.connection_ref.host_instance_id,
            &self.connection_ref.owner_kind,
            &self.connection_ref.native_id,
            &self.owner_scope.0,
            &self.external_identity,
        ] {
            if value.is_empty() {
                return Err(invalid("instagram.authorization"));
            }
            bounds.text("instagram.authorization", value, bounds.max_text_bytes)?;
        }
        bounded_context(self, bounds)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorizedSelection {
    connection_ref: HostRef,
    owner_scope: ScopeRef,
    external_identity: String,
}
fn bounded_context(context: &AuthorizedContext, bounds: &Bounds) -> rss_contracts::Result<()> {
    if serde_json::to_vec(context)
        .map_err(|_| invalid("instagram.authorization"))?
        .len()
        > bounds.max_operation_bytes
    {
        return Err(Error::LimitExceeded {
            field: "instagram.authorization".into(),
        });
    }
    Ok(())
}

/// Explicit authorized transport. No secret is passed through this interface.
/// Keep the same host lease guard through canonical settlement. Recheck current
/// account/profile/provider/login identity, expiry and exact attempt generation.
/// A new attempt may renew its lease while replaying original retained bytes;
/// preserve their original provenance and never silently refetch a missing digest.
pub trait AuthorizedFetchPort: Send + Sync {
    fn current<'a>(
        &'a self,
        source: &'a SourceRequest,
        execution: &'a HostRef,
        authorization: &'a AuthorizedContext,
    ) -> Pending<'a, ()>;
    fn fetch<'a>(
        &'a self,
        source: &'a SourceRequest,
        execution: &'a HostRef,
        authorization: &'a AuthorizedContext,
        request: FetchRequest,
    ) -> Pending<'a, FetchResponse>;
}

pub struct AuthorizedInstagramProvider<F, C> {
    pub source: Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub authorization: AuthorizedContext,
    pub fetch: F,
    pub canonical: C,
}
struct Authorized<'a, F> {
    port: &'a F,
    context: &'a AuthorizedContext,
}
impl<F: AuthorizedFetchPort> PageFetch for Authorized<'_, F> {
    fn authorization(&self) -> Option<AuthorizedSelection> {
        Some(self.context.selection())
    }
    fn current<'a>(&'a self, source: &'a SourceRequest, execution: &'a HostRef) -> Pending<'a, ()> {
        self.port.current(source, execution, self.context)
    }
    fn fetch<'a>(
        &'a self,
        source: &'a SourceRequest,
        execution: &'a HostRef,
        request: FetchRequest,
    ) -> Pending<'a, FetchResponse> {
        self.port.fetch(source, execution, self.context, request)
    }
}
impl<F: AuthorizedFetchPort, C: CanonicalEntryPort> ProviderPort
    for AuthorizedInstagramProvider<F, C>
{
    fn capabilities(&self) -> Vec<Capability> {
        [AccessMode::PublicProfile, AccessMode::PublicReels]
            .into_iter()
            .map(|mode| Capability {
                provider: Provider::Instagram,
                mode,
                status: CapabilityStatus::Available,
                paginated: true,
                media_metadata: true,
                adapter_version: "instagram.authorized-source.v1".into(),
            })
            .collect()
    }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage> {
        Box::pin(async move {
            self.authorization.validate(&self.source, &request)?;
            Traversal {
                source: &self.source,
                execution_ref: &self.execution_ref,
                limits: &self.limits,
                supported_extensions: &self.supported_extensions,
                fetch: Authorized {
                    port: &self.fetch,
                    context: &self.authorization,
                },
                canonical: &self.canonical,
            }
            .acquire_page(request)
            .await
        })
    }
}
