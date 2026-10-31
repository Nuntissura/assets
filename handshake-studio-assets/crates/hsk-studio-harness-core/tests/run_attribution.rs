//! Lane SMB-ATTRIBUTION-1 (RF-06; GUI-DESKTOP-029/041/045/050, STU-ARC-014/015): a `Run` takes its
//! attribution from the caller's `RunContext`. Every op record carries the actor and, for a command,
//! its command id and correlation; history steps carry actor, command id, base revision, revision
//! and correlation; harness-owned Observe frames and chronicle patches carry the command
//! correlation, never the op ordinal `n`; undo / redo are fresh commands caused by the step they
//! reverse. Correlation sources start at 1000 / 5000, so an ordinal can never pass for one.
//! Fixture: the harness crate's `tests/fixtures/demo.handshake` (raster layer ...0002).
//! Session-owned report frames of open and save keep the session's own counter; since lane
//! SMB-R47-FOLLOWUPS-1 (R48 F-R48-6) every commit frame carries the command correlation
//! (`commit_frames_carry_the_command_correlation`).

use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_harness_core::{ClientSessionId, Harness, Run, RunContext, Selection, TransientCorrelation};
use serde_json::Value;
use std::sync::Arc;

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

/// A caller context: its own actor (session id `<who>-session`), client session and correlations
/// from `first`.
fn context(who: &str, first: u64) -> RunContext {
    let actor = ActorContext::new(&format!("{who}-account"), &format!("{who}-principal"), "owner", "owner-principal", "space", &format!("{who}-session")).unwrap();
    RunContext::new(actor, ClientSessionId::parse(&format!("{who}-window")).unwrap(), Arc::new(TransientCorrelation::starting_at(first)))
}

fn last(run: &Run<'_>) -> Value {
    run.ops().last().cloned().unwrap()
}

fn correlation(record: &Value) -> u64 {
    record["correlation"].as_u64().unwrap_or_else(|| panic!("no correlation: {record}"))
}

/// open, rename, summary under `context`; checks every record and the history step against it.
fn attributed_run(who: &str, first: u64) {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(context(who, first), &cancel);
    let session = format!("{who}-session");
    assert_eq!(run.actor().session_id(), session);
    assert!(run.open_bytes(FIXTURE), "{}", last(&run));
    let opened = run.snapshot().unwrap().document().revision;
    assert!(run.rename(L2, &format!("{who}-name"), None), "{}", last(&run));
    let rename = last(&run);
    assert!(run.summary(), "{}", last(&run));
    let summary = last(&run);
    for record in run.ops() {
        assert_eq!(record["actor"]["session_id"], session, "{record}");
        assert_eq!(record["actor"]["account_id"], format!("{who}-account"), "{record}");
    }
    let c = correlation(&rename);
    assert!((first..first + 100).contains(&c), "{rename}");
    assert_eq!(rename["command_id"], "layer.rename", "{rename}");
    assert_eq!(rename["detail"]["correlation_id"], c, "the chronicle patch carries the command correlation: {rename}");
    assert_eq!(rename["detail"]["command"], "layer.rename", "{rename}");
    // The harness-owned summary frame carries a correlation of this run's source.
    let s = correlation(&summary);
    assert!((first..first + 100).contains(&s) && s != c, "{summary}");
    assert_eq!(summary["observe"]["correlation"], s, "{summary}");
    assert!(summary["command_id"].is_null(), "no registered command for a summary: {summary}");
    let step = &run.history()["undo_steps"][0];
    assert_eq!(step["actor"]["session_id"], session, "{step}");
    assert_eq!(step["client_session"], format!("{who}-window"), "{step}");
    assert_eq!(step["command_id"], "layer.rename", "{step}");
    assert_eq!(step["correlation"], c, "{step}");
    assert_eq!(step["base_revision"], opened, "{step}");
    assert_eq!(step["revision"], rename["revision"], "{step}");
}

#[test]
fn two_runs_attribute_ops_history_and_frames_to_their_own_context() {
    attributed_run("alice", 1000);
    attributed_run("bob", 5000);
}

#[test]
fn no_frame_patch_or_record_carries_the_op_ordinal() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(context("carol", 1000), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.rename(L2, "a", None) && run.set_visible(L2, false, None) && run.set_opacity(L2, 50.0, None));
    assert!(run.undo() && run.redo() && run.summary());
    assert!(run.verify_live(None, vec![(L2.to_string(), "name", Value::from("a"))]));
    let mut seen = Vec::new();
    for record in run.ops() {
        let n = record["n"].as_u64().unwrap();
        if record["op"] == "open" {
            // Session-owned open: no command context, so no correlation (never n either).
            assert!(record["correlation"].is_null(), "{record}");
            continue;
        }
        let c = correlation(record);
        assert!(c >= 1000 && c != n, "record correlation is the source's, never n: {record}");
        assert!(!seen.contains(&c), "one correlation per record: {record}");
        seen.push(c);
        if let Some(patch) = record["detail"]["correlation_id"].as_u64() {
            assert_eq!(patch, c, "patch correlation: {record}");
        }
        if matches!(record["op"].as_str(), Some("summary" | "verify")) {
            assert_eq!(record["observe"]["correlation"], c, "harness-owned frame: {record}");
        }
    }
    assert_eq!(seen.len(), run.ops().len() - 1);
}

