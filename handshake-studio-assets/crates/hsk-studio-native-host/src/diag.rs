//! Terminal outcomes through observe (common rule 5, STU-ARC-026): codes, counters and opaque ids only, never text.
//! The caller supplies the actor, the resource id, the correlation and the sink: every host run entry
//! ([`crate::offscreen::run`], [`crate::window::run`]) takes a [`HostRun`] and sends exactly one terminal frame
//! through it (F-R60-22). [`LineSink`] is studio-vk's sink.
#![deny(clippy::arithmetic_side_effects)]
use crate::capture::Json;
use crate::error::HostError;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Error, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

/// The observe outcome of a host run.
pub fn outcome(result: Result<(), HostError>) -> Outcome {
    match result {
        Ok(()) => Outcome::Success,
        Err(HostError::Vulkan(r)) => Outcome::Failure(match r.class() {
            "loss" => FailureCode::Loss,
            "unsupported" => FailureCode::Unsupported,
            "validation" => FailureCode::Validation,
            _ => FailureCode::Unavailable,
        }),
        Err(HostError::Usage(_) | HostError::Document(_)) => Outcome::Failure(FailureCode::Validation),
        Err(HostError::FocusViolation) => Outcome::Failure(FailureCode::Validation),
        Err(HostError::Io(_) | HostError::Window(_) | HostError::Timeout) => Outcome::Failure(FailureCode::Unavailable),
    }
}

/// Emits the terminal frame of one host run (device open, window run or offscreen capture).
pub fn emit_terminal(
    actor: &ActorContext,
    resource: &DomainId,
    correlation_id: u64,
    result: Result<(), HostError>,
    cancel: &CancellationToken,
    sink: &mut dyn SinkPort,
) -> Result<Receipt, Error> {
    let mut observe = Observe::new(correlation_id, 0, resource.clone(), actor.clone(), Budget::new(0, 0)?);
    let observation =
        Observation { correlation_id, revision: 0, outcome: outcome(result), progress: None, private_project_text: None };
    observe.emit(observation, cancel, sink)
}

/// The caller's side of one host run (HOST INTEGRATION (4), (5)): who runs it, on which resource, under which
/// correlation and cancellation, and the sink its one terminal frame goes to. A run entry sends that frame;
/// [`HostRun::terminal`] then holds what its delivery returned. A delivery error never changes the run's result.
pub struct HostRun<'a> {
    observe: Observe,
    correlation_id: u64,
    cancel: &'a CancellationToken,
    sink: &'a mut dyn SinkPort,
    terminal: Option<Result<Receipt, Error>>,
}

impl<'a> HostRun<'a> {
    pub fn new(
        actor: ActorContext,
        resource: DomainId,
        correlation_id: u64,
        cancel: &'a CancellationToken,
        sink: &'a mut dyn SinkPort,
    ) -> Result<Self, Error> {
        let observe = Observe::new(correlation_id, 0, resource, actor, Budget::new(0, 0)?);
        Ok(Self { observe, correlation_id, cancel, sink, terminal: None })
    }

    /// What the delivery of this run's terminal frame returned; `None` while no frame was sent.
    pub fn terminal(&self) -> Option<Result<Receipt, Error>> {
        self.terminal
    }

    /// Sends the terminal frame of `result`. Only the first call sends: a later call sends nothing and returns
    /// observe's `Closed`, so a run has one terminal frame. The run entries call it; a host calls it itself only for a
    /// run it refused before any entry (studio-vk's usage refusals).
    pub fn finish(&mut self, result: Result<(), HostError>) -> Result<Receipt, Error> {
        self.send(outcome(result))
    }

    fn send(&mut self, outcome: Outcome) -> Result<Receipt, Error> {
        if self.terminal.is_some() {
            return Err(Error::Closed);
        }
        let observation =
            Observation { correlation_id: self.correlation_id, revision: 0, outcome, progress: None, private_project_text: None };
        let delivered = self.observe.emit(observation, self.cancel, &mut *self.sink);
        self.terminal = Some(delivered);
        delivered
    }
}

/// `schema` of every [`LineSink`] line.
pub const OBSERVE_LINE_SCHEMA: &str = "hsk.studio.native_host.observe@1";

/// One frame as one JSON line: the class, the byte count, the frame header's outcome and failure codes, correlation
/// and sequence (observe wire `HSKO` header; null when the frame is shorter or not `HSKO`), whether the run had no
/// named caller, and the frame bytes as lowercase hex (ids, codes and counters, never text).
pub fn observe_line(class: DeliveryClass, bytes: &[u8], caller_default: bool) -> String {
    let header = bytes.get(..32).filter(|h| h.starts_with(b"HSKO"));
    let byte = |i: usize| header.and_then(|h| h.get(i)).map_or(Json::Null, |b| Json::Num(u64::from(*b)));
    let word = |at: usize, to: usize| {
        header
            .and_then(|h| h.get(at..to))
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .map_or(Json::Null, |w| Json::Num(u64::from_be_bytes(w)))
    };
    let mut hex = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    Json::obj(vec![
        ("schema", Json::str(OBSERVE_LINE_SCHEMA)),
        ("class", Json::str(if class == DeliveryClass::Terminal { "terminal" } else { "progress" })),
        ("bytes", Json::Num(u64::try_from(bytes.len()).unwrap_or(u64::MAX))),
        ("outcome", byte(5)),
        ("failure", byte(6)),
        ("correlation", word(8, 16)),
        ("sequence", word(24, 32)),
        ("caller_default", Json::Bool(caller_default)),
        ("frame_hex", Json::Str(hex)),
    ])
    .render()
}

/// studio-vk's sink: one [`observe_line`] per delivered frame, appended to the caller's `--observe` file, else
/// written to stderr. Nothing is buffered: a file that does not open is `Unavailable` (no byte left the process), a
/// failed write `Indeterminate` (part of the line may be out).
pub struct LineSink {
    path: Option<PathBuf>,
    caller_default: bool,
}

impl LineSink {
    pub fn new(path: Option<PathBuf>, caller_default: bool) -> Self {
        Self { path, caller_default }
    }
}

impl SinkPort for LineSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        let mut line = observe_line(class, bytes, self.caller_default);
        line.push('\n');
        match &self.path {
            Some(path) => {
                let mut file =
                    std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|_| DeliveryError::Unavailable)?;
                file.write_all(line.as_bytes()).map_err(|_| DeliveryError::Indeterminate)
            }
            None => std::io::stderr().lock().write_all(line.as_bytes()).map_err(|_| DeliveryError::Indeterminate),
        }
    }
}
