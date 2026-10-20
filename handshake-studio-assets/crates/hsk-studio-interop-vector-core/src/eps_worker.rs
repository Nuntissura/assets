//! Isolated EPS-to-PDF conversion (lane SMB-DXF-EPS-IMPORT; START_LOG lead decision 2026-10-16:
//! `stet`, Apache-2.0 OR MIT, in an isolated capped worker; Ghostscript excluded).
//!
//! PostScript is a Turing-complete language, so it never runs in the host process:
//!
//! - host (feature `eps`): [`WorkerEpsConverter`] starts one fresh worker process per conversion
//!   through `hsk-studio-worker` (the single owner of worker supervision, D18): empty environment,
//!   no console window, piped stdin/stdout, null stderr, a wall-clock deadline with cancel polling,
//!   and on Windows a Job Object with a process memory cap and a CPU-time cap (`job-caps`). A
//!   worker that overruns is killed and reaped; nothing it produced is used;
//! - worker (feature `eps-worker`, binary `interop-vector-eps-worker`): runs `stet` 0.8.4 with
//!   `render_to_pdf` on a thread with its own 64 MiB stack. Before any input is read it replaces
//!   the file-system operators: `file` and `run` open only the interpreter's embedded resources
//!   (never `%stdin`, `%stdout`, `%stderr` or a disk path), `deletefile`, `renamefile` and
//!   `filenameforall` fail with `invalidfileaccess`, `status` reports a disk name as absent, the
//!   system-font loaders (`.loadsystemfont`, `.loadbinarysystemfont`, `.loadbinaryfontfile`)
//!   answer `false`; PostScript `print` output goes to a sink. A panic exits the worker at once
//!   (exit code 70) instead of aborting through the platform crash reporter.
//!
//! Wire (both directions): `u32` little-endian length, then the payload. Worker to host: first a
//! HELLO frame `H` + protocol version byte; per request a reply `P` + PDF bytes or `E` + an ASCII
//! error code. Host to worker: `C` + `u32` output cap + the PostScript bytes.
//!
//! Losses on the import side are `eps_via_pdf_conversion` (see `eps`). Not covered: a stack
//! overflow or foreign abort inside the interpreter is contained only by process isolation (the
//! host reports `crashed`); without `job-caps` (non-Windows) there is no memory or CPU cap, only
//! the deadline.
//!
//! Proofs: `tests/eps_worker.rs` (`file_operators_are_denied_in_the_worker`,
//! `postscript_loops_and_bombs_are_contained`, `eps_converts_to_pdf_in_the_isolated_worker`,
//! `exported_epsf_converts_and_caps_are_typed`).
#![deny(clippy::arithmetic_side_effects)]
// The frame helpers serve the host (`eps`) and the worker (`eps-worker`) halves only.
#![cfg_attr(not(feature = "eps"), allow(dead_code))]
use std::io::{Read, Write};

/// Program name of the worker binary (looked up beside the running executable).
pub const EPS_WORKER_PROGRAM: &str = "interop-vector-eps-worker";
/// Wire protocol version in the HELLO frame.
pub const EPS_WORKER_PROTOCOL: u8 = 1;
/// Exit code of a worker whose panic hook fired.
pub const EPS_WORKER_PANIC_EXIT: i32 = 70;
/// Largest request frame the worker accepts (the PostScript program plus 5 bytes).
pub const MAX_REQUEST_FRAME: usize = 64 * 1024 * 1024;

const TAG_HELLO: u8 = b'H';
const TAG_CONVERT: u8 = b'C';
const TAG_PDF: u8 = b'P';
const TAG_ERROR: u8 = b'E';

fn read_frame(reader: &mut impl Read, cap: usize) -> Result<Vec<u8>, &'static str> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).map_err(|_| "closed")?;
    let length = usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| "frame_too_large")?;
    if length > cap {
        return Err("frame_too_large");
    }
    let mut payload = Vec::new();
    let wanted = u64::try_from(length).map_err(|_| "frame_too_large")?;
    let got = reader.take(wanted).read_to_end(&mut payload).map_err(|_| "closed")?;
    if got != length {
        return Err("closed");
    }
    Ok(payload)
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(payload.len().saturating_add(4));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()
}

