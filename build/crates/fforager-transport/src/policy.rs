use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const MAX_URL_BYTES: usize = 4 * 1024;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Http11,
    Http2,
    Proxy,
    Range,
    Streaming,
    Compression,
    RedirectPolicy,
    CookieScope,
    DnsProvenance,
    SsrfPolicy,
    PoolPartition,
    Replay,
    Cancellation,
    MetadataBounds,
    BodyBounds,
    DecompressionBounds,
    RetryBounds,
    TlsFingerprint,
    Http2Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedCapability {
    pub capability: Capability,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDecision {
    pub requested: BTreeSet<Capability>,
    pub satisfied: BTreeSet<Capability>,
    pub blocked: Vec<BlockedCapability>,
    pub execution_allowed: bool,
}

#[derive(Debug, Clone)]
pub struct CandidateAdapter {
    identity: String,
    supported: BTreeSet<Capability>,
}

impl CandidateAdapter {
    #[must_use]
    pub fn std_first() -> Self {
        Self {
            identity: "ferric-std-first-transport-spike-v1".to_owned(),
            supported: BTreeSet::from([
                Capability::Http11,
                Capability::Range,
                Capability::Streaming,
                Capability::Replay,
                Capability::Cancellation,
                Capability::MetadataBounds,
                Capability::BodyBounds,
            ]),
        }
    }

    pub(crate) fn wreq_adjudication() -> Self {
        Self {
            identity: "ferric-wreq-adjudication-v1".to_owned(),
            supported: BTreeSet::from([
                Capability::Http11,
                Capability::Http2,
                Capability::BodyBounds,
                Capability::TlsFingerprint,
                Capability::Http2Fingerprint,
            ]),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn negotiate(&self, requested: impl IntoIterator<Item = Capability>) -> CapabilityDecision {
        let requested = requested.into_iter().collect::<BTreeSet<_>>();
        let mut satisfied = BTreeSet::new();
        let mut blocked = Vec::new();
        for capability in &requested {
            if self.supported.contains(capability) {
                satisfied.insert(*capability);
            } else {
                blocked.push(blocked_capability(*capability));
            }
        }
        CapabilityDecision {
            execution_allowed: blocked.is_empty(),
            requested,
            satisfied,
            blocked,
        }
    }

    /// Negotiates and executes one operation through the candidate boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed blocked-capability error before the operation closure is
    /// invoked when any requested capability is unavailable, or propagates the
    /// operation's transport error.
    pub(crate) fn execute<T>(
        &self,
        requested: impl IntoIterator<Item = Capability>,
        operation: impl FnOnce(&ExecutionGrant) -> Result<T, TransportError>,
    ) -> Result<(CapabilityDecision, T), TransportError> {
        self.execute_typed(requested, operation)
    }

    pub(crate) fn execute_typed<T, E>(
        &self,
        requested: impl IntoIterator<Item = Capability>,
        operation: impl FnOnce(&ExecutionGrant) -> Result<T, E>,
    ) -> Result<(CapabilityDecision, T), E>
    where
        E: From<TransportError>,
    {
        let decision = self.negotiate(requested);
        if decision.requested.is_empty() {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-CAPABILITY-EMPTY: operation requires declared capabilities"
                    .to_owned(),
            )
            .into());
        }
        if !decision.execution_allowed {
            return Err(TransportError::CapabilityBlocked(decision.blocked).into());
        }
        let grant = ExecutionGrant {
            capabilities: decision.satisfied.clone(),
        };
        let output = operation(&grant)?;
        Ok((decision, output))
    }

    pub(crate) fn without_capability(mut self, capability: Capability) -> Self {
        self.supported.remove(&capability);
        self
    }
}

#[derive(Debug)]
pub struct ExecutionGrant {
    capabilities: BTreeSet<Capability>,
}

impl ExecutionGrant {
    /// Requires a capability already negotiated at the candidate boundary.
    ///
    /// # Errors
    ///
    /// Returns a typed capability error if an operation attempts to use a
    /// capability that was not part of its request.
    pub fn require(&self, capability: Capability) -> Result<(), TransportError> {
        if self.capabilities.contains(&capability) {
            Ok(())
        } else {
            Err(TransportError::CapabilityBlocked(vec![blocked_capability(
                capability,
            )]))
        }
    }
}

fn blocked_capability(capability: Capability) -> BlockedCapability {
    let (code, reason) = match capability {
        Capability::TlsFingerprint => (
            "FF-TRANSPORT-E-TLS-FINGERPRINT-BLOCKED",
            "the authorized std-first candidate has no browser ClientHello parity mechanism",
        ),
        Capability::Http2Fingerprint => (
            "FF-TRANSPORT-E-H2-FINGERPRINT-BLOCKED",
            "the authorized std-first candidate has no browser HTTP/2 wire parity mechanism",
        ),
        Capability::Http2 => (
            "FF-TRANSPORT-E-HTTP2-BLOCKED",
            "the bounded std-first candidate implements only HTTP/1.1 local evidence",
        ),
        Capability::Proxy => (
            "FF-TRANSPORT-E-PROXY-EVIDENCE-BLOCKED",
            "no trusted proxy destination-evidence contract is available",
        ),
        Capability::Compression => (
            "FF-TRANSPORT-E-COMPRESSION-BLOCKED",
            "no authorized decompressor implementation is present in the candidate",
        ),
        Capability::DecompressionBounds => (
            "FF-TRANSPORT-E-DECOMPRESSION-BOUNDS-BLOCKED",
            "no authorized decompressor exists through which decompressed-byte admission can be enforced",
        ),
        Capability::RedirectPolicy => (
            "FF-TRANSPORT-E-REDIRECT-INTEGRATION-BLOCKED",
            "redirect targets are not integrated with authoritative DNS and peer-address SSRF validation",
        ),
        Capability::CookieScope => (
            "FF-TRANSPORT-E-COOKIE-PSL-BLOCKED",
            "no versioned authoritative public-suffix snapshot with wildcard and exception semantics is present",
        ),
        Capability::DnsProvenance => (
            "FF-TRANSPORT-E-DNS-PROVENANCE-BLOCKED",
            "the candidate has no resolver-bound DNS provenance implementation",
        ),
        Capability::SsrfPolicy => (
            "FF-TRANSPORT-E-SSRF-REGISTRY-BLOCKED",
            "the candidate has no generated complete current special-purpose address registry",
        ),
        Capability::PoolPartition => (
            "FF-TRANSPORT-E-POOL-CONTEXT-BLOCKED",
            "pool identity is not derived from immutable candidate execution context",
        ),
        Capability::RetryBounds => (
            "FF-TRANSPORT-E-RETRY-EXECUTOR-BLOCKED",
            "retry policy is not integrated with request attempts, deadlines, cancellation, and partial-body state",
        ),
        _ => (
            "FF-TRANSPORT-E-CAPABILITY-BLOCKED",
            "the candidate does not implement this capability",
        ),
    };
    BlockedCapability {
        capability,
        code: code.to_owned(),
        reason: reason.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    InvalidUrl(String),
    InvalidHeader(String),
    CapabilityBlocked(Vec<BlockedCapability>),
    Policy(String),
    Bound {
        kind: &'static str,
        observed: u64,
        maximum: u64,
    },
    Cancellation(String),
    Io(String),
    Protocol(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(message)
            | Self::InvalidHeader(message)
            | Self::Policy(message)
            | Self::Cancellation(message)
            | Self::Io(message)
            | Self::Protocol(message) => formatter.write_str(message),
            Self::CapabilityBlocked(blocked) => {
                write!(formatter, "blocked capabilities: {blocked:?}")
            }
            Self::Bound {
                kind,
                observed,
                maximum,
            } => write!(
                formatter,
                "FF-TRANSPORT-E-BOUND: {kind} observed {observed} exceeds {maximum}"
            ),
        }
    }
}

impl std::error::Error for TransportError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path_and_query: String,
}

