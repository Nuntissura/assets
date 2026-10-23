use crate::local_server::{LocalProtocolServer, execute_local, run_cancellation_probe};
use crate::policy::{
    BodyBudget, ByteCredits, CancellationModel, CandidateAdapter, Capability, Cookie, CookieJar,
    DnsEvidence, HeaderValue, HttpRequest, HttpUrl, PoolKey, PoolRegistry, ProxyEvidence,
    PublicSuffixSet, RedirectPolicy, RetryBudget, TransportError, sanitize_exchange,
    validate_connected_address, validate_dns_evidence,
};
use fforager_contracts::{
    CancellationAcknowledgement, CancellationOutcome, CancellationRequest, EnvelopeHeader,
    ProducerId, ProtocolLimits, RequestId, SchemaVersion, validate_cancellation_correlation,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::Instant;

const SCHEMA_ID: &str = "fforager.transport-corpus-manifest.v1";
const REPORT_SCHEMA_ID: &str = "fforager.transport-corpus-report.v1";
const CORPUS_ID: &str = "WP-FF-007-transport-corpus-v1";
const NORMALIZATION_VERSION: &str = "WP-FF-004-sanitized-transcript-v1";
const MAX_CASES: usize = 128;
const SECRET_CANARY: &str = "ff-secret-canary-never-record";
const CANONICAL_MANIFEST_DECLARATION_SHA256: &str =
    "31f52c2b188fdf68252cf4294adfb5d93f6080ac5fb6b565e2f3ba46f42ecf02";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    pub schema_id: String,
    pub corpus_id: String,
    pub normalization_version: String,
    pub candidate_identity: String,
    pub max_cases: usize,
    pub cases: Vec<CorpusCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusCase {
    pub id: String,
    pub kind: CaseKind,
    pub mandatory: bool,
    pub proof_class: ProofClass,
    pub requested_capabilities: BTreeSet<Capability>,
    pub expected_outcome: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    StandardCapability,
    TlsFingerprintBlocked,
    Http2FingerprintBlocked,
    Http2Blocked,
    ProxyBlocked,
    CompressionBlocked,
    Http11Local,
    Http11HeaderBoundary,
    Http11HugeLength,
    RangeLocal,
    RangeBoundary,
    RangeInvalid,
    StreamingPositive,
    StreamingLocal,
    RedirectCrossOrigin,
    RedirectDowngrade,
    RedirectLimit,
    RedirectSpecialUse,
    CookieScope,
    CookiePublicSuffix,
    CookieIpDomain,
    CookieSyntax,
    CookieCountBoundary,
    DnsPositive,
    DnsProvenanceMismatch,
    SsrfMixedAnswers,
    SsrfRegistryBoundary,
    DnsSelection,
    DnsRebind,
    ProxyEvidence,
    ProxyEvidencePositive,
    PoolReuse,
    PoolSessionPartition,
    PoolFingerprintPartition,
    PoolBound,
    PoolAllDimensions,
    BoundsPositive,
    BoundsExact,
    MetadataBound,
    BodyBound,
    DecompressionBound,
    ByteCredit,
    RetryBound,
    RetryAllowed,
    CancellationPositive,
    CancellationBoundary,
    CancellationCorrelation,
    CancellationPool,
    ReplayBoundary,
    ReplayNegative,
    SanitizedReplay,
    UrlPositive,
    UrlBoundary,
    UrlBounds,
}

impl CaseKind {
    fn family(self) -> &'static str {
        match self {
            Self::TlsFingerprintBlocked => "tls_fingerprint",
            Self::Http2FingerprintBlocked => "http2_fingerprint",
            Self::Http2Blocked => "http2",
            Self::ProxyBlocked | Self::ProxyEvidence | Self::ProxyEvidencePositive => "proxy",
            Self::CompressionBlocked | Self::DecompressionBound => "compression",
            Self::Http11Local | Self::Http11HeaderBoundary | Self::Http11HugeLength => "http11",
            Self::RangeLocal | Self::RangeBoundary | Self::RangeInvalid => "range",
            Self::StreamingPositive | Self::StreamingLocal | Self::ByteCredit => "streaming",
            Self::RedirectCrossOrigin
            | Self::RedirectDowngrade
            | Self::RedirectLimit
            | Self::RedirectSpecialUse => "redirect",
            Self::CookieScope
            | Self::CookiePublicSuffix
            | Self::CookieIpDomain
            | Self::CookieSyntax
            | Self::CookieCountBoundary => "cookie",
            Self::DnsPositive
            | Self::DnsProvenanceMismatch
            | Self::SsrfMixedAnswers
            | Self::SsrfRegistryBoundary
            | Self::DnsSelection
            | Self::DnsRebind => "dns_ssrf",
            Self::PoolReuse
            | Self::PoolSessionPartition
            | Self::PoolFingerprintPartition
            | Self::PoolBound
            | Self::PoolAllDimensions => "pool",
            Self::BoundsPositive | Self::BoundsExact | Self::MetadataBound | Self::BodyBound => {
                "bounds"
            }
            Self::RetryBound | Self::RetryAllowed => "retry",
            Self::CancellationPositive
            | Self::CancellationBoundary
            | Self::CancellationCorrelation
            | Self::CancellationPool => "cancellation",
            Self::ReplayBoundary | Self::ReplayNegative | Self::SanitizedReplay => "replay",
            Self::UrlPositive | Self::UrlBoundary | Self::UrlBounds => "url",
            Self::StandardCapability => "capability_contract",
        }
    }

    fn scenario_class(self) -> &'static str {
        match self {
            Self::StandardCapability
            | Self::Http11Local
            | Self::RangeLocal
            | Self::StreamingPositive
            | Self::RedirectCrossOrigin
            | Self::CookieScope
            | Self::DnsPositive
            | Self::ProxyEvidencePositive
            | Self::PoolReuse
            | Self::PoolAllDimensions
            | Self::CancellationPositive
            | Self::CancellationPool
            | Self::SanitizedReplay
            | Self::BoundsPositive
            | Self::UrlPositive => "positive",
            Self::RedirectLimit
            | Self::CookieCountBoundary
            | Self::PoolBound
            | Self::BoundsExact
            | Self::RetryAllowed
            | Self::Http11HeaderBoundary
            | Self::RangeBoundary
            | Self::StreamingLocal
            | Self::CancellationBoundary
            | Self::ReplayBoundary
            | Self::UrlBoundary => "boundary",
            Self::TlsFingerprintBlocked
            | Self::Http2FingerprintBlocked
            | Self::Http2Blocked
            | Self::ProxyBlocked
            | Self::CompressionBlocked => "blocked",
            _ => "negative",
        }
    }

    fn required_capabilities(self) -> BTreeSet<Capability> {
        use Capability::{
            BodyBounds, Cancellation, Compression, CookieScope, DecompressionBounds, DnsProvenance,
            Http2, Http2Fingerprint, Http11, MetadataBounds, PoolPartition, Proxy, Range,
            RedirectPolicy, Replay, RetryBounds, SsrfPolicy, Streaming, TlsFingerprint,
        };
        let capabilities: &[Capability] = match self {
            Self::StandardCapability => &[
                Http11,
                Range,
                Streaming,
                Replay,
                Cancellation,
                MetadataBounds,
                BodyBounds,
            ],
            Self::TlsFingerprintBlocked => &[TlsFingerprint],
            Self::Http2FingerprintBlocked => &[Http2Fingerprint],
            Self::Http2Blocked => &[Http2],
            Self::ProxyBlocked => &[Proxy],
            Self::CompressionBlocked => &[Compression],
            Self::Http11Local | Self::Http11HeaderBoundary | Self::Http11HugeLength => &[Http11],
            Self::RangeLocal | Self::RangeBoundary | Self::RangeInvalid => &[Http11, Range],
            Self::StreamingPositive | Self::StreamingLocal => &[Http11, Streaming, BodyBounds],
            Self::ByteCredit => &[Streaming, BodyBounds],
            Self::RedirectCrossOrigin | Self::RedirectDowngrade | Self::RedirectSpecialUse => {
                &[RedirectPolicy]
            }
            Self::RedirectLimit => &[RedirectPolicy, RetryBounds],
            Self::CookieScope
            | Self::CookiePublicSuffix
            | Self::CookieIpDomain
            | Self::CookieSyntax
            | Self::CookieCountBoundary => &[CookieScope],
            Self::DnsPositive
            | Self::DnsProvenanceMismatch
            | Self::SsrfMixedAnswers
            | Self::SsrfRegistryBoundary
            | Self::DnsSelection
            | Self::DnsRebind
            | Self::ProxyEvidence
            | Self::ProxyEvidencePositive => &[DnsProvenance, SsrfPolicy],
            Self::PoolReuse
            | Self::PoolSessionPartition
            | Self::PoolFingerprintPartition
            | Self::PoolBound
            | Self::PoolAllDimensions => &[PoolPartition],
            Self::BoundsPositive | Self::BoundsExact => &[MetadataBounds, BodyBounds],
            Self::MetadataBound | Self::UrlPositive | Self::UrlBoundary | Self::UrlBounds => {
                &[MetadataBounds]
            }
            Self::BodyBound => &[BodyBounds],
            Self::DecompressionBound => &[DecompressionBounds],
            Self::RetryBound | Self::RetryAllowed => &[RetryBounds],
            Self::CancellationPositive
            | Self::CancellationBoundary
            | Self::CancellationCorrelation => &[Cancellation],
            Self::CancellationPool => &[Cancellation, PoolPartition],
            Self::ReplayBoundary | Self::ReplayNegative | Self::SanitizedReplay => &[Replay],
        };
        capabilities.iter().copied().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofClass {
    Runtime,
    Protocol,
    Security,
    Resource,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AggregateVerdict {
    PassPureRustPath,
    FailedSpikeRequiresOperatorDecision,
}

impl AggregateVerdict {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PassPureRustPath => "PASS_PURE_RUST_PATH",
            Self::FailedSpikeRequiresOperatorDecision => "FAILED_SPIKE_REQUIRES_OPERATOR_DECISION",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusReport {
    pub schema_id: String,
    pub corpus_id: String,
    pub normalization_version: String,
    pub candidate_identity: String,
    pub manifest_declaration_sha256: String,
    pub candidate_implementation_sha256: String,
    pub semantic_projection_sha256: String,
    pub zero_product_progress: bool,
    pub cases: Vec<CaseReport>,
    pub summary: CorpusSummary,
    pub counterfactual: CounterfactualProof,
    pub residual_uncertainties: Vec<String>,
    pub verdict: AggregateVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseReport {
    pub id: String,
    pub mandatory: bool,
    pub proof_class: ProofClass,
    pub family: String,
    pub scenario_class: String,
    pub concrete_input: String,
    pub executed_boundary: String,
    pub requested_capabilities: BTreeSet<Capability>,
    pub satisfied_capabilities: BTreeSet<Capability>,
    pub blocked_capabilities: Vec<BlockedCapabilityReport>,
    pub connection_identity: String,
    pub wire_identity: String,
    pub pool_identity: String,
    pub selected_address: String,
    pub proxy_identity: String,
    pub alpn_identity: String,
    pub protocol_identity: String,
    pub limits: BTreeMap<String, u64>,
    pub timing_phases_micros: BTreeMap<String, u128>,
    pub transcript: serde_json::Value,
    pub expected_outcome: String,
    pub observed_outcome: String,
    pub status: CaseStatus,
    pub exact_mismatch: Option<String>,
    pub skipped_semantic_dependencies: Vec<String>,
    pub duration_micros: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedCapabilityReport {
    pub capability: Capability,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CaseStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub mandatory_total: usize,
    pub mandatory_passed: usize,
    pub mandatory_blocked: usize,
    pub blocked_capability_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterfactualProof {
    pub mutation: String,
    pub rejected_by_same_oracle: bool,
    pub rejection: String,
}

/// Executes every declared case and constructs the fail-closed aggregate report.
///
/// # Errors
///
/// Returns a stable diagnostic when the manifest, a case oracle, the aggregate
/// evidence, the counterfactual, or report sanitization is invalid.
pub fn run_corpus(manifest: &CorpusManifest) -> Result<CorpusReport, String> {
    validate_manifest(manifest)?;
    let candidate = CandidateAdapter::std_first();
    if manifest.candidate_identity != candidate.identity() {
        return Err(format!(
            "FF-TRANSPORT-E-CANDIDATE-IDENTITY: expected {}, observed {}",
            manifest.candidate_identity,
            candidate.identity()
        ));
    }
    let cases = manifest
        .cases
        .iter()
        .map(|case| run_case(case, &candidate))
        .collect::<Vec<_>>();
    let (summary, verdict) = validate_aggregate_evidence(manifest, &cases)?;
    let counterfactual = counterfactual_proof(manifest, &cases);
    if !counterfactual.rejected_by_same_oracle {
        return Err("FF-TRANSPORT-E-COUNTERFACTUAL: mutation was accepted".to_owned());
    }
    let semantic_projection_sha256 = semantic_projection_sha256(&cases)?;
    let report = CorpusReport {
        schema_id: REPORT_SCHEMA_ID.to_owned(),
        corpus_id: manifest.corpus_id.clone(),
        normalization_version: manifest.normalization_version.clone(),
        candidate_identity: manifest.candidate_identity.clone(),
        manifest_declaration_sha256: manifest_declaration_sha256(manifest)?,
        candidate_implementation_sha256: candidate_implementation_sha256(),
        semantic_projection_sha256,
        zero_product_progress: true,
        cases,
        summary,
        counterfactual,
        residual_uncertainties: vec![
            "No browser-equivalent ClientHello was emitted; TLS fingerprint parity remains blocked."
                .to_owned(),
            "No browser-equivalent HTTP/2 SETTINGS, ordering, or priority frames were emitted; HTTP/2 wire parity remains blocked."
                .to_owned(),
            "The local protocol harness proves bounded HTTP/1.1 semantics, not internet interoperability."
                .to_owned(),
            "Cookie scope remains blocked until a versioned authoritative PSL with wildcard and exception behavior is bound to execution.".to_owned(),
            "Redirect, DNS provenance, and SSRF remain blocked until a complete generated special-purpose registry, resolver evidence, and actual socket peer address are bound at every hop.".to_owned(),
            "Pool partition remains blocked until keys are derived from immutable execution context rather than caller fields.".to_owned(),
            "Retry remains blocked until attempts, idempotency, deadlines, cancellation, and partial-body state cross one executor.".to_owned(),
        ],
        verdict,
    };
    reject_secret_canary(&report)?;
    Ok(report)
}

/// Applies the same strict aggregate oracle used by the corpus runner.
///
/// # Errors
///
/// Returns a stable diagnostic for missing, duplicate, undeclared, drifted, or
/// behavior-mismatched case evidence.
#[allow(clippy::too_many_lines)]
pub fn validate_aggregate_evidence(
    manifest: &CorpusManifest,
    cases: &[CaseReport],
) -> Result<(CorpusSummary, AggregateVerdict), String> {
    if cases.len() != manifest.cases.len() {
        return Err(format!(
            "FF-TRANSPORT-E-CASE-CARDINALITY: expected {}, observed {}",
            manifest.cases.len(),
            cases.len()
        ));
    }
    let expected_by_id = manifest
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let mut observed_ids = BTreeSet::new();
    let mut passed = 0;
    let mut mandatory_total = 0;
    let mut mandatory_passed = 0;
    let mut blocked_codes = BTreeSet::new();
    let candidate = CandidateAdapter::std_first();
    for report in cases {
        if !observed_ids.insert(report.id.as_str()) {
            return Err(format!("FF-TRANSPORT-E-DUPLICATE-REPORT-ID: {}", report.id));
        }
        let expected = expected_by_id
            .get(report.id.as_str())
            .ok_or_else(|| format!("FF-TRANSPORT-E-UNDECLARED-REPORT-ID: {}", report.id))?;
        if report.mandatory != expected.mandatory
            || report.proof_class != expected.proof_class
            || report.requested_capabilities != expected.requested_capabilities
            || report.expected_outcome != expected.expected_outcome
        {
            return Err(format!("FF-TRANSPORT-E-DECLARATION-DRIFT: {}", report.id));
        }
        let decision = candidate.negotiate(expected.requested_capabilities.iter().copied());
        let expected_blocked = decision
            .blocked
            .iter()
            .map(|blocked| BlockedCapabilityReport {
                capability: blocked.capability,
                code: blocked.code.clone(),
                reason: blocked.reason.clone(),
            })
            .collect::<Vec<_>>();
        if report.satisfied_capabilities != decision.satisfied
            || report.blocked_capabilities != expected_blocked
            || !report.satisfied_capabilities.is_disjoint(
                &report
                    .blocked_capabilities
                    .iter()
                    .map(|item| item.capability)
                    .collect(),
            )
        {
            return Err(format!("FF-TRANSPORT-E-CAPABILITY-EVIDENCE: {}", report.id));
        }
        let represented = report
            .satisfied_capabilities
            .iter()
            .copied()
            .chain(
                report
                    .blocked_capabilities
                    .iter()
                    .map(|blocked| blocked.capability),
            )
            .collect::<BTreeSet<_>>();
        if represented != report.requested_capabilities {
            return Err(format!(
                "FF-TRANSPORT-E-CAPABILITY-PARTITION: {}",
                report.id
            ));
        }
        if report.family != expected.kind.family()
            || report.scenario_class != expected.kind.scenario_class()
            || report.connection_identity.is_empty()
            || report.wire_identity.is_empty()
            || report.pool_identity.is_empty()
            || report.selected_address.is_empty()
            || report.proxy_identity.is_empty()
            || report.alpn_identity.is_empty()
            || report.protocol_identity.is_empty()
            || !report.transcript.is_object()
            || !report.timing_phases_micros.contains_key("total")
        {
            return Err(format!("FF-TRANSPORT-E-EVIDENCE-SHAPE: {}", report.id));
        }
        if decision.execution_allowed {
            if report.executed_boundary == "typed-capability-negotiation"
                || report.connection_identity == "not_executed:capability_blocked"
            {
                return Err(format!("FF-TRANSPORT-E-EXECUTION-EVIDENCE: {}", report.id));
            }
        } else if report.executed_boundary != "typed-capability-negotiation"
            || report.connection_identity != "not_executed:capability_blocked"
            || report.wire_identity != "not_executed:capability_blocked"
        {
            return Err(format!("FF-TRANSPORT-E-BLOCKED-EXECUTION: {}", report.id));
        }
        if report.observed_outcome != expected.expected_outcome
            || report.status != CaseStatus::Pass
            || report.exact_mismatch.is_some()
        {
            return Err(format!(
                "FF-TRANSPORT-E-OUTCOME-MISMATCH: {} expected {}, observed {}",
                report.id, expected.expected_outcome, report.observed_outcome
            ));
        }
        passed += 1;
        if report.mandatory {
            mandatory_total += 1;
            mandatory_passed += 1;
            for blocked in &report.blocked_capabilities {
                blocked_codes.insert(blocked.code.clone());
            }
        }
    }
    if observed_ids.len() != expected_by_id.len() {
        return Err("FF-TRANSPORT-E-MISSING-REPORT-ID".to_owned());
    }
    let blocked_capability_codes = blocked_codes.into_iter().collect::<Vec<_>>();
    let verdict = if blocked_capability_codes.is_empty() {
        AggregateVerdict::PassPureRustPath
    } else {
        AggregateVerdict::FailedSpikeRequiresOperatorDecision
    };
    Ok((
        CorpusSummary {
            total: cases.len(),
            passed,
            failed: cases.len().saturating_sub(passed),
            mandatory_total,
            mandatory_passed,
            mandatory_blocked: cases
                .iter()
                .filter(|case| case.mandatory && !case.blocked_capabilities.is_empty())
                .count(),
            blocked_capability_codes,
        },
        verdict,
    ))
}

fn validate_manifest(manifest: &CorpusManifest) -> Result<(), String> {
    if manifest.schema_id != SCHEMA_ID
        || manifest.corpus_id != CORPUS_ID
        || manifest.normalization_version != NORMALIZATION_VERSION
    {
        return Err("FF-TRANSPORT-E-MANIFEST-AUTHORITY".to_owned());
    }
    if manifest.max_cases == 0
        || manifest.max_cases > MAX_CASES
        || manifest.cases.is_empty()
        || manifest.cases.len() > manifest.max_cases
    {
        return Err("FF-TRANSPORT-E-CASE-BOUNDS".to_owned());
    }
    let mut ids = BTreeSet::new();
    for case in &manifest.cases {
        if !valid_case_id(&case.id) || !ids.insert(&case.id) {
            return Err(format!("FF-TRANSPORT-E-CASE-ID: {}", case.id));
        }
        if !case.mandatory
            || case.requested_capabilities.is_empty()
            || case.expected_outcome.is_empty()
            || case.requested_capabilities != case.kind.required_capabilities()
        {
            return Err(format!("FF-TRANSPORT-E-CASE-DECLARATION: {}", case.id));
        }
    }
    let digest = manifest_declaration_sha256(manifest)?;
    if CANONICAL_MANIFEST_DECLARATION_SHA256 != "PENDING"
        && digest != CANONICAL_MANIFEST_DECLARATION_SHA256
    {
        return Err(format!(
            "FF-TRANSPORT-E-CANONICAL-CORPUS: expected {CANONICAL_MANIFEST_DECLARATION_SHA256}, observed {digest}"
        ));
    }
    validate_coverage(manifest)?;
    Ok(())
}

fn valid_case_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_coverage(manifest: &CorpusManifest) -> Result<(), String> {
    let families = manifest
        .cases
        .iter()
        .map(|case| case.kind.family())
        .collect::<BTreeSet<_>>();
    for required in [
        "tls_fingerprint",
        "http2_fingerprint",
        "http2",
        "proxy",
        "compression",
        "http11",
        "range",
        "streaming",
        "redirect",
        "cookie",
        "dns_ssrf",
        "pool",
        "bounds",
        "retry",
        "cancellation",
        "replay",
        "url",
    ] {
        if !families.contains(required) {
            return Err(format!("FF-TRANSPORT-E-COVERAGE-FAMILY: {required}"));
        }
    }
    let candidate = CandidateAdapter::std_first();
    for family in families {
        if matches!(
            family,
            "tls_fingerprint" | "http2_fingerprint" | "http2" | "capability_contract"
        ) {
            continue;
        }
        let family_cases = manifest
            .cases
            .iter()
            .filter(|case| case.kind.family() == family)
            .collect::<Vec<_>>();
        let family_blocked = family_cases.iter().all(|case| {
            !candidate
                .negotiate(case.requested_capabilities.iter().copied())
                .execution_allowed
        });
        if family_blocked {
            continue;
        }
        let classes = family_cases
            .iter()
            .map(|case| case.kind.scenario_class())
            .collect::<BTreeSet<_>>();
        for required in ["positive", "boundary", "negative"] {
            if !classes.contains(required) {
                return Err(format!(
                    "FF-TRANSPORT-E-COVERAGE-CLASS: {family}:{required}"
                ));
            }
        }
    }
    Ok(())
}

fn manifest_declaration_sha256(manifest: &CorpusManifest) -> Result<String, String> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| format!("FF-TRANSPORT-E-MANIFEST-DIGEST: {error}"))?;
    Ok(encode_hex(&Sha256::digest(bytes)))
}

fn candidate_implementation_sha256() -> String {
    let mut digest = Sha256::new();
    digest.update(include_bytes!("lib.rs"));
    digest.update(include_bytes!("policy.rs"));
    digest.update(include_bytes!("local_server.rs"));
    digest.update(include_bytes!("corpus.rs"));
    digest.update(include_bytes!("bin/fforager-transport-corpus.rs"));
    encode_hex(&digest.finalize())
}

fn semantic_projection_sha256(cases: &[CaseReport]) -> Result<String, String> {
    let projection = cases
        .iter()
        .map(|case| {
            serde_json::json!({
                "id": case.id,
                "family": case.family,
                "scenario_class": case.scenario_class,
                "requested": case.requested_capabilities,
                "satisfied": case.satisfied_capabilities,
                "blocked": case.blocked_capabilities,
                "connection_identity": case.connection_identity,
                "wire_identity": case.wire_identity,
                "pool_identity": case.pool_identity,
                "selected_address": case.selected_address,
                "proxy_identity": case.proxy_identity,
                "alpn_identity": case.alpn_identity,
                "protocol_identity": case.protocol_identity,
                "limits": case.limits,
                "transcript": case.transcript,
                "expected": case.expected_outcome,
                "observed": case.observed_outcome,
                "status": case.status
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&projection)
        .map_err(|error| format!("FF-TRANSPORT-E-SEMANTIC-DIGEST: {error}"))?;
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

#[allow(clippy::too_many_lines)]
fn run_case(case: &CorpusCase, candidate: &CandidateAdapter) -> CaseReport {
    let started = Instant::now();
    let decision_started = Instant::now();
    let decision = candidate.negotiate(case.requested_capabilities.iter().copied());
    let decision_micros = decision_started.elapsed().as_micros();
    let blocked_capabilities = decision
        .blocked
        .iter()
        .map(|blocked| BlockedCapabilityReport {
            capability: blocked.capability,
            code: blocked.code.clone(),
            reason: blocked.reason.clone(),
        })
        .collect::<Vec<_>>();
    let execution = if decision.execution_allowed {
        candidate
            .execute(case.requested_capabilities.iter().copied(), |grant| {
                execute_case(case.kind, grant, &case.requested_capabilities)
            })
            .map(|(_, evidence)| evidence)
    } else {
        Ok(CaseEvidence {
            concrete_input: format!("requested={:?}", decision.requested),
            executed_boundary: "typed-capability-negotiation".to_owned(),
            observed_outcome: decision.blocked.first().map_or_else(
                || "blocked-without-code".to_owned(),
                |blocked| blocked.code.clone(),
            ),
            connection_identity: "not_executed:capability_blocked".to_owned(),
            wire_identity: "not_executed:capability_blocked".to_owned(),
            pool_identity: "not_applicable".to_owned(),
            selected_address: "not_applicable".to_owned(),
            proxy_identity: "not_executed".to_owned(),
            alpn_identity: "not_negotiated".to_owned(),
            protocol_identity: "not_executed".to_owned(),
            limits: BTreeMap::from([("execution_allowed".to_owned(), 0)]),
            transcript: None,
            skipped_semantic_dependencies: decision
                .blocked
                .iter()
                .map(|blocked| format!("{:?}:{}", blocked.capability, blocked.reason))
                .collect(),
        })
    };
    let evidence = execution.unwrap_or_else(|error| CaseEvidence {
        concrete_input: format!("{:?}", case.kind),
        executed_boundary: "case-execution-error".to_owned(),
        observed_outcome: error.to_string(),
        connection_identity: "not_executed:error".to_owned(),
        wire_identity: "not_executed:error".to_owned(),
        pool_identity: "not_applicable".to_owned(),
        selected_address: "not_applicable".to_owned(),
        proxy_identity: "not_executed".to_owned(),
        alpn_identity: "not_negotiated".to_owned(),
        protocol_identity: "execution-error".to_owned(),
        limits: BTreeMap::from([("execution_attempted".to_owned(), 1)]),
        transcript: None,
        skipped_semantic_dependencies: Vec::new(),
    });
    let exact_mismatch = (evidence.observed_outcome != case.expected_outcome).then(|| {
        format!(
            "expected {}, observed {}",
            case.expected_outcome, evidence.observed_outcome
        )
    });
    let duration_micros = started.elapsed().as_micros();
    let execution_micros = duration_micros.saturating_sub(decision_micros);
    let transcript = serde_json::json!({
        "normalization_version": NORMALIZATION_VERSION,
        "case_id": case.id,
        "family": case.kind.family(),
        "scenario_class": case.kind.scenario_class(),
        "executed_boundary": &evidence.executed_boundary,
        "result": &evidence.observed_outcome,
        "exchange": evidence.transcript.unwrap_or_else(|| serde_json::json!("not_applicable"))
    });
    CaseReport {
        id: case.id.clone(),
        mandatory: case.mandatory,
        proof_class: case.proof_class,
        family: case.kind.family().to_owned(),
        scenario_class: case.kind.scenario_class().to_owned(),
        concrete_input: evidence.concrete_input,
        executed_boundary: evidence.executed_boundary,
        requested_capabilities: decision.requested,
        satisfied_capabilities: decision.satisfied,
        blocked_capabilities,
        connection_identity: evidence.connection_identity,
        wire_identity: evidence.wire_identity,
        pool_identity: evidence.pool_identity,
        selected_address: evidence.selected_address,
        proxy_identity: evidence.proxy_identity,
        alpn_identity: evidence.alpn_identity,
        protocol_identity: evidence.protocol_identity,
        limits: evidence.limits,
        timing_phases_micros: BTreeMap::from([
            ("decision".to_owned(), decision_micros),
            ("execution".to_owned(), execution_micros),
            ("total".to_owned(), duration_micros),
        ]),
        transcript,
        expected_outcome: case.expected_outcome.clone(),
        observed_outcome: evidence.observed_outcome,
        status: if exact_mismatch.is_none() {
            CaseStatus::Pass
        } else {
            CaseStatus::Fail
        },
        exact_mismatch,
        skipped_semantic_dependencies: evidence.skipped_semantic_dependencies,
        duration_micros,
    }
}

#[derive(Debug)]
struct CaseEvidence {
    concrete_input: String,
    executed_boundary: String,
    observed_outcome: String,
    connection_identity: String,
    wire_identity: String,
    pool_identity: String,
    selected_address: String,
    proxy_identity: String,
    alpn_identity: String,
    protocol_identity: String,
    limits: BTreeMap<String, u64>,
    transcript: Option<serde_json::Value>,
    skipped_semantic_dependencies: Vec<String>,
}

impl CaseEvidence {
    fn policy(input: impl Into<String>, outcome: impl Into<String>) -> Self {
        Self {
            concrete_input: input.into(),
            executed_boundary: "pure-policy-model".to_owned(),
            observed_outcome: outcome.into(),
            connection_identity: "not_executed:policy_model".to_owned(),
            wire_identity: "not_executed:policy_model".to_owned(),
            pool_identity: "not_applicable".to_owned(),
            selected_address: "not_applicable".to_owned(),
            proxy_identity: "direct-policy-model".to_owned(),
            alpn_identity: "not_negotiated".to_owned(),
            protocol_identity: "policy-model-v1".to_owned(),
            limits: BTreeMap::from([("model_operation_limit".to_owned(), 1)]),
            transcript: None,
            skipped_semantic_dependencies: Vec::new(),
        }
    }
}

fn execute_case(
    kind: CaseKind,
    grant: &crate::policy::ExecutionGrant,
    requested: &BTreeSet<Capability>,
) -> Result<CaseEvidence, TransportError> {
    for capability in requested {
        grant.require(*capability)?;
    }
    match kind {
        CaseKind::StandardCapability => Ok(CaseEvidence::policy(
            "std-first standard capability set",
            "standard-capabilities-supported",
        )),
        CaseKind::Http11Local => local_case("/ok", None, 16, "http11-body-ok"),
        CaseKind::Http11HeaderBoundary => {
            local_case("/ok", None, 16, "http11-wire-exact-boundary-accepted")
        }
        CaseKind::Http11HugeLength => http11_huge_length(),
        CaseKind::RangeLocal => local_case("/range", Some("bytes=2-5"), 4, "range-206-2-5"),
        CaseKind::RangeBoundary => local_case(
            "/range-boundary",
            Some("bytes=9-9"),
            1,
            "range-last-byte-accepted",
        ),
        CaseKind::RangeInvalid => local_case(
            "/range-invalid",
            Some("bytes=20-30"),
            0,
            "range-416-rejected",
        ),
        CaseKind::StreamingPositive => local_case(
            "/stream-small",
            None,
            6,
            "streaming-single-fragment-accepted",
        ),
        CaseKind::StreamingLocal => local_case("/stream", None, 21, "streamed-with-byte-credits"),
        CaseKind::RedirectCrossOrigin => redirect_cross_origin(),
        CaseKind::RedirectDowngrade => redirect_downgrade(),
        CaseKind::RedirectLimit => redirect_limit(),
        CaseKind::RedirectSpecialUse => redirect_special_use(),
        CaseKind::CookieScope => cookie_scope(),
        CaseKind::CookiePublicSuffix => cookie_public_suffix(),
        CaseKind::CookieIpDomain => cookie_ip_domain(),
        CaseKind::CookieSyntax => cookie_syntax(),
        CaseKind::CookieCountBoundary => cookie_count_boundary(),
        CaseKind::DnsPositive => Ok(dns_positive()),
        CaseKind::DnsProvenanceMismatch => Ok(dns_provenance_mismatch()),
        CaseKind::SsrfMixedAnswers => Ok(ssrf_mixed_answers()),
        CaseKind::SsrfRegistryBoundary => Ok(ssrf_registry_boundary()),
        CaseKind::DnsSelection => Ok(dns_selection()),
        CaseKind::DnsRebind => Ok(dns_rebind()),
        CaseKind::ProxyEvidence => Ok(proxy_evidence()),
        CaseKind::ProxyEvidencePositive => Ok(proxy_evidence_positive()),
        CaseKind::PoolReuse => pool_reuse(),
        CaseKind::PoolSessionPartition => pool_partition(true),
        CaseKind::PoolFingerprintPartition => pool_partition(false),
        CaseKind::PoolBound => pool_bound(),
        CaseKind::PoolAllDimensions => pool_all_dimensions(),
        CaseKind::BoundsPositive => Ok(bounds_positive()),
        CaseKind::BoundsExact => Ok(bounds_exact()),
        CaseKind::MetadataBound => Ok(body_bound("metadata")),
        CaseKind::BodyBound => Ok(body_bound("body")),
        CaseKind::DecompressionBound => Ok(body_bound("decompression")),
        CaseKind::ByteCredit => byte_credit(),
        CaseKind::RetryBound => Ok(retry_bound()),
        CaseKind::RetryAllowed => retry_allowed(),
        CaseKind::CancellationPositive => cancellation_positive(),
        CaseKind::CancellationBoundary => cancellation_boundary(),
        CaseKind::CancellationCorrelation => cancellation_correlation(),
        CaseKind::CancellationPool => cancellation_pool(),
        CaseKind::ReplayBoundary => replay_boundary(),
        CaseKind::ReplayNegative => replay_negative(),
        CaseKind::SanitizedReplay => sanitized_replay(),
        CaseKind::UrlPositive => url_positive(),
        CaseKind::UrlBoundary => url_boundary(),
        CaseKind::UrlBounds => Ok(url_bounds()),
        CaseKind::TlsFingerprintBlocked
        | CaseKind::Http2FingerprintBlocked
        | CaseKind::Http2Blocked
        | CaseKind::ProxyBlocked
        | CaseKind::CompressionBlocked => Err(TransportError::Protocol(
            "FF-TRANSPORT-E-INTERNAL: blocked case crossed execution boundary".to_owned(),
        )),
    }
}

fn local_case(
    path: &str,
    range: Option<&str>,
    body_bytes: u64,
    outcome: &str,
) -> Result<CaseEvidence, TransportError> {
    let server = LocalProtocolServer::spawn()?;
    let target = server.target();
    let mut credits = ByteCredits::new(body_bytes);
    credits.grant(body_bytes)?;
    let response = execute_local(&target, path, range, &mut credits)?;
    let receipt = server.finish()?;
    let (expected_status, expected_body, required_header) = match path {
        "/ok" => (200, b"ferric-transport".as_slice(), None),
        "/range" => (
            206,
            b"2345".as_slice(),
            Some(("content-range", "bytes 2-5/10")),
        ),
        "/range-boundary" => (
            206,
            b"9".as_slice(),
            Some(("content-range", "bytes 9-9/10")),
        ),
        "/range-invalid" => (416, b"".as_slice(), Some(("content-range", "bytes */10"))),
        "/stream" => (200, b"stream-one-stream-two".as_slice(), None),
        "/stream-small" => (200, b"stream".as_slice(), None),
        _ => {
            return Err(TransportError::Protocol(
                "FF-TRANSPORT-E-LOCAL-CASE".to_owned(),
            ));
        }
    };
    let header_matches = required_header
        .is_none_or(|(name, value)| response.headers.get(name).map(String::as_str) == Some(value));
    let expected_fragments = match path {
        "/stream" => 4,
        "/range-invalid" => 0,
        _ => 1,
    };
    let streaming_fragmented = response.body_read_operations == expected_fragments;
    if response.status != expected_status
        || response.body != expected_body
        || response.response_sha256 != expected_response_sha256(path)
        || credits.accepted() != body_bytes
        || !header_matches
        || !streaming_fragmented
        || receipt.normalized_wire_sha256 != expected_request_sha256(path)
        || !receipt.request_line.ends_with(" HTTP/1.1")
        || range.is_some() != receipt.headers.contains_key("range")
    {
        return Err(TransportError::Protocol(
            "FF-TRANSPORT-E-LOCAL-ASSERTION".to_owned(),
        ));
    }
    Ok(CaseEvidence {
        concrete_input: format!("{}; range={range:?}", receipt.request_line),
        executed_boundary: "loopback-tcp-http1.1".to_owned(),
        observed_outcome: outcome.to_owned(),
        connection_identity: response.connection_identity,
        wire_identity: format!(
            "{};request_sha256={};response_sha256={}",
            response.wire_identity, receipt.normalized_wire_sha256, response.response_sha256
        ),
        pool_identity: "connection-close:no-pool".to_owned(),
        selected_address: "loopback-harness".to_owned(),
        proxy_identity: "direct".to_owned(),
        alpn_identity: "not_applicable:cleartext-http1".to_owned(),
        protocol_identity: "http/1.1".to_owned(),
        limits: BTreeMap::from([
            ("body_bytes".to_owned(), body_bytes),
            ("metadata_bytes".to_owned(), 16 * 1024),
        ]),
        transcript: None,
        skipped_semantic_dependencies: Vec::new(),
    })
}

fn expected_request_sha256(path: &str) -> &'static str {
    match path {
        "/ok" => "70802ef8b900229b7802b1582e9689e66dc8a4aa1badf20871aff9949b3b1520",
        "/range" => "7bc2bb848d8da6c592ef853dcc89cc9928cfb5a962c68723050032395bce1841",
        "/range-boundary" => "2305c49a0cab031a6a000205a9e04b23694b04a2bd2315e0f44f7a17f9b3d387",
        "/range-invalid" => "729f5064cc036f25dbfe806706ceee7b0f6c684c5460a8e99cf47fa6637eddce",
        "/stream-small" => "0ef5d961c83e921aee4ae4b2169aec622b0d06b4b46445df3d39b426fbc27b58",
        "/stream" => "094d3f7a4caa1df9bbd30ae2a0035323c8d7e0d0bbc7dbbd0a7b4d8f62d82df3",
        _ => "unsupported",
    }
}

fn expected_response_sha256(path: &str) -> &'static str {
    match path {
        "/ok" => "b3ac0ad43bb781320d8b1c59671e110aaa251972c7ac64bc83c3a521a06cce18",
        "/range" => "c488879da67aae3a930ddb171fa095618620711c88516166aaa7260cf5b6eec1",
        "/range-boundary" => "9cef28d73496441ba2d8f7cd847f5d2abf79bd9a04b2618678ce5ff6c5c9a8cb",
        "/range-invalid" => "c5385ff191bb05a70bdf4626b5659be07ad8fa27266f465e592384974e9c0be3",
        "/stream-small" => "8e43fd7f1f6cf24324c8fa70675f8d2e6a2cf313ed6b6dbc0c6d759b21ec21b7",
        "/stream" => "412345768c8b043872f22f7199ffcceffa7d82424bfe8dec5a35be2fd22f194b",
        _ => "unsupported",
    }
}

fn http11_huge_length() -> Result<CaseEvidence, TransportError> {
    let server = LocalProtocolServer::spawn()?;
    let target = server.target();
    let mut credits = ByteCredits::new(32);
    credits.grant(32)?;
    let error = execute_local(&target, "/huge-length", None, &mut credits)
        .expect_err("huge declared response must fail before body allocation");
    let receipt = server.finish()?;
    if receipt.request_line != "GET /huge-length HTTP/1.1" {
        return Err(TransportError::Protocol(
            "FF-TRANSPORT-E-HARNESS-REQUEST".to_owned(),
        ));
    }
    let mut evidence = CaseEvidence::policy(
        "Content-Length=18446744073709551615; maximum_body_bytes=32",
        error.to_string(),
    );
    "std-tcp-http11-preallocation-bound".clone_into(&mut evidence.executed_boundary);
    "loopback-http11-ephemeral".clone_into(&mut evidence.connection_identity);
    "http/1.1-std-tcp-v1;huge-content-length".clone_into(&mut evidence.wire_identity);
    "http/1.1".clone_into(&mut evidence.protocol_identity);
    evidence.limits = BTreeMap::from([
        ("maximum_body_bytes".to_owned(), 32),
        ("observed_content_length".to_owned(), u64::MAX),
    ]);
    Ok(evidence)
}

fn redirect_cross_origin() -> Result<CaseEvidence, TransportError> {
    let mut request = HttpRequest::new(
        "redirect-cross",
        "GET",
        HttpUrl::parse("https://media.example/account")?,
    )?;
    request.insert_header(
        "authorization",
        HeaderValue::new(SECRET_CANARY, false, false)?,
    )?;
    request.insert_header("accept", HeaderValue::new("*/*", false, false)?)?;
    let result = redirect_policy().apply(&request, HttpUrl::parse("https://cdn.example/media")?)?;
    if result.request.headers.contains_key("authorization")
        || !result.request.headers.contains_key("accept")
    {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-REDIRECT-STRIP".to_owned(),
        ));
    }
    Ok(CaseEvidence::policy(
        "authorization + accept; cross-origin target",
        "cross-origin-sensitive-headers-stripped",
    ))
}

fn redirect_downgrade() -> Result<CaseEvidence, TransportError> {
    let request = HttpRequest::new(
        "redirect-downgrade",
        "GET",
        HttpUrl::parse("https://media.example/secure")?,
    )?;
    let error = redirect_policy()
        .apply(&request, HttpUrl::parse("http://media.example/plain")?)
        .expect_err("downgrade fixture must be rejected");
    Ok(CaseEvidence::policy(
        "https origin -> http target",
        error.to_string(),
    ))
}

fn redirect_limit() -> Result<CaseEvidence, TransportError> {
    let mut request = HttpRequest::new(
        "redirect-limit",
        "GET",
        HttpUrl::parse("https://media.example/one")?,
    )?;
    request.redirect_count = 3;
    let error = redirect_policy()
        .apply(&request, HttpUrl::parse("https://media.example/four")?)
        .expect_err("redirect limit fixture must be rejected");
    Ok(CaseEvidence::policy(
        "redirect_count=3; maximum=3",
        error.to_string(),
    ))
}

fn redirect_special_use() -> Result<CaseEvidence, TransportError> {
    let request = HttpRequest::new(
        "redirect-special-use",
        "GET",
        HttpUrl::parse("https://media.example/public")?,
    )?;
    let error = redirect_policy()
        .apply(&request, HttpUrl::parse("https://127.0.0.1/admin")?)
        .expect_err("special-use literal redirect must fail");
    Ok(CaseEvidence::policy(
        "public HTTPS origin -> https://127.0.0.1/admin",
        error.to_string(),
    ))
}

fn redirect_policy() -> RedirectPolicy {
    RedirectPolicy {
        maximum_hops: 3,
        reject_https_downgrade: true,
    }
}

fn suffixes() -> PublicSuffixSet {
    PublicSuffixSet::new(["com".to_owned(), "invalid".to_owned()])
}

fn cookie_scope() -> Result<CaseEvidence, TransportError> {
    let source = HttpUrl::parse("https://media.example.com/account/index")?;
    let mut jar = CookieJar::new(4);
    jar.store(
        &source,
        Cookie {
            name: "session".to_owned(),
            value: SECRET_CANARY.to_owned(),
            domain: "media.example.com".to_owned(),
            host_only: true,
            path: "/account".to_owned(),
            secure: true,
        },
        &suffixes(),
    )?;
    let correct = jar.values_for(&source).len() == 1;
    let insecure = jar
        .values_for(&HttpUrl::parse("http://media.example.com/account/index")?)
        .is_empty();
    let cross_host = jar
        .values_for(&HttpUrl::parse("https://cdn.example.com/account/index")?)
        .is_empty();
    if !(correct && insecure && cross_host) {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-COOKIE-SCOPE".to_owned(),
        ));
    }
    Ok(CaseEvidence::policy(
        "secure host-only /account cookie",
        "cookie-scoped-to-secure-host-path",
    ))
}

fn cookie_public_suffix() -> Result<CaseEvidence, TransportError> {
    let source = HttpUrl::parse("https://media.example.com/")?;
    let error = CookieJar::new(4)
        .store(
            &source,
            Cookie {
                name: "bad".to_owned(),
                value: "value".to_owned(),
                domain: "com".to_owned(),
                host_only: false,
                path: "/".to_owned(),
                secure: false,
            },
            &suffixes(),
        )
        .expect_err("public suffix fixture must fail");
    Ok(CaseEvidence::policy("Domain=com", error.to_string()))
}

fn cookie_ip_domain() -> Result<CaseEvidence, TransportError> {
    let source = HttpUrl::parse("https://127.0.0.1/")?;
    let error = CookieJar::new(4)
        .store(
            &source,
            Cookie {
                name: "bad".to_owned(),
                value: "value".to_owned(),
                domain: "0.0.0.1".to_owned(),
                host_only: false,
                path: "/".to_owned(),
                secure: true,
            },
            &suffixes(),
        )
        .expect_err("cross-IP domain cookie must fail");
    Ok(CaseEvidence::policy(
        "source=127.0.0.1 Domain=0.0.0.1",
        error.to_string(),
    ))
}

fn cookie_syntax() -> Result<CaseEvidence, TransportError> {
    let source = HttpUrl::parse("https://media.example.com/")?;
    let error = CookieJar::new(4)
        .store(
            &source,
            Cookie {
                name: "bad\r\nname".to_owned(),
                value: "value".to_owned(),
                domain: "media.example.com".to_owned(),
                host_only: true,
                path: "/".to_owned(),
                secure: true,
            },
            &suffixes(),
        )
        .expect_err("control character fixture must fail");
    Ok(CaseEvidence::policy(
        "cookie name contains CRLF",
        error.to_string(),
    ))
}

fn cookie_count_boundary() -> Result<CaseEvidence, TransportError> {
    let source = HttpUrl::parse("https://media.example.com/")?;
    let mut jar = CookieJar::new(1);
    for name in ["first", "second"] {
        let result = jar.store(
            &source,
            Cookie {
                name: name.to_owned(),
                value: "value".to_owned(),
                domain: "media.example.com".to_owned(),
                host_only: true,
                path: "/".to_owned(),
                secure: true,
            },
            &suffixes(),
        );
        if name == "second" {
            let error = result.expect_err("second distinct cookie must exceed the bound");
            return Ok(CaseEvidence::policy(
                "maximum cookies=1; distinct cookie=2",
                error.to_string(),
            ));
        }
        result?;
    }
    Err(TransportError::Policy(
        "FF-TRANSPORT-E-COOKIE-BOUNDARY".to_owned(),
    ))
}

fn public_evidence() -> DnsEvidence {
    DnsEvidence {
        query_host: "media.example".to_owned(),
        answers: vec!["93.184.216.34".parse().expect("static public fixture IP")],
        selected: "93.184.216.34".parse().expect("static public fixture IP"),
        resolver_identity: "fixture-resolver-v1".to_owned(),
    }
}

fn dns_positive() -> CaseEvidence {
    validate_dns_evidence("media.example", &public_evidence(), ProxyEvidence::Direct)
        .expect("public evidence fixture must pass");
    let mut evidence = CaseEvidence::policy(
        "media.example -> 93.184.216.34 via fixture-resolver-v1",
        "dns-public-evidence-accepted",
    );
    "93.184.216.34".clone_into(&mut evidence.selected_address);
    evidence
}

fn dns_provenance_mismatch() -> CaseEvidence {
    let error = validate_dns_evidence("other.example", &public_evidence(), ProxyEvidence::Direct)
        .expect_err("host/provenance mismatch must fail");
    CaseEvidence::policy(
        "expected_host=other.example query_host=media.example",
        error.to_string(),
    )
}

fn ssrf_registry_boundary() -> CaseEvidence {
    let evidence = DnsEvidence {
        query_host: "media.example".to_owned(),
        answers: vec!["5f00::1".parse().expect("static IPv6 fixture")],
        selected: "5f00::1".parse().expect("static IPv6 fixture"),
        resolver_identity: "fixture-resolver-v1".to_owned(),
    };
    let error = validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct)
        .expect_err("IANA SRv6 special-purpose address must fail");
    CaseEvidence::policy("IANA special-purpose 5f00::/16", error.to_string())
}

fn ssrf_mixed_answers() -> CaseEvidence {
    let mut evidence = public_evidence();
    evidence
        .answers
        .push("127.0.0.1".parse().expect("static loopback fixture IP"));
    let error = validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct)
        .expect_err("mixed answer fixture must fail");
    CaseEvidence::policy("93.184.216.34 + 127.0.0.1", error.to_string())
}

