//! Lane SMB-PR-MULTICAM-1: the multicam record set (`hsk.studio.multicam_records@1`, STU-VID-033 /
//! STU-VID-030a) round-trips through wire JSON and is validated against the document: declared
//! source sequences, angle numbering, clip references, angle ranges and counts are typed refusals.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;

const F: u64 = 10_160_640_000;
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";

fn ident(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

struct Granted;
impl Resolver for Granted {
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
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

fn clip(n: u32, source: ClipSource, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: ident("SCLP", n),
        source_ref: source,
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn media(n: u32) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: format!("camera-{n}"),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: format!("ab{n:04}") },
        asset_id: None,
    }
}
fn nest(sequence_id: &str) -> ClipSource {
    ClipSource::Sequence { sequence_id: sequence_id.into() }
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: ident("STRK", n),
        kind,
        index,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}
fn sequence(sequence_id: &str, tracks: Vec<StudioTrack>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: sequence_id.into(),
        name: sequence_id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks,
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}

/// `seq-mc`: `angles` video tracks (track ids 100+i, index i, a camera clip each) and two audio
/// tracks paired with angles 1 and 2; `seq-main`: V1 multicam clip 1 and A1 linked multicam clip 2,
/// plus V2 media clip 3.
fn document(angles: u32) -> StudioDocument {
    let json = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[]
    });
    let mut d: StudioDocument = serde_json::from_value(json).unwrap();
    let mut tracks: Vec<StudioTrack> =
        (0..angles).map(|i| track(100 + i, TrackKind::Video, i, vec![clip(100 + i, media(i), 0, 8)])).collect();
    tracks.push(track(200, TrackKind::Audio, 0, vec![clip(200, media(900), 0, 8)]));
    tracks.push(track(201, TrackKind::Audio, 1, vec![clip(201, media(901), 0, 8)]));
    d.sequences.push(sequence("seq-mc", tracks));
    let mut v = clip(1, nest("seq-mc"), 0, 8);
    v.link_group = Some("lg-1".into());
    let mut a = clip(2, nest("seq-mc"), 0, 8);
    a.link_group = Some("lg-1".into());
    d.sequences.push(sequence(
        "seq-main",
        vec![
            track(1, TrackKind::Video, 0, vec![v]),
            track(3, TrackKind::Video, 1, vec![clip(3, media(7), 0, 4)]),
            track(2, TrackKind::Audio, 0, vec![a]),
        ],
    ));
    validate_document(d.clone(), Budget::default(), &CancellationToken::default(), &Granted).expect("fixture is a valid document");
    d
}

fn mode(n: u32, angle: u32) -> StudioMulticamClip {
    StudioMulticamClip {
        clip_id: ident("SCLP", n),
        active_angle: angle,
        audio_angle: angle,
        audio_follows_video: true,
        follows_nest_setting: false,
        enabled: true,
    }
}
fn records(d: &StudioDocument, order: MulticamSelectionOrder) -> StudioMulticamRecords {
    let mut r = StudioMulticamRecords::new(DOC);
    r.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-mc".into(),
        selection_order: order,
        angle_order: multicam_angle_order(&d.sequences[0], order),
    });
    r.clips = vec![mode(1, 2), mode(2, 2)];
    r
}
fn check(r: &StudioMulticamRecords, d: &StudioDocument) -> Result<(), (Code, String)> {
    validate_multicam_records(r, d, Budget::default(), &CancellationToken::default()).map_err(|e| (e.code, e.target))
}
fn code(r: &StudioMulticamRecords, d: &StudioDocument) -> Option<Code> {
    check(r, d).err().map(|(c, _)| c)
}

