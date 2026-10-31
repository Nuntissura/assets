//! Headless development consumer of the selected Studio foundations. A [`Harness`] is created
//! from an explicit [`Selection`]; a [`Run`] executes typed operations (open, summary, edit,
//! panel, save, reopen, verify), records one JSON record per operation, and makes sure every
//! operation produced exactly one Observe terminal frame (session operations through the session's
//! own report, harness-owned operations through a harness-emitted frame).
//!
//! A report never passes without evidence: `finish` is `pass` only when at least one operation ran
//! and every operation succeeded (no operations is `nothing_checked`), and `verify_live` refuses a
//! verify with nothing to check. Tests: `verify_checks_something_and_save_never_clobbers_the_input`
//! and `an_empty_run_is_nothing_checked_not_a_pass` in `tests/harness_headless.rs`.
#![deny(clippy::arithmetic_side_effects)]

use crate::action::{self, ActionRecord, Call, DialogOption, NotRecordable, PlayMode, RecordedValue, Recording, RecordingReport, UnitToken};
use crate::panel::{self, PanelValues, Route};
use crate::context::RunContext;
use hsk_studio_accord::{
    ActorContext, BusinessEventRecord, CancellationToken, CausationId, CommandContext, CommandId, DomainId, MAX_BUSINESS_EVENT_ARTIFACTS,
    StudioBusinessEventId,
};
use hsk_studio_folio::{self as folio, Parent, Resolution, Resolver, StudioLayer, TileRef};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observation, Observe, Outcome, SinkPort};
use hsk_studio_session::{
    Applied, CHRONICLE_PROVIDER_ID, ChronicleProvider, Failed, MULTICAM_ENTRY, MulticamStep, ProviderRegistry, ReportStatus, SequenceEdit, Session,
    SessionConfig, SessionPorts,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

pub const SCHEMA: &str = "hsk.studio.harness@1";

/// Providers the harness knows how to register. A provider must be named in the selection to be
/// usable; nothing is registered by default.
pub const AVAILABLE_PROVIDERS: &[&str] = &[CHRONICLE_PROVIDER_ID];

/// Capabilities the harness cannot prove today (listed in every report's `not_proven`).
pub const NOT_PROVEN: &[&str] = &[
    "visual_render_proof (the run itself renders nothing; the feature `viewer` renders through composite + render-gpu/render-cpu, asserted on a synthetic document only)",
    "appearance_commit_beyond_opacity_and_blend_mode (fill opacity, clipping and isolation have no chronicle property; those controls are read-only projections)",
    "desktop_window_visual_check (feature `desktop` window opens only in an Operator-attended session, RUN-UI.md)",
];

/// How document references (tiles, profiles) are answered while opening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum References {
    /// Nothing is available (folio `Unresolved`): the honest default.
    #[default]
    Unresolved,
    /// Every reference is reported available. A development stand-in, not evidence.
    TrustAll,
}

impl References {
    pub fn name(self) -> &'static str {
        match self {
            References::Unresolved => "unresolved",
            References::TrustAll => "trust_all",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub providers: Vec<String>,
    pub references: References,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HarnessError {
    UnknownProvider(String),
    DuplicateProvider(String),
    DescriptorMismatch(String),
}

impl HarnessError {
    pub fn code(&self) -> &'static str {
        match self {
            HarnessError::UnknownProvider(_) => "unknown_provider",
            HarnessError::DuplicateProvider(_) => "duplicate_provider",
            HarnessError::DescriptorMismatch(_) => "descriptor_mismatch",
        }
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::UnknownProvider(id) | HarnessError::DuplicateProvider(id) | HarnessError::DescriptorMismatch(id) => {
                write!(f, "{}: {id}", self.code())
            }
        }
    }
}

impl std::error::Error for HarnessError {}

#[doc(hidden)]
pub struct TrustAll;
impl Resolver for TrustAll {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    /// Binds the adjustment primitive (an adjustment layer hosting `hsk.studio.adjustment@1`), so
    /// documents with adjustment layers open and render their adjustment records (lane
    /// SMB-HARNESS-RENDER-RECORDS); every other primitive layer stays unbound.
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == hsk_studio_folio::ADJUSTMENT_SCHEMA
    }
}

/// Shared in-memory record of delivered Observe frames (class and size only; no content).
#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<(DeliveryClass, usize)>>>);

impl Frames {
    fn count(&self) -> usize {
        self.0.lock().map(|f| f.len()).unwrap_or(0)
    }
}

struct FrameSink(Frames);

impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.0.lock().map_err(|_| DeliveryError::Unavailable)?.push((class, bytes.len()));
        Ok(())
    }
}

/// An explicitly configured consumer. There is no default "everything" instance.
#[derive(Clone, Debug)]
pub struct Harness {
    selection: Selection,
}

impl Harness {
    /// Fails with `UnknownProvider` for any id the harness cannot register.
    pub fn new(selection: Selection) -> Result<Self, HarnessError> {
        for (i, id) in selection.providers.iter().enumerate() {
            if !AVAILABLE_PROVIDERS.contains(&id.as_str()) {
                return Err(HarnessError::UnknownProvider(id.clone()));
            }
            if selection.providers[..i].contains(id) {
                return Err(HarnessError::DuplicateProvider(id.clone()));
            }
        }
        Ok(Self { selection })
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn provider_selected(&self, id: &str) -> bool {
        self.selection.providers.iter().any(|p| p == id)
    }

    fn registry(&self) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        for id in &self.selection.providers {
            if id == CHRONICLE_PROVIDER_ID {
                let _ = registry.register(id, Box::new(ChronicleProvider));
            }
        }
        registry
    }

    fn resolver(&self) -> Box<dyn Resolver + Send + Sync> {
        match self.selection.references {
            References::Unresolved => Box::new(folio::Unresolved),
            References::TrustAll => Box::new(TrustAll),
        }
    }

    /// Parses the real `DESCRIPTOR` const of every foundation the harness is allowed to use. A
    /// descriptor that is not JSON, or has no owner, is `DescriptorMismatch`; there is no
    /// hard-coded success list.
    pub fn collect_descriptors(&self) -> Result<Vec<Value>, HarnessError> {
        let sources: [(&str, &str); 5] = [
            ("accord", hsk_studio_accord::DESCRIPTOR),
            ("folio", hsk_studio_folio::DESCRIPTOR),
            ("session", hsk_studio_session::DESCRIPTOR),
            ("controls", hsk_studio_controls::DESCRIPTOR),
            ("observe", hsk_studio_observe::DESCRIPTOR),
        ];
        sources
            .iter()
            .map(|(crate_name, raw)| {
                let parsed: Value = serde_json::from_str(raw).map_err(|_| HarnessError::DescriptorMismatch((*crate_name).into()))?;
                let owner = ["owner", "module", "crate"]
                    .iter()
                    .find_map(|k| parsed.get(k).and_then(Value::as_str))
                    .ok_or_else(|| HarnessError::DescriptorMismatch((*crate_name).into()))?;
                let version = ["version", "descriptor_version", "slice"].iter().find_map(|k| parsed.get(k).and_then(Value::as_u64));
                Ok(json!({"crate":crate_name,"owner":owner,"version":version,"bytes":raw.len()}))
            })
            .collect()
    }

    /// A run attributed to `context` (lane SMB-ATTRIBUTION-1): every mutating command mints its
    /// own command context (actor, client session, command id, base revision, correlation) from it.
    pub fn run<'a>(&'a self, context: RunContext, cancel: &'a CancellationToken) -> Run<'a> {
        Run {
            harness: self,
            cancel,
            frames: Frames::default(),
            context,
            current: None,
            last_command: None,
            ops: Vec::new(),
            session: None,
            reopened: None,
            expectations: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            recording: None,
            quiet: false,
            pending_events: Vec::new(),
            event_sequence: None,
            event_stats: BusinessEventStats::default(),
            ambient_causation: None,
        }
    }
}

/// What the run's business-event emission did (lane SMB-BUSINESS-EVENTS-1; [`Run::business_events`]).
#[derive(Clone, Copy, Default)]
struct BusinessEventStats {
    delivered: u64,
    failed: u64,
    no_sink: u64,
    refused: u64,
    refused_missing_command_id: u64,
    /// Set by an `indeterminate` delivery: the event may have escaped (observe semantics).
    reconciliation_required: bool,
}

/// The business event a session document entry token maps to unambiguously (lane
/// SMB-BUSINESS-EVENTS-1): `develop` holds the RAW develop settings, `text` the text stories, and
/// `vector` the vector path records. Every other token (`selection` is view state; `composition`,
/// `motion`, `audio_routing`, `waveform_edit`, `multicam`, `proof`, `design`, `info`) maps to no
/// single catalog variant and emits nothing.
fn entry_business_event(token: &str) -> Option<StudioBusinessEventId> {
    match token {
        "develop" => Some(StudioBusinessEventId::RawDeveloped),
        "text" => Some(StudioBusinessEventId::TextEdited),
        "vector" => Some(StudioBusinessEventId::VectorPathEdited),
        _ => None,
    }
}

fn outcome_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Progress => "progress",
        Outcome::Success => "success",
        Outcome::Canceled => "canceled",
        Outcome::Failure(FailureCode::Validation) => "failure_validation",
        Outcome::Failure(FailureCode::Unavailable) => "failure_unavailable",
        Outcome::Failure(FailureCode::Loss) => "failure_loss",
        Outcome::Failure(FailureCode::Unsupported) => "failure_unsupported",
    }
}

fn observe_json(report: ReportStatus, frames: usize) -> Value {
    match report {
        ReportStatus::Delivered(r) => json!({"frames":frames,"status":"delivered","outcome":outcome_name(r.outcome),"correlation":r.correlation_id}),
        ReportStatus::Undelivered(e) => json!({"frames":frames,"status":"undelivered","error":e.code()}),
        ReportStatus::Unaddressed => json!({"frames":frames,"status":"unaddressed"}),
    }
}

#[derive(Clone, Copy)]
enum Prop {
    Name,
    Visible,
    Opacity,
    BlendMode,
    Tiles,
    /// A vector layer's path ids (chronicle `vector_paths`, lane SMB-ILLUSTRATOR-PEN-2).
    VectorPaths,
}

impl Prop {
    fn field(self) -> &'static str {
        match self {
            Prop::Name => "name",
            Prop::Visible => "visible",
            Prop::Opacity => "opacity",
            Prop::BlendMode => "blend_mode",
            Prop::Tiles => "tiles",
            Prop::VectorPaths => "vector_paths",
        }
    }
}

enum PropValue {
    Text(String),
    Bool(bool),
    Number(f64),
    Tiles(Vec<TileRef>),
    PathIds(Vec<String>),
}

impl PropValue {
    fn wire(&self) -> Value {
        match self {
            PropValue::Text(t) => json!({"value_kind":"string","value":t}),
            PropValue::Bool(b) => json!({"value_kind":"bool","value":b}),
            // A whole value goes out as a JSON integer: chronicle types a blend-mode discriminant
            // as an integer number.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            PropValue::Number(n) if n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(n) => json!({"value_kind":"number","value":*n as u64}),
            PropValue::Number(n) => json!({"value_kind":"number","value":n}),
            PropValue::Tiles(t) => json!({"value_kind":"tiles","value":t}),
            PropValue::PathIds(ids) => json!({"value_kind":"path_ids","value":ids}),
        }
    }
}

/// One undoable step: chronicle's inverse of a committed change (its wire form; `Null` when the
/// step changed no chronicle property), the session document entry it replaced (lane
/// SMB-HARNESS-DEVELOP-3: undo restores it together with the chronicle inverse) and a label.
struct HistoryStep {
    label: String,
    inverse: Value,
    entry: Option<EntryRestore>,
    /// A timeline step (lane SMB-AUDITION-UI: mixer faders, pan, mute, solo) that undoes / redoes
    /// this step through `Session::submit_timeline`.
    timeline: Option<hsk_studio_session::TimelineStep>,
    /// A group step (lane SMB-PS-ACTIONS-1: one replayed action, [STU-AUT-004]): the inner steps
    /// in stack order (newest last). Undo / redo apply them newest-first as ONE history step; the
    /// other fields are unused (`label` names the group).
    group: Option<Vec<HistoryStep>>,
    /// A multi-sequence step (lane SMB-PR-NEST-1: Nest, Make Subsequence) that undoes / redoes
    /// this step through `Session::submit_multicam` with the multicam records passed through.
    sequences: Option<MulticamStep>,
    /// The command that made this step (lane SMB-ATTRIBUTION-1): actor, client session, command
    /// id, base revision, correlation and causation.
    ctx: CommandContext,
    /// The document revision the command produced.
    revision: u64,
}

impl HistoryStep {
    /// The step's attribution as shown by [`Run::history`].
    fn attribution(&self) -> Value {
        json!({
            "label": self.label, "actor": actor_json(self.ctx.actor()), "client_session": self.ctx.client_session().as_str(),
            "command_id": self.ctx.command().map(CommandId::as_str), "base_revision": self.ctx.base_revision(),
            "revision": self.revision, "correlation": self.ctx.correlation().value(),
            "causation": self.ctx.causation().map(CausationId::value),
        })
    }
}

/// A session document entry as it was before a step (`None` = absent).
struct EntryRestore {
    token: String,
    bytes: Option<Vec<u8>>,
}

