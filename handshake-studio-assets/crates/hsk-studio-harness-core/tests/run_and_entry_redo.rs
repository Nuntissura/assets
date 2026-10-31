//! Lane SMB-R42-FOLLOWUPS-1 (finding F-R42-2 of round STUDIO-R42): when the entry write of a
//! `Run::*_and_entry` step is refused, the committed structure / tile / path / sequence edit is
//! rolled back AND the redo history is exactly what it was before the call: `nothing_to_redo`
//! when it was empty, the user's earlier redo step intact when there was one. Before the fix the
//! rollback `undo()` left the half step (the edit without its entry) on the redo history.
//! The refusal is an entry token the session does not know (`not-a-token`, typed `unsafe_name`).
//! Fixture: the harness crate's `tests/fixtures/demo.handshake` (group layer ...0001, raster layer
//! ...0002 "layer-2" with one tile). The sequence cases build a package with
//! `hsk-studio-package` (feature `import`); without it they print NOT_PROVEN.

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::{Harness, Place, PlacementWrite, Run, Selection};
mod support;
use support::test_context;
use serde_json::{Value, json};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const NEW: &str = "SLYR-019abcde-0000-7000-8000-0000000000aa";
const VEC: &str = "SLYR-019abcde-0000-7000-8000-0000000000bb";
const PATH: &str = "SVPT-019abcde-0000-7000-8000-0000000000b1";
const BAD_TOKEN: &str = "not-a-token";
const REDONE_NAME: &str = "redo-me";

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

fn layer(id: &str, kind: &str, payload: Value) -> PlacementWrite {
    let record = serde_json::from_value(json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": kind, "parent": null,
        "kind": kind, "payload": payload, "visible": true,
    }))
    .unwrap();
    let operation = serde_json::from_value(json!({"layer_id": id, "operation_key": "source", "operation_kind": "source", "inputs": [],
        "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
    .unwrap();
    PlacementWrite { layer_id: id.to_string(), to: Some(Place::AtBeginning(None)), create: Some((record, operation)) }
}

/// A new raster layer at the top of the root stack.
fn create_raster() -> PlacementWrite {
    layer(NEW, "raster", json!({"payload_kind": "raster", "tiles": []}))
}

fn find<'r>(run: &'r Run<'_>, id: &str) -> Option<&'r hsk_studio_folio::StudioLayer> {
    run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == id)
}

fn name_of(run: &Run<'_>, id: &str) -> String {
    find(run, id).unwrap().name.clone()
}

fn tile_count(run: &Run<'_>, id: &str) -> usize {
    match &find(run, id).unwrap().payload {
        hsk_studio_folio::Payload::Raster { tiles } => tiles.len(),
        other => panic!("not raster: {other:?}"),
    }
}

fn path_ids(run: &Run<'_>, id: &str) -> Vec<String> {
    match &find(run, id).unwrap().payload {
        hsk_studio_folio::Payload::Vector { path_ids } => path_ids.clone(),
        other => panic!("not vector: {other:?}"),
    }
}

/// Puts one earlier redo step on the history: rename layer-2, then undo it.
fn earlier_redo_step(run: &mut Run<'_>) {
    assert!(run.rename(L2, REDONE_NAME, None), "{:?}", run.ops().last());
    assert!(run.undo(), "{:?}", run.ops().last());
    assert_eq!(run.history()["redo"].as_array().unwrap().len(), 1, "{}", run.history());
}

/// Runs `call` (a `*_and_entry` call whose entry write is refused) and checks that it fails and
/// leaves both histories as they were; then checks the redo history by redoing.
fn refused_entry_keeps_history(run: &mut Run<'_>, prior_redo: bool, call: impl FnOnce(&mut Run<'_>) -> bool) {
    let before = run.history();
    assert_eq!(before["redo"].as_array().unwrap().is_empty(), !prior_redo, "{before}");
    assert!(!call(run), "the entry write must be refused: {:?}", run.ops().last());
    assert_eq!(run.ops().last().unwrap()["error"], "unsafe_name", "{:?}", run.ops().last());
    assert_eq!(run.history(), before, "undo and redo histories after a refused entry write");
    if prior_redo {
        // The earlier redo step is intact: redo re-applies the rename, nothing else.
        assert!(run.redo(), "{:?}", run.ops().last());
        assert_eq!(name_of(run, L2), REDONE_NAME);
        assert!(run.history()["redo"].as_array().unwrap().is_empty(), "{}", run.history());
    } else {
        assert!(!run.redo(), "a half step was left on the redo history: {}", run.history());
        assert_eq!(run.ops().last().unwrap()["error"], "nothing_to_redo");
    }
}

