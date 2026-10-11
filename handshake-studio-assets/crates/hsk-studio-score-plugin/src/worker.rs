//! Worker process side (`score-plugin-worker`): serves the framed protocol on stdin/stdout.
//!
//! This is the ONLY place foreign plugin code ever runs; the host crate never loads it. Backends:
//! * `reference` (default): a pure-Rust gain-and-delay processor. It proves the protocol, isolation,
//!   timeout and reclaim contract; it does NOT load, run or prove CLAP or VST3 (MT-35560: a mock host
//!   does not prove a plugin format). The approved plugin file is only read and SHA-256-verified.
//! * `clap` (cargo feature `clap`, off by default): loads the approved `.clap` library after the
//!   worker re-verified its SHA-256 and hosts it through `clack-host`.
//!
//! Crash handling without a platform crash dialog: a panic hook exits with [`PANIC_EXIT_CODE`] at once
//! instead of aborting, and injected crashes use [`FAULT_EXIT_CODE`]. The worker exits 0 when stdin
//! closes or after a Shutdown request, and [`BACKEND_UNAVAILABLE_EXIT_CODE`] when asked for a backend
//! this build does not contain.
//!
//! Command line: `--backend reference|clap`, `--latency N` (reference), and test-only `--fault SPEC`
//! (set only by `HostOptions::test_args` for the child the host spawns): `crash-on-block=N`,
//! `hang-on-block=N`, `garbage-on-block=N`, `oversize-on-block=N`, `short-on-block=N`,
//! `hang-on-probe`, `partial-state`, `no-state` (block numbers count Process requests from 1).
use crate::backend::{Backend, Reference, Refusal};
use crate::types::{Arch, Completeness};
use crate::wire::{self, Frame, Hello, Reply, Request, WireError};
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS};
use std::{
    io::{self, Write},
    thread,
    time::Duration,
};

/// File name of the worker executable (without the platform suffix).
pub const WORKER_PROGRAM: &str = "score-plugin-worker";
/// Exit code of a worker whose panic hook fired.
pub const PANIC_EXIT_CODE: i32 = 70;
/// Exit code of an injected crash.
pub const FAULT_EXIT_CODE: i32 = 86;
/// Exit code of a worker asked for a backend that this build does not contain.
pub const BACKEND_UNAVAILABLE_EXIT_CODE: i32 = 3;
/// Largest request frame the worker accepts (a full 16-channel block plus header fields).
const MAX_REQUEST_FRAME: u32 = (MAX_CHANNELS * MAX_BLOCK_FRAMES * 4 + 4096) as u32;
const MAX_LATENCY: u32 = 1 << 16;
/// How long an injected hang lasts before the worker leaves by itself (the host kills it first).
const HANG_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    CrashOnBlock(u64),
    HangOnBlock(u64),
    GarbageOnBlock(u64),
    OversizeOnBlock(u64),
    ShortOnBlock(u64),
    HangOnProbe,
    PartialState,
    NoState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendChoice {
    Reference,
    Clap,
}

struct Options {
    backend: BackendChoice,
    latency: u32,
    faults: Vec<Fault>,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self {
            backend: BackendChoice::Reference,
            latency: 0,
            faults: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it.next()?;
            match flag.as_str() {
                "--backend" => {
                    options.backend = match value.as_str() {
                        "reference" => BackendChoice::Reference,
                        "clap" => BackendChoice::Clap,
                        _ => return None,
                    };
                }
                "--latency" => {
                    options.latency = value.parse().ok().filter(|l| *l <= MAX_LATENCY)?;
                }
                "--fault" => options.faults.push(Self::fault(value)?),
                _ => return None,
            }
        }
        Some(options)
    }

    fn fault(spec: &str) -> Option<Fault> {
        let (name, number) = match spec.split_once('=') {
            Some((name, number)) => (name, Some(number.parse::<u64>().ok()?)),
            None => (spec, None),
        };
        Some(match (name, number) {
            ("crash-on-block", Some(n)) => Fault::CrashOnBlock(n),
            ("hang-on-block", Some(n)) => Fault::HangOnBlock(n),
            ("garbage-on-block", Some(n)) => Fault::GarbageOnBlock(n),
            ("oversize-on-block", Some(n)) => Fault::OversizeOnBlock(n),
            ("short-on-block", Some(n)) => Fault::ShortOnBlock(n),
            ("hang-on-probe", None) => Fault::HangOnProbe,
            ("partial-state", None) => Fault::PartialState,
            ("no-state", None) => Fault::NoState,
            _ => return None,
        })
    }

    fn has(&self, fault: Fault) -> bool {
        self.faults.contains(&fault)
    }

    fn completeness(&self) -> Completeness {
        if self.has(Fault::NoState) {
            Completeness::Unavailable
        } else if self.has(Fault::PartialState) {
            Completeness::Partial
        } else {
            Completeness::Complete
        }
    }

    fn backend(&self) -> Option<Box<dyn Backend>> {
        match self.backend {
            BackendChoice::Reference => {
                Some(Box::new(Reference::new(self.latency, self.completeness())))
            }
            #[cfg(feature = "clap")]
            BackendChoice::Clap => Some(Box::new(crate::clap_backend::ClapBackend::new())),
            #[cfg(not(feature = "clap"))]
            BackendChoice::Clap => None,
        }
    }
}

fn refusal(id: u64, (code, detail): Refusal) -> Reply {
    Reply::Error { id, code, detail }
}

struct Server<'a> {
    options: &'a Options,
    out: io::StdoutLock<'static>,
    backend: Box<dyn Backend>,
    blocks: u64,
}

