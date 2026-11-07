//! The standalone host's durable diagnostics (lane SMB-OBSERVE-DIAG-SINK-1, work order item 5): what ran survives the
//! process as JSONL in a diag dir. This is the standalone host's sink, not observe's: observe stays a bounded adapter
//! with no file I/O, and Flight Recorder, Palmistry and the ring adapter stay Handshake's.
//!
//! # What is written
//!
//! One JSON object per line, field names taken from the ring record and the observe wire (`event_code`, `phase_marker`,
//! `severity`, `sequence_id`, `counter_a`, `counter_b`, `timestamp_nanos`, `correlation_id`, `resource_id`,
//! `session_id`, ...). Kinds: `segment_start`, `segment_end`, `run_start`, `run_end`, `op`, `frame`, `frame_refused`,
//! `health`, `panic`. Only ids, closed codes and counters: a frame's own attribution strings and detail fields, and
//! op and command names that pass the token rule. Never document contents, panic messages or command-line arguments.
//! A frame that does not decode whole is `frame_refused` with its reason and length, none of its bytes.
//!
//! # Bounds, rotation and the stop rule
//!
//! Every line is at most [`MAX_LINE_BYTES`] (over it, a `frame_refused` or a null token takes its place), every file at
//! most [`MAX_FILE_BYTES`], and a directory holds at most [`MAX_FILES`] files of ours, so at most 64 MiB. When a file is
//! full the next one starts; when a new file would pass the count, the oldest of ours goes first, ordered by the numbers
//! in the file names. Only files with exactly our names are ever removed. A process that loses its current file to
//! another process's rotation keeps writing to a removed file until its own next rotation. Pinned by
//! `tests/diag_sink.rs` `rotation_keeps_the_directory_inside_its_bound_and_removes_only_our_names`.
//!
//! A failed write or open never reaches the caller: no method here returns an error or panics on I/O. The line is
//! dropped and counted, and the next line tries again from a fresh open. After [`MAX_CONSECUTIVE_FAILURES`] failures in
//! a row the sink stops for the life of the process: it touches the disk no more and only counts ([`DiagState`]).
//!
//! # What survives what
//!
//! Each line is one unbuffered write to the OS, so everything written survives the death of the process (a panic,
//! `abort`, a stack overflow), not the loss of the machine: nothing is synced except a panic record. A panic also leaves
//! a `panic` record ([`install_panic_hook`]). A stack overflow or an abort runs no hook and leaves no record of itself:
//! what it leaves is every line up to the last one written and no `run_end`, which [`survey`] counts as
//! `unterminated_runs`. Pinned by `a_stack_overflow_leaves_the_lines_written_and_no_end_of_run` and
//! `an_abort_leaves_the_lines_written_and_no_end_of_run`.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_observe::ring::ring_of_outcome;
use hsk_studio_observe::{DeliveryClass, FailureCode, MAX_FRAME_BYTES, Outcome};
use serde_json::{Map, Value, json};
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, TryLockError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// `schema` of every line.
pub const DIAG_SCHEMA: &str = "hsk.studio.diag@1";
/// The variable naming the diag dir of a process that gets no `--diag-dir`.
pub const DIAG_DIR_ENV: &str = "STUDIO_DIAG_DIR";
/// The longest line, newline included.
pub const MAX_LINE_BYTES: usize = 8192;
/// The longest file.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The most files of ours in one directory.
pub const MAX_FILES: usize = 16;
/// Failed writes or opens in a row before the sink stops for the life of the process.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 8;
/// Panic records one process writes; later panics are not recorded.
pub const MAX_PANIC_RECORDS: u32 = 64;
/// The shortest file: two lines.
const MIN_FILE_BYTES: u64 = 2 * MAX_LINE_BYTES as u64;
/// `health` exit: no diag dir, so what runs would not survive the process.
pub const HEALTH_EXIT_DIAG_NOT_CONFIGURED: i32 = 3;
/// `health` exit: the diag dir could not be written.
pub const HEALTH_EXIT_DIAG_UNAVAILABLE: i32 = 4;
/// `health` exit: a foundation descriptor did not parse.
pub const HEALTH_EXIT_DESCRIPTOR_MISMATCH: i32 = 5;

/// Directory entries one listing looks at, so a hostile directory cannot make a listing unbounded.
const MAX_SCAN_ENTRIES: usize = 4096;
/// Newest runs whose last line [`survey`] reads, and how much of each.
const MAX_SURVEYED_RUNS: usize = 64;
const SURVEY_TAIL_BYTES: u64 = 16 * 1024;
/// Room kept in a file for its `segment_end` line.
const SEGMENT_END_RESERVE: u64 = 256;
/// Lines between checks that the current file is still there.
const LINES_PER_PRESENCE_CHECK: u64 = 64;
/// The most that `seq` and a newline add to a line.
const SEQ_ROOM: usize = 28;
/// The longest name that counts as a token.
const MAX_TOKEN_BYTES: usize = 64;