impl HttpUrl {
    /// Parses a bounded absolute HTTP or HTTPS URL without user information.
    ///
    /// # Errors
    ///
    /// Returns a transport error for unsupported schemes, invalid authorities,
    /// unsafe characters, or exceeded URL and hostname bounds.
    pub fn parse(value: &str) -> Result<Self, TransportError> {
        if value.len() > MAX_URL_BYTES
            || value.is_empty()
            || value.chars().any(char::is_control)
            || value.contains('#')
            || value.contains('@')
        {
            return Err(TransportError::InvalidUrl(
                "FF-TRANSPORT-E-URL-BOUNDS: invalid or unsafe URL".to_owned(),
            ));
        }
        let (scheme, remainder, default_port) = if let Some(rest) = value.strip_prefix("http://") {
            ("http", rest, 80)
        } else if let Some(rest) = value.strip_prefix("https://") {
            ("https", rest, 443)
        } else {
            return Err(TransportError::InvalidUrl(
                "FF-TRANSPORT-E-SCHEME: only http and https are allowed".to_owned(),
            ));
        };
        let authority_end = remainder.find('/').unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        let path_and_query = if authority_end == remainder.len() {
            "/"
        } else {
            &remainder[authority_end..]
        };
        let (host, port) = parse_authority(authority, default_port)?;
        Ok(Self {
            scheme: scheme.to_owned(),
            host: host.to_ascii_lowercase(),
            port,
            path_and_query: path_and_query.to_owned(),
        })
    }

    #[must_use]
    pub fn origin(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.host, self.port)
    }

    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.scheme == "https"
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{}://{}:{}{}",
            self.scheme, self.host, self.port, self.path_and_query
        )
    }
}

fn parse_authority(authority: &str, default_port: u16) -> Result<(String, u16), TransportError> {
    if authority.is_empty() {
        return Err(TransportError::InvalidUrl(
            "FF-TRANSPORT-E-HOST: host is empty".to_owned(),
        ));
    }
    if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| {
            TransportError::InvalidUrl("FF-TRANSPORT-E-HOST: malformed IPv6 literal".to_owned())
        })?;
        let host = &authority[1..close];
        let port = if close + 1 == authority.len() {
            default_port
        } else {
            authority
                .get(close + 1..)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .ok_or_else(|| {
                    TransportError::InvalidUrl(
                        "FF-TRANSPORT-E-PORT: malformed IPv6 port".to_owned(),
                    )
                })?
                .parse::<u16>()
                .map_err(|_| {
                    TransportError::InvalidUrl("FF-TRANSPORT-E-PORT: invalid port".to_owned())
                })?
        };
        host.parse::<Ipv6Addr>().map_err(|_| {
            TransportError::InvalidUrl("FF-TRANSPORT-E-HOST: invalid IPv6 literal".to_owned())
        })?;
        return Ok((host.to_owned(), port));
    }
    let mut pieces = authority.rsplitn(2, ':');
    let tail = pieces.next().unwrap_or_default();
    let head = pieces.next();
    if let Some(host) = head
        && !tail.is_empty()
        && tail.bytes().all(|byte| byte.is_ascii_digit())
    {
        let port = tail.parse::<u16>().map_err(|_| {
            TransportError::InvalidUrl("FF-TRANSPORT-E-PORT: invalid port".to_owned())
        })?;
        validate_hostname(host)?;
        return Ok((host.to_owned(), port));
    }
    validate_hostname(authority)?;
    Ok((authority.to_owned(), default_port))
}