fn dns_selection() -> CaseEvidence {
    let mut evidence = public_evidence();
    evidence.selected = "8.8.8.8".parse().expect("static fixture IP");
    let error = validate_dns_evidence("media.example", &evidence, ProxyEvidence::Direct)
        .expect_err("unapproved selection fixture must fail");
    CaseEvidence::policy("selected address absent from answers", error.to_string())
}

fn dns_rebind() -> CaseEvidence {
    let connected: IpAddr = "93.184.216.35".parse().expect("static fixture IP");
    let error = validate_connected_address("media.example", &public_evidence(), connected)
        .expect_err("rebinding fixture must fail");
    CaseEvidence::policy(
        "selected=93.184.216.34 connected=93.184.216.35",
        error.to_string(),
    )
}

fn proxy_evidence() -> CaseEvidence {
    let error = validate_dns_evidence(
        "media.example",
        &public_evidence(),
        ProxyEvidence::Unavailable,
    )
    .expect_err("missing proxy evidence fixture must fail");
    CaseEvidence::policy("proxy destination evidence unavailable", error.to_string())
}

fn proxy_evidence_positive() -> CaseEvidence {
    let address = "93.184.216.34".parse().expect("static fixture IP");
    validate_dns_evidence(
        "media.example",
        &public_evidence(),
        ProxyEvidence::TrustedDestinationEvidence(address),
    )
    .expect("trusted matching proxy evidence must pass");
    let mut evidence = CaseEvidence::policy(
        "trusted proxy destination=93.184.216.34",
        "proxy-destination-evidence-accepted",
    );
    evidence.selected_address = address.to_string();
    "trusted-fixture-proxy-v1".clone_into(&mut evidence.proxy_identity);
    evidence
}

