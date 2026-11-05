//! Lane SMB-PR-ENTRY-REFERENCES-1 (R56 F-R56-3): the measured table of what removing a sequence or a
//! clip does to the document entries that name one (V4-06). One row per naming token, one column per
//! path that can remove either; every other token is listed with why it does not apply.
mod sequence_records_support;
use hsk_studio_folio::*;
use hsk_studio_package::Quarantined;
use hsk_studio_session::*;
use sequence_records_support::*;
use std::sync::{Arc, Mutex};

const NAMING: [&str; 4] = ["audio_routing", "multicam", SEQUENCE_SETTINGS_ENTRY, TRACK_DISPLAY_ENTRY];
/// Tokens that name neither a sequence nor a clip, read in their owners (lane notes,
/// `reference_table`), with what they do name.
const NOT_APPLICABLE: [(&str, &str); 11] = [
    ("selection", "a pixel plane (harness selection_store), no id"),
    ("text", "text stories keyed by SLYR layer (hsk.harness.text_layers@1)"),
    ("develop", "a camera-raw recipe, no sequence or clip id"),
    ("vector", "vector paths keyed by SLYR layer (hsk.harness.vector_paths@1)"),
    ("composition", "Folio composition records: SLYR layers and SCOM compositions"),
    ("motion", "Folio motion records: composition layers"),
    ("waveform_edit", "source identity digest and operations; `clip` there is a clipboard range"),
    ("proof", "Folio proof state: SDOC document"),
    ("design", "Folio design state: SLYR layers and components"),
    ("info", "colour samplers of the Info panel"),
    (SEQUENCE_PRESETS_ENTRY, "presets are not bound to a document sequence"),
];

const ENVELOPED: u32 = 21;
const NESTED: u32 = 20;
const OTHER: u32 = 31;

fn audio_track(sequence_id: &str, clips: Vec<StudioClip>) -> StudioTrack {
    let mut track = sequence(sequence_id, F24, clips).tracks.remove(0);
    track.track_id = identity("STRK", sequence_id.bytes().map(u32::from).sum::<u32>() + 1000);
    track.kind = TrackKind::Audio;
    track
}
fn with_end(mut s: StudioSequence) -> StudioSequence {
    s.duration = s.content_end().unwrap();
    s
}
fn nested_clip() -> StudioClip {
    let mut c = clip(NESTED, 12);
    c.source_ref = ClipSource::Sequence { sequence_id: "seq-src".into() };
    c
}
/// `seq-a`: a nested multicam clip and an audio clip with envelopes. `seq-b`: an audio clip with an
/// envelope, also a declared multicam source. `seq-src`: the source of the nested clip.
fn seq_a_full() -> StudioSequence {
    let mut s = sequence("seq-a", F24, vec![nested_clip()]);
    s.tracks.push(audio_track("seq-a", vec![clip(ENVELOPED, 24)]));
    with_end(s)
}
fn seq_b_full() -> StudioSequence {
    let mut s = sequence("seq-b", F24, vec![]);
    s.tracks.push(audio_track("seq-b", vec![clip(OTHER, 24)]));
    with_end(s)
}
fn seq_src() -> StudioSequence {
    sequence("seq-src", F24, vec![clip(40, 24)])
}
fn all_sequences() -> Vec<StudioSequence> {
    vec![seq_a_full(), seq_b_full(), seq_src()]
}
fn without_clip(s: &StudioSequence, n: u32) -> StudioSequence {
    let id = identity("SCLP", n);
    let mut s = s.clone();
    for t in &mut s.tracks {
        t.clips.retain(|c| c.clip_id != id);
    }
    with_end(s)
}

fn envelope(n: u32) -> (StudioClipGainEnvelope, StudioClipPanEnvelope) {
    (
        StudioClipGainEnvelope { clip_id: identity("SCLP", n), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] },
        StudioClipPanEnvelope { clip_id: identity("SCLP", n), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: 0.5 }] },
    )
}
fn routing() -> StudioAudioRoutingSet {
    let mut a = StudioAudioRouting::new("seq-a");
    let (gain, pan) = envelope(ENVELOPED);
    a.clip_gains.push(gain);
    a.clip_pans.push(pan);
    let mut b = StudioAudioRouting::new("seq-b");
    b.clip_gains.push(envelope(OTHER).0);
    StudioAudioRoutingSet { routings: vec![a, b] }
}
fn multicam_body() -> StudioMulticamRecords {
    let mut r = StudioMulticamRecords::new(doc_id());
    for s in [seq_b_full(), seq_src()] {
        r.multicam_sequences.push(StudioMulticamSequence {
            sequence_id: s.sequence_id.clone(),
            selection_order: MulticamSelectionOrder::SourceOrder,
            angle_order: multicam_angle_order(&s, MulticamSelectionOrder::SourceOrder),
        });
    }
    r.clips.push(StudioMulticamClip {
        clip_id: identity("SCLP", NESTED),
        active_angle: 1,
        audio_angle: 1,
        audio_follows_video: true,
        follows_nest_setting: false,
        enabled: true,
    });
    r
}
fn display_body() -> StudioTrackDisplayRecords {
    let mut r = StudioTrackDisplayRecords::new(doc_id());
    for id in ["seq-a", "seq-b"] {
        r.sequences.push(StudioTrackDisplayEntry {
            sequence_id: id.into(),
            switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT,
            heights: StudioTrackHeights::all(EXPANDED_TRACK_HEIGHT),
            height_presets: Vec::new(),
        });
    }
    r
}
fn body_of(named: &str) -> Vec<u8> {
    match named {
        "audio_routing" => serde_json::to_vec(&routing()).unwrap(),
        "multicam" => serde_json::to_vec(&multicam_body()).unwrap(),
        SEQUENCE_SETTINGS_ENTRY => serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F24), settings_entry("seq-b", "custom", F24)])).unwrap(),
        TRACK_DISPLAY_ENTRY => serde_json::to_vec(&display_body()).unwrap(),
        other => panic!("{other} is not a naming token"),
    }
}
fn held_of(named: &str) -> Vec<Quarantined> {
    vec![entry(named, &body_of(named))]
}

