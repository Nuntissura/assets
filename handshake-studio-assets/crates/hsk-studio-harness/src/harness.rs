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

use crate::panel::{self, PanelValues, Route};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{self as folio, Resolution, Resolver, StudioLayer, TileRef};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observation, Observe, Outcome, SinkPort};
use hsk_studio_session::{
    Applied, CHRONICLE_PROVIDER_ID, ChronicleProvider, Failed, ProviderRegistry, ReportStatus, Session, SessionConfig,
    SessionPorts,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const SCHEMA: &str = "hsk.studio.harness@1";

/// Providers the harness knows how to register. A provider must be named in the selection to be
/// usable; nothing is registered by default.
pub const AVAILABLE_PROVIDERS: &[&str] = &[CHRONICLE_PROVIDER_ID];

/// Capabilities the harness cannot prove today (listed in every report's `not_proven`).
pub const NOT_PROVEN: &[&str] = &[
    "visual_render_proof (the run itself renders nothing; the feature `viewer` renders through composite + render-gpu/render-cpu, asserted on a synthetic document only)",
    "appearance_commit (chronicle has no appearance property; appearance controls are read-only projections)",
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

pub(crate) struct TrustAll;
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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
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

    pub fn run<'a>(&'a self, actor: ActorContext, cancel: &'a CancellationToken) -> Run<'a> {
        Run {
            harness: self,
            cancel,
            frames: Frames::default(),
            actor,
            ops: Vec::new(),
            session: None,
            reopened: None,
            expectations: Vec::new(),
        }
    }
}

/// The attribution the harness uses unless the caller supplies one.
pub fn default_actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .expect("static actor literals satisfy accord bounds")
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
        ReportStatus::Delivered(r) => json!({"frames":frames,"status":"delivered","outcome":outcome_name(r.outcome)}),
        ReportStatus::Undelivered(e) => json!({"frames":frames,"status":"undelivered","error":e.code()}),
        ReportStatus::Unaddressed => json!({"frames":frames,"status":"unaddressed"}),
    }
}

#[derive(Clone, Copy)]
enum Prop {
    Name,
    Visible,
}

enum PropValue {
    Text(String),
    Bool(bool),
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
    actor: ActorContext,
    ops: Vec<Value>,
    session: Option<Session>,
    reopened: Option<Session>,
    expectations: Vec<Expectation>,
}