fn validate_hostname(host: &str) -> Result<(), TransportError> {
    if host.is_empty()
        || host.len() > 253
        || host.starts_with('.')
        || host.ends_with('.')
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(TransportError::InvalidUrl(
            "FF-TRANSPORT-E-HOST: invalid hostname".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderValue {
    pub value: String,
    pub sensitive: bool,
    pub origin_bound: bool,
}

impl HeaderValue {
    /// Constructs a bounded header value with explicit sensitivity metadata.
    ///
    /// # Errors
    ///
    /// Returns a transport error for control characters or excessive length.
    pub fn new(
        value: impl Into<String>,
        sensitive: bool,
        origin_bound: bool,
    ) -> Result<Self, TransportError> {
        let value = value.into();
        if value.len() > MAX_HEADER_VALUE_BYTES || value.chars().any(char::is_control) {
            return Err(TransportError::InvalidHeader(
                "FF-TRANSPORT-E-HEADER-VALUE: invalid header value".to_owned(),
            ));
        }
        Ok(Self {
            value,
            sensitive,
            origin_bound,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub request_id: String,
    pub method: String,
    pub url: HttpUrl,
    pub headers: BTreeMap<String, HeaderValue>,
    pub redirect_count: u8,
}

impl HttpRequest {
    /// Constructs a request with a stable identifier and uppercase method.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the identifier or method is invalid.
    pub fn new(
        request_id: impl Into<String>,
        method: impl Into<String>,
        url: HttpUrl,
    ) -> Result<Self, TransportError> {
        let request_id = request_id.into();
        let method = method.into();
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(TransportError::Protocol(
                "FF-TRANSPORT-E-REQUEST-ID: invalid request ID".to_owned(),
            ));
        }
        if method.is_empty()
            || method.len() > 16
            || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err(TransportError::Protocol(
                "FF-TRANSPORT-E-METHOD: invalid method".to_owned(),
            ));
        }
        Ok(Self {
            request_id,
            method,
            url,
            headers: BTreeMap::new(),
            redirect_count: 0,
        })
    }

    /// Adds or replaces one bounded, normalized request header.
    ///
    /// # Errors
    ///
    /// Returns a transport error for an invalid name or excessive header count.
    pub fn insert_header(
        &mut self,
        name: impl Into<String>,
        value: HeaderValue,
    ) -> Result<(), TransportError> {
        let name = name.into().to_ascii_lowercase();
        if self.headers.len() >= MAX_HEADERS
            || name.is_empty()
            || name.len() > MAX_HEADER_NAME_BYTES
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(TransportError::InvalidHeader(
                "FF-TRANSPORT-E-HEADER-NAME: invalid or excessive header".to_owned(),
            ));
        }
        self.headers.insert(name, value);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectPolicy {
    pub maximum_hops: u8,
    pub reject_https_downgrade: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectResult {
    pub request: HttpRequest,
    pub stripped_headers: Vec<String>,
}

impl RedirectPolicy {
    /// Applies redirect bounds, downgrade rejection, and cross-origin stripping.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the hop limit is exhausted or an HTTPS
    /// downgrade is requested.
    pub fn apply(
        self,
        request: &HttpRequest,
        target: HttpUrl,
    ) -> Result<RedirectResult, TransportError> {
        if request.redirect_count >= self.maximum_hops {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-REDIRECT-LIMIT: redirect budget exhausted".to_owned(),
            ));
        }
        if self.reject_https_downgrade && request.url.is_secure() && !target.is_secure() {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-REDIRECT-DOWNGRADE: HTTPS downgrade rejected".to_owned(),
            ));
        }
        if target
            .host
            .parse::<IpAddr>()
            .is_ok_and(|address| !is_allowed_public_address(address))
        {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-REDIRECT-SSRF: special-use literal target rejected".to_owned(),
            ));
        }
        let cross_origin = request.url.origin() != target.origin();
        let mut headers = request.headers.clone();
        let mut stripped_headers = Vec::new();
        if cross_origin {
            headers.retain(|name, value| {
                let keep = !value.sensitive && !value.origin_bound && !is_origin_bound_header(name);
                if !keep {
                    stripped_headers.push(name.clone());
                }
                keep
            });
        }
        stripped_headers.sort();
        Ok(RedirectResult {
            request: HttpRequest {
                request_id: request.request_id.clone(),
                method: request.method.clone(),
                url: target,
                headers,
                redirect_count: request.redirect_count.saturating_add(1),
            },
            stripped_headers,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicSuffixSet {
    suffixes: BTreeSet<String>,
}

impl PublicSuffixSet {
    #[must_use]
    pub fn new(suffixes: impl IntoIterator<Item = String>) -> Self {
        Self {
            suffixes: suffixes
                .into_iter()
                .map(|suffix| suffix.to_ascii_lowercase())
                .collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, domain: &str) -> bool {
        self.suffixes.contains(&domain.to_ascii_lowercase())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.suffixes.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
    maximum_cookies: usize,
}

impl CookieJar {
    #[must_use]
    pub fn new(maximum_cookies: usize) -> Self {
        Self {
            cookies: Vec::new(),
            maximum_cookies,
        }
    }

    /// Validates and stores a cookie within its source and public-suffix scope.
    ///
    /// # Errors
    ///
    /// Returns a transport error for invalid syntax, domain scope, public suffix,
    /// source mismatch, or cookie-count overflow.
    pub fn store(
        &mut self,
        source: &HttpUrl,
        mut cookie: Cookie,
        public_suffixes: &PublicSuffixSet,
    ) -> Result<(), TransportError> {
        validate_cookie_text(&cookie)?;
        cookie.domain = cookie.domain.to_ascii_lowercase();
        if public_suffixes.is_empty() {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-COOKIE-PSL-UNAVAILABLE: public suffix data is empty".to_owned(),
            ));
        }
        let source_ip = source.host.parse::<IpAddr>().ok();
        let cookie_ip = cookie.domain.parse::<IpAddr>().ok();
        if let Some(source_ip) = source_ip {
            if !cookie.host_only || cookie_ip != Some(source_ip) {
                return Err(TransportError::Policy(
                    "FF-TRANSPORT-E-COOKIE-IP-DOMAIN: IP cookies must be exact and host-only"
                        .to_owned(),
                ));
            }
        } else if cookie_ip.is_some() {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-COOKIE-IP-DOMAIN: DNS hosts cannot set IP-domain cookies"
                    .to_owned(),
            ));
        }
        if public_suffixes.contains(&cookie.domain) {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-COOKIE-PUBLIC-SUFFIX: cookie domain is a public suffix".to_owned(),
            ));
        }
        if cookie.host_only {
            if cookie.domain != source.host {
                return Err(TransportError::Policy(
                    "FF-TRANSPORT-E-COOKIE-HOST-ONLY: source host does not match".to_owned(),
                ));
            }
        } else if !domain_matches(&source.host, &cookie.domain) {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-COOKIE-DOMAIN: source host does not domain-match".to_owned(),
            ));
        }
        if self.cookies.len() >= self.maximum_cookies {
            return Err(TransportError::Bound {
                kind: "cookie_count",
                observed: (self.cookies.len() + 1) as u64,
                maximum: self.maximum_cookies as u64,
            });
        }
        self.cookies.retain(|existing| {
            !(existing.name == cookie.name
                && existing.domain == cookie.domain
                && existing.path == cookie.path)
        });
        self.cookies.push(cookie);
        Ok(())
    }

    #[must_use]
    pub fn values_for(&self, destination: &HttpUrl) -> Vec<(String, String)> {
        let mut values = self
            .cookies
            .iter()
            .filter(|cookie| {
                let domain_ok = if cookie.host_only {
                    destination.host == cookie.domain
                } else {
                    domain_matches(&destination.host, &cookie.domain)
                };
                domain_ok
                    && path_matches(&destination.path_and_query, &cookie.path)
                    && (!cookie.secure || destination.is_secure())
            })
            .map(|cookie| (cookie.name.clone(), cookie.value.clone()))
            .collect::<Vec<_>>();
        values.sort();
        values
    }
}

