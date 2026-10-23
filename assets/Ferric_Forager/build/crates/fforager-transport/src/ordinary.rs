use crate::Capability;
use crate::local_server::LocalProtocolServer;
use crate::policy::{CandidateAdapter, TransportError};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const SCHEMA_VERSION: &str = "ff-ordinary-transport-component-proof-v1";
const DECISION_ID: &str = "FF-DEC-002";
const REQWEST_ID: &str = "reqwest@0.13.4";
const RUSTLS_ID: &str = "rustls@0.23.46";
const RING_ID: &str = "ring@0.17.14";
const TRUST_ROOTS_ID: &str = "webpki-roots@1.0.10";
const PLATFORM_VERIFIER_ID: &str = "rustls-platform-verifier@0.7.0";
const COMPONENT_PASS: &str = "PASS_ORDINARY_TRANSPORT_COMPONENT_PROOF";
const ZERO_PROGRESS: &str =
    "PREREQUISITE only: zero Phase 1 product implementation and runtime progress";
const BROWSER_REQUIRED_CODE: &str = "FF-TRANSPORT-E-BROWSER-TRANSPORT-REQUIRED";

#[derive(Debug)]
pub enum OrdinaryTransportDecisionError {
    ClientBuild(String),
    Transport(TransportError),
    Policy(String),
    Report(String),
}

impl fmt::Display for OrdinaryTransportDecisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientBuild(detail) => {
                write!(formatter, "ordinary client build failed: {detail}")
            }
            Self::Transport(error) => write!(formatter, "{error}"),
            Self::Policy(detail) => write!(formatter, "ordinary transport policy failed: {detail}"),
            Self::Report(detail) => write!(formatter, "ordinary transport report failed: {detail}"),
        }
    }
}

impl std::error::Error for OrdinaryTransportDecisionError {}

impl From<TransportError> for OrdinaryTransportDecisionError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryControlCheck {
    pub id: String,
    pub proof_class: String,
    pub concrete_input: String,
    pub executed_boundary: String,
    pub expected_result: String,
    pub observed_result: String,
    pub status: String,
    pub skipped_semantic_dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrdinaryTransportDecisionReport {
    pub schema_version: String,
    pub decision_id: String,
    pub reqwest: String,
    pub rustls: String,
    pub crypto_provider: String,
    pub trust_roots: String,
    pub selected_native_runtime_packages: Vec<String>,
    pub default_features: bool,
    pub reqwest_features: Vec<String>,
    pub rustls_features: Vec<String>,
    pub checks: Vec<OrdinaryControlCheck>,
    pub ferric_owned_control_ids: Vec<String>,
    pub wreq_product_fallback: bool,
    pub product_progress: String,
    pub component_verdict: String,
    pub final_decision_verdict_emitted: bool,
}

fn rustls_client_config() -> Result<rustls::ClientConfig, OrdinaryTransportDecisionError> {
    let roots = webpki_roots::TLS_SERVER_ROOTS
        .iter()
        .cloned()
        .collect::<rustls::RootCertStore>();
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| OrdinaryTransportDecisionError::ClientBuild(error.to_string()))
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

fn build_ordinary_client_from(
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, OrdinaryTransportDecisionError> {
    builder
        .tls_backend_preconfigured(rustls_client_config()?)
        .no_proxy()
        .redirect(Policy::none())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .timeout(Duration::from_mins(1))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| OrdinaryTransportDecisionError::ClientBuild(error.to_string()))
}

fn build_ordinary_client() -> Result<reqwest::Client, OrdinaryTransportDecisionError> {
    build_ordinary_client_from(reqwest::Client::builder())
}

fn runtime() -> Result<tokio::runtime::Runtime, OrdinaryTransportDecisionError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| OrdinaryTransportDecisionError::ClientBuild(error.to_string()))
}

