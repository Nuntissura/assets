//! Selected own/private profile content through host-authorized transport.
use crate::{authorized::Authorized, public::Traversal, AuthorizedContext, AuthorizedFetchPort,
    CanonicalEntryPort, ProviderLimits};
use rss_contracts::{AccessMode, AcquisitionPage, Capability, CapabilityStatus, Error, HostRef,
    Pending, Provider, ProviderPort, Source, SourceRequest};
use std::collections::BTreeSet;

pub struct ConnectedInstagramProvider<F, C> {
    pub source: Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub authorization: AuthorizedContext,
    pub fetch: F,
    pub canonical: C,
}
impl<F: AuthorizedFetchPort, C: CanonicalEntryPort> ProviderPort for ConnectedInstagramProvider<F, C> {
    fn capabilities(&self) -> Vec<Capability> {
        [AccessMode::OwnUploads, AccessMode::PrivateSource].into_iter().map(|mode| Capability {
            provider: Provider::Instagram, mode, status: CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "instagram.connected-profile.v1".into(),
        }).collect()
    }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage> {
        Box::pin(async move {
            let mode = request.subscription.partition.mode;
            if !matches!(mode, AccessMode::OwnUploads | AccessMode::PrivateSource) {
                return Err(Error::Unsupported { provider: Provider::Instagram, mode });
            }
            self.authorization.validate(&self.source, &request)?;
            Traversal {
                source: &self.source, execution_ref: &self.execution_ref, limits: &self.limits,
                supported_extensions: &self.supported_extensions,
                fetch: Authorized { port: &self.fetch, context: &self.authorization, connected_profile: true },
                canonical: &self.canonical,
            }.acquire_page(request).await
        })
    }
}