/// The size bounds of one sink; [`DiagLimits::new`] keeps them workable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagLimits {
    max_file_bytes: u64,
    max_files: usize,
}
impl DiagLimits {
    /// A file holds at least two lines of [`MAX_LINE_BYTES`]; a directory at least 3 files (one is kept free for a
    /// panic record).
    pub fn new(max_file_bytes: u64, max_files: usize) -> Self {
        Self { max_file_bytes: max_file_bytes.max(MIN_FILE_BYTES), max_files: max_files.max(3) }
    }
    pub fn max_file_bytes(self) -> u64 {
        self.max_file_bytes
    }
    pub fn max_files(self) -> usize {
        self.max_files
    }
}
impl Default for DiagLimits {
    fn default() -> Self {
        Self::new(MAX_FILE_BYTES, MAX_FILES)
    }
}

/// One operation of a run, as the sink records it. Names that fail the token rule are written as null.
#[derive(Clone, Copy, Debug)]
pub struct OpLine<'a> {
    pub op: &'a str,
    pub n: u64,
    pub ok: bool,
    pub revision: Option<u64>,
    pub frames: u64,
    pub error: Option<&'a str>,
    pub correlation: Option<u64>,
    pub command_id: Option<&'a str>,
}

/// What a sink has done so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagState {
    /// `active`, `unavailable` (the last write or open failed; the next line tries again) or `stopped`.
    pub status: &'static str,
    /// Why, when not active: `dir_unavailable`, `write_failed`, `prune_failed`.
    pub code: Option<&'static str>,
    pub lines_written: u64,
    pub lines_dropped: u64,
    pub write_failures: u64,
    pub rotations: u64,
    pub files_removed: u64,
    pub remove_failures: u64,
}

type Opened = (Box<dyn Write + Send>, u64);
type Opener = fn(&Path) -> io::Result<Opened>;

fn open_file(path: &Path) -> io::Result<Opened> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let len = file.metadata()?.len();
    Ok((Box::new(file), len))
}

struct Inner {
    opener: Opener,
    file: Option<Box<dyn Write + Send>>,
    file_bytes: u64,
    segment: u32,
    torn_line: bool,
    consecutive_failures: u32,
    stopped: Option<&'static str>,
    failure: Option<&'static str>,
    seq: u64,
    lines_since_check: u64,
    lines_written: u64,
    lines_dropped: u64,
    write_failures: u64,
    rotations: u64,
    files_removed: u64,
    remove_failures: u64,
}

struct Shared {
    dir: PathBuf,
    limits: DiagLimits,
    instance: u32,
    started: Instant,
    inner: Mutex<Inner>,
}

/// A durable JSONL sink over one directory. Cloning shares it; every method is infallible for the caller.
#[derive(Clone)]
pub struct DiagSink {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for DiagSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagSink").field("instance", &self.shared.instance).finish_non_exhaustive()
    }
}

static NEXT_INSTANCE: AtomicU32 = AtomicU32::new(0);
static PROCESS_SINK: OnceLock<DiagSink> = OnceLock::new();
static HOOK_INSTALLED: OnceLock<()> = OnceLock::new();
static PANIC_RECORDS: AtomicU32 = AtomicU32::new(0);
static PROCESS_BIRTH: OnceLock<u64> = OnceLock::new();

/// When this process first used its diagnostics, in milliseconds since the Unix epoch. Observed by the host, so it
/// is not the kernel's process creation time; with the PID it tells runs apart, and it is not the process-birth
/// binding of GUI-DESKTOP-050, which stays the host adapter's.
fn process_birth_observed() -> u64 {
    *PROCESS_BIRTH.get_or_init(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| u64::try_from(d.as_millis()).ok()).unwrap_or(0)
    })
}