#[cfg(feature = "eps")]
pub use host::{EpsWorkerOptions, WorkerEpsConverter};

#[cfg(feature = "eps")]
mod host {
    use super::*;
    use crate::eps::EpsPdfConverter;
    use crate::error::{Result, VectorError};
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_worker::{
        Classified, EXIT_PATIENCE, Failure, JOB_CAPS_AVAILABLE, OsLimits, ReadFailure, SpawnError, StartRequest, Supervisor, SupervisorConfig, Waited, after, resolve_program,
    };
    use std::time::Instant;

    /// How often a running conversion is checked for a memory-cap event. An allocation the Job
    /// Object refuses aborts the worker through a fast-fail that the crash reporter may hold for
    /// seconds; the kernel's memory notice arrives at once, so the host reclaims the worker on it.
    const MEMORY_POLL: Duration = Duration::from_millis(100);
    use std::path::PathBuf;
    use std::process::ChildStdout;
    use std::time::Duration;

    /// Caps of one conversion.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct EpsWorkerOptions {
        /// Explicit worker program; `None` looks beside the running executable only.
        pub program: Option<PathBuf>,
        /// Wall-clock time for start-up plus conversion.
        pub deadline: Duration,
        /// Most bytes the worker process may commit (Windows Job Object; needs `job-caps`).
        pub memory_bytes: u64,
        /// CPU time of the conversion (Windows Job Object).
        pub cpu_time: Duration,
        /// Largest PostScript program sent.
        pub max_input_bytes: usize,
        /// Largest PDF accepted back.
        pub max_output_bytes: u32,
    }

    impl Default for EpsWorkerOptions {
        fn default() -> Self {
            Self {
                program: None,
                deadline: Duration::from_secs(30),
                memory_bytes: 1024 * 1024 * 1024,
                cpu_time: Duration::from_secs(20),
                max_input_bytes: 32 * 1024 * 1024,
                max_output_bytes: 64 * 1024 * 1024,
            }
        }
    }

    /// One reply frame: tag and body.
    pub(super) struct Frame {
        tag: u8,
        body: Vec<u8>,
    }

    fn read(stdout: &mut ChildStdout, cap: u32) -> std::result::Result<Frame, ReadFailure<&'static str>> {
        // The cap bounds the body; the tag byte comes on top.
        let cap = usize::try_from(cap).unwrap_or(usize::MAX).saturating_add(1);
        match read_frame(stdout, cap) {
            Ok(mut payload) if !payload.is_empty() => {
                let tag = payload.remove(0);
                Ok(Frame { tag, body: payload })
            }
            Ok(_) => Err(ReadFailure::Failed("empty_frame")),
            Err("closed") => Err(ReadFailure::Closed),
            Err(code) => Err(ReadFailure::Failed(code)),
        }
    }

    /// EPS to PDF through the isolated worker (module docs).
    pub struct WorkerEpsConverter {
        supervisor: Supervisor,
        options: EpsWorkerOptions,
    }

    impl WorkerEpsConverter {
        /// [`VectorError::EpsConverterUnavailable`] when the worker program does not exist.
        pub fn new(options: EpsWorkerOptions) -> Result<Self> {
            let program = resolve_program(options.program.clone(), EPS_WORKER_PROGRAM).ok_or(VectorError::EpsConverterUnavailable)?;
            let supervisor = Supervisor::new(SupervisorConfig {
                program,
                args: Vec::new(),
                env: Vec::new(),
                max_workers: 4,
                max_in_flight_bytes: u64::MAX,
                spawn_timeout: options.deadline,
                os_limits: JOB_CAPS_AVAILABLE.then_some(OsLimits { process_memory_bytes: options.memory_bytes, cpu_time: options.cpu_time }),
                reply_buffer: Some(1),
                io_thread_name: "eps-worker-io".to_owned(),
            });
            Ok(Self { supervisor, options })
        }

        /// Process and admission counters of the supervisor (tests and diagnostics).
        pub fn stats(&self) -> hsk_studio_worker::SupervisorStats {
            self.supervisor.stats()
        }
    }

    fn classified(c: Classified<&'static str>, memory: bool) -> VectorError {
        match c {
            Classified::Canceled => VectorError::Canceled,
            Classified::Deadline | Classified::Crashed { .. } if memory => VectorError::EpsConversionFailed("memory_cap"),
            Classified::Deadline => VectorError::EpsConversionFailed("deadline"),
            Classified::Crashed { .. } if memory => VectorError::EpsConversionFailed("memory_cap"),
            Classified::Crashed { .. } => VectorError::EpsConversionFailed("crashed"),
            Classified::Protocol(_) => VectorError::EpsConversionFailed("protocol"),
        }
    }

    fn error_code(body: &[u8]) -> &'static str {
        match body {
            b"postscript_error" => "postscript_error",
            b"no_pages" => "no_pages",
            b"output_over_cap" => "output_over_cap",
            b"bad_request" => "protocol",
            b"panicked" => "crashed",
            _ => "protocol",
        }
    }

    impl EpsPdfConverter for WorkerEpsConverter {
        fn eps_to_pdf(&self, postscript: &[u8], cancel: &CancellationToken) -> Result<Vec<u8>> {
            cancel.check().map_err(|_| VectorError::Canceled)?;
            if postscript.len() > self.options.max_input_bytes || postscript.len() > MAX_REQUEST_FRAME.saturating_sub(5) {
                return Err(VectorError::BudgetExceeded("eps_worker_input"));
            }
            let permit = self.supervisor.admit(0).map_err(|_| VectorError::BudgetExceeded("eps_workers"))?;
            let request = StartRequest { owner_session: "interop-vector-eps", extra_args: &[], keepalive: None, hello_cap: 16 };
            let (mut session, hello) = match self.supervisor.spawn(permit, request, read, cancel) {
                Ok(s) => s,
                Err(SpawnError::Canceled) => return Err(VectorError::Canceled),
                Err(SpawnError::Unavailable) => return Err(VectorError::EpsConverterUnavailable),
                Err(SpawnError::Handshake(c, _)) => return Err(classified(c, false)),
            };
            if hello.tag != TAG_HELLO || hello.body.first() != Some(&EPS_WORKER_PROTOCOL) {
                session.terminate(std::time::Duration::ZERO);
                return Err(VectorError::EpsConversionFailed("protocol"));
            }
            let _ = session.arm_cpu(self.options.cpu_time);
            let mut payload = Vec::with_capacity(postscript.len().saturating_add(9));
            let body_len = u32::try_from(postscript.len().saturating_add(5)).map_err(|_| VectorError::BudgetExceeded("eps_worker_input"))?;
            payload.extend_from_slice(&body_len.to_le_bytes());
            payload.push(TAG_CONVERT);
            payload.extend_from_slice(&self.options.max_output_bytes.to_le_bytes());
            payload.extend_from_slice(postscript);
            if !session.send_request(payload, self.options.max_output_bytes) {
                let (c, _) = session.fail(Failure::Read(ReadFailure::Closed));
                return Err(classified(c, false));
            }
            let until = after(self.options.deadline);
            let reply = loop {
                let slice = after(MEMORY_POLL).min(until);
                let failure: Failure<&'static str> = match session.wait(cancel, slice) {
                    Waited::Frame(frame) => break frame,
                    Waited::Canceled => Failure::Canceled,
                    Waited::CpuCap => Failure::Deadline,
                    Waited::Failed(f) => Failure::Read(f),
                    Waited::TimedOut => {
                        if session.drain_os_events().memory {
                            session.fail(Failure::Deadline);
                            return Err(VectorError::EpsConversionFailed("memory_cap"));
                        }
                        if Instant::now() < until {
                            continue;
                        }
                        Failure::Deadline
                    }
                };
                let (c, _) = session.fail(failure);
                let memory = session.drain_os_events().memory;
                return Err(classified(c, memory));
            };
            // One conversion per process: close stdin and reap (the worker exits at end of input).
            session.terminate(EXIT_PATIENCE);
            match reply.tag {
                TAG_PDF if reply.body.starts_with(b"%PDF-") => Ok(reply.body),
                TAG_ERROR => Err(VectorError::EpsConversionFailed(error_code(&reply.body))),
                _ => Err(VectorError::EpsConversionFailed("protocol")),
            }
        }
    }
}

