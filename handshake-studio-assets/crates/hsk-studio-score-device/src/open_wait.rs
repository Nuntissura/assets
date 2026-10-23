//! Cancel-aware wait for a backend's readiness message. Native backends open their stream on an
//! owner thread and report readiness over a bounded channel; this is the control-side wait. It
//! polls the cancellation token every `slice`, so a cancel is observed within one slice instead
//! of after the whole open limit.
use crate::error::DeviceError;
use hsk_studio_accord::CancellationToken;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Waits for the readiness message of an opening backend.
///
/// - the message arrives: its result is returned (a ready backend wins over a late cancel);
/// - `cancel` is set first: `Canceled` (the caller drops the backend, which stops and joins it);
/// - `limit` passes or the sender is gone: `Unavailable`.
pub(crate) fn wait_ready(
    ready: &Receiver<Result<(), DeviceError>>,
    cancel: &CancellationToken,
    limit: Duration,
    slice: Duration,
) -> Result<(), DeviceError> {
    let deadline = Instant::now().checked_add(limit);
    loop {
        match ready.recv_timeout(slice) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Disconnected) => return Err(DeviceError::Unavailable),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if cancel.check().is_err() {
            return Err(DeviceError::Canceled);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(DeviceError::Unavailable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const SLICE: Duration = Duration::from_millis(5);

    #[test]
    fn ready_result_is_returned() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Ok(())).unwrap();
        let cancel = CancellationToken::default();
        assert_eq!(wait_ready(&rx, &cancel, Duration::from_secs(30), SLICE), Ok(()));
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(Err(DeviceError::Unavailable)).unwrap();
        assert_eq!(
            wait_ready(&rx, &cancel, Duration::from_secs(30), SLICE),
            Err(DeviceError::Unavailable)
        );
    }

    #[test]
    fn cancel_is_observed_within_slices_not_the_open_limit() {
        // The sender stays alive and silent (a stream build that hangs).
        let (_tx, rx) = mpsc::sync_channel(1);
        let cancel = CancellationToken::default();
        let remote = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            remote.cancel();
        });
        let started = Instant::now();
        let result = wait_ready(&rx, &cancel, Duration::from_secs(60), SLICE);
        canceller.join().unwrap();
        assert_eq!(result, Err(DeviceError::Canceled));
        assert!(started.elapsed() < Duration::from_secs(10), "cancel waited for the limit");
    }

    #[test]
    fn limit_and_gone_sender_are_unavailable() {
        let cancel = CancellationToken::default();
        let (_tx, rx) = mpsc::sync_channel::<Result<(), DeviceError>>(1);
        assert_eq!(
            wait_ready(&rx, &cancel, Duration::from_millis(40), SLICE),
            Err(DeviceError::Unavailable)
        );
        // A limit too large to represent never panics and still honours cancel.
        cancel.cancel();
        assert_eq!(wait_ready(&rx, &cancel, Duration::MAX, SLICE), Err(DeviceError::Canceled));
        let (tx, rx) = mpsc::sync_channel::<Result<(), DeviceError>>(1);
        drop(tx);
        assert_eq!(
            wait_ready(&rx, &CancellationToken::default(), Duration::from_secs(30), SLICE),
            Err(DeviceError::Unavailable)
        );
    }
}