fn pool_key(partition: &str, fingerprint: &str) -> PoolKey {
    PoolKey {
        scheme: "https".to_owned(),
        origin: "https://media.example:443".to_owned(),
        proxy_identity: "direct".to_owned(),
        tls_identity: "tls-standard".to_owned(),
        http_identity: "http1-standard".to_owned(),
        fingerprint_identity: fingerprint.to_owned(),
        client_certificate_identity: "none".to_owned(),
        session_partition: partition.to_owned(),
        credential_scope: "origin-media".to_owned(),
    }
}

fn pool_reuse() -> Result<CaseEvidence, TransportError> {
    let mut registry = PoolRegistry::new(4);
    let key = pool_key("session-a", "standard");
    let first = registry.acquire(key.clone())?;
    let second = registry.acquire(key)?;
    if first.reused || !second.reused || first.connection_id != second.connection_id {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-POOL-REUSE".to_owned(),
        ));
    }
    Ok(CaseEvidence {
        pool_identity: second.key_digest,
        ..CaseEvidence::policy("identical complete pool keys", "identical-pool-key-reused")
    })
}

fn pool_partition(session: bool) -> Result<CaseEvidence, TransportError> {
    let mut registry = PoolRegistry::new(4);
    let first_key = pool_key("session-a", "standard");
    let second_key = if session {
        pool_key("session-b", "standard")
    } else {
        pool_key("session-a", "browser-chrome-unknown")
    };
    let first = registry.acquire(first_key)?;
    let second = registry.acquire(second_key)?;
    if first.connection_id == second.connection_id || second.reused {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-POOL-PARTITION".to_owned(),
        ));
    }
    let outcome = if session {
        "session-partition-prevents-reuse"
    } else {
        "fingerprint-partition-prevents-reuse"
    };
    Ok(CaseEvidence {
        pool_identity: second.key_digest,
        ..CaseEvidence::policy("pool keys differ in one security dimension", outcome)
    })
}