fn validate_cookie_text(cookie: &Cookie) -> Result<(), TransportError> {
    let invalid_name = cookie.name.is_empty()
        || cookie.name.len() > 256
        || cookie
            .name
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b';' | b'=' | b',' | b' '));
    let invalid_value = cookie.value.len() > 4 * 1024
        || cookie
            .value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b';' | b'\r' | b'\n'));
    let invalid_path = !cookie.path.starts_with('/')
        || cookie.path.len() > 2 * 1024
        || cookie.path.chars().any(char::is_control);
    if invalid_name || invalid_value || invalid_path {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-COOKIE-SYNTAX: invalid cookie".to_owned(),
        ));
    }
    validate_hostname(&cookie.domain)
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    let path = request_path.split('?').next().unwrap_or("/");
    path == cookie_path
        || path
            .strip_prefix(cookie_path)
            .is_some_and(|suffix| cookie_path.ends_with('/') || suffix.starts_with('/'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsEvidence {
    pub query_host: String,
    pub answers: Vec<IpAddr>,
    pub selected: IpAddr,
    pub resolver_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyEvidence {
    Direct,
    TrustedDestinationEvidence(IpAddr),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpPolicyError {
    ProvenanceMismatch,
    EmptyAnswers,
    SpecialUse(IpAddr),
    SelectedNotApproved(IpAddr),
    ConnectedAddressMismatch { selected: IpAddr, connected: IpAddr },
    ProxyEvidenceUnavailable,
}

impl fmt::Display for IpPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProvenanceMismatch => formatter.write_str("FF-TRANSPORT-E-DNS-PROVENANCE"),
            Self::EmptyAnswers => formatter.write_str("FF-TRANSPORT-E-DNS-EMPTY"),
            Self::SpecialUse(address) => {
                write!(formatter, "FF-TRANSPORT-E-SSRF-SPECIAL-USE: {address}")
            }
            Self::SelectedNotApproved(address) => {
                write!(formatter, "FF-TRANSPORT-E-DNS-SELECTION: {address}")
            }
            Self::ConnectedAddressMismatch {
                selected,
                connected,
            } => write!(
                formatter,
                "FF-TRANSPORT-E-DNS-REBIND: selected {selected}, connected {connected}"
            ),
            Self::ProxyEvidenceUnavailable => {
                formatter.write_str("FF-TRANSPORT-E-PROXY-DESTINATION-EVIDENCE")
            }
        }
    }
}

/// Validates all DNS answers, the selected address, and proxy destination evidence.
///
/// # Errors
///
/// Returns a policy error for empty or special-use answers, an unapproved selected
/// address, missing proxy evidence, or a proxy destination mismatch.
pub fn validate_dns_evidence(
    expected_host: &str,
    evidence: &DnsEvidence,
    proxy: ProxyEvidence,
) -> Result<(), IpPolicyError> {
    if !expected_host.eq_ignore_ascii_case(&evidence.query_host)
        || evidence.resolver_identity.is_empty()
        || evidence.resolver_identity.len() > 256
        || evidence.resolver_identity.chars().any(char::is_control)
    {
        return Err(IpPolicyError::ProvenanceMismatch);
    }
    if evidence.answers.is_empty() {
        return Err(IpPolicyError::EmptyAnswers);
    }
    for address in &evidence.answers {
        if !is_allowed_public_address(*address) {
            return Err(IpPolicyError::SpecialUse(*address));
        }
    }
    if !evidence.answers.contains(&evidence.selected) {
        return Err(IpPolicyError::SelectedNotApproved(evidence.selected));
    }
    match proxy {
        ProxyEvidence::Direct => Ok(()),
        ProxyEvidence::TrustedDestinationEvidence(destination)
            if destination == evidence.selected =>
        {
            Ok(())
        }
        ProxyEvidence::TrustedDestinationEvidence(destination) => {
            Err(IpPolicyError::ConnectedAddressMismatch {
                selected: evidence.selected,
                connected: destination,
            })
        }
        ProxyEvidence::Unavailable => Err(IpPolicyError::ProxyEvidenceUnavailable),
    }
}

/// Revalidates that the connected address equals the approved DNS selection.
///
/// # Errors
///
/// Returns any DNS evidence error or a connect-time address mismatch.
pub fn validate_connected_address(
    expected_host: &str,
    evidence: &DnsEvidence,
    connected: IpAddr,
) -> Result<(), IpPolicyError> {
    validate_dns_evidence(expected_host, evidence, ProxyEvidence::Direct)?;
    if connected != evidence.selected {
        return Err(IpPolicyError::ConnectedAddressMismatch {
            selected: evidence.selected,
            connected,
        });
    }
    Ok(())
}

fn is_allowed_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_allowed_public_v4(address),
        IpAddr::V6(address) => is_allowed_public_v6(address),
    }
}