#[cfg(feature = "eps-worker")]
pub use worker::eps_worker_main;

#[cfg(feature = "eps-worker")]
mod worker {
    use super::*;
    use std::sync::OnceLock;
    use stet_core::context::Context;
    use stet_core::error::PsError;
    use stet_core::object::{PsObject, PsValue};

    type Op = fn(&mut Context) -> Result<(), PsError>;
    /// Operator name, replacement, and where the original is kept (when the replacement calls it).
    type Guard = (&'static [u8], Op, Option<&'static OnceLock<Op>>);

    /// Native stack of the interpreter thread.
    const STACK_BYTES: usize = 64 << 20;
    /// Resolution handed to the PDF device (it only scales image sampling; geometry stays vector).
    const PDF_DPI: f64 = 300.0;

    static ORIGINAL_FILE: OnceLock<Op> = OnceLock::new();
    static ORIGINAL_RUN: OnceLock<Op> = OnceLock::new();
    static ORIGINAL_STATUS: OnceLock<Op> = OnceLock::new();

    fn string_operand(ctx: &Context, from_top: usize) -> Option<String> {
        let obj = ctx.o_stack.peek(from_top).ok()?;
        match obj.value {
            PsValue::String { entity, start, len } => String::from_utf8(ctx.strings.get(entity, start, len).to_vec()).ok(),
            _ => None,
        }
    }