#[test]
fn multicam_records_validate_against_the_document() {
    let d = document(4);
    let token = CancellationToken::default();
    let r = records(&d, MulticamSelectionOrder::SourceOrder);
    assert_eq!(check(&r, &d), Ok(()));
    // Numbering: source order is ascending track index, top-down the reverse (declared reading).
    assert_eq!(r.multicam_sequences[0].angle_order, (100..104).map(|n| ident("STRK", n)).collect::<Vec<_>>());
    let top = records(&d, MulticamSelectionOrder::TopDown);
    assert_eq!(top.multicam_sequences[0].angle_order[0], ident("STRK", 103));
    assert_eq!(check(&top, &d), Ok(()));
    // Paired audio: same per-kind index; angles 3 and 4 are silent.
    assert_eq!(multicam_paired_audio_track(&d.sequences[0], &ident("STRK", 101)), Some(ident("STRK", 201).as_str()));
    assert_eq!(multicam_paired_audio_track(&d.sequences[0], &ident("STRK", 103)), None);

    // Wire round trip through the bounded parser; duplicate keys and unknown fields are refused.
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(parse_multicam_records(&bytes, &d, Budget::default(), &token).unwrap(), r);
    let dup = br#"{"schema_id":"hsk.studio.multicam_records@1","document_id":"x","document_id":"y"}"#;
    assert_eq!(parse_multicam_records(dup, &d, Budget::default(), &token).unwrap_err().code, Code::DuplicateField);
    let mut extra: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    extra["clips"][0]["camera"] = 3.into();
    let extra = serde_json::to_vec(&extra).unwrap();
    assert_eq!(parse_multicam_records(&extra, &d, Budget::default(), &token).unwrap_err().code, Code::InvalidJson);
    let tight = Budget { input_bytes: 16, ..Budget::default() };
    assert_eq!(parse_multicam_records(&bytes, &d, tight, &token).unwrap_err().code, Code::BudgetExceeded);

    // angle_order is never free: a swapped order or the wrong selection order is refused.
    let mut swapped = r.clone();
    swapped.multicam_sequences[0].angle_order.swap(0, 1);
    assert_eq!(check(&swapped, &d), Err((Code::InvalidField, "seq-mc/angle_order".into())));
    let mut wrong = r.clone();
    wrong.multicam_sequences[0].selection_order = MulticamSelectionOrder::TopDown;
    assert_eq!(code(&wrong, &d), Some(Code::InvalidField));
    let mut short = r.clone();
    short.multicam_sequences[0].angle_order.pop();
    assert_eq!(code(&short, &d), Some(Code::InvalidField));

    // Angles are 1-based and within the angle count; audio follows video exactly.
    for bad in [0u32, 5, u32::MAX] {
        let mut a = r.clone();
        a.clips[0].active_angle = bad;
        a.clips[0].audio_angle = bad;
        assert_eq!(code(&a, &d), Some(Code::InvalidField), "angle {bad}");
    }
    let mut split = r.clone();
    split.clips[0].audio_angle = 1;
    assert_eq!(check(&split, &d), Err((Code::InvalidField, format!("{}/audio_angle", ident("SCLP", 1)))));
    split.clips[0].audio_follows_video = false;
    assert_eq!(check(&split, &d), Ok(()), "without audio_follows_video the audio keeps its own angle");

    // The clip must reference a declared multicam source: a media clip is refused.
    let mut on_media = r.clone();
    on_media.clips.push(mode(3, 1));
    assert_eq!(check(&on_media, &d), Err((Code::DanglingReference, format!("{}/source_ref", ident("SCLP", 3)))));
    // A source sequence that is the clip's own sequence is refused.
    let mut own = d.clone();
    own.sequences[0].tracks[0].clips[0].source_ref = nest("seq-mc");
    let mut self_ref = records(&own, MulticamSelectionOrder::SourceOrder);
    self_ref.clips.push(mode(100, 1));
    assert_eq!(check(&self_ref, &own), Err((Code::InvalidField, format!("{}/source_ref", ident("SCLP", 100)))));
    // Another document id is refused.
    let mut other = r.clone();
    other.document_id = ident("SDOC", 2);
    assert_eq!(check(&other, &d), Err((Code::InvalidField, "document_id".into())));
}

#[test]
fn sixteen_angles_are_addressable() {
    for angles in [16u32, 64] {
        let d = document(angles);
        let mut r = records(&d, MulticamSelectionOrder::SourceOrder);
        r.clips[0].active_angle = angles;
        r.clips[0].audio_angle = angles;
        assert_eq!(check(&r, &d), Ok(()), "{angles} angles");
        assert!(usize::try_from(angles).unwrap() >= MIN_ADDRESSABLE_ANGLES);
        r.clips[0].active_angle = angles + 1;
        r.clips[0].audio_angle = angles + 1;
        assert_eq!(code(&r, &d), Some(Code::InvalidField));
    }
}

