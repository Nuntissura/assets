//! One worker-invocation admission using the accepted coupled resource ledger.

use crate::engine::FetchError;
use crate::resource::{
    OwnedAdmission, OwnedResourceBroker, OwnerId, ResourceContractV1, ResourceVector,
};

/// Reserve the complete one-job vector before any adapter effect and acknowledge
/// release before returning either the operation's result or a cleanup failure.
/// This is local invocation accounting, not an operating-system memory ceiling
/// or an aggregate limit across workers or embedded callers.
pub(crate) fn with_fetch_resources<R>(
    operation: impl FnOnce() -> Result<R, FetchError>,
) -> Result<R, FetchError> {
    // One direct request has one origin and no redirected/concurrent request.
    // One retained delivery frame (at most 1 MiB), 16-KiB borrowed sink writes,
    // headers and hash/read windows fit this logical payload reservation.
    // Dependency preallocation and other runtime allocations remain separate.
    let resources = ResourceVector {
        metadata_requests: 0,
        media_requests: 1,
        memory_bytes: 2 * 1024 * 1024,
        disk_read_bytes_in_flight: 64 * 1024,
        disk_write_bytes_in_flight: 64 * 1024,
        open_handles: 128,
        cpu_light_slots: 2,
        cpu_heavy_slots: 0,
        javascript_workers: 0,
        ffmpeg_processes: 0,
        ffmpeg_cpu_threads: 0,
        archive_writer_slots: 1,
        sink_bytes: 64 * 1024,
    };
    let contract = ResourceContractV1::new(resources, 1, 1, 0, 0);
    let broker = OwnedResourceBroker::from_contract(&contract)
        .map_err(|error| FetchError::Storage(format!("resource contract rejected: {error:?}")))?;
    let lease = match broker.request(OwnerId(1), resources).map_err(|error| {
        FetchError::Storage(format!("atomic resource admission rejected: {error:?}"))
    })? {
        OwnedAdmission::Granted(lease) => lease,
        OwnedAdmission::Queued(waiter) => {
            let cancelled = waiter.cancel();
            let verified = broker.verify();
            return Err(FetchError::Storage(format!(
                "queuing is disabled for a single fetch; cancellation={cancelled:?}; ledger={verified:?}"
            )));
        }
    };
    let result = operation();
    let released = lease.release();
    let verified = broker.verify();
    if let Err(error) = released {
        return Err(FetchError::Storage(format!(
            "resource release was not acknowledged: {error:?}"
        )));
    }
    if let Err(error) = verified {
        return Err(FetchError::Storage(format!(
            "resource ledger failed after release: {error:?}"
        )));
    }
    if broker.in_use() != ResourceVector::default()
        || broker.active_grant_count() != 0
        || broker.waiter_occupancy() != (0, 0)
        || !broker.ready_waiter_ids().is_empty()
    {
        return Err(FetchError::Storage(
            "resource ownership remains after fetch release".to_owned(),
        ));
    }
    result
}
