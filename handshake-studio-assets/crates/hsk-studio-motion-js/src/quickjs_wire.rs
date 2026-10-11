//! Length-prefixed, versioned binary protocol between the host-side `WorkerEngine` and the
//! `motion-js-worker` process (stdin/stdout, little-endian, no third-party serializer).
//!
//! Frame: `u32 length` + payload, length bounded by a caller-supplied cap BEFORE any allocation.
//! Payload: `u8 kind, u16 protocol version, u64 request id` + body. A worker speaks first with a
//! HELLO frame (version + engine identity); every reply echoes the id of its request, so a stale or
//! mismatched frame is a protocol fault. All decoding is bounds-checked; a malformed frame never
//! panics and never allocates beyond the bytes actually present in the frame.
#![deny(clippy::arithmetic_side_effects)]
use crate::{
    engine::{BudgetKind, EngineFailure, JsValue, ThrownKind},
    quickjs::{ENGINE_VERSION, Input, LimitReadback, Pending, ReadKey, RunEnd, RunReport},
};
use hsk_studio_motion::{ExprError, ExprErrorCode, MAX_PATH_BYTES, SourceSpan};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
};

pub(crate) const PROTOCOL_VERSION: u16 = 1;
/// Largest request frame (source, host-read cache) either side accepts.
pub(crate) const MAX_REQUEST_FRAME: usize = 32 << 20;
pub(crate) const HELLO_CAP: usize = 1024;

const KIND_HELLO: u8 = 1;
const KIND_EVAL: u8 = 2;
const KIND_VERIFY: u8 = 3;
const KIND_REPORT: u8 = 4;
const KIND_LIMITS: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    /// End of stream or I/O failure: the peer is gone.
    Closed,
    /// A declared frame length above the cap (nothing was allocated or read).
    TooLarge,
    /// Wrong kind, version, id, truncated or inconsistent content.
    Malformed,
}

pub(crate) fn read_frame(reader: &mut impl Read, cap: usize) -> Result<Vec<u8>, WireError> {
    let mut prefix = [0_u8; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|_| WireError::Closed)?;
    let length = usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| WireError::TooLarge)?;
    if length > cap {
        return Err(WireError::TooLarge);
    }
    let mut payload = Vec::new();
    let wanted = u64::try_from(length).map_err(|_| WireError::TooLarge)?;
    let got = reader
        .take(wanted)
        .read_to_end(&mut payload)
        .map_err(|_| WireError::Closed)?;
    if got != length {
        return Err(WireError::Closed);
    }
    Ok(payload)
}

pub(crate) fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(payload.len().saturating_add(4));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    writer.write_all(&frame)?;
    writer.flush()
}

struct Out(Vec<u8>);

impl Out {
    fn header(kind: u8, id: u64) -> Self {
        let mut out = Self(Vec::with_capacity(64));
        out.u8(kind);
        out.0.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        out.u64(id);
        out
    }

    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn f64(&mut self, value: f64) {
        self.0.extend_from_slice(&value.to_bits().to_le_bytes());
    }

    fn count(&mut self, value: usize) {
        self.u32(u32::try_from(value).unwrap_or(u32::MAX));
    }

    fn text(&mut self, value: &str) {
        self.count(value.len());
        self.0.extend_from_slice(value.as_bytes());
    }

    fn opt_i64(&mut self, value: Option<i64>) {
        self.u8(u8::from(value.is_some()));
        self.i64(value.unwrap_or(0));
    }

    fn span(&mut self, span: Option<SourceSpan>) {
        self.u8(u8::from(span.is_some()));
        let span = span.unwrap_or(SourceSpan { line: 0, column: 0 });
        self.u32(span.line);
        self.u32(span.column);
    }
}

struct In<'a> {
    buf: &'a [u8],
    pos: usize,
}

type Res<T> = Result<T, WireError>;

impl<'a> In<'a> {
    /// Validates kind and version, returns the request id and a reader over the body.
    fn open(frame: &'a [u8], kind: u8) -> Res<(u64, Self)> {
        let mut reader = Self { buf: frame, pos: 0 };
        if reader.u8()? != kind || reader.u16()? != PROTOCOL_VERSION {
            return Err(WireError::Malformed);
        }
        let id = reader.u64()?;
        Ok((id, reader))
    }