#[test]
fn restructure_and_entry_refused_entry_keeps_the_redo_history() {
    let h = harness();
    let cancel = CancellationToken::default();
    for prior_redo in [false, true] {
        let mut run = h.run(test_context(), &cancel);
        assert!(run.open_bytes(FIXTURE));
        if prior_redo {
            earlier_redo_step(&mut run);
        }
        refused_entry_keeps_history(&mut run, prior_redo, |run| run.restructure_and_entry("replace", &[create_raster()], BAD_TOKEN, b"x".to_vec()));
        assert!(find(&run, NEW).is_none(), "the structure edit was rolled back and not redone");
    }
}

#[test]
fn set_tiles_and_entry_refused_entry_keeps_the_redo_history() {
    let h = harness();
    let cancel = CancellationToken::default();
    for prior_redo in [false, true] {
        let mut run = h.run(test_context(), &cancel);
        assert!(run.open_bytes(FIXTURE));
        assert_eq!(tile_count(&run, L2), 1);
        if prior_redo {
            earlier_redo_step(&mut run);
        }
        refused_entry_keeps_history(&mut run, prior_redo, |run| run.set_tiles_and_entry("develop", L2, Some(Vec::new()), BAD_TOKEN, b"x".to_vec()));
        assert_eq!(tile_count(&run, L2), 1, "the tile edit was rolled back and not redone");
    }
}

#[test]
fn set_vector_paths_and_entry_refused_entry_keeps_the_redo_history() {
    let h = harness();
    let cancel = CancellationToken::default();
    for prior_redo in [false, true] {
        let mut run = h.run(test_context(), &cancel);
        assert!(run.open_bytes(FIXTURE));
        assert!(run.restructure("new_vector", &[layer(VEC, "vector", json!({"payload_kind": "vector", "path_ids": []}))]), "{:?}", run.ops().last());
        if prior_redo {
            earlier_redo_step(&mut run);
        }
        let writes = [(VEC.to_string(), vec![PATH.to_string()])];
        refused_entry_keeps_history(&mut run, prior_redo, |run| run.set_vector_paths_and_entry("pen", &writes, Some((BAD_TOKEN, b"x".to_vec()))));
        assert!(path_ids(&run, VEC).is_empty(), "the path edit was rolled back and not redone");
    }
}

#[cfg(feature = "import")]
mod sequence {
    use super::*;
    use hsk_studio_accord::Ticks;
    use hsk_studio_folio::{
        self as folio, Budget, ClipSchema, ClipSource, ClipTransitions, ContentDigest, Inspection, Resolution, SequenceFrameRate, SequenceSchema, Speed, StudioClip,
        StudioSequence, StudioTrack, TileRef, TrackKind, TrackSchema,
    };

    /// 25 fps ticks per frame (STU-VID-013).
    const F: u64 = 10_160_640_000;
    const ACT: &str = "SACT-019abcde-0000-7000-8000-0000000000a2";

    struct Any;
    impl folio::Resolver for Any {
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
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }

    /// One sequence `seq-main`: audio track A1 (volume 1) with clip `tone` source [0, 50) at [0, 50).
    pub(super) fn sequence() -> StudioSequence {
        let clip = StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
            source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
            source_in: Ticks::new(0),
            source_out: Ticks::new(50 * F),
            timeline_start: Ticks::new(0),
            duration: Ticks::new(50 * F),
            speed: Speed::UNIT,
            enabled: true,
            link_group: None,
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
        };
        let track = StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Audio,
            index: 0,
            name: "A1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip],
            volume: 1.0,
            pan: 0.0,
        };
        StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(50 * F),
            tracks: vec![track],
            markers: Vec::new(),
            work_area: None,
        }
    }

    /// Native package bytes whose document holds [`sequence`] and one group layer.
    fn package() -> Vec<u8> {
        let doc = json!({
            "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000042","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
            "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
            "artboards":[],"page_spreads":[],
            "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
            "sequences":[serde_json::to_value(sequence()).unwrap()]
        });
        let token = CancellationToken::default();
        let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
            Inspection::Editable(s) => s,
            Inspection::ReadOnly(_) => panic!("read-only fixture"),
        };
        let assets = hsk_studio_package::MemoryAssetSource::new();
        let mut bytes = Vec::new();
        hsk_studio_package::write_package_with(
            &snapshot,
            &assets as &dyn hsk_studio_package::AssetSource,
            &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
            &hsk_studio_package::WriteOptions::default(),
            &hsk_studio_package::Limits::default(),
            &token,
            &mut bytes,
        )
        .unwrap();
        bytes
    }

    fn volume(run: &Run<'_>) -> f64 {
        run.snapshot().unwrap().document().sequences[0].tracks[0].volume
    }

    /// The sequence with track A1's fader at `volume`.
    fn faded(volume: f64) -> StudioSequence {
        let mut s = sequence();
        s.tracks[0].volume = volume;
        s
    }

    #[test]
    fn submit_sequence_and_entry_refused_entry_keeps_the_redo_history() {
        let h = harness();
        let cancel = CancellationToken::default();
        let bytes = package();
        for prior_redo in [false, true] {
            let mut run = h.run(test_context(), &cancel);
            assert!(run.open_bytes(&bytes), "{:?}", run.ops().last());
            assert!((volume(&run) - 1.0).abs() < 1e-9);
            if prior_redo {
                // The earlier redo step: a fader move, undone.
                assert!(run.submit_sequence("fader", faded(0.25)), "{:?}", run.ops().last());
                assert!(run.undo(), "{:?}", run.ops().last());
            }
            let before = run.history();
            assert_eq!(before["redo"].as_array().unwrap().is_empty(), !prior_redo, "{before}");
            assert!(!run.submit_sequence_and_entry("fader", faded(0.5), BAD_TOKEN, b"x".to_vec()));
            assert_eq!(run.ops().last().unwrap()["error"], "unsafe_name", "{:?}", run.ops().last());
            assert_eq!(run.history(), before, "undo and redo histories after a refused entry write");
            assert!((volume(&run) - 1.0).abs() < 1e-9, "the sequence edit was rolled back");
            if prior_redo {
                assert!(run.redo(), "{:?}", run.ops().last());
                assert!((volume(&run) - 0.25).abs() < 1e-6, "the earlier redo step re-applies the 0.25 fader, got {}", volume(&run));
            } else {
                assert!(!run.redo(), "a half step was left on the redo history: {}", run.history());
                assert_eq!(run.ops().last().unwrap()["error"], "nothing_to_redo");
            }
        }
    }

    /// F-R42-3: while recording, `submit_sequence_and_entry` is reported once under its own id;
    /// its inner submit and a refused entry write's rollback add no further entries.
    #[test]
    fn submit_sequence_and_entry_is_reported_under_its_own_id_while_recording() {
        let h = harness();
        let cancel = CancellationToken::default();
        let bytes = package();
        let mut run = h.run(test_context(), &cancel);
        assert!(run.open_bytes(&bytes));
        assert!(run.start_recording(ACT, "mix"));
        // Accepted: one step (sequence and entry); refused entry write: rolled back.
        assert!(run.submit_sequence_and_entry("fader", faded(0.5), "audio_routing", b"{}".to_vec()), "{:?}", run.ops().last());
        assert!(!run.submit_sequence_and_entry("fader", faded(0.75), BAD_TOKEN, b"x".to_vec()));
        let (_, report) = run.stop_recording().unwrap();
        let nr: Vec<(&str, &str)> = report.not_recordable.iter().map(|n| (n.command_id, n.reason)).collect();
        assert_eq!(nr, [("document.submit_sequence_and_entry", "recordable_false"), ("document.submit_sequence_and_entry", "recordable_false")], "{}", report.to_json());
        assert!(report.refused.is_empty() && report.steps == 0, "{}", report.to_json());
    }
}

#[cfg(not(feature = "import"))]
#[test]
fn submit_sequence_and_entry_not_proven_without_import() {
    println!("NOT_PROVEN: submit_sequence_and_entry redo history after a refused entry write and its recording report (the sequence package is written with hsk-studio-package: run with --features hsk-studio-harness-core/import)");
}