fn browser_refusal_check(
    runtime: &tokio::runtime::Runtime,
    id: &str,
    requested: impl IntoIterator<Item = Capability>,
) -> Result<OrdinaryControlCheck, OrdinaryTransportDecisionError> {
    let requested = requested.into_iter().collect::<BTreeSet<_>>();
    let factory_called = AtomicBool::new(false);
    let result =
        CandidateAdapter::ordinary_reqwest().execute_typed(requested.iter().copied(), |_| {
            factory_called.store(true, Ordering::SeqCst);
            let _runtime_context = runtime.enter();
            let _client = build_ordinary_client()?;
            Ok::<(), OrdinaryTransportDecisionError>(())
        });
    let blocked = match result {
        Err(OrdinaryTransportDecisionError::Transport(TransportError::CapabilityBlocked(
            blocked,
        ))) => blocked,
        Ok(_) => {
            return Err(OrdinaryTransportDecisionError::Policy(format!(
                "{id} unexpectedly received an execution grant"
            )));
        }
        Err(error) => {
            return Err(OrdinaryTransportDecisionError::Policy(format!(
                "{id} returned the wrong error variant: {error}"
            )));
        }
    };
    let observed = blocked
        .iter()
        .map(|item| item.capability)
        .collect::<BTreeSet<_>>();
    if factory_called.load(Ordering::SeqCst)
        || observed != requested
        || blocked
            .iter()
            .any(|item| item.code != BROWSER_REQUIRED_CODE)
    {
        return Err(OrdinaryTransportDecisionError::Policy(format!(
            "{id} did not return exact typed blockers before client construction"
        )));
    }
    Ok(OrdinaryControlCheck {
        id: id.to_owned(),
        proof_class: "public_boundary".to_owned(),
        concrete_input: format!("{requested:?}"),
        executed_boundary: "CandidateAdapter::execute_typed before client factory".to_owned(),
        expected_result: format!(
            "one exact {BROWSER_REQUIRED_CODE} blocker per requested fingerprint capability; client factory not called"
        ),
        observed_result: format!("typed blockers={observed:?}; client_factory_called=false"),
        status: "PASS".to_owned(),
        skipped_semantic_dependencies: vec![
            "No browser transport is selected or executed by this prerequisite proof.".to_owned(),
        ],
    })
}

