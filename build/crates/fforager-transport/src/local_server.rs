use crate::policy::{ByteCredits, TransportError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_METADATA_BYTES: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
static HARNESS_NONCE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct LocalProtocolServer {
    target: LocalHarnessTarget,
    request_rx: Receiver<CapturedRequest>,
    stop_tx: Sender<()>,
    worker: Option<JoinHandle<Result<(), String>>>,
}

#[derive(Clone)]
pub struct LocalHarnessTarget {
    address: SocketAddr,
    authorization: String,
    fragment_rx: Arc<Mutex<Receiver<usize>>>,
    fragment_ack_tx: Sender<()>,
}

impl std::fmt::Debug for LocalHarnessTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalHarnessTarget")
            .field("address", &self.address)
            .field("authorization", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl LocalHarnessTarget {
    #[cfg(test)]
    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}:{}{path}", self.address.ip(), self.address.port())
    }

    #[cfg(test)]
    pub(crate) fn authorization(&self) -> &str {
        &self.authorization
    }

    #[cfg(test)]
    pub(crate) fn acknowledge_fragment(&self) -> Result<(), TransportError> {
        let receiver = self.fragment_rx.lock().map_err(|_| {
            TransportError::Protocol("FF-TRANSPORT-E-HARNESS-FRAGMENT-LOCK".to_owned())
        })?;
        receiver
            .recv_timeout(IO_TIMEOUT)
            .map_err(|error| {
                TransportError::Protocol(format!(
                    "FF-TRANSPORT-E-HARNESS-FRAGMENT-RECEIVE: {error}"
                ))
            })
            .and_then(|_| {
                self.fragment_ack_tx.send(()).map_err(|error| {
                    TransportError::Protocol(format!(
                        "FF-TRANSPORT-E-HARNESS-FRAGMENT-ACK: {error}"
                    ))
                })
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedRequest {
    pub request_line: String,
    pub headers: BTreeMap<String, String>,
    pub normalized_wire_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub connection_identity: String,
    pub wire_identity: String,
    pub response_sha256: String,
    pub body_read_operations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationProbe {
    pub partial_body_bytes: u64,
    pub socket_shutdown: bool,
    pub worker_reaped: bool,
}

impl LocalProtocolServer {
    /// Starts one authenticated, one-request loopback proof server.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the listener, identity, or worker setup fails.
    pub fn spawn() -> Result<Self, TransportError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-BIND: {error}")))?;
        let address = listener
            .local_addr()
            .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-ADDR: {error}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-MODE: {error}")))?;
        let authorization = harness_nonce(address);
        let expected_authorization = authorization.clone();
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel();
        let (fragment_tx, fragment_rx) = mpsc::sync_channel(1);
        let (fragment_ack_tx, fragment_ack_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + IO_TIMEOUT;
            let (mut stream, request) = loop {
                match stop_rx.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => {
                        return Ok(());
                    }
                    Err(TryRecvError::Empty) => {}
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).map_err(|error| {
                            format!("FF-TRANSPORT-E-HARNESS-CLIENT-MODE: {error}")
                        })?;
                        stream
                            .set_read_timeout(Some(IO_TIMEOUT))
                            .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-TIMEOUT: {error}"))?;
                        stream
                            .set_write_timeout(Some(IO_TIMEOUT))
                            .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-TIMEOUT: {error}"))?;
                        let request = read_request(&mut stream)?;
                        if request.headers.get("x-ff-harness-authorization")
                            == Some(&expected_authorization)
                        {
                            break (stream, request);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err("FF-TRANSPORT-E-HARNESS-ACCEPT-TIMEOUT".to_owned());
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => {
                        return Err(format!("FF-TRANSPORT-E-HARNESS-ACCEPT: {error}"));
                    }
                }
            };
            let response = response_for(&request)?;
            request_tx
                .send(request)
                .map_err(|_| "FF-TRANSPORT-E-HARNESS-RECEIPT".to_owned())?;
            stream
                .write_all(&response.head)
                .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-WRITE: {error}"))?;
            for chunk in response.body_chunks {
                stream
                    .write_all(&chunk)
                    .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-WRITE: {error}"))?;
                fragment_tx
                    .send(chunk.len())
                    .map_err(|_| "FF-TRANSPORT-E-HARNESS-FRAGMENT-RECEIPT".to_owned())?;
                fragment_ack_rx
                    .recv_timeout(IO_TIMEOUT)
                    .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-FRAGMENT-ACK: {error}"))?;
            }
            wait_for_peer_close(&mut stream, response.wait_for_peer_close)?;
            Ok(())
        });
        Ok(Self {
            target: LocalHarnessTarget {
                address,
                authorization,
                fragment_rx: Arc::new(Mutex::new(fragment_rx)),
                fragment_ack_tx,
            },
            request_rx,
            stop_tx,
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn target(&self) -> LocalHarnessTarget {
        self.target.clone()
    }

    /// Waits for the captured request and joins the server worker.
    ///
    /// # Errors
    ///
    /// Returns a transport error for a missing receipt, worker panic, timeout, or
    /// protocol failure.
    pub fn finish(mut self) -> Result<CapturedRequest, TransportError> {
        let request = self.request_rx.recv_timeout(IO_TIMEOUT).map_err(|error| {
            TransportError::Protocol(format!("FF-TRANSPORT-E-HARNESS-RECEIPT: {error}"))
        })?;
        self.worker
            .take()
            .ok_or_else(|| TransportError::Protocol("FF-TRANSPORT-E-HARNESS-JOIN".to_owned()))?
            .join()
            .map_err(|_| TransportError::Protocol("FF-TRANSPORT-E-HARNESS-PANIC".to_owned()))?
            .map_err(TransportError::Protocol)?;
        Ok(request)
    }
}

impl Drop for LocalProtocolServer {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Executes one bounded HTTP/1.1 request through a private local harness target.
///
/// # Errors
///
/// Returns a transport error for an invalid target path or range, I/O failure,
/// malformed response, missing byte credit, or body bound violation.
pub fn execute_local(
    target: &LocalHarnessTarget,
    path_and_query: &str,
    range: Option<&str>,
    credits: &mut ByteCredits,
) -> Result<LocalResponse, TransportError> {
    if !path_and_query.starts_with('/')
        || path_and_query.len() > 4 * 1024
        || path_and_query.chars().any(char::is_control)
        || path_and_query
            .bytes()
            .any(|byte| byte == b' ' || byte == b'\\')
    {
        return Err(TransportError::Protocol(
            "FF-TRANSPORT-E-HARNESS-PATH".to_owned(),
        ));
    }
    let mut stream = TcpStream::connect_timeout(&target.address, IO_TIMEOUT)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-CONNECT: {error}")))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-TIMEOUT: {error}")))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-TIMEOUT: {error}")))?;
    let mut request = format!(
        "GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nX-FF-Harness-Authorization: {}\r\n",
        target.address.port(),
        target.authorization
    );
    if let Some(range) = range {
        if range.chars().any(char::is_control) || range.len() > 128 {
            return Err(TransportError::InvalidHeader(
                "FF-TRANSPORT-E-HARNESS-RANGE".to_owned(),
            ));
        }
        write!(request, "Range: {range}\r\n")
            .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-FORMAT: {error}")))?;
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-HARNESS-WRITE: {error}")))?;
    read_response(&mut stream, credits, target)
}

/// Cancels a genuinely in-flight partial HTTP response and reaps its worker.
///
/// # Errors
///
/// Returns a transport error for listener, socket, synchronization, protocol,
/// shutdown, or worker-join failure.
pub fn run_cancellation_probe() -> Result<CancellationProbe, TransportError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-BIND: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-ADDR: {error}")))?;
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || -> Result<bool, String> {
        let (mut stream, _) = listener
            .accept()
            .map_err(|error| format!("FF-TRANSPORT-E-CANCEL-ACCEPT: {error}"))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| format!("FF-TRANSPORT-E-CANCEL-TIMEOUT: {error}"))?;
        read_request(&mut stream)?;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial",
            )
            .map_err(|error| format!("FF-TRANSPORT-E-CANCEL-WRITE: {error}"))?;
        ready_tx
            .send(())
            .map_err(|_| "FF-TRANSPORT-E-CANCEL-READY".to_owned())?;
        let mut byte = [0_u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => Ok(true),
            Ok(_) => Ok(false),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                Ok(true)
            }
            Err(error) => Err(format!("FF-TRANSPORT-E-CANCEL-READ: {error}")),
        }
    });
    let mut client = TcpStream::connect_timeout(&address, IO_TIMEOUT)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-CONNECT: {error}")))?;
    client
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-TIMEOUT: {error}")))?;
    client
        .write_all(b"GET /stall HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-WRITE: {error}")))?;
    ready_rx.recv_timeout(IO_TIMEOUT).map_err(|error| {
        TransportError::Cancellation(format!("FF-TRANSPORT-E-CANCEL-READY: {error}"))
    })?;
    let metadata = read_until_header_end(&mut client, MAX_RESPONSE_METADATA_BYTES)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-HEADER: {error}")))?;
    if !metadata.starts_with(b"HTTP/1.1 200 ") {
        return Err(TransportError::Protocol(
            "FF-TRANSPORT-E-CANCEL-STATUS".to_owned(),
        ));
    }
    let mut partial = [0_u8; 7];
    client
        .read_exact(&mut partial)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-PARTIAL: {error}")))?;
    client
        .shutdown(Shutdown::Both)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-CANCEL-SHUTDOWN: {error}")))?;
    let socket_shutdown = worker
        .join()
        .map_err(|_| TransportError::Cancellation("FF-TRANSPORT-E-CANCEL-PANIC".to_owned()))?
        .map_err(TransportError::Cancellation)?;
    Ok(CancellationProbe {
        partial_body_bytes: 7,
        socket_shutdown,
        worker_reaped: true,
    })
}

