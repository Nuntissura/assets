//! Fixed-buffer pipe drains with an explicit diagnostic-tail loss policy.

use crate::progress::{ProgressError, ProgressLimits, ProgressParser, ProgressSummary};
use std::{
    collections::VecDeque,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    time::{Duration, Instant},
};

/// Pipe byte and tail ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundedDrainLimits {
    pub max_total_bytes: u64,
    pub tail_bytes: usize,
}

/// Diagnostic bytes may retain only a tail after total input exceeds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticLoss {
    None,
    PrefixTruncated { dropped_bytes: u64 },
}

/// Complete diagnostic drain result; the pipe is always read to EOF.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedDrainResult {
    pub total_bytes: u64,
    pub tail: Vec<u8>,
    pub loss: DiagnosticLoss,
    pub total_limit_exceeded: bool,
}

impl BoundedDrainResult {
    /// Return an independently replayable exact transcript only when the
    /// retained diagnostic bytes are the complete stream.
    #[must_use]
    pub fn complete_transcript(&self) -> Option<&[u8]> {
        (self.loss == DiagnosticLoss::None
            && self.total_bytes == u64::try_from(self.tail.len()).unwrap_or(u64::MAX))
        .then_some(self.tail.as_slice())
    }
}

/// Drain human diagnostics to EOF with a fixed read buffer and bounded tail.
///
/// # Errors
///
/// Returns only an underlying pipe read error. Exceeding the declared total is
/// recorded while draining continues so cancellation and reap cannot deadlock.
pub fn drain_diagnostics(
    mut reader: impl Read,
    limits: BoundedDrainLimits,
) -> Result<BoundedDrainResult, std::io::Error> {
    let mut buffer = [0_u8; 8 * 1024];
    let mut tail = VecDeque::with_capacity(limits.tail_bytes.min(64 * 1024));
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        for byte in &buffer[..count] {
            if tail.len() == limits.tail_bytes && limits.tail_bytes != 0 {
                tail.pop_front();
            }
            if limits.tail_bytes != 0 {
                tail.push_back(*byte);
            }
        }
    }
    let retained = u64::try_from(tail.len()).unwrap_or(u64::MAX);
    let dropped = total.saturating_sub(retained);
    Ok(BoundedDrainResult {
        total_bytes: total,
        tail: tail.into_iter().collect(),
        loss: if dropped == 0 {
            DiagnosticLoss::None
        } else {
            DiagnosticLoss::PrefixTruncated {
                dropped_bytes: dropped,
            }
        },
        total_limit_exceeded: total > limits.max_total_bytes,
    })
}

/// Runtime bounds which cannot be enforced by the grammar parser alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgressRuntimeLimits {
    /// Maximum storage owned by parser state, the reader, and its one-item channel.
    pub allocation_bytes: usize,
    /// Maximum silence between pipe chunks, including the first chunk.
    pub cadence_timeout: Duration,
    /// Maximum time the reader may be unable to hand one chunk to the parser.
    pub consumer_stall_timeout: Duration,
}