#[allow(clippy::too_many_lines)]
fn local_http_control_checks(
    runtime: &tokio::runtime::Runtime,
) -> Result<Vec<OrdinaryControlCheck>, OrdinaryTransportDecisionError> {
    let redirect_server = LocalProtocolServer::spawn()?;
    let redirect_target = redirect_server.target();
    let seeded_proxy = reqwest::Proxy::all("http://127.0.0.1:9")
        .map_err(|error| OrdinaryTransportDecisionError::ClientBuild(error.to_string()))?;
    let redirect_client = {
        let _runtime_context = runtime.enter();
        build_ordinary_client_from(reqwest::Client::builder().proxy(seeded_proxy))?
    };
    let redirect_response = runtime
        .block_on(async {
            redirect_client
                .get(redirect_target.url("/redirect"))
                .header(
                    "x-ff-harness-authorization",
                    redirect_target.authorization(),
                )
                .send()
                .await
        })
        .map_err(|error| OrdinaryTransportDecisionError::Policy(error.to_string()))?;
    let redirect_status = redirect_response.status();
    drop(redirect_response);
    let redirect_request = redirect_server.finish()?;
    if redirect_status != reqwest::StatusCode::FOUND
        || !redirect_request.request_line.contains("/redirect")
    {
        return Err(OrdinaryTransportDecisionError::Policy(
            "proxy clearing or redirect refusal was not observed".to_owned(),
        ));
    }

    let retry_server = LocalProtocolServer::spawn()?;
    let retry_target = retry_server.target();
    let retry_seed = reqwest::retry::for_host("127.0.0.1")
        .no_budget()
        .max_retries_per_request(2)
        .classify_fn(|request_response| {
            if request_response.status() == Some(reqwest::StatusCode::SERVICE_UNAVAILABLE) {
                request_response.retryable()
            } else {
                request_response.success()
            }
        });
    let retry_client = {
        let _runtime_context = runtime.enter();
        build_ordinary_client_from(reqwest::Client::builder().retry(retry_seed))?
    };
    let retry_response = runtime
        .block_on(async {
            retry_client
                .get(retry_target.url("/retryable"))
                .header("x-ff-harness-authorization", retry_target.authorization())
                .send()
                .await
        })
        .map_err(|error| OrdinaryTransportDecisionError::Policy(error.to_string()))?;
    let retry_status = retry_response.status();
    drop(retry_response);
    let retry_request = retry_server.finish()?;
    if retry_status != reqwest::StatusCode::SERVICE_UNAVAILABLE
        || !retry_request.request_line.contains("/retryable")
    {
        return Err(OrdinaryTransportDecisionError::Policy(
            "dependency retry suppression was not observed".to_owned(),
        ));
    }

    let cookie_seed_server = LocalProtocolServer::spawn()?;
    let cookie_seed_target = cookie_seed_server.target();
    let cookie_client = {
        let _runtime_context = runtime.enter();
        build_ordinary_client()?
    };
    let cookie_seed_response = runtime
        .block_on(async {
            cookie_client
                .get(cookie_seed_target.url("/set-cookie"))
                .header(
                    "x-ff-harness-authorization",
                    cookie_seed_target.authorization(),
                )
                .send()
                .await
        })
        .map_err(|error| OrdinaryTransportDecisionError::Policy(error.to_string()))?;
    drop(cookie_seed_response);
    cookie_seed_server.finish()?;
    let cookie_probe_server = LocalProtocolServer::spawn()?;
    let cookie_probe_target = cookie_probe_server.target();
    let cookie_probe_response = runtime
        .block_on(async {
            cookie_client
                .get(cookie_probe_target.url("/retryable"))
                .header(
                    "x-ff-harness-authorization",
                    cookie_probe_target.authorization(),
                )
                .send()
                .await
        })
        .map_err(|error| OrdinaryTransportDecisionError::Policy(error.to_string()))?;
    drop(cookie_probe_response);
    let cookie_probe_request = cookie_probe_server.finish()?;
    if cookie_probe_request.headers.contains_key("cookie") {
        return Err(OrdinaryTransportDecisionError::Policy(
            "dependency cookie storage was not disabled".to_owned(),
        ));
    }

    let compression_client = {
        let _runtime_context = runtime.enter();
        build_ordinary_client()?
    };
    let compression_server = LocalProtocolServer::spawn()?;
    let compression_target = compression_server.target();
    let acknowledgement_target = compression_target.clone();
    let acknowledgement = std::thread::spawn(move || acknowledgement_target.acknowledge_fragment());
    let compressed_result = runtime
        .block_on(async {
            compression_client
                .get(compression_target.url("/gzip"))
                .header(
                    "x-ff-harness-authorization",
                    compression_target.authorization(),
                )
                .send()
                .await?
                .bytes()
                .await
        })
        .map_err(|error| OrdinaryTransportDecisionError::Policy(error.to_string()));
    let compressed = match compressed_result {
        Ok(compressed) => compressed,
        Err(primary_error) => {
            reap_failed_compression_probe(compression_server, acknowledgement)?;
            return Err(primary_error);
        }
    };
    acknowledgement.join().map_err(|_| {
        OrdinaryTransportDecisionError::Policy(
            "decompression acknowledgement worker panicked".to_owned(),
        )
    })??;
    let compression_request = compression_server.finish()?;
    if !compressed.starts_with(&[31, 139]) || !compression_request.request_line.contains("/gzip") {
        return Err(OrdinaryTransportDecisionError::Policy(
            "transparent decompression was not disabled".to_owned(),
        ));
    }

    Ok(vec![
        OrdinaryControlCheck {
            id: "ordinary-proxy-and-redirect-disabled".to_owned(),
            proof_class: "wire_boundary".to_owned(),
            concrete_input: "seeded unreachable proxy; authenticated local /redirect".to_owned(),
            executed_boundary: "configured reqwest client -> local TCP harness".to_owned(),
            expected_result: "seed proxy cleared and 302 returned without following Location"
                .to_owned(),
            observed_result: format!(
                "status={redirect_status}; request_line={}",
                redirect_request.request_line
            ),
            status: "PASS".to_owned(),
            skipped_semantic_dependencies: vec![
                "This proves dependency proxy/redirect authority is disabled; Ferric DNS/SSRF and redirect approval remain unimplemented product controls.".to_owned(),
            ],
        },
        OrdinaryControlCheck {
            id: "ordinary-dependency-retry-disabled".to_owned(),
            proof_class: "wire_boundary".to_owned(),
            concrete_input: "seeded retry-on-503 policy; authenticated local /retryable".to_owned(),
            executed_boundary: "configured reqwest retry service -> one-request local TCP harness"
                .to_owned(),
            expected_result: "503 returned from the first request without a retry attempt".to_owned(),
            observed_result: format!(
                "status={retry_status}; request_line={}",
                retry_request.request_line
            ),
            status: "PASS".to_owned(),
            skipped_semantic_dependencies: vec![
                "Ferric retry budgets and attempt receipts remain unimplemented product controls."
                    .to_owned(),
            ],
        },
        OrdinaryControlCheck {
            id: "ordinary-transparent-decompression-disabled".to_owned(),
            proof_class: "wire_boundary".to_owned(),
            concrete_input: "authenticated local gzip-encoded payload".to_owned(),
            executed_boundary: "configured reqwest response body -> byte observation".to_owned(),
            expected_result: "gzip framing bytes remain intact".to_owned(),
            observed_result: format!(
                "body_bytes={}; prefix={:?}",
                compressed.len(),
                &compressed[..2]
            ),
            status: "PASS".to_owned(),
            skipped_semantic_dependencies: vec![
                "Ferric decompressed-byte admission remains an unimplemented product control."
                    .to_owned(),
            ],
        },
        OrdinaryControlCheck {
            id: "ordinary-cookie-storage-disabled".to_owned(),
            proof_class: "wire_boundary".to_owned(),
            concrete_input:
                "one client receives Set-Cookie, then requests a second loopback origin".to_owned(),
            executed_boundary: "configured reqwest client cookie state -> captured request headers"
                .to_owned(),
            expected_result: "the second request contains no Cookie header".to_owned(),
            observed_result: format!("headers={:?}", cookie_probe_request.headers.keys()),
            status: "PASS".to_owned(),
            skipped_semantic_dependencies: vec![
                "Ferric cookie storage, scope, persistence, and session partitioning remain unimplemented product controls.".to_owned(),
            ],
        },
    ])
}