/// Why an operation did not complete.
enum Fail {
    /// Refused before any session operation ran (no frame owed by a session).
    Refused(&'static str),
    Session(Failed),
}

struct Expectation {
    layer_id: String,
    field: &'static str,
    expected: Value,
}

/// An in-progress workflow. Every public method records exactly one operation record and returns
/// whether it succeeded.
pub struct Run<'a> {
    harness: &'a Harness,
    cancel: &'a CancellationToken,
    frames: Frames,
    context: RunContext,
    /// The command context of the operation in progress; the next record takes it.
    current: Option<CommandContext>,
    /// The command context the newest record took (a `*_and_entry` step's entry refusal and
    /// entry-only step keep their command's correlation).
    last_command: Option<CommandContext>,
    ops: Vec<Value>,
    session: Option<Session>,
    reopened: Option<Session>,
    expectations: Vec<Expectation>,
    /// Undo history of the working session: chronicle inverses, newest last.
    undo_stack: Vec<HistoryStep>,
    /// Redo history: the inverses of undone steps, newest last; any new edit clears it.
    redo_stack: Vec<HistoryStep>,
    /// The action being recorded (lane SMB-PS-ACTIONS-1, [`Run::start_recording`]).
    recording: Option<Recording>,
    /// Set while an operation runs other public operations internally (and during playback): they
    /// are neither recorded nor reported as not recordable.
    quiet: bool,
    /// Business events of accepted commits not yet in an op record (lane SMB-BUSINESS-EVENTS-1):
    /// the next record takes them as `business_events`.
    pending_events: Vec<Value>,
    /// (revision, next sequence) of the newest business event: the events of one revision are
    /// numbered 0, 1, ... (event id `document@revision#sequence`).
    event_sequence: Option<(u64, u64)>,
    event_stats: BusinessEventStats,
    /// The causation of the inner commands of an action being played (its correlation).
    ambient_causation: Option<CausationId>,
}