#[test]
fn undo_and_redo_are_caused_by_the_step_they_reverse() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(context("dave", 1000), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.rename(L2, "first", None));
    let edit = correlation(&last(&run));
    assert!(run.undo(), "{}", last(&run));
    let undo = last(&run);
    assert_eq!((undo["command_id"].as_str(), undo["causation"].as_u64()), (Some("history.undo"), Some(edit)), "{undo}");
    let u = correlation(&undo);
    assert_ne!(u, edit);
    assert_eq!(undo["detail"]["correlation_id"], u, "the inverse patch carries the undo's correlation: {undo}");
    let redo_step = &run.history()["redo_steps"][0];
    assert_eq!((redo_step["correlation"].as_u64(), redo_step["causation"].as_u64()), (Some(u), Some(edit)), "{redo_step}");
    assert_eq!(redo_step["command_id"], "history.undo", "{redo_step}");
    assert!(run.redo(), "{}", last(&run));
    let redo = last(&run);
    assert_eq!((redo["command_id"].as_str(), redo["causation"].as_u64()), (Some("history.redo"), Some(u)), "{redo}");
    let r = correlation(&redo);
    let undo_step = &run.history()["undo_steps"][0];
    assert_eq!((undo_step["correlation"].as_u64(), undo_step["causation"].as_u64()), (Some(r), Some(u)), "{undo_step}");
    assert_eq!(undo_step["revision"], redo["revision"], "{undo_step}");
}

const VEC: &str = "SLYR-019abcde-0000-7000-8000-0000000000bb";
const PATH: &str = "SVPT-019abcde-0000-7000-8000-0000000000b1";

/// One commit step of [`commit_frames_carry_the_command_correlation`]: a label and the call.
type Step = (&'static str, Box<dyn Fn(&mut Run<'_>) -> bool>);

/// A sequence `seq-attr` with one empty audio track A1 at `volume` (duration 0 = its content end).
fn sequence(volume: f64) -> hsk_studio_folio::StudioSequence {
    use hsk_studio_folio::{SequenceFrameRate, SequenceSchema, StudioSequence, StudioTrack, TrackKind, TrackSchema};
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-attr".into(),
        name: "attr".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: 10_160_640_000 },
        duration: hsk_studio_accord::Ticks::new(0),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-0000000000a1".into(),
            kind: TrackKind::Audio,
            index: 0,
            name: "A1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: Vec::new(),
            volume,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

/// R48 F-R48-6 (lane SMB-R47-FOLLOWUPS-1): the session-owned commit frame of every mutating command
/// carries the command's correlation (`Session::submit_as` / `submit_timeline_as` /
/// `submit_multicam_as` take the command context), so for rename, property, tile, path, structure and
/// sequence commits and their undo / redo the op record's `observe.correlation` equals its
/// `correlation`, never the session's operation counter. Correlations start at 7000. The fixture's
/// layer ...0002 is hidden, so the property step shows it (a hide would change nothing).
#[test]
fn commit_frames_carry_the_command_correlation() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(context("erin", 7000), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let vector = {
        let record = serde_json::from_value(serde_json::json!({
            "schema_id": "hsk.studio.layer@1", "layer_id": VEC, "name": "vector", "parent": null,
            "kind": "vector", "payload": {"payload_kind": "vector", "path_ids": []}, "visible": true,
        }))
        .unwrap();
        let operation = serde_json::from_value(serde_json::json!({"layer_id": VEC, "operation_key": "source", "operation_kind": "source", "inputs": [],
            "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
        .unwrap();
        hsk_studio_harness_core::PlacementWrite { layer_id: VEC.to_string(), to: Some(hsk_studio_harness_core::Place::AtBeginning(None)), create: Some((record, operation)) }
    };
    let steps: Vec<Step> = vec![
        ("rename", Box::new(|r: &mut Run<'_>| r.rename(L2, "e", None))),
        ("visible", Box::new(|r: &mut Run<'_>| r.set_visible(L2, true, None))),
        ("opacity", Box::new(|r: &mut Run<'_>| r.set_opacity(L2, 40.0, None))),
        ("tiles", Box::new(|r: &mut Run<'_>| r.set_tiles("clear", L2, Vec::new()))),
        ("structure", Box::new(move |r: &mut Run<'_>| r.restructure("new_vector", std::slice::from_ref(&vector)))),
        ("path", Box::new(|r: &mut Run<'_>| r.set_vector_paths("pen", VEC, vec![PATH.to_string()]))),
        ("sequence insert", Box::new(|r: &mut Run<'_>| r.submit_sequence_edits("add", vec![hsk_studio_session::SequenceEdit::Insert { at: 0, write: sequence(1.0) }]))),
        ("sequence", Box::new(|r: &mut Run<'_>| r.submit_sequence("fader", sequence(0.5)))),
        ("undo", Box::new(|r: &mut Run<'_>| r.undo())),
        ("redo", Box::new(|r: &mut Run<'_>| r.redo())),
    ];
    let opened = run.snapshot().unwrap().document().revision;
    for (what, step) in steps {
        let before = run.snapshot().unwrap().document().revision;
        assert!(step(&mut run), "{what}: {}", last(&run));
        let record = last(&run);
        assert!(record["revision"].as_u64().is_some_and(|r| r > before), "{what} committed a revision: {record}");
        let c = correlation(&record);
        assert!(c >= 7000, "{what}: {record}");
        assert_eq!(record["observe"]["status"], "delivered", "{what}: {record}");
        assert_eq!(record["observe"]["correlation"].as_u64(), Some(c), "{what}: the commit frame carries the command correlation: {record}");
    }
    assert!(run.snapshot().unwrap().document().revision > opened);
}