impl DiagSink {
    /// A sink over `dir` with the default bounds. It tries to open its first file now; a failure is state, not an
    /// error.
    pub fn open(dir: &Path) -> Self {
        Self::open_with(dir, DiagLimits::default())
    }
    pub fn open_with(dir: &Path, limits: DiagLimits) -> Self {
        Self::build(dir, limits, open_file)
    }
    fn build(dir: &Path, limits: DiagLimits, opener: Opener) -> Self {
        let sink = Self {
            shared: Arc::new(Shared {
                dir: dir.to_path_buf(),
                limits,
                instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                started: Instant::now(),
                inner: Mutex::new(Inner {
                    opener,
                    file: None,
                    file_bytes: 0,
                    segment: 0,
                    torn_line: false,
                    consecutive_failures: 0,
                    stopped: None,
                    failure: None,
                    seq: 0,
                    lines_since_check: 0,
                    lines_written: 0,
                    lines_dropped: 0,
                    write_failures: 0,
                    rotations: 0,
                    files_removed: 0,
                    remove_failures: 0,
                }),
            }),
        };
        let mut inner = sink.lock();
        inner.open_segment(&sink.shared);
        drop(inner);
        sink
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub fn dir(&self) -> &Path {
        &self.shared.dir
    }
    pub fn state(&self) -> DiagState {
        let inner = self.lock();
        let (status, code) = match (inner.stopped, inner.failure) {
            (Some(code), _) => ("stopped", Some(code)),
            (None, Some(code)) => ("unavailable", Some(code)),
            (None, None) => ("active", None),
        };
        DiagState {
            status,
            code,
            lines_written: inner.lines_written,
            lines_dropped: inner.lines_dropped,
            write_failures: inner.write_failures,
            rotations: inner.rotations,
            files_removed: inner.files_removed,
            remove_failures: inner.remove_failures,
        }
    }
    fn record(&self, kind: &'static str, fields: Map<String, Value>) {
        let mut inner = self.lock();
        inner.record(&self.shared, kind, fields);
    }
    /// One observe frame as it was delivered. It is decoded here, so what is written is what the wire defines; a frame
    /// that does not decode whole is `frame_refused` with none of its bytes.
    pub fn record_frame(&self, class: DeliveryClass, bytes: &[u8]) {
        let class = match class {
            DeliveryClass::Progress => "progress",
            DeliveryClass::Terminal => "terminal",
        };
        match decode_frame(bytes) {
            Ok(mut fields) => {
                fields.insert("class".into(), json!(class));
                self.record("frame", fields);
            }
            Err(reason) => self.record("frame_refused", refused_fields(class, reason, bytes.len())),
        }
    }
    pub fn record_op(&self, line: &OpLine<'_>) {
        let mut fields = Map::new();
        fields.insert("op".into(), token_or_null(line.op));
        fields.insert("n".into(), json!(line.n));
        fields.insert("ok".into(), json!(line.ok));
        fields.insert("revision".into(), json!(line.revision));
        fields.insert("frames".into(), json!(line.frames));
        fields.insert("error".into(), line.error.map_or(Value::Null, token_or_null));
        fields.insert("correlation_id".into(), json!(line.correlation));
        fields.insert("command_id".into(), line.command_id.map_or(Value::Null, token_or_null));
        self.record("op", fields);
    }
    /// The start of one command of this process. `command` is a name of the host's closed table.
    pub fn record_run_start(&self, command: &str) {
        let mut fields = Map::new();
        fields.insert("command".into(), token_or_null(command));
        self.record("run_start", fields);
    }
    /// The end of one command, with its exit code. A process that dies first has none: see the module docs.
    pub fn record_run_end(&self, command: &str, exit: i32) {
        let mut fields = Map::new();
        fields.insert("command".into(), token_or_null(command));
        fields.insert("exit".into(), json!(exit));
        let mut inner = self.lock();
        // The last line of a run is the one worth a check: a directory removed since the last check comes back here.
        inner.lines_since_check = LINES_PER_PRESENCE_CHECK;
        inner.record(&self.shared, "run_end", fields);
    }
    /// A probe line for `health`; whether it reached the disk is in [`DiagSink::state`].
    pub fn record_health(&self) {
        self.record("health", Map::new());
    }
    fn file_name(&self) -> String {
        segment_name(self.shared.instance, self.lock().segment)
    }
}

fn segment_name(instance: u32, segment: u32) -> String {
    format!("studio-diag-{}-{}-{instance}-{segment}.jsonl", process_birth_observed(), std::process::id())
}
fn panic_name() -> String {
    format!("studio-panic-{}-{}.jsonl", process_birth_observed(), std::process::id())
}

impl Inner {
    fn record(&mut self, shared: &Shared, kind: &'static str, fields: Map<String, Value>) {
        if self.stopped.is_some() {
            self.lines_dropped = self.lines_dropped.saturating_add(1);
            return;
        }
        let timestamp = u64::try_from(shared.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut body = render_body(kind, timestamp, fields);
        if body.len().saturating_add(SEQ_ROOM) >= MAX_LINE_BYTES {
            let len = body.len();
            body = render_body("frame_refused", timestamp, refused_fields(kind, "line_limit", len));
        }
        // The file is settled first: a rotation writes lines of its own, and seq is the order of the lines.
        self.prepare(shared, body.len().saturating_add(SEQ_ROOM));
        self.seq = self.seq.saturating_add(1);
        let text = with_seq(self.seq, &body);
        if self.stopped.is_none() && self.put(&text) {
            self.lines_written = self.lines_written.saturating_add(1);
        } else {
            self.lines_dropped = self.lines_dropped.saturating_add(1);
        }
    }

    /// A file with room for a line of `len` bytes, if one can be had.
    fn prepare(&mut self, shared: &Shared, len: usize) {
        self.lines_since_check = self.lines_since_check.saturating_add(1);
        if self.lines_since_check >= LINES_PER_PRESENCE_CHECK {
            self.lines_since_check = 0;
            self.check_present(shared);
        }
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        let needed = self.file_bytes.saturating_add(len).saturating_add(SEGMENT_END_RESERVE);
        if self.file.is_some() && needed > shared.limits.max_file_bytes {
            self.rotate(shared);
        }
        if self.file.is_none() {
            self.open_segment(shared);
        }
    }

    /// One write of one line. A failure may leave part of a line behind: the next line starts on a new one.
    fn put(&mut self, text: &str) -> bool {
        let Some(file) = self.file.as_mut() else { return false };
        let torn = self.torn_line;
        let written = if torn { file.write_all(b"\n") } else { Ok(()) }.and_then(|()| file.write_all(text.as_bytes()));
        match written {
            Ok(()) => {
                let len = u64::try_from(text.len()).unwrap_or(u64::MAX);
                self.file_bytes = self.file_bytes.saturating_add(len).saturating_add(u64::from(torn));
                self.torn_line = false;
                self.consecutive_failures = 0;
                self.failure = None;
                true
            }
            Err(_) => {
                self.torn_line = true;
                self.file = None;
                self.fail("write_failed");
                false
            }
        }
    }

    fn fail(&mut self, code: &'static str) {
        self.write_failures = self.write_failures.saturating_add(1);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.failure = Some(code);
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            self.stopped = Some(code);
            self.file = None;
        }
    }