impl Run<'_> {
    /// The run's attribution (lane SMB-ATTRIBUTION-1).
    pub fn context(&self) -> &RunContext {
        &self.context
    }

    /// The actor every command of this run is attributed to.
    pub fn actor(&self) -> &ActorContext {
        self.context.actor()
    }

    /// Mints the command context of a mutating command issued against `base` (the working
    /// revision when `None`); the operation's record takes it.
    fn begin_at(&mut self, command: &'static str, base: Option<u64>, causation: Option<CausationId>) -> CommandContext {
        let base = base.or_else(|| self.revision()).unwrap_or(0);
        let ctx = self.context.command(Some(command), base, causation.or(self.ambient_causation));
        self.current = Some(ctx.clone());
        ctx
    }

    fn begin(&mut self, command: &'static str, causation: Option<CausationId>) -> CommandContext {
        self.begin_at(command, None, causation)
    }

    fn op_no(&self) -> u64 {
        u64::try_from(self.ops.len()).unwrap_or(u64::MAX).saturating_add(1)
    }

    fn ports(&self) -> SessionPorts {
        SessionPorts::new(Box::new(FrameSink(self.frames.clone())))
            .with_providers(self.harness.registry())
            .with_resolver(self.harness.resolver())
    }

    fn push(&mut self, op: &str, before: usize, result: Result<Value, &'static str>, report: Option<ReportStatus>, revision: Option<u64>) -> bool {
        let frames = self.frames.count().saturating_sub(before);
        let observe = match report {
            Some(r) => observe_json(r, frames),
            None => json!({"frames":frames,"status":"unaddressed"}),
        };
        let ok = result.is_ok();
        // `n` is a local ordinal only; the correlation is the command context's (null for an
        // operation that minted none, e.g. a refusal before any document is open).
        let ctx = self.current.take();
        let mut record = json!({
            "op":op,"n":self.op_no(),"ok":ok,"revision":revision,"observe":observe,
            "actor": actor_json(self.context.actor()),
            "command_id": ctx.as_ref().and_then(CommandContext::command).map(CommandId::as_str),
            "correlation": ctx.as_ref().map(|c| c.correlation().value()),
            "causation": ctx.as_ref().and_then(CommandContext::causation).map(CausationId::value),
        });
        self.last_command = ctx;
        match result {
            Ok(detail) => record["detail"] = detail,
            Err(code) => record["error"] = json!(code),
        }
        if !self.pending_events.is_empty() {
            record["business_events"] = Value::Array(std::mem::take(&mut self.pending_events));
        }
        self.ops.push(record);
        ok
    }

    // ----- business events (lane SMB-BUSINESS-EVENTS-1) -------------------------------------------

    /// Emits `event` for a commit the session accepted (STU-CON-014: the binding, after the
    /// canonical event; never on a refusal): an accord [`BusinessEventRecord`] of the working
    /// document at the exact `revision`, attributed to `ctx`, handed to the run context's sink. The
    /// outcome goes into the op record (`business_events`) and the run's counters
    /// ([`Run::business_events`]). A delivery failure never undoes the commit; `indeterminate`
    /// sets the run's reconciliation flag. A context without a command id is refused
    /// `missing_command_id` and counted (lead decision 2026-10-31). No sink: `no_sink`, nothing
    /// emitted.
    fn emit_business_event(&mut self, event: StudioBusinessEventId, ctx: &CommandContext, revision: u64, artifacts: &[&str]) {
        let entry = self.business_event_entry(event, ctx, revision, artifacts, None);
        self.pending_events.push(entry);
    }

    /// `sequence`: the event's sequence when it is not the next of `revision`'s per-run counter (an
    /// entry-only event, [`hsk_studio_accord::entry_event_sequence`]); `None` numbers it 0, 1, ...
    fn business_event_entry(&mut self, event: StudioBusinessEventId, ctx: &CommandContext, revision: u64, artifacts: &[&str], sequence: Option<u64>) -> Value {
        let wire = event.as_str();
        if ctx.command().is_none() {
            self.event_stats.refused = self.event_stats.refused.saturating_add(1);
            self.event_stats.refused_missing_command_id = self.event_stats.refused_missing_command_id.saturating_add(1);
            return json!({"event": wire, "delivery": "refused", "error": "missing_command_id"});
        }
        let Some(document) = self.session.as_ref().map(|s| s.snapshot().document().document_id.clone()) else {
            self.event_stats.refused = self.event_stats.refused.saturating_add(1);
            return json!({"event": wire, "delivery": "refused", "error": "not_open"});
        };
        let sequence = match sequence {
            Some(entry_only) => entry_only,
            None => {
                let next = match self.event_sequence {
                    Some((r, next)) if r == revision => next,
                    _ => 0,
                };
                self.event_sequence = Some((revision, next.saturating_add(1)));
                next
            }
        };
        // Artifact refs are typed accord ids only; anything else is left out, never copied as text.
        let refs: Vec<&str> = artifacts.iter().copied().filter(|a| DomainId::parse(a).is_ok()).take(MAX_BUSINESS_EVENT_ARTIFACTS).collect();
        let record = match BusinessEventRecord::new(event, &document, revision, sequence, ctx.clone()).and_then(|r| r.with_artifacts(&refs)) {
            Ok(r) => r,
            Err(e) => {
                self.event_stats.refused = self.event_stats.refused.saturating_add(1);
                return json!({"event": wire, "delivery": "refused", "error": e.code()});
            }
        };
        let event_id = record.event_id();
        let Some(sink) = self.context.business_events().cloned() else {
            self.event_stats.no_sink = self.event_stats.no_sink.saturating_add(1);
            return json!({"event": wire, "event_id": event_id, "delivery": "no_sink"});
        };
        let result = sink.lock().unwrap_or_else(PoisonError::into_inner).try_emit(&record);
        match result {
            Ok(()) => {
                self.event_stats.delivered = self.event_stats.delivered.saturating_add(1);
                json!({"event": wire, "event_id": event_id, "delivery": "delivered"})
            }
            Err(e) => {
                self.event_stats.failed = self.event_stats.failed.saturating_add(1);
                if e.needs_reconciliation() {
                    self.event_stats.reconciliation_required = true;
                }
                json!({"event": wire, "event_id": event_id, "delivery": "failed", "error": e.code(), "reconciliation_required": e.needs_reconciliation()})
            }
        }
    }

    /// The entry part of a step: the event its token maps to, unless the step already emitted it, and
    /// only when the entry bytes changed (R48 F-R48-2: `before` is the step's `EntryRestore` bytes, the
    /// entry now holds what the step wrote; equal bytes emit nothing, as a no-change commit does).
    /// `entry_only`: the step made no document revision, so the event is minted at the previous
    /// commit's revision with [`hsk_studio_accord::entry_event_sequence`] (R48 F-R48-1: an id that
    /// survives runs); otherwise it is the next event of the step's own new revision.
    fn emit_entry_business_event(&mut self, token: &str, ctx: &CommandContext, already: Option<StudioBusinessEventId>, before: Option<&[u8]>, entry_only: bool) {
        let Some(event) = entry_business_event(token).filter(|e| already != Some(*e)) else {
            return;
        };
        let after = self.session.as_ref().and_then(|s| s.document_entry(token)).map(<[u8]>::to_vec);
        if after.as_deref() == before {
            return;
        }
        let revision = self.revision().unwrap_or(0);
        let sequence = entry_only.then(|| hsk_studio_accord::entry_event_sequence(event, token, before, after.as_deref(), ctx));
        let entry = self.business_event_entry(event, ctx, revision, &[], sequence);
        self.pending_events.push(entry);
    }

    /// Puts emitted events into the newest op record (a step whose record was written before its
    /// entry part ran).
    fn attach_events_to_last_record(&mut self) {
        if self.pending_events.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.pending_events);
        if let Some(record) = self.ops.last_mut() {
            match record["business_events"].as_array_mut() {
                Some(list) => list.extend(events),
                None => record["business_events"] = Value::Array(events),
            }
        }
    }

    /// The run's business-event counters (lane SMB-BUSINESS-EVENTS-1): delivered, failed,
    /// no_sink, refused (of which missing_command_id) and the reconciliation flag an
    /// `indeterminate` delivery sets.
    pub fn business_events(&self) -> Value {
        let s = self.event_stats;
        json!({
            "delivered": s.delivered, "failed": s.failed, "no_sink": s.no_sink, "refused": s.refused,
            "refused_missing_command_id": s.refused_missing_command_id, "reconciliation_required": s.reconciliation_required,
        })
    }

    /// Frame owned by the harness for an operation no session performed.
    fn emit_own(&self, outcome: Outcome) -> Option<ReportStatus> {
        let session = self.session.as_ref()?;
        let doc = session.snapshot().document();
        let resource = DomainId::parse(&doc.document_id).ok()?;
        let budget = Budget::new(0, 0).ok()?;
        // The frame carries the operation's command correlation (own_op mints one), never `n`.
        let correlation = self.current.as_ref()?.correlation().value();
        let mut observe = Observe::new(correlation, doc.revision, resource, self.context.actor().clone(), budget);
        let observation = Observation { correlation_id: correlation, revision: doc.revision, outcome, progress: None, private_project_text: None };
        let mut sink = FrameSink(self.frames.clone());
        Some(match observe.emit(observation, self.cancel, &mut sink) {
            Ok(receipt) => ReportStatus::Delivered(receipt),
            Err(e) => ReportStatus::Undelivered(e),
        })
    }

    fn revision(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.document_revision())
    }

    /// Records a harness-owned operation: emits its frame, then the record.
    fn own_op(&mut self, op: &str, before: usize, result: Result<Value, &'static str>) -> bool {
        let outcome = match &result {
            Ok(_) => Outcome::Success,
            Err("canceled") => Outcome::Canceled,
            Err("unsupported_route" | "no_commit_route" | "provider_not_selected") => Outcome::Failure(FailureCode::Unsupported),
            Err(_) => Outcome::Failure(FailureCode::Validation),
        };
        if self.current.is_none() && self.session.is_some() {
            // A harness-owned operation without a command id still gets its own correlation.
            let base = self.revision().unwrap_or(0);
            self.current = Some(self.context.command(None, base, None));
        }
        let report = self.emit_own(outcome);
        let revision = self.revision();
        self.push(op, before, result, report, revision)
    }

    fn layer(&self, id: &str) -> Option<&StudioLayer> {
        self.session.as_ref()?.snapshot().document().layers.iter().find(|l| l.layer_id == id)
    }

    // ----- recording hooks (lane SMB-PS-ACTIONS-1) ----------------------------------------------

    /// Runs `f` with recording suppressed (an operation's internal calls of other operations).
    fn quietly<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = std::mem::replace(&mut self.quiet, true);
        let r = f(self);
        self.quiet = prev;
        r
    }

    /// A `recordable: false` operation ran: a typed report entry while recording ([STU-AUT-109]).
    fn note_unrecordable(&mut self, command_id: &'static str) {
        self.note_unrecordable_because(command_id, "recordable_false");
    }

    fn note_unrecordable_because(&mut self, command_id: &'static str, reason: &'static str) {
        if !self.quiet
            && let Some(r) = self.recording.as_mut()
        {
            r.not_recordable.push(NotRecordable { command_id, reason });
        }
    }

    /// A recordable operation finished: a successful one appends its step, a refused one is
    /// reported and not recorded. Returns `ok`.
    fn record(&mut self, ok: bool, command_id: &'static str, params: impl FnOnce() -> Vec<(&'static str, RecordedValue)>) -> bool {
        if self.quiet || self.recording.is_none() {
            return ok;
        }
        let error = self.ops.last().and_then(|o| o["error"].as_str()).unwrap_or("refused").to_string();
        if let Some(r) = self.recording.as_mut() {
            if ok {
                r.push_step(command_id, params());
            } else {
                r.refused.push((command_id.to_string(), error));
            }
        }
        ok
    }

    /// Records a harness-owned result: with a frame when a session exists, unaddressed otherwise.
    fn own_or_push(&mut self, op: &str, before: usize, result: Result<Value, &'static str>) -> bool {
        if self.session.is_some() { self.own_op(op, before, result) } else { self.push(op, before, result, None, None) }
    }

    /// Starts recording an action (`action_id` = `SACT-{uuid_v7}`, checked as text): each
    /// successful recordable operation from now on appends one step ([STU-AUT-109]). One record
    /// `action_record_start` (`already_recording`, `action_id_malformed`).
    pub fn start_recording(&mut self, action_id: &str, name: &str) -> bool {
        let before = self.frames.count();
        let result = if self.recording.is_some() {
            Err("already_recording")
        } else if !action::action_id_ok(action_id) {
            Err("action_id_malformed")
        } else {
            self.recording = Some(Recording::new(action_id, name));
            Ok(json!({"action_id": action_id}))
        };
        self.own_or_push("action_record_start", before, result)
    }

    /// Stops recording: the recorded action (`declared_step_count` = its steps) and the report of
    /// what was not recorded. One record `action_record_stop` (`not_recording`).
    pub fn stop_recording(&mut self) -> Option<(ActionRecord, RecordingReport)> {
        let before = self.frames.count();
        let Some(recording) = self.recording.take() else {
            self.own_or_push("action_record_stop", before, Err("not_recording"));
            return None;
        };
        let (action, report) = recording.finish();
        self.own_or_push("action_record_stop", before, Ok(report.to_json()));
        Some((action, report))
    }

    /// Plays `action` on the working session ([STU-AUT-109], [STU-AUT-004]): every enabled step
    /// runs through the same public `Run` method the original edit used (its own op record, same
    /// op name); disabled steps are skipped and kept. Every enabled step is resolved first
    /// (`action_step_command_unresolvable`, `action_step_not_playable`,
    /// `action_step_parameter_invalid`) and, headless, an enabled `always_show` step refuses
    /// (`action_always_show_dialog_in_headless`) before any step runs. A step whose layer does not
    /// resolve (`action_step_reference_unresolved`), a refused step (`action_step_failed`) or a
    /// cancellation between steps (`canceled`) rolls back the steps already applied: document
    /// values and the undo/redo history are as before (the revision advances). Success leaves ONE
    /// undo step, `action <name>`. One record `action_play` after the steps' records.
    pub fn play_action(&mut self, action: &ActionRecord, mode: PlayMode) -> bool {
        self.note_unrecordable("action.play");
        self.quietly(|run| run.play_inner(action, mode))
    }

    fn play_refusal(&mut self, before: usize, code: &'static str, detail: Value) -> bool {
        let ok = self.own_or_push("action_play", before, Err(code));
        if let Some(record) = self.ops.last_mut() {
            record["detail"] = detail;
        }
        ok
    }

    fn play_inner(&mut self, action: &ActionRecord, mode: PlayMode) -> bool {
        if self.session.is_none() {
            let before = self.frames.count();
            return self.push("action_play", before, Err("not_open"), None, None);
        }
        let enabled: Vec<&action::ActionStep> = action.steps.iter().filter(|s| s.enabled).collect();
        if mode == PlayMode::Headless
            && let Some(s) = enabled.iter().find(|s| s.dialog_options == DialogOption::AlwaysShow)
        {
            let before = self.frames.count();
            let detail = json!({"action_id": action.action_id, "step_index": s.index, "command_id": s.command_id});
            return self.play_refusal(before, "action_always_show_dialog_in_headless", detail);
        }
        let mut calls = Vec::with_capacity(enabled.len());
        for s in &enabled {
            match action::call_of(s) {
                Ok(call) => calls.push((*s, call)),
                Err(fault) => {
                    let before = self.frames.count();
                    let detail = json!({"action_id": action.action_id, "step_index": s.index, "command_id": s.command_id, "key": fault.key});
                    return self.play_refusal(before, fault.code, detail);
                }
            }
        }
        // Interactive playback has no dialog host in the engine: the steps that would show one are
        // listed (GUI gap). Headless: show_in_interactive_playback and show_if_configured are
        // suppressed (Studio reading of [STU-AUT-105], UNVERIFIED (D5)).
        let dialogs_not_shown: Vec<u64> = match mode {
            PlayMode::Headless => Vec::new(),
            PlayMode::Interactive => enabled
                .iter()
                .filter(|s| match s.dialog_options {
                    DialogOption::Silent => false,
                    DialogOption::ShowIfConfigured => s.with_dialog,
                    DialogOption::ShowInInteractivePlayback | DialogOption::AlwaysShow => true,
                })
                .map(|s| s.index)
                .collect(),
        };
        let depth = self.undo_stack.len();
        let saved_redo = std::mem::take(&mut self.redo_stack);
        // The action's own command context: each inner command is caused by it (lane
        // SMB-BUSINESS-EVENTS-1: every inner commit emits its own event with this causation).
        let action_ctx = self.context.command(Some("action.play"), self.revision().unwrap_or(0), None);
        self.ambient_causation = Some(CausationId::from(action_ctx.correlation()));
        let mut fault: Option<(&'static str, &action::ActionStep, Value)> = None;
        for (step, call) in calls {
            if self.cancel.check().is_err() {
                fault = Some(("canceled", step, Value::Null));
                break;
            }
            if let Some(missing) = call.layers().into_iter().find(|l| self.layer(l).is_none()) {
                fault = Some(("action_step_reference_unresolved", step, json!({"reference": missing})));
                break;
            }
            let ok = match call {
                Call::Rename { layer, name } => self.rename(&layer, &name, None),
                Call::SetVisible { layer, visible } => self.set_visible(&layer, visible, None),
                Call::SetOpacity { layer, percent } => self.set_opacity(&layer, percent, None),
                Call::SetBlendMode { layer, mode } => self.set_blend_mode(&layer, mode, None),
                Call::PanelSet { layer, control, value } => self.panel_set(&layer, &control, &value, None),
                Call::Restructure { op, writes } => self.restructure(&op, &writes),
            };
            if !ok {
                let cause = self.ops.last().map_or(Value::Null, |o| o["error"].clone());
                fault = Some(("action_step_failed", step, json!({"cause": cause})));
                break;
            }
        }
        self.ambient_causation = None;
        if let Some((code, step, extra)) = fault {
            // Roll back newest-first; the rollback's redo entries are dropped and the history the
            // replay found is restored.
            let mut restored = true;
            while self.undo_stack.len() > depth {
                if !self.history_step(true) {
                    restored = false;
                    break;
                }
            }
            self.redo_stack = saved_redo;
            let before = self.frames.count();
            let detail = json!({"action_id": action.action_id, "step_index": step.index, "command_id": step.command_id, "detail": extra, "rolled_back": restored});
            self.current = Some(action_ctx);
            return self.play_refusal(before, if restored { code } else { "action_rollback_failed" }, detail);
        }
        let inner: Vec<HistoryStep> = self.undo_stack.drain(depth..).collect();
        let label = format!("action {}", action.name);
        let ctx = action_ctx;
        self.current = Some(ctx.clone());
        let history_label = if inner.is_empty() {
            // Nothing changed: like an unchanged edit, the redo history stays.
            self.redo_stack = saved_redo;
            Value::Null
        } else {
            let revision = self.revision().unwrap_or(0);
            self.undo_stack.push(HistoryStep { label: label.clone(), inverse: Value::Null, entry: None, timeline: None, group: Some(inner), sequences: None, ctx, revision });
            json!(label)
        };
        let skipped: Vec<u64> = action.steps.iter().filter(|s| !s.enabled).map(|s| s.index).collect();
        let before = self.frames.count();
        let detail = json!({
            "action_id": action.action_id, "name": action.name, "steps_played": enabled.len(), "skipped": skipped,
            "dialogs_not_shown": dialogs_not_shown, "history_label": history_label,
        });
        self.own_op("action_play", before, Ok(detail))
    }

    /// Undo (or redo) of a group step: its inner steps go back on the source history and are
    /// applied one by one (each writes its own record), then the steps they produced are grouped
    /// again on the other history. A failing inner step re-applies the ones already done, regroups
    /// and records `history_group_failed`.
    fn group_step(&mut self, undo: bool, label: String, inner: Vec<HistoryStep>) -> bool {
        let n = inner.len();
        let source_base = if undo { self.undo_stack.len() } else { self.redo_stack.len() };
        if undo {
            self.undo_stack.extend(inner);
        } else {
            self.redo_stack.extend(inner);
        }
        let target_base = if undo { self.redo_stack.len() } else { self.undo_stack.len() };
        let mut done = 0usize;
        while done < n && self.history_step(undo) {
            done = done.saturating_add(1);
        }
        let regroup = |stack: &mut Vec<HistoryStep>, base: usize, label: &str| {
            if stack.len() > base {
                let steps: Vec<HistoryStep> = stack.drain(base..).collect();
                // The regrouped step carries the attribution of its newest inner step.
                if let Some((ctx, revision)) = steps.last().map(|s| (s.ctx.clone(), s.revision)) {
                    stack.push(HistoryStep { label: label.to_string(), inverse: Value::Null, entry: None, timeline: None, group: Some(steps), sequences: None, ctx, revision });
                }
            }
        };
        if done == n {
            if undo {
                regroup(&mut self.redo_stack, target_base, &label);
            } else {
                regroup(&mut self.undo_stack, target_base, &label);
            }
            return true;
        }
        let mut restored = true;
        for _ in 0..done {
            if !self.history_step(!undo) {
                restored = false;
                break;
            }
        }
        regroup(&mut self.undo_stack, if undo { source_base } else { target_base }, &label);
        regroup(&mut self.redo_stack, if undo { target_base } else { source_base }, &label);
        let before = self.frames.count();
        let op = if undo { "undo" } else { "redo" };
        self.own_op(op, before, Err(if restored { "history_group_failed" } else { "history_group_rollback_failed" }))
    }

    // ----- operations ----------------------------------------------------------------------

    /// Opens a package file as the working session.
    pub fn open_path(&mut self, path: &Path) -> bool {
        self.note_unrecordable("document.open");
        let before = self.frames.count();
        let opened = Session::open_path(path, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        self.finish_open("open", before, opened, false)
    }

    /// Opens package bytes as the working session.
    pub fn open_bytes(&mut self, bytes: &[u8]) -> bool {
        self.note_unrecordable("document.open");
        let before = self.frames.count();
        let opened = Session::open_bytes(bytes, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        self.finish_open("open", before, opened, false)
    }

    /// Open directly (SMB-CONVERT-NATIVE CN-3): sniffs `bytes`, runs the selected importer, writes the
    /// native container (document, tile assets, typed import entries) and opens it as the working
    /// session through `Session::open_bytes`. One record `import`; its detail adds an `import` object
    /// (format, package bytes, import entry roles, loss entry count). A refusal before the session
    /// exists is unaddressed with the importer's or the dispatcher's typed code
    /// (`format_unrecognised`, `format_unavailable`, or the importer's own code).
    #[cfg(feature = "import")]
    pub fn open_import(&mut self, bytes: &[u8]) -> bool {
        self.open_import_detailed(bytes).is_some()
    }

    /// [`Run::open_import`] that also returns the native package bytes and the import receipt
    /// (`None` exactly when the `import` record failed). The desktop viewer reads its tile bytes
    /// from the returned package.
    #[cfg(feature = "import")]
    pub fn open_import_detailed(&mut self, bytes: &[u8]) -> Option<(Vec<u8>, Value)> {
        self.note_unrecordable("document.import");
        let before = self.frames.count();
        let resolver = self.harness.resolver();
        let (package, imported) = match crate::import::convert_to_native_as(bytes, resolver.as_ref(), self.context.actor(), self.cancel) {
            Ok(done) => done,
            Err(e) => {
                self.push("import", before, Err(e.code()), None, None);
                if let Some(record) = self.ops.last_mut() {
                    record["detail"] = json!({"import": e.detail()});
                }
                return None;
            }
        };
        let summary = json!({
            "format": imported.format.name(),
            "package_bytes": package.len(),
            "entries": imported.imports.iter().map(|e| e.role.token()).collect::<Vec<_>>(),
            "loss_entries": imported.receipt["loss"].as_array().map_or(0, Vec::len),
            "layers": imported.receipt["layers"],
            "not_proven": imported.receipt["not_proven"],
        });
        let ctx = self.begin_at("document.import", Some(0), None);
        let opened = Session::open_bytes(&package, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        let ok = self.finish_open("import", before, opened, false);
        if ok {
            // Every import route converts a foreign file into a new native document with a fresh
            // SDOC id: the document is created, then the import completed (lane SMB-BUSINESS-EVENTS-1).
            let revision = self.revision().unwrap_or(0);
            self.emit_business_event(StudioBusinessEventId::DocumentCreated, &ctx, revision, &[]);
            self.emit_business_event(StudioBusinessEventId::ImportCompleted, &ctx, revision, &[]);
            self.attach_events_to_last_record();
        }
        if let Some(record) = self.ops.last_mut() {
            if record["detail"].is_object() {
                record["detail"]["import"] = summary;
            } else {
                record["detail"] = json!({"import": summary});
            }
        }
        ok.then_some((package, imported.receipt))
    }

    /// The working document (read-only), when one is open.
    pub fn snapshot(&self) -> Option<&folio::Snapshot> {
        self.session.as_ref().map(Session::snapshot)
    }

    /// The operation records so far (the `ops` of [`Run::finish`]).
    pub fn ops(&self) -> &[Value] {
        &self.ops
    }

    fn finish_open(&mut self, op: &str, before: usize, opened: Result<Session, Failed>, reopen: bool) -> bool {
        match opened {
            Ok(session) => {
                let detail = json!({"revision":session.document_revision(),"operations":session.operations(),"loss_notes":session.loss().len()});
                let report = session.last_report();
                let revision = session.document_revision();
                if reopen {
                    self.reopened = Some(session);
                } else {
                    self.session = Some(session);
                    self.undo_stack.clear();
                    self.redo_stack.clear();
                }
                self.push(op, before, Ok(detail), Some(report), Some(revision))
            }
            Err(f) => self.push(op, before, Err(f.code()), Some(f.report), None),
        }
    }

    /// Read-only summary of the working document.
    pub fn summary(&mut self) -> bool {
        self.note_unrecordable("document.summary");
        let before = self.frames.count();
        if self.cancel.check().is_err() {
            return self.own_op("summary", before, Err("canceled"));
        }
        let Some(session) = self.session.as_ref() else {
            return self.push("summary", before, Err("not_open"), None, None);
        };
        let doc = session.snapshot().document();
        let layers: Vec<Value> = doc
            .layers
            .iter()
            .map(|l| {
                let a = l.effective_appearance();
                json!({
                    "id": l.layer_id, "name": l.name, "kind": l.kind, "visible": l.visible,
                    "parent": serde_json::to_value(&l.parent).unwrap_or(Value::Null),
                    "payload_kind": serde_json::to_value(&l.payload).ok().and_then(|p| p.get("payload_kind").cloned()),
                    "opacity": a.opacity, "fill_opacity": a.fill_opacity, "blend_mode": a.blend_mode, "clipped": a.clipped,
                })
            })
            .collect();
        let stories: Vec<Value> = doc
            .layers
            .iter()
            .filter_map(|l| {
                let p = serde_json::to_value(&l.payload).ok()?;
                (p.get("payload_kind")? == "text").then(|| json!({"layer_id":l.layer_id,"story_id":p.get("story_id")}))
            })
            .collect();
        let sequences: Vec<Value> = doc
            .sequences
            .iter()
            .map(|s| {
                let wire = serde_json::to_value(s).unwrap_or(Value::Null);
                json!({"id":s.sequence_id,"name":s.name,"tracks":s.tracks.len(),"duration":wire.get("duration")})
            })
            .collect();
        let detail = json!({
            "document_id": doc.document_id, "revision": doc.revision,
            "geometry_unit": serde_json::to_value(doc.geometry_unit).unwrap_or(Value::Null),
            "layers": layers,
            "artboards": doc.artboards.iter().map(|a| json!({"id":a.artboard_id,"name":a.name})).collect::<Vec<_>>(),
            "page_spreads": doc.page_spreads.iter().map(|p| json!({"id":p.page_spread_id,"name":p.name,"pages":p.pages.len()})).collect::<Vec<_>>(),
            "sequences": sequences, "stories": stories,
            "unresolved_references": session.snapshot().resolution_issues().len(),
            "providers": session.providers().ids().collect::<Vec<_>>(),
        });
        self.own_op("summary", before, Ok(detail))
    }

    /// Projects the control panel bound to `layer_id` (agent JSON; nothing is edited).
    pub fn panel(&mut self, layer_id: &str) -> bool {
        self.note_unrecordable("layer.panel");
        let before = self.frames.count();
        if self.cancel.check().is_err() {
            return self.own_op("panel", before, Err("canceled"));
        }
        let built = match self.layer(layer_id) {
            None => Err(if self.session.is_some() { "unknown_layer" } else { "not_open" }),
            Some(l) => panel::build(&PanelValues::of(l)).map_err(|e| e.code()),
        };
        match built {
            Ok(p) => self.own_op("panel", before, Ok(p.to_json(layer_id))),
            Err(code) if self.session.is_none() => self.push("panel", before, Err(code), None, None),
            Err(code) => self.own_op("panel", before, Err(code)),
        }
    }

    /// Renames a layer through the selected chronicle provider.
    pub fn rename(&mut self, layer_id: &str, name: &str, expected_revision: Option<u64>) -> bool {
        let ok = self.property_edit("rename", layer_id, Prop::Name, PropValue::Text(name.to_string()), expected_revision, action::CMD_RENAME);
        self.record(ok, action::CMD_RENAME, || vec![("layer", RecordedValue::Reference(layer_id.to_string())), ("name", RecordedValue::String(name.to_string()))])
    }

    /// Shows or hides a layer through the selected chronicle provider.
    pub fn set_visible(&mut self, layer_id: &str, visible: bool, expected_revision: Option<u64>) -> bool {
        let ok = self.property_edit("set_visible", layer_id, Prop::Visible, PropValue::Bool(visible), expected_revision, action::CMD_SET_VISIBLE);
        self.record(ok, action::CMD_SET_VISIBLE, || vec![("layer", RecordedValue::Reference(layer_id.to_string())), ("visible", RecordedValue::Boolean(visible))])
    }

    /// Edits a panel control: the value is parsed and validated by the control descriptor, applied
    /// to the panel's `ValueStore`, then committed through the control's route. A refused commit
    /// is undone in the store. Appearance controls have no route and are refused.
    pub fn panel_set(&mut self, layer_id: &str, control_id: &str, text: &str, expected_revision: Option<u64>) -> bool {
        let ok = self.panel_set_inner(layer_id, control_id, text, expected_revision);
        self.record(ok, action::CMD_PANEL_SET, || {
            vec![
                ("layer", RecordedValue::Reference(layer_id.to_string())),
                ("control", RecordedValue::String(control_id.to_string())),
                ("value", RecordedValue::String(text.to_string())),
            ]
        })
    }

    fn panel_set_inner(&mut self, layer_id: &str, control_id: &str, text: &str, expected_revision: Option<u64>) -> bool {
        let before = self.frames.count();
        let Some(layer) = self.layer(layer_id) else {
            let code = if self.session.is_some() { "unknown_layer" } else { "not_open" };
            return if self.session.is_some() { self.own_op("panel_set", before, Err(code)) } else { self.push("panel_set", before, Err(code), None, None) };
        };
        let mut panel = match panel::build(&PanelValues::of(layer)) {
            Ok(p) => p,
            Err(e) => return self.own_op("panel_set", before, Err(e.code())),
        };
        let Some(spec) = panel.registry.get(control_id) else {
            return self.own_op("panel_set", before, Err("unknown_control"));
        };
        let route = panel.routes.get(control_id).copied().unwrap_or(Route::None("no_route"));
        if let Route::None(_) = route {
            return self.own_op("panel_set", before, Err("no_commit_route"));
        }
        let value = match spec.parse_text(text) {
            Ok(v) => v,
            Err(e) => return self.own_op("panel_set", before, Err(e.code())),
        };
        let event = match panel.store.set(&panel.registry, control_id, value.clone(), hsk_studio_controls::ChangeSource::Typed, None) {
            Ok(ev) => ev,
            Err(e) => return self.own_op("panel_set", before, Err(e.code())),
        };
        let Some(event) = event else {
            return self.own_op("panel_set", before, Ok(json!({"control":control_id,"changed":false})));
        };
        let (prop, new) = match (route, &value) {
            (Route::ChronicleName, hsk_studio_controls::Value::Text(t)) => (Prop::Name, PropValue::Text(t.clone())),
            (Route::ChronicleVisible, hsk_studio_controls::Value::Bool(b)) => (Prop::Visible, PropValue::Bool(*b)),
            (Route::ChronicleOpacity, hsk_studio_controls::Value::Number(n)) => (Prop::Opacity, PropValue::Number(*n)),
            #[allow(clippy::cast_precision_loss)]
            (Route::ChronicleBlendMode, hsk_studio_controls::Value::Int(i)) => (Prop::BlendMode, PropValue::Number(*i as f64)),
            _ => return self.own_op("panel_set", before, Err("unsupported_route")),
        };
        let ok = self.property_edit("panel_set", layer_id, prop, new, expected_revision, action::CMD_PANEL_SET);
        if !ok {
            // The commit was refused: the panel's store must not keep the rejected value.
            let _ = panel.store.undo(&panel.registry, &event, None);
        }
        if let Some(record) = self.ops.last_mut() {
            record["control"] = json!(control_id);
            record["store_value_after"] = json!(panel.store.get(control_id).map(|v| panel.registry.get(control_id).map_or_else(String::new, |s| s.format_value(v))));
        }
        ok
    }

    fn property_edit(&mut self, op: &str, layer_id: &str, prop: Prop, new: PropValue, expected_revision: Option<u64>, command: &'static str) -> bool {
        let before = self.frames.count();
        let ctx = self.begin_at(command, expected_revision, None);
        let result = self.submit_property(layer_id, prop, &new, expected_revision, &ctx);
        match result {
            Ok(applied) => {
                let field = prop.field();
                let expected = match &new {
                    PropValue::Text(t) => Some(json!(t)),
                    PropValue::Bool(b) => Some(json!(b)),
                    PropValue::Number(n) => Some(json!(n)),
                    PropValue::Tiles(_) | PropValue::PathIds(_) => None,
                };
                if let Some(expected) = expected {
                    self.expectations.push(Expectation { layer_id: layer_id.to_string(), field, expected });
                }
                if applied.changed && !applied.replayed {
                    let event = if matches!(prop, Prop::VectorPaths) { StudioBusinessEventId::VectorPathEdited } else { StudioBusinessEventId::LayerMutated };
                    self.emit_business_event(event, &ctx, applied.revision, &[layer_id]);
                }
                if applied.changed
                    && let Some(inverse) = applied.inverse.as_ref().and_then(|i| serde_json::to_value(i).ok())
                {
                    self.undo_stack.push(HistoryStep { label: format!("{op} {field} {layer_id}"), inverse, entry: None, timeline: None, group: None, sequences: None, ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                let detail = json!({
                    "layer_id": layer_id, "field": field, "provider": applied.provider, "command": applied.command_id,
                    "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id,
                    "changed": applied.changed, "has_inverse": applied.inverse.is_some(), "replayed": applied.replayed,
                });
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(Fail::Refused(code)) => {
                if self.session.is_some() {
                    self.own_op(op, before, Err(code))
                } else {
                    self.push(op, before, Err(code), None, None)
                }
            }
            Err(Fail::Session(f)) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    fn submit_property(&mut self, layer_id: &str, prop: Prop, new: &PropValue, expected: Option<u64>, ctx: &CommandContext) -> Result<Applied, Fail> {
        if !self.harness.provider_selected(CHRONICLE_PROVIDER_ID) {
            return Err(Fail::Refused("provider_not_selected"));
        }
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or(Fail::Refused("not_open"))?;
        let params = patch_json(session, ctx, layer_id, prop, new).map_err(Fail::Refused)?;
        let revision = expected.unwrap_or_else(|| session.document_revision());
        session.submit_as(ctx, CHRONICLE_PROVIDER_ID, &params, revision, cancel).map_err(Fail::Session)
    }

    /// Sets a layer's opacity (percent) through chronicle (undoable).
    pub fn set_opacity(&mut self, layer_id: &str, percent: f64, expected_revision: Option<u64>) -> bool {
        let ok = self.property_edit("set_opacity", layer_id, Prop::Opacity, PropValue::Number(percent), expected_revision, action::CMD_SET_OPACITY);
        self.record(ok, action::CMD_SET_OPACITY, || {
            vec![("layer", RecordedValue::Reference(layer_id.to_string())), ("opacity", RecordedValue::Measured { value: percent, unit: Some(UnitToken::Percent) })]
        })
    }

    /// Sets a layer's blend mode (StudioBlendMode discriminant) through chronicle (undoable).
    pub fn set_blend_mode(&mut self, layer_id: &str, mode: u16, expected_revision: Option<u64>) -> bool {
        let ok = self.property_edit("set_blend_mode", layer_id, Prop::BlendMode, PropValue::Number(f64::from(mode)), expected_revision, action::CMD_SET_BLEND_MODE);
        self.record(ok, action::CMD_SET_BLEND_MODE, || vec![("layer", RecordedValue::Reference(layer_id.to_string())), ("blend_mode", RecordedValue::Number(f64::from(mode)))])
    }

    /// Replaces a raster layer's tile list (a pixel edit) through chronicle (undoable). Every new
    /// tile body must be staged first ([`Run::stage_tile`]); otherwise the session refuses
    /// `missing_asset`.
    pub fn set_tiles(&mut self, op: &str, layer_id: &str, tiles: Vec<TileRef>) -> bool {
        self.note_unrecordable("layer.set_tiles");
        self.property_edit(op, layer_id, Prop::Tiles, PropValue::Tiles(tiles), None, "layer.set_tiles")
    }

    /// Replaces a vector layer's path id list through chronicle `vector_paths` (lane
    /// SMB-ILLUSTRATOR-PEN-2; undoable, the layer keeps its id). The caller makes the new path
    /// records available first (the viewer's `vector` document entry).
    pub fn set_vector_paths(&mut self, op: &str, layer_id: &str, path_ids: Vec<String>) -> bool {
        self.note_unrecordable("layer.set_vector_paths");
        self.property_edit(op, layer_id, Prop::VectorPaths, PropValue::PathIds(path_ids), None, "layer.set_vector_paths")
    }

    /// ONE undoable step (lane SMB-ILLUSTRATOR-TOOLS-2): the `vector_paths` of every layer in
    /// `writes` through one chronicle patch, and the session document entry `token` set to `bytes`
    /// (when given) inside the same history step, so undo/redo restore both together. Without
    /// path writes the step is entry-only. A refused entry write undoes the path step and leaves
    /// the redo history as it was. One record `op`.
    pub fn set_vector_paths_and_entry(&mut self, op: &str, writes: &[(String, Vec<String>)], entry: Option<(&str, Vec<u8>)>) -> bool {
        self.note_unrecordable("layer.set_vector_paths_and_entry");
        self.keeping_redo_without_new_step(|run| run.vector_paths_and_entry_step(op, writes, entry))
    }

    /// Runs a `*_and_entry` step (finding F-R42-2 of round STUDIO-R42) with the redo history set
    /// aside: when `step` leaves no new undo step (refused, unchanged, or rolled back after a
    /// refused entry write) the redo history it found is restored, so the rollback undo's redo
    /// entry (the edit without its entry) is dropped, as [`Run::play_action`] does; a new step keeps
    /// the redo history cleared, as any new edit does.
    fn keeping_redo_without_new_step(&mut self, step: impl FnOnce(&mut Self) -> bool) -> bool {
        let depth = self.undo_stack.len();
        let saved_redo = std::mem::take(&mut self.redo_stack);
        let ok = step(self);
        if self.undo_stack.len() <= depth {
            self.redo_stack = saved_redo;
        }
        ok
    }

    fn vector_paths_and_entry_step(&mut self, op: &str, writes: &[(String, Vec<String>)], entry: Option<(&str, Vec<u8>)>) -> bool {
        let before = self.frames.count();
        let Some(session) = self.session.as_ref() else {
            return self.push(op, before, Err("not_open"), None, None);
        };
        let previous = entry.as_ref().map(|(token, _)| EntryRestore { token: (*token).to_string(), bytes: session.document_entry(token).map(<[u8]>::to_vec) });
        let depth = self.undo_stack.len();
        let ctx = self.begin("layer.set_vector_paths_and_entry", None);
        let mut emitted = None;
        if !writes.is_empty() {
            if !self.harness.provider_selected(CHRONICLE_PROVIDER_ID) {
                return self.own_op(op, before, Err("provider_not_selected"));
            }
            let cancel = self.cancel;
            let Some(session) = self.session.as_mut() else {
                return self.push(op, before, Err("not_open"), None, None);
            };
            let params = match patch_json_vector_paths(session, &ctx, writes) {
                Ok(p) => p,
                Err(code) => return self.own_op(op, before, Err(code)),
            };
            let revision = session.document_revision();
            match session.submit_as(&ctx, CHRONICLE_PROVIDER_ID, &params, revision, cancel) {
                Ok(applied) => {
                    if applied.changed && !applied.replayed {
                        let layers: Vec<&str> = writes.iter().map(|(l, _)| l.as_str()).collect();
                        self.emit_business_event(StudioBusinessEventId::VectorPathEdited, &ctx, applied.revision, &layers);
                        emitted = Some(StudioBusinessEventId::VectorPathEdited);
                    }
                    if applied.changed
                        && let Some(inverse) = applied.inverse.as_ref().and_then(|i| serde_json::to_value(i).ok())
                    {
                        self.undo_stack.push(HistoryStep { label: format!("{op} vector_paths"), inverse, entry: None, timeline: None, group: None, sequences: None, ctx: ctx.clone(), revision: applied.revision });
                        self.redo_stack.clear();
                    }
                }
                Err(f) => {
                    let revision = self.revision();
                    return self.push(op, before, Err(f.code()), Some(f.report), revision);
                }
            }
        }
        if let (Some((token, bytes)), Some(restore)) = (entry, previous) {
            let cancel = self.cancel;
            let set = match self.session.as_mut() {
                Some(s) => s.set_document_entry(token, Some(bytes), cancel).map_err(|e| e.code()),
                None => Err("not_open"),
            };
            if let Err(code) = set {
                // A committed path step without its entry would split the step: undo the paths.
                // The accepted path commit's event stays with this step's record; the rollback
                // undo's record carries its own.
                let accepted = std::mem::take(&mut self.pending_events);
                if self.undo_stack.len() > depth {
                    let _ = self.quietly(Self::undo);
                }
                self.pending_events = accepted;
                self.current = Some(ctx);
                return self.own_op(op, before, Err(code));
            }
            let entry_only = self.undo_stack.len() <= depth;
            self.emit_entry_business_event(token, &ctx, emitted, restore.bytes.as_deref(), entry_only);
            if self.undo_stack.len() > depth {
                if let Some(step) = self.undo_stack.last_mut() {
                    step.entry = Some(restore);
                }
            } else {
                let revision = self.revision().unwrap_or(0);
                self.undo_stack.push(HistoryStep { label: format!("{op} {token}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx, revision });
                self.redo_stack.clear();
            }
        }
        let layers: Vec<&str> = writes.iter().map(|(l, _)| l.as_str()).collect();
        self.own_op(op, before, Ok(json!({"layers": layers})))
    }

    /// Stack parent and index (0 = bottom-most) of a layer of the working revision (chronicle
    /// `placement` through the session); `None` when absent, unstacked or nothing is open.
    pub fn placement(&self, layer_id: &str) -> Option<(Parent, u64)> {
        self.session.as_ref()?.layer_placement(layer_id)
    }

    /// Structure edit (lane SMB-HARNESS-UI-3): one chronicle transaction of `placement` writes,
    /// applied in order (create, delete, move: new layer, duplicate, delete, reorder, group,
    /// ungroup). Undoable as one step. One record `op`.
    pub fn restructure(&mut self, op: &str, writes: &[PlacementWrite]) -> bool {
        if writes.iter().any(|w| w.create.is_some()) {
            // A create carries a whole layer record and graph operation: not replayable from the
            // recorded value types, so not recorded (placement-only writes are).
            self.note_unrecordable_because(action::CMD_RESTRUCTURE, "restructure_create_payload");
            return self.quietly(|run| run.restructure_inner(op, writes, action::CMD_RESTRUCTURE));
        }
        let ok = self.restructure_inner(op, writes, action::CMD_RESTRUCTURE);
        self.record(ok, action::CMD_RESTRUCTURE, || vec![("op", RecordedValue::String(op.to_string())), ("writes", RecordedValue::List(writes.iter().map(action::placement_value).collect()))])
    }

    fn restructure_inner(&mut self, op: &str, writes: &[PlacementWrite], command: &'static str) -> bool {
        let before = self.frames.count();
        let ctx = self.begin(command, None);
        match self.submit_structure(writes, &ctx) {
            Ok(applied) => {
                if applied.changed && !applied.replayed {
                    // A placement that adds a layer creates it; every other placement mutates.
                    let event = if writes.iter().any(|w| w.create.is_some()) { StudioBusinessEventId::LayerCreated } else { StudioBusinessEventId::LayerMutated };
                    let layers: Vec<&str> = writes.iter().map(|w| w.layer_id.as_str()).collect();
                    self.emit_business_event(event, &ctx, applied.revision, &layers);
                }
                if applied.changed
                    && let Some(inverse) = applied.inverse.as_ref().and_then(|i| serde_json::to_value(i).ok())
                {
                    let first = writes.first().map_or("", |w| w.layer_id.as_str());
                    self.undo_stack.push(HistoryStep { label: format!("{op} {first}"), inverse, entry: None, timeline: None, group: None, sequences: None, ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                let detail = json!({
                    "layers": writes.iter().map(|w| w.layer_id.clone()).collect::<Vec<_>>(),
                    "provider": applied.provider, "command": applied.command_id,
                    "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id,
                    "changed": applied.changed, "has_inverse": applied.inverse.is_some(),
                });
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(Fail::Refused(code)) => {
                if self.session.is_some() {
                    self.own_op(op, before, Err(code))
                } else {
                    self.push(op, before, Err(code), None, None)
                }
            }
            Err(Fail::Session(f)) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// ONE undoable step (lane SMB-AI-AREA-TYPE-3): a structure edit exactly like
    /// [`Run::restructure`] and the session document entry `token` set to `bytes` inside the same
    /// history step, so undo/redo restore the layers and the entry together (a text layer replaced
    /// by a vector layer whose records live in the entry). A refused structure edit changes
    /// nothing; a refused entry write undoes the structure edit (no split step) and leaves the redo
    /// history as it was. Records: the `restructure` record `op`, plus an `op` refusal record when
    /// the entry write is refused.
    pub fn restructure_and_entry(&mut self, op: &str, writes: &[PlacementWrite], token: &str, bytes: Vec<u8>) -> bool {
        self.note_unrecordable("layer.restructure_and_entry");
        self.keeping_redo_without_new_step(|run| run.restructure_and_entry_step(op, writes, token, bytes))
    }

    fn restructure_and_entry_step(&mut self, op: &str, writes: &[PlacementWrite], token: &str, bytes: Vec<u8>) -> bool {
        let previous = match self.session.as_ref() {
            Some(s) => s.document_entry(token).map(<[u8]>::to_vec),
            None => {
                let before = self.frames.count();
                return self.push(op, before, Err("not_open"), None, None);
            }
        };
        let depth = self.undo_stack.len();
        if !self.quietly(|run| run.restructure_inner(op, writes, "layer.restructure_and_entry")) {
            return false;
        }
        let ctx = self.last_command.clone();
        let cancel = self.cancel;
        let set = match self.session.as_mut() {
            Some(s) => s.set_document_entry(token, Some(bytes), cancel).map_err(|e| e.code()),
            None => Err("not_open"),
        };
        if let Err(code) = set {
            // A committed structure step without its entry would split the step: undo it.
            if self.undo_stack.len() > depth {
                let _ = self.quietly(Self::undo);
            }
            let before = self.frames.count();
            self.current = ctx;
            return self.own_op(op, before, Err(code));
        }
        let restore = EntryRestore { token: token.to_string(), bytes: previous };
        if self.undo_stack.len() > depth {
            if let Some(ctx) = ctx {
                self.emit_entry_business_event(token, &ctx, None, restore.bytes.as_deref(), false);
            }
            if let Some(step) = self.undo_stack.last_mut() {
                step.entry = Some(restore);
            }
        } else {
            // The structure edit changed nothing: the step is entry-only (same command).
            let (ctx, revision) = self.entry_only_attribution(ctx, "layer.restructure_and_entry");
            self.emit_entry_business_event(token, &ctx, None, restore.bytes.as_deref(), true);
            self.undo_stack.push(HistoryStep { label: format!("{op} {token}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx, revision });
            self.redo_stack.clear();
        }
        // The restructure record was written before the entry part ran.
        self.attach_events_to_last_record();
        true
    }

    fn submit_structure(&mut self, writes: &[PlacementWrite], ctx: &CommandContext) -> Result<Applied, Fail> {
        if !self.harness.provider_selected(CHRONICLE_PROVIDER_ID) {
            return Err(Fail::Refused("provider_not_selected"));
        }
        if writes.is_empty() {
            return Err(Fail::Refused("nothing_to_change"));
        }
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or(Fail::Refused("not_open"))?;
        let mut reads = Vec::new();
        let mut wire = Vec::new();
        for (i, w) in writes.iter().enumerate() {
            if writes[..i].iter().any(|o| o.layer_id == w.layer_id) {
                return Err(Fail::Refused("duplicate_layer"));
            }
            let exists = session.snapshot().document().layers.iter().any(|l| l.layer_id == w.layer_id);
            let current = session.layer_placement(&w.layer_id);
            if exists && current.is_none() {
                return Err(Fail::Refused("layer_not_stacked"));
            }
            let address = json!({"target":{"node_kind":"layer","node_id":w.layer_id},"property":"placement"});
            // A layer the session never held is at revision 0 (chronicle); every other address
            // quotes the session's vector.
            let revision = session
                .revisions()
                .iter()
                .find(|e| serde_json::to_value(&e.address).ok().as_ref() == Some(&address))
                .map_or(0, |e| e.revision);
            reads.push(json!({"address": address, "expected_revision": revision, "expected_value": placement_state_wire(current.as_ref())}));
            wire.push(json!({"address": address, "value": place_wire(w.to.as_ref(), w.create.as_ref())}));
        }
        let patch = json!({
            "transport_version": 1,
            "document_id": session.snapshot().document().document_id,
            "base_revision": session.document_revision(),
            "command_id": command_text(ctx),
            "correlation_id": ctx.correlation().value(),
            "actor": actor_json(ctx.actor()),
            "reads": reads,
            "writes": wire,
        });
        let params = serde_json::to_vec(&patch).map_err(|_| Fail::Refused("patch_encoding"))?;
        let revision = session.document_revision();
        session.submit_as(ctx, CHRONICLE_PROVIDER_ID, &params, revision, cancel).map_err(Fail::Session)
    }

    /// Sets or removes a session document entry (`selection`, `text`; saved with the package as
    /// `studio/<token>.<sha256>`, restored on reopen). No record: it is saved by the next `save`.
    pub fn set_document_entry(&mut self, token: &str, bytes: Option<Vec<u8>>) -> Result<(), &'static str> {
        self.note_unrecordable("document.set_document_entry");
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or("not_open")?;
        session.set_document_entry(token, bytes, cancel).map_err(|e| e.code())
    }

    /// Sets the entry `restore` names to its bytes; returns the entry it replaced (for the opposite step).
    fn swap_entry(&mut self, restore: &EntryRestore) -> Result<Option<EntryRestore>, &'static str> {
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or("not_open")?;
        let current = session.document_entry(&restore.token).map(<[u8]>::to_vec);
        session.set_document_entry(&restore.token, restore.bytes.clone(), cancel).map_err(|e| e.code())?;
        Ok(Some(EntryRestore { token: restore.token.clone(), bytes: current }))
    }

    /// One undoable step of a tile edit and a session document entry (lane SMB-HARNESS-DEVELOP-3;
    /// the develop panel's edit lives in the entry `develop`): `tiles` (when given and different)
    /// commits through chronicle like [`Run::set_tiles`], the entry is set to `bytes`, and undo/redo
    /// restore both as one step. Without a tile change the step is entry-only (still one history
    /// step, op `op`). A refused entry write undoes the tile step and leaves the redo history as it
    /// was. One record `op`.
    pub fn set_tiles_and_entry(&mut self, op: &str, layer_id: &str, tiles: Option<Vec<TileRef>>, token: &str, bytes: Vec<u8>) -> bool {
        self.note_unrecordable("layer.set_tiles_and_entry");
        self.keeping_redo_without_new_step(|run| run.tiles_and_entry_step(op, layer_id, tiles, token, bytes))
    }

    fn tiles_and_entry_step(&mut self, op: &str, layer_id: &str, tiles: Option<Vec<TileRef>>, token: &str, bytes: Vec<u8>) -> bool {
        let previous = match self.session.as_ref() {
            Some(s) => s.document_entry(token).map(<[u8]>::to_vec),
            None => {
                let before = self.frames.count();
                return self.push(op, before, Err("not_open"), None, None);
            }
        };
        let depth = self.undo_stack.len();
        let mut ctx = None;
        if let Some(tiles) = tiles {
            if !self.quietly(|run| run.property_edit(op, layer_id, Prop::Tiles, PropValue::Tiles(tiles), None, "layer.set_tiles_and_entry")) {
                return false;
            }
            ctx = self.last_command.clone();
        }
        let cancel = self.cancel;
        let set = match self.session.as_mut() {
            Some(s) => s.set_document_entry(token, Some(bytes), cancel).map_err(|e| e.code()),
            None => Err("not_open"),
        };
        let restore = EntryRestore { token: token.to_string(), bytes: previous };
        if let Err(code) = set {
            // A committed tile step without its entry would split the step: undo the tiles.
            if self.undo_stack.len() > depth {
                let _ = self.quietly(Self::undo);
            }
            let before = self.frames.count();
            self.current = ctx;
            return self.own_op(op, before, Err(code));
        }
        if self.undo_stack.len() > depth {
            if let Some(ctx) = ctx {
                // The tile record was written before the entry part ran.
                self.emit_entry_business_event(token, &ctx, None, restore.bytes.as_deref(), false);
                self.attach_events_to_last_record();
            }
            if let Some(step) = self.undo_stack.last_mut() {
                step.entry = Some(restore);
            }
            true
        } else {
            let before = self.frames.count();
            let (ctx, revision) = self.entry_only_attribution(ctx, "layer.set_tiles_and_entry");
            self.emit_entry_business_event(token, &ctx, None, restore.bytes.as_deref(), true);
            self.current = Some(ctx.clone());
            self.undo_stack.push(HistoryStep { label: format!("{op} {token}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx, revision });
            self.redo_stack.clear();
            self.own_op(op, before, Ok(json!({"layer_id": layer_id, "entry": token, "tiles_changed": false})))
        }
    }

    /// One undoable step replacing the sequence `write.sequence_id` with `write` through the
    /// session's timeline step (lane SMB-AUDITION-UI: mixer faders, pan, mute and solo are track
    /// fields); undo / redo submit the step's inverse. Folio validates the result (refusals change
    /// nothing). One record `op`.
    pub fn submit_sequence(&mut self, op: &str, write: folio::StudioSequence) -> bool {
        self.note_unrecordable("document.submit_sequence");
        self.submit_sequence_as(op, write, "document.submit_sequence")
    }

    fn submit_sequence_as(&mut self, op: &str, write: folio::StudioSequence, command: &'static str) -> bool {
        let before = self.frames.count();
        let cancel = self.cancel;
        if self.session.is_none() {
            return self.push(op, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(command, None);
        let Some(session) = self.session.as_mut() else {
            return self.push(op, before, Err("not_open"), None, None);
        };
        let Some(expect) = session.snapshot().document().sequences.iter().find(|q| q.sequence_id == write.sequence_id).cloned() else {
            return self.own_op(op, before, Err("unknown_sequence"));
        };
        let sequence_id = write.sequence_id.clone();
        let step = hsk_studio_session::TimelineStep { command_id: command_text(&ctx).to_string(), correlation_id: ctx.correlation().value(), sequence_id: sequence_id.clone(), expect, write };
        let revision = session.document_revision();
        match session.submit_timeline_as(&ctx, &step, revision, cancel) {
            Ok(applied) => {
                if applied.timeline_inverse.is_some() {
                    self.undo_stack.push(HistoryStep { label: format!("{op} {sequence_id}"), inverse: Value::Null, entry: None, timeline: applied.timeline_inverse.clone(), group: None, sequences: None, ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                let detail = json!({"sequence_id": sequence_id, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(f) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// ONE undoable step (lane SMB-AU-MULTITRACK-2): a sequence replacement exactly like
    /// [`Run::submit_sequence`] and the session document entry `token` set to `bytes` inside the same
    /// history step, so undo/redo restore the sequence and the entry together (a timeline edit that
    /// also edits the clip envelopes of the audio routing entry). A refused sequence changes
    /// nothing; a refused entry write undoes the sequence step (no split step) and leaves the redo
    /// history as it was. Records: the `submit_sequence` record `op`, plus an `op` refusal record
    /// when the entry write is refused. While recording it is reported once as
    /// `document.submit_sequence_and_entry` (not recordable).
    pub fn submit_sequence_and_entry(&mut self, op: &str, write: folio::StudioSequence, token: &str, bytes: Vec<u8>) -> bool {
        self.note_unrecordable("document.submit_sequence_and_entry");
        self.keeping_redo_without_new_step(|run| run.sequence_and_entry_step(op, write, token, bytes))
    }

    /// ONE undoable step (lane SMB-PR-NEST-1, [STU-VID-030] / [STU-VID-040]) that replaces, inserts
    /// and removes document sequences together through the session's multi-sequence step
    /// (`Session::submit_multicam`): Nest is an insert of the new sequence plus a replace of the
    /// parent, Make Subsequence an insert. The multicam records entry passes through unchanged
    /// (`records_expect == records_write ==` the live entry), so only sequences change; undo / redo
    /// submit the step's inverse. The history label and the record are `op` (for example
    /// `sequence.nest`); the session's receipt keeps its provider name `multicam.step` (a session
    /// reading, not renamed here). Folio validates the result; a refusal changes nothing
    /// (`value_conflict`, `invalid_target`, `invalid_input`, `folio_rejected`, ...;
    /// `multicam_records_invalid` when the stored records do not parse). One record `op`; while
    /// recording it is reported once as `document.submit_sequence_edits` (not recordable).
    pub fn submit_sequence_edits(&mut self, op: &str, edits: Vec<SequenceEdit>) -> bool {
        self.note_unrecordable("document.submit_sequence_edits");
        self.submit_sequence_edits_as(op, edits, "document.submit_sequence_edits")
    }

    fn submit_sequence_edits_as(&mut self, op: &str, edits: Vec<SequenceEdit>, command: &'static str) -> bool {
        let before = self.frames.count();
        let cancel = self.cancel;
        if self.session.is_none() {
            return self.push(op, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(command, None);
        let records = match self.live_multicam_records() {
            Ok(r) => r,
            Err(code) => return self.own_op(op, before, Err(code)),
        };
        let ids: Vec<String> = edits
            .iter()
            .map(|e| match e {
                SequenceEdit::Replace { expect, .. } | SequenceEdit::Remove { expect } => expect.sequence_id.clone(),
                SequenceEdit::Insert { write, .. } => write.sequence_id.clone(),
            })
            .collect();
        let step = MulticamStep { command_id: command_text(&ctx).to_string(), correlation_id: ctx.correlation().value(), sequences: edits, records_expect: records.clone(), records_write: records };
        let Some(session) = self.session.as_mut() else {
            return self.push(op, before, Err("not_open"), None, None);
        };
        let revision = session.document_revision();
        match session.submit_multicam_as(&ctx, &step, revision, cancel) {
            Ok(applied) => {
                if applied.multicam_inverse.is_some() {
                    self.undo_stack.push(HistoryStep { label: format!("{op} {}", ids.join(",")), inverse: Value::Null, entry: None, timeline: None, group: None, sequences: applied.multicam_inverse.clone(), ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                let detail = json!({"sequence_ids": ids, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed, "provider": applied.provider});
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(f) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// ONE undoable step (lane SMB-PR-NEST-2, [STU-VID-040]): the sequence edits exactly like
    /// [`Run::submit_sequence_edits`] and the session document entry `token` set to `bytes` inside
    /// the same history step, so undo / redo restore the sequences and the entry together (Nest
    /// moving the clip gain and pan envelopes of the nested clips into the new sequence's routing;
    /// Make Subsequence copying them under the copies' ids). A refused edit changes nothing; a
    /// refused entry write undoes the sequence step (no split step) and leaves the redo history as
    /// it was (as [`Run::submit_sequence_and_entry`]). Records: the `submit_sequence_edits` record
    /// `op`, plus an `op` refusal record when the entry write is refused. While recording it is
    /// reported once as `document.submit_sequence_edits_and_entry` (not recordable).
    pub fn submit_sequence_edits_and_entry(&mut self, op: &str, edits: Vec<SequenceEdit>, token: &str, bytes: Vec<u8>) -> bool {
        self.note_unrecordable("document.submit_sequence_edits_and_entry");
        self.keeping_redo_without_new_step(|run| run.sequence_edits_and_entry_step(op, edits, token, bytes))
    }

    fn sequence_edits_and_entry_step(&mut self, op: &str, edits: Vec<SequenceEdit>, token: &str, bytes: Vec<u8>) -> bool {
        let previous = match self.session.as_ref() {
            Some(s) => s.document_entry(token).map(<[u8]>::to_vec),
            None => {
                let before = self.frames.count();
                return self.push(op, before, Err("not_open"), None, None);
            }
        };
        let depth = self.undo_stack.len();
        if !self.quietly(|run| run.submit_sequence_edits_as(op, edits, "document.submit_sequence_edits_and_entry")) {
            return false;
        }
        let ctx = self.last_command.clone();
        let cancel = self.cancel;
        let set = match self.session.as_mut() {
            Some(s) => s.set_document_entry(token, Some(bytes), cancel).map_err(|e| e.code()),
            None => Err("not_open"),
        };
        if let Err(code) = set {
            // A committed sequence step without its entry would split the step: undo it.
            if self.undo_stack.len() > depth {
                let _ = self.quietly(Self::undo);
            }
            let before = self.frames.count();
            self.current = ctx;
            return self.own_op(op, before, Err(code));
        }
        let restore = EntryRestore { token: token.to_string(), bytes: previous };
        if self.undo_stack.len() > depth {
            if let Some(step) = self.undo_stack.last_mut() {
                step.entry = Some(restore);
            }
        } else {
            // The sequences did not change: the step is entry-only (same command).
            let (ctx, revision) = self.entry_only_attribution(ctx, "document.submit_sequence_edits_and_entry");
            self.undo_stack.push(HistoryStep { label: format!("{op} {token}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx, revision });
            self.redo_stack.clear();
        }
        true
    }

    /// The working session's multicam records entry as stored (`None` = no entry);
    /// `multicam_records_invalid` when its bytes do not parse.
    fn live_multicam_records(&self) -> Result<Option<folio::StudioMulticamRecords>, &'static str> {
        match self.session.as_ref().and_then(|s| s.document_entry(MULTICAM_ENTRY)) {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(bytes).map(Some).map_err(|_| "multicam_records_invalid"),
        }
    }

    /// The attribution of an entry-only step: the command whose record the call wrote (`ctx`), or
    /// a fresh context for `command` when no record took one.
    fn entry_only_attribution(&self, ctx: Option<CommandContext>, command: &'static str) -> (CommandContext, u64) {
        let revision = self.revision().unwrap_or(0);
        (ctx.unwrap_or_else(|| self.context.command(Some(command), revision, None)), revision)
    }

    /// Puts a step that could not be applied back on the stack it came from.
    fn restore_step(&mut self, undo: bool, step: HistoryStep) {
        if undo {
            self.undo_stack.push(step);
        } else {
            self.redo_stack.push(step);
        }
    }

    fn sequence_and_entry_step(&mut self, op: &str, write: folio::StudioSequence, token: &str, bytes: Vec<u8>) -> bool {
        let previous = match self.session.as_ref() {
            Some(s) => s.document_entry(token).map(<[u8]>::to_vec),
            None => {
                let before = self.frames.count();
                return self.push(op, before, Err("not_open"), None, None);
            }
        };
        let depth = self.undo_stack.len();
        if !self.quietly(|run| run.submit_sequence_as(op, write, "document.submit_sequence_and_entry")) {
            return false;
        }
        let ctx = self.last_command.clone();
        let cancel = self.cancel;
        let set = match self.session.as_mut() {
            Some(s) => s.set_document_entry(token, Some(bytes), cancel).map_err(|e| e.code()),
            None => Err("not_open"),
        };
        if let Err(code) = set {
            // A committed sequence step without its entry would split the step: undo it.
            if self.undo_stack.len() > depth {
                let _ = self.quietly(Self::undo);
            }
            let before = self.frames.count();
            self.current = ctx;
            return self.own_op(op, before, Err(code));
        }
        let restore = EntryRestore { token: token.to_string(), bytes: previous };
        if self.undo_stack.len() > depth {
            if let Some(step) = self.undo_stack.last_mut() {
                step.entry = Some(restore);
            }
        } else {
            // The sequence did not change: the step is entry-only (same command).
            let (ctx, revision) = self.entry_only_attribution(ctx, "document.submit_sequence_and_entry");
            self.undo_stack.push(HistoryStep { label: format!("{op} {token}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx, revision });
            self.redo_stack.clear();
        }
        true
    }

    /// The bytes of a session document entry of the working session.
    pub fn document_entry(&self, token: &str) -> Option<&[u8]> {
        self.session.as_ref()?.document_entry(token)
    }

    /// Stages a tile body in the working session (no record; nothing changes until a
    /// [`Run::set_tiles`] names it). `None` without a session or when the session refuses it.
    pub fn stage_tile(&mut self, bytes: Vec<u8>) -> Result<folio::ContentDigest, &'static str> {
        self.note_unrecordable("document.stage_tile");
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or("not_open")?;
        session.stage_tile(bytes, cancel).map_err(|e| e.code())
    }

    /// Every tile body the working revision references, by sha256 hex (session render_assets).
    pub fn render_assets(&self) -> Result<std::collections::BTreeMap<String, Vec<u8>>, &'static str> {
        let session = self.session.as_ref().ok_or("not_open")?;
        session.render_assets(self.cancel).map_err(|e| e.code())
    }

    /// Undoes the newest committed step by submitting chronicle's inverse; the step moves to the
    /// redo history. One record `undo` (`nothing_to_undo` when the history is empty).
    pub fn undo(&mut self) -> bool {
        self.note_unrecordable("history.undo");
        self.history_step(true)
    }

    /// Re-applies the newest undone step. One record `redo` (`nothing_to_redo` when empty).
    pub fn redo(&mut self) -> bool {
        self.note_unrecordable("history.redo");
        self.history_step(false)
    }

    /// Labels of the undo and redo histories, oldest first, and each step's attribution (lane
    /// SMB-ATTRIBUTION-1: `undo_steps` / `redo_steps` = label, actor, client session, command id,
    /// base revision, revision, correlation, causation).
    pub fn history(&self) -> Value {
        json!({
            "undo": self.undo_stack.iter().map(|s| s.label.clone()).collect::<Vec<_>>(),
            "redo": self.redo_stack.iter().map(|s| s.label.clone()).collect::<Vec<_>>(),
            "undo_steps": self.undo_stack.iter().map(HistoryStep::attribution).collect::<Vec<_>>(),
            "redo_steps": self.redo_stack.iter().map(HistoryStep::attribution).collect::<Vec<_>>(),
        })
    }

    fn history_step(&mut self, undo: bool) -> bool {
        let op = if undo { "undo" } else { "redo" };
        let before = self.frames.count();
        let popped = if undo { self.undo_stack.pop() } else { self.redo_stack.pop() };
        let Some(step) = popped else {
            let code = if undo { "nothing_to_undo" } else { "nothing_to_redo" };
            return if self.session.is_some() { self.own_op(op, before, Err(code)) } else { self.push(op, before, Err(code), None, None) };
        };
        let step = match step {
            HistoryStep { label, group: Some(inner), .. } => return self.group_step(undo, label, inner),
            other => other,
        };
        // A fresh command for the undo / redo, caused by the step it reverses.
        let ctx = self.begin(if undo { "history.undo" } else { "history.redo" }, Some(CausationId::from(step.ctx.correlation())));
        let history_event = if undo { StudioBusinessEventId::HistoryUndone } else { StudioBusinessEventId::HistoryRedone };
        if let Some(timeline) = step.timeline.clone() {
            // A timeline step: submit it under fresh identity; its inverse goes to the other stack.
            let cancel = self.cancel;
            let mut submit = timeline;
            submit.command_id = command_text(&ctx).to_string();
            submit.correlation_id = ctx.correlation().value();
            let Some(session) = self.session.as_mut() else {
                if undo {
                    self.undo_stack.push(step);
                } else {
                    self.redo_stack.push(step);
                }
                return self.push(op, before, Err("not_open"), None, None);
            };
            let revision = session.document_revision();
            return match session.submit_timeline_as(&ctx, &submit, revision, cancel) {
                Ok(applied) => {
                    // The step's entry comes back with the sequence (submit_sequence_and_entry;
                    // refused: the sequence step stays applied, reported, never silently split).
                    let previous = match step.entry.as_ref().map(|e| self.swap_entry(e)) {
                        Some(Ok(p)) => p,
                        Some(Err(code)) => {
                            let revision = self.revision();
                            return self.push(op, before, Err(code), Some(applied.report), revision);
                        }
                        None => None,
                    };
                    if applied.changed && !applied.replayed {
                        self.emit_business_event(history_event, &ctx, applied.revision, &[]);
                    }
                    let detail = json!({"step": step.label, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                    let next = HistoryStep { label: step.label, inverse: Value::Null, entry: previous, timeline: applied.timeline_inverse.clone(), group: None, sequences: None, ctx, revision: applied.revision };
                    if undo {
                        self.redo_stack.push(next);
                    } else {
                        self.undo_stack.push(next);
                    }
                    self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
                }
                Err(f) => {
                    if undo {
                        self.undo_stack.push(step);
                    } else {
                        self.redo_stack.push(step);
                    }
                    let revision = self.revision();
                    self.push(op, before, Err(f.code()), Some(f.report), revision)
                }
            };
        }
        if let Some(mut submit) = step.sequences.clone() {
            // A multi-sequence step (lane SMB-PR-NEST-1): submit its inverse under fresh identity
            // with the live multicam records passed through; its inverse goes to the other stack.
            let cancel = self.cancel;
            submit.command_id = command_text(&ctx).to_string();
            submit.correlation_id = ctx.correlation().value();
            let records = match self.live_multicam_records() {
                Ok(r) => r,
                Err(code) => {
                    self.restore_step(undo, step);
                    return if self.session.is_some() { self.own_op(op, before, Err(code)) } else { self.push(op, before, Err(code), None, None) };
                }
            };
            submit.records_expect.clone_from(&records);
            submit.records_write = records;
            let Some(session) = self.session.as_mut() else {
                self.restore_step(undo, step);
                return self.push(op, before, Err("not_open"), None, None);
            };
            let revision = session.document_revision();
            return match session.submit_multicam_as(&ctx, &submit, revision, cancel) {
                Ok(applied) => {
                    // The step's entry comes back with the sequences (lane SMB-PR-NEST-2,
                    // submit_sequence_edits_and_entry; refused: the sequence step stays applied,
                    // reported, never silently split).
                    let previous = match step.entry.as_ref().map(|e| self.swap_entry(e)) {
                        Some(Ok(p)) => p,
                        Some(Err(code)) => {
                            let revision = self.revision();
                            return self.push(op, before, Err(code), Some(applied.report), revision);
                        }
                        None => None,
                    };
                    if applied.changed && !applied.replayed {
                        self.emit_business_event(history_event, &ctx, applied.revision, &[]);
                    }
                    let detail = json!({"step": step.label, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                    let next = HistoryStep { label: step.label, inverse: Value::Null, entry: previous, timeline: None, group: None, sequences: applied.multicam_inverse.clone(), ctx, revision: applied.revision };
                    if undo {
                        self.redo_stack.push(next);
                    } else {
                        self.undo_stack.push(next);
                    }
                    self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
                }
                Err(f) => {
                    self.restore_step(undo, step);
                    let revision = self.revision();
                    self.push(op, before, Err(f.code()), Some(f.report), revision)
                }
            };
        }
        if step.inverse.is_null() {
            // An entry-only step: swap the session document entry, nothing to submit.
            let swapped = step.entry.as_ref().map(|e| self.swap_entry(e));
            return match swapped {
                Some(Ok(previous)) => {
                    let detail = json!({"step": step.label, "entry": step.entry.as_ref().map(|e| e.token.clone()), "changed": true});
                    let revision = self.revision().unwrap_or(0);
                    // Entry-only: no new revision, so the id comes from the swap itself (R48 F-R48-1);
                    // a swap that changed no byte emits nothing (F-R48-2).
                    if let (Some(restored), Some(replaced)) = (step.entry.as_ref(), previous.as_ref())
                        && restored.bytes != replaced.bytes
                    {
                        let sequence = hsk_studio_accord::entry_event_sequence(history_event, &restored.token, replaced.bytes.as_deref(), restored.bytes.as_deref(), &ctx);
                        let entry = self.business_event_entry(history_event, &ctx, revision, &[], Some(sequence));
                        self.pending_events.push(entry);
                    }
                    let entry = HistoryStep { label: step.label, inverse: Value::Null, entry: previous, timeline: None, group: None, sequences: None, ctx, revision };
                    if undo {
                        self.redo_stack.push(entry);
                    } else {
                        self.undo_stack.push(entry);
                    }
                    self.own_op(op, before, Ok(detail))
                }
                Some(Err(code)) => {
                    if undo {
                        self.undo_stack.push(step);
                    } else {
                        self.redo_stack.push(step);
                    }
                    self.own_op(op, before, Err(code))
                }
                None => self.own_op(op, before, Err("empty_history_step")),
            };
        }
        match self.submit_inverse(&step.inverse, &ctx) {
            Ok(applied) => {
                // The entry of the step comes back with the chronicle inverse (refused: the step
                // stays applied in chronicle, so it is reported, never silently split).
                let previous = match step.entry.as_ref().map(|e| self.swap_entry(e)) {
                    Some(Ok(p)) => p,
                    Some(Err(code)) => {
                        let revision = self.revision();
                        return self.push(op, before, Err(code), Some(applied.report), revision);
                    }
                    None => None,
                };
                let next = applied.inverse.as_ref().and_then(|i| serde_json::to_value(i).ok()).unwrap_or(Value::Null);
                // The values the step restored are what a later verify expects.
                for write in step.inverse["writes"].as_array().into_iter().flatten() {
                    let layer = write["address"]["target"]["node_id"].as_str().unwrap_or_default().to_string();
                    let field = match write["address"]["property"].as_str() {
                        Some("name") => "name",
                        Some("visible") => "visible",
                        Some("opacity") => "opacity",
                        Some("blend_mode") => "blend_mode",
                        _ => continue,
                    };
                    self.expectations.retain(|e| !(e.layer_id == layer && e.field == field));
                    self.expectations.push(Expectation { layer_id: layer, field, expected: write["value"]["value"].clone() });
                }
                if applied.changed && !applied.replayed {
                    self.emit_business_event(history_event, &ctx, applied.revision, &[]);
                }
                let detail = json!({"step": step.label, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                let entry = HistoryStep { label: step.label, inverse: next, entry: previous, timeline: None, group: None, sequences: None, ctx, revision: applied.revision };
                if undo {
                    self.redo_stack.push(entry);
                } else {
                    self.undo_stack.push(entry);
                }
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(fail) => {
                if undo {
                    self.undo_stack.push(step);
                } else {
                    self.redo_stack.push(step);
                }
                match fail {
                    Fail::Refused(code) if self.session.is_some() => self.own_op(op, before, Err(code)),
                    Fail::Refused(code) => self.push(op, before, Err(code), None, None),
                    Fail::Session(f) => {
                        let revision = self.revision();
                        self.push(op, before, Err(f.code()), Some(f.report), revision)
                    }
                }
            }
        }
    }

    fn submit_inverse(&mut self, inverse: &Value, ctx: &CommandContext) -> Result<Applied, Fail> {
        if !self.harness.provider_selected(CHRONICLE_PROVIDER_ID) {
            return Err(Fail::Refused("provider_not_selected"));
        }
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or(Fail::Refused("not_open"))?;
        // The history is last-in first-out: every later step was undone before this one, and
        // those undos advanced the revisions this inverse quotes. Its reads are refreshed to the
        // session's current vector; their expected values still guard the step (chronicle
        // recovery: invert only while the conditional reads hold). Without this, two edits of one
        // property could not both be undone (test `multi_step_history_undoes_every_step` in
        // tests/viewer_layers.rs).
        let mut reads = inverse["reads"].clone();
        for read in reads.as_array_mut().into_iter().flatten() {
            let revision = session
                .revisions()
                .iter()
                .find(|e| serde_json::to_value(&e.address).ok().as_ref() == Some(&read["address"]))
                .map_or(0, |e| e.revision);
            read["expected_revision"] = json!(revision);
        }
        // Inverse::invocation in wire form: fresh command, correlation and actor identity.
        let patch = json!({
            "transport_version": 1,
            "document_id": inverse["document_id"],
            "base_revision": session.document_revision(),
            "command_id": command_text(ctx),
            "correlation_id": ctx.correlation().value(),
            "actor": actor_json(ctx.actor()),
            "reads": reads,
            "writes": inverse["writes"],
        });
        let params = serde_json::to_vec(&patch).map_err(|_| Fail::Refused("patch_encoding"))?;
        let revision = session.document_revision();
        session.submit_as(ctx, CHRONICLE_PROVIDER_ID, &params, revision, cancel).map_err(Fail::Session)
    }

    /// Atomically saves the working session to `path`.
    pub fn save_to(&mut self, path: &Path) -> bool {
        self.note_unrecordable("document.save");
        let before = self.frames.count();
        let cancel = self.cancel;
        let Some(session) = self.session.as_mut() else {
            return self.push("save", before, Err("not_open"), None, None);
        };
        match session.save_to(path, cancel) {
            Ok(r) => {
                let detail = json!({"revision":r.revision,"entries":r.entries,"bytes_written":r.bytes_written,"previews_preserved":r.previews_preserved,"previews_dropped":r.previews_dropped,"quarantined_preserved":r.quarantined_preserved});
                self.push("save", before, Ok(detail), Some(r.report), Some(r.revision))
            }
            Err(f) => {
                let revision = self.revision();
                self.push("save", before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// Serializes the working session to verified in-memory package bytes.
    pub fn checkpoint(&mut self) -> Option<Vec<u8>> {
        self.note_unrecordable("document.checkpoint");
        let before = self.frames.count();
        let cancel = self.cancel;
        let Some(session) = self.session.as_mut() else {
            self.push("checkpoint", before, Err("not_open"), None, None);
            return None;
        };
        match session.export_checkpoint(cancel) {
            Ok(c) => {
                let detail = json!({"revision":c.revision,"bytes":c.bytes.len(),"sha256":c.sha256_hex,"entries":c.entries});
                self.push("checkpoint", before, Ok(detail), Some(c.report), Some(c.revision));
                Some(c.bytes)
            }
            Err(f) => {
                let revision = self.revision();
                self.push("checkpoint", before, Err(f.code()), Some(f.report), revision);
                None
            }
        }
    }

    /// Opens a second, independent session on saved bytes (the reopen half of save/reopen/verify).
    pub fn reopen_bytes(&mut self, bytes: &[u8]) -> bool {
        self.note_unrecordable("document.reopen");
        let before = self.frames.count();
        let opened = Session::open_bytes(bytes, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        self.finish_open("reopen", before, opened, true)
    }

    /// Opens a second, independent session on a saved file.
    pub fn reopen_path(&mut self, path: &Path) -> bool {
        self.note_unrecordable("document.reopen");
        let before = self.frames.count();
        let opened = Session::open_path(path, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        self.finish_open("reopen", before, opened, true)
    }

    /// Verifies the reopened session against the working one and every edit this run committed:
    /// same revision, byte-identical encoded document, and each edited value present.
    pub fn verify_reopened(&mut self) -> bool {
        self.note_unrecordable("document.verify");
        let before = self.frames.count();
        let (Some(live), Some(again)) = (self.session.as_ref(), self.reopened.as_ref()) else {
            return self.push("verify", before, Err("nothing_to_verify"), None, None);
        };
        let revision_match = live.document_revision() == again.document_revision();
        let bytes_equal = live.snapshot().encoded_bytes() == again.snapshot().encoded_bytes();
        let checks = self.check_expectations(again);
        let all = revision_match && bytes_equal && checks.iter().all(|c| c["ok"] == true);
        let detail = json!({"revision_match":revision_match,"encoded_bytes_equal":bytes_equal,"expectations":checks});
        let result = if all { Ok(detail.clone()) } else { Err("verify_mismatch") };
        let ok = self.own_op("verify", before, result);
        if !ok && let Some(record) = self.ops.last_mut() {
            record["detail"] = detail;
        }
        ok
    }

    /// Verifies the working session against caller expectations (no reopen). A verify that has
    /// nothing to check (no revision, no caller expectation, no edit committed earlier in this
    /// run) is refused as `nothing_to_check`, not reported as a pass (test
    /// `verify_checks_something_and_save_never_clobbers_the_input`).
    pub fn verify_live(&mut self, revision: Option<u64>, expectations: Vec<(String, &'static str, Value)>) -> bool {
        self.note_unrecordable("document.verify");
        let before = self.frames.count();
        for (layer_id, field, expected) in expectations {
            self.expectations.push(Expectation { layer_id, field, expected });
        }
        let Some(live) = self.session.as_ref() else {
            return self.push("verify", before, Err("not_open"), None, None);
        };
        if revision.is_none() && self.expectations.is_empty() {
            return self.own_op("verify", before, Err("nothing_to_check"));
        }
        let revision_ok = revision.is_none_or(|r| r == live.document_revision());
        let checks = self.check_expectations(live);
        let all = revision_ok && checks.iter().all(|c| c["ok"] == true);
        let detail = json!({"revision_match":revision_ok,"expectations":checks});
        let ok = self.own_op("verify", before, if all { Ok(detail.clone()) } else { Err("verify_mismatch") });
        if !ok && let Some(record) = self.ops.last_mut() {
            record["detail"] = detail;
        }
        ok
    }

    fn check_expectations(&self, session: &Session) -> Vec<Value> {
        self.expectations
            .iter()
            .map(|e| {
                let actual = session.snapshot().document().layers.iter().find(|l| l.layer_id == e.layer_id).map(|l| match e.field {
                    "name" => json!(l.name),
                    "opacity" => json!(l.effective_appearance().opacity),
                    "blend_mode" => json!(l.effective_appearance().blend_mode),
                    _ => json!(l.visible),
                });
                // Numbers compare by value (100 and 100.0 are the same opacity).
                let ok = match (actual.as_ref().and_then(Value::as_f64), e.expected.as_f64()) {
                    (Some(a), Some(b)) => a == b,
                    _ => actual.as_ref() == Some(&e.expected),
                };
                json!({"layer_id":e.layer_id,"field":e.field,"expected":e.expected,"actual":actual,"ok":ok})
            })
            .collect()
    }

    /// Finishes the run: the machine-readable report. `ok` only when at least one operation ran
    /// and every operation succeeded; a run that performed no operation is `nothing_checked`
    /// (`ok` false), not a pass (test `an_empty_run_is_nothing_checked_not_a_pass`).
    pub fn finish(self, command: &str) -> Value {
        let ran = !self.ops.is_empty();
        let ok = ran && self.ops.iter().all(|o| o["ok"] == true);
        let frames = self.frames.count();
        let per_op_one_frame = self.ops.iter().all(|o| o["observe"]["frames"] == 1 || o["observe"]["status"] == "unaddressed");
        let verdict = if !ran {
            "nothing_checked"
        } else if ok {
            "pass"
        } else {
            "fail"
        };
        json!({
            "schema": SCHEMA,
            "command": command,
            "ok": ok,
            "verdict": verdict,
            "selection": {"providers": self.harness.selection.providers, "references": self.harness.selection.references.name()},
            "ops": self.ops,
            "observe": {"frames_total": frames, "one_frame_per_operation": per_op_one_frame},
            "not_proven": NOT_PROVEN,
        })
    }
}

/// Builds the chronicle `Patch` JSON for one property edit. The address and revision come from the
/// session's own revision vector, so the patch quotes what the session holds (exercised by every
/// edit in `open_edit_save_reopen_verify_end_to_end`).
/// One chronicle patch of several `vector_paths` writes (lane SMB-ILLUSTRATOR-TOOLS-2).
fn patch_json_vector_paths(session: &Session, ctx: &CommandContext, writes: &[(String, Vec<String>)]) -> Result<Vec<u8>, &'static str> {
    let doc = session.snapshot().document();
    let mut reads = Vec::with_capacity(writes.len());
    let mut wire = Vec::with_capacity(writes.len());
    for (layer_id, path_ids) in writes {
        let layer = doc.layers.iter().find(|l| &l.layer_id == layer_id).ok_or("unknown_layer")?;
        let folio::Payload::Vector { path_ids: current } = &layer.payload else {
            return Err("not_a_vector_layer");
        };
        let address = json!({"target":{"node_kind":"layer","node_id":layer_id},"property":Prop::VectorPaths.field()});
        let revision = session
            .revisions()
            .iter()
            .find(|e| serde_json::to_value(&e.address).ok().as_ref() == Some(&address))
            .map(|e| e.revision)
            .ok_or("no_editable_property")?;
        reads.push(json!({"address": address, "expected_revision": revision, "expected_value": PropValue::PathIds(current.clone()).wire()}));
        wire.push(json!({"address": address, "value": PropValue::PathIds(path_ids.clone()).wire()}));
    }
    let patch = json!({
        "transport_version": 1,
        "document_id": doc.document_id,
        "base_revision": session.document_revision(),
        "command_id": command_text(ctx),
        "correlation_id": ctx.correlation().value(),
        "actor": actor_json(ctx.actor()),
        "reads": reads,
        "writes": wire,
    });
    serde_json::to_vec(&patch).map_err(|_| "patch_encoding")
}

fn patch_json(session: &Session, ctx: &CommandContext, layer_id: &str, prop: Prop, new: &PropValue) -> Result<Vec<u8>, &'static str> {
    let layer = session.snapshot().document().layers.iter().find(|l| l.layer_id == layer_id).ok_or("unknown_layer")?;
    let property = prop.field();
    let current = match prop {
        Prop::Name => PropValue::Text(layer.name.clone()),
        Prop::Visible => PropValue::Bool(layer.visible),
        Prop::Opacity => PropValue::Number(layer.effective_appearance().opacity),
        Prop::BlendMode => PropValue::Number(f64::from(layer.effective_appearance().blend_mode)),
        Prop::Tiles => match &layer.payload {
            folio::Payload::Raster { tiles } => PropValue::Tiles(tiles.clone()),
            _ => return Err("not_a_raster_layer"),
        },
        Prop::VectorPaths => match &layer.payload {
            folio::Payload::Vector { path_ids } => PropValue::PathIds(path_ids.clone()),
            _ => return Err("not_a_vector_layer"),
        },
    }
    .wire();
    let wanted = new.wire();
    let address = json!({"target":{"node_kind":"layer","node_id":layer_id},"property":property});
    let revision = session
        .revisions()
        .iter()
        .find(|e| serde_json::to_value(&e.address).ok().as_ref() == Some(&address))
        .map(|e| e.revision)
        .ok_or("no_editable_property")?;
    let patch = json!({
        "transport_version": 1,
        "document_id": session.snapshot().document().document_id,
        "base_revision": session.document_revision(),
        "command_id": command_text(ctx),
        "correlation_id": ctx.correlation().value(),
        "actor": actor_json(ctx.actor()),
        "reads": [{"address": address, "expected_revision": revision, "expected_value": current}],
        "writes": [{"address": address, "value": wanted}],
    });
    serde_json::to_vec(&patch).map_err(|_| "patch_encoding")
}

/// An enumerated placement ([STU-RAS-114], chronicle `PlaceMode`): containers are a group layer
/// id or `None` for the document root; directions follow the Photoshop document model (top-first
/// list; Studio reading, UNVERIFIED): beginning = top, end = bottom, before = directly above,
/// after = directly below, inside = top of the group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Inside(String),
    AtBeginning(Option<String>),
    AtEnd(Option<String>),
    Before(String),
    After(String),
}

impl Place {
    fn wire(&self) -> Value {
        let (mode, anchor) = match self {
            Place::Inside(g) => ("place_inside", Some(g)),
            Place::AtBeginning(c) => ("place_at_beginning", c.as_ref()),
            Place::AtEnd(c) => ("place_at_end", c.as_ref()),
            Place::Before(a) => ("place_before", Some(a)),
            Place::After(a) => ("place_after", Some(a)),
        };
        let mut v = json!({"mode": mode});
        if let Some(anchor) = anchor {
            v["anchor"] = json!(anchor);
        }
        v
    }
}

/// One `placement` command of [`Run::restructure`].
#[derive(Clone, Debug)]
pub struct PlacementWrite {
    pub layer_id: String,
    /// Where the layer goes; `None` deletes it.
    pub to: Option<Place>,
    /// Record and graph operation of a layer this command creates.
    pub create: Option<(StudioLayer, folio::Operation)>,
}

/// The chronicle placement state (reads) in wire form.
fn placement_state_wire(at: Option<&(Parent, u64)>) -> Value {
    let value = at.map(|(parent, index)| json!({"parent": serde_json::to_value(parent).unwrap_or(Value::Null), "index": index}));
    json!({"value_kind": "placement", "value": value})
}

/// The chronicle placement command (writes) in wire form.
fn place_wire(to: Option<&Place>, create: Option<&(StudioLayer, folio::Operation)>) -> Value {
    let value = to.map(|place| {
        let mut v = place.wire();
        if let Some((layer, operation)) = create {
            v["create"] = json!({"layer": serde_json::to_value(layer).unwrap_or(Value::Null), "operation": serde_json::to_value(operation).unwrap_or(Value::Null)});
        }
        v
    });
    json!({"value_kind": "place", "value": value})
}

/// The chronicle `command_id` of a command context: its registered command id (every mutating
/// `Run` command has one; the fallback names the harness without an ordinal).
fn command_text(ctx: &CommandContext) -> &str {
    ctx.command().map_or("harness.command", CommandId::as_str)
}

/// The chronicle `Actor` wire form of the run's attribution.
fn actor_json(actor: &ActorContext) -> Value {
    json!({
        "account_id": actor.account_id(), "principal_id": actor.principal_id(),
        "owner_account_id": actor.owner_account_id(), "owner_principal_id": actor.owner_principal_id(),
        "access_space_id": actor.access_space_id(), "session_id": actor.session_id(),
    })
}

#[cfg(test)]
mod business_event_tests {
    //! Lane SMB-BUSINESS-EVENTS-1: every `Run` command mints a context with a RUN_COMMANDS id, so
    //! a context without one cannot reach emission through the public API; this proves the guard
    //! (lead decision 2026-10-31: refused `missing_command_id`, counted, never emitted).
    use super::*;
    use hsk_studio_accord::{BusinessEventDeliveryError, BusinessEventSink, ClientSessionId, TransientCorrelation};

    const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");

    #[derive(Default)]
    struct Capture(Vec<BusinessEventRecord>);

    impl BusinessEventSink for Capture {
        fn try_emit(&mut self, record: &BusinessEventRecord) -> Result<(), BusinessEventDeliveryError> {
            self.0.push(record.clone());
            Ok(())
        }
    }

    #[test]
    fn a_commit_without_a_command_id_is_refused_and_counted() {
        let harness = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
        let cancel = CancellationToken::default();
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let capture = Arc::new(Mutex::new(Capture::default()));
        let sink: crate::context::SharedBusinessEventSink = capture.clone();
        let context = RunContext::new(actor, ClientSessionId::parse("unit").unwrap(), Arc::new(TransientCorrelation::new())).with_business_events(sink);
        let mut run = harness.run(context, &cancel);
        assert!(run.open_bytes(FIXTURE));
        let revision = run.revision().unwrap();
        let no_command = run.context.command(None, revision, None);
        run.emit_business_event(StudioBusinessEventId::LayerMutated, &no_command, revision, &[]);
        assert_eq!(run.pending_events, [json!({"event": "STUDIO_LAYER_MUTATED", "delivery": "refused", "error": "missing_command_id"})]);
        assert!(capture.lock().unwrap().0.is_empty(), "nothing reached the sink");
        assert_eq!(run.business_events()["refused_missing_command_id"], 1);
        assert_eq!(run.business_events()["refused"], 1);
        // The next record carries the refusal.
        assert!(run.summary());
        assert_eq!(run.ops().last().unwrap()["business_events"][0]["error"], "missing_command_id");
    }
}
