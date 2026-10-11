//! Worker process side (`score-plugin-worker`): serves the framed protocol on stdin/stdout.
//!
//! This is the ONLY place foreign plugin code would ever run; the host crate never loads it. The
//! backend in this build is the REFERENCE backend: a pure-Rust gain-and-delay processor. It proves the
//! protocol, isolation, timeout and reclaim contract; it does NOT load, run or prove CLAP or VST3
//! (MT-35560: a mock host does not prove a plugin format). The approved plugin file is read and
//! SHA-256-verified by the worker too (the host already checked it before spawning, so a file swapped
//! in between is caught), but its content is not executed.
//!
//! Crash handling without `unsafe`: a panic hook exits with [`PANIC_EXIT_CODE`] at once instead of
//! aborting, so the platform crash reporter (on Windows a possible foreground dialog) never runs.
//! Injected crashes use [`FAULT_EXIT_CODE`] for the same reason. The worker exits 0 when stdin closes.
//!
//! Test-only faults come from the command line (`--fault SPEC`, `--latency N`), which only
//! `HostOptions::test_args` sets for the child the host spawns: `crash-on-block=N`, `hang-on-block=N`,
//! `garbage-on-block=N`, `oversize-on-block=N`, `short-on-block=N`, `hang-on-probe`, `partial-state`,
//! `no-state` (block numbers count Process requests from 1).
use crate::types::{Arch, Completeness, PluginFormat, SUPPORTED_CLAP_ABI};
use crate::wire::{
    self, ConfigureSpec, E_ABI, E_BUDGET, E_CONFIG, E_HASH, E_NOT_CONFIGURED, E_STALE,
    E_UNAVAILABLE, Frame, Hello, PluginSpec, Reply, Request, WireError,
};
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, sha256};
use std::{
    io::{self, Write},
    thread,
    time::Duration,
};

/// File name of the worker executable (without the platform suffix).
pub const WORKER_PROGRAM: &str = "score-plugin-worker";
/// Identity the reference backend reports in HELLO and receipts.
pub const REFERENCE_IDENTITY: &str = "reference-gain-delay@1";
/// Exit code of a worker whose panic hook fired.
pub const PANIC_EXIT_CODE: i32 = 70;
/// Exit code of an injected crash.
pub const FAULT_EXIT_CODE: i32 = 86;
/// Largest request frame the worker accepts (a full 16-channel block plus header fields).
const MAX_REQUEST_FRAME: u32 = (MAX_CHANNELS * MAX_BLOCK_FRAMES * 4 + 4096) as u32;
const MAX_PLUGIN_FILE_BYTES: u64 = 512 << 20;
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

struct Options {
    latency: u32,
    faults: Vec<Fault>,
}

