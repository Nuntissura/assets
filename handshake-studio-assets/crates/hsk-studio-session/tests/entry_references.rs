//! Lane SMB-PR-ENTRY-REFERENCES-1 (R56 F-R56-3, F-R56-7): the measured table of what removing a
//! sequence, a clip or a track does to the document entries that name one (V4-06). One row per naming
//! token, one column per path that can remove one; every other token is listed with why it does not
//! apply. Lead decision: `audio_routing` and multicam clip modes follow the track display rule (an id
//! the document does not hold is inert); settings and multicam sequences keep their refusals.
mod sequence_records_support;
use hsk_studio_folio::*;
use hsk_studio_package::Quarantined;
use hsk_studio_session::*;
use sequence_records_support::*;
use std::sync::{Arc, Mutex};

const NAMING: [&str; 4] = [AUDIO_ROUTING_ENTRY, MULTICAM_ENTRY, SEQUENCE_SETTINGS_ENTRY, TRACK_DISPLAY_ENTRY];
/// Tokens that name neither a sequence nor a clip, read in their owners (lane notes,
/// `reference_table`), with what they do name.
const NOT_APPLICABLE: [(&str, &str); 14] = [
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
    (COMPOSITION_TOKEN_ENTRY, "Folio token record: SDOC document, tokens, and the story ids of its upgrades (a history checked for grammar and order only)"),
    (MEDIA_MARKERS_ENTRY, "Folio media item markers: SDOC document, content digests and marker ids, no sequence or clip id (tests/media_markers.rs)"),
    (PROJECT_BIN_ENTRY, "Folio project bin: SDOC document, bin-local folder and item keys and media clip sources by content digest, no sequence, track or clip id (harness tests/project_bin.rs)"),
];

const ENVELOPED: u32 = 21;
const NESTED: u32 = 20;
const OTHER: u32 = 31;

fn kind_track(sequence_id: &str, offset: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    let mut track = sequence(sequence_id, F24, clips).tracks.remove(0);
    track.track_id = identity("STRK", sequence_id.bytes().map(u32::from).sum::<u32>() + offset);
    track.kind = kind;
    track
}
fn audio_id(sequence_id: &str) -> String {
    kind_track(sequence_id, 1000, TrackKind::Audio, vec![]).track_id
}
fn submix_id(sequence_id: &str) -> String {
    kind_track(sequence_id, 2000, TrackKind::Submix, vec![]).track_id
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
/// `seq-a`: a nested multicam clip, an audio clip with envelopes, a submix track. `seq-b`: an audio clip
/// with an envelope, also a declared multicam source. `seq-src`: the source of the nested clip.
fn seq_a_full() -> StudioSequence {
    let mut s = sequence("seq-a", F24, vec![nested_clip()]);
    s.tracks.push(kind_track("seq-a", 1000, TrackKind::Audio, vec![clip(ENVELOPED, 24)]));
    s.tracks.push(kind_track("seq-a", 2000, TrackKind::Submix, vec![]));
    with_end(s)
}
fn seq_b_full() -> StudioSequence {
    let mut s = sequence("seq-b", F24, vec![]);
    s.tracks.push(kind_track("seq-b", 1000, TrackKind::Audio, vec![clip(OTHER, 24)]));
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

fn gain(n: u32) -> StudioClipGainEnvelope {
    StudioClipGainEnvelope { clip_id: identity("SCLP", n), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] }
}
fn pan(n: u32) -> StudioClipPanEnvelope {
    StudioClipPanEnvelope { clip_id: identity("SCLP", n), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: 0.5 }] }
}
fn lane(track_id: String, param: StudioTrackParam) -> StudioTrackAutomation {
    StudioTrackAutomation { track_id, param, points: vec![StudioAutomationPoint { offset_ticks: 0, value: 0.5 }] }
}
fn routing() -> StudioAudioRoutingSet {
    let mut a = StudioAudioRouting::new("seq-a");
    a.routes = vec![
        StudioTrackRoute { track_id: audio_id("seq-a"), output: None, sends: vec![] },
        StudioTrackRoute { track_id: submix_id("seq-a"), output: None, sends: vec![] },
    ];
    a.bus_effects = vec![StudioBusEffects { track_id: submix_id("seq-a"), effects: vec![] }];
    a.track_automation = vec![lane(audio_id("seq-a"), StudioTrackParam::Volume), lane(submix_id("seq-a"), StudioTrackParam::Pan)];
    a.clip_gains.push(gain(ENVELOPED));
    a.clip_pans.push(pan(ENVELOPED));
    let mut b = StudioAudioRouting::new("seq-b");
    b.clip_gains.push(gain(OTHER));
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
        AUDIO_ROUTING_ENTRY => serde_json::to_vec(&routing()).unwrap(),
        MULTICAM_ENTRY => serde_json::to_vec(&multicam_body()).unwrap(),
        SEQUENCE_SETTINGS_ENTRY => serde_json::to_vec(&records(vec![settings_entry("seq-a", "custom", F24), settings_entry("seq-b", "custom", F24)])).unwrap(),
        TRACK_DISPLAY_ENTRY => serde_json::to_vec(&display_body()).unwrap(),
        other => panic!("{other} is not a naming token"),
    }
}
fn opened(named: &str) -> Session {
    open(&package_of(&all_sequences(), &[entry(named, &body_of(named))]))
}