fn read_request(stream: &mut TcpStream) -> Result<CapturedRequest, String> {
    let bytes = read_until_header_end(stream, MAX_REQUEST_BYTES)
        .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-READ: {error}"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-UTF8: {error}"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| "FF-TRANSPORT-E-HARNESS-REQUEST-LINE".to_owned())?
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "FF-TRANSPORT-E-HARNESS-HEADER".to_owned())?;
        let normalized = name.trim().to_ascii_lowercase();
        if headers
            .insert(normalized, value.trim().to_owned())
            .is_some()
        {
            return Err("FF-TRANSPORT-E-HARNESS-DUPLICATE-HEADER".to_owned());
        }
    }
    let mut normalized_wire = format!("{request_line}\n");
    for (name, value) in &headers {
        let normalized_value = match name.as_str() {
            "x-ff-harness-authorization" => "{{HARNESS_AUTHORIZATION}}".to_owned(),
            "host" if value.starts_with("127.0.0.1:") => "127.0.0.1:{{PORT}}".to_owned(),
            _ => value.clone(),
        };
        writeln!(normalized_wire, "{name}:{normalized_value}")
            .map_err(|error| format!("FF-TRANSPORT-E-HARNESS-WIRE-FORMAT: {error}"))?;
    }
    Ok(CapturedRequest {
        request_line,
        headers,
        normalized_wire_sha256: encode_hex(&Sha256::digest(normalized_wire.as_bytes())),
    })
}

