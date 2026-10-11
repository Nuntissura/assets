//! Worker process side: hosts the in-process rav1d decoder of `hsk-studio-reel` (`av1::Av1Decoder`) and
//! serves the length-prefixed protocol on stdin/stdout.
//!
//! Why a process: rav1d 1.1.0 reaches `unwrap`/index panics from corrupted bitstreams inside `extern "C"`
//! functions, and a panic that cannot unwind aborts the whole process. In this process that abort only
//! ever takes down the worker; the host sees end-of-stream and reports a typed `decoder_crashed`.
//!
//! Abort handling without `unsafe`: Rust runs the panic hook BEFORE it aborts, so the worker installs a
//! hook that exits with `PANIC_EXIT_CODE` at once. That ends the process without the platform crash
//! reporter (on Windows an aborted process otherwise stays alive for seconds while Windows Error
//! Reporting handles it). Crashes that never reach the hook (stack overflow, foreign abort) are still
//! contained: the host kills the worker at its batch deadline.
//!
//! Threads: a reader thread owns stdin and exits the process when the host closes it (parent gone while
//! a decode is running), the decode loop runs on a thread with its own sized native stack, so decoding
//! never depends on the executable's 1 MiB main-thread stack.
//!
//! Built only with the `av1-worker` feature (the only feature that links rav1d). Fault injection for the
//! supervision tests exists only with `test-faults`; a product worker never reads the fault environment
//! variable (`tests/worker_fault_env.rs`).
#![deny(clippy::arithmetic_side_effects)]
#[cfg(feature = "test-faults")]
use crate::test_fault::WorkerTestFault;
use crate::wire::{self, OpenRequest, Request, WireFrame};
use hsk_studio_reel::av1::{Av1Decoder, DECODER_ID, DecodedFrame, decoder_version};
use hsk_studio_reel::{Limits, ReelError};
use std::{
    io::{self, Write},
    sync::mpsc,
    thread,
};

/// Exit code of a worker whose panic hook fired (an abort that was converted to a prompt exit).
pub const PANIC_EXIT_CODE: i32 = 70;
/// Native stack of the decode thread (virtual reservation; pages are committed on use).
const DECODE_STACK_BYTES: usize = 32 << 20;
/// How long an injected hang lasts before the worker leaves by itself (the host normally kills it first).
#[cfg(feature = "test-faults")]
const HANG_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Fault this worker injects into its first request (`test-faults` only).
#[cfg(feature = "test-faults")]
struct Faults {
    fault: WorkerTestFault,
    fired: bool,
}

#[cfg(feature = "test-faults")]
impl Faults {
    fn from_env() -> Self {
        Self {
            fault: WorkerTestFault::from_env(),
            fired: false,
        }
    }

    fn exit_on_open(&self) {
        if self.fault == WorkerTestFault::ExitOnOpen {
            std::process::exit(86);
        }
    }

    /// Before HELLO: sleep (the host normally kills the worker first), then carry on.
    fn stall_hello(&self) {
        if self.fault == WorkerTestFault::StallHello {
            thread::sleep(HANG_LIMIT);
        }
    }

    /// OPEN: answers with a FRAME instead of OPENED. Returns `Some(alive)` when the fault replaced the
    /// normal answer, `None` when the request must be served normally.
    fn on_open(&self, out: &mut impl Write, id: u64) -> Option<bool> {
        (self.fault == WorkerTestFault::FrameOnOpen)
            .then(|| reply(out, &wire::encode_tiny_frame(id)))
    }

    /// Answers a decoded picture with another size (zeroed planes) for the picture-size faults; every
    /// other fault leaves the picture as decoded. Sizes use saturating arithmetic (the hook must not panic).
    fn reshape(&self, mut frame: WireFrame) -> WireFrame {
        let (width, height) = match self.fault {
            WorkerTestFault::OversizePicture => {
                (frame.format.width.saturating_add(2), frame.format.height)
            }
            WorkerTestFault::UndersizePicture => (
                frame.format.width.saturating_sub(2).max(1),
                frame.format.height.saturating_sub(2).max(1),
            ),
            _ => return frame,
        };
        frame.format.width = width;
        frame.format.height = height;
        let bytes_per_sample = frame.format.bytes_per_sample();
        frame.planes = frame
            .format
            .plane_dims()
            .into_iter()
            .map(|(w, h)| {
                let samples = usize::try_from(w)
                    .unwrap_or(0)
                    .saturating_mul(usize::try_from(h).unwrap_or(0));
                vec![0_u8; samples.saturating_mul(bytes_per_sample)]
            })
            .collect();
        frame
    }

    /// Injects the fault on the first PACKET. Returns `Some(alive)` when the fault replaced the
    /// normal answer, `None` when the packet must be decoded normally.
    fn on_packet(&mut self, out: &mut impl Write, id: u64) -> Option<bool> {
        if self.fired || self.fault == WorkerTestFault::None {
            return None;
        }
        self.fired = true;
        match self.fault {
            WorkerTestFault::HangOnPacket => {
                thread::sleep(HANG_LIMIT);
                std::process::exit(3);
            }
            WorkerTestFault::ExitOnPacket => std::process::exit(86),
            WorkerTestFault::OversizeFrame => {
                Some(reply(out, &wire::encode_oversize_frame(id, 2 << 20)))
            }
            WorkerTestFault::GarbageReply => Some(reply(out, &wire::encode_garbage())),
            WorkerTestFault::None
            | WorkerTestFault::ExitOnOpen
            | WorkerTestFault::StallHello
            | WorkerTestFault::OversizePicture
            | WorkerTestFault::UndersizePicture
            | WorkerTestFault::FrameOnOpen => None,
        }
    }
}