/// What a typed read of `named` answers now, as a code.
fn read(session: &Session, named: &str) -> Result<(), String> {
    match named {
        AUDIO_ROUTING_ENTRY => session.audio_routing_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        MULTICAM_ENTRY => session.multicam_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        SEQUENCE_SETTINGS_ENTRY => session.sequence_settings_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        TRACK_DISPLAY_ENTRY => session.track_display_records(&token()).map(drop).map_err(|e| e.code().to_owned()),
        other => panic!("{other}"),
    }
}
/// How many inert ids the typed listing of `named` answers.
fn inert_count(session: &Session, named: &str) -> usize {
    match named {
        AUDIO_ROUTING_ENTRY => session.inert_audio_routing_references(&token()).unwrap().len(),
        MULTICAM_ENTRY => session.inert_multicam_clips(&token()).unwrap().len(),
        TRACK_DISPLAY_ENTRY => session.inert_track_display_entries(&token()).unwrap().len(),
        other => panic!("{other}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Removed {
    Sequence,
    Clip,
    Track,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    Timeline,
    Multicam,
    Records,
    Resync,
}
const PATHS: [Path; 4] = [Path::Timeline, Path::Multicam, Path::Records, Path::Resync];
const REMOVALS: [Removed; 3] = [Removed::Sequence, Removed::Clip, Removed::Track];

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

/// The removal a cell makes. The clip edit removes both clips the fixture's entries name. The track
/// edit removes what `named` names: multicam's angle track, else the audio and submix tracks of `seq-a`.
fn edit(named: &str, removed: Removed) -> SequenceEdit {
    match removed {
        Removed::Sequence => SequenceEdit::Remove { expect: seq_b_full() },
        Removed::Clip => SequenceEdit::Replace { expect: seq_a_full(), write: without_clip(&without_clip(&seq_a_full(), ENVELOPED), NESTED) },
        Removed::Track if named == MULTICAM_ENTRY => {
            let mut write = seq_b_full();
            write.tracks.retain(|t| t.kind != TrackKind::Video);
            SequenceEdit::Replace { expect: seq_b_full(), write: with_end(write) }
        }
        Removed::Track => {
            let mut write = seq_a_full();
            write.tracks.retain(|t| t.kind == TrackKind::Video);
            SequenceEdit::Replace { expect: seq_a_full(), write: with_end(write) }
        }
    }
}
fn timeline_step(named: &str, removed: Removed) -> TimelineStep {
    let SequenceEdit::Replace { expect, write } = edit(named, removed) else { panic!("a timeline step replaces") };
    TimelineStep { command_id: "cmd.remove".into(), correlation_id: 1, sequence_id: expect.sequence_id.clone(), expect, write }
}
fn expected_document(session: &Session, named: &str, removed: Removed) -> StudioDocument {
    let mut document = session.snapshot().document().clone();
    match edit(named, removed) {
        SequenceEdit::Remove { expect } => document.sequences.retain(|s| s.sequence_id != expect.sequence_id),
        SequenceEdit::Replace { expect, write } => {
            let at = document.sequences.iter().position(|s| s.sequence_id == expect.sequence_id).unwrap();
            document.sequences[at] = write;
        }
        SequenceEdit::Insert { .. } => unreachable!(),
    }
    document
}
fn resynced(named: &str, removed: Removed) -> Result<Session, String> {
    let bytes = package_of(&all_sequences(), &[entry(named, &body_of(named))]);
    let ports = ports(&Frames::default()).with_commits(Box::new(Authority::default()));
    let mut session = Session::open_bytes(&bytes, SessionConfig::new(actor()), ports, &token()).unwrap_or_else(|f| panic!("{f:?}"));
    let key = IdempotencyKey::parse("presets-1").unwrap();
    let presets = step(vec![], None, Some(RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) }));
    assert_eq!(session.submit_sequence_records_keyed(&key, &presets, 3, &token()).unwrap_err().code(), "receipt_mismatch");
    let mut document = expected_document(&session, named, removed);
    document.revision = 4;
    let authority_bytes = serde_json::to_vec(&document).unwrap();
    session.resync_diverged(&key, &authority_bytes, &token()).map(drop).map_err(|f| f.code().to_owned()).map(|()| session)
}