    fn take(&mut self, count: usize) -> Res<&'a [u8]> {
        let end = self.pos.checked_add(count).ok_or(WireError::Malformed)?;
        let slice = self.buf.get(self.pos..end).ok_or(WireError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Res<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| WireError::Malformed)
    }

    fn u8(&mut self) -> Res<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Res<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Res<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Res<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Res<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn f64(&mut self) -> Res<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// An element count that cannot exceed the bytes still present (`each` bytes per element).
    fn count(&mut self, each: usize) -> Res<usize> {
        let count = usize::try_from(self.u32()?).map_err(|_| WireError::Malformed)?;
        if count
            .checked_mul(each.max(1))
            .is_none_or(|bytes| bytes > self.buf.len().saturating_sub(self.pos))
        {
            return Err(WireError::Malformed);
        }
        Ok(count)
    }

    fn text(&mut self) -> Res<String> {
        let length = self.count(1)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| WireError::Malformed)
    }

    fn flag(&mut self) -> Res<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::Malformed),
        }
    }

    fn opt_i64(&mut self) -> Res<Option<i64>> {
        let present = self.flag()?;
        let value = self.i64()?;
        Ok(present.then_some(value))
    }

    fn span(&mut self) -> Res<Option<SourceSpan>> {
        let present = self.flag()?;
        let (line, column) = (self.u32()?, self.u32()?);
        Ok(present.then_some(SourceSpan { line, column }))
    }

    fn finish(&self) -> Res<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(WireError::Malformed)
        }
    }
}

const BUDGETS: [BudgetKind; BudgetKind::COUNT] = [
    BudgetKind::Fuel,
    BudgetKind::Memory,
    BudgetKind::Stack,
    BudgetKind::Recursion,
    BudgetKind::Deadline,
    BudgetKind::HostReads,
    BudgetKind::SourceBytes,
    BudgetKind::Realms,
    BudgetKind::Cpu,
];

const EXPR_CODES: [ExprErrorCode; 9] = [
    ExprErrorCode::Parse,
    ExprErrorCode::Unsupported,
    ExprErrorCode::Alias,
    ExprErrorCode::ReferenceMissing,
    ExprErrorCode::Ambiguous,
    ExprErrorCode::DependencyCycle,
    ExprErrorCode::Budget,
    ExprErrorCode::OutputType,
    ExprErrorCode::Canceled,
];

fn index_of<T: PartialEq + Copy>(table: &[T], value: T) -> u8 {
    table
        .iter()
        .position(|item| *item == value)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(u8::MAX)
}

fn encode_failure(out: &mut Out, failure: &EngineFailure) {
    match failure {
        EngineFailure::Parse(span) => {
            out.u8(0);
            out.span(*span);
        }
        EngineFailure::Unsupported(span) => {
            out.u8(1);
            out.span(*span);
        }
        EngineFailure::Thrown { kind, span } => {
            out.u8(2);
            out.u8(u8::from(*kind == ThrownKind::Other));
            out.span(*span);
        }
        EngineFailure::Budget(kind) => {
            out.u8(3);
            out.u8(index_of(&BUDGETS, *kind));
        }
        EngineFailure::Output => out.u8(4),
        EngineFailure::Host(error) => {
            out.u8(5);
            out.u8(index_of(&EXPR_CODES, error.code));
            out.span(error.span);
        }
        EngineFailure::Canceled => out.u8(6),
        // Isolation faults are produced by the host, never by a worker: encode as internal.
        EngineFailure::Internal | EngineFailure::Faulted(_) => out.u8(7),
    }
}

