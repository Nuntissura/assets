//! Production composition of the single fetch engine and its adapters.
#![forbid(unsafe_code)]

use fforager_core::engine::{FetchEngine, FetchTransport, TransferReceipt};
pub use fforager_core::engine::{FetchError, FetchRequest, FetchResult};
use fforager_net::{OrdinaryTransport, TransportError, TransportErrorKind, TransportPolicy};
use fforager_storage::DurableFetchStorage;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct NetPort<'a> {
    runtime: &'a tokio::runtime::Runtime,
    transport: OrdinaryTransport,
}

impl FetchTransport for NetPort<'_> {
    fn download(
        &mut self,
        request: &FetchRequest,
        sink: &mut dyn Write,
        cancel: &AtomicBool,
    ) -> Result<TransferReceipt, FetchError> {
        self.runtime.block_on(async {
            let addresses = self
                .transport
                .resolve_addresses(&request.url, cancel)
                .await
                .map_err(map_transport_error)?;
            let receipt = self
                .transport
                .download(&request.url, &addresses, sink, cancel)
                .await
                .map_err(map_transport_error)?;
            Ok(TransferReceipt {
                bytes: receipt.bytes_received,
            })
        })
    }
}

/// Runs one acquisition through the shared engine and concrete production adapters.
///
/// # Errors
/// Returns the typed request, cancellation, transport, storage, or byte-limit failure.
pub fn fetch(request: &FetchRequest, cancel: &AtomicBool) -> Result<FetchResult, FetchError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| FetchError::Transport(format!("runtime initialization: {error}")))?;
    let transport = OrdinaryTransport::new(TransportPolicy {
        max_bytes: request.max_bytes,
        total_timeout: Duration::from_millis(request.timeout_ms),
        allow_loopback: request.allowed_loopback,
        ..TransportPolicy::default()
    })
    .map_err(map_transport_error)?;
    let mut engine = FetchEngine::new(
        NetPort {
            runtime: &runtime,
            transport,
        },
        DurableFetchStorage::new(),
    );
    let result = engine.fetch(request, cancel);
    drop(engine);
    runtime.shutdown_timeout(Duration::from_millis(250));
    result
}
fn map_transport_error(error: TransportError) -> FetchError {
    match error.kind {
        TransportErrorKind::Cancelled => FetchError::Cancelled,
        TransportErrorKind::SizeLimit => FetchError::LimitExceeded,
        TransportErrorKind::InvalidInput => FetchError::InvalidRequest(error.to_string()),
        TransportErrorKind::Sink => FetchError::Storage(error.to_string()),
        _ => FetchError::Transport(error.to_string()),
    }
}