    /// A file or directory removed under us is seen here, not by the write, which goes on to the removed file.
    fn check_present(&mut self, shared: &Shared) {
        if self.file.is_some() && !shared.dir.join(segment_name(shared.instance, self.segment)).is_file() {
            self.file = None;
            self.file_bytes = 0;
        }
    }

    fn rotate(&mut self, shared: &Shared) {
        self.seq = self.seq.saturating_add(1);
        let timestamp = u64::try_from(shared.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let end = render("segment_end", self.seq, timestamp, Map::new());
        if self.put(&end) {
            self.lines_written = self.lines_written.saturating_add(1);
        }
        self.file = None;
        self.file_bytes = 0;
        self.segment = self.segment.saturating_add(1);
        self.rotations = self.rotations.saturating_add(1);
    }

    fn open_segment(&mut self, shared: &Shared) {
        if fs::create_dir_all(&shared.dir).is_err() {
            self.fail("dir_unavailable");
            return;
        }
        if !self.make_room(shared) {
            self.stopped = Some("prune_failed");
            return;
        }
        match (self.opener)(&shared.dir.join(segment_name(shared.instance, self.segment))) {
            Ok((file, len)) => {
                self.file = Some(file);
                self.file_bytes = len;
                if len == 0 {
                    self.seq = self.seq.saturating_add(1);
                    let mut fields = Map::new();
                    fields.insert("process_id".into(), json!(std::process::id()));
                    fields.insert("process_birth_observed_unix_ms".into(), json!(process_birth_observed()));
                    fields.insert("instance".into(), json!(shared.instance));
                    fields.insert("segment".into(), json!(self.segment));
                    let timestamp = u64::try_from(shared.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    let start = render("segment_start", self.seq, timestamp, fields);
                    if self.put(&start) {
                        self.lines_written = self.lines_written.saturating_add(1);
                    }
                } else {
                    self.failure = None;
                }
            }
            Err(_) => self.fail("dir_unavailable"),
        }
    }

    /// Removes our oldest files until one more file and a panic record fit; false when they still would not.
    fn make_room(&mut self, shared: &Shared) -> bool {
        // A directory that cannot be listed cannot be kept inside its bound, so nothing is written to it.
        if fs::read_dir(&shared.dir).is_err() {
            return false;
        }
        let mut ours = list_ours(&shared.dir);
        ours.sort();
        let allowed = shared.limits.max_files.saturating_sub(2);
        let current = segment_name(shared.instance, self.segment);
        let mut left = ours.len();
        for name in ours.iter().map(|o| &o.name) {
            if left <= allowed {
                break;
            }
            if *name == current {
                continue;
            }
            let path = shared.dir.join(name);
            let plain = fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
            if plain && fs::remove_file(&path).is_ok() {
                self.files_removed = self.files_removed.saturating_add(1);
                left = left.saturating_sub(1);
            } else {
                self.remove_failures = self.remove_failures.saturating_add(1);
            }
        }
        left <= allowed
    }
}

fn render_body(kind: &'static str, timestamp: u64, mut fields: Map<String, Value>) -> String {
    fields.insert("schema".into(), json!(DIAG_SCHEMA));
    fields.insert("kind".into(), json!(kind));
    fields.insert("timestamp_nanos".into(), json!(timestamp));
    serde_json::to_string(&Value::Object(fields)).unwrap_or_default()
}
/// One line: `seq` goes in front of a rendered body, newline included.
fn with_seq(seq: u64, body: &str) -> String {
    format!("{{\"seq\":{seq},{}\n", body.strip_prefix('{').unwrap_or(body))
}
fn render(kind: &'static str, seq: u64, timestamp: u64, fields: Map<String, Value>) -> String {
    with_seq(seq, &render_body(kind, timestamp, fields))
}

fn refused_fields(class: &str, reason: &'static str, len: usize) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("class".into(), json!(class));
    fields.insert("reason".into(), json!(reason));
    fields.insert("len".into(), json!(len));
    fields
}

/// The token rule: a short name of letters, digits and `_ . : -`, so a path, a sentence or a document name never passes.
fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TOKEN_BYTES
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}
fn token_or_null(text: &str) -> Value {
    if is_token(text) { json!(text) } else { Value::Null }
}

// ----- the observe wire ---------------------------------------------------------------------------------------

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], &'static str> {
        let (head, tail) = self.0.split_at_checked(count).ok_or("truncated")?;
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(*self.take(1)?.first().ok_or("truncated")?)
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| "truncated")?))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| "truncated")?))
    }
    fn i64(&mut self) -> Result<i64, &'static str> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().map_err(|_| "truncated")?))
    }
    fn string(&mut self) -> Result<&'a str, &'static str> {
        let count = u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| "truncated")?);
        std::str::from_utf8(self.take(usize::from(count))?).map_err(|_| "not_utf8")
    }
    fn named_u32(&mut self, out: &mut Map<String, Value>, names: &[&str]) -> Result<(), &'static str> {
        for name in names {
            out.insert((*name).into(), json!(self.u32()?));
        }
        Ok(())
    }
    fn named_u64(&mut self, out: &mut Map<String, Value>, names: &[&str]) -> Result<(), &'static str> {
        for name in names {
            out.insert((*name).into(), json!(self.u64()?));
        }
        Ok(())
    }
}