fn reap_failed_compression_probe(
    server: LocalProtocolServer,
    acknowledgement: JoinHandle<Result<(), TransportError>>,
) -> Result<(), OrdinaryTransportDecisionError> {
    drop(server);
    let _acknowledgement_result = acknowledgement.join().map_err(|_| {
        OrdinaryTransportDecisionError::Policy(
            "decompression acknowledgement worker panicked while reaping failed probe".to_owned(),
        )
    })?;
    Ok(())
}

/// Executes behavior-sensitive component proof for the exact non-shipped
/// ordinary transport candidate. It intentionally cannot emit the packet's
/// final decision verdict.
///
/// # Errors
///
/// Returns an error if capability admission, exact client construction, or a
/// local proxy/redirect/retry/decompression control probe fails.
pub fn run_ordinary_transport_decision()
-> Result<OrdinaryTransportDecisionReport, OrdinaryTransportDecisionError> {
    let runtime = runtime()?;
    let mut checks = vec![
        browser_refusal_check(
            &runtime,
            "ordinary-browser-required-tls",
            [Capability::TlsFingerprint],
        )?,
        browser_refusal_check(
            &runtime,
            "ordinary-browser-required-http2",
            [Capability::Http2Fingerprint],
        )?,
        browser_refusal_check(
            &runtime,
            "ordinary-browser-required-combined",
            [Capability::TlsFingerprint, Capability::Http2Fingerprint],
        )?,
    ];
    let _client = {
        let _runtime_context = runtime.enter();
        build_ordinary_client()?
    };
    checks.push(OrdinaryControlCheck {
        id: "ordinary-provider-bound-client-construction".to_owned(),
        proof_class: "integration".to_owned(),
        concrete_input: format!("{REQWEST_ID}; {RUSTLS_ID}; {RING_ID}; {TRUST_ROOTS_ID}"),
        executed_boundary:
            "ring provider -> rustls ClientConfig -> reqwest tls_backend_preconfigured".to_owned(),
        expected_result:
            "client builds without process-global provider or platform trust-store selection"
                .to_owned(),
        observed_result: "provider-bound client constructed".to_owned(),
        status: "PASS".to_owned(),
        skipped_semantic_dependencies: vec![
            "Public certificate-chain fixtures and staged product HTTPS remain future runtime proof."
                .to_owned(),
        ],
    });
    checks.extend(local_http_control_checks(&runtime)?);

    Ok(OrdinaryTransportDecisionReport {
        schema_version: SCHEMA_VERSION.to_owned(),
        decision_id: DECISION_ID.to_owned(),
        reqwest: REQWEST_ID.to_owned(),
        rustls: RUSTLS_ID.to_owned(),
        crypto_provider: RING_ID.to_owned(),
        trust_roots: TRUST_ROOTS_ID.to_owned(),
        selected_native_runtime_packages: vec![PLATFORM_VERIFIER_ID.to_owned()],
        default_features: false,
        reqwest_features: vec![
            "http2".to_owned(),
            "rustls-no-provider".to_owned(),
            "stream".to_owned(),
        ],
        rustls_features: vec!["ring".to_owned(), "std".to_owned(), "tls12".to_owned()],
        checks,
        ferric_owned_control_ids: vec![
            "address_admission".to_owned(),
            "dns_provenance_and_ssrf".to_owned(),
            "redirect_approval".to_owned(),
            "cookie_storage_and_scope".to_owned(),
            "proxy_authorization".to_owned(),
            "connection_pool_partitioning".to_owned(),
            "global_and_per_origin_in_flight_admission".to_owned(),
            "download_and_decompression_limits".to_owned(),
            "retry_budgets".to_owned(),
            "timeouts_and_cancellation".to_owned(),
            "receipts_and_diagnostics".to_owned(),
        ],
        wreq_product_fallback: false,
        product_progress: ZERO_PROGRESS.to_owned(),
        component_verdict: COMPONENT_PASS.to_owned(),
        final_decision_verdict_emitted: false,
    })
}