fn decode_failure(input: &mut In<'_>) -> Res<EngineFailure> {
    Ok(match input.u8()? {
        0 => EngineFailure::Parse(input.span()?),
        1 => EngineFailure::Unsupported(input.span()?),
        2 => {
            let kind = if input.flag()? {
                ThrownKind::Other
            } else {
                ThrownKind::Reference
            };
            EngineFailure::Thrown {
                kind,
                span: input.span()?,
            }
        }
        3 => EngineFailure::Budget(
            *BUDGETS
                .get(usize::from(input.u8()?))
                .ok_or(WireError::Malformed)?,
        ),
        4 => EngineFailure::Output,
        5 => {
            let code = *EXPR_CODES
                .get(usize::from(input.u8()?))
                .ok_or(WireError::Malformed)?;
            EngineFailure::Host(ExprError {
                code,
                span: input.span()?,
            })
        }
        6 => EngineFailure::Canceled,
        7 => EngineFailure::Internal,
        _ => return Err(WireError::Malformed),
    })
}

pub(crate) fn encode_hello() -> Vec<u8> {
    let mut out = Out::header(KIND_HELLO, 0);
    out.text(ENGINE_VERSION);
    out.0
}

pub(crate) fn decode_hello(frame: &[u8]) -> Res<()> {
    let (_, mut input) = In::open(frame, KIND_HELLO)?;
    if input.text()? != ENGINE_VERSION {
        return Err(WireError::Malformed);
    }
    input.finish()
}

/// Owned decoded evaluation request (worker side).
pub(crate) struct EvalRequest {
    pub(crate) memory_bytes: u64,
    pub(crate) stack_bytes: u64,
    pub(crate) allowance: u64,
    pub(crate) remaining_ns: u64,
    pub(crate) max_nodes: u64,
    pub(crate) utc: Option<i64>,
    pub(crate) source: String,
    pub(crate) cache: HashMap<ReadKey, Vec<f64>>,
}

pub(crate) enum Request {
    Eval(u64, EvalRequest),
    Verify(u64, u64, u64),
}

pub(crate) fn encode_eval(id: u64, input: &Input<'_>, remaining_ns: u64) -> Vec<u8> {
    let mut out = Out::header(KIND_EVAL, id);
    out.u64(u64::try_from(input.memory_bytes).unwrap_or(u64::MAX));
    out.u64(u64::try_from(input.stack_bytes).unwrap_or(u64::MAX));
    out.u64(input.allowance);
    out.u64(remaining_ns);
    out.u64(u64::try_from(input.max_nodes).unwrap_or(u64::MAX));
    out.opt_i64(input.utc);
    out.text(input.source);
    out.count(input.cache.len());
    for (key, values) in input.cache {
        out.text(&key.path);
        out.opt_i64(key.tick);
        out.count(values.len());
        for value in values {
            out.f64(*value);
        }
    }
    out.0
}

pub(crate) fn encode_verify(id: u64, memory_bytes: u64, stack_bytes: u64) -> Vec<u8> {
    let mut out = Out::header(KIND_VERIFY, id);
    out.u64(memory_bytes);
    out.u64(stack_bytes);
    out.0
}

pub(crate) fn decode_request(frame: &[u8]) -> Res<Request> {
    match frame.first() {
        Some(&KIND_EVAL) => {
            let (id, mut input) = In::open(frame, KIND_EVAL)?;
            let memory_bytes = input.u64()?;
            let stack_bytes = input.u64()?;
            let allowance = input.u64()?;
            let remaining_ns = input.u64()?;
            let max_nodes = input.u64()?;
            let utc = input.opt_i64()?;
            let source = input.text()?;
            let entries = input.count(1)?;
            let mut cache = HashMap::with_capacity(entries.min(1024));
            for _ in 0..entries {
                let path = input.text()?;
                let tick = input.opt_i64()?;
                let length = input.count(8)?;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(input.f64()?);
                }
                cache.insert(ReadKey { path, tick }, values);
            }
            input.finish()?;
            Ok(Request::Eval(
                id,
                EvalRequest {
                    memory_bytes,
                    stack_bytes,
                    allowance,
                    remaining_ns,
                    max_nodes,
                    utc,
                    source,
                    cache,
                },
            ))
        }
        Some(&KIND_VERIFY) => {
            let (id, mut input) = In::open(frame, KIND_VERIFY)?;
            let (memory, stack) = (input.u64()?, input.u64()?);
            input.finish()?;
            Ok(Request::Verify(id, memory, stack))
        }
        _ => Err(WireError::Malformed),
    }
}