struct ResponsePlan {
    head: Vec<u8>,
    body_chunks: Vec<Vec<u8>>,
    wait_for_peer_close: bool,
}

fn wait_for_peer_close(stream: &mut TcpStream, required: bool) -> Result<(), String> {
    if !required {
        return Ok(());
    }
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(())
        }
        Ok(_) => Err("FF-TRANSPORT-E-HARNESS-UNEXPECTED-BODY".to_owned()),
        Err(error) => Err(format!("FF-TRANSPORT-E-HARNESS-PEER-CLOSE: {error}")),
    }
}

fn response_for(request: &CapturedRequest) -> Result<ResponsePlan, String> {
    let path = request_target(request)?;
    let (status, reason, headers, body_chunks, wait_for_peer_close) =
        match path.split('?').next().unwrap_or(path) {
            "/ok" => (
                200,
                "OK",
                Vec::new(),
                vec![b"ferric-transport".to_vec()],
                false,
            ),
            "/range" => {
                if request.headers.get("range").map(String::as_str) != Some("bytes=2-5") {
                    return Err("FF-TRANSPORT-E-HARNESS-RANGE-EXPECTATION".to_owned());
                }
                (
                    206,
                    "Partial Content",
                    vec![("Content-Range", "bytes 2-5/10")],
                    vec![b"2345".to_vec()],
                    false,
                )
            }
            "/range-boundary" => {
                if request.headers.get("range").map(String::as_str) != Some("bytes=9-9") {
                    return Err("FF-TRANSPORT-E-HARNESS-RANGE-EXPECTATION".to_owned());
                }
                (
                    206,
                    "Partial Content",
                    vec![("Content-Range", "bytes 9-9/10")],
                    vec![b"9".to_vec()],
                    false,
                )
            }
            "/range-invalid" => (
                416,
                "Range Not Satisfiable",
                vec![("Content-Range", "bytes */10")],
                Vec::new(),
                false,
            ),
            "/stream" => (
                200,
                "OK",
                Vec::new(),
                vec![
                    b"stream-".to_vec(),
                    b"one-".to_vec(),
                    b"stream-".to_vec(),
                    b"two".to_vec(),
                ],
                false,
            ),
            "/stream-small" => (200, "OK", Vec::new(), vec![b"stream".to_vec()], false),
            "/huge-length" => (
                200,
                "OK",
                vec![("X-FF-Override-Length", "18446744073709551615")],
                Vec::new(),
                true,
            ),
            "/declared-oversize" => (
                200,
                "OK",
                vec![("X-FF-Override-Length", "1073741824")],
                Vec::new(),
                true,
            ),
            _ => (
                404,
                "Not Found",
                Vec::new(),
                vec![b"not-found".to_vec()],
                false,
            ),
        };
    let body_len = body_chunks.iter().map(Vec::len).sum::<usize>();
    let override_length = headers
        .iter()
        .find(|(name, _)| *name == "X-FF-Override-Length")
        .map(|(_, value)| *value);
    let content_length_text = override_length.map_or_else(|| body_len.to_string(), str::to_owned);
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {content_length_text}\r\nConnection: close\r\n"
    )
    .into_bytes();
    for (name, value) in headers {
        if name != "X-FF-Override-Length" {
            head.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
    }
    head.extend_from_slice(b"\r\n");
    Ok(ResponsePlan {
        head,
        body_chunks,
        wait_for_peer_close,
    })
}