/// Strictly validates a serialized component report against newly executed,
/// behavior-sensitive local proof.
///
/// # Errors
///
/// Returns an error for malformed, stale, incomplete, forged, or behaviorally
/// irreproducible reports.
pub fn validate_ordinary_transport_decision_report(
    bytes: &[u8],
) -> Result<(), OrdinaryTransportDecisionError> {
    let observed: OrdinaryTransportDecisionReport = serde_json::from_slice(bytes)
        .map_err(|error| OrdinaryTransportDecisionError::Report(error.to_string()))?;
    let expected = run_ordinary_transport_decision()?;
    if observed != expected {
        return Err(OrdinaryTransportDecisionError::Report(
            "report does not match fresh behavior-sensitive component proof".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn exact_client_and_browser_required_boundary_pass_component_validation() {
        let report = run_ordinary_transport_decision().expect("exact component proof");
        assert_eq!(report.component_verdict, COMPONENT_PASS);
        assert!(!report.final_decision_verdict_emitted);
        assert!(!report.wreq_product_fallback);
        assert_eq!(report.product_progress, ZERO_PROGRESS);
        let bytes = serde_json::to_vec(&report).expect("serialize report");
        validate_ordinary_transport_decision_report(&bytes).expect("strict report");
    }

    #[test]
    fn browser_refusals_are_independent_typed_and_precede_client_factory() {
        let runtime = runtime().expect("runtime");
        browser_refusal_check(&runtime, "tls-only", [Capability::TlsFingerprint])
            .expect("tls refusal");
        browser_refusal_check(&runtime, "http2-only", [Capability::Http2Fingerprint])
            .expect("http2 refusal");
        browser_refusal_check(
            &runtime,
            "combined",
            [Capability::TlsFingerprint, Capability::Http2Fingerprint],
        )
        .expect("combined refusal");
    }

    #[test]
    fn dependency_cookie_storage_boundary_is_disabled() {
        let runtime = runtime().expect("runtime");
        let checks = local_http_control_checks(&runtime).expect("local control checks");
        let cookie = checks
            .iter()
            .find(|check| check.id == "ordinary-cookie-storage-disabled")
            .expect("cookie boundary check");
        assert_eq!(cookie.status, "PASS");
        assert!(
            cookie
                .observed_result
                .contains("x-ff-harness-authorization")
        );
        assert!(!cookie.observed_result.contains("\"cookie\""));
    }

    #[test]
    fn forged_control_or_dependency_identity_is_rejected() {
        let report = run_ordinary_transport_decision().expect("exact component proof");
        let mut value = serde_json::to_value(report).expect("report value");
        value["checks"][0]["observed_result"] =
            serde_json::Value::String("forged execution".to_owned());
        assert!(
            validate_ordinary_transport_decision_report(
                &serde_json::to_vec(&value).expect("forged report")
            )
            .is_err()
        );

        let report = run_ordinary_transport_decision().expect("exact component proof");
        let mut value = serde_json::to_value(report).expect("report value");
        value["crypto_provider"] = serde_json::Value::String("aws-lc-rs@unapproved".to_owned());
        assert!(
            validate_ordinary_transport_decision_report(
                &serde_json::to_vec(&value).expect("forged report")
            )
            .is_err()
        );
    }

    #[test]
    fn failed_compression_probe_reaps_server_and_acknowledgement_workers() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let target = server.target();
        let acknowledgement = std::thread::spawn(move || target.acknowledge_fragment());
        let started = Instant::now();
        reap_failed_compression_probe(server, acknowledgement).expect("workers reaped");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