/// Reads one frame whole per the wire in observe's descriptor (versions 1 to 5): an unknown tag, a length past the end
/// or a trailing byte refuses the frame. The bytes are checked against [`MAX_FRAME_BYTES`] before anything is read.
fn decode_frame(bytes: &[u8]) -> Result<Map<String, Value>, &'static str> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame_limit");
    }
    let mut c = Cursor(bytes);
    if c.take(4)? != b"HSKO" {
        return Err("bad_magic");
    }
    let version = c.u8()?;
    let outcome_wire = c.u8()?;
    let failure_wire = c.u8()?;
    let progress_flag = c.u8()?;
    let outcome = match (outcome_wire, failure_wire) {
        (1, 0) => Outcome::Progress,
        (2, 0) => Outcome::Success,
        (3, 1) => Outcome::Failure(FailureCode::Validation),
        (3, 2) => Outcome::Failure(FailureCode::Unavailable),
        (3, 3) => Outcome::Failure(FailureCode::Loss),
        (3, 4) => Outcome::Failure(FailureCode::Unsupported),
        (4, 0) => Outcome::Canceled,
        _ => return Err("bad_outcome"),
    };
    if progress_flag != u8::from(outcome == Outcome::Progress) {
        return Err("bad_progress");
    }
    let (correlation, revision, sequence, completed, total) = (c.u64()?, c.u64()?, c.u64()?, c.u64()?, c.u64()?);
    let (code, phase, severity) = ring_of_outcome(outcome);
    let mut out = Map::new();
    out.insert("wire".into(), json!(version));
    out.insert("event_code".into(), json!(code.as_u16()));
    out.insert("code".into(), json!(format!("{code:?}")));
    out.insert("phase_marker".into(), json!(phase.as_u8()));
    out.insert("severity".into(), json!(severity.as_u8()));
    out.insert("correlation_id".into(), json!(correlation));
    out.insert("revision".into(), json!(revision));
    out.insert("sequence_id".into(), json!(sequence));
    // As the ring record's table: delivered frame bytes, revision.
    out.insert("counter_a".into(), json!(bytes.len()));
    out.insert("counter_b".into(), json!(revision));
    if outcome == Outcome::Progress {
        out.insert("progress_completed".into(), json!(completed));
        out.insert("progress_total".into(), json!(total));
    }
    for name in ["resource_id", "account_id", "principal_id", "owner_account_id", "owner_principal_id", "access_space_id", "session_id"] {
        out.insert(name.into(), json!(c.string()?));
    }
    let detail = match version {
        1 => None,
        2 => Some(detail_v2(&mut c)?),
        3 => Some(detail_v3(&mut c)?),
        4 => Some(detail_v4(&mut c)?),
        5 => Some(detail_v5(&mut c)?),
        _ => return Err("bad_version"),
    };
    if !c.0.is_empty() {
        return Err("trailing_bytes");
    }
    if let Some(detail) = detail {
        out.insert("detail".into(), Value::Object(detail));
    }
    Ok(out)
}