fn request_target(request: &CapturedRequest) -> Result<&str, String> {
    request
        .request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "FF-TRANSPORT-E-HARNESS-TARGET".to_owned())
}

fn read_response(
    stream: &mut TcpStream,
    credits: &mut ByteCredits,
    target: &LocalHarnessTarget,
) -> Result<LocalResponse, TransportError> {
    let metadata = read_until_header_end(stream, MAX_RESPONSE_METADATA_BYTES)
        .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-RESPONSE-READ: {error}")))?;
    let metadata_text = std::str::from_utf8(&metadata).map_err(|error| {
        TransportError::Protocol(format!("FF-TRANSPORT-E-RESPONSE-UTF8: {error}"))
    })?;
    let mut lines = metadata_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| TransportError::Protocol("FF-TRANSPORT-E-STATUS-LINE".to_owned()))?;
    if !status_line.starts_with("HTTP/1.1 ") {
        return Err(TransportError::Protocol(
            "FF-TRANSPORT-E-HTTP-VERSION".to_owned(),
        ));
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| TransportError::Protocol("FF-TRANSPORT-E-STATUS".to_owned()))?
        .parse::<u16>()
        .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-STATUS: {error}")))?;
    let mut headers = BTreeMap::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| TransportError::Protocol("FF-TRANSPORT-E-RESPONSE-HEADER".to_owned()))?;
        if headers
            .insert(name.trim().to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err(TransportError::Protocol(
                "FF-TRANSPORT-E-DUPLICATE-RESPONSE-HEADER".to_owned(),
            ));
        }
    }
    let content_length = headers
        .get("content-length")
        .ok_or_else(|| TransportError::Protocol("FF-TRANSPORT-E-CONTENT-LENGTH".to_owned()))?
        .parse::<usize>()
        .map_err(|error| {
            TransportError::Protocol(format!("FF-TRANSPORT-E-CONTENT-LENGTH: {error}"))
        })?;
    let content_length_u64 = u64::try_from(content_length)
        .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-SIZE: {error}")))?;
    credits.preflight(content_length_u64)?;
    let mut body = Vec::new();
    let mut read_operations = 0_u64;
    while body.len() < content_length {
        let fragment_len = target
            .fragment_rx
            .lock()
            .map_err(|_| {
                TransportError::Protocol("FF-TRANSPORT-E-HARNESS-FRAGMENT-LOCK".to_owned())
            })?
            .recv_timeout(IO_TIMEOUT)
            .map_err(|error| {
                TransportError::Protocol(format!(
                    "FF-TRANSPORT-E-HARNESS-FRAGMENT-RECEIPT: {error}"
                ))
            })?;
        if fragment_len == 0 || body.len().saturating_add(fragment_len) > content_length {
            return Err(TransportError::Protocol(
                "FF-TRANSPORT-E-RESPONSE-FRAGMENT-LENGTH".to_owned(),
            ));
        }
        let accepted = u64::try_from(fragment_len)
            .map_err(|error| TransportError::Protocol(format!("FF-TRANSPORT-E-SIZE: {error}")))?;
        credits.accept(accepted)?;
        let mut chunk = vec![0_u8; fragment_len];
        stream
            .read_exact(&mut chunk)
            .map_err(|error| TransportError::Io(format!("FF-TRANSPORT-E-BODY-READ: {error}")))?;
        body.extend_from_slice(&chunk);
        read_operations = read_operations.saturating_add(1);
        target.fragment_ack_tx.send(()).map_err(|_| {
            TransportError::Protocol("FF-TRANSPORT-E-HARNESS-FRAGMENT-ACK".to_owned())
        })?;
    }
    let mut response_bytes = metadata;
    response_bytes.extend_from_slice(&body);
    Ok(LocalResponse {
        status,
        headers,
        body,
        connection_identity: "tcp-loopback-harness-v1".to_owned(),
        wire_identity: "http/1.1-std-tcp-v1".to_owned(),
        response_sha256: encode_hex(&Sha256::digest(response_bytes)),
        body_read_operations: read_operations,
    })
}

