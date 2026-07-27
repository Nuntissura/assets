#[cfg(test)]
use crate::local_server::{CapturedRequest, LocalProtocolServer};
use crate::policy::{
    BlockedCapability, CandidateAdapter, Capability, CapabilityDecision, TransportError,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;
use wreq_util::{Emulation, Platform, Profile};

const BACKEND_ID: &str = "wreq";
const BACKEND_VERSION: &str = "6.0.0-rc.29";
const PROFILE_ID: &str = "chrome-136-windows";
const LIVE_WIRE_URL: &str = "https://tls.peet.ws/api/all";
const MAX_WIRE_RECEIPT_BYTES: u64 = 1024 * 1024;
#[cfg(test)]
const RAW_GZIP_BYTES: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 0, 10, 75, 75, 45, 42, 202, 76, 214, 77, 175, 202, 44, 208, 45, 72,
    172, 204, 201, 79, 76, 1, 0, 191, 75, 160, 215, 19, 0, 0, 0,
];

#[derive(Debug, Clone, Copy)]
struct ClientLimits {
    connect_timeout: Duration,
    read_timeout: Duration,
    total_timeout: Duration,
    maximum_idle_per_host: usize,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_secs(10),
            maximum_idle_per_host: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintProfile {
    None,
    Chrome136Windows,
    Chrome149Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvenienceState {
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConveniences {
    pub ambient_proxy: ConvenienceState,
    pub automatic_redirects: ConvenienceState,
    pub internal_cookie_store: ConvenienceState,
    pub transparent_decompression: ConvenienceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum EvidenceState {
    Observed(String),
    Unavailable(String),
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum StructuralWireEvidence {
    NotProvided,
    ObservationOnly { receipt_sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjudicationObservation {
    backend_id: String,
    backend_version: String,
    requested_profile: FingerprintProfile,
    configured_profile: Option<String>,
    transport_preflight: CapabilityDecision,
    semantic_capabilities: CapabilityDecision,
    wire_evidence: StructuralWireEvidence,
    policy_conveniences: PolicyConveniences,
    profile_client_partition: String,
    protocol: String,
    peer_evidence: EvidenceState,
    proxy_evidence: EvidenceState,
    dns_evidence: EvidenceState,
    alpn_evidence: EvidenceState,
    status: u16,
    body_bytes: u64,
    body_sha256: String,
}

impl AdjudicationObservation {
    #[must_use]
    pub fn configured_profile(&self) -> Option<&str> {
        self.configured_profile.as_deref()
    }

    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn body_bytes(&self) -> u64 {
        self.body_bytes
    }

    #[must_use]
    pub fn body_sha256(&self) -> &str {
        &self.body_sha256
    }

    #[must_use]
    pub fn profile_client_partition(&self) -> &str {
        &self.profile_client_partition
    }

    #[must_use]
    pub fn transport_preflight(&self) -> &CapabilityDecision {
        &self.transport_preflight
    }

    #[must_use]
    pub fn semantic_capabilities(&self) -> &CapabilityDecision {
        &self.semantic_capabilities
    }

    #[must_use]
    pub fn wire_evidence(&self) -> &StructuralWireEvidence {
        &self.wire_evidence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg(test)]
pub(crate) enum LocalProbeScenario {
    Ok,
    Redirect,
    SetCookie,
    Gzip,
    HugeDeclaredBody,
    UnknownLengthOversize,
}

#[cfg(test)]
impl LocalProbeScenario {
    fn path(self) -> &'static str {
        match self {
            Self::Ok => "/ok",
            Self::Redirect => "/redirect",
            Self::SetCookie => "/set-cookie",
            Self::Gzip => "/gzip",
            Self::HugeDeclaredBody => "/declared-oversize",
            Self::UnknownLengthOversize => "/chunked-oversize",
        }
    }

    fn has_body_fragment(self) -> bool {
        matches!(self, Self::Ok | Self::Gzip | Self::UnknownLengthOversize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
pub(crate) struct LocalProbeRequest {
    pub scenario: LocalProbeScenario,
    pub profile: FingerprintProfile,
    pub maximum_body_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
pub(crate) struct LocalProbeReceipt {
    request_line: String,
    header_names: BTreeSet<String>,
    user_agent: Option<String>,
    sec_ch_ua_platform: Option<String>,
    accept_encoding: Option<String>,
    normalized_wire_sha256: String,
}

#[cfg(test)]
impl LocalProbeReceipt {
    #[must_use]
    pub fn request_line(&self) -> &str {
        &self.request_line
    }

    #[must_use]
    pub fn header_names(&self) -> &BTreeSet<String> {
        &self.header_names
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
pub(crate) struct LocalProbeReport {
    observation: AdjudicationObservation,
    receipt: LocalProbeReceipt,
}

#[cfg(test)]
impl LocalProbeReport {
    #[must_use]
    pub fn observation(&self) -> &AdjudicationObservation {
        &self.observation
    }

    #[must_use]
    pub fn receipt(&self) -> &LocalProbeReceipt {
        &self.receipt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveProbeOptions {
    pub maximum_body_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileParityStatus {
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FingerprintReceipt {
    schema_version: String,
    endpoint_id: String,
    backend_id: String,
    backend_version: String,
    configured_profile: String,
    profile_parity: ProfileParityStatus,
    negotiated_protocol: String,
    tls_ja3_hash: String,
    tls_ja4: String,
    http2_akamai_fingerprint: String,
    http2_akamai_fingerprint_hash: String,
    response_sha256: String,
}

impl FingerprintReceipt {
    #[must_use]
    pub fn profile_parity(&self) -> ProfileParityStatus {
        self.profile_parity
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveWireProbeReport {
    schema_version: String,
    evidence_class: String,
    observation: AdjudicationObservation,
    receipt: FingerprintReceipt,
}

impl LiveWireProbeReport {
    #[must_use]
    pub fn observation(&self) -> &AdjudicationObservation {
        &self.observation
    }

    #[must_use]
    pub fn receipt(&self) -> &FingerprintReceipt {
        &self.receipt
    }
}

/// Strictly consumes a serialized live report and rejects identity, capability,
/// structure, parity, and receipt-hash drift.
///
/// This validates internal report integrity only. It does not establish
/// freshness, endpoint authenticity, or independent browser parity.
///
/// # Errors
///
/// Returns a typed wire-receipt error when the input is oversized, malformed,
/// mutated, or inconsistent.
pub fn validate_live_wire_probe_report(
    bytes: &[u8],
) -> Result<LiveWireProbeReport, AdjudicationError> {
    if bytes.len() > usize::try_from(MAX_WIRE_RECEIPT_BYTES).unwrap_or(usize::MAX) {
        return Err(AdjudicationError::WireReceipt(
            "serialized live report exceeds 1048576 bytes".to_owned(),
        ));
    }
    let report: LiveWireProbeReport = serde_json::from_slice(bytes).map_err(|_| {
        AdjudicationError::WireReceipt("invalid strict live report JSON".to_owned())
    })?;
    let expected_transport = transport_capabilities(FingerprintProfile::Chrome136Windows);
    let expected_semantics = BTreeSet::from([
        Capability::TlsFingerprint,
        Capability::Http2Fingerprint,
        Capability::PoolPartition,
    ]);
    let receipt_json = serde_json::to_vec(&report.receipt)
        .map_err(|_| AdjudicationError::WireReceipt("receipt encoding failed".to_owned()))?;
    let expected_receipt_hash = encode_hex(&Sha256::digest(receipt_json));
    let receipt_hash_matches = matches!(
        &report.observation.wire_evidence,
        StructuralWireEvidence::ObservationOnly { receipt_sha256 }
            if receipt_sha256 == &expected_receipt_hash
    );
    let one_pool_blocker = report.observation.semantic_capabilities.blocked.len() == 1
        && report.observation.semantic_capabilities.blocked[0].capability
            == Capability::PoolPartition;
    if report.schema_version != "ff-wreq-live-probe-v1"
        || report.evidence_class != "structural_observation_only"
        || report.observation.backend_id != BACKEND_ID
        || report.observation.backend_version != BACKEND_VERSION
        || report.observation.requested_profile != FingerprintProfile::Chrome136Windows
        || report.observation.configured_profile.as_deref() != Some(PROFILE_ID)
        || report.observation.status != 200
        || !matches!(report.observation.protocol.as_str(), "HTTP/2.0" | "HTTP/2")
        || report.observation.transport_preflight.requested != expected_transport
        || report.observation.transport_preflight.satisfied != expected_transport
        || !report.observation.transport_preflight.blocked.is_empty()
        || !report.observation.transport_preflight.execution_allowed
        || report.observation.semantic_capabilities.requested != expected_semantics
        || report.observation.semantic_capabilities.satisfied
            != BTreeSet::from([Capability::TlsFingerprint, Capability::Http2Fingerprint])
        || report.observation.semantic_capabilities.execution_allowed
        || !one_pool_blocker
        || !receipt_hash_matches
        || report.receipt.schema_version != "ff-fingerprint-observation-v1"
        || report.receipt.endpoint_id != "tls-peet-api-all"
        || report.receipt.backend_id != BACKEND_ID
        || report.receipt.backend_version != BACKEND_VERSION
        || report.receipt.configured_profile != PROFILE_ID
        || report.receipt.profile_parity != ProfileParityStatus::NotAssessed
        || report.receipt.negotiated_protocol != "h2"
        || report.receipt.response_sha256 != report.observation.body_sha256
        || !is_lower_hex(&report.receipt.tls_ja3_hash, 32)
        || !valid_ja4(&report.receipt.tls_ja4)
        || !valid_akamai_fingerprint(&report.receipt.http2_akamai_fingerprint)
        || !is_lower_hex(&report.receipt.http2_akamai_fingerprint_hash, 32)
    {
        return Err(AdjudicationError::WireReceipt(
            "live report failed strict identity or consistency validation".to_owned(),
        ));
    }
    Ok(report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationError {
    InvalidRequest(String),
    ClientBuild(String),
    Runtime(String),
    NestedRuntime,
    Request(String),
    UnsupportedProfile(String),
    CapabilityBlocked(Vec<BlockedCapability>),
    BodyTooLarge { maximum: u64, observed: u64 },
    Harness(String),
    WireReceipt(String),
}

impl fmt::Display for AdjudicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(detail) => {
                write!(formatter, "FF-WREQ-E-INVALID-REQUEST: {detail}")
            }
            Self::ClientBuild(detail) => write!(formatter, "FF-WREQ-E-CLIENT-BUILD: {detail}"),
            Self::Runtime(detail) => write!(formatter, "FF-WREQ-E-RUNTIME: {detail}"),
            Self::NestedRuntime => write!(
                formatter,
                "FF-WREQ-E-NESTED-RUNTIME: synchronous adjudication is unavailable inside a Tokio runtime"
            ),
            Self::Request(detail) => write!(formatter, "FF-WREQ-E-REQUEST: {detail}"),
            Self::UnsupportedProfile(profile) => {
                write!(formatter, "FF-WREQ-E-UNSUPPORTED-PROFILE: {profile}")
            }
            Self::CapabilityBlocked(blocked) => {
                write!(formatter, "FF-WREQ-E-CAPABILITY-BLOCKED: {blocked:?}")
            }
            Self::BodyTooLarge { maximum, observed } => write!(
                formatter,
                "FF-WREQ-E-BODY-LIMIT: maximum={maximum}, observed={observed}"
            ),
            Self::Harness(detail) => write!(formatter, "FF-WREQ-E-HARNESS: {detail}"),
            Self::WireReceipt(detail) => write!(formatter, "FF-WREQ-E-WIRE-RECEIPT: {detail}"),
        }
    }
}

impl std::error::Error for AdjudicationError {}

impl From<TransportError> for AdjudicationError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::CapabilityBlocked(blocked) => Self::CapabilityBlocked(blocked),
            _ => Self::Harness("transport boundary rejected operation".to_owned()),
        }
    }
}

pub struct WreqAdjudicationAdapter {
    capability_boundary: CandidateAdapter,
    limits: ClientLimits,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    ordinary_client: wreq::Client,
    chrome136_windows_client: wreq::Client,
}

impl fmt::Debug for WreqAdjudicationAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WreqAdjudicationAdapter")
            .field("backend", &BACKEND_ID)
            .field("version", &BACKEND_VERSION)
            .field("pool_max_idle_per_host", &self.limits.maximum_idle_per_host)
            .finish_non_exhaustive()
    }
}

impl WreqAdjudicationAdapter {
    /// Builds the exact-pinned non-shipped observation adapter.
    ///
    /// # Errors
    ///
    /// Returns a stable client construction error.
    pub fn new() -> Result<Self, AdjudicationError> {
        Self::with_limits(ClientLimits::default())
    }

    fn with_limits(limits: ClientLimits) -> Result<Self, AdjudicationError> {
        reject_nested_runtime()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| AdjudicationError::Runtime("runtime construction failed".to_owned()))?;
        let ordinary_client = client_builder(limits)
            .build()
            .map_err(classify_client_build_error)?;
        let emulation = Emulation::builder()
            .profile(Profile::Chrome136)
            .platform(Platform::Windows)
            .http2(true)
            .headers(true)
            .build();
        let chrome136_windows_client = client_builder(limits)
            .emulation(emulation)
            .build()
            .map_err(classify_client_build_error)?;
        Ok(Self {
            capability_boundary: CandidateAdapter::wreq_adjudication(),
            limits,
            runtime: Mutex::new(Some(runtime)),
            ordinary_client,
            chrome136_windows_client,
        })
    }

    /// Runs one enumerated local regression. The caller cannot supply a URL,
    /// address, authorization token, or header.
    ///
    /// # Errors
    ///
    /// Returns a typed profile, capability, network, body-bound, or harness error.
    #[cfg(test)]
    pub(crate) fn execute_local_probe(
        &self,
        request: LocalProbeRequest,
    ) -> Result<LocalProbeReport, AdjudicationError> {
        reject_nested_runtime()?;
        validate_body_bound(request.maximum_body_bytes)?;
        self.client_for(request.profile)?;
        let server = LocalProtocolServer::spawn().map_err(AdjudicationError::from)?;
        let target = server.target();
        let execution = self.execute_target(
            request.profile,
            request.maximum_body_bytes,
            &target.url(request.scenario.path()),
            vec![(
                "x-ff-harness-authorization".to_owned(),
                target.authorization().to_owned(),
            )],
        );
        if request.scenario.has_body_fragment() {
            target
                .acknowledge_fragment()
                .map_err(AdjudicationError::from)?;
        }
        let captured = server.finish().map_err(AdjudicationError::from)?;
        let (observation, _) = execution?;
        Ok(LocalProbeReport {
            observation,
            receipt: sanitize_local_receipt(captured),
        })
    }

    /// Executes the single compile-time approved external endpoint.
    ///
    /// The returned receipt is structural observation only. It never asserts
    /// Chrome parity and leaves fingerprint and pool semantic capabilities blocked.
    ///
    /// # Errors
    ///
    /// Returns a typed failure for bounds, transport, HTTP/2, or receipt structure.
    pub fn execute_live_wire_probe(
        &self,
        options: LiveProbeOptions,
    ) -> Result<LiveWireProbeReport, AdjudicationError> {
        reject_nested_runtime()?;
        validate_body_bound(options.maximum_body_bytes)?;
        if options.maximum_body_bytes > MAX_WIRE_RECEIPT_BYTES {
            return Err(AdjudicationError::WireReceipt(
                "wire receipt bound exceeds 1048576 bytes".to_owned(),
            ));
        }
        let (mut observation, body) = self.execute_target(
            FingerprintProfile::Chrome136Windows,
            options.maximum_body_bytes,
            LIVE_WIRE_URL,
            Vec::new(),
        )?;
        let receipt = parse_structural_wire_receipt(&observation, &body)?;
        let receipt_json = serde_json::to_vec(&receipt)
            .map_err(|_| AdjudicationError::WireReceipt("receipt encoding failed".to_owned()))?;
        observation.wire_evidence = StructuralWireEvidence::ObservationOnly {
            receipt_sha256: encode_hex(&Sha256::digest(receipt_json)),
        };
        let report = LiveWireProbeReport {
            schema_version: "ff-wreq-live-probe-v1".to_owned(),
            evidence_class: "structural_observation_only".to_owned(),
            observation,
            receipt,
        };
        let encoded = serde_json::to_vec(&report)
            .map_err(|_| AdjudicationError::WireReceipt("report encoding failed".to_owned()))?;
        validate_live_wire_probe_report(&encoded)
    }

    fn client_for(
        &self,
        profile: FingerprintProfile,
    ) -> Result<(&wreq::Client, &'static str), AdjudicationError> {
        match profile {
            FingerprintProfile::None => Ok((&self.ordinary_client, "ordinary-profile-client")),
            FingerprintProfile::Chrome136Windows => Ok((
                &self.chrome136_windows_client,
                "chrome-136-windows-profile-client",
            )),
            FingerprintProfile::Chrome149Windows => Err(AdjudicationError::UnsupportedProfile(
                "chrome-149-windows".to_owned(),
            )),
        }
    }

    fn execute_target(
        &self,
        profile: FingerprintProfile,
        maximum_body_bytes: u64,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<(AdjudicationObservation, Vec<u8>), AdjudicationError> {
        reject_nested_runtime()?;
        validate_body_bound(maximum_body_bytes)?;
        validate_headers(&headers)?;
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| AdjudicationError::Runtime("runtime lock poisoned".to_owned()))?;
        let runtime = runtime
            .as_ref()
            .ok_or_else(|| AdjudicationError::Runtime("runtime is already shut down".to_owned()))?;
        let requested = transport_capabilities(profile);
        self.capability_boundary
            .execute_typed(requested.clone(), |grant| {
                for capability in requested {
                    grant.require(capability)?;
                }
                runtime.block_on(async {
                    let result = self
                        .execute_async(profile, maximum_body_bytes, url, headers)
                        .await;
                    tokio::task::yield_now().await;
                    result
                })
            })
            .map(|(decision, (mut observation, body))| {
                observation.transport_preflight = decision;
                (observation, body)
            })
    }

    async fn execute_async(
        &self,
        profile: FingerprintProfile,
        maximum_body_bytes: u64,
        url: &str,
        headers: Vec<(String, String)>,
    ) -> Result<(AdjudicationObservation, Vec<u8>), AdjudicationError> {
        let (client, profile_client_partition) = self.client_for(profile)?;
        let mut builder = client.get(url);
        for (name, value) in headers {
            builder = builder.header(&name, &value);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| classify_request_error(&error))?;
        let status = response.status().as_u16();
        let protocol = format!("{:?}", response.version());
        let peer = response.remote_addr();
        if let Some(length) = response.content_length()
            && length > maximum_body_bytes
        {
            return Err(AdjudicationError::BodyTooLarge {
                maximum: maximum_body_bytes,
                observed: length,
            });
        }
        let body = read_bounded_body(response, maximum_body_bytes).await?;
        let body_bytes = u64::try_from(body.len())
            .map_err(|_| AdjudicationError::Request("body length conversion failed".to_owned()))?;
        let configured_profile =
            (profile == FingerprintProfile::Chrome136Windows).then(|| PROFILE_ID.to_owned());
        Ok((
            AdjudicationObservation {
                backend_id: BACKEND_ID.to_owned(),
                backend_version: BACKEND_VERSION.to_owned(),
                requested_profile: profile,
                configured_profile,
                transport_preflight: empty_decision(),
                semantic_capabilities: semantic_capability_decision(profile),
                wire_evidence: StructuralWireEvidence::NotProvided,
                policy_conveniences: disabled_policy_conveniences(),
                profile_client_partition: profile_client_partition.to_owned(),
                protocol: protocol.clone(),
                peer_evidence: peer.map_or_else(
                    || {
                        EvidenceState::Unavailable(
                            "dependency did not expose peer address".to_owned(),
                        )
                    },
                    |address| EvidenceState::Observed(format_socket_address(address)),
                ),
                proxy_evidence: EvidenceState::Skipped(
                    "ambient and configured proxies are disabled for adjudication".to_owned(),
                ),
                dns_evidence: EvidenceState::Unavailable(
                    "resolver provenance is not exposed by this candidate".to_owned(),
                ),
                alpn_evidence: EvidenceState::Unavailable(
                    "ALPN evidence is not exposed independently of response protocol".to_owned(),
                ),
                status,
                body_bytes,
                body_sha256: encode_hex(&Sha256::digest(&body)),
            },
            body,
        ))
    }
}

impl Drop for WreqAdjudicationAdapter {
    fn drop(&mut self) {
        let runtime = self
            .runtime
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
    }
}

fn client_builder(limits: ClientLimits) -> wreq::ClientBuilder {
    wreq::Client::builder()
        .no_proxy()
        .redirect(wreq::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(limits.connect_timeout)
        .read_timeout(limits.read_timeout)
        .timeout(limits.total_timeout)
        .pool_max_idle_per_host(limits.maximum_idle_per_host)
}

fn transport_capabilities(profile: FingerprintProfile) -> BTreeSet<Capability> {
    let mut capabilities = BTreeSet::from([Capability::Http11, Capability::BodyBounds]);
    if profile == FingerprintProfile::Chrome136Windows {
        capabilities.insert(Capability::Http2);
        capabilities.insert(Capability::TlsFingerprint);
        capabilities.insert(Capability::Http2Fingerprint);
    }
    capabilities
}

fn semantic_capability_decision(profile: FingerprintProfile) -> CapabilityDecision {
    if profile == FingerprintProfile::Chrome136Windows {
        CandidateAdapter::wreq_adjudication().negotiate([
            Capability::TlsFingerprint,
            Capability::Http2Fingerprint,
            Capability::PoolPartition,
        ])
    } else {
        empty_decision()
    }
}

fn empty_decision() -> CapabilityDecision {
    CapabilityDecision {
        requested: BTreeSet::new(),
        satisfied: BTreeSet::new(),
        blocked: Vec::new(),
        execution_allowed: false,
    }
}

fn disabled_policy_conveniences() -> PolicyConveniences {
    PolicyConveniences {
        ambient_proxy: ConvenienceState::Disabled,
        automatic_redirects: ConvenienceState::Disabled,
        internal_cookie_store: ConvenienceState::Disabled,
        transparent_decompression: ConvenienceState::Disabled,
    }
}

async fn read_bounded_body(
    response: wreq::Response,
    maximum_body_bytes: u64,
) -> Result<Vec<u8>, AdjudicationError> {
    let mut stream = Box::pin(response.bytes_stream());
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| classify_request_error(&error))?;
        let current = u64::try_from(body.len())
            .map_err(|_| AdjudicationError::Request("body length conversion failed".to_owned()))?;
        let chunk_bytes = u64::try_from(chunk.len())
            .map_err(|_| AdjudicationError::Request("chunk length conversion failed".to_owned()))?;
        let observed = current
            .checked_add(chunk_bytes)
            .ok_or(AdjudicationError::BodyTooLarge {
                maximum: maximum_body_bytes,
                observed: u64::MAX,
            })?;
        if observed > maximum_body_bytes {
            return Err(AdjudicationError::BodyTooLarge {
                maximum: maximum_body_bytes,
                observed,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn reject_nested_runtime() -> Result<(), AdjudicationError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        Err(AdjudicationError::NestedRuntime)
    } else {
        Ok(())
    }
}

fn validate_body_bound(maximum: u64) -> Result<(), AdjudicationError> {
    if maximum == 0 {
        return Err(AdjudicationError::InvalidRequest(
            "response body bound must be nonzero".to_owned(),
        ));
    }
    Ok(())
}

fn validate_headers(headers: &[(String, String)]) -> Result<(), AdjudicationError> {
    let valid = headers.len() <= 64
        && headers.iter().all(|(name, value)| {
            !name.is_empty()
                && name.len() <= 256
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && value.len() <= 8 * 1024
                && !value.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
        });
    if !valid {
        return Err(AdjudicationError::InvalidRequest(
            "header validation failed".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn sanitize_local_receipt(captured: CapturedRequest) -> LocalProbeReceipt {
    LocalProbeReceipt {
        request_line: captured.request_line,
        header_names: captured
            .headers
            .keys()
            .filter(|name| name.as_str() != "x-ff-harness-authorization")
            .cloned()
            .collect(),
        user_agent: captured.headers.get("user-agent").cloned(),
        sec_ch_ua_platform: captured.headers.get("sec-ch-ua-platform").cloned(),
        accept_encoding: captured.headers.get("accept-encoding").cloned(),
        normalized_wire_sha256: captured.normalized_wire_sha256,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalWireEcho {
    http_version: String,
    tls: ExternalTlsEcho,
    http2: ExternalHttp2Echo,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalTlsEcho {
    ja3_hash: String,
    ja4: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalHttp2Echo {
    akamai_fingerprint: String,
    akamai_fingerprint_hash: String,
}

fn parse_structural_wire_receipt(
    observation: &AdjudicationObservation,
    body: &[u8],
) -> Result<FingerprintReceipt, AdjudicationError> {
    if observation.status != 200
        || observation.requested_profile != FingerprintProfile::Chrome136Windows
        || observation.configured_profile.as_deref() != Some(PROFILE_ID)
        || observation.wire_evidence != StructuralWireEvidence::NotProvided
        || !matches!(observation.protocol.as_str(), "HTTP/2.0" | "HTTP/2")
    {
        return Err(AdjudicationError::WireReceipt(
            "observation is not eligible for structural HTTP/2 receipt parsing".to_owned(),
        ));
    }
    let external: ExternalWireEcho = serde_json::from_slice(body)
        .map_err(|_| AdjudicationError::WireReceipt("invalid strict wire echo JSON".to_owned()))?;
    if external.http_version != "h2"
        || !is_lower_hex(&external.tls.ja3_hash, 32)
        || !valid_ja4(&external.tls.ja4)
        || !valid_akamai_fingerprint(&external.http2.akamai_fingerprint)
        || !is_lower_hex(&external.http2.akamai_fingerprint_hash, 32)
    {
        return Err(AdjudicationError::WireReceipt(
            "wire echo failed bounded structural validation".to_owned(),
        ));
    }
    Ok(FingerprintReceipt {
        schema_version: "ff-fingerprint-observation-v1".to_owned(),
        endpoint_id: "tls-peet-api-all".to_owned(),
        backend_id: observation.backend_id.clone(),
        backend_version: observation.backend_version.clone(),
        configured_profile: PROFILE_ID.to_owned(),
        profile_parity: ProfileParityStatus::NotAssessed,
        negotiated_protocol: external.http_version,
        tls_ja3_hash: external.tls.ja3_hash,
        tls_ja4: external.tls.ja4,
        http2_akamai_fingerprint: external.http2.akamai_fingerprint,
        http2_akamai_fingerprint_hash: external.http2.akamai_fingerprint_hash,
        response_sha256: observation.body_sha256.clone(),
    })
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_ja4(value: &str) -> bool {
    value.len() <= 256
        && value.split('_').count() == 3
        && value.split('_').all(|segment| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn valid_akamai_fingerprint(value: &str) -> bool {
    value.len() <= 4096
        && value.split('|').count() == 4
        && value.split('|').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b'|')
        })
}

fn classify_client_build_error(_: wreq::Error) -> AdjudicationError {
    AdjudicationError::ClientBuild("client construction failed".to_owned())
}

fn classify_request_error(error: &wreq::Error) -> AdjudicationError {
    let class = if error.is_timeout() {
        "timeout"
    } else if error.is_tls() {
        "tls"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "request"
    };
    AdjudicationError::Request(class.to_owned())
}

fn format_socket_address(address: SocketAddr) -> String {
    address.to_string()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn eligible_observation() -> AdjudicationObservation {
        AdjudicationObservation {
            backend_id: BACKEND_ID.to_owned(),
            backend_version: BACKEND_VERSION.to_owned(),
            requested_profile: FingerprintProfile::Chrome136Windows,
            configured_profile: Some(PROFILE_ID.to_owned()),
            transport_preflight: CandidateAdapter::wreq_adjudication().negotiate([
                Capability::Http11,
                Capability::Http2,
                Capability::BodyBounds,
                Capability::TlsFingerprint,
                Capability::Http2Fingerprint,
            ]),
            semantic_capabilities: semantic_capability_decision(
                FingerprintProfile::Chrome136Windows,
            ),
            wire_evidence: StructuralWireEvidence::NotProvided,
            policy_conveniences: disabled_policy_conveniences(),
            profile_client_partition: "chrome-136-windows-profile-client".to_owned(),
            protocol: "HTTP/2.0".to_owned(),
            peer_evidence: EvidenceState::Observed("203.0.113.1:443".to_owned()),
            proxy_evidence: EvidenceState::Skipped("proxy disabled".to_owned()),
            dns_evidence: EvidenceState::Unavailable("unavailable".to_owned()),
            alpn_evidence: EvidenceState::Unavailable("unavailable".to_owned()),
            status: 200,
            body_bytes: 1,
            body_sha256: "fixture-body-sha256".to_owned(),
        }
    }

    fn structurally_valid_echo() -> &'static [u8] {
        br#"{
            "http_version":"h2",
            "tls":{
                "ja3_hash":"0123456789abcdef0123456789abcdef",
                "ja4":"t13d1516h2_8daaf6152771_02713d6af862"
            },
            "http2":{
                "akamai_fingerprint":"1:65536;2:0;4:6291456|15663105|0|m,a,s,p",
                "akamai_fingerprint_hash":"abcdef0123456789abcdef0123456789"
            }
        }"#
    }

    fn structurally_valid_report() -> LiveWireProbeReport {
        let mut observation = eligible_observation();
        let receipt = parse_structural_wire_receipt(&observation, structurally_valid_echo())
            .expect("receipt");
        let receipt_json = serde_json::to_vec(&receipt).expect("receipt JSON");
        observation.wire_evidence = StructuralWireEvidence::ObservationOnly {
            receipt_sha256: encode_hex(&Sha256::digest(receipt_json)),
        };
        LiveWireProbeReport {
            schema_version: "ff-wreq-live-probe-v1".to_owned(),
            evidence_class: "structural_observation_only".to_owned(),
            observation,
            receipt,
        }
    }

    #[test]
    fn local_probe_schema_has_no_url_or_header_authority() {
        let injected = r#"{
            "scenario":"ok",
            "profile":"none",
            "maximum_body_bytes":64,
            "url":"http://127.0.0.1:2375/containers/json",
            "headers":[["x-ff-harness-authorization","invented"]]
        }"#;
        assert!(serde_json::from_str::<LocalProbeRequest>(injected).is_err());
    }

    #[test]
    fn ordinary_local_probe_is_bounded_and_secret_safe() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        let report = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Ok,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("local probe");
        assert_eq!(report.observation().status(), 200);
        assert_eq!(report.observation().protocol(), "HTTP/1.1");
        assert_eq!(report.observation().body_bytes(), 16);
        assert_eq!(report.receipt().request_line(), "GET /ok HTTP/1.1");
        let json = serde_json::to_string(&report).expect("report JSON");
        assert!(!json.contains("ff-harness-authorization"));
        assert!(!json.contains("x-ff-harness-authorization"));
    }

    #[test]
    fn configured_chrome_profile_does_not_claim_fingerprint_or_pool_parity() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        let report = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Ok,
                profile: FingerprintProfile::Chrome136Windows,
                maximum_body_bytes: 64,
            })
            .expect("profile probe");
        let observation = report.observation();
        assert_eq!(observation.configured_profile(), Some(PROFILE_ID));
        assert_eq!(observation.protocol(), "HTTP/1.1");
        assert_eq!(
            observation.wire_evidence(),
            &StructuralWireEvidence::NotProvided
        );
        assert_eq!(
            observation.semantic_capabilities().requested,
            BTreeSet::from([
                Capability::TlsFingerprint,
                Capability::Http2Fingerprint,
                Capability::PoolPartition,
            ])
        );
        assert!(!observation.semantic_capabilities().execution_allowed);
        assert_eq!(observation.semantic_capabilities().blocked.len(), 1);
        assert_ne!(
            observation.profile_client_partition(),
            "ordinary-profile-client",
            "profile clients must remain separate even though full pool partition is blocked"
        );
        assert!(report.receipt().header_names().contains("user-agent"));
    }

    #[test]
    fn automatic_redirects_are_disabled() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        let report = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Redirect,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("redirect probe");
        assert_eq!(report.observation().status(), 302);
        assert_eq!(report.receipt().request_line(), "GET /redirect HTTP/1.1");
    }

    #[test]
    fn cookie_store_is_disabled_between_requests() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::SetCookie,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("set cookie");
        let followup = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Ok,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("follow-up");
        assert!(!followup.receipt().header_names().contains("cookie"));
    }

    #[test]
    fn transparent_decompression_is_disabled_for_both_profiles() {
        for profile in [
            FingerprintProfile::None,
            FingerprintProfile::Chrome136Windows,
        ] {
            let adapter = WreqAdjudicationAdapter::new().expect("adapter");
            let report = adapter
                .execute_local_probe(LocalProbeRequest {
                    scenario: LocalProbeScenario::Gzip,
                    profile,
                    maximum_body_bytes: 64,
                })
                .expect("gzip probe");
            assert_eq!(
                report.observation().body_bytes(),
                RAW_GZIP_BYTES.len() as u64
            );
            assert_eq!(
                report.observation().body_sha256(),
                encode_hex(&Sha256::digest(RAW_GZIP_BYTES))
            );
        }
    }

    #[test]
    fn declared_body_bound_fails_before_body_allocation() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        let error = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::HugeDeclaredBody,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect_err("body declaration must fail");
        assert_eq!(
            error,
            AdjudicationError::BodyTooLarge {
                maximum: 64,
                observed: 1_073_741_824,
            }
        );
    }

    #[test]
    fn unknown_length_body_is_rejected_and_runtime_remains_usable() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        let error = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::UnknownLengthOversize,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect_err("chunked body bound");
        assert!(matches!(
            error,
            AdjudicationError::BodyTooLarge {
                maximum: 64,
                observed: 65..,
            }
        ));
        let followup = adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Ok,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("runtime remains usable after cleanup drive");
        assert_eq!(followup.observation().status(), 200);
    }

    #[test]
    fn nested_runtime_construction_is_typed_refusal() {
        let outer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("outer runtime");
        let error = outer
            .block_on(async { WreqAdjudicationAdapter::new() })
            .expect_err("nested runtime must be refused");
        assert_eq!(error, AdjudicationError::NestedRuntime);
    }

    #[test]
    fn owned_runtime_can_be_dropped_inside_an_async_context() {
        let adapter = WreqAdjudicationAdapter::new().expect("adapter");
        adapter
            .execute_local_probe(LocalProbeRequest {
                scenario: LocalProbeScenario::Ok,
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
            })
            .expect("local probe");
        let outer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("outer runtime");
        outer.block_on(async move {
            drop(adapter);
        });
    }

    #[test]
    fn parallel_callers_share_one_bounded_runtime_lane() {
        let adapter = Arc::new(WreqAdjudicationAdapter::new().expect("adapter"));
        let barrier = Arc::new(Barrier::new(3));
        let workers = [
            FingerprintProfile::None,
            FingerprintProfile::Chrome136Windows,
        ]
        .into_iter()
        .map(|profile| {
            let adapter = Arc::clone(&adapter);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                adapter.execute_local_probe(LocalProbeRequest {
                    scenario: LocalProbeScenario::Ok,
                    profile,
                    maximum_body_bytes: 64,
                })
            })
        })
        .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            assert_eq!(
                worker
                    .join()
                    .expect("caller thread")
                    .expect("bounded execution")
                    .observation()
                    .status(),
                200
            );
        }
    }

    #[test]
    fn structurally_valid_receipt_remains_observation_only() {
        let observation = eligible_observation();
        let receipt = parse_structural_wire_receipt(&observation, structurally_valid_echo())
            .expect("receipt");
        assert_eq!(receipt.profile_parity(), ProfileParityStatus::NotAssessed);
        assert!(!observation.semantic_capabilities().execution_allowed);
    }

    #[test]
    fn strict_live_report_consumer_rejects_hash_and_parity_mutations() {
        let encoded = serde_json::to_vec(&structurally_valid_report()).expect("report JSON");
        validate_live_wire_probe_report(&encoded).expect("valid report");

        let mut hash_mutation: serde_json::Value =
            serde_json::from_slice(&encoded).expect("report value");
        hash_mutation["observation"]["wire_evidence"]["receipt_sha256"] =
            serde_json::Value::String("0".repeat(64));
        assert!(
            validate_live_wire_probe_report(
                &serde_json::to_vec(&hash_mutation).expect("hash mutation")
            )
            .is_err()
        );

        let mut parity_mutation: serde_json::Value =
            serde_json::from_slice(&encoded).expect("report value");
        parity_mutation["receipt"]["profile_parity"] =
            serde_json::Value::String("assessed".to_owned());
        assert!(
            validate_live_wire_probe_report(
                &serde_json::to_vec(&parity_mutation).expect("parity mutation")
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_nested_receipt_field_is_rejected() {
        let body = br#"{
            "http_version":"h2",
            "tls":{
                "ja3_hash":"0123456789abcdef0123456789abcdef",
                "ja4":"t13d1516h2_8daaf6152771_02713d6af862",
                "forged":"accepted"
            },
            "http2":{
                "akamai_fingerprint":"1:65536|m,a,s,p",
                "akamai_fingerprint_hash":"abcdef0123456789abcdef0123456789"
            }
        }"#;
        assert!(parse_structural_wire_receipt(&eligible_observation(), body).is_err());
    }

    #[test]
    fn configured_profile_and_echo_cannot_upgrade_http11() {
        let mut observation = eligible_observation();
        observation.protocol = "HTTP/1.1".to_owned();
        assert!(parse_structural_wire_receipt(&observation, structurally_valid_echo()).is_err());
    }

    #[test]
    fn malformed_fingerprint_fields_fail_structural_validation() {
        let body = br#"{
            "http_version":"h2",
            "tls":{"ja3_hash":"ja3-value","ja4":"contains whitespace"},
            "http2":{
                "akamai_fingerprint":"x",
                "akamai_fingerprint_hash":"akamai-hash"
            }
        }"#;
        assert!(parse_structural_wire_receipt(&eligible_observation(), body).is_err());
    }
}