fn pool_bound() -> Result<CaseEvidence, TransportError> {
    let mut registry = PoolRegistry::new(1);
    registry.acquire(pool_key("session-a", "standard"))?;
    let error = registry
        .acquire(pool_key("session-b", "standard"))
        .expect_err("pool capacity fixture must fail");
    Ok(CaseEvidence::policy(
        "maximum pool entries=1; requested=2",
        error.to_string(),
    ))
}

fn pool_all_dimensions() -> Result<CaseEvidence, TransportError> {
    let base = pool_key("session-a", "standard");
    let mut variants = Vec::new();
    for dimension in [
        "scheme",
        "origin",
        "proxy",
        "tls",
        "http",
        "fingerprint",
        "client_certificate",
        "session",
        "credential",
    ] {
        let mut key = base.clone();
        match dimension {
            "scheme" => {
                "http".clone_into(&mut key.scheme);
                "http://media.example:80".clone_into(&mut key.origin);
            }
            "origin" => "https://cdn.example:443".clone_into(&mut key.origin),
            "proxy" => "proxy-a".clone_into(&mut key.proxy_identity),
            "tls" => "tls-alternate".clone_into(&mut key.tls_identity),
            "http" => "http2-standard".clone_into(&mut key.http_identity),
            "fingerprint" => "fingerprint-a".clone_into(&mut key.fingerprint_identity),
            "client_certificate" => {
                "cert-a".clone_into(&mut key.client_certificate_identity);
            }
            "session" => "session-b".clone_into(&mut key.session_partition),
            "credential" => "origin-cdn".clone_into(&mut key.credential_scope),
            _ => unreachable!("static pool-key dimension"),
        }
        variants.push((dimension, key));
    }
    let mut registry = PoolRegistry::new(variants.len() + 1);
    let base_use = registry.acquire(base)?;
    for (dimension, variant) in variants {
        let acquired = registry.acquire(variant)?;
        if acquired.reused || acquired.connection_id == base_use.connection_id {
            return Err(TransportError::Policy(format!(
                "FF-TRANSPORT-E-POOL-DIMENSION: {dimension}"
            )));
        }
    }
    Ok(CaseEvidence::policy(
        "each complete pool-key dimension varied independently",
        "all-pool-key-dimensions-partitioned",
    ))
}