impl Options {
    fn parse(args: &[String]) -> Option<Self> {
        let mut options = Self {
            latency: 0,
            faults: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it.next()?;
            match flag.as_str() {
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
}

/// The configured reference processor: `out[n] = in[n - latency] * gain`, history carried across blocks.
struct Active {
    channels: usize,
    max_frames: usize,
    gain: f64,
    version: u64,
    delay: Vec<Vec<f32>>,
}

impl Active {
    fn process(&mut self, frames: usize, samples: &[f32]) -> Vec<f32> {
        let gain = self.gain as f32;
        let latency = self.delay.first().map_or(0, Vec::len);
        let mut out = vec![0.0_f32; samples.len()];
        for (ch, history) in self.delay.iter_mut().enumerate() {
            let input = &samples[ch * frames..(ch + 1) * frames];
            let dst = &mut out[ch * frames..(ch + 1) * frames];
            for (i, slot) in dst.iter_mut().enumerate() {
                let source = if i < latency {
                    history[i]
                } else {
                    input[i - latency]
                };
                *slot = source * gain;
            }
            if latency > 0 {
                if frames >= latency {
                    history.copy_from_slice(&input[frames - latency..]);
                } else {
                    history.rotate_left(frames);
                    history[latency - frames..].copy_from_slice(input);
                }
            }
        }
        out
    }
}

fn gain_ok(value: f64) -> bool {
    value.is_finite() && (-16.0..=16.0).contains(&value)
}

/// Format, architecture and ABI gate plus the content hash re-check, before anything is "loaded".
fn verify(spec: &PluginSpec) -> Result<(), u8> {
    let supported = PluginFormat::from_wire(spec.format) == Some(PluginFormat::Clap)
        && spec.abi == SUPPORTED_CLAP_ABI
        && Arch::host().map(Arch::wire) == Some(spec.arch);
    if !supported {
        return Err(E_ABI);
    }
    let meta = std::fs::metadata(&spec.path).map_err(|_| E_UNAVAILABLE)?;
    if !meta.is_file() || meta.len() > MAX_PLUGIN_FILE_BYTES {
        return Err(E_UNAVAILABLE);
    }
    let bytes = std::fs::read(&spec.path).map_err(|_| E_UNAVAILABLE)?;
    if sha256(&bytes) == spec.sha {
        Ok(())
    } else {
        Err(E_HASH)
    }
}

fn error(id: u64, code: u8, detail: u64) -> Reply {
    Reply::Error { id, code, detail }
}

struct Server<'a> {
    options: &'a Options,
    out: io::StdoutLock<'static>,
    active: Option<Active>,
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

    fn configure(&mut self, id: u64, spec: &PluginSpec, cfg: &ConfigureSpec) -> Reply {
        if let Err(code) = verify(spec) {
            return error(id, code, 0);
        }
        let channels = usize::from(cfg.channels);
        let max_frames = usize::try_from(cfg.max_frames).unwrap_or(usize::MAX);
        if channels == 0
            || channels > MAX_CHANNELS
            || max_frames == 0
            || max_frames > MAX_BLOCK_FRAMES
            || cfg.sample_rate_hz == 0
            || cfg.sample_rate_hz > MAX_SAMPLE_RATE_HZ
        {
            return error(id, E_CONFIG, 0);
        }
        let mut gain = 1.0;
        for (param, value) in &cfg.params {
            if *param != 0 || !gain_ok(*value) {
                return error(id, E_CONFIG, u64::from(*param));
            }
            gain = *value;
        }
        let latency = usize::try_from(self.options.latency).unwrap_or(0);
        self.active = Some(Active {
            channels,
            max_frames,
            gain,
            version: 1,
            delay: vec![vec![0.0; latency]; channels],
        });
        Reply::Configured {
            id,
            identity: REFERENCE_IDENTITY.to_owned(),
            latency: self.options.latency,
            version: 1,
        }
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
                let reply = match verify(&spec) {
                    Ok(()) => Reply::Probed {
                        id,
                        plugin_id: "hsk.reference.gain-delay".to_owned(),
                        name: "Reference gain and delay (plugin file verified, not loaded)"
                            .to_owned(),
                        format: PluginFormat::Clap.wire(),
                        abi: SUPPORTED_CLAP_ABI,
                        inputs: 2,
                        outputs: 2,
                    },
                    Err(code) => error(id, code, 0),
                };
                self.send(&reply)
            }
            Request::Configure { id, spec, cfg } => {
                let reply = self.configure(id, &spec, &cfg);
                self.send(&reply)
            }
            Request::Process {
                id,
                expected_version,
                frames,
                channels,
                samples,
                ..
            } => {
                self.blocks += 1;
                if self.inject() {
                    return true;
                }
                let completeness = self.options.completeness();
                let latency = self.options.latency;
                let reply = match self.active.as_mut() {
                    None => error(id, E_NOT_CONFIGURED, 0),
                    Some(active) if expected_version != active.version => {
                        error(id, E_STALE, active.version)
                    }
                    Some(active)
                        if usize::from(channels) != active.channels
                            || usize::try_from(frames).map_or(true, |f| f > active.max_frames) =>
                    {
                        error(id, E_BUDGET, u64::from(frames))
                    }
                    Some(active) => {
                        let version = active.version;
                        let frame_count = usize::try_from(frames).unwrap_or(0);
                        Reply::Processed {
                            id,
                            version,
                            completeness: completeness.wire(),
                            latency,
                            frames,
                            channels,
                            samples: active.process(frame_count, &samples),
                        }
                    }
                };
                self.send(&reply)
            }
            Request::SetParam { id, param, value } => {
                let reply = match self.active.as_mut() {
                    None => error(id, E_NOT_CONFIGURED, 0),
                    Some(_) if param != 0 || !gain_ok(value) => {
                        error(id, E_CONFIG, u64::from(param))
                    }
                    Some(active) => {
                        active.gain = value;
                        active.version += 1;
                        Reply::ParamSet {
                            id,
                            version: active.version,
                        }
                    }
                };
                self.send(&reply)
            }
            Request::Capture { id, max_bytes } => {
                let completeness = self.options.completeness();
                let reply = match self.active.as_ref() {
                    None => error(id, E_NOT_CONFIGURED, 0),
                    Some(active) => {
                        let bytes = match completeness {
                            Completeness::Complete => active.gain.to_le_bytes().to_vec(),
                            Completeness::Partial => (active.gain as f32).to_le_bytes().to_vec(),
                            Completeness::Unavailable => Vec::new(),
                        };
                        if bytes.len() > usize::try_from(max_bytes).unwrap_or(usize::MAX) {
                            error(id, E_BUDGET, u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                        } else {
                            Reply::State {
                                id,
                                version: active.version,
                                completeness: completeness.wire(),
                                bytes,
                            }
                        }
                    }
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
    std::panic::set_hook(Box::new(|_| std::process::exit(PANIC_EXIT_CODE)));
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut server = Server {
        options: &options,
        out: io::stdout().lock(),
        active: None,
        blocks: 0,
    };
    let hello = Hello {
        identity: REFERENCE_IDENTITY.to_owned(),
        clap_abi: SUPPORTED_CLAP_ABI,
        arch: Arch::host().map_or(0, Arch::wire),
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