#[test]
fn dangling_and_duplicate_entries_are_refused() {
    let d = document(4);
    let r = records(&d, MulticamSelectionOrder::SourceOrder);
    let mut unknown_seq = r.clone();
    unknown_seq.multicam_sequences[0].sequence_id = "seq-gone".into();
    assert_eq!(check(&unknown_seq, &d), Err((Code::DanglingReference, "seq-gone".into())));
    let mut bad_key = r.clone();
    bad_key.multicam_sequences[0].sequence_id = "../seq".into();
    assert_eq!(code(&bad_key, &d), Some(Code::InvalidId));
    let mut unknown_clip = r.clone();
    unknown_clip.clips.push(mode(77, 1));
    assert_eq!(check(&unknown_clip, &d), Err((Code::DanglingReference, ident("SCLP", 77))));
    let mut bad_clip_id = r.clone();
    bad_clip_id.clips[0].clip_id = "clip-1".into();
    assert_eq!(code(&bad_clip_id, &d), Some(Code::InvalidId));
    let mut dup_seq = r.clone();
    dup_seq.multicam_sequences.push(dup_seq.multicam_sequences[0].clone());
    assert_eq!(check(&dup_seq, &d), Err((Code::DuplicateIdentity, "seq-mc".into())));
    let mut dup_clip = r.clone();
    dup_clip.clips.push(mode(1, 1));
    assert_eq!(check(&dup_clip, &d), Err((Code::DuplicateIdentity, ident("SCLP", 1))));
    // An angle naming an audio track, or a track of another sequence, dangles.
    let mut audio_angle = r.clone();
    audio_angle.multicam_sequences[0].angle_order[0] = ident("STRK", 200);
    assert_eq!(code(&audio_angle, &d), Some(Code::DanglingReference));
    let mut foreign = r.clone();
    foreign.multicam_sequences[0].angle_order[0] = ident("STRK", 1);
    assert_eq!(code(&foreign, &d), Some(Code::DanglingReference));
    // An undeclared source: the clips reference seq-mc but no multicam sequence is declared.
    let mut undeclared = r.clone();
    undeclared.multicam_sequences.clear();
    assert_eq!(code(&undeclared, &d), Some(Code::DanglingReference));
    // A sequence with no video track has no angle.
    let mut empty = d.clone();
    empty.sequences[0].tracks.retain(|t| t.kind == TrackKind::Audio);
    let mut no_angles = r.clone();
    no_angles.multicam_sequences[0].angle_order.clear();
    no_angles.clips.clear();
    assert_eq!(code(&no_angles, &empty), Some(Code::InvalidField));

    // Counts: the caller node budget and the hard ceilings, checked before any entry is walked.
    let token = CancellationToken::default();
    let tight = Budget { nodes: 2, ..Budget::default() };
    assert_eq!(validate_multicam_records(&r, &d, tight, &token).unwrap_err().code, Code::BudgetExceeded);
    let mut bomb = r.clone();
    bomb.clips = vec![mode(1, 1); MAX_MULTICAM_CLIPS + 1];
    assert_eq!(check(&bomb, &d), Err((Code::BudgetExceeded, "clips".into())));
    let mut seq_bomb = r.clone();
    seq_bomb.multicam_sequences = vec![r.multicam_sequences[0].clone(); MAX_MULTICAM_SEQUENCES + 1];
    assert_eq!(check(&seq_bomb, &d), Err((Code::BudgetExceeded, "multicam_sequences".into())));
    let mut angle_bomb = r.clone();
    angle_bomb.multicam_sequences[0].angle_order = vec![ident("STRK", 100); MAX_MULTICAM_ANGLES + 1];
    assert_eq!(check(&angle_bomb, &d), Err((Code::BudgetExceeded, "seq-mc/angle_order".into())));
    // Cancellation is honoured.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(validate_multicam_records(&r, &d, Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn records_absent_keep_documents_byte_identical() {
    let d = document(4);
    let before = serde_json::to_vec(&d).unwrap();
    let r = records(&d, MulticamSelectionOrder::SourceOrder);
    assert_eq!(check(&r, &d), Ok(()));
    // The record set lives beside the document: the document wire and schema are untouched.
    assert_eq!(serde_json::to_vec(&d).unwrap(), before);
    let reread: StudioDocument = serde_json::from_slice(&before).unwrap();
    assert_eq!(serde_json::to_vec(&reread).unwrap(), before);
    assert!(!serde_json::to_string(&document_schema()).unwrap().contains("multicam"));
    // An empty record set encodes only its identity (empty lists are not encoded).
    let empty = StudioMulticamRecords::new(DOC);
    assert_eq!(
        serde_json::to_string(&empty).unwrap(),
        format!(r#"{{"schema_id":"hsk.studio.multicam_records@1","document_id":"{DOC}"}}"#)
    );
    assert_eq!(check(&empty, &d), Ok(()));
    let schema = serde_json::to_string(&multicam_records_schema()).unwrap();
    assert!(schema.contains("hsk.studio.multicam_records@1") && schema.contains("audio_follows_video"));
}