/// What a typed read of `token` answers now, as a code.
fn read(session: &Session, named: &str) -> Result<(), String> {
    fn code(d: &Diagnostic) -> String {
        format!("{:?} {}", d.code, d.target)
    }
    match named {
        "audio_routing" => match session.document_entry(named) {
            None => Ok(()),
            Some(bytes) => parse_audio_routing(bytes, session.snapshot().document(), Budget::default(), &token()).map(drop).map_err(|d| code(&d)),
        },
        "multicam" => session.multicam_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        SEQUENCE_SETTINGS_ENTRY => session.sequence_settings_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        TRACK_DISPLAY_ENTRY => session.track_display_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        other => panic!("{other}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Removed {
    Sequence,
    Clip,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    Timeline,
    Multicam,
    Records,
    Resync,
}
/// What one cell did.
#[derive(Clone, Debug, PartialEq)]
enum Cell {
    /// The path cannot remove this at all (a timeline step edits one value under one id).
    CannotRemove,
    /// The commit was refused and nothing changed.
    Refused(String),
    /// It committed and the entry reads.
    Reads,
    /// It committed and the entry no longer reads: the whole set, every sequence.
    Unreadable(String),
}

#[derive(Clone, Default)]
struct Authority(Arc<Mutex<Option<CommitReceipt>>>);
impl CommitPort for Authority {
    fn commit(&self, request: &CommitRequest<'_>) -> CommitOutcome {
        let receipt = CommitReceipt { revision: request.successor_revision, sequence: 1, binding: "someone-else".to_owned() };
        *self.0.lock().unwrap() = Some(receipt.clone());
        CommitOutcome::Committed(receipt)
    }
    fn lookup(&self, _: &IdempotencyKey) -> Lookup {
        match &*self.0.lock().unwrap() {
            Some(receipt) => Lookup::Found(receipt.clone()),
            None => Lookup::Absent,
        }
    }
}

fn multicam_step(session: &Session, sequences: Vec<SequenceEdit>) -> MulticamStep {
    let live: Option<StudioMulticamRecords> = session.document_entry(MULTICAM_ENTRY).map(|b| serde_json::from_slice(b).unwrap());
    MulticamStep { command_id: "cmd.remove".into(), correlation_id: 1, sequences, records_expect: live.clone(), records_write: live }
}

fn edit(removed: Removed) -> SequenceEdit {
    match removed {
        Removed::Sequence => SequenceEdit::Remove { expect: seq_b_full() },
        Removed::Clip => clip_edit(),
    }
}
/// The clip a token names: the multicam record names the nested clip, the routing the audio clip.
fn clip_edit() -> SequenceEdit {
    let without = without_clip(&without_clip(&seq_a_full(), ENVELOPED), NESTED);
    SequenceEdit::Replace { expect: seq_a_full(), write: without }
}
fn expected_document(session: &Session, removed: Removed) -> StudioDocument {
    let mut document = session.snapshot().document().clone();
    match removed {
        Removed::Sequence => document.sequences.retain(|s| s.sequence_id != "seq-b"),
        Removed::Clip => {
            let SequenceEdit::Replace { write, .. } = clip_edit() else { unreachable!() };
            document.sequences[0] = write;
        }
    }
    document
}

fn cell(named: &str, path: Path, removed: Removed) -> Cell {
    let bytes = package_of(&all_sequences(), &held_of(named));
    let outcome = match path {
        Path::Timeline => {
            let mut session = open(&bytes);
            let SequenceEdit::Replace { expect, write } = clip_edit() else { unreachable!() };
            if removed == Removed::Sequence {
                // One value under one id: another id is refused before anything else.
                let renamed = {
                    let mut s = seq_b_full();
                    s.sequence_id = "seq-gone".into();
                    s
                };
                let t = TimelineStep { command_id: "cmd.remove".into(), correlation_id: 1, sequence_id: "seq-b".into(), expect: seq_b_full(), write: renamed };
                assert_eq!(session.submit_timeline(&t, 3, &token()).unwrap_err().error.code(), "invalid_input");
                return Cell::CannotRemove;
            }
            let t = TimelineStep { command_id: "cmd.remove".into(), correlation_id: 1, sequence_id: "seq-a".into(), expect, write };
            session.submit_timeline(&t, 3, &token()).map(drop).map_err(|f| f.error.code().to_owned()).map(|()| session)
        }
        Path::Multicam => {
            let mut session = open(&bytes);
            let s = multicam_step(&session, vec![edit(removed)]);
            session.submit_multicam(&s, 3, &token()).map(drop).map_err(|f| f.error.code().to_owned()).map(|()| session)
        }
        Path::Records => {
            let mut session = open(&bytes);
            let s = step(vec![edit(removed)], None, None);
            session.submit_sequence_records(&s, 3, &token()).map(drop).map_err(|f| f.code().to_owned()).map(|()| session)
        }
        Path::Resync => {
            let authority = Authority::default();
            let ports = ports(&Frames::default()).with_commits(Box::new(authority.clone()));
            let mut session = Session::open_bytes(&bytes, SessionConfig::new(actor()), ports, &token()).unwrap_or_else(|f| panic!("{f:?}"));
            let key = IdempotencyKey::parse("presets-1").unwrap();
            let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
            assert_eq!(session.submit_sequence_records_keyed(&key, &presets, 3, &token()).unwrap_err().code(), "receipt_mismatch");
            let mut document = expected_document(&session, removed);
            document.revision = 4;
            let authority_bytes = serde_json::to_vec(&document).unwrap();
            session.resync_diverged(&key, &authority_bytes, &token()).map(drop).map_err(|f| f.code().to_owned()).map(|()| session)
        }
    };
    match outcome {
        Err(code) => Cell::Refused(code),
        Ok(session) => match read(&session, named) {
            Ok(()) => Cell::Reads,
            Err(code) => Cell::Unreadable(code),
        },
    }
}

/// The table. Red on base: every cell that leaves an entry unreadable.
#[test]
fn no_removal_leaves_a_naming_entry_unreadable() {
    bounded_case("table", || {
        let mut lines = Vec::new();
        let mut unreadable = Vec::new();
        for named in NAMING {
            for path in [Path::Timeline, Path::Multicam, Path::Records, Path::Resync] {
                for removed in [Removed::Sequence, Removed::Clip] {
                    let got = cell(named, path, removed);
                    lines.push(format!("{named:18} {path:?} {removed:?}: {got:?}"));
                    if matches!(got, Cell::Unreadable(_)) {
                        unreadable.push(format!("{named} {path:?} {removed:?}"));
                    }
                }
            }
        }
        println!("{}", lines.join("\n"));
        assert!(unreadable.is_empty(), "unreadable cells: {unreadable:?}\n{}", lines.join("\n"));
    });
}

/// Every token is in the table or listed with its reason: a new token has to say which.
#[test]
fn every_document_entry_token_is_classified() {
    let mut classified: Vec<&str> = NAMING.to_vec();
    classified.extend(NOT_APPLICABLE.iter().map(|(t, _)| *t));
    classified.sort_unstable();
    let mut tokens = DOCUMENT_ENTRY_TOKENS.to_vec();
    tokens.sort_unstable();
    assert_eq!(classified, tokens);
    assert!(!DOCUMENT_ENTRY_TOKENS.contains(&"clip_finishing"), "clip finishing names clips: it needs a row when the session carries it");
}

/// A token that names neither keeps its bytes through a removal of either, on the path each can take.
#[test]
fn a_token_that_names_neither_is_untouched_by_a_removal() {
    bounded_case("not_applicable", || {
        let composition = serde_json::to_vec(&StudioCompositionRecords::new(doc_id())).unwrap();
        for (held_token, _) in NOT_APPLICABLE {
            let body = if held_token == "composition" { composition.clone() } else { b"{}".to_vec() };
            let held = [entry(held_token, &body)];
            let mut session = open(&package_of(&all_sequences(), &held));
            let removal = multicam_step(&session, vec![edit(Removed::Sequence), edit(Removed::Clip)]);
            session.submit_multicam(&removal, 3, &token()).unwrap_or_else(|f| panic!("{held_token}: {f:?}"));
            assert_eq!(session.document_entry(held_token), Some(body.as_slice()), "{held_token}");
        }
        let held = [entry("composition", &composition)];
        let mut session = open(&package_of(&all_sequences(), &held));
        let removal = multicam_step(&session, vec![edit(Removed::Sequence), edit(Removed::Clip)]);
        session.submit_multicam(&removal, 3, &token()).unwrap();
        assert!(session.composition_records(&Granted, &token()).unwrap().is_some(), "reads after both removals");
    });
}