impl Server<'_> {
    fn send(&mut self, reply: &Reply) -> bool {
        wire::write_frame(&mut self.out, &wire::encode_reply(reply)).is_ok()
    }

    fn raw(&mut self, bytes: &[u8]) -> bool {
        self.out
            .write_all(bytes)
            .and_then(|()| self.out.flush())
            .is_ok()
    }

    /// Injects the fault selected for this block number. Returns true when the request must not be
    /// answered normally (the fault already wrote its own bytes).
    fn inject(&mut self) -> bool {
        let n = self.blocks;
        let options = self.options;
        if options.has(Fault::CrashOnBlock(n)) {
            std::process::exit(FAULT_EXIT_CODE);
        }
        if options.has(Fault::HangOnBlock(n)) {
            thread::sleep(HANG_LIMIT);
            std::process::exit(3);
        }
        if options.has(Fault::GarbageOnBlock(n)) {
            self.raw(&[0xAB; 16]);
            return true;
        }
        if options.has(Fault::OversizeOnBlock(n)) {
            let mut header = Vec::with_capacity(wire::HEADER_BYTES);
            header.extend_from_slice(&wire::MAGIC.to_le_bytes());
            header.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
            header.push(wire::K_PROCESSED);
            header.extend_from_slice(&0x7FFF_FFFF_u32.to_le_bytes());
            self.raw(&header);
            return true;
        }
        if options.has(Fault::ShortOnBlock(n)) {
            let mut partial = Vec::with_capacity(wire::HEADER_BYTES + 10);
            partial.extend_from_slice(&wire::MAGIC.to_le_bytes());
            partial.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
            partial.push(wire::K_PROCESSED);
            partial.extend_from_slice(&100_u32.to_le_bytes());
            partial.extend_from_slice(&[0; 10]);
            self.raw(&partial);
            std::process::exit(0);
        }
        false
    }

    /// Serves one request; false ends the loop.
    fn serve(&mut self, request: Request) -> bool {
        match request {
            Request::Probe { id, spec } => {
                if self.options.has(Fault::HangOnProbe) {
                    thread::sleep(HANG_LIMIT);
                    std::process::exit(3);
                }
                let reply = match self.backend.probe(&spec) {
                    Ok(info) => Reply::Probed {
                        id,
                        plugin_id: info.plugin_id,
                        name: info.name,
                        format: info.format.wire(),
                        abi: info.abi,
                        inputs: info.inputs,
                        outputs: info.outputs,
                    },
                    Err(error) => refusal(id, error),
                };
                self.send(&reply)
            }
            Request::Configure { id, spec, cfg } => {
                let reply = match self.backend.configure(&spec, &cfg) {
                    Ok(done) => Reply::Configured {
                        id,
                        identity: self.backend.identity().to_owned(),
                        latency: done.latency,
                        version: done.version,
                    },
                    Err(error) => refusal(id, error),
                };
                self.send(&reply)
            }
            Request::Process {
                id,
                expected_version,
                start_sample,
                frames,
                channels,
                samples,
            } => {
                self.blocks += 1;
                if self.inject() {
                    return true;
                }
                let frame_count = usize::try_from(frames).unwrap_or(usize::MAX);
                let reply = match self.backend.process(
                    expected_version,
                    start_sample,
                    frame_count,
                    usize::from(channels),
                    &samples,
                ) {
                    Ok(done) => Reply::Processed {
                        id,
                        version: done.version,
                        completeness: done.completeness.wire(),
                        latency: done.latency,
                        frames,
                        channels,
                        samples: done.samples,
                    },
                    Err(error) => refusal(id, error),
                };
                self.send(&reply)
            }
            Request::SetParam { id, param, value } => {
                let reply = match self.backend.set_parameter(param, value) {
                    Ok(version) => Reply::ParamSet { id, version },
                    Err(error) => refusal(id, error),
                };
                self.send(&reply)
            }
            Request::Capture { id, max_bytes } => {
                let reply = match self.backend.capture_state(max_bytes) {
                    Ok(done) => Reply::State {
                        id,
                        version: done.version,
                        completeness: done.completeness.wire(),
                        bytes: done.bytes,
                    },
                    Err(error) => refusal(id, error),
                };
                self.send(&reply)
            }
            Request::Shutdown { id } => {
                self.send(&Reply::Bye { id });
                false
            }
        }
    }
}

/// Entry point of the `score-plugin-worker` binary; returns the process exit code.
pub fn worker_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(options) = Options::parse(&args) else {
        return 2;
    };
    let Some(backend) = options.backend() else {
        return BACKEND_UNAVAILABLE_EXIT_CODE;
    };
    std::panic::set_hook(Box::new(|_| std::process::exit(PANIC_EXIT_CODE)));
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let hello = Hello {
        identity: backend.identity().to_owned(),
        clap_abi: backend.clap_abi(),
        arch: Arch::host().map_or(0, Arch::wire),
    };
    let mut server = Server {
        options: &options,
        out: io::stdout().lock(),
        backend,
        blocks: 0,
    };
    if !server.raw(&wire::encode_hello(&hello)) {
        return 1;
    }
    loop {
        let frame: Frame = match wire::read_frame(&mut input, MAX_REQUEST_FRAME) {
            Ok(frame) => frame,
            // The host closed stdin (or is gone): leave at once.
            Err(WireError::Closed) => return 0,
            Err(_) => return 2,
        };
        let Ok(request) = wire::decode_request(&frame) else {
            return 2;
        };
        if !server.serve(request) {
            return 0;
        }
    }
}