fn cell(named: &str, path: Path, removed: Removed) -> Cell {
    let outcome = match path {
        Path::Timeline if removed == Removed::Sequence => {
            // One value under one id: another id is refused before anything else.
            let mut session = opened(named);
            let mut renamed = seq_b_full();
            renamed.sequence_id = "seq-gone".into();
            let t = TimelineStep { command_id: "cmd.remove".into(), correlation_id: 1, sequence_id: "seq-b".into(), expect: seq_b_full(), write: renamed };
            assert_eq!(session.submit_timeline(&t, 3, &token()).unwrap_err().error.code(), "invalid_input");
            return Cell::CannotRemove;
        }
        Path::Timeline => {
            let mut session = opened(named);
            session.submit_timeline(&timeline_step(named, removed), 3, &token()).map(drop).map_err(|f| f.error.code().to_owned()).map(|()| session)
        }
        Path::Multicam => {
            let mut session = opened(named);
            let s = multicam_step(&session, vec![edit(named, removed)]);
            session.submit_multicam(&s, 3, &token()).map(drop).map_err(|f| f.error.code().to_owned()).map(|()| session)
        }
        Path::Records => {
            let mut session = opened(named);
            let s = step(vec![edit(named, removed)], None, None);
            session.submit_sequence_records(&s, 3, &token()).map(drop).map_err(|f| f.code().to_owned()).map(|()| session)
        }
        Path::Resync => resynced(named, removed),
    };
    match outcome {
        Err(code) => Cell::Refused(code),
        Ok(session) => match read(&session, named) {
            Ok(()) => Cell::Reads,
            Err(code) => Cell::Unreadable(code),
        },
    }
}

/// What each cell must do. Multicam's `angle_order` is read by index (pulse multicam.rs:374, :957, :991),
/// so a stale track is not inert there, and no commit may make it stale (lead ruling on F-R57-2, option A).
fn expected(named: &str, path: Path, removed: Removed) -> Cell {
    let refused = || Cell::Refused("folio_rejected".into());
    match (named, path, removed) {
        (_, Path::Timeline, Removed::Sequence) => Cell::CannotRemove,
        (MULTICAM_ENTRY, _, Removed::Track) => refused(),
        (MULTICAM_ENTRY | SEQUENCE_SETTINGS_ENTRY, _, Removed::Sequence) => refused(),
        _ => Cell::Reads,
    }
}