fn detail_v2(c: &mut Cursor<'_>) -> Result<Map<String, Value>, &'static str> {
    let mut d = Map::new();
    d.insert("disposition".into(), json!(c.u8()?));
    d.insert("code".into(), json!(c.u8()?));
    let address = c.u8()?;
    c.named_u32(&mut d, &["attempted_reads", "attempted_writes", "admitted_reads", "admitted_writes", "changed_writes"])?;
    match address {
        0 => {}
        1 => {
            let (target, property) = (c.u8()?, c.u8()?);
            d.insert("address".into(), json!({"target":target,"property":property,"id":c.string()?}));
        }
        _ => return Err("bad_tag"),
    }
    Ok(d)
}

fn detail_v3(c: &mut Cursor<'_>) -> Result<Map<String, Value>, &'static str> {
    let mut d = Map::new();
    d.insert("disposition".into(), json!(c.u8()?));
    d.insert("code".into(), json!(c.u8()?));
    let address = c.u8()?;
    c.named_u64(&mut d, &["epoch"])?;
    c.named_u32(&mut d, &["tiles_attempted", "tiles_admitted", "tiles_changed", "tile_limit"])?;
    c.named_u64(&mut d, &["output_bytes", "scratch_bytes", "retained_preimage_bytes", "byte_limit"])?;
    match address {
        0 => {}
        1 => {
            let property = c.u8()?;
            let (layer_id, object_key) = (c.string()?, c.string()?);
            d.insert("address".into(), json!({"property":property,"layer_id":layer_id,"object_key":object_key,"column":c.i64()?,"row":c.i64()?}));
        }
        _ => return Err("bad_tag"),
    }
    Ok(d)
}

fn detail_v4(c: &mut Cursor<'_>) -> Result<Map<String, Value>, &'static str> {
    let mut d = Map::new();
    d.insert("disposition".into(), json!(c.u8()?));
    d.insert("code".into(), json!(c.u8()?));
    let flags = c.u8()?;
    if flags & 0xE0 != 0 {
        return Err("bad_tag");
    }
    c.named_u64(&mut d, &["input_anchors", "input_segments", "output_vertices", "output_segments", "output_regions", "output_loops", "intersections", "work_units"])?;
    c.named_u64(&mut d, &["requested_owned", "precharged_input", "retained_owned"])?;
    c.named_u64(&mut d, &["delivery_attempts", "count_limit", "work_limit", "byte_limit", "delivery_limit"])?;
    for (mask, name) in [(0x04u8, "read_revision"), (0x08, "target_revision"), (0x10, "cancel_epoch")] {
        if flags & mask != 0 {
            d.insert(name.into(), json!(c.u64()?));
        }
    }
    if flags & 1 != 0 {
        let property = c.u8()?;
        let (layer_id, object_key) = (c.string()?, c.string()?);
        d.insert("address".into(), json!({"property":property,"layer_id":layer_id,"object_key":object_key}));
    }
    if flags & 2 != 0 {
        d.insert("source_path_id".into(), json!(c.string()?));
    }
    Ok(d)
}

fn detail_v5(c: &mut Cursor<'_>) -> Result<Map<String, Value>, &'static str> {
    let mut d = Map::new();
    d.insert("disposition".into(), json!(c.u8()?));
    d.insert("code".into(), json!(c.u8()?));
    d.insert("result".into(), json!(c.u8()?));
    let flags = c.u8()?;
    if flags & 0xE0 != 0 {
        return Err("bad_tag");
    }
    c.named_u64(&mut d, &["input_utf8_bytes", "input_scalars", "fonts", "runs", "glyphs", "fallback_attempts", "work_units", "retained_generations"])?;
    c.named_u64(&mut d, &["current_requested_owned", "peak_requested_owned", "retained_requested_owned", "delivery_attempts"])?;
    c.named_u64(&mut d, &["limit_input_utf8_bytes", "limit_input_scalars", "limit_fonts", "limit_runs", "limit_glyphs", "limit_fallback_attempts", "limit_work_units", "limit_retained_generations"])?;
    c.named_u64(&mut d, &["byte_limit", "delivery_limit"])?;
    for (mask, name) in [(0x02u8, "source_revision"), (0x04, "resolver_revision"), (0x08, "cancel_epoch"), (0x10, "target_revision")] {
        if flags & mask != 0 {
            d.insert(name.into(), json!(c.u64()?));
        }
    }
    if flags & 1 != 0 {
        let property = c.u8()?;
        d.insert("address".into(), json!({"property":property,"id":c.string()?}));
    }
    Ok(d)
}

// ----- our files ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Ours {
    /// birth, pid, instance, segment; a panic file sorts by birth and pid.
    key: [u64; 4],
    name: String,
    panic: bool,
}

/// Exactly our names, every number field all digits: anything else in the directory is not ours.
fn parse_ours(name: &str) -> Option<Ours> {
    let stem = name.strip_suffix(".jsonl")?;
    let number = |text: &str| -> Option<u64> {
        // As we write them: no leading zeros.
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) || (text.len() > 1 && text.starts_with('0')) {
            return None;
        }
        text.parse().ok()
    };
    if let Some(rest) = stem.strip_prefix("studio-diag-") {
        let mut parts = rest.split('-');
        let key = [number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?];
        return parts.next().is_none().then(|| Ours { key, name: name.to_string(), panic: false });
    }
    if let Some(rest) = stem.strip_prefix("studio-panic-") {
        let mut parts = rest.split('-');
        let key = [number(parts.next()?)?, number(parts.next()?)?, 0, 0];
        return parts.next().is_none().then(|| Ours { key, name: name.to_string(), panic: true });
    }
    None
}

fn list_ours(dir: &Path) -> Vec<Ours> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries.take(MAX_SCAN_ENTRIES).flatten().filter_map(|e| e.file_name().to_str().and_then(parse_ours)).collect()
}

/// The runs of a diag dir, read back. Bounded: at most [`MAX_SCAN_ENTRIES`] entries and the last
/// [`SURVEY_TAIL_BYTES`] of at most [`MAX_SURVEYED_RUNS`] files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Survey {
    pub files: u64,
    pub bytes: u64,
    pub panic_files: u64,
    /// Runs other than `own` whose newest file does not end in `run_end`: a process that died, or one still running.
    pub unterminated_runs: u64,
    pub runs_surveyed: u64,
}
impl Survey {
    pub fn to_json(self) -> Value {
        json!({
            "files": self.files,
            "bytes": self.bytes,
            "panic_files": self.panic_files,
            "unterminated_or_running": self.unterminated_runs,
            "runs_surveyed": self.runs_surveyed,
        })
    }
}

