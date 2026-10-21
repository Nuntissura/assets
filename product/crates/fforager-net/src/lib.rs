//! Ferric-owned bounded ordinary HTTP acquisition under FF-DEC-002.
#![forbid(unsafe_code)]

mod address;

/// Parses the ordinary URL before a caller opens durable job state.
/// # Errors
/// Rejects invalid schemes, authorities, credentials, and fragments.
pub fn validate_url(input: &str) -> Result<(), TransportError> {
    address::parse_url(input).map(|_| ())
}

use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING};
use std::fmt;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAXIMUM_HEADER_BYTES: usize = 64 * 1024;
const MAXIMUM_BUFFERED_CHUNK: usize = 64 * 1024;
const WRITE_CHUNK: usize = 16 * 1024;
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// Per-transfer limits. The loopback permit is one exact socket, never a subnet bypass.
#[derive(Clone, Debug)]
pub struct TransportPolicy {
    pub max_bytes: u64,
    pub total_timeout: Duration,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub allow_loopback: Option<SocketAddr>,
}

impl Default for TransportPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            total_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(30),
            allow_loopback: None,
        }
    }
}

/// Stable transport failures; messages omit request URLs and response header values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportErrorKind {
    InvalidInput,
    AddressPolicy,
    AddressMismatch,
    ResourceExhausted,
    Cancelled,
    TimedOut,
    Network,
    ResponseStatus,
    RedirectDenied,
    EncodedResponse,
    SizeLimit,
    Sink,
}

#[derive(Debug)]
pub struct TransportError {
    pub kind: TransportErrorKind,
    detail: String,
}

impl TransportError {
    fn new(kind: TransportErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    fn input(detail: &'static str) -> Self {
        Self::new(TransportErrorKind::InvalidInput, detail)
    }

    /// Returns the stable diagnostic identifier for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.kind {
            TransportErrorKind::InvalidInput => "FF-TRANSPORT-E-INVALID-INPUT",
            TransportErrorKind::AddressPolicy => "FF-TRANSPORT-E-SSRF-SPECIAL-USE",
            TransportErrorKind::AddressMismatch => "FF-TRANSPORT-E-DNS-REBIND",
            TransportErrorKind::ResourceExhausted => "FF-TRANSPORT-E-RESOURCE-EXHAUSTED",
            TransportErrorKind::Cancelled => "FF-TRANSPORT-E-CANCELLED",
            TransportErrorKind::TimedOut => "FF-TRANSPORT-E-TIMED-OUT",
            TransportErrorKind::Network => "FF-TRANSPORT-E-NETWORK",
            TransportErrorKind::ResponseStatus => "FF-TRANSPORT-E-HTTP-STATUS",
            TransportErrorKind::RedirectDenied => "FF-TRANSPORT-E-REDIRECT-POLICY",
            TransportErrorKind::EncodedResponse => "FF-TRANSPORT-E-CONTENT-ENCODING",
            TransportErrorKind::SizeLimit => "FF-TRANSPORT-E-BODY-LIMIT",
            TransportErrorKind::Sink => "FF-TRANSPORT-E-SINK",
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.detail)
    }
}

impl std::error::Error for TransportError {}

/// Network completion only; durable output and archive acknowledgement belong to the engine.
#[derive(Clone, Debug)]
pub struct TransferReceipt {
    pub bytes_received: u64,
    pub status: u16,
    pub selected_address: SocketAddr,
    pub connected_address: SocketAddr,
    pub content_length: Option<u64>,
    pub elapsed: Duration,
}

/// One admitted transfer at a time, with a fresh client and no cross-context pool reuse.
#[derive(Debug)]
pub struct OrdinaryTransport {
    policy: TransportPolicy,
    in_flight: AtomicBool,
}

struct Admission<'a>(&'a AtomicBool);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl OrdinaryTransport {
    /// Creates the ordinary transport without starting network work.
    ///
    /// # Errors
    /// Rejects zero or excessive bounds and any non-loopback exception.
    pub fn new(mut policy: TransportPolicy) -> Result<Self, TransportError> {
        if policy.max_bytes == 0
            || policy.total_timeout.is_zero()
            || policy.total_timeout > Duration::from_secs(24 * 60 * 60)
            || policy.connect_timeout.is_zero()
            || policy.idle_timeout.is_zero()
            || policy
                .allow_loopback
                .is_some_and(|address| !address.ip().is_loopback() || address.port() == 0)
        {
            return Err(TransportError::input(
                "invalid transfer limits or loopback permit",
            ));
        }
        policy.connect_timeout = policy.connect_timeout.min(policy.total_timeout);
        policy.idle_timeout = policy.idle_timeout.min(policy.total_timeout);
        Ok(Self {
            policy,
            in_flight: AtomicBool::new(false),
        })
    }