/// The table: every token, path and removal does what `expected` says.
#[test]
fn every_removal_does_what_the_table_says() {
    bounded_case("table", || {
        let mut lines = Vec::new();
        let mut wrong = Vec::new();
        for named in NAMING {
            for path in PATHS {
                for removed in REMOVALS {
                    let got = cell(named, path, removed);
                    lines.push(format!("{named:18} {path:?} {removed:?}: {got:?}"));
                    if got != expected(named, path, removed) {
                        wrong.push(format!("{named} {path:?} {removed:?}"));
                    }
                }
            }
        }
        println!("{}", lines.join("\n"));
        assert!(wrong.is_empty(), "cells off the table: {wrong:?}\n{}", lines.join("\n"));
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

/// A token that names neither keeps its bytes through every removal, and composition still reads.
#[test]
fn a_token_that_names_neither_is_untouched_by_a_removal() {
    bounded_case("not_applicable", || {
        let composition = serde_json::to_vec(&StudioCompositionRecords::new(doc_id())).unwrap();
        let authored = serde_json::to_vec(&StudioCompositionTokenRecord::new(doc_id(), CURRENT_COMPOSITION_TOKEN)).unwrap();
        let mut media_records = StudioMediaMarkerRecords::new(doc_id());
        media_records.media_items.push(StudioMediaMarkerEntry {
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab0001".into() },
            markers: vec![StudioMarker { marker_id: "media-marker-1".into(), name: "A note".into(), comment: String::new(), color: LabelColor::Mango, start: hsk_studio_accord::Ticks::new(F24), duration: hsk_studio_accord::Ticks::new(F24), role: None }],
        });
        let media = serde_json::to_vec(&media_records).unwrap();
        for (held_token, _) in NOT_APPLICABLE {
            let body = match held_token {
                "composition" => composition.clone(),
                COMPOSITION_TOKEN_ENTRY => authored.clone(),
                MEDIA_MARKERS_ENTRY => media.clone(),
                _ => b"{}".to_vec(),
            };
            let mut session = open(&package_of(&all_sequences(), &[entry(held_token, &body)]));
            let removal = multicam_step(&session, vec![edit(held_token, Removed::Sequence), edit(held_token, Removed::Clip)]);
            session.submit_multicam(&removal, 3, &token()).unwrap_or_else(|f| panic!("{held_token}: {f:?}"));
            assert_eq!(session.document_entry(held_token), Some(body.as_slice()), "{held_token}");
        }
        let mut session = open(&package_of(&all_sequences(), &[entry("composition", &composition)]));
        let removal = multicam_step(&session, vec![edit("composition", Removed::Sequence), edit("composition", Removed::Clip)]);
        session.submit_multicam(&removal, 3, &token()).unwrap();
        assert!(session.composition_records(&Granted, &token()).unwrap().is_some(), "reads after both removals");
        let mut session = open(&package_of(&all_sequences(), &[entry(COMPOSITION_TOKEN_ENTRY, &authored)]));
        let removal = multicam_step(&session, vec![edit(COMPOSITION_TOKEN_ENTRY, Removed::Sequence), edit(COMPOSITION_TOKEN_ENTRY, Removed::Clip)]);
        session.submit_multicam(&removal, 3, &token()).unwrap();
        assert!(session.composition_token_record(&token()).unwrap().is_some(), "the token reads after both removals");
        let mut session = open(&package_of(&all_sequences(), &[entry(MEDIA_MARKERS_ENTRY, &media)]));
        let removal = multicam_step(&session, vec![edit(MEDIA_MARKERS_ENTRY, Removed::Sequence), edit(MEDIA_MARKERS_ENTRY, Removed::Clip)]);
        session.submit_multicam(&removal, 3, &token()).unwrap();
        assert_eq!(session.media_marker_records(&token()).unwrap(), Some(media_records), "the media item markers read after both removals");
    });
}

// ----- The newly inert cells: undo, redo, save and reopen, bytes identical -----

#[derive(Clone)]
enum Inverse {
    Timeline(TimelineStep),
    Multicam(MulticamStep),
    Records(SequenceRecordsStep),
}
impl Inverse {
    /// Submits this step at the session's revision and answers its own inverse.
    fn apply(&self, session: &mut Session) -> Inverse {
        let revision = session.document_revision();
        match self {
            Self::Timeline(s) => Self::Timeline(session.submit_timeline(s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}")).timeline_inverse.unwrap()),
            Self::Multicam(s) => Self::Multicam(session.submit_multicam(s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}")).multicam_inverse.unwrap()),
            Self::Records(s) => Self::Records(session.submit_sequence_records(s, revision, &token()).unwrap_or_else(|f| panic!("{f:?}")).sequence_records_inverse.unwrap()),
        }
    }
}
fn newly_inert() -> Vec<(&'static str, Path, Removed)> {
    let mut cells = Vec::new();
    for path in [Path::Timeline, Path::Multicam, Path::Records] {
        for removed in REMOVALS {
            if !(path == Path::Timeline && removed == Removed::Sequence) {
                cells.push((AUDIO_ROUTING_ENTRY, path, removed));
            }
        }
        cells.push((MULTICAM_ENTRY, path, Removed::Clip));
    }
    cells
}

/// The entry's bytes never move: the removal leaves them, the checkpoint and the reopen hold them with
/// no loss note, the listing names what is absent, an undo (also an inverse taken before the save, on
/// the reopened document) empties the listing, and a redo fills it again.
#[test]
fn newly_inert_cells_undo_redo_save_and_reopen_with_their_bytes() {
    bounded_case("inert_round_trip", || {
        for (named, path, removed) in newly_inert() {
            let what = format!("{named} {path:?} {removed:?}");
            let mut session = opened(named);
            let body = session.document_entry(named).unwrap().to_vec();
            let before = session.snapshot().document().sequences.clone();
            assert_eq!(inert_count(&session, named), 0, "{what}");
            let forward = match path {
                Path::Timeline => Inverse::Timeline(timeline_step(named, removed)),
                Path::Multicam => Inverse::Multicam(multicam_step(&session, vec![edit(named, removed)])),
                Path::Records => Inverse::Records(step(vec![edit(named, removed)], None, None)),
                Path::Resync => unreachable!(),
            };
            let undo = forward.apply(&mut session);
            let removed_sequences = session.snapshot().document().sequences.clone();
            let listed = inert_count(&session, named);
            assert!(listed > 0, "{what}: the listing names it");
            assert_eq!(session.document_entry(named), Some(body.as_slice()), "{what}: left alone");
            assert!(session.loss().is_empty(), "{what}");

            let checkpoint = session.export_checkpoint(&token()).unwrap();
            let mut reopened = open(&checkpoint.bytes);
            assert!(reopened.loss().is_empty(), "{what}: inert is not a loss: {:?}", reopened.loss());
            assert_eq!(reopened.document_entry(named), Some(body.as_slice()), "{what}: reopened bytes");
            assert_eq!(read(&reopened, named), Ok(()), "{what}: reopened read");
            assert_eq!(inert_count(&reopened, named), listed, "{what}: reopened listing");
            undo.apply(&mut reopened);
            assert_eq!(reopened.snapshot().document().sequences, before, "{what}: undo after reopen");
            assert_eq!(inert_count(&reopened, named), 0, "{what}: live again after reopen");
            assert_eq!(reopened.document_entry(named), Some(body.as_slice()), "{what}: undo after reopen, bytes");

            let redo = undo.apply(&mut session);
            assert_eq!(session.snapshot().document().sequences, before, "{what}: undo");
            assert_eq!(session.document_entry(named), Some(body.as_slice()), "{what}: undo, bytes");
            assert_eq!(inert_count(&session, named), 0, "{what}: live again");
            redo.apply(&mut session);
            assert_eq!(session.snapshot().document().sequences, removed_sequences, "{what}: redo");
            assert_eq!(session.document_entry(named), Some(body.as_slice()), "{what}: redo, bytes");
            assert_eq!(inert_count(&session, named), listed, "{what}: redo, listing");
        }
    });
}

/// A resync has no undo: the bytes stay, the reopen holds them, the listing names what is absent.
#[test]
fn a_resync_leaves_an_inert_entry_byte_identical_through_reopen() {
    bounded_case("inert_resync", || {
        let cells = [(AUDIO_ROUTING_ENTRY, Removed::Sequence), (AUDIO_ROUTING_ENTRY, Removed::Clip), (AUDIO_ROUTING_ENTRY, Removed::Track), (MULTICAM_ENTRY, Removed::Clip)];
        for (named, removed) in cells {
            let mut session = resynced(named, removed).unwrap_or_else(|code| panic!("{named} {removed:?}: {code}"));
            let body = body_of(named);
            assert_eq!(session.document_entry(named), Some(body.as_slice()));
            let listed = inert_count(&session, named);
            assert!(listed > 0, "{named} {removed:?}");
            let reopened = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(reopened.document_entry(named), Some(body.as_slice()));
            assert_eq!(inert_count(&reopened, named), listed);
            assert!(reopened.loss().is_empty());
        }
    });
}

/// Through a file too, not only a checkpoint.
#[test]
fn an_inert_routing_survives_save_to_and_open_path() {
    bounded_case("inert_file", || {
        let dir = std::env::temp_dir().join(format!("hsk-studio-session-entry-references-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inert.handshake");
        let mut session = opened(AUDIO_ROUTING_ENTRY);
        let s = step(vec![edit(AUDIO_ROUTING_ENTRY, Removed::Sequence)], None, None);
        session.submit_sequence_records(&s, 3, &token()).unwrap();
        session.save_to(&path, &token()).unwrap();
        let reopened = match Session::open_path(&path, SessionConfig::new(actor()), ports(&Frames::default()), &token()) {
            Ok(s) => s,
            Err(f) => panic!("{f:?}"),
        };
        assert_eq!(reopened.document_entry(AUDIO_ROUTING_ENTRY), Some(body_of(AUDIO_ROUTING_ENTRY).as_slice()));
        assert_eq!(reopened.audio_routing_records(&token()).unwrap(), Some(routing()));
        assert!(reopened.loss().is_empty());
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    });
}

// ----- The listing -----

/// The typed listing: ids and kinds, no values, in stored order, read-only, no frame.
#[test]
fn the_listing_names_what_is_absent_and_changes_nothing() {
    bounded_case("listing", || {
        use StudioInertRoutingKind as K;
        let row = |kind: K, id: String| StudioInertRoutingReference { sequence_id: "seq-a".into(), kind, id };
        let (mut session, frames) = open_counted(&package_of(&all_sequences(), &[entry(AUDIO_ROUTING_ENTRY, &body_of(AUDIO_ROUTING_ENTRY))]));
        assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![]);
        session.submit_timeline(&timeline_step(AUDIO_ROUTING_ENTRY, Removed::Track), 3, &token()).unwrap();
        let start = frames.count();
        let listed = session.inert_audio_routing_references(&token()).unwrap();
        assert_eq!(frames.count(), start, "a typed read sends no frame");
        assert_eq!(
            listed,
            vec![
                row(K::Route, audio_id("seq-a")),
                row(K::Route, submix_id("seq-a")),
                row(K::BusEffects, submix_id("seq-a")),
                row(K::TrackAutomation, audio_id("seq-a")),
                row(K::TrackAutomation, submix_id("seq-a")),
                row(K::ClipGain, identity("SCLP", ENVELOPED)),
                row(K::ClipPan, identity("SCLP", ENVELOPED)),
            ]
        );

        // An absent sequence is one row, and the other sequence's routing still reads.
        let mut session = opened(AUDIO_ROUTING_ENTRY);
        session.submit_sequence_records(&step(vec![edit(AUDIO_ROUTING_ENTRY, Removed::Sequence)], None, None), 3, &token()).unwrap();
        assert_eq!(
            session.inert_audio_routing_references(&token()).unwrap(),
            vec![StudioInertRoutingReference { sequence_id: "seq-b".into(), kind: K::Routing, id: "seq-b".into() }]
        );
        let held = session.audio_routing_records(&token()).unwrap().unwrap();
        assert_eq!(held.routings[0], routing().routings[0], "seq-a's routing reads");

        let mut session = opened(MULTICAM_ENTRY);
        session.submit_timeline(&timeline_step(MULTICAM_ENTRY, Removed::Clip), 3, &token()).unwrap();
        assert_eq!(session.inert_multicam_clips(&token()).unwrap(), multicam_body().clips);

        // While the entry is invalid both are the refusal of the read, not a guess.
        let held = [entry(AUDIO_ROUTING_ENTRY, b"{not json"), entry(MULTICAM_ENTRY, b"{not json")];
        let session = open(&package_of(&all_sequences(), &held));
        assert_eq!(session.inert_audio_routing_references(&token()).unwrap_err().code(), "document_rejected");
        assert_eq!(session.inert_multicam_clips(&token()).unwrap_err().code(), "document_rejected");
        // No entry, nothing to list.
        let session = open(&package_of(&all_sequences(), &[]));
        assert_eq!(session.inert_audio_routing_references(&token()).unwrap(), vec![]);
        assert_eq!(session.inert_multicam_clips(&token()).unwrap(), vec![]);
    });
}

// ----- A damaged file (brief item 5) -----

/// The ways a routing entry can be damaged while the package still opens. An inert id is not damage.
fn damaged_routings() -> Vec<(&'static str, Vec<Quarantined>)> {
    let mut twice = routing();
    twice.routings.push(StudioAudioRouting::new("seq-a"));
    let mut video_clip = routing();
    video_clip.routings[0].clip_gains[0].clip_id = identity("SCLP", NESTED);
    let mut out_of_range = routing();
    out_of_range.routings.push(StudioAudioRouting::new("seq-gone"));
    out_of_range.routings[2].master.volume = 99.0;
    let good = serde_json::to_vec(&routing()).unwrap();
    let mut other = routing();
    other.routings.pop();
    let other = serde_json::to_vec(&other).unwrap();
    vec![
        ("not json", vec![entry(AUDIO_ROUTING_ENTRY, b"{not json")]),
        ("wrong shape", vec![entry(AUDIO_ROUTING_ENTRY, b"{}")]),
        ("unknown field", vec![entry(AUDIO_ROUTING_ENTRY, br#"{"routings":[],"x":1}"#)]),
        ("one sequence twice", vec![entry(AUDIO_ROUTING_ENTRY, &serde_json::to_vec(&twice).unwrap())]),
        ("an envelope of a clip held on a video track", vec![entry(AUDIO_ROUTING_ENTRY, &serde_json::to_vec(&video_clip).unwrap())]),
        ("an inert routing with a value out of range", vec![entry(AUDIO_ROUTING_ENTRY, &serde_json::to_vec(&out_of_range).unwrap())]),
        ("nesting past the budget", vec![entry(AUDIO_ROUTING_ENTRY, "[".repeat(100_000).as_bytes())]),
        ("two entries of the token", vec![entry(AUDIO_ROUTING_ENTRY, &good), entry(AUDIO_ROUTING_ENTRY, &other)]),
    ]
}

/// It opens, one note per entry, the read gives the typed refusal, save keeps the bytes, a removal is
/// not held up by it, and a direct write replaces it and takes the note.
#[test]
fn a_damaged_routing_opens_noted_refuses_typed_and_save_keeps_its_bytes() {
    bounded_case("routing_damaged", || {
        for (what, held) in damaged_routings() {
            let mut session = open(&package_of(&all_sequences(), &held));
            assert_eq!(session.loss().len(), held.len(), "{what}: {:?}", session.loss());
            assert!(session.loss().iter().all(|n| n.reason == hsk_studio_package::LossReason::Opaque), "{what}");
            assert!(session.audio_routing_records(&token()).is_err(), "{what}: refused on read");
            let again = open(&session.export_checkpoint(&token()).unwrap().bytes);
            assert_eq!(again.loss().len(), held.len(), "{what}: noted again");
            assert!(again.audio_routing_records(&token()).is_err(), "{what}");
            for q in &held {
                let names: Vec<&str> = again.loss().iter().map(|n| n.name.as_str()).collect();
                assert!(names.contains(&q.name.as_str()), "{what}: same digest, so the same bytes: {names:?}");
            }
            let removal = step(vec![edit(AUDIO_ROUTING_ENTRY, Removed::Sequence)], None, None);
            session.submit_sequence_records(&removal, 3, &token()).unwrap_or_else(|f| panic!("{what}: {f:?}"));
            assert_eq!(session.loss().len(), held.len(), "{what}: still noted");
            session.set_document_entry(AUDIO_ROUTING_ENTRY, Some(body_of(AUDIO_ROUTING_ENTRY)), &token()).unwrap();
            assert!(session.loss().is_empty(), "{what}: the note named an entry that is gone: {:?}", session.loss());
            assert_eq!(session.audio_routing_records(&token()).unwrap(), Some(routing()), "{what}");
        }
    });
}

/// A valid routing has no note and reads typed; a package without one reads as none.
#[test]
fn a_valid_routing_opens_typed_without_a_note() {
    bounded_case("routing_typed", || {
        let session = opened(AUDIO_ROUTING_ENTRY);
        assert!(session.loss().is_empty(), "{:?}", session.loss());
        assert_eq!(session.audio_routing_records(&token()).unwrap(), Some(routing()));
        assert_eq!(open(&package_of(&all_sequences(), &[])).audio_routing_records(&token()).unwrap(), None);
    });
}

// ----- F-R56-7: the undo of a step that replaced invalid records -----

fn dangling_settings() -> StudioSequenceSettingsRecords {
    records(vec![settings_entry("seq-x", "custom", F24)])
}
fn good_settings() -> StudioSequenceSettingsRecords {
    records(vec![settings_entry("seq-a", "custom", F24)])
}
fn opened_with_dangling_settings() -> Session {
    open(&package_of(&all_sequences(), &[entry(SEQUENCE_SETTINGS_ENTRY, &serde_json::to_vec(&dangling_settings()).unwrap())]))
}
fn repair() -> RecordEdit<StudioSequenceSettingsRecords> {
    RecordEdit { expect: Some(dangling_settings()), write: Some(good_settings()) }
}
fn renamed_b() -> StudioSequence {
    let mut s = seq_b_full();
    s.name = "renamed".into();
    s
}

/// The repair alone has no undo: the answer says so, and no step that could never apply is handed out.
#[test]
fn a_repair_alone_answers_that_no_undo_exists() {
    bounded_case("repair_alone", || {
        let mut session = opened_with_dangling_settings();
        let applied = session.submit_sequence_records(&step(vec![], Some(repair()), None), 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_inverse, None);
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Unavailable(vec![SEQUENCE_SETTINGS_ENTRY])));
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good_settings()));
    });
}

/// A repair with a sequence edit: the sequence edit keeps its undo, the repair stands, and the redo
/// redoes the sequence edit. The inverse handed out applies.
#[test]
fn a_repair_with_a_sequence_edit_keeps_the_undo_of_the_sequence_edit() {
    bounded_case("repair_with_edit", || {
        let mut session = opened_with_dangling_settings();
        let rename = SequenceEdit::Replace { expect: seq_b_full(), write: renamed_b() };
        let applied = session.submit_sequence_records(&step(vec![rename], Some(repair()), None), 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::WithoutEntries(vec![SEQUENCE_SETTINGS_ENTRY])));
        assert_eq!(applied.sequence_records_inverse.as_ref().unwrap().settings, None, "the entry is left alone");
        let undone = session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(session.snapshot().document().sequences[1], seq_b_full());
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good_settings()), "the repair stands");
        assert_eq!(undone.sequence_records_undo, Some(SequenceRecordsUndo::Complete));
        session.submit_sequence_records(&fresh_inverse(&undone, "cmd.redo"), 5, &token()).unwrap();
        assert_eq!(session.snapshot().document().sequences[1], renamed_b());
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good_settings()));
    });
}