fn is_allowed_public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    let special = a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 192 && b == 31 && c == 196)
        || (a == 192 && b == 52 && c == 193)
        || (a == 192 && b == 175 && c == 48)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (a == 255 && b == 255 && c == 255 && d == 255);
    !special
}

fn is_allowed_public_v6(address: Ipv6Addr) -> bool {
    if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
        return false;
    }
    let segments = address.segments();
    if segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || (segments[0] == 0x0100 && segments[1] == 0)
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
        || (segments[0] & 0xfff0 == 0x3ff0)
        || segments[0] == 0x5f00
    {
        return false;
    }
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_allowed_public_v4(mapped);
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolKey {
    pub scheme: String,
    pub origin: String,
    pub proxy_identity: String,
    pub tls_identity: String,
    pub http_identity: String,
    pub fingerprint_identity: String,
    pub client_certificate_identity: String,
    pub session_partition: String,
    pub credential_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolUse {
    pub connection_id: String,
    pub reused: bool,
    pub key_digest: String,
}

#[derive(Debug, Clone)]
pub struct PoolRegistry {
    entries: BTreeMap<PoolKey, String>,
    maximum_entries: usize,
    next_connection: u64,
}

impl PoolRegistry {
    #[must_use]
    pub fn new(maximum_entries: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            maximum_entries,
            next_connection: 1,
        }
    }

    /// Acquires or creates a connection identity for the complete pool key.
    ///
    /// # Errors
    ///
    /// Returns a transport error when key serialization fails or the pool bound is
    /// exceeded.
    pub fn acquire(&mut self, key: PoolKey) -> Result<PoolUse, TransportError> {
        key.validate()?;
        let key_digest = digest_json(&key)?;
        if let Some(connection_id) = self.entries.get(&key) {
            return Ok(PoolUse {
                connection_id: connection_id.clone(),
                reused: true,
                key_digest,
            });
        }
        if self.entries.len() >= self.maximum_entries {
            return Err(TransportError::Bound {
                kind: "pool_entries",
                observed: (self.entries.len() + 1) as u64,
                maximum: self.maximum_entries as u64,
            });
        }
        let connection_id = format!("connection_{}", self.next_connection);
        self.next_connection = self.next_connection.saturating_add(1);
        self.entries.insert(key, connection_id.clone());
        Ok(PoolUse {
            connection_id,
            reused: false,
            key_digest,
        })
    }

    pub fn discard(&mut self, key: &PoolKey) -> bool {
        self.entries.remove(key).is_some()
    }
}

