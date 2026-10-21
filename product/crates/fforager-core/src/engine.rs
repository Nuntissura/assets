//! Direct-media job orchestration over inward-facing transport and storage ports.

use std::fmt;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// One bounded direct-media acquisition requested by a composition root.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub url: String,
    pub output: PathBuf,
    pub state_dir: PathBuf,
    pub max_bytes: u64,
    pub timeout_ms: u64,
    pub cancel_file: Option<PathBuf>,
    /// An explicit single-endpoint permission; absent for ordinary public URLs.
    pub allowed_loopback: Option<SocketAddr>,
}

impl FetchRequest {
    /// Reject unusable requests before opening a file or admitting network work.
    pub fn validate(&self) -> Result<(), FetchError> {
        if self.url.is_empty() || self.url.len() > 16 * 1024 {
            return Err(FetchError::InvalidRequest(
                "URL length is outside its bound".into(),
            ));
        }
        if self.output.file_name().is_none() || self.state_dir.as_os_str().is_empty() {
            return Err(FetchError::InvalidRequest(
                "output and state directory are required".into(),
            ));
        }
        if self.max_bytes == 0
            || self.max_bytes > 1024_u64.pow(4)
            || self.timeout_ms == 0
            || self.timeout_ms > 3_600_000
        {
            return Err(FetchError::InvalidRequest(
                "byte or deadline limit is outside its bound".into(),
            ));
        }
        if self
            .allowed_loopback
            .is_some_and(|address| !address.ip().is_loopback() || address.port() == 0)
        {
            return Err(FetchError::InvalidRequest(
                "loopback permission must name one loopback socket".into(),
            ));
        }
        Ok(())
    }
}

/// Result is returned only after the storage adapter acknowledges durable commit.
#[derive(Clone, Debug)]
pub struct FetchResult {
    pub job_id: String,
    pub output: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub archive_sequence: u64,
    pub recovered: bool,
}

/// Transfer completion is distinct from durable output/archive completion.
#[derive(Clone, Copy, Debug)]
pub struct TransferReceipt {
    pub bytes: u64,
}

/// Stable failure category retained across concrete adapter boundaries.
#[derive(Debug)]
pub enum FetchError {
    InvalidRequest(String),
    Cancelled,
    Transport(String),
    Storage(String),
    LimitExceeded,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(detail) => write!(f, "invalid_request: {detail}"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Transport(detail) => write!(f, "transport: {detail}"),
            Self::Storage(detail) => write!(f, "storage: {detail}"),
            Self::LimitExceeded => f.write_str("byte_limit_exceeded"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Concrete networking stays outside the engine's dependency ring.
pub trait FetchTransport {
    fn download(
        &mut self,
        request: &FetchRequest,
        sink: &mut dyn Write,
        cancel: &AtomicBool,
    ) -> Result<TransferReceipt, FetchError>;
}

/// The adapter owns staging, recovery, hashing, output validation and archive commit.
pub trait FetchStorage {
    type Writer: Write;
    /// Reconcile an already durable prefix before any network acquisition.
    fn recover(&mut self, _request: &FetchRequest) -> Result<Option<FetchResult>, FetchError> {
        Ok(None)
    }
    fn prepare(&mut self, request: &FetchRequest) -> Result<Self::Writer, FetchError>;
    /// Actual SHA-256 locator identity shared by graph and archive ownership.
    fn source_identity(&self, writer: &Self::Writer) -> Result<String, FetchError>;
    fn commit(
        &mut self,
        request: &FetchRequest,
        writer: Self::Writer,
        receipt: TransferReceipt,
    ) -> Result<FetchResult, FetchError>;
    /// Preserve recoverable state while closing a failed or cancelled writer.
    fn abort(&mut self, writer: Self::Writer) -> Result<(), FetchError>;
}

/// A single orchestration path is shared by worker and embedded callers.
#[derive(Debug)]
pub struct FetchEngine<T, S> {
    transport: T,
    storage: S,
}

impl<T: FetchTransport, S: FetchStorage> FetchEngine<T, S> {
    pub const fn new(transport: T, storage: S) -> Self {
        Self { transport, storage }
    }

    pub fn fetch(
        &mut self,
        request: &FetchRequest,
        cancel: &AtomicBool,
    ) -> Result<FetchResult, FetchError> {
        crate::admission::with_fetch_resources(|| self.fetch_inner(request, cancel))
    }

    fn fetch_inner(
        &mut self,
        request: &FetchRequest,
        cancel: &AtomicBool,
    ) -> Result<FetchResult, FetchError> {
        request.validate()?;
        if cancel.load(Ordering::Acquire) {
            return Err(FetchError::Cancelled);
        }
        if let Some(recovered) = self.storage.recover(request)? {
            return Ok(recovered);
        }
        let mut staged = self.storage.prepare(request)?;
        let identity = self.storage.source_identity(&staged)?;
        let plan = crate::direct::plan_direct(&request.url, &identity)
            .map_err(|error| FetchError::InvalidRequest(error.to_string()))?;
        let mut selected = request.clone();
        selected.url = match plan.acquisition {
            fforager_contracts::AcquisitionSource::DirectUrl { url } => url,
            _ => {
                return Err(FetchError::InvalidRequest(
                    "direct acquisition plan required".into(),
                ));
            }
        };
        let transfer = self.transport.download(&selected, &mut staged, cancel);
        match transfer {
            Ok(receipt)
                if receipt.bytes > 0
                    && receipt.bytes <= request.max_bytes
                    && !cancel.load(Ordering::Acquire) =>
            {
                self.storage.commit(request, staged, receipt)
            }
            Ok(receipt) => {
                self.storage.abort(staged)?;
                if cancel.load(Ordering::Acquire) {
                    Err(FetchError::Cancelled)
                } else if receipt.bytes == 0 {
                    Err(FetchError::Transport("empty direct-media response".into()))
                } else {
                    Err(FetchError::LimitExceeded)
                }
            }
            Err(error) => {
                self.storage.abort(staged)?;
                Err(error)
            }
        }
    }
}