/// A repair with another record edit: that edit keeps its undo.
#[test]
fn a_repair_with_a_record_edit_keeps_the_undo_of_the_record_edit() {
    bounded_case("repair_with_record", || {
        let mut session = opened_with_dangling_settings();
        let presets = RecordEdit { expect: None, write: Some(library(vec![preset("mine", F25)])) };
        let applied = session.submit_sequence_records(&step(vec![], Some(repair()), Some(presets)), 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::WithoutEntries(vec![SEQUENCE_SETTINGS_ENTRY])));
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(session.sequence_presets(&token()).unwrap(), None);
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), Some(good_settings()));
    });
}

/// A repair whose new records name the sequence the same step inserts: undoing the insert would orphan
/// the repair, which cannot be put back either. No undo exists, and the answer says so.
#[test]
fn a_repair_naming_what_the_step_inserts_answers_that_no_undo_exists() {
    bounded_case("repair_with_insert", || {
        let mut session = opened_with_dangling_settings();
        let write = records(vec![settings_entry("seq-new", "custom", F24)]);
        let insert = SequenceEdit::Insert { at: 0, write: sequence("seq-new", F24, vec![]) };
        let s = step(vec![insert], Some(RecordEdit { expect: Some(dangling_settings()), write: Some(write) }), None);
        let applied = session.submit_sequence_records(&s, 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_inverse, None);
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Unavailable(vec![SEQUENCE_SETTINGS_ENTRY])));
    });
}

