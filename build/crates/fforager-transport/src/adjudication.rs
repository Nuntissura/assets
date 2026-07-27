use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

const BACKEND_ID: &str = "wreq";
const BACKEND_VERSION: &str = "5.3.0";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_IDLE_PER_HOST: usize = 2;

/// Browser profile requested from the non-shipped transport adjudication adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintProfile {
    /// Ordinary transport with no browser fingerprint claim.
    None,
    /// Fixed Windows Chrome profile used by the v6 adjudication candidate.
    Chrome136Windows,
}

/// External evidence status for an asserted browser wire profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "receipt", rename_all = "snake_case")]
pub enum WireProof {
    NotProvided,
    ExternalReceipt(String),
}

/// Whether a dependency-owned policy convenience is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvenienceState {
    Disabled,
    Enabled,
}

/// Dependency conveniences that Ferric must keep disabled at this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConveniences {
    pub ambient_proxy: ConvenienceState,
    pub automatic_redirects: ConvenienceState,
    pub internal_cookie_store: ConvenienceState,
    pub transparent_decompression: ConvenienceState,
}

/// One bounded request supplied to the non-shipped candidate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjudicationRequest {
    /// Absolute HTTP or HTTPS URL.
    pub url: String,
    /// Requested browser profile.
    pub profile: FingerprintProfile,
    /// Maximum response body bytes accepted by this bounded control.
    pub maximum_body_bytes: u64,
    /// Header pairs supplied by the Ferric-owned harness.
    pub headers: Vec<(String, String)>,
}

/// Behavior and evidence observed at the candidate boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdjudicationObservation {
    pub backend_id: String,
    pub backend_version: String,
    pub requested_profile: FingerprintProfile,
    pub configured_profile: Option<String>,
    pub wire_proof: WireProof,
    pub policy_conveniences: PolicyConveniences,
    pub status: u16,
    pub protocol: String,
    pub peer_address: Option<String>,
    pub body_bytes: u64,
    pub body_sha256: String,
}

/// Stable failures returned by the candidate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjudicationError {
    UnsupportedProfile {
        backend_version: &'static str,
        profile: FingerprintProfile,
    },
    InvalidRequest(String),
    ClientBuild(String),
    Runtime(String),
    Request(String),
    BodyTooLarge {
        maximum: u64,
        observed: u64,
    },
}

impl fmt::Display for AdjudicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProfile {
                backend_version,
                profile,
            } => write!(
                formatter,
                "FF-WREQ-E-UNSUPPORTED-PROFILE: wreq {backend_version} cannot supply {profile:?} without an unselected profile bundle"
            ),
            Self::InvalidRequest(detail) => {
                write!(formatter, "FF-WREQ-E-INVALID-REQUEST: {detail}")
            }
            Self::ClientBuild(detail) => write!(formatter, "FF-WREQ-E-CLIENT-BUILD: {detail}"),
            Self::Runtime(detail) => write!(formatter, "FF-WREQ-E-RUNTIME: {detail}"),
            Self::Request(detail) => write!(formatter, "FF-WREQ-E-REQUEST: {detail}"),
            Self::BodyTooLarge { maximum, observed } => write!(
                formatter,
                "FF-WREQ-E-BODY-LIMIT: maximum={maximum}, observed={observed}"
            ),
        }
    }
}

impl std::error::Error for AdjudicationError {}

/// Stable wreq 5.3.0 control adapter.
///
/// The control deliberately excludes `wreq-util` 2.x. Its predefined browser
/// profile bundle is GPL-3.0 and was not selected for this adjudication. A
/// browser-profile request therefore fails before any network execution.
#[derive(Debug)]
pub struct WreqAdjudicationAdapter {
    client: wreq::Client,
}