fn harness_nonce(address: SocketAddr) -> String {
    let sequence = HARNESS_NONCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let material = format!("{}:{address}:{sequence}:{timestamp}", std::process::id());
    encode_hex(&Sha256::digest(material.as_bytes()))
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

fn read_until_header_end(stream: &mut TcpStream, maximum: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut one = [0_u8; 1];
    while bytes.len() < maximum {
        let read = stream.read(&mut one)?;
        if read == 0 {
            break;
        }
        bytes.push(one[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "header terminator missing or bound exceeded",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn real_http_range_evidence_crosses_loopback_socket() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let mut credits = ByteCredits::new(4);
        credits.grant(4).expect("credits");
        let response = execute_local(&server.target(), "/range", Some("bytes=2-5"), &mut credits)
            .expect("local response");
        let request = server.finish().expect("server receipt");
        assert_eq!(response.status, 206);
        assert_eq!(response.body, b"2345");
        assert_eq!(credits.accepted(), 4);
        assert_eq!(
            request.headers.get("range").map(String::as_str),
            Some("bytes=2-5")
        );
    }

    #[test]
    fn declared_huge_body_is_rejected_before_allocation() {
        let server = LocalProtocolServer::spawn().expect("local server");
        let mut credits = ByteCredits::new(1024);
        credits.grant(1024).expect("credits");
        let error = execute_local(&server.target(), "/huge-length", None, &mut credits)
            .expect_err("huge declaration must fail");
        server.finish().expect("server reaped");
        assert!(matches!(
            error,
            TransportError::Bound {
                kind: "byte_credit",
                ..
            }
        ));
        assert_eq!(credits.accepted(), 0);
    }

    #[test]
    fn preconnect_rejection_reaps_server_worker() {
        let started = Instant::now();
        let server = LocalProtocolServer::spawn().expect("local server");
        let mut credits = ByteCredits::new(1);
        let error = execute_local(&server.target(), "/invalid path", None, &mut credits)
            .expect_err("invalid path");
        assert!(matches!(error, TransportError::Protocol(_)));
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
