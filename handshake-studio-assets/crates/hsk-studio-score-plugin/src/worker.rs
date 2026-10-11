//! Worker process side (`score-plugin-worker`): serves the framed protocol on stdin/stdout.
//!
//! This is the only place foreign plugin code runs; the host crate does not load it. Backends:
//! * `reference` (default): a pure-Rust gain-and-delay processor. It exercises the protocol, isolation,
//!   timeout and reclaim contract; it does NOT load, run or prove CLAP or VST3 (MT-35560: a mock host
//!   does not prove a plugin format). The approved plugin file is only read and SHA-256-verified.
//! * `clap` (cargo feature `clap`, off by default): loads the approved `.clap` library after the
//!   worker re-verified its SHA-256 and hosts it through `clack-host`.
//!
//! Crash handling without a platform crash dialog: a panic hook exits with [`PANIC_EXIT_CODE`] at once
//! instead of aborting. The worker exits 0 when stdin closes or after a Shutdown request, and [`BACKEND_UNAVAILABLE_EXIT_CODE`] when asked for a backend
//! this build does not contain.
//!
//! Command line: `--backend reference|clap` and `--latency N` (reference). Test-only fault injection
//! (`--fault SPEC`, see `faults.rs`) exists only in builds with the non-default cargo feature
//! `test-faults`; without it `--fault` is an unknown flag and the worker exits 2.
#![deny(clippy::arithmetic_side_effects)]
use crate::backend::{Backend, Reference, Refusal};
use crate::types::{Arch, Completeness};
use crate::wire::{self, Frame, Hello, Reply, Request, WireError};
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS};
use std::io::{self, Write};

#[cfg(feature = "test-faults")]
use crate::faults::Faults;
#[cfg(not(feature = "test-faults"))]
use no_faults::Faults;

/// Without the `test-faults` feature there are no faults: every hook is a no-op.
#[cfg(not(feature = "test-faults"))]
mod no_faults {
    use crate::types::Completeness;
    use std::io::Write;

    #[derive(Default)]
    pub(super) struct Faults(());

    impl Faults {
        pub(super) fn completeness(&self) -> Completeness {
            Completeness::Complete
        }
        pub(super) fn on_probe(&self) {}
        pub(super) fn on_block(&self, _n: u64, _out: &mut impl Write) -> bool {
            false
        }
    }
}

/// File name of the worker executable (without the platform suffix).
pub const WORKER_PROGRAM: &str = "score-plugin-worker";
/// Exit code of a worker whose panic hook fired.
pub const PANIC_EXIT_CODE: i32 = 70;
/// Exit code of a worker asked for a backend that this build does not contain.
pub const BACKEND_UNAVAILABLE_EXIT_CODE: i32 = 3;
/// Largest request frame the worker accepts (a full 16-channel block plus header fields).
const MAX_REQUEST_FRAME: u32 = (MAX_CHANNELS * MAX_BLOCK_FRAMES * 4 + 4096) as u32;
const MAX_LATENCY: u32 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendChoice {
    Reference,
    Clap,
}

struct Options {
    backend: BackendChoice,
    latency: u32,
    faults: Faults,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self {
            backend: BackendChoice::Reference,
            latency: 0,
            faults: Faults::default(),
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
                #[cfg(feature = "test-faults")]
                "--fault" => options.faults.add(value)?,
                _ => return None,
            }
        }
        Some(options)
    }

    fn completeness(&self) -> Completeness {
        self.faults.completeness()
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

    /// Serves one request; false ends the loop.
    fn serve(&mut self, request: Request) -> bool {
        match request {
            Request::Probe { id, spec } => {
                self.options.faults.on_probe();
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
                self.blocks = self.blocks.saturating_add(1);
                if self.options.faults.on_block(self.blocks, &mut self.out) {
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