/// Reads `dir`, leaving out the run of the sink `own`.
pub fn survey(dir: &Path, own: Option<&DiagSink>) -> Survey {
    let mut ours = list_ours(dir);
    ours.sort();
    let mut result = Survey::default();
    for one in &ours {
        result.files = result.files.saturating_add(1);
        result.bytes = result.bytes.saturating_add(fs::metadata(dir.join(&one.name)).map_or(0, |m| m.len()));
        if one.panic {
            result.panic_files = result.panic_files.saturating_add(1);
        }
    }
    let own_key = own.map(|sink| {
        let name = sink.file_name();
        parse_ours(&name).map_or([0; 3], |o| [o.key[0], o.key[1], o.key[2]])
    });
    // Newest segment of each run, newest run first.
    let mut seen: Vec<[u64; 3]> = Vec::new();
    for one in ours.iter().rev().filter(|o| !o.panic) {
        let run = [one.key[0], one.key[1], one.key[2]];
        if seen.contains(&run) || Some(run) == own_key {
            continue;
        }
        if seen.len() >= MAX_SURVEYED_RUNS {
            break;
        }
        seen.push(run);
        result.runs_surveyed = result.runs_surveyed.saturating_add(1);
        if last_kind(&dir.join(&one.name)).as_deref() != Some("run_end") {
            result.unterminated_runs = result.unterminated_runs.saturating_add(1);
        }
    }
    result
}

fn last_kind(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(SURVEY_TAIL_BYTES))).ok()?;
    let mut tail = Vec::new();
    file.take(SURVEY_TAIL_BYTES).read_to_end(&mut tail).ok()?;
    let line = tail.split(|b| *b == b'\n').rev().find(|l| !l.is_empty())?;
    let value: Value = serde_json::from_slice(line).ok()?;
    value["kind"].as_str().map(str::to_string)
}

// ----- the process's sink ---------------------------------------------------------------------------------------

/// Makes `sink` the one every later `Harness::new` of this process forwards to. Set once; false when one is set.
pub fn install_process_sink(sink: &DiagSink) -> bool {
    PROCESS_SINK.set(sink.clone()).is_ok()
}
/// The sink of [`install_process_sink`], if a host installed one. Nothing installs one but a host's `main`.
pub fn process_sink() -> Option<DiagSink> {
    PROCESS_SINK.get().cloned()
}

/// The diag dir of a process given no explicit one: [`DIAG_DIR_ENV`], else the OS's per-user local state directory
/// (`LOCALAPPDATA`, `XDG_STATE_HOME`, `HOME/.local/state`) under `hsk-studio/diag`, else the temp directory. There is
/// no fallback from a directory that cannot be written: the command runs on and `health` says so.
pub fn default_diag_dir() -> PathBuf {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    if let Some(dir) = set(DIAG_DIR_ENV) {
        return dir;
    }
    let state = set("LOCALAPPDATA").or_else(|| set("XDG_STATE_HOME")).or_else(|| set("HOME").map(|home| home.join(".local").join("state")));
    state.unwrap_or_else(std::env::temp_dir).join("hsk-studio").join("diag")
}

/// A host's `main` starts its diagnostics: opens the sink over `explicit` or [`default_diag_dir`], makes it the
/// process's sink and installs the panic hook. Library callers and tests that only call `cli::run` never reach this.
pub fn start_process(explicit: Option<PathBuf>) -> DiagSink {
    let dir = explicit.filter(|dir| !dir.as_os_str().is_empty()).unwrap_or_else(default_diag_dir);
    let sink = DiagSink::open(&dir);
    install_process_sink(&sink);
    install_panic_hook(&sink);
    sink
}

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Chains a hook in front of the one present: it writes a typed `panic` record, then the previous hook runs, so what
/// is printed and the exit status do not change. Set once per process. The record has where and what kind, never the
/// message, which can hold document text or a path. A panic that is later caught is recorded too: the hook runs before
/// anyone knows.
pub fn install_panic_hook(sink: &DiagSink) {
    if HOOK_INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    let sink = sink.clone();
    std::panic::set_hook(Box::new(move |info| {
        let again = IN_HOOK.try_with(|flag| flag.replace(true)).unwrap_or(true);
        if !again {
            write_panic_record(&sink, info);
            let _ = IN_HOOK.try_with(|flag| flag.set(false));
        }
        previous(info);
    }));
}

/// The last components of a source path, the drive and any user directory left behind. Dependencies compile with
/// absolute paths into the machine's registry, so the whole path can name a user.
fn source_tail(file: &str) -> String {
    let parts: Vec<&str> = file.split(['/', '\\']).filter(|p| !p.is_empty() && !p.contains(':')).collect();
    parts[parts.len().saturating_sub(3)..].join("/")
}