impl PoolKey {
    fn validate(&self) -> Result<(), TransportError> {
        for (field, value) in [
            ("scheme", self.scheme.as_str()),
            ("origin", self.origin.as_str()),
            ("proxy_identity", self.proxy_identity.as_str()),
            ("tls_identity", self.tls_identity.as_str()),
            ("http_identity", self.http_identity.as_str()),
            ("fingerprint_identity", self.fingerprint_identity.as_str()),
            (
                "client_certificate_identity",
                self.client_certificate_identity.as_str(),
            ),
            ("session_partition", self.session_partition.as_str()),
            ("credential_scope", self.credential_scope.as_str()),
        ] {
            if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return Err(TransportError::Policy(format!(
                    "FF-TRANSPORT-E-POOL-KEY: {field}"
                )));
            }
        }
        let origin = HttpUrl::parse(&self.origin)?;
        if origin.scheme != self.scheme || origin.path_and_query != "/" {
            return Err(TransportError::Policy(
                "FF-TRANSPORT-E-POOL-ORIGIN".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteCredits {
    available: u64,
    accepted: u64,
    maximum_body_bytes: u64,
}

impl ByteCredits {
    #[must_use]
    pub fn new(maximum_body_bytes: u64) -> Self {
        Self {
            available: 0,
            accepted: 0,
            maximum_body_bytes,
        }
    }

    /// Grants explicit downstream byte credit.
    ///
    /// # Errors
    ///
    /// Returns a transport error if the credit counter overflows.
    pub fn grant(&mut self, bytes: u64) -> Result<(), TransportError> {
        self.available = self
            .available
            .checked_add(bytes)
            .ok_or(TransportError::Bound {
                kind: "byte_credit_overflow",
                observed: u64::MAX,
                maximum: self.maximum_body_bytes,
            })?;
        Ok(())
    }

    /// Proves a proposed read fits both current credit and the body ceiling
    /// before allocation or I/O admission.
    ///
    /// # Errors
    ///
    /// Returns the same typed credit/body bound that acceptance would return.
    pub fn preflight(&self, bytes: u64) -> Result<(), TransportError> {
        if bytes > self.available {
            return Err(TransportError::Bound {
                kind: "byte_credit",
                observed: bytes,
                maximum: self.available,
            });
        }
        let next = self
            .accepted
            .checked_add(bytes)
            .ok_or(TransportError::Bound {
                kind: "body_bytes",
                observed: u64::MAX,
                maximum: self.maximum_body_bytes,
            })?;
        if next > self.maximum_body_bytes {
            return Err(TransportError::Bound {
                kind: "body_bytes",
                observed: next,
                maximum: self.maximum_body_bytes,
            });
        }
        Ok(())
    }

    /// Accepts bytes only within available credit and the body ceiling.
    ///
    /// # Errors
    ///
    /// Returns a transport error for insufficient credit, overflow, or body excess.
    pub fn accept(&mut self, bytes: u64) -> Result<(), TransportError> {
        self.preflight(bytes)?;
        let next = self.accepted + bytes;
        self.available -= bytes;
        self.accepted = next;
        Ok(())
    }

    #[must_use]
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    maximum_retries: u8,
    retries_used: u8,
}

impl RetryBudget {
    #[must_use]
    pub fn new(maximum_retries: u8) -> Self {
        Self {
            maximum_retries,
            retries_used: 0,
        }
    }

    /// Admits one retry within the declared retry ceiling.
    ///
    /// # Errors
    ///
    /// Returns a typed retry-limit error when the next retry would exceed the
    /// configured maximum.
    pub fn retry(&mut self) -> Result<u8, TransportError> {
        if self.retries_used >= self.maximum_retries {
            return Err(TransportError::Bound {
                kind: "retry_attempts",
                observed: u64::from(self.retries_used) + 1,
                maximum: u64::from(self.maximum_retries),
            });
        }
        self.retries_used += 1;
        Ok(self.retries_used)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct BodyBudget {
    pub maximum_metadata_bytes: u64,
    pub maximum_body_bytes: u64,
    pub maximum_decompressed_bytes: u64,
}

impl BodyBudget {
    /// Validates metadata, wire body, and decompressed body counts independently.
    ///
    /// # Errors
    ///
    /// Returns a transport error naming the first exceeded bound.
    pub fn validate(
        &self,
        metadata_bytes: u64,
        body_bytes: u64,
        decompressed_bytes: u64,
    ) -> Result<(), TransportError> {
        for (kind, observed, maximum) in [
            (
                "metadata_bytes",
                metadata_bytes,
                self.maximum_metadata_bytes,
            ),
            ("body_bytes", body_bytes, self.maximum_body_bytes),
            (
                "decompressed_bytes",
                decompressed_bytes,
                self.maximum_decompressed_bytes,
            ),
        ] {
            if observed > maximum {
                return Err(TransportError::Bound {
                    kind,
                    observed,
                    maximum,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationStatus {
    Running,
    CancellationRequested,
    AcknowledgedCancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationModel {
    request_id: String,
    generation: u64,
    status: CancellationStatus,
}

impl CancellationModel {
    /// Starts a correlated cancellation state machine for one request.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the request identifier is empty.
    pub fn new(request_id: impl Into<String>) -> Result<Self, TransportError> {
        let request_id = request_id.into();
        if request_id.is_empty() {
            return Err(TransportError::Cancellation(
                "FF-TRANSPORT-E-CANCEL-ID: request ID is empty".to_owned(),
            ));
        }
        Ok(Self {
            request_id,
            generation: 0,
            status: CancellationStatus::Running,
        })
    }

    /// Requests cancellation and returns the new correlation generation.
    ///
    /// # Errors
    ///
    /// Returns a transport error for a repeated request or generation overflow.
    pub fn request(&mut self) -> Result<u64, TransportError> {
        if self.status != CancellationStatus::Running {
            return Err(TransportError::Cancellation(
                "FF-TRANSPORT-E-CANCEL-STATE: cancellation already requested".to_owned(),
            ));
        }
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            TransportError::Cancellation(
                "FF-TRANSPORT-E-CANCEL-GENERATION: generation overflow".to_owned(),
            )
        })?;
        self.status = CancellationStatus::CancellationRequested;
        Ok(self.generation)
    }

    /// Acknowledges cancellation only for the exact request and generation.
    ///
    /// # Errors
    ///
    /// Returns a transport error when state, request identity, or generation differs.
    pub fn acknowledge(&mut self, request_id: &str, generation: u64) -> Result<(), TransportError> {
        if self.status != CancellationStatus::CancellationRequested
            || self.request_id != request_id
            || self.generation != generation
        {
            return Err(TransportError::Cancellation(
                "FF-TRANSPORT-E-CANCEL-CORRELATION: acknowledgement mismatch".to_owned(),
            ));
        }
        self.status = CancellationStatus::AcknowledgedCancelled;
        Ok(())
    }

    #[must_use]
    pub fn pool_reusable(&self) -> bool {
        self.status == CancellationStatus::Running
    }

    #[cfg(test)]
    #[must_use]
    pub fn status(&self) -> CancellationStatus {
        self.status
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizedExchange {
    pub request_id: String,
    pub method: String,
    pub url: String,
    pub request_headers: BTreeMap<String, String>,
    pub status: u16,
    pub response_headers: BTreeMap<String, String>,
    pub body_sha256: String,
}

/// Produces a deterministic secret-sanitized exchange record.
///
/// # Errors
///
/// Returns a transport error when response headers are invalid or serialization
/// required by the record cannot be completed.
pub fn sanitize_exchange(
    request_id: impl Into<String>,
    method: impl Into<String>,
    url: &HttpUrl,
    request_headers: &BTreeMap<String, HeaderValue>,
    status: u16,
    response_headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<SanitizedExchange, TransportError> {
    let request_id = request_id.into();
    let mut sanitized_request = BTreeMap::new();
    for (name, value) in request_headers {
        let normalized = normalize_transcript_header_name(name)?;
        let sanitized = if value.sensitive || is_secret_header(&normalized) {
            placeholder_for_header(&normalized)
        } else {
            value.value.clone()
        };
        sanitized_request.insert(normalized, sanitized);
    }
    let mut sanitized_response = BTreeMap::new();
    for (name, value) in response_headers {
        let normalized = normalize_transcript_header_name(name)?;
        let sanitized = if is_secret_header(&normalized) {
            placeholder_for_header(&normalized)
        } else if normalized == "date" {
            "{{TIMESTAMP}}".to_owned()
        } else if normalized == "location" {
            sanitize_location(value)
        } else {
            value.clone()
        };
        if sanitized.chars().any(char::is_control) || sanitized.len() > MAX_HEADER_VALUE_BYTES {
            return Err(TransportError::InvalidHeader(
                "FF-TRANSPORT-E-TRANSCRIPT-HEADER: invalid response header".to_owned(),
            ));
        }
        sanitized_response.insert(normalized, sanitized);
    }
    Ok(SanitizedExchange {
        request_id,
        method: method.into(),
        url: sanitize_url(url),
        request_headers: sanitized_request,
        status,
        response_headers: sanitized_response,
        body_sha256: encode_hex(&Sha256::digest(body)),
    })
}

fn normalize_transcript_header_name(name: &str) -> Result<String, TransportError> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > MAX_HEADER_NAME_BYTES
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(TransportError::InvalidHeader(
            "FF-TRANSPORT-E-TRANSCRIPT-HEADER-NAME".to_owned(),
        ));
    }
    Ok(normalized)
}

fn sanitize_location(value: &str) -> String {
    HttpUrl::parse(value).map_or_else(|_| "{{LOCATION}}".to_owned(), |url| sanitize_url(&url))
}

fn is_secret_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "x-auth-token"
            | "x-access-token"
    ) || name.contains("token")
        || name.ends_with("-key")
}

fn placeholder_for_header(name: &str) -> String {
    match name {
        "authorization" => "{{AUTHORIZATION}}",
        "proxy-authorization" => "{{PROXY_AUTHORIZATION}}",
        "cookie" => "{{COOKIE}}",
        "set-cookie" => "{{SET_COOKIE}}",
        "x-api-key" => "{{API_KEY}}",
        _ => "{{SECRET}}",
    }
    .to_owned()
}

fn sanitize_url(url: &HttpUrl) -> String {
    let Some((path, query)) = url.path_and_query.split_once('?') else {
        return url.render();
    };
    let sanitized_query = query
        .split('&')
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "token"
                    | "access_token"
                    | "sig"
                    | "signature"
                    | "key"
                    | "api_key"
                    | "auth"
                    | "authorization"
            ) {
                format!("{key}={{{{QUERY_TOKEN}}}}")
            } else {
                let _ = value;
                format!("{key}={{{{QUERY_VALUE}}}}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    format!(
        "{}://{}:{}{}?{}",
        url.scheme, url.host, url.port, path, sanitized_query
    )
}

fn is_origin_bound_header(name: &str) -> bool {
    matches!(
        name,
        "authorization" | "proxy-authorization" | "cookie" | "host" | "origin" | "referer"
    ) || is_secret_header(name)
}

fn digest_json<T: Serialize>(value: &T) -> Result<String, TransportError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-DIGEST: {error}")))?;
    Ok(encode_hex(&Sha256::digest(bytes)))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_key(partition: &str, fingerprint: &str) -> PoolKey {
        PoolKey {
            scheme: "https".to_owned(),
            origin: "https://media.invalid:443".to_owned(),
            proxy_identity: "direct".to_owned(),
            tls_identity: "tls-standard".to_owned(),
            http_identity: "http2-standard".to_owned(),
            fingerprint_identity: fingerprint.to_owned(),
            client_certificate_identity: "none".to_owned(),
            session_partition: partition.to_owned(),
            credential_scope: "origin-media".to_owned(),
        }
    }

    #[test]
    fn blocked_fingerprint_never_executes_as_standard_http() {
        let decision = CandidateAdapter::std_first()
            .negotiate([Capability::Http11, Capability::TlsFingerprint]);
        assert!(!decision.execution_allowed);
        assert_eq!(decision.satisfied, BTreeSet::from([Capability::Http11]));
        assert_eq!(decision.blocked.len(), 1);
        assert_eq!(decision.blocked[0].capability, Capability::TlsFingerprint);
    }

    #[test]
    fn redirect_strips_sensitive_cross_origin_and_rejects_downgrade() {
        let mut request = HttpRequest::new(
            "request_redirect",
            "GET",
            HttpUrl::parse("https://a.invalid/path").expect("valid fixture URL"),
        )
        .expect("valid request");
        request
            .insert_header(
                "Authorization",
                HeaderValue::new("secret", true, true).expect("valid fixture header"),
            )
            .expect("header inserts");
        request
            .insert_header(
                "Accept",
                HeaderValue::new("*/*", false, false).expect("valid fixture header"),
            )
            .expect("header inserts");
        let policy = RedirectPolicy {
            maximum_hops: 3,
            reject_https_downgrade: true,
        };
        let redirected = policy
            .apply(
                &request,
                HttpUrl::parse("https://b.invalid/next").expect("valid fixture URL"),
            )
            .expect("cross-origin redirect is allowed");
        assert!(!redirected.request.headers.contains_key("authorization"));
        assert!(redirected.request.headers.contains_key("accept"));
        assert_eq!(redirected.stripped_headers, ["authorization"]);
        assert!(
            policy
                .apply(
                    &request,
                    HttpUrl::parse("http://a.invalid/next").expect("valid fixture URL")
                )
                .is_err()
        );
    }

    #[test]
    fn cookie_scope_and_public_suffix_fail_closed() {
        let suffixes = PublicSuffixSet::new(["com".to_owned(), "invalid".to_owned()]);
        let source =
            HttpUrl::parse("https://media.example.com/account").expect("valid fixture URL");
        let mut jar = CookieJar::new(4);
        jar.store(
            &source,
            Cookie {
                name: "session".to_owned(),
                value: "secret".to_owned(),
                domain: "media.example.com".to_owned(),
                host_only: true,
                path: "/account".to_owned(),
                secure: true,
            },
            &suffixes,
        )
        .expect("host-only cookie stores");
        assert_eq!(jar.values_for(&source).len(), 1);
        assert!(
            jar.values_for(
                &HttpUrl::parse("http://media.example.com/account").expect("valid fixture URL")
            )
            .is_empty()
        );
        assert!(
            jar.store(
                &source,
                Cookie {
                    name: "bad".to_owned(),
                    value: "value".to_owned(),
                    domain: "com".to_owned(),
                    host_only: false,
                    path: "/".to_owned(),
                    secure: false,
                },
                &suffixes
            )
            .is_err()
        );
    }

    #[test]
    fn dns_rejects_any_special_answer_and_rebinding() {
        let mixed = DnsEvidence {
            query_host: "media.invalid".to_owned(),
            answers: vec![
                "93.184.216.34".parse().expect("valid fixture IP"),
                "127.0.0.1".parse().expect("valid fixture IP"),
            ],
            selected: "93.184.216.34".parse().expect("valid fixture IP"),
            resolver_identity: "fixture-resolver".to_owned(),
        };
        assert!(matches!(
            validate_dns_evidence("media.invalid", &mixed, ProxyEvidence::Direct),
            Err(IpPolicyError::SpecialUse(_))
        ));
        let public = DnsEvidence {
            answers: vec!["93.184.216.34".parse().expect("valid fixture IP")],
            ..mixed
        };
        assert!(matches!(
            validate_connected_address(
                "media.invalid",
                &public,
                "93.184.216.35".parse().expect("valid fixture IP")
            ),
            Err(IpPolicyError::ConnectedAddressMismatch { .. })
        ));
    }

    #[test]
    fn pool_keys_partition_sessions_and_fingerprints() {
        let mut pool = PoolRegistry::new(4);
        let first = pool
            .acquire(pool_key("partition_a", "profile_a"))
            .expect("first key admitted");
        let reused = pool
            .acquire(pool_key("partition_a", "profile_a"))
            .expect("same key admitted");
        let other_session = pool
            .acquire(pool_key("partition_b", "profile_a"))
            .expect("other session admitted");
        let other_fingerprint = pool
            .acquire(pool_key("partition_a", "profile_b"))
            .expect("other fingerprint admitted");
        assert!(!first.reused);
        assert!(reused.reused);
        assert_eq!(first.connection_id, reused.connection_id);
        assert_ne!(first.connection_id, other_session.connection_id);
        assert_ne!(first.connection_id, other_fingerprint.connection_id);
    }

    #[test]
    fn byte_credit_and_cancellation_require_exact_acknowledgement() {
        let mut credits = ByteCredits::new(8);
        credits.grant(4).expect("credit grant");
        credits.accept(4).expect("exact credit");
        assert!(credits.accept(1).is_err());

        let mut cancellation =
            CancellationModel::new("request_cancel").expect("valid cancellation");
        let generation = cancellation.request().expect("request cancellation");
        assert!(!cancellation.pool_reusable());
        assert!(
            cancellation
                .acknowledge("request_other", generation)
                .is_err()
        );
        assert_eq!(
            cancellation.status(),
            CancellationStatus::CancellationRequested
        );
        cancellation
            .acknowledge("request_cancel", generation)
            .expect("exact acknowledgement");
        assert_eq!(
            cancellation.status(),
            CancellationStatus::AcknowledgedCancelled
        );
    }

    #[test]
    fn ip_domain_cookie_cannot_cross_ip_origins() {
        let suffixes = PublicSuffixSet::new(["com".to_owned()]);
        let source = HttpUrl::parse("http://127.0.0.1/").expect("fixture URL");
        let error = CookieJar::new(4)
            .store(
                &source,
                Cookie {
                    name: "session".to_owned(),
                    value: "secret".to_owned(),
                    domain: "0.0.1".to_owned(),
                    host_only: false,
                    path: "/".to_owned(),
                    secure: false,
                },
                &suffixes,
            )
            .expect_err("IP Domain attribute must fail");
        assert!(error.to_string().contains("COOKIE-IP-DOMAIN"));
    }

    #[test]
    fn dns_provenance_and_special_ipv6_fail_closed() {
        let mut evidence = DnsEvidence {
            query_host: "other.example".to_owned(),
            answers: vec!["93.184.216.34".parse().expect("fixture IP")],
            selected: "93.184.216.34".parse().expect("fixture IP"),
            resolver_identity: String::new(),
        };
        assert_eq!(
            validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct),
            Err(IpPolicyError::ProvenanceMismatch)
        );
        evidence.query_host = "media.example".to_owned();
        evidence.resolver_identity = "fixture-resolver".to_owned();
        evidence.answers = vec!["100::1".parse().expect("fixture IPv6")];
        evidence.selected = evidence.answers[0];
        assert!(matches!(
            validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct),
            Err(IpPolicyError::SpecialUse(_))
        ));
        for special in ["5f00::1", "192.31.196.1", "192.52.193.1", "192.175.48.1"] {
            evidence.answers = vec![special.parse().expect("special-purpose fixture IP")];
            evidence.selected = evidence.answers[0];
            assert!(
                matches!(
                    validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct),
                    Err(IpPolicyError::SpecialUse(_))
                ),
                "{special} must fail closed"
            );
        }
    }

    #[test]
    fn adapter_rejects_empty_capabilities_and_redirect_special_use() {
        let empty = CandidateAdapter::std_first().execute([], |_| Ok("must-not-run"));
        assert!(
            matches!(empty, Err(TransportError::Policy(message)) if message.contains("CAPABILITY-EMPTY"))
        );

        let request = HttpRequest::new(
            "redirect-special",
            "GET",
            HttpUrl::parse("https://media.example/public").expect("source URL"),
        )
        .expect("request");
        let error = RedirectPolicy {
            maximum_hops: 3,
            reject_https_downgrade: true,
        }
        .apply(
            &request,
            HttpUrl::parse("https://127.0.0.1/admin").expect("target URL"),
        )
        .expect_err("special-use redirect must fail");
        assert!(error.to_string().contains("REDIRECT-SSRF"));
    }

    #[test]
    fn sanitizer_redacts_aliases_arbitrary_queries_and_locations() {
        let canary = "secret-canary";
        let url = HttpUrl::parse(&format!(
            "https://media.example/file?access_token={canary}&quality={canary}"
        ))
        .expect("fixture URL");
        let request = BTreeMap::from([(
            "x-auth-token".to_owned(),
            HeaderValue::new(canary, false, false).expect("fixture header"),
        )]);
        let response = BTreeMap::from([(
            "location".to_owned(),
            format!("https://cdn.example/file?signature={canary}"),
        )]);
        let exchange = sanitize_exchange(
            "request_sanitize",
            "GET",
            &url,
            &request,
            302,
            &response,
            b"",
        )
        .expect("sanitize");
        assert!(
            !serde_json::to_string(&exchange)
                .expect("JSON")
                .contains(canary)
        );
    }
}