impl WreqAdjudicationAdapter {
    /// Constructs a client with dependency conveniences disabled so Ferric
    /// remains authoritative for proxy, redirect, cookie, and decompression policy.
    ///
    /// # Errors
    ///
    /// Returns a typed client-build failure when the TLS/client configuration
    /// cannot be constructed.
    pub fn new() -> Result<Self, AdjudicationError> {
        let client = wreq::Client::builder()
            .no_proxy()
            .redirect(wreq::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .pool_max_idle_per_host(MAX_IDLE_PER_HOST)
            .build()
            .map_err(|error| AdjudicationError::ClientBuild(error.to_string()))?;
        Ok(Self { client })
    }

    /// Executes one bounded ordinary-transport control request.
    ///
    /// # Errors
    ///
    /// Returns a typed failure for an unsupported fingerprint profile, invalid
    /// request, runtime/client failure, or response body bound violation.
    pub fn execute(
        &self,
        request: AdjudicationRequest,
    ) -> Result<AdjudicationObservation, AdjudicationError> {
        if request.profile != FingerprintProfile::None {
            return Err(AdjudicationError::UnsupportedProfile {
                backend_version: BACKEND_VERSION,
                profile: request.profile,
            });
        }
        if request.maximum_body_bytes == 0
            || request.url.len() > 8 * 1024
            || !matches!(
                request.url.split_once("://").map(|(scheme, _)| scheme),
                Some("http" | "https")
            )
        {
            return Err(AdjudicationError::InvalidRequest(
                "URL scheme/length or body bound is invalid".to_owned(),
            ));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|error| AdjudicationError::Runtime(error.to_string()))?;
        runtime.block_on(self.execute_async(request))
    }

    async fn execute_async(
        &self,
        request: AdjudicationRequest,
    ) -> Result<AdjudicationObservation, AdjudicationError> {
        let mut builder = self.client.get(&request.url);
        for (name, value) in request.headers {
            builder = builder.header(&name, &value);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| AdjudicationError::Request(error.to_string()))?;
        let status = response.status().as_u16();
        let protocol = format!("{:?}", response.version());
        let peer_address = response.remote_addr().map(format_socket_address);
        if let Some(length) = response.content_length()
            && length > request.maximum_body_bytes
        {
            return Err(AdjudicationError::BodyTooLarge {
                maximum: request.maximum_body_bytes,
                observed: length,
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| AdjudicationError::Request(error.to_string()))?;
        let body_bytes = u64::try_from(body.len()).map_err(|error| {
            AdjudicationError::Request(format!("response body length conversion: {error}"))
        })?;
        if body_bytes > request.maximum_body_bytes {
            return Err(AdjudicationError::BodyTooLarge {
                maximum: request.maximum_body_bytes,
                observed: body_bytes,
            });
        }
        Ok(AdjudicationObservation {
            backend_id: BACKEND_ID.to_owned(),
            backend_version: BACKEND_VERSION.to_owned(),
            requested_profile: FingerprintProfile::None,
            configured_profile: None,
            wire_proof: WireProof::NotProvided,
            policy_conveniences: PolicyConveniences {
                ambient_proxy: ConvenienceState::Disabled,
                automatic_redirects: ConvenienceState::Disabled,
                internal_cookie_store: ConvenienceState::Disabled,
                transparent_decompression: ConvenienceState::Disabled,
            },
            status,
            protocol,
            peer_address,
            body_bytes,
            body_sha256: encode_hex(&Sha256::digest(&body)),
        })
    }
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
    use crate::local_server::LocalProtocolServer;

    #[test]
    fn stable_control_executes_http11_with_policy_conveniences_disabled() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let target = server.target();
        let adapter = WreqAdjudicationAdapter::new().expect("stable adapter");
        let observation = adapter
            .execute(AdjudicationRequest {
                url: target.url("/ok"),
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
                headers: vec![(
                    "x-ff-harness-authorization".to_owned(),
                    target.authorization().to_owned(),
                )],
            })
            .expect("bounded local request");
        target
            .acknowledge_fragment()
            .expect("fragment acknowledgement");
        let captured = server.finish().expect("server receipt");

        assert_eq!(observation.status, 200);
        assert_eq!(observation.protocol, "HTTP/1.1");
        assert_eq!(observation.body_bytes, 16);
        assert_eq!(
            observation.policy_conveniences,
            PolicyConveniences {
                ambient_proxy: ConvenienceState::Disabled,
                automatic_redirects: ConvenienceState::Disabled,
                internal_cookie_store: ConvenienceState::Disabled,
                transparent_decompression: ConvenienceState::Disabled,
            }
        );
        assert_eq!(observation.wire_proof, WireProof::NotProvided);
        assert_eq!(captured.request_line, "GET /ok HTTP/1.1");
    }

    #[test]
    fn stable_control_blocks_profile_before_network_execution() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let target = server.target();
        let adapter = WreqAdjudicationAdapter::new().expect("stable adapter");
        let error = adapter
            .execute(AdjudicationRequest {
                url: target.url("/ok"),
                profile: FingerprintProfile::Chrome136Windows,
                maximum_body_bytes: 64,
                headers: Vec::new(),
            })
            .expect_err("profile must fail closed");
        assert!(matches!(
            error,
            AdjudicationError::UnsupportedProfile {
                backend_version: "5.3.0",
                profile: FingerprintProfile::Chrome136Windows
            }
        ));
        drop(server);
    }

    #[test]
    fn stable_control_rejects_declared_body_before_allocation() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let target = server.target();
        let adapter = WreqAdjudicationAdapter::new().expect("stable adapter");
        let error = adapter
            .execute(AdjudicationRequest {
                url: target.url("/declared-oversize"),
                profile: FingerprintProfile::None,
                maximum_body_bytes: 64,
                headers: vec![(
                    "x-ff-harness-authorization".to_owned(),
                    target.authorization().to_owned(),
                )],
            })
            .expect_err("declared body must be rejected");
        assert_eq!(
            error,
            AdjudicationError::BodyTooLarge {
                maximum: 64,
                observed: 1_073_741_824
            }
        );
        let _ = server.finish().expect("server receipt");
    }
}
