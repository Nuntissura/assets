//! Lane SMB-BUSINESS-EVENTS-1 (RF-07; STU-CON-013, STU-CON-014): `Run` emits an accord
//! `BusinessEventRecord` through its context's optional `BusinessEventSink` after the session
//! accepted a commit, never on a refusal; the outcome is in the op record (`business_events`) and
//! in `Run::business_events`. A capture sink stands in for the host's (the asset-local envelope
//! sink is kernel-bindings `TransientBusinessEvents`; wiring a sink into a running host is
//! MT-36230). Correlations start at 1000, so an op ordinal can never pass for one.
//! Fixture: the harness crate's `tests/fixtures/demo.handshake` (raster layer ...0002).

use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_harness_core::{
    BusinessEventDeliveryError, BusinessEventRecord, BusinessEventSink, ClientSessionId, Harness, Place, PlacementWrite, PlayMode,
    Run, RunContext, Selection, SharedBusinessEventSink, StudioBusinessEventId, TransientCorrelation,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const VEC: &str = "SLYR-019abcde-0000-7000-8000-0000000000bb";
const PATH: &str = "SVPT-019abcde-0000-7000-8000-0000000000b1";
const ACT: &str = "SACT-019abcde-0000-7000-8000-0000000000a1";
const DECOY: &str = "Quarterly Secret Poster";

/// A capture sink: keeps every record, or answers every emit with `fail`.
#[derive(Default)]
struct Capture {
    records: Vec<BusinessEventRecord>,
    fail: Option<BusinessEventDeliveryError>,
}

impl BusinessEventSink for Capture {
    fn try_emit(&mut self, record: &BusinessEventRecord) -> Result<(), BusinessEventDeliveryError> {
        match self.fail {
            Some(error) => Err(error),
            None => {
                self.records.push(record.clone());
                Ok(())
            }
        }
    }
}

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn actor() -> ActorContext {
    ActorContext::new("events-account", "events-principal", "owner", "owner-principal", "space", "events-session").unwrap()
}

fn plain_context() -> RunContext {
    RunContext::new(actor(), ClientSessionId::parse("events-window").unwrap(), Arc::new(TransientCorrelation::starting_at(1000)))
}

fn context_with(capture: &Arc<Mutex<Capture>>) -> RunContext {
    let sink: SharedBusinessEventSink = capture.clone();
    plain_context().with_business_events(sink)
}

fn captured(capture: &Arc<Mutex<Capture>>) -> Vec<BusinessEventRecord> {
    capture.lock().unwrap().records.clone()
}

fn last(run: &Run<'_>) -> Value {
    run.ops().last().cloned().unwrap()
}

fn new_vector_layer() -> PlacementWrite {
    let record = serde_json::from_value(json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": VEC, "name": "vector", "parent": null,
        "kind": "vector", "payload": {"payload_kind": "vector", "path_ids": []}, "visible": true,
    }))
    .unwrap();
    let operation = serde_json::from_value(json!({"layer_id": VEC, "operation_key": "source", "operation_kind": "source", "inputs": [],
        "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
    .unwrap();
    PlacementWrite { layer_id: VEC.to_string(), to: Some(Place::AtBeginning(None)), create: Some((record, operation)) }
}

/// Runs `call`, which must succeed, and returns the ONE record it emitted, checked against the
/// op record: same event, event id, revision, correlation, command id and causation.
fn one_event(run: &mut Run<'_>, capture: &Arc<Mutex<Capture>>, event: StudioBusinessEventId, call: impl FnOnce(&mut Run<'_>) -> bool) -> BusinessEventRecord {
    let before = captured(capture).len();
    assert!(call(run), "{}", last(run));
    let after = captured(capture);
    assert_eq!(after.len(), before + 1, "exactly one event: {}", last(run));
    let record = after.last().unwrap().clone();
    let op = last(run);
    assert_eq!(record.event(), event, "{op}");
    assert_eq!(op["business_events"], json!([{"event": event.as_str(), "event_id": record.event_id(), "delivery": "delivered"}]), "{op}");
    assert_eq!(Some(record.revision()), op["revision"].as_u64(), "the exact successor revision: {op}");
    assert_eq!(record.revision(), run.snapshot().unwrap().document().revision, "emitted after the commit");
    assert_eq!(record.document().as_str(), run.snapshot().unwrap().document().document_id);
    let ctx = record.context();
    assert_eq!((ctx.actor(), ctx.client_session().as_str()), (&actor(), "events-window"));
    assert_eq!(Some(ctx.correlation().value()), op["correlation"].as_u64(), "{op}");
    assert_eq!(ctx.command().map(|c| c.as_str()), op["command_id"].as_str(), "{op}");
    assert_eq!(ctx.causation().map(|c| c.value()), op["causation"].as_u64(), "{op}");
    record
}

#[test]
fn commits_emit_their_event_after_the_commit_with_the_run_attribution() {
    let h = harness();
    let cancel = CancellationToken::default();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut run = h.run(context_with(&capture), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(last(&run).get("business_events").is_none(), "an open is not a business event");
    assert!(captured(&capture).is_empty());

    let rename = one_event(&mut run, &capture, StudioBusinessEventId::LayerMutated, |run| run.rename(L2, DECOY, None));
    assert_eq!(rename.context().command().unwrap().as_str(), "layer.rename");
    assert_eq!(rename.artifacts().iter().map(|a| a.as_str()).collect::<Vec<_>>(), [L2]);
    assert!(rename.context().correlation().value() >= 1000);
    let created = one_event(&mut run, &capture, StudioBusinessEventId::LayerCreated, |run| run.restructure("new_vector", &[new_vector_layer()]));
    assert_eq!(created.artifacts()[0].as_str(), VEC);
    let path = one_event(&mut run, &capture, StudioBusinessEventId::VectorPathEdited, |run| run.set_vector_paths("pen", VEC, vec![PATH.to_string()]));
    let undone = one_event(&mut run, &capture, StudioBusinessEventId::HistoryUndone, |run| run.undo());
    assert_eq!(undone.context().causation().map(|c| c.value()), Some(path.context().correlation().value()), "the undo names the step it reverses");
    let redone = one_event(&mut run, &capture, StudioBusinessEventId::HistoryRedone, |run| run.redo());
    assert_eq!(redone.context().causation().map(|c| c.value()), Some(undone.context().correlation().value()));

    // Event ids are deterministic per document revision and never repeat in a run.
    let records = captured(&capture);
    let ids: std::collections::BTreeSet<String> = records.iter().map(BusinessEventRecord::event_id).collect();
    assert_eq!(ids.len(), records.len());
    assert_eq!(records[0].event_id(), format!("{}@{}#0", records[0].document().as_str(), records[0].revision()));
    // Ids only: the decoy layer name is in the document, never in a record.
    assert_eq!(run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == L2).unwrap().name, DECOY);
    assert!(!format!("{records:?}").contains(DECOY));
    assert_eq!(run.business_events()["delivered"], 5);
}

#[test]
fn refused_and_unchanged_commits_emit_nothing_and_no_sink_is_recorded() {
    let h = harness();
    let cancel = CancellationToken::default();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut run = h.run(context_with(&capture), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let stale = run.snapshot().unwrap().document().revision;
    assert!(run.rename(L2, "first", None));
    let n = captured(&capture).len();
    // A refused commit: unknown layer, then a stale expected revision.
    assert!(!run.rename("SLYR-019abcde-0000-7000-8000-0000000000ff", "x", None));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    assert!(!run.rename(L2, "second", Some(stale)));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    // An accepted request that changed nothing has no successor revision: nothing to emit.
    assert!(run.rename(L2, "first", None));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    assert_eq!(captured(&capture).len(), n, "refusals and no-change edits emit nothing");

    // No sink configured: nothing is emitted, and the record says so.
    let mut bare = h.run(plain_context(), &cancel);
    assert!(bare.open_bytes(FIXTURE));
    assert!(bare.rename(L2, "renamed", None));
    let op = last(&bare);
    assert_eq!(op["business_events"][0]["delivery"], "no_sink", "{op}");
    assert_eq!(op["business_events"][0]["event"], "STUDIO_LAYER_MUTATED", "{op}");
    assert_eq!(bare.business_events(), json!({"delivered": 0, "failed": 0, "no_sink": 1, "refused": 0, "refused_missing_command_id": 0, "reconciliation_required": false}));
}

#[test]
fn a_sink_failure_leaves_the_commit_and_history_intact_and_is_recorded() {
    let h = harness();
    let cancel = CancellationToken::default();
    for (fail, reconcile) in [(BusinessEventDeliveryError::Indeterminate, true), (BusinessEventDeliveryError::Saturated, false)] {
        let capture = Arc::new(Mutex::new(Capture { records: Vec::new(), fail: Some(fail) }));
        let mut run = h.run(context_with(&capture), &cancel);
        assert!(run.open_bytes(FIXTURE));
        let opened = run.snapshot().unwrap().document().revision;
        assert!(run.rename(L2, "kept", None), "the commit stands: {}", last(&run));
        let op = last(&run);
        assert_eq!(op["ok"], true);
        assert_eq!(op["business_events"][0]["delivery"], "failed", "{op}");
        assert_eq!(op["business_events"][0]["error"], fail.code(), "{op}");
        assert_eq!(op["business_events"][0]["reconciliation_required"], reconcile, "{op}");
        assert!(run.snapshot().unwrap().document().revision > opened);
        assert_eq!(run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == L2).unwrap().name, "kept");
        assert_eq!(run.history()["undo"].as_array().unwrap().len(), 1, "{}", run.history());
        assert_eq!(run.business_events()["failed"], 1);
        assert_eq!(run.business_events()["reconciliation_required"], reconcile, "only indeterminate needs reconciliation");
        // Undo still works and reports its own delivery.
        assert!(run.undo(), "{}", last(&run));
        assert_eq!(last(&run)["business_events"][0]["event"], "STUDIO_HISTORY_UNDONE");
    }
}

#[test]
fn actions_and_entry_steps_emit_per_commit_with_their_causation() {
    let h = harness();
    let cancel = CancellationToken::default();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut run = h.run(context_with(&capture), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.start_recording(ACT, "two renames"));
    assert!(run.rename(L2, "one", None));
    assert!(run.rename(L2, "two", None));
    let (action, _) = run.stop_recording().unwrap();
    let n = captured(&capture).len();
    assert!(run.rename(L2, "base", None));
    assert!(run.play_action(&action, PlayMode::Headless), "{}", last(&run));
    let play = last(&run);
    let inner: Vec<BusinessEventRecord> = captured(&capture).split_off(n + 1);
    assert_eq!(inner.len(), 2, "each inner commit emits its own event");
    for record in &inner {
        assert_eq!(record.event(), StudioBusinessEventId::LayerMutated);
        assert_eq!(record.context().causation().map(|c| c.value()), play["correlation"].as_u64(), "caused by the action: {play}");
    }
    assert_eq!(play["command_id"], "action.play");

    // The develop entry of a tile step maps to RAW_DEVELOPED (entry-only: no tile change).
    let before = captured(&capture).len();
    assert!(run.set_tiles_and_entry("develop", L2, None, "develop", b"{}".to_vec()), "{}", last(&run));
    let records = captured(&capture);
    assert_eq!(records.len(), before + 1);
    assert_eq!(records.last().unwrap().event(), StudioBusinessEventId::RawDeveloped);
    assert_eq!(last(&run)["business_events"][0]["event"], "STUDIO_RAW_DEVELOPED", "{}", last(&run));
    // An unmapped token (selection is view state) emits nothing.
    assert!(run.set_tiles_and_entry("select", L2, None, "selection", b"{}".to_vec()), "{}", last(&run));
    assert_eq!(captured(&capture).len(), before + 1);
}

/// A sink that keeps the first record per event id and counts a different record under a stored id
/// (the immutable-envelope collision refusal of a host or kernel-bindings sink) and an identical one
/// (a replay).
#[derive(Default)]
struct ById {
    records: std::collections::BTreeMap<String, BusinessEventRecord>,
    collisions: u64,
    replays: u64,
}

impl BusinessEventSink for ById {
    fn try_emit(&mut self, record: &BusinessEventRecord) -> Result<(), BusinessEventDeliveryError> {
        match self.records.get(&record.event_id()) {
            Some(stored) if stored == record => {
                self.replays += 1;
                Ok(())
            }
            Some(_) => {
                self.collisions += 1;
                Err(BusinessEventDeliveryError::Rejected)
            }
            None => {
                self.records.insert(record.event_id(), record.clone());
                Ok(())
            }
        }
    }
}

const ENTRY_BIT: u64 = 1 << 63;

/// R48 F-R48-1 (lane SMB-R47-FOLLOWUPS-1): two runs on one saved package, each with an entry-only
/// develop edit at the same document revision, mint disjoint event ids and no counted collision.
/// Measured first: every run builds its context as a host would after a restart, with its
/// correlations again from 1000 (the asset-local test host's `TransientCorrelation::new()` restarts
/// at 1 per CLI invocation the same way), so the two runs' develop commands carry the SAME
/// correlation and command id: neither identifies the event across runs. The entry-only id comes
/// from the entry change itself (accord `entry_event_sequence`, top bit set); the commit that made
/// the revision keeps sequence 0 of it.
#[test]
fn entry_only_events_of_two_runs_on_one_saved_package_have_disjoint_ids() {
    let h = harness();
    let cancel = CancellationToken::default();
    let sink = Arc::new(Mutex::new(ById::default()));
    let context = || {
        let shared: SharedBusinessEventSink = sink.clone();
        plain_context().with_business_events(shared)
    };
    // Run 0 makes the revision (a commit, LAYER_MUTATED at R#0) and saves.
    let mut run0 = h.run(context(), &cancel);
    assert!(run0.open_bytes(FIXTURE));
    assert!(run0.rename(L2, "saved", None), "{}", last(&run0));
    let revision = run0.snapshot().unwrap().document().revision;
    let saved = run0.checkpoint().expect("checkpoint");
    let mut develop = Vec::new();
    for exposure in ["0.5", "0.7"] {
        let mut run = h.run(context(), &cancel);
        assert!(run.open_bytes(&saved));
        assert!(run.set_tiles_and_entry("develop", L2, None, "develop", format!("{{\"exposure\":{exposure}}}").into_bytes()), "{}", last(&run));
        let op = last(&run);
        assert_eq!(run.snapshot().unwrap().document().revision, revision, "entry-only: no new revision");
        assert_eq!(op["business_events"][0]["delivery"], "delivered", "{op}");
        develop.push(op);
    }
    // The measurement: both runs' develop commands carry the same correlation and command id.
    assert_eq!(develop[0]["correlation"], develop[1]["correlation"], "a restarted correlation source");
    assert_eq!(develop[0]["command_id"], develop[1]["command_id"]);
    let sink = sink.lock().unwrap();
    assert_eq!((sink.collisions, sink.replays), (0, 0), "no counted collision");
    let records: Vec<&BusinessEventRecord> = sink.records.values().collect();
    assert_eq!(records.len(), 3, "LAYER_MUTATED and two RAW_DEVELOPED: {:?}", sink.records.keys().collect::<Vec<_>>());
    let raw: Vec<&&BusinessEventRecord> = records.iter().filter(|r| r.event() == StudioBusinessEventId::RawDeveloped).collect();
    assert_eq!(raw.len(), 2);
    assert!(raw.iter().all(|r| r.revision() == revision && r.sequence() & ENTRY_BIT != 0), "entry-only sequences carry the top bit");
    assert_ne!(raw[0].event_id(), raw[1].event_id());
    let commit = records.iter().find(|r| r.event() == StudioBusinessEventId::LayerMutated).unwrap();
    assert_eq!((commit.revision(), commit.sequence()), (revision, 0));
}

/// R48 F-R48-2: the `text` and `vector` entry tokens map to TEXT_EDITED and VECTOR_PATH_EDITED; an
/// entry rewritten with identical bytes emits nothing (as a no-change commit), and the undo of such a
/// step swaps identical bytes and emits nothing either, while the undo of a real entry change emits
/// HISTORY_UNDONE with an entry-only id.
#[test]
fn text_and_vector_entries_emit_their_events_and_identical_bytes_emit_nothing() {
    let h = harness();
    let cancel = CancellationToken::default();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut run = h.run(context_with(&capture), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let revision = run.snapshot().unwrap().document().revision;
    let n = captured(&capture).len();
    assert!(run.set_tiles_and_entry("type", L2, None, "text", b"{\"stories\":[1]}".to_vec()), "{}", last(&run));
    assert!(run.set_vector_paths_and_entry("pen", &[], Some(("vector", b"{\"paths\":[1]}".to_vec()))), "{}", last(&run));
    let records = captured(&capture);
    let events: Vec<StudioBusinessEventId> = records[n..].iter().map(BusinessEventRecord::event).collect();
    assert_eq!(events, [StudioBusinessEventId::TextEdited, StudioBusinessEventId::VectorPathEdited]);
    for record in &records[n..] {
        assert_eq!(record.revision(), revision, "entry-only events are minted at the open revision");
        assert_ne!(record.sequence() & ENTRY_BIT, 0, "entry-only sequence");
    }
    assert_eq!(last(&run)["business_events"][0]["event"], "STUDIO_VECTOR_PATH_EDITED", "{}", last(&run));

    // Identical bytes: the steps succeed and emit nothing.
    let n = captured(&capture).len();
    assert!(run.set_tiles_and_entry("type", L2, None, "text", b"{\"stories\":[1]}".to_vec()), "{}", last(&run));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    assert!(run.set_vector_paths_and_entry("pen", &[], Some(("vector", b"{\"paths\":[1]}".to_vec()))), "{}", last(&run));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    assert_eq!(captured(&capture).len(), n, "no-change entry writes emit nothing");
    // Undo of the no-change vector step swaps identical bytes: nothing.
    assert!(run.undo(), "{}", last(&run));
    assert!(last(&run).get("business_events").is_none(), "{}", last(&run));
    assert_eq!(captured(&capture).len(), n);
    // Undo of the no-change text step: nothing; then the undo of the real vector change: HISTORY_UNDONE.
    assert!(run.undo(), "{}", last(&run));
    assert_eq!(captured(&capture).len(), n);
    assert!(run.undo(), "{}", last(&run));
    let records = captured(&capture);
    assert_eq!(records.len(), n + 1, "{}", last(&run));
    let undone = records.last().unwrap();
    assert_eq!((undone.event(), undone.revision()), (StudioBusinessEventId::HistoryUndone, revision));
    assert_ne!(undone.sequence() & ENTRY_BIT, 0);
    assert_eq!(run.document_entry("vector"), None, "the vector entry is gone again");
    let ids: std::collections::BTreeSet<String> = records.iter().map(BusinessEventRecord::event_id).collect();
    assert_eq!(ids.len(), records.len(), "every id of the run is distinct");
}

/// A 1x1 RGBA PNG (precomputed bytes; no codec in this test).
#[cfg(feature = "import-raster")]
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63,
    0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

#[cfg(feature = "import-raster")]
#[test]
fn an_import_creates_the_document_then_completes() {
    let h = harness();
    let cancel = CancellationToken::default();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut run = h.run(context_with(&capture), &cancel);
    assert!(run.open_import(PNG_1X1), "{}", last(&run));
    let op = last(&run);
    let events: Vec<StudioBusinessEventId> = captured(&capture).iter().map(BusinessEventRecord::event).collect();
    assert_eq!(events, [StudioBusinessEventId::DocumentCreated, StudioBusinessEventId::ImportCompleted], "{op}");
    assert_eq!(op["command_id"], "document.import", "{op}");
    assert_eq!(op["business_events"].as_array().unwrap().len(), 2, "{op}");
    let records = captured(&capture);
    assert_eq!((records[0].sequence(), records[1].sequence()), (0, 1), "two events of one revision");
}

#[cfg(not(feature = "import-raster"))]
#[test]
fn an_import_creates_the_document_then_completes() {
    println!(
        "NOT_PROVEN: import business events (DOCUMENT_CREATED then IMPORT_COMPLETED) need an import route; harness-core's round unit is default (run with --features hsk-studio-harness-core/import-raster)"
    );
}