fn write_panic_record(sink: &DiagSink, info: &PanicHookInfo<'_>) {
    if PANIC_RECORDS.fetch_add(1, Ordering::Relaxed) >= MAX_PANIC_RECORDS {
        return;
    }
    let payload = info.payload();
    let (payload_kind, payload_len) = if let Some(text) = payload.downcast_ref::<&str>() {
        ("str", text.len())
    } else if let Some(text) = payload.downcast_ref::<String>() {
        ("string", text.len())
    } else {
        ("other", 0)
    };
    let mut fields = Map::new();
    fields.insert("process_id".into(), json!(std::process::id()));
    fields.insert("process_birth_observed_unix_ms".into(), json!(process_birth_observed()));
    fields.insert("payload_kind".into(), json!(payload_kind));
    fields.insert("payload_len".into(), json!(payload_len));
    if let Some(location) = info.location() {
        fields.insert("source".into(), json!(source_tail(location.file())));
        fields.insert("line".into(), json!(location.line()));
        fields.insert("column".into(), json!(location.column()));
    }
    // Its own file first: the sink's lock may be held by this very thread.
    let timestamp = u64::try_from(sink.shared.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let text = render("panic", 0, timestamp, fields.clone());
    let dir = sink.dir();
    // A failure leaves no record and changes nothing else.
    let _ = fs::create_dir_all(dir).and_then(|()| {
        let mut file = OpenOptions::new().create(true).append(true).open(dir.join(panic_name()))?;
        file.write_all(text.as_bytes())?;
        file.sync_data()
    });
    // Then the run's own stream, for the order, when nobody holds it.
    let inner = match sink.shared.inner.try_lock() {
        Ok(inner) => Some(inner),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    };
    if let Some(mut inner) = inner {
        inner.record(&sink.shared, "panic", fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disk fills in the middle of a line: part of it lands, then every write fails, and a fresh open does not help.
    static ROOM: AtomicU32 = AtomicU32::new(300);
    struct FullDisk;
    impl Write for FullDisk {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let room = ROOM.load(Ordering::Relaxed);
            if room == 0 {
                return Err(io::Error::new(io::ErrorKind::StorageFull, "full"));
            }
            let count = u32::try_from(bytes.len()).unwrap().min(room);
            ROOM.fetch_sub(count, Ordering::Relaxed);
            Ok(usize::try_from(count).unwrap())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn full_disk(_path: &Path) -> io::Result<Opened> {
        Ok((Box::new(FullDisk), 0))
    }
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hsk-diag-unit-{}-{label}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_full_disk_drops_lines_counts_them_and_stops_touching_the_disk() {
        let dir = scratch("full");
        let sink = DiagSink::build(&dir, DiagLimits::default(), full_disk);
        for _ in 0..1000 {
            sink.record_run_start("edit");
        }
        let state = sink.state();
        assert_eq!((state.status, state.code), ("stopped", Some("write_failed")));
        assert_eq!(state.write_failures, u64::from(MAX_CONSECUTIVE_FAILURES));
        assert_eq!(state.lines_written.saturating_add(state.lines_dropped), 1001, "every line is written or counted: {state:?}");
        assert!(state.lines_dropped > 900, "{state:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[derive(Clone, Default)]
    struct Memory(Arc<Mutex<Vec<u8>>>);
    impl Write for Memory {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A line torn by a failed write never glues onto the next one.
    #[test]
    fn a_torn_line_does_not_join_the_next_line() {
        static MEMORY: OnceLock<Memory> = OnceLock::new();
        static TORN: AtomicU32 = AtomicU32::new(0);
        struct TearOnce(Memory);
        impl Write for TearOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if TORN.load(Ordering::Relaxed) == 0 && bytes.len() > 40 && self.0.0.lock().unwrap().len() > 100 {
                    // Part of the line lands, then the write fails.
                    self.0.write_all(&bytes[..20])?;
                    TORN.store(1, Ordering::Relaxed);
                    return Err(io::Error::new(io::ErrorKind::StorageFull, "full"));
                }
                self.0.write(bytes)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        fn opener(_path: &Path) -> io::Result<Opened> {
            let memory = MEMORY.get_or_init(Memory::default).clone();
            let len = u64::try_from(memory.0.lock().unwrap().len()).unwrap();
            Ok((Box::new(TearOnce(memory)), len))
        }
        let dir = scratch("torn");
        let sink = DiagSink::build(&dir, DiagLimits::default(), opener);
        for _ in 0..8 {
            sink.record_run_start("edit");
        }
        let text = String::from_utf8(MEMORY.get().unwrap().0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let broken = lines.iter().filter(|l| serde_json::from_str::<Value>(l).is_err()).count();
        assert_eq!(broken, 1, "only the torn line itself fails to parse: {text}");
        assert_eq!(sink.state().status, "active");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_path_keeps_its_tail_and_loses_the_machine() {
        let drive = format!("{}{}", 'X', ':');
        let absolute = format!("{drive}\\home-of-someone\\.cargo\\registry\\src\\egui-0.33.3\\src\\lib.rs");
        assert_eq!(source_tail(&absolute), "egui-0.33.3/src/lib.rs");
        assert_eq!(source_tail("crates/hsk-studio-layout/src/compose.rs"), "hsk-studio-layout/src/compose.rs");
        assert_eq!(source_tail(&format!("{drive}/a.rs")), "a.rs");
    }

    #[test]
    fn the_token_rule_refuses_what_is_not_a_name() {
        assert!(is_token("edit.rename") && is_token("chronicle.patch") && is_token("SCMD-019abcde-0000"));
        for text in ["", "a b", "a/b", "a\\b", "a\"b", "naïve", &"x".repeat(65)] {
            assert!(!is_token(text), "{text}");
        }
    }
}