fn bounds_exact() -> CaseEvidence {
    BodyBudget {
        maximum_metadata_bytes: 10,
        maximum_body_bytes: 20,
        maximum_decompressed_bytes: 30,
    }
    .validate(10, 20, 30)
    .expect("exact bounds fixture must pass");
    let mut evidence = CaseEvidence::policy(
        "metadata=10 body=20 decompressed=30",
        "exact-resource-bounds-accepted",
    );
    evidence.limits = BTreeMap::from([
        ("metadata_bytes".to_owned(), 10),
        ("body_bytes".to_owned(), 20),
        ("decompressed_bytes".to_owned(), 30),
    ]);
    evidence
}

fn bounds_positive() -> CaseEvidence {
    BodyBudget {
        maximum_metadata_bytes: 10,
        maximum_body_bytes: 20,
        maximum_decompressed_bytes: 30,
    }
    .validate(9, 19, 29)
    .expect("below-bound fixture must pass");
    let mut evidence = CaseEvidence::policy(
        "metadata=9 body=19 decompressed=29",
        "below-resource-bounds-accepted",
    );
    evidence.limits = BTreeMap::from([
        ("maximum_metadata_bytes".to_owned(), 10),
        ("maximum_body_bytes".to_owned(), 20),
        ("maximum_decompressed_bytes".to_owned(), 30),
    ]);
    evidence
}

