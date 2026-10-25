use super::timeline::{Timeline, TimelineFetch, TimelineSelection};
use crate::{AuthorizedContext, AuthorizedFetchPort,
    CanonicalEntryPort, FetchResponse, ProviderLimits};
use rss_contracts::{AccessMode, AcquisitionPage, Capability, CapabilityStatus, HostRef, Pending, Provider, ProviderPort, Source, SourceRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const HOME_URL: &str = "https://www.instagram.com/";
const GRAPHQL_URL: &str = "https://www.instagram.com/graphql/query/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeDocumentKind { Html, Graphql }
impl HomeDocumentKind {
    pub fn url(self) -> &'static str { match self { Self::Html => HOME_URL, Self::Graphql => GRAPHQL_URL } }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeFetchRequest {
    pub kind: HomeDocumentKind,
    pub after: Option<String>,
    pub page_size: u16,
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
}
pub struct HomeFetchResponse {
    pub kind: HomeDocumentKind,
    pub response: FetchResponse,
}
/// Return raw initial homepage HTML or raw Home timeline GraphQL JSON.
/// The host binds a current Home query seed, authenticated login and exact request;
/// it owns session/device/query variables and retains original request/body provenance.
/// A digest selects those original bytes, never a refetch or normalized feed.
pub trait HomeFetchPort: AuthorizedFetchPort {
    fn fetch_home<'a>(&'a self, source: &'a SourceRequest, execution: &'a HostRef,
        authorization: &'a AuthorizedContext, request: HomeFetchRequest) -> Pending<'a, HomeFetchResponse>;
}
pub struct HomeInstagramProvider<F, C> {
    pub source: Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub authorization: AuthorizedContext,
    pub fetch: F,
    pub canonical: C,
}
struct HomeTransport<'a, F>(&'a F);
impl<F: HomeFetchPort> TimelineFetch for HomeTransport<'_, F> {
    fn current<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext) -> Pending<'a, ()> {
        self.0.current(request, execution, authorization)
    }
    fn fetch_timeline<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, authorization: &'a AuthorizedContext, fetch: HomeFetchRequest) -> Pending<'a, HomeFetchResponse> {
        self.0.fetch_home(request, execution, authorization, fetch)
    }
}
impl<F: HomeFetchPort, C: CanonicalEntryPort> ProviderPort for HomeInstagramProvider<F, C> {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability { provider: Provider::Instagram, mode: AccessMode::HomeForYou, status: CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "instagram.home.v1".into() }]
    }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage> { Box::pin(async move {
        Timeline { source: &self.source, execution_ref: &self.execution_ref, limits: &self.limits,
            supported_extensions: &self.supported_extensions, authorization: &self.authorization,
            fetch: HomeTransport(&self.fetch), canonical: &self.canonical, selection: TimelineSelection::Home,
        }.acquire_page(request).await
    }) }
}