    fn embedded(ctx: &Context, name: &str) -> bool {
        !name.starts_with('%') && ctx.files.get_embedded_file(name).is_some()
    }

    /// `file`: only the interpreter's embedded resources, read-only.
    fn guarded_file(ctx: &mut Context) -> Result<(), PsError> {
        let mode = string_operand(ctx, 0);
        let name = string_operand(ctx, 1);
        match (name, mode) {
            (Some(name), Some(mode)) if mode == "r" && embedded(ctx, &name) => ORIGINAL_FILE.get().ok_or(PsError::InvalidFileAccess).and_then(|op| op(ctx)),
            (Some(_), Some(_)) => Err(PsError::InvalidFileAccess),
            _ => ORIGINAL_FILE.get().ok_or(PsError::InvalidFileAccess).and_then(|op| op(ctx)),
        }
    }

    /// `run`: only embedded resources.
    fn guarded_run(ctx: &mut Context) -> Result<(), PsError> {
        match string_operand(ctx, 0) {
            Some(name) if embedded(ctx, &name) => ORIGINAL_RUN.get().ok_or(PsError::InvalidFileAccess).and_then(|op| op(ctx)),
            Some(_) => Err(PsError::InvalidFileAccess),
            None => ORIGINAL_RUN.get().ok_or(PsError::InvalidFileAccess).and_then(|op| op(ctx)),
        }
    }

    /// `status` of a file name: a disk name is reported absent.
    fn guarded_status(ctx: &mut Context) -> Result<(), PsError> {
        match string_operand(ctx, 0) {
            Some(name) if !embedded(ctx, &name) => {
                ctx.o_stack.pop()?;
                ctx.o_stack.push(PsObject::bool(false))
            }
            _ => ORIGINAL_STATUS.get().ok_or(PsError::InvalidFileAccess).and_then(|op| op(ctx)),
        }
    }

    fn denied(_: &mut Context) -> Result<(), PsError> {
        Err(PsError::InvalidFileAccess)
    }