/// Drain progress through a one-item bounded channel so cadence, consumer
/// stall, and pipe-side allocation are independently bounded.
///
/// The reader continues until EOF or until the parser receiver is dropped. A
/// caller must terminate/reap the child before joining this function after a
/// cadence error, because an operating-system pipe read may remain blocked
/// until the child closes its handle.
///
/// # Errors
///
/// Returns the first I/O, grammar, cadence, stall, allocation, or worker error.
pub fn drain_progress_supervised(
    mut reader: impl Read + Send + 'static,
    parser_limits: ProgressLimits,
    runtime_limits: ProgressRuntimeLimits,
) -> Result<ProgressSummary, ProgressDrainError> {
    let parser_bytes = usize::try_from(parser_limits.max_record_bytes)
        .ok()
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(ProgressDrainError::InvalidRuntimeLimits)?;
    let available_bytes = runtime_limits
        .allocation_bytes
        .checked_sub(parser_bytes)
        .ok_or(ProgressDrainError::InvalidRuntimeLimits)?;
    // Split the governed stdout credit between four simultaneously live pipe
    // chunks and the correctness-complete replay transcript. Half is reserved
    // for evidence until the fixed 8 KiB chunk ceiling is reached.
    let chunk_bytes = (available_bytes / 8).min(8 * 1024);
    let chunk_allocation = chunk_bytes
        .checked_mul(4)
        .ok_or(ProgressDrainError::InvalidRuntimeLimits)?;
    let transcript_capacity = available_bytes
        .checked_sub(chunk_allocation)
        .ok_or(ProgressDrainError::InvalidRuntimeLimits)?
        .min(usize::try_from(parser_limits.max_total_bytes).unwrap_or(usize::MAX));
    if chunk_bytes == 0
        || transcript_capacity == 0
        || runtime_limits.cadence_timeout.is_zero()
        || runtime_limits.consumer_stall_timeout.is_zero()
    {
        return Err(ProgressDrainError::InvalidRuntimeLimits);
    }
    // At most four equal chunks coexist: the reader buffer, the value being
    // offered, the one queued value, and the parser-owned received value.
    // Parser state owns two exact max-record buffers and retains no records.
    let cadence_deadline = Instant::now()
        .checked_add(runtime_limits.cadence_timeout)
        .unwrap_or_else(Instant::now);
    let (sender, receiver) = sync_channel(1);
    let stalled = Arc::new(AtomicBool::new(false));
    let read_error = Arc::new(Mutex::new(None));
    let reader_stalled = Arc::clone(&stalled);
    let reader_error = Arc::clone(&read_error);
    let stall_timeout = runtime_limits.consumer_stall_timeout;
    let worker = std::thread::spawn(move || {
        let mut buffer = vec![0_u8; chunk_bytes];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _sent = send_with_stall(
                        &sender,
                        (Instant::now(), None),
                        stall_timeout,
                        &reader_stalled,
                    );
                    return;
                }
                Ok(count) => {
                    if send_with_stall(
                        &sender,
                        (Instant::now(), Some(buffer[..count].to_vec())),
                        stall_timeout,
                        &reader_stalled,
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    if let Ok(mut slot) = reader_error.lock() {
                        *slot = Some(error);
                    }
                    return;
                }
            }
        }
    });

    let parsed = receive_progress(
        &receiver,
        parser_limits,
        runtime_limits.cadence_timeout,
        cadence_deadline,
        &stalled,
        &read_error,
        transcript_capacity,
    );
    drop(receiver);
    worker
        .join()
        .map_err(|_| ProgressDrainError::ReaderJoinFailed)?;
    parsed
}

fn send_with_stall<T>(
    sender: &SyncSender<T>,
    mut value: T,
    timeout: Duration,
    stalled: &AtomicBool,
) -> Result<(), ()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(()),
            Err(TrySendError::Full(returned)) => {
                value = returned;
                if Instant::now() >= deadline {
                    stalled.store(true, Ordering::Release);
                    return Err(());
                }
                std::thread::yield_now();
            }
        }
    }
}

fn receive_progress(
    receiver: &Receiver<(Instant, Option<Vec<u8>>)>,
    limits: ProgressLimits,
    cadence_timeout: Duration,
    mut cadence_deadline: Instant,
    stalled: &AtomicBool,
    read_error: &Mutex<Option<std::io::Error>>,
    transcript_capacity: usize,
) -> Result<ProgressSummary, ProgressDrainError> {
    let mut parser = ProgressParser::new(limits, transcript_capacity);
    loop {
        let remaining = cadence_deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((observed_at, _)) if observed_at > cadence_deadline => {
                return Err(ProgressDrainError::CadenceTimedOut);
            }
            Ok((observed_at, Some(bytes))) => {
                parser.feed(&bytes).map_err(ProgressDrainError::Progress)?;
                cadence_deadline = observed_at
                    .checked_add(cadence_timeout)
                    .unwrap_or(observed_at);
            }
            Ok((_, None)) => return parser.finish().map_err(ProgressDrainError::Progress),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(ProgressDrainError::CadenceTimedOut);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if stalled.load(Ordering::Acquire) {
                    return Err(ProgressDrainError::ConsumerStalled);
                }
                let error = read_error
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take())
                    .unwrap_or_else(|| std::io::Error::other("progress reader disconnected"));
                return Err(ProgressDrainError::Io(error));
            }
        }
    }
}