    /// Resolves and admits every answer before selecting an address.
    ///
    /// The caller deadline bounds awaiting DNS; OS resolver work may outlive a cancelled lookup.
    /// # Errors
    /// Rejects invalid URLs, forbidden answers, cancellation, and deadline exhaustion.
    pub async fn resolve_addresses(
        &self,
        input: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<SocketAddr>, TransportError> {
        let url = address::parse_url(input)?;
        let host = url
            .host_str()
            .ok_or_else(|| TransportError::input("missing host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| TransportError::input("missing port"))?;
        let addresses = if let Some(ip) = address::literal_address(host) {
            check_control(cancel, Instant::now() + self.policy.total_timeout)?;
            vec![SocketAddr::new(ip, port)]
        } else {
            let answers = controlled(
                tokio::net::lookup_host((host, port)),
                cancel,
                Instant::now() + self.policy.connect_timeout,
            )
            .await?
            .map_err(|_| {
                TransportError::new(TransportErrorKind::Network, "DNS resolution failed")
            })?;
            answers.take(address::MAXIMUM_DNS_ANSWERS + 1).collect()
        };
        address::validate_addresses(&url, &addresses, &self.policy)?;
        Ok(addresses)
    }

    /// Streams one GET into a bounded synchronous sink, with no implicit retry or redirect.
    ///
    /// The sink must return from each write; cancellation is observed between bounded writes.
    /// # Errors
    /// Returns typed policy, timeout, cancellation, response, byte-limit, or sink failures.
    pub async fn download<W: Write + ?Sized>(
        &self,
        input: &str,
        admitted_addresses: &[SocketAddr],
        sink: &mut W,
        cancel: &AtomicBool,
    ) -> Result<TransferReceipt, TransportError> {
        let started = Instant::now();
        let deadline = started + self.policy.total_timeout;
        check_control(cancel, deadline)?;
        let url = address::parse_url(input)?;
        let selected = address::validate_addresses(&url, admitted_addresses, &self.policy)?;
        self.in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                TransportError::new(
                    TransportErrorKind::ResourceExhausted,
                    "transfer capacity is occupied",
                )
            })?;
        let _admission = Admission(&self.in_flight);
        let client = self.client(&url, selected)?;
        let response = controlled(
            client.get(url).header(ACCEPT_ENCODING, "identity").send(),
            cancel,
            deadline,
        )
        .await?
        .map_err(network_error)?;
        let (status, connected, content_length) = self.validate_response(&response, selected)?;
        let bytes_received = self.copy_response(response, sink, cancel, deadline).await?;
        if content_length.is_some_and(|length| length != bytes_received) {
            return Err(TransportError::new(
                TransportErrorKind::Network,
                "response body length is inconsistent",
            ));
        }
        check_control(cancel, deadline)?;
        Ok(TransferReceipt {
            bytes_received,
            status,
            selected_address: selected,
            connected_address: connected,
            content_length,
            elapsed: started.elapsed(),
        })
    }

    fn client(
        &self,
        url: &reqwest::Url,
        selected: SocketAddr,
    ) -> Result<reqwest::Client, TransportError> {
        let roots = webpki_roots::TLS_SERVER_ROOTS
            .iter()
            .cloned()
            .collect::<rustls::RootCertStore>();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| {
            TransportError::new(
                TransportErrorKind::Network,
                "TLS provider configuration failed",
            )
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let host = url
            .host_str()
            .ok_or_else(|| TransportError::input("missing host"))?;
        reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .connect_timeout(self.policy.connect_timeout)
            .read_timeout(self.policy.idle_timeout)
            .timeout(self.policy.total_timeout)
            .pool_max_idle_per_host(0)
            .http2_adaptive_window(false)
            .http2_initial_stream_window_size(64 * 1024)
            .http2_initial_connection_window_size(64 * 1024)
            .http2_max_header_list_size(64 * 1024)
            .resolve_to_addrs(host, &[selected])
            .build()
            .map_err(network_error)
    }

    fn validate_response(
        &self,
        response: &reqwest::Response,
        selected: SocketAddr,
    ) -> Result<(u16, SocketAddr, Option<u64>), TransportError> {
        let connected = response.remote_addr().ok_or_else(|| {
            TransportError::new(
                TransportErrorKind::AddressMismatch,
                "connected peer evidence is absent",
            )
        })?;
        if connected != selected {
            return Err(TransportError::new(
                TransportErrorKind::AddressMismatch,
                "connected peer differs from admitted destination",
            ));
        }
        let status = response.status();
        if status.is_redirection() {
            return Err(TransportError::new(
                TransportErrorKind::RedirectDenied,
                "redirect requires a separately admitted request",
            ));
        }
        if status != reqwest::StatusCode::OK {
            return Err(TransportError::new(
                TransportErrorKind::ResponseStatus,
                format!("HTTP status {}", status.as_u16()),
            ));
        }
        let header_bytes = response
            .headers()
            .iter()
            .try_fold(0_usize, |total, (name, value)| {
                total
                    .checked_add(name.as_str().len())?
                    .checked_add(value.as_bytes().len())
            });
        if response.headers().len() > 128
            || header_bytes.is_none_or(|bytes| bytes > MAXIMUM_HEADER_BYTES)
        {
            return Err(TransportError::new(
                TransportErrorKind::SizeLimit,
                "response metadata exceeds its bound",
            ));
        }
        if response
            .headers()
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|value| value.as_bytes() != b"identity")
        {
            return Err(TransportError::new(
                TransportErrorKind::EncodedResponse,
                "encoded response is not admitted for direct acquisition",
            ));
        }
        let length = response.content_length();
        if length.is_some_and(|length| length > self.policy.max_bytes) {
            return Err(TransportError::new(
                TransportErrorKind::SizeLimit,
                "declared body exceeds its bound",
            ));
        }
        Ok((status.as_u16(), connected, length))
    }

    async fn copy_response<W: Write + ?Sized>(
        &self,
        mut response: reqwest::Response,
        sink: &mut W,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<u64, TransportError> {
        let mut received = 0_u64;
        loop {
            let Some(chunk) = controlled(response.chunk(), cancel, deadline)
                .await?
                .map_err(network_error)?
            else {
                return Ok(received);
            };
            if chunk.len() > MAXIMUM_BUFFERED_CHUNK {
                return Err(TransportError::new(
                    TransportErrorKind::SizeLimit,
                    "response chunk exceeds the byte-credit window",
                ));
            }
            let length = u64::try_from(chunk.len())
                .map_err(|_| TransportError::input("body length overflow"))?;
            let next = received
                .checked_add(length)
                .filter(|next| *next <= self.policy.max_bytes)
                .ok_or_else(|| {
                    TransportError::new(
                        TransportErrorKind::SizeLimit,
                        "body exceeds its admitted byte bound",
                    )
                })?;
            for part in chunk.chunks(WRITE_CHUNK) {
                let mut remaining = part;
                while !remaining.is_empty() {
                    check_control(cancel, deadline)?;
                    match sink.write(remaining) {
                        Ok(0) => {
                            return Err(TransportError::new(
                                TransportErrorKind::Sink,
                                "sink made no progress",
                            ));
                        }
                        Ok(written) if written <= remaining.len() => {
                            remaining = &remaining[written..]
                        }
                        Ok(_) => {
                            return Err(TransportError::new(
                                TransportErrorKind::Sink,
                                "sink returned an invalid byte count",
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => {
                            return Err(TransportError::new(
                                TransportErrorKind::Sink,
                                "sink write failed",
                            ));
                        }
                    }
                }
            }
            received = next;
        }
    }
}

fn check_control(cancel: &AtomicBool, deadline: Instant) -> Result<(), TransportError> {
    if cancel.load(Ordering::Acquire) {
        return Err(TransportError::new(
            TransportErrorKind::Cancelled,
            "transfer cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(TransportError::new(
            TransportErrorKind::TimedOut,
            "transfer deadline exhausted",
        ));
    }
    Ok(())
}

async fn controlled<F: Future>(
    future: F,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<F::Output, TransportError> {
    let mut future = std::pin::pin!(future);
    loop {
        check_control(cancel, deadline)?;
        let poll = deadline
            .saturating_duration_since(Instant::now())
            .min(CANCEL_POLL);
        if let Ok(result) = tokio::time::timeout(poll, future.as_mut()).await {
            check_control(cancel, deadline)?;
            return Ok(result);
        }
    }
}

fn network_error(error: reqwest::Error) -> TransportError {
    let kind = if error.is_timeout() {
        TransportErrorKind::TimedOut
    } else {
        TransportErrorKind::Network
    };
    let detail = if error.is_timeout() {
        "ordinary HTTP operation timed out"
    } else if error.is_connect() {
        "ordinary HTTP connection failed"
    } else if error.is_body() {
        "ordinary HTTP response stream failed"
    } else if error.is_builder() {
        "ordinary HTTP client configuration failed"
    } else {
        "ordinary HTTP operation failed"
    };
    TransportError::new(kind, detail)
}