    /// System-font loaders: `name -> false` and `name path -> false`.
    fn no_system_font(ctx: &mut Context) -> Result<(), PsError> {
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::bool(false))
    }
    fn no_font_file(ctx: &mut Context) -> Result<(), PsError> {
        ctx.o_stack.pop()?;
        ctx.o_stack.pop()?;
        ctx.o_stack.push(PsObject::bool(false))
    }

    /// Replaces the file-system operators (module docs). Returns false when an operator to guard
    /// is missing (the worker then refuses to run anything).
    fn guard(ctx: &mut Context) -> bool {
        let table: [Guard; 9] = [
            (b"file", guarded_file, Some(&ORIGINAL_FILE)),
            (b"run", guarded_run, Some(&ORIGINAL_RUN)),
            (b"status", guarded_status, Some(&ORIGINAL_STATUS)),
            (b"deletefile", denied, None),
            (b"renamefile", denied, None),
            (b"filenameforall", denied, None),
            (b".loadsystemfont", no_system_font, None),
            (b".loadbinarysystemfont", no_system_font, None),
            (b".loadbinaryfontfile", no_font_file, None),
        ];
        let mut all = true;
        for (name, replacement, original) in table {
            let Some(id) = ctx.names.find(name) else {
                all = false;
                continue;
            };
            let mut found = false;
            for entry in ctx.operators.iter_mut().filter(|e| e.name == id) {
                if let Some(slot) = original {
                    let _ = slot.set(entry.func);
                }
                entry.func = replacement;
                found = true;
            }
            all &= found;
        }
        ctx.stdout = Box::new(std::io::sink());
        all
    }

    fn convert(request: &[u8]) -> Vec<u8> {
        let fail = |code: &[u8]| {
            let mut v = vec![TAG_ERROR];
            v.extend_from_slice(code);
            v
        };
        let (Some(&TAG_CONVERT), Some(cap)) = (request.first(), request.get(1..5)) else { return fail(b"bad_request") };
        let cap = usize::try_from(u32::from_le_bytes([cap[0], cap[1], cap[2], cap[3]])).unwrap_or(usize::MAX);
        let postscript = request.get(5..).unwrap_or_default();
        let mut interp = stet::Interpreter::builder().suppress_output().build();
        if !guard(interp.context()) {
            return fail(b"bad_request");
        }
        match interp.render_to_pdf(postscript, PDF_DPI) {
            Ok(pdf) if pdf.is_empty() => fail(b"no_pages"),
            Ok(pdf) if pdf.len() > cap => fail(b"output_over_cap"),
            Ok(pdf) => {
                let mut v = Vec::with_capacity(pdf.len().saturating_add(1));
                v.push(TAG_PDF);
                v.extend_from_slice(&pdf);
                v
            }
            Err(_) => fail(b"postscript_error"),
        }
    }

    /// Worker process entry (binary `interop-vector-eps-worker`): HELLO, then one reply per
    /// request until stdin closes. Returns the process exit code.
    pub fn eps_worker_main() -> i32 {
        std::panic::set_hook(Box::new(|_| std::process::exit(EPS_WORKER_PANIC_EXIT)));
        hsk_studio_worker::harden_worker_process();
        let mut stdout = std::io::stdout().lock();
        if write_frame(&mut stdout, &[TAG_HELLO, EPS_WORKER_PROTOCOL]).is_err() {
            return 1;
        }
        let mut stdin = std::io::stdin().lock();
        loop {
            let request = match read_frame(&mut stdin, MAX_REQUEST_FRAME) {
                Ok(r) => r,
                Err("closed") => return 0,
                Err(_) => return 2,
            };
            let reply = std::thread::Builder::new()
                .name("eps-worker-interp".to_owned())
                .stack_size(STACK_BYTES)
                .spawn(move || convert(&request))
                .ok()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| vec![TAG_ERROR, b'p', b'a', b'n', b'i', b'c', b'k', b'e', b'd']);
            if write_frame(&mut stdout, &reply).is_err() {
                return 1;
            }
        }
    }
}