/// Every record half follows the same rule, and a retry under a key replays the same answer.
#[test]
fn every_record_half_answers_the_same_and_a_replay_repeats_it() {
    bounded_case("repair_halves", || {
        let mut bad = display_body();
        bad.sequences[0].heights.video = 0;
        let mut session = open(&package_of(&all_sequences(), &[entry(TRACK_DISPLAY_ENTRY, &serde_json::to_vec(&bad).unwrap())]));
        let s = SequenceRecordsStep { track_display: Some(RecordEdit { expect: Some(bad), write: Some(display_body()) }), ..step(vec![], None, None) };
        let applied = session.submit_sequence_records(&s, 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Unavailable(vec![TRACK_DISPLAY_ENTRY])));
        // Presets: one id twice parses and does not validate.
        let bad = library(vec![preset("mine", F25), preset("mine", F25)]);
        let mut session = open(&package_of(&all_sequences(), &[entry(SEQUENCE_PRESETS_ENTRY, &serde_json::to_vec(&bad).unwrap())]));
        let s = step(vec![], None, Some(RecordEdit { expect: Some(bad), write: None }));
        let key = IdempotencyKey::parse("repair-1").unwrap();
        let applied = session.submit_sequence_records_keyed(&key, &s, 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Unavailable(vec![SEQUENCE_PRESETS_ENTRY])));
        let replay = session.submit_sequence_records_keyed(&key, &s, 3, &token()).unwrap();
        assert!(replay.replayed);
        assert_eq!((replay.sequence_records_undo, replay.sequence_records_inverse), (applied.sequence_records_undo, applied.sequence_records_inverse));
    });
}

/// A step over valid records is untouched by this: complete, and the inverse restores them.
#[test]
fn a_step_over_valid_records_has_a_complete_undo() {
    bounded_case("valid_undo", || {
        let mut session = opened(SEQUENCE_SETTINGS_ENTRY);
        let held = session.sequence_settings_records(&token()).unwrap();
        let s = step(vec![], Some(RecordEdit { expect: held.clone(), write: Some(good_settings()) }), None);
        let applied = session.submit_sequence_records(&s, 3, &token()).unwrap();
        assert_eq!(applied.sequence_records_undo, Some(SequenceRecordsUndo::Complete));
        session.submit_sequence_records(&fresh_inverse(&applied, "cmd.undo"), 4, &token()).unwrap();
        assert_eq!(session.sequence_settings_records(&token()).unwrap(), held);
    });
}