impl Run<'_> {
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
        let mut record = json!({"op":op,"n":self.op_no(),"ok":ok,"revision":revision,"observe":observe});
        match result {
            Ok(detail) => record["detail"] = detail,
            Err(code) => record["error"] = json!(code),
        }
        self.ops.push(record);
        ok
    }

    /// Frame owned by the harness for an operation no session performed.
    fn emit_own(&self, outcome: Outcome) -> Option<ReportStatus> {
        let session = self.session.as_ref()?;
        let doc = session.snapshot().document();
        let resource = DomainId::parse(&doc.document_id).ok()?;
        let budget = Budget::new(0, 0).ok()?;
        let n = self.op_no();
        let mut observe = Observe::new(n, doc.revision, resource, self.actor.clone(), budget);
        let observation = Observation { correlation_id: n, revision: doc.revision, outcome, progress: None, private_project_text: None };
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
        let report = self.emit_own(outcome);
        let revision = self.revision();
        self.push(op, before, result, report, revision)
    }

    fn layer(&self, id: &str) -> Option<&StudioLayer> {
        self.session.as_ref()?.snapshot().document().layers.iter().find(|l| l.layer_id == id)
    }

    // ----- operations ----------------------------------------------------------------------

    /// Opens a package file as the working session.
    pub fn open_path(&mut self, path: &Path) -> bool {
        let before = self.frames.count();
        let opened = Session::open_path(path, SessionConfig::new(self.actor.clone()), self.ports(), self.cancel);
        self.finish_open("open", before, opened, false)
    }

    /// Opens package bytes as the working session.
    pub fn open_bytes(&mut self, bytes: &[u8]) -> bool {
        let before = self.frames.count();
        let opened = Session::open_bytes(bytes, SessionConfig::new(self.actor.clone()), self.ports(), self.cancel);
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
        let before = self.frames.count();
        let resolver = self.harness.resolver();
        let (package, imported) = match crate::import::convert_to_native(bytes, resolver.as_ref(), self.cancel) {
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
        let opened = Session::open_bytes(&package, SessionConfig::new(self.actor.clone()), self.ports(), self.cancel);
        let ok = self.finish_open("import", before, opened, false);
        if let Some(record) = self.ops.last_mut() {
            if record["detail"].is_object() {
                record["detail"]["import"] = summary;
            } else {
                record["detail"] = json!({"import": summary});
            }
        }
        ok.then(|| (package, imported.receipt))
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
                }
                self.push(op, before, Ok(detail), Some(report), Some(revision))
            }
            Err(f) => self.push(op, before, Err(f.code()), Some(f.report), None),
        }
    }

    /// Read-only summary of the working document.
    pub fn summary(&mut self) -> bool {
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
        self.property_edit("rename", layer_id, Prop::Name, PropValue::Text(name.to_string()), expected_revision)
    }

    /// Shows or hides a layer through the selected chronicle provider.
    pub fn set_visible(&mut self, layer_id: &str, visible: bool, expected_revision: Option<u64>) -> bool {
        self.property_edit("set_visible", layer_id, Prop::Visible, PropValue::Bool(visible), expected_revision)
    }

    /// Edits a panel control: the value is parsed and validated by the control descriptor, applied
    /// to the panel's `ValueStore`, then committed through the control's route. A refused commit
    /// is undone in the store. Appearance controls have no route and are refused.
    pub fn panel_set(&mut self, layer_id: &str, control_id: &str, text: &str, expected_revision: Option<u64>) -> bool {
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
            _ => return self.own_op("panel_set", before, Err("unsupported_route")),
        };
        let ok = self.property_edit("panel_set", layer_id, prop, new, expected_revision);
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

    fn property_edit(&mut self, op: &str, layer_id: &str, prop: Prop, new: PropValue, expected_revision: Option<u64>) -> bool {
        let before = self.frames.count();
        let n = self.op_no();
        let result = self.submit_property(layer_id, prop, &new, expected_revision, n);
        match result {
            Ok(applied) => {
                let field = match prop {
                    Prop::Name => "name",
                    Prop::Visible => "visible",
                };
                let expected = match &new {
                    PropValue::Text(t) => json!(t),
                    PropValue::Bool(b) => json!(b),
                };
                self.expectations.push(Expectation { layer_id: layer_id.to_string(), field, expected });
                let detail = json!({
                    "layer_id": layer_id, "field": field, "provider": applied.provider, "command": applied.command_id,
                    "previous_revision": applied.previous_revision, "revision": applied.revision,
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

    fn submit_property(&mut self, layer_id: &str, prop: Prop, new: &PropValue, expected: Option<u64>, n: u64) -> Result<Applied, Fail> {
        if !self.harness.provider_selected(CHRONICLE_PROVIDER_ID) {
            return Err(Fail::Refused("provider_not_selected"));
        }
        let cancel = self.cancel;
        let session = self.session.as_mut().ok_or(Fail::Refused("not_open"))?;
        let params = patch_json(session, &self.actor, layer_id, prop, new, n).map_err(Fail::Refused)?;
        let revision = expected.unwrap_or_else(|| session.document_revision());
        session.submit(CHRONICLE_PROVIDER_ID, &params, revision, cancel).map_err(Fail::Session)
    }

    /// Atomically saves the working session to `path`.
    pub fn save_to(&mut self, path: &Path) -> bool {
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
        let before = self.frames.count();
        let opened = Session::open_bytes(bytes, SessionConfig::new(self.actor.clone()), self.ports(), self.cancel);
        self.finish_open("reopen", before, opened, true)
    }

    /// Opens a second, independent session on a saved file.
    pub fn reopen_path(&mut self, path: &Path) -> bool {
        let before = self.frames.count();
        let opened = Session::open_path(path, SessionConfig::new(self.actor.clone()), self.ports(), self.cancel);
        self.finish_open("reopen", before, opened, true)
    }

    /// Verifies the reopened session against the working one and every edit this run committed:
    /// same revision, byte-identical encoded document, and each edited value present.
    pub fn verify_reopened(&mut self) -> bool {
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
                    _ => json!(l.visible),
                });
                json!({"layer_id":e.layer_id,"field":e.field,"expected":e.expected,"actual":actual,"ok":actual.as_ref() == Some(&e.expected)})
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
fn patch_json(session: &Session, actor: &ActorContext, layer_id: &str, prop: Prop, new: &PropValue, n: u64) -> Result<Vec<u8>, &'static str> {
    let layer = session.snapshot().document().layers.iter().find(|l| l.layer_id == layer_id).ok_or("unknown_layer")?;
    let (property, current) = match prop {
        Prop::Name => ("name", json!({"value_kind":"string","value":layer.name})),
        Prop::Visible => ("visible", json!({"value_kind":"bool","value":layer.visible})),
    };
    let wanted = match new {
        PropValue::Text(t) => json!({"value_kind":"string","value":t}),
        PropValue::Bool(b) => json!({"value_kind":"bool","value":b}),
    };
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
        "command_id": format!("harness-op-{n}"),
        "correlation_id": n,
        "actor": {
            "account_id": actor.account_id(), "principal_id": actor.principal_id(),
            "owner_account_id": actor.owner_account_id(), "owner_principal_id": actor.owner_principal_id(),
            "access_space_id": actor.access_space_id(), "session_id": actor.session_id(),
        },
        "reads": [{"address": address, "expected_revision": revision, "expected_value": current}],
        "writes": [{"address": address, "value": wanted}],
    });
    serde_json::to_vec(&patch).map_err(|_| "patch_encoding")
}
