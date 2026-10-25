use super::{home::{HomeDocumentKind, HomeFetchRequest, HomeFetchResponse}, timeline::{Timeline, TimelineFetch, TimelineSelection}};
use crate::{invalid, AuthorizedContext, CanonicalEntryPort, FetchResponse, ProviderLimits};
use rss_contracts::{AccessMode, AcquisitionPage, Bounds, Capability, CapabilityStatus, CaptureIntent,
    Error, HostRef, Pending, Provider, ProviderPort, Source, SourceRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowingDocumentKind { Html, Graphql }
impl FollowingDocumentKind {
    pub fn url(self) -> &'static str { match self {
        Self::Html => "https://www.instagram.com/?variant=following",
        Self::Graphql => "https://www.instagram.com/graphql/query",
    } }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowingContext {
    pub authorization: AuthorizedContext,
    pub capture_intent: CaptureIntent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowingFetchRequest {
    pub kind: FollowingDocumentKind,
    pub after: Option<String>,
    pub page_size: u16,
    pub expected_digest: Option<String>,
    pub max_body_bytes: usize,
    pub timeout_millis: u64,
}
impl FollowingFetchRequest {
    pub fn url(&self) -> &'static str { self.kind.url() }
}
pub struct FollowingFetchResponse {
    pub kind: FollowingDocumentKind,
    pub response: FetchResponse,
}
/// Supply raw selected Following HTML or GraphQL, never Home or normalized rows.
/// Bind the actual current account, profile, lease attempt and capture intent.
/// Keep the runtime Following query seed, variables, device and CSRF state in the
/// host; retain exact request/body provenance and hold the lease through settlement.
pub trait FollowingFetchPort: Send + Sync {
    fn current<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, context: &'a FollowingContext) -> Pending<'a, ()>;
    fn fetch_following<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, context: &'a FollowingContext, fetch: FollowingFetchRequest) -> Pending<'a, FollowingFetchResponse>;
}
pub struct FollowingInstagramProvider<F, C> {
    pub source: Source,
    pub execution_ref: HostRef,
    pub limits: ProviderLimits,
    pub supported_extensions: BTreeSet<String>,
    pub authorization: FollowingContext,
    pub fetch: F,
    pub canonical: C,
}
struct FollowingTransport<'a, F> { port: &'a F, context: &'a FollowingContext }
impl<F: FollowingFetchPort> TimelineFetch for FollowingTransport<'_, F> {
    fn current<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, _: &'a AuthorizedContext) -> Pending<'a, ()> {
        self.port.current(request, execution, self.context)
    }
    fn fetch_timeline<'a>(&'a self, request: &'a SourceRequest, execution: &'a HostRef, _: &'a AuthorizedContext, fetch: HomeFetchRequest) -> Pending<'a, HomeFetchResponse> {
        Box::pin(async move {
            let response = self.port.fetch_following(request, execution, self.context, FollowingFetchRequest {
                kind: match fetch.kind { HomeDocumentKind::Html => FollowingDocumentKind::Html, HomeDocumentKind::Graphql => FollowingDocumentKind::Graphql },
                after: fetch.after, page_size: fetch.page_size, expected_digest: fetch.expected_digest,
                max_body_bytes: fetch.max_body_bytes, timeout_millis: fetch.timeout_millis,
            }).await?;
            Ok(HomeFetchResponse { kind: match response.kind { FollowingDocumentKind::Html => HomeDocumentKind::Html, FollowingDocumentKind::Graphql => HomeDocumentKind::Graphql }, response: response.response })
        })
    }
}
impl FollowingContext {
    fn validate(&self, source: &Source, request: &SourceRequest, bounds: &Bounds) -> rss_contracts::Result<()> {
        match &self.capture_intent {
            CaptureIntent::FollowSource { original_url, provider, partition }
                if original_url == &source.original_url && *provider == Provider::Instagram && partition == &request.subscription.partition => {},
            _ => return Err(Error::Denied),
        }
        if serde_json::to_vec(self).map_err(|_| invalid("instagram.following_context"))?.len() > bounds.max_operation_bytes {
            return Err(Error::LimitExceeded { field: "instagram.following_context".into() });
        }
        Ok(())
    }
}
impl<F: FollowingFetchPort, C: CanonicalEntryPort> ProviderPort for FollowingInstagramProvider<F, C> {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability { provider: Provider::Instagram, mode: AccessMode::Following, status: CapabilityStatus::Available,
            paginated: true, media_metadata: true, adapter_version: "instagram.following.v1".into() }]
    }
    fn acquire<'a>(&'a self, request: SourceRequest) -> Pending<'a, AcquisitionPage> {
        Box::pin(async move {
            self.authorization.validate(&self.source, &request, &request.bounds)?;
            Timeline { source: &self.source, execution_ref: &self.execution_ref, limits: &self.limits,
                supported_extensions: &self.supported_extensions, authorization: &self.authorization.authorization,
                fetch: FollowingTransport { port: &self.fetch, context: &self.authorization }, canonical: &self.canonical,
                selection: TimelineSelection::Following,
            }.acquire_page(request).await
        })
    }
}