fn body_bound(kind: &str) -> CaseEvidence {
    let budget = BodyBudget {
        maximum_metadata_bytes: 10,
        maximum_body_bytes: 20,
        maximum_decompressed_bytes: 30,
    };
    let values = match kind {
        "metadata" => (11, 20, 30),
        "body" => (10, 21, 30),
        _ => (10, 20, 31),
    };
    let error = budget
        .validate(values.0, values.1, values.2)
        .expect_err("bound fixture must fail");
    let mut evidence = CaseEvidence::policy(
        format!(
            "metadata={} body={} decompressed={}",
            values.0, values.1, values.2
        ),
        error.to_string(),
    );
    let (maximum_key, maximum, observed_key, observed) = match kind {
        "metadata" => (
            "maximum_metadata_bytes",
            budget.maximum_metadata_bytes,
            "observed_metadata_bytes",
            values.0,
        ),
        "body" => (
            "maximum_body_bytes",
            budget.maximum_body_bytes,
            "observed_body_bytes",
            values.1,
        ),
        _ => (
            "maximum_decompressed_bytes",
            budget.maximum_decompressed_bytes,
            "observed_decompressed_bytes",
            values.2,
        ),
    };
    evidence.limits = BTreeMap::from([
        (maximum_key.to_owned(), maximum),
        (observed_key.to_owned(), observed),
    ]);
    evidence
}