pub(crate) fn encode_report(id: u64, report: &RunReport) -> Vec<u8> {
    let mut out = Out::header(KIND_REPORT, id);
    out.u64(report.polls);
    out.u8(u8::from(report.dropped));
    match &report.end {
        RunEnd::Value(JsValue::Number(number)) => {
            out.u8(0);
            out.f64(*number);
        }
        RunEnd::Value(JsValue::Array(items)) => {
            out.u8(1);
            let numbers: Vec<f64> = items
                .iter()
                .filter_map(|item| {
                    if let JsValue::Number(n) = item {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .collect();
            out.count(numbers.len());
            for number in numbers {
                out.f64(number);
            }
        }
        RunEnd::Value(_) => {
            out.u8(2);
            encode_failure(&mut out, &EngineFailure::Output);
        }
        RunEnd::Failure(failure) => {
            out.u8(2);
            encode_failure(&mut out, failure);
        }
        RunEnd::Pending(Pending::Read(key)) => {
            out.u8(3);
            out.text(&key.path);
            out.opt_i64(key.tick);
        }
        RunEnd::Pending(Pending::Utc) => out.u8(4),
        RunEnd::Pending(Pending::Failure(failure)) => {
            out.u8(5);
            encode_failure(&mut out, failure);
        }
    }
    out.0
}

/// `max_nodes` bounds the array a worker may return (output size cap).
pub(crate) fn decode_report(frame: &[u8], expected_id: u64, max_nodes: usize) -> Res<RunReport> {
    let (id, mut input) = In::open(frame, KIND_REPORT)?;
    if id != expected_id {
        return Err(WireError::Malformed);
    }
    let polls = input.u64()?;
    let dropped = input.flag()?;
    let end = match input.u8()? {
        0 => RunEnd::Value(JsValue::Number(input.f64()?)),
        1 => {
            let length = input.count(8)?;
            if length > max_nodes {
                return Err(WireError::Malformed);
            }
            let mut numbers = Vec::with_capacity(length);
            for _ in 0..length {
                numbers.push(JsValue::Number(input.f64()?));
            }
            RunEnd::Value(JsValue::Array(numbers))
        }
        2 => RunEnd::Failure(decode_failure(&mut input)?),
        3 => RunEnd::Pending(Pending::Read(ReadKey {
            path: input.text()?,
            tick: input.opt_i64()?,
        })),
        4 => RunEnd::Pending(Pending::Utc),
        5 => RunEnd::Pending(Pending::Failure(decode_failure(&mut input)?)),
        _ => return Err(WireError::Malformed),
    };
    input.finish()?;
    Ok(RunReport {
        polls,
        end,
        dropped,
    })
}

pub(crate) fn encode_limits(id: u64, readback: Result<LimitReadback, ()>) -> Vec<u8> {
    let mut out = Out::header(KIND_LIMITS, id);
    match readback {
        Ok(readback) => {
            out.u8(1);
            out.i64(readback.memory_limit);
            out.u64(u64::try_from(readback.stack_applied).unwrap_or(u64::MAX));
        }
        Err(()) => {
            out.u8(0);
            out.i64(0);
            out.u64(0);
        }
    }
    out.0
}

pub(crate) fn decode_limits(frame: &[u8], expected_id: u64) -> Res<Option<LimitReadback>> {
    let (id, mut input) = In::open(frame, KIND_LIMITS)?;
    if id != expected_id {
        return Err(WireError::Malformed);
    }
    let ok = input.flag()?;
    let memory_limit = input.i64()?;
    let stack_applied = usize::try_from(input.u64()?).map_err(|_| WireError::Malformed)?;
    input.finish()?;
    Ok(ok.then_some(LimitReadback {
        memory_limit,
        stack_applied,
    }))
}

/// Fixed part of every reply cap: header, counters, a typed failure with its span, and a host-read
/// request (path text plus tick) all fit below it.
const REPLY_SLACK: usize = 4096;
// The realm refuses a host-read path above `MAX_PATH_BYTES` before it can reach a reply
// (`quickjs::install_host`), so a well-formed reply never carries more path text than this.
const _: () = assert!(MAX_PATH_BYTES.saturating_add(128) <= REPLY_SLACK);

/// Upper bound of a well-formed reply to `input`: an array of `max_nodes` numbers plus the fixed
/// fields. It does not depend on the source: the only other variable-size field is a host-read path,
/// which the realm bounds at `MAX_PATH_BYTES` (R7 carried PLAUSIBLE: the old bound assumed a path no
/// longer than the source, but a script can build a longer one at run time).
pub(crate) fn response_cap(input: &Input<'_>) -> usize {
    input
        .max_nodes
        .saturating_mul(8)
        .saturating_add(REPLY_SLACK)
        .min(MAX_REQUEST_FRAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "test-faults")]
    use crate::quickjs::WorkerTestFault;
    use crate::{clock::ManualClock, quickjs::Input};
    use hsk_studio_accord::CancellationToken;
    use std::sync::Arc;

    fn report(end: RunEnd) -> RunReport {
        RunReport {
            polls: 7,
            end,
            dropped: true,
        }
    }

    #[test]
    fn wire_roundtrip_and_bounds() {
        // Request: every field survives; map order is irrelevant.
        let mut cache = HashMap::new();
        cache.insert(
            ReadKey {
                path: "comp/a".to_owned(),
                tick: None,
            },
            vec![2.0],
        );
        cache.insert(
            ReadKey {
                path: "tick/x".to_owned(),
                tick: Some(-5),
            },
            vec![1.5, f64::MAX],
        );
        let input = Input {
            source: "hostRead('comp/a')[0] + 1",
            max_nodes: 4096,
            memory_bytes: 16 << 20,
            stack_bytes: 512 << 10,
            allowance: 99,
            cancel: CancellationToken::default(),
            clock: Arc::new(ManualClock::new()),
            deadline_ns: 5,
            cache: &cache,
            utc: Some(1001),
            #[cfg(feature = "test-faults")]
            fault: WorkerTestFault::None,
        };
        let Ok(Request::Eval(id, got)) = decode_request(&encode_eval(42, &input, 250_000_000))
        else {
            panic!("eval request must decode");
        };
        assert_eq!(
            (id, got.memory_bytes, got.stack_bytes, got.allowance),
            (42, 16 << 20, 512 << 10, 99)
        );
        assert_eq!(
            (got.remaining_ns, got.max_nodes, got.utc),
            (250_000_000, 4096, Some(1001))
        );
        assert_eq!(
            (got.source.as_str(), got.cache),
            (input.source, cache.clone())
        );
        assert!(matches!(
            decode_request(&encode_verify(3, 1 << 20, 1 << 16)),
            Ok(Request::Verify(3, 1048576, 65536))
        ));

        // Replies: values, typed failures with spans, pending reads; the id must match.
        for (sent, expected) in ends().into_iter().zip(ends()) {
            let frame = encode_report(8, &report(sent));
            let back = decode_report(&frame, 8, 4096).expect("reply must decode");
            assert_eq!((back.polls, back.dropped), (7, true));
            assert_eq!(end_debug(&back.end), end_debug(&expected));
            assert_eq!(
                decode_report(&frame, 9, 4096).err(),
                Some(WireError::Malformed),
                "stale id"
            );
            // Every truncation is rejected, never a panic or a partial value.
            for cut in 0..frame.len() {
                assert!(decode_report(&frame[..cut], 8, 4096).is_err(), "cut {cut}");
            }
        }
        // Output cap: a worker array above max_nodes is a protocol violation.
        let big = encode_report(
            1,
            &report(RunEnd::Value(JsValue::Array(vec![JsValue::Number(0.0); 5]))),
        );
        assert_eq!(decode_report(&big, 1, 4).err(), Some(WireError::Malformed));
        assert!(decode_report(&big, 1, 5).is_ok());

        // Frames: a hostile length above the cap is rejected before any payload is read or allocated.
        let mut huge = (u32::MAX).to_le_bytes().to_vec();
        huge.extend_from_slice(&[0; 16]);
        assert_eq!(
            read_frame(&mut huge.as_slice(), 1 << 20),
            Err(WireError::TooLarge)
        );
        let mut ok = Vec::new();
        write_frame(&mut ok, b"abc").unwrap();
        assert_eq!(read_frame(&mut ok.as_slice(), 3), Ok(b"abc".to_vec()));
        assert_eq!(read_frame(&mut ok.as_slice(), 2), Err(WireError::TooLarge));
        assert_eq!(
            read_frame(&mut ok[..5].as_ref(), 8),
            Err(WireError::Closed),
            "short payload"
        );
        assert_eq!(read_frame(&mut [].as_slice(), 8), Err(WireError::Closed));
        // Version and handshake checks.
        assert!(decode_hello(&encode_hello()).is_ok());
        let mut other_version = encode_hello();
        other_version[1] ^= 1;
        assert_eq!(decode_hello(&other_version), Err(WireError::Malformed));
        assert!(decode_request(&[KIND_EVAL, 1, 0]).is_err());
    }

    /// The reply cap holds the largest reply the realm can produce for a tiny source and the smallest
    /// output quota: a host-read request with a path of exactly `MAX_PATH_BYTES` (the realm refuses
    /// longer ones) and a typed failure with a span; it never depends on the source length.
    #[test]
    fn reply_cap_covers_the_longest_realm_reply_for_a_tiny_source() {
        let cache = HashMap::new();
        let input = Input {
            source: "1",
            max_nodes: 1,
            memory_bytes: 16 << 20,
            stack_bytes: 512 << 10,
            allowance: 1,
            cancel: CancellationToken::default(),
            clock: Arc::new(ManualClock::new()),
            deadline_ns: 5,
            cache: &cache,
            utc: None,
            #[cfg(feature = "test-faults")]
            fault: WorkerTestFault::None,
        };
        let cap = response_cap(&input);
        assert_eq!(cap, 8 + 4096, "cap = 8 bytes per output node + the fixed slack");
        let longest_read = encode_report(
            u64::MAX,
            &report(RunEnd::Pending(Pending::Read(ReadKey {
                path: "p".repeat(MAX_PATH_BYTES),
                tick: Some(i64::MIN),
            }))),
        );
        assert!(longest_read.len() <= cap, "{} > {cap}", longest_read.len());
        let failure = encode_report(
            u64::MAX,
            &report(RunEnd::Failure(EngineFailure::Host(ExprError::at(
                ExprErrorCode::Ambiguous,
                u32::MAX,
                u32::MAX,
            )))),
        );
        assert!(failure.len() <= cap, "{} > {cap}", failure.len());
    }

    fn ends() -> Vec<RunEnd> {
        let span = Some(SourceSpan { line: 3, column: 9 });
        vec![
            RunEnd::Value(JsValue::Number(3.0)),
            RunEnd::Value(JsValue::Array(vec![
                JsValue::Number(1.0),
                JsValue::Number(-0.5),
            ])),
            RunEnd::Failure(EngineFailure::Parse(span)),
            RunEnd::Failure(EngineFailure::Thrown {
                kind: ThrownKind::Reference,
                span,
            }),
            RunEnd::Failure(EngineFailure::Budget(BudgetKind::Deadline)),
            RunEnd::Failure(EngineFailure::Host(ExprError::at(
                ExprErrorCode::Ambiguous,
                1,
                2,
            ))),
            RunEnd::Pending(Pending::Read(ReadKey {
                path: "a/b".to_owned(),
                tick: Some(9),
            })),
            RunEnd::Pending(Pending::Utc),
        ]
    }

    fn end_debug(end: &RunEnd) -> String {
        match end {
            RunEnd::Value(v) => format!("value {v:?}"),
            RunEnd::Failure(f) => format!("failure {f:?}"),
            RunEnd::Pending(Pending::Read(k)) => format!("read {k:?}"),
            RunEnd::Pending(Pending::Utc) => "utc".to_owned(),
            RunEnd::Pending(Pending::Failure(f)) => format!("pending failure {f:?}"),
        }
    }
}