fn to_wire(frame: DecodedFrame) -> WireFrame {
    WireFrame {
        tag: frame.tag,
        format: frame.format,
        film_grain_applied: frame.film_grain_applied,
        planes: frame.planes,
    }
}

fn open_decoder(open: &OpenRequest) -> Result<Av1Decoder, ReelError> {
    let limits = Limits {
        max_chunk_bytes: usize::try_from(open.max_chunk_bytes).unwrap_or(usize::MAX),
        ..Limits::default()
    };
    Av1Decoder::open(&limits, &open.config, open.color)
}

/// Writes every frame the decoder is holding, then the batch terminator. Returns false when the host
/// is gone (a write failed).
fn flush_batch(
    out: &mut impl Write,
    id: u64,
    decoder: &mut Av1Decoder,
    #[cfg(feature = "test-faults")] faults: &Faults,
) -> bool {
    let mut count = 0_u32;
    while let Some(frame) = decoder.next_frame() {
        let wire_frame = to_wire(frame);
        #[cfg(feature = "test-faults")]
        let wire_frame = faults.reshape(wire_frame);
        let encoded = wire::encode_frame(id, &wire_frame);
        if wire::write_frame(out, &encoded).is_err() {
            return false;
        }
        count = count.saturating_add(1);
    }
    wire::write_frame(out, &wire::encode_batch_end(id, count)).is_ok()
}

fn reply(out: &mut impl Write, payload: &[u8]) -> bool {
    wire::write_frame(out, payload).is_ok()
}

fn serve(requests: &mpsc::Receiver<Vec<u8>>) -> i32 {
    #[cfg(feature = "test-faults")]
    let mut faults = Faults::from_env();
    #[cfg(feature = "test-faults")]
    faults.stall_hello();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if !reply(&mut out, &wire::encode_hello(DECODER_ID, decoder_version())) {
        return 1;
    }
    let mut decoder: Option<Av1Decoder> = None;
    while let Ok(frame) = requests.recv() {
        let Ok(request) = wire::decode_request(&frame) else {
            return 2;
        };
        let alive = match request {
            Request::Open(id, open) => {
                // A new OPEN always replaces whatever decoder a cancelled segment left behind.
                decoder = None;
                #[cfg(feature = "test-faults")]
                faults.exit_on_open();
                #[cfg(feature = "test-faults")]
                {
                    if let Some(alive) = faults.on_open(&mut out, id) {
                        if !alive {
                            return 1;
                        }
                        continue;
                    }
                }
                match open_decoder(&open) {
                    Ok(opened) => {
                        decoder = Some(opened);
                        reply(&mut out, &wire::encode_opened(id))
                    }
                    Err(error) => reply(&mut out, &wire::encode_error(id, &error)),
                }
            }
            Request::Packet(id, tag, bytes) => {
                #[cfg(feature = "test-faults")]
                {
                    if let Some(alive) = faults.on_packet(&mut out, id) {
                        if !alive {
                            return 1;
                        }
                        continue;
                    }
                }
                match decoder.as_mut() {
                    None => reply(
                        &mut out,
                        &wire::encode_error(
                            id,
                            &ReelError::Unsupported {
                                what: "av1-worker-no-open",
                            },
                        ),
                    ),
                    Some(active) => match active.send(&bytes, tag) {
                        Ok(()) => flush_batch(
                            &mut out,
                            id,
                            active,
                            #[cfg(feature = "test-faults")]
                            &faults,
                        ),
                        Err(error) => {
                            decoder = None;
                            reply(&mut out, &wire::encode_error(id, &error))
                        }
                    },
                }
            }
            Request::Finish(id) => match decoder.take() {
                None => reply(
                    &mut out,
                    &wire::encode_error(
                        id,
                        &ReelError::Unsupported {
                            what: "av1-worker-no-open",
                        },
                    ),
                ),
                Some(mut active) => match active.finish() {
                    Ok(()) => flush_batch(
                        &mut out,
                        id,
                        &mut active,
                        #[cfg(feature = "test-faults")]
                        &faults,
                    ),
                    Err(error) => reply(&mut out, &wire::encode_error(id, &error)),
                },
            },
        };
        if !alive {
            return 1;
        }
    }
    0
}

/// Entry point of the `reel-native-av1-worker` binary; returns the process exit code. Reads request
/// frames from stdin, writes reply frames to stdout, and exits 0 when stdin closes.
pub fn worker_main() -> i32 {
    // A decoder panic would abort the process through the platform crash reporter; leave at once instead.
    std::panic::set_hook(Box::new(|_| std::process::exit(PANIC_EXIT_CODE)));
    let (requests_tx, requests_rx) = mpsc::channel::<Vec<u8>>();
    let reader = thread::Builder::new()
        .name("av1-worker-stdin".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read_frame(&mut input, wire::MAX_REQUEST_FRAME) {
                    Ok(frame) => {
                        if requests_tx.send(frame).is_err() {
                            return;
                        }
                    }
                    // The host closed stdin (or is gone): stop at once, even in the middle of a decode.
                    Err(wire::WireError::Closed) => std::process::exit(0),
                    Err(_) => std::process::exit(2),
                }
            }
        });
    if reader.is_err() {
        return 1;
    }
    thread::Builder::new()
        .name("av1-worker-decode".to_owned())
        .stack_size(DECODE_STACK_BYTES)
        .spawn(move || serve(&requests_rx))
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or(1)
}