fn byte_credit() -> Result<CaseEvidence, TransportError> {
    let mut credits = ByteCredits::new(8);
    credits.grant(4)?;
    let error = credits
        .accept(5)
        .expect_err("credit overrun fixture must fail");
    let mut evidence = CaseEvidence::policy("granted=4 attempted=5", error.to_string());
    evidence.limits = BTreeMap::from([
        ("maximum_body_bytes".to_owned(), 8),
        ("granted_byte_credits".to_owned(), 4),
        ("attempted_body_bytes".to_owned(), 5),
    ]);
    Ok(evidence)
}

fn retry_bound() -> CaseEvidence {
    let mut budget = RetryBudget::new(2);
    budget.retry().expect("first retry");
    budget.retry().expect("second retry");
    let error = budget.retry().expect_err("third retry must fail");
    CaseEvidence::policy("maximum_retries=2 attempted_retry=3", error.to_string())
}

fn retry_allowed() -> Result<CaseEvidence, TransportError> {
    let mut budget = RetryBudget::new(2);
    budget.retry()?;
    let exact = budget.retry()?;
    if exact != 2 {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-RETRY-BOUNDARY".to_owned(),
        ));
    }
    Ok(CaseEvidence::policy(
        "maximum_retries=2 retries_used=2",
        "retry-exact-limit-accepted",
    ))
}