/// Required-progress drain failure.
#[derive(Debug)]
pub enum ProgressDrainError {
    Io(std::io::Error),
    Progress(ProgressError),
    InvalidRuntimeLimits,
    CadenceTimedOut,
    ConsumerStalled,
    ReaderJoinFailed,
}

/// Drain correctness-critical bytes to EOF without truncation.
///
/// # Errors
///
/// Returns an I/O error or `InvalidData` as soon as the byte ceiling would be
/// exceeded. The reader is still consumed to EOF so the child cannot block on
/// a full pipe.
pub fn drain_required_bytes(
    mut reader: impl Read,
    maximum: u64,
) -> Result<Vec<u8>, std::io::Error> {
    let capacity = usize::try_from(maximum.min(64 * 1024)).unwrap_or(64 * 1024);
    let mut output = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let next = u64::try_from(output.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        if next <= maximum && !exceeded {
            output.extend_from_slice(&buffer[..count]);
        } else {
            exceeded = true;
        }
    }
    if exceeded {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "required pipe byte limit exceeded",
        ))
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    struct DelayedReader {
        delayed: bool,
    }

    impl Read for DelayedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.delayed {
                Ok(0)
            } else {
                self.delayed = true;
                std::thread::sleep(Duration::from_millis(20));
                let value = b"progress=end\n";
                buffer[..value.len()].copy_from_slice(value);
                Ok(value.len())
            }
        }
    }

    #[test]
    fn diagnostic_tail_is_bounded_and_loss_is_explicit() {
        let result = drain_diagnostics(
            Cursor::new(b"0123456789"),
            BoundedDrainLimits {
                max_total_bytes: 8,
                tail_bytes: 4,
            },
        )
        .expect("drain");
        assert_eq!(result.tail, b"6789");
        assert_eq!(
            result.loss,
            DiagnosticLoss::PrefixTruncated { dropped_bytes: 6 }
        );
        assert!(result.total_limit_exceeded);
    }

    fn progress_limits() -> ProgressLimits {
        ProgressLimits {
            max_records: 2,
            max_total_bytes: 128,
            max_record_bytes: 64,
            max_field_bytes: 32,
            max_parser_steps: 256,
        }
    }

    #[test]
    fn supervised_progress_uses_bounded_allocation_and_requires_terminal_frame() {
        let result = drain_progress_supervised(
            Cursor::new(b"frame=1\nprogress=end\n".to_vec()),
            progress_limits(),
            ProgressRuntimeLimits {
                allocation_bytes: 192,
                cadence_timeout: Duration::from_secs(1),
                consumer_stall_timeout: Duration::from_secs(1),
            },
        )
        .expect("bounded supervised progress");
        assert!(result.saw_terminal);
        assert_eq!(result.record_count, 1);
        assert_eq!(result.raw_transcript, b"frame=1\nprogress=end\n");
    }

    #[test]
    fn governed_progress_share_rejects_unretainable_correctness_transcript() {
        let result = drain_progress_supervised(
            Cursor::new(b"frame=12345678901234567890\nprogress=end\n".to_vec()),
            progress_limits(),
            ProgressRuntimeLimits {
                // Two 64-byte parser buffers plus four 8-byte chunks leave
                // exactly 32 bytes for the complete evidence transcript.
                allocation_bytes: 192,
                cadence_timeout: Duration::from_secs(1),
                consumer_stall_timeout: Duration::from_secs(1),
            },
        );
        assert!(
            matches!(
                result,
                Err(ProgressDrainError::Progress(
                    ProgressError::EvidenceBytesExceeded
                ))
            ),
            "expected governed transcript rejection, observed {result:?}"
        );
    }

    #[test]
    fn full_channel_trips_bounded_consumer_stall() {
        let (sender, _receiver) = sync_channel(1);
        sender.try_send(Some(vec![1])).expect("seed channel");
        let stalled = AtomicBool::new(false);
        assert_eq!(
            send_with_stall(&sender, Some(vec![2]), Duration::from_millis(1), &stalled,),
            Err(())
        );
        assert!(stalled.load(Ordering::Acquire));
    }

    #[test]
    fn zero_runtime_bound_is_rejected() {
        assert!(matches!(
            drain_progress_supervised(
                Cursor::new(Vec::<u8>::new()),
                progress_limits(),
                ProgressRuntimeLimits {
                    allocation_bytes: 1,
                    cadence_timeout: Duration::from_secs(1),
                    consumer_stall_timeout: Duration::from_secs(1),
                },
            ),
            Err(ProgressDrainError::InvalidRuntimeLimits)
        ));
    }

    #[test]
    fn first_progress_cadence_deadline_is_enforced() {
        assert!(matches!(
            drain_progress_supervised(
                DelayedReader { delayed: false },
                progress_limits(),
                ProgressRuntimeLimits {
                    allocation_bytes: 256,
                    cadence_timeout: Duration::from_millis(1),
                    consumer_stall_timeout: Duration::from_secs(1),
                },
            ),
            Err(ProgressDrainError::CadenceTimedOut)
        ));
    }

    #[test]
    fn queued_chunk_observed_after_cadence_deadline_is_rejected() {
        let (sender, receiver) = sync_channel(1);
        let cadence_deadline = Instant::now();
        sender
            .send((
                cadence_deadline + Duration::from_millis(1),
                Some(b"progress=end\n".to_vec()),
            ))
            .expect("queue stale chunk");
        assert!(matches!(
            receive_progress(
                &receiver,
                progress_limits(),
                Duration::from_secs(1),
                cadence_deadline,
                &AtomicBool::new(false),
                &Mutex::new(None),
                128,
            ),
            Err(ProgressDrainError::CadenceTimedOut)
        ));
    }

    #[test]
    fn timely_queued_events_survive_receiver_scheduling_delay() {
        let (sender, receiver) = sync_channel(2);
        let observed_at = Instant::now();
        let cadence_timeout = Duration::from_millis(20);
        sender
            .send((observed_at, Some(b"progress=end\n".to_vec())))
            .expect("queue timely chunk");
        sender
            .send((observed_at + Duration::from_millis(1), None))
            .expect("queue timely EOF");
        std::thread::sleep(Duration::from_millis(30));
        let summary = receive_progress(
            &receiver,
            progress_limits(),
            cadence_timeout,
            observed_at + cadence_timeout,
            &AtomicBool::new(false),
            &Mutex::new(None),
            128,
        )
        .expect("reader-observed cadence must survive receiver scheduling delay");
        assert!(summary.saw_terminal);
        assert_eq!(summary.raw_transcript, b"progress=end\n");
    }

    #[test]
    #[allow(
        clippy::format_collect,
        reason = "the bounded regression constructs a small deterministic multi-record fixture"
    )]
    fn many_records_are_counted_without_retaining_record_payloads() {
        let input = (0..32)
            .map(|index| {
                format!(
                    "frame={index}\nprogress={}\n",
                    if index == 31 { "end" } else { "continue" }
                )
            })
            .collect::<String>();
        let result = drain_progress_supervised(
            Cursor::new(input.into_bytes()),
            ProgressLimits {
                max_records: 32,
                max_total_bytes: 2_048,
                max_record_bytes: 32,
                max_field_bytes: 16,
                max_parser_steps: 4_096,
            },
            ProgressRuntimeLimits {
                allocation_bytes: 2_048,
                cadence_timeout: Duration::from_secs(1),
                consumer_stall_timeout: Duration::from_secs(1),
            },
        )
        .expect("fixed allocation progress");
        assert_eq!(result.record_count, 32);
        assert!(result.saw_terminal);
        assert_eq!(result.total_bytes, result.raw_transcript.len() as u64);
    }

    #[test]
    fn diagnostic_complete_transcript_is_available_only_without_loss() {
        let complete = drain_diagnostics(
            Cursor::new(b"exact diagnostic"),
            BoundedDrainLimits {
                max_total_bytes: 64,
                tail_bytes: 64,
            },
        )
        .expect("complete diagnostic");
        assert_eq!(
            complete.complete_transcript(),
            Some(b"exact diagnostic".as_slice())
        );

        let truncated = drain_diagnostics(
            Cursor::new(b"0123456789"),
            BoundedDrainLimits {
                max_total_bytes: 64,
                tail_bytes: 4,
            },
        )
        .expect("truncated diagnostic");
        assert_eq!(truncated.complete_transcript(), None);
    }
}