fn cancellation_boundary() -> Result<CaseEvidence, TransportError> {
    let request_id = RequestId::new("request_cancel_boundary").map_err(contract_error)?;
    let target = RequestId::new("request_target_boundary").map_err(contract_error)?;
    let requester = ProducerId::new("producer_requester").map_err(contract_error)?;
    let acknowledger = ProducerId::new("producer_acknowledger").map_err(contract_error)?;
    let request = CancellationRequest {
        header: EnvelopeHeader {
            schema_id: "ff.cancel@1".to_owned(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id: request_id.clone(),
            producer_id: requester,
            job_id: None,
            sequence: 1,
        },
        target_request_id: target.clone(),
        generation: 1,
        reason: "transport_cancelled".to_owned(),
    };
    let acknowledgement = CancellationAcknowledgement {
        header: EnvelopeHeader {
            schema_id: "ff.cancel@1".to_owned(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id,
            producer_id: acknowledger.clone(),
            job_id: None,
            sequence: 2,
        },
        target_request_id: target,
        generation: 1,
        outcome: CancellationOutcome::Accepted,
    };
    validate_cancellation_correlation(
        &request,
        &acknowledgement,
        &acknowledger,
        ProtocolLimits::default(),
    )
    .map_err(|error| {
        TransportError::Cancellation(format!("FF-TRANSPORT-E-CANCEL-BOUNDARY: {error:?}"))
    })?;
    let mut evidence = CaseEvidence::policy(
        "matching request, target, producer, and generation=1",
        "cancellation-exact-generation-accepted",
    );
    evidence.limits = BTreeMap::from([
        ("cancellation_generation".to_owned(), 1),
        ("acknowledgement_sequence".to_owned(), 2),
    ]);
    Ok(evidence)
}

fn cancellation_positive() -> Result<CaseEvidence, TransportError> {
    let probe = run_cancellation_probe()?;
    if !probe.socket_shutdown || !probe.worker_reaped || probe.partial_body_bytes != 7 {
        return Err(TransportError::Cancellation(
            "FF-TRANSPORT-E-CANCEL-WORKER-REAP".to_owned(),
        ));
    }
    let mut evidence = CaseEvidence::policy(
        "partial_body_bytes=7; socket shutdown; worker joined",
        "cancelled-socket-worker-reaped",
    );
    "std-tcp-inflight-cancellation".clone_into(&mut evidence.executed_boundary);
    "loopback-cancelled-connection".clone_into(&mut evidence.connection_identity);
    "http/1.1-partial-body-then-shutdown".clone_into(&mut evidence.wire_identity);
    "http/1.1".clone_into(&mut evidence.protocol_identity);
    evidence.limits = BTreeMap::from([
        ("partial_body_bytes".to_owned(), probe.partial_body_bytes),
        ("worker_reaped".to_owned(), u64::from(probe.worker_reaped)),
    ]);
    Ok(evidence)
}

fn cancellation_correlation() -> Result<CaseEvidence, TransportError> {
    let request_id = RequestId::new("request_cancel").map_err(contract_error)?;
    let target = RequestId::new("request_target").map_err(contract_error)?;
    let requester = ProducerId::new("producer_requester").map_err(contract_error)?;
    let acknowledger = ProducerId::new("producer_acknowledger").map_err(contract_error)?;
    let request = CancellationRequest {
        header: EnvelopeHeader {
            schema_id: "ff.cancel@1".to_owned(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id: request_id.clone(),
            producer_id: requester,
            job_id: None,
            sequence: 1,
        },
        target_request_id: target,
        generation: 1,
        reason: "transport_cancelled".to_owned(),
    };
    let acknowledgement = CancellationAcknowledgement {
        header: EnvelopeHeader {
            schema_id: "ff.cancel@1".to_owned(),
            version: SchemaVersion { major: 1, minor: 0 },
            request_id,
            producer_id: acknowledger.clone(),
            job_id: None,
            sequence: 2,
        },
        target_request_id: RequestId::new("request_wrong").map_err(contract_error)?,
        generation: 1,
        outcome: CancellationOutcome::Accepted,
    };
    let error = validate_cancellation_correlation(
        &request,
        &acknowledgement,
        &acknowledger,
        ProtocolLimits::default(),
    )
    .expect_err("wrong target acknowledgement must fail");
    Ok(CaseEvidence::policy(
        "WP-005 cancellation request/ack target mismatch",
        format!("FF-TRANSPORT-E-CANCEL-CORRELATION: {error:?}"),
    ))
}

#[allow(clippy::needless_pass_by_value)]
fn contract_error(error: fforager_contracts::IdError) -> TransportError {
    TransportError::Cancellation(format!("FF-TRANSPORT-E-CONTRACT-ID: {error:?}"))
}

fn cancellation_pool() -> Result<CaseEvidence, TransportError> {
    let key = pool_key("session-cancel", "standard");
    let mut registry = PoolRegistry::new(2);
    let before = registry.acquire(key.clone())?;
    let mut cancellation = CancellationModel::new("request-a")?;
    let generation = cancellation.request()?;
    let probe = run_cancellation_probe()?;
    if !probe.socket_shutdown || !probe.worker_reaped || probe.partial_body_bytes != 7 {
        return Err(TransportError::Cancellation(
            "FF-TRANSPORT-E-CANCEL-WORKER-REAP".to_owned(),
        ));
    }
    cancellation.acknowledge("request-a", generation)?;
    if cancellation.pool_reusable() {
        return Err(TransportError::Cancellation(
            "FF-TRANSPORT-E-CANCELLED-POOL-REUSE".to_owned(),
        ));
    }
    if !registry.discard(&key) {
        return Err(TransportError::Cancellation(
            "FF-TRANSPORT-E-CANCELLED-POOL-DISCARD".to_owned(),
        ));
    }
    let after = registry.acquire(key)?;
    if after.reused || after.connection_id == before.connection_id {
        return Err(TransportError::Cancellation(
            "FF-TRANSPORT-E-CANCELLED-POOL-REUSE".to_owned(),
        ));
    }
    let mut evidence = CaseEvidence::policy(
        "request-a generation=1; partial socket shutdown; worker reaped; pool discarded",
        "cancelled-connection-not-reusable",
    );
    evidence.pool_identity = after.key_digest;
    Ok(evidence)
}

fn sanitized_replay() -> Result<CaseEvidence, TransportError> {
    let url = HttpUrl::parse(&format!(
        "https://media.example/video?access_token={SECRET_CANARY}&quality={SECRET_CANARY}"
    ))?;
    let mut request_headers = BTreeMap::new();
    request_headers.insert(
        "authorization".to_owned(),
        HeaderValue::new(SECRET_CANARY, true, true)?,
    );
    request_headers.insert(
        "accept".to_owned(),
        HeaderValue::new("video/*", false, false)?,
    );
    request_headers.insert(
        "x-auth-token".to_owned(),
        HeaderValue::new(SECRET_CANARY, false, false)?,
    );
    let response_headers = BTreeMap::from([
        (
            "date".to_owned(),
            "Mon, 01 Jan 2024 00:00:00 GMT".to_owned(),
        ),
        ("set-cookie".to_owned(), SECRET_CANARY.to_owned()),
        (
            "location".to_owned(),
            format!("https://cdn.example/file?signature={SECRET_CANARY}"),
        ),
    ]);
    let transcript = sanitize_exchange(
        "replay-a",
        "GET",
        &url,
        &request_headers,
        200,
        &response_headers,
        b"body",
    )?;
    let value = serde_json::to_value(&transcript)
        .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-JSON: {error}")))?;
    if value.to_string().contains(SECRET_CANARY) {
        return Err(TransportError::Policy(
            "FF-TRANSPORT-E-TRANSCRIPT-SECRET".to_owned(),
        ));
    }
    Ok(CaseEvidence {
        transcript: Some(value),
        ..CaseEvidence::policy(
            "authorization, token query, set-cookie, date, body",
            "transcript-deterministic-and-sanitized",
        )
    })
}

fn replay_boundary() -> Result<CaseEvidence, TransportError> {
    let url = HttpUrl::parse("https://media.example/replay")?;
    let request_headers = BTreeMap::from([(
        "x-boundary".to_owned(),
        HeaderValue::new("a".repeat(8 * 1024), false, false)?,
    )]);
    let transcript = sanitize_exchange(
        "replay-boundary",
        "GET",
        &url,
        &request_headers,
        200,
        &BTreeMap::new(),
        b"",
    )?;
    let mut evidence =
        CaseEvidence {
            transcript: Some(serde_json::to_value(transcript).map_err(|error| {
                TransportError::Protocol(format!("FF-TRANSPORT-E-JSON: {error}"))
            })?),
            ..CaseEvidence::policy(
                "request header value bytes=8192",
                "replay-header-exact-boundary-accepted",
            )
        };
    evidence.limits = BTreeMap::from([("maximum_header_value_bytes".to_owned(), 8 * 1024)]);
    Ok(evidence)
}

fn replay_negative() -> Result<CaseEvidence, TransportError> {
    let url = HttpUrl::parse("https://media.example/replay")?;
    let response_headers = BTreeMap::from([("x-overlong".to_owned(), "a".repeat(8 * 1024 + 1))]);
    let error = sanitize_exchange(
        "replay-negative",
        "GET",
        &url,
        &BTreeMap::new(),
        200,
        &response_headers,
        b"",
    )
    .expect_err("overlong response header must be rejected");
    let mut evidence = CaseEvidence::policy("response header value bytes=8193", error.to_string());
    evidence.limits = BTreeMap::from([("maximum_header_value_bytes".to_owned(), 8 * 1024)]);
    Ok(evidence)
}

fn url_positive() -> Result<CaseEvidence, TransportError> {
    let url = HttpUrl::parse("https://media.example/ok")?;
    Ok(CaseEvidence::policy(url.render(), "bounded-url-accepted"))
}

fn url_boundary() -> Result<CaseEvidence, TransportError> {
    let prefix = "https://media.example/";
    let value = format!("{prefix}{}", "a".repeat(4 * 1024 - prefix.len()));
    let url = HttpUrl::parse(&value)?;
    let mut evidence = CaseEvidence::policy(
        format!("URL bytes={}", value.len()),
        "url-exact-boundary-accepted",
    );
    evidence.limits = BTreeMap::from([("maximum_url_bytes".to_owned(), 4 * 1024)]);
    if value.len() != 4 * 1024
        || url.path_and_query.len() != 4 * 1024 - "https://media.example".len()
    {
        return Err(TransportError::InvalidUrl(
            "FF-TRANSPORT-E-URL-BOUNDARY".to_owned(),
        ));
    }
    Ok(evidence)
}

fn url_bounds() -> CaseEvidence {
    let overlong = format!("https://media.example/{}", "a".repeat(4 * 1024));
    let error = HttpUrl::parse(&overlong).expect_err("overlong URL fixture must fail");
    CaseEvidence::policy("URL exceeds 4096 bytes", error.to_string())
}

fn counterfactual_proof(manifest: &CorpusManifest, cases: &[CaseReport]) -> CounterfactualProof {
    let behavior_result = manifest
        .cases
        .iter()
        .position(|case| {
            CandidateAdapter::std_first()
                .negotiate(case.requested_capabilities.iter().copied())
                .execution_allowed
        })
        .map_or_else(
            || Err("no supported case available".to_owned()),
            |index| {
                let capability = manifest.cases[index]
                    .requested_capabilities
                    .iter()
                    .next()
                    .copied()
                    .ok_or_else(|| "counterfactual case has no capability".to_owned())?;
                let deficient = CandidateAdapter::std_first().without_capability(capability);
                let mut mutated = cases.to_vec();
                mutated[index] = run_case(&manifest.cases[index], &deficient);
                validate_aggregate_evidence(manifest, &mutated).map(|_| ())
            },
        );
    let evidence_result = cases
        .iter()
        .position(|case| !case.blocked_capabilities.is_empty())
        .map_or_else(
            || Err("no blocked case available".to_owned()),
            |index| {
                let mut mutated = cases.to_vec();
                mutated[index].blocked_capabilities.clear();
                let requested = mutated[index].requested_capabilities.clone();
                mutated[index].satisfied_capabilities.clone_from(&requested);
                validate_aggregate_evidence(manifest, &mutated).map(|_| ())
            },
        );
    let rejected = behavior_result.is_err() && evidence_result.is_err();
    CounterfactualProof {
        mutation:
            "remove one supported adapter capability; separately clear blocked evidence while preserving declarations"
                .to_owned(),
        rejected_by_same_oracle: rejected,
        rejection: format!(
            "behavior={}; evidence={}",
            behavior_result
                .err()
                .unwrap_or_else(|| "accepted".to_owned()),
            evidence_result
                .err()
                .unwrap_or_else(|| "accepted".to_owned())
        ),
    }
}

fn reject_secret_canary(report: &CorpusReport) -> Result<(), String> {
    let encoded = serde_json::to_string(report)
        .map_err(|error| format!("FF-TRANSPORT-E-REPORT-ENCODE: {error}"))?;
    if encoded.contains(SECRET_CANARY) {
        return Err("FF-TRANSPORT-E-REPORT-SECRET-CANARY".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn manifest() -> CorpusManifest {
        serde_json::from_str(include_str!("../../../fixtures/transport-v1/manifest.json"))
            .expect("canonical manifest")
    }

    fn report() -> CorpusReport {
        static REPORT: OnceLock<Result<CorpusReport, String>> = OnceLock::new();
        REPORT
            .get_or_init(|| run_corpus(&manifest()))
            .clone()
            .expect("corpus")
    }

    #[test]
    fn strict_manifest_rejects_unknown_fields() {
        let json = r#"{
            "schema_id":"fforager.transport-corpus-manifest.v1",
            "corpus_id":"WP-FF-007-transport-corpus-v1",
            "normalization_version":"WP-FF-004-sanitized-transcript-v1",
            "candidate_identity":"ferric-std-first-transport-spike-v1",
            "max_cases":1,
            "cases":[],
            "undeclared":true
        }"#;
        assert!(serde_json::from_str::<CorpusManifest>(json).is_err());
    }

    #[test]
    fn duplicate_case_ids_are_rejected() {
        let mut manifest = manifest();
        manifest.cases.push(manifest.cases[0].clone());
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn same_oracle_rejects_behavior_mutation() {
        let report = report();
        assert!(report.counterfactual.rejected_by_same_oracle);
    }

    #[test]
    fn mandatory_blocked_capability_forces_failed_spike_verdict() {
        let report = report();
        assert_eq!(
            report.verdict,
            AggregateVerdict::FailedSpikeRequiresOperatorDecision
        );
    }

    #[test]
    fn report_never_contains_secret_canary() {
        let report = report();
        let encoded = serde_json::to_string(&report).expect("report JSON");
        assert!(!encoded.contains(SECRET_CANARY));
    }

    #[test]
    fn missing_canonical_case_is_rejected() {
        let mut manifest = manifest();
        manifest.cases.pop();
        assert!(
            validate_manifest(&manifest)
                .expect_err("missing case")
                .contains("CANONICAL-CORPUS")
        );
    }

    #[test]
    fn supported_family_requires_positive_boundary_negative_triad() {
        let mut manifest = manifest();
        manifest
            .cases
            .retain(|case| case.kind != CaseKind::RangeBoundary);
        let error = validate_coverage(&manifest).expect_err("range boundary omission");
        assert_eq!(error, "FF-TRANSPORT-E-COVERAGE-CLASS: range:boundary");
    }

    #[test]
    fn forged_blocked_capability_state_is_rejected() {
        let manifest = manifest();
        let report = report();
        let mut cases = report.cases;
        let blocked = cases
            .iter_mut()
            .find(|case| !case.blocked_capabilities.is_empty())
            .expect("blocked case");
        blocked.blocked_capabilities.clear();
        blocked.satisfied_capabilities = blocked.requested_capabilities.clone();
        assert!(validate_aggregate_evidence(&manifest, &cases).is_err());
    }

    #[test]
    fn ip_policy_error_is_specific_for_rebinding() {
        assert!(matches!(
            validate_connected_address(
                "media.example",
                &public_evidence(),
                "93.184.216.35".parse().expect("fixture IP")
            ),
            Err(crate::policy::IpPolicyError::ConnectedAddressMismatch { .. })
        ));
    }
}
