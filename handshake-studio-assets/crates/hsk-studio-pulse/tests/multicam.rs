//! Lane SMB-PR-MULTICAM-1 (STU-VID-033 / STU-VID-040): multicam planners. Every step is proven
//! ONE step by planning its inverse and comparing the exact prior sequences and records.
//! NOT_PROVEN: program-monitor playback and composite render of the active angle (pulse playback
//! and composite do not resolve nested sequences); the paged preview grid in the window; Premiere
//! behaviour (no Premiere oracle).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

fn ident(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    ident("SCLP", n)
}
fn tid(n: u32) -> String {
    ident("STRK", n)
}

struct Media;
impl Resolver for Media {
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

fn media(n: u32) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: format!("camera-{n}"),
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: format!("ab{n:04}") },
        asset_id: None,
    }
}
/// Clip `n` of `source`: source frames `[si, si + frames)` at timeline frame `start`.
fn clip(n: u32, source: ClipSource, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: source,
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn nest() -> ClipSource {
    ClipSource::Sequence { sequence_id: "seq-mc".into() }
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    }
}
fn sequence(id: &str, tracks: Vec<StudioTrack>) -> StudioSequence {
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks,
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}
fn empty_document(sequences: Vec<StudioSequence>) -> Snapshot {
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: ident("SDOC", 1),
        revision: 3,
        geometry_unit: GeometryUnit::Px,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences,
    };
    validate_document(document, Budget::default(), &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}

/// `seq-mc` (four angles): V idx0 cam 100 src 0..48 @0; V idx1 cam 101 src 0..46 @2; V idx2 two clips
/// with a gap (102 src 0..20 @0, 103 src 100..118 @30); V idx3 cam 104 src 10..58 @0; A idx0 sound
/// 200 src 0..48 @0; A idx1 sound 201 src 0..48 @0. `seq-main`: V1 multicam clip 1 window 4..28 @0
/// linked (lg-1) with A1 multicam clip 2 (same window); V2 media clip 3 (0..10 @0).
fn source_sequence() -> StudioSequence {
    sequence(
        "seq-mc",
        vec![
            track(100, TrackKind::Video, 0, vec![clip(100, media(0), 0, 48, 0)]),
            track(101, TrackKind::Video, 1, vec![clip(101, media(1), 0, 46, 2)]),
            track(102, TrackKind::Video, 2, vec![clip(102, media(2), 0, 20, 0), clip(103, media(3), 100, 18, 30)]),
            track(103, TrackKind::Video, 3, vec![clip(104, media(4), 10, 48, 0)]),
            track(200, TrackKind::Audio, 0, vec![clip(200, media(90), 0, 48, 0)]),
            track(201, TrackKind::Audio, 1, vec![clip(201, media(91), 0, 48, 0)]),
        ],
    )
}
fn fixture() -> (Snapshot, StudioMulticamRecords) {
    let mut v = clip(1, nest(), 4, 24, 0);
    v.link_group = Some("lg-1".into());
    let mut a = clip(2, nest(), 4, 24, 0);
    a.link_group = Some("lg-1".into());
    let main = sequence(
        "seq-main",
        vec![
            track(1, TrackKind::Video, 0, vec![v]),
            track(3, TrackKind::Video, 1, vec![clip(3, media(7), 0, 10, 0)]),
            track(2, TrackKind::Audio, 0, vec![a]),
        ],
    );
    let snapshot = empty_document(vec![source_sequence(), main]);
    let mut records = StudioMulticamRecords::new(ident("SDOC", 1));
    records.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-mc".into(),
        selection_order: MulticamSelectionOrder::SourceOrder,
        angle_order: multicam_angle_order(&snapshot.document().sequences[0], MulticamSelectionOrder::SourceOrder),
    });
    for n in [1, 2] {
        records.clips.push(StudioMulticamClip {
            clip_id: cid(n),
            active_angle: 2,
            audio_angle: 2,
            audio_follows_video: true,
            follows_nest_setting: false,
            enabled: true,
        });
    }
    validate_multicam_records(&records, snapshot.document(), Budget::default(), &CancellationToken::default()).unwrap();
    (snapshot, records)
}

fn with_ctx<T>(cancel: &CancellationToken, f: impl FnOnce(&EditContext) -> T) -> T {
    f(&EditContext { budget: Budget::default(), resolver: &Media, extents: &UnknownExtents, cancel })
}
fn ok<T>(f: impl FnOnce(&EditContext) -> MulticamResult<T>) -> T {
    let cancel = CancellationToken::default();
    with_ctx(&cancel, f).expect("planner accepts")
}
fn refused<T: std::fmt::Debug>(f: impl FnOnce(&EditContext) -> MulticamResult<T>) -> String {
    let cancel = CancellationToken::default();
    with_ctx(&cancel, f).expect_err("planner refuses").code().to_owned()
}
fn main_seq(s: &Snapshot) -> &StudioSequence {
    s.document().sequences.iter().find(|q| q.sequence_id == "seq-main").unwrap()
}
fn clips_on(s: &Snapshot, track: u32) -> Vec<(String, u64, u64)> {
    main_seq(s)
        .tracks
        .iter()
        .find(|t| t.track_id == tid(track))
        .unwrap()
        .clips
        .iter()
        .map(|c| (c.clip_id.clone(), c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F))
        .collect()
}
fn angles(r: &StudioMulticamRecords, n: u32) -> (u32, u32) {
    let m = r.clip(&cid(n)).unwrap();
    (m.active_angle, m.audio_angle)
}
fn cut_ids(right: u32, partner: (u32, u32), group: &str) -> MulticamCutIds {
    MulticamCutIds {
        new_right_clip_id: cid(right),
        linked: Some(LinkedRazor { right_link_group: group.into(), right_clip_ids: vec![(cid(partner.0), cid(partner.1))] }),
    }
}
/// Plans the revert of `p` and checks it restores exactly `(snapshot, records)`; returns the redo.
fn assert_one_step(p: &MulticamProposal, snapshot: &Snapshot, records: &StudioMulticamRecords) {
    let undo = ok(|ctx| plan_multicam_revert(p.snapshot(), p.target_revision(), p.records(), p.inverse(), ctx));
    assert_eq!(undo.snapshot().document().sequences, snapshot.document().sequences, "undo restores the sequences");
    assert_eq!(undo.records(), records, "undo restores the records");
    let redo = ok(|ctx| plan_multicam_revert(undo.snapshot(), undo.target_revision(), undo.records(), undo.inverse(), ctx));
    assert_eq!(redo.snapshot().document().sequences, p.snapshot().document().sequences, "redo is the undo's inverse");
    assert_eq!(redo.records(), p.records());
    // The inverse is bound to the state the step left.
    assert_eq!(refused(|ctx| plan_multicam_revert(snapshot, snapshot.document().revision, records, p.inverse(), ctx)), "revert_mismatch");
}

#[test]
fn switch_with_cut_razors_and_sets_the_later_angle() {
    bounded_case("switch_with_cut", || {
        let (s, r) = fixture();
        let switch = SwitchAngle { clip_id: cid(1), angle: 3, cut_at: Some(Ticks::new(10 * F)), cut_ids: Some(cut_ids(11, (2, 12), "lg-2")) };
        let p = ok(|ctx| plan_multicam_switch(&s, 3, &r, &switch, ctx));
        assert_eq!(p.command_id(), "cmd.multicam.choose.camera.3");
        assert_eq!((p.base_revision(), p.target_revision()), (3, 4), "one revision");
        assert_eq!(clips_on(p.snapshot(), 1), vec![(cid(1), 0, 10), (cid(11), 10, 24)]);
        assert_eq!(clips_on(p.snapshot(), 2), vec![(cid(2), 0, 10), (cid(12), 10, 24)], "the linked audio is cut with it");
        assert_eq!(angles(p.records(), 1), (2, 2), "the earlier part keeps its angle");
        assert_eq!(angles(p.records(), 11), (3, 3), "the later part takes the angle");
        assert_eq!(angles(p.records(), 12), (3, 3), "audio follows video on the linked audio clip");
        assert_eq!(p.splits(), &[(cid(1), cid(11)), (cid(2), cid(12))]);
        let (id, before, after) = p.sequence_change().expect("one sequence changed");
        assert_eq!((id, before), ("seq-main", main_seq(&s)));
        assert_eq!(after, main_seq(p.snapshot()));
        // The right half reads the source sequence where the left half stops.
        let right = main_seq(p.snapshot()).tracks[0].clips[1].clone();
        assert_eq!((right.source_in.value() / F, right.source_out.value() / F), (14, 28));
        assert_one_step(&p, &s, &r);

        // A cut needs the right-half ids; a cut off the frame grid is refused by the razor.
        let missing = SwitchAngle { cut_ids: None, ..switch.clone() };
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &missing, ctx)), "insufficient_ids");
        let off_grid = SwitchAngle { cut_at: Some(Ticks::new(10 * F + 1)), ..switch.clone() };
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &off_grid, ctx)), "not_frame_aligned");
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 2, &r, &switch, ctx)), "stale_revision");
    });
}

#[test]
fn switch_without_cut_changes_the_whole_clip() {
    bounded_case("switch_without_cut", || {
        let (s, r) = fixture();
        let switch = SwitchAngle { clip_id: cid(1), angle: 4, cut_at: None, cut_ids: None };
        let p = ok(|ctx| plan_multicam_switch(&s, 3, &r, &switch, ctx));
        assert_eq!(p.command_id(), "cmd.multicam.choosenocut.camera4");
        assert_eq!(p.target_revision(), 3, "no sequence changed");
        assert_eq!(p.snapshot().document(), s.document());
        assert!(p.sequence_change().is_none() && p.inverse().sequences_restore().is_none());
        assert_eq!((angles(p.records(), 1), angles(p.records(), 2)), ((4, 4), (4, 4)));
        assert_one_step(&p, &s, &r);
        // The resolved angle follows the record for the whole clip.
        let at = |tick| ok(|ctx| resolve_active_angle(p.snapshot(), p.records(), &cid(1), Ticks::new(tick * F), ctx));
        assert_eq!(at(0).angle_track_id, tid(103));
        assert_eq!(at(23).angle_track_id, tid(103));

        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &SwitchAngle { angle: 5, ..switch.clone() }, ctx)), "angle_out_of_range");
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &SwitchAngle { angle: 0, ..switch.clone() }, ctx)), "angle_out_of_range");
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &SwitchAngle { angle: 2, ..switch.clone() }, ctx)), "no_change");
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &SwitchAngle { clip_id: cid(3), ..switch.clone() }, ctx)), "not_multicam");
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &SwitchAngle { clip_id: cid(99), ..switch.clone() }, ctx)), "unknown_clip");
        let ids_without_cut = SwitchAngle { cut_ids: Some(cut_ids(11, (2, 12), "lg-2")), ..switch.clone() };
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, &r, &ids_without_cut, ctx)), "invalid_input");
        // Multicam mode off: the clip plays as an ordinary nest and refuses switches.
        let off = ok(|ctx| plan_multicam_toggle_enabled(&s, 3, &r, &cid(1), ctx));
        assert_eq!(off.command_id(), "cmd.clip.multicam.toggle");
        assert!(!off.records().clip(&cid(1)).unwrap().enabled && !off.records().clip(&cid(2)).unwrap().enabled);
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, off.records(), &switch, ctx)), "not_multicam");
        let on = ok(|ctx| plan_multicam_toggle_enabled(&s, 3, off.records(), &cid(1), ctx));
        assert_eq!(on.records(), &r, "re-enabling keeps the stored angles");
        // A nest of a declared source without a mode gains one at angle 1.
        let mut bare = r.clone();
        bare.clips.clear();
        let fresh = ok(|ctx| plan_multicam_toggle_enabled(&s, 3, &bare, &cid(1), ctx));
        assert_eq!((angles(fresh.records(), 1), angles(fresh.records(), 2)), ((1, 1), (1, 1)));
        assert_eq!(refused(|ctx| plan_multicam_toggle_enabled(&s, 3, &r, &cid(3), ctx)), "not_multicam");
    });
}

#[test]
fn audio_follows_video_switches_audio_with_video() {
    bounded_case("audio_follows_video", || {
        let (s, r) = fixture();
        let off = ok(|ctx| plan_multicam_toggle_audio_follows_video(&s, 3, &r, &cid(1), ctx));
        assert_eq!(off.command_id(), "cmd.multicam.audio.follows.video");
        assert!(!off.records().clip(&cid(1)).unwrap().audio_follows_video);
        assert_one_step(&off, &s, &r);
        // Without it a switch moves the picture only: the audio clip keeps playing angle 2's sound.
        let video_only = SwitchAngle { clip_id: cid(1), angle: 3, cut_at: None, cut_ids: None };
        let p = ok(|ctx| plan_multicam_switch(&s, 3, off.records(), &video_only, ctx));
        assert_eq!((angles(p.records(), 1), angles(p.records(), 2)), ((3, 2), (3, 2)));
        let sound = ok(|ctx| resolve_active_angle(&s, p.records(), &cid(2), Ticks::new(5 * F), ctx));
        assert_eq!((sound.audio_track_id.as_deref(), sound.sound.clone()), (Some(tid(201).as_str()), Some((cid(201), Ticks::new(9 * F)))));
        // Turning it back on aligns the audio with the picture; the next switch moves both.
        let on = ok(|ctx| plan_multicam_toggle_audio_follows_video(&s, 3, p.records(), &cid(1), ctx));
        assert_eq!(angles(on.records(), 2), (3, 3));
        let both = ok(|ctx| plan_multicam_switch(&s, 3, on.records(), &SwitchAngle { angle: 1, ..video_only.clone() }, ctx));
        assert_eq!((angles(both.records(), 1), angles(both.records(), 2)), ((1, 1), (1, 1)));
        let sound = ok(|ctx| resolve_active_angle(&s, both.records(), &cid(2), Ticks::new(5 * F), ctx));
        assert_eq!(sound.audio_track_id, Some(tid(200)));
        // Angle 3 has no paired audio track: a silent angle, never another angle's sound.
        assert_eq!(refused(|ctx| plan_multicam_switch(&s, 3, on.records(), &SwitchAngle { angle: 3, ..video_only.clone() }, ctx)), "no_change");
        let silent = ok(|ctx| resolve_active_angle(&s, on.records(), &cid(2), Ticks::new(5 * F), ctx));
        assert_eq!((silent.audio_track_id, silent.sound), (None, None));
        // STU-VID-030a follow-nest is an explicit boolean toggled as one step.
        let nest_on = ok(|ctx| plan_multicam_toggle_follows_nest(&s, 3, &r, &cid(1), ctx));
        assert_eq!(nest_on.command_id(), "cmd.multicam.follows.nest.setting");
        assert!(nest_on.records().clip(&cid(1)).unwrap().follows_nest_setting);
        assert_one_step(&nest_on, &s, &r);
    });
}

#[test]
fn selection_order_renumbers_angles() {
    bounded_case("selection_order", || {
        let (s, r) = fixture();
        let p = ok(|ctx| plan_multicam_angle_numbering(&s, 3, &r, "seq-mc", MulticamSelectionOrder::TopDown, ctx));
        assert_eq!(p.command_id(), "cmd.multicam.selection.top.down");
        let decl = p.records().multicam_sequence("seq-mc").unwrap();
        assert_eq!(decl.angle_order, vec![tid(103), tid(102), tid(101), tid(100)]);
        // Angle 2 (track 101) is angle 3 in top-down numbering: the clip shows the same track.
        assert_eq!(angles(p.records(), 1), (3, 3));
        let before = ok(|ctx| resolve_active_angle(&s, &r, &cid(1), Ticks::new(F), ctx));
        let after = ok(|ctx| resolve_active_angle(&s, p.records(), &cid(1), Ticks::new(F), ctx));
        assert_eq!((before.angle_track_id, before.picture), (after.angle_track_id, after.picture));
        assert_one_step(&p, &s, &r);
        assert_eq!(refused(|ctx| plan_multicam_angle_numbering(&s, 3, &r, "seq-mc", MulticamSelectionOrder::SourceOrder, ctx)), "no_change");
        assert_eq!(refused(|ctx| plan_multicam_angle_numbering(&s, 3, &r, "seq-main", MulticamSelectionOrder::TopDown, ctx)), "not_multicam");
    });
}

#[test]
fn create_multicam_source_aligns_angles_by_caller_offsets() {
    bounded_case("create_multicam_source", || {
        let main = sequence("seq-main", vec![track(1, TrackKind::Video, 0, vec![clip(3, media(7), 0, 10, 0)])]);
        let s = empty_document(vec![main]);
        let r = StudioMulticamRecords::new(ident("SDOC", 1));
        let request = MulticamSourceRequest {
            sequence_id: "seq-mc".into(),
            name: "Interview multicam".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            sync: MulticamSync::CallerOffsets,
            selection_order: MulticamSelectionOrder::SourceOrder,
            angles: (0..3u32)
                .map(|i| MulticamAngleInput {
                    video: clip(10 + i, media(i), 0, 20, 0),
                    audio: (i < 2).then(|| clip(20 + i, media(50 + i), 0, 20, 0)),
                    offset: Ticks::new(u64::from(i) * 3 * F),
                })
                .collect(),
            video_track_ids: (0..3).map(|i| tid(300 + i)).collect(),
            audio_track_ids: (0..3).map(|i| tid(400 + i)).collect(),
        };
        let p = ok(|ctx| plan_create_multicam_source(&s, 3, &r, &request, ctx));
        assert_eq!((p.command_id(), p.target_revision()), ("cmd.clip.create.multicam", 4));
        let created = p.snapshot().document().sequences.iter().find(|q| q.sequence_id == "seq-mc").unwrap();
        let starts: Vec<(TrackKind, u32, Vec<u64>)> =
            created.tracks.iter().map(|t| (t.kind, t.index, t.clips.iter().map(|c| c.timeline_start.value() / F).collect())).collect();
        assert_eq!(
            starts,
            vec![
                (TrackKind::Video, 0, vec![0]),
                (TrackKind::Video, 1, vec![3]),
                (TrackKind::Video, 2, vec![6]),
                (TrackKind::Audio, 0, vec![0]),
                (TrackKind::Audio, 1, vec![3]),
                (TrackKind::Audio, 2, vec![]),
            ]
        );
        assert_eq!(p.records().multicam_sequence("seq-mc").unwrap().angle_order, vec![tid(300), tid(301), tid(302)]);
        assert!(p.sequence_change().is_none(), "a created sequence is not a one-sequence timeline step");
        assert_one_step(&p, &s, &r);

        let with = |f: &dyn Fn(&mut MulticamSourceRequest)| {
            let mut q = request.clone();
            f(&mut q);
            refused(|ctx| plan_create_multicam_source(&s, 3, &r, &q, ctx))
        };
        assert_eq!(with(&|q| q.sync = MulticamSync::Timecode), "multicam_sync_unsupported");
        assert_eq!(with(&|q| q.sync = MulticamSync::AudioWaveform), "multicam_sync_unsupported");
        assert_eq!(with(&|q| q.sync = MulticamSync::InPoints), "invalid_input", "in points need zero offsets");
        assert_eq!(with(&|q| q.angles[1].offset = Ticks::new(F + 1)), "not_frame_aligned");
        assert_eq!(with(&|q| q.angles.clear()), "invalid_input");
        assert_eq!(with(&|q| { q.audio_track_ids.pop(); }), "insufficient_ids");
        assert_eq!(with(&|q| q.sequence_id = "seq-main".into()), "duplicate_id");
        assert_eq!(with(&|q| q.audio_track_ids[0] = tid(300)), "folio_rejected", "track ids are unique");
        // Seventeen angles: STU-VID-033 needs at least 16 addressable.
        let mut many = request.clone();
        many.angles = (0..17u32).map(|i| MulticamAngleInput { video: clip(10 + i, media(i), 0, 4, 0), audio: None, offset: Ticks::new(0) }).collect();
        many.sync = MulticamSync::InPoints;
        many.video_track_ids = (0..17).map(|i| tid(300 + i)).collect();
        many.audio_track_ids = (0..17).map(|i| tid(400 + i)).collect();
        let p = ok(|ctx| plan_create_multicam_source(&s, 3, &r, &many, ctx));
        assert_eq!(p.records().multicam_sequence("seq-mc").unwrap().angle_order.len(), 17);
    });
}

fn record_events() -> Vec<RecordEvent> {
    vec![
        RecordEvent { at: Ticks::new(0), angle: 3, cut_ids: None },
        RecordEvent { at: Ticks::new(8 * F), angle: 1, cut_ids: Some(cut_ids(11, (2, 12), "lg-2")) },
        RecordEvent { at: Ticks::new(16 * F), angle: 4, cut_ids: Some(cut_ids(21, (12, 22), "lg-3")) },
    ]
}

#[test]
fn record_pass_is_one_step_with_one_inverse() {
    bounded_case("record_pass", || {
        let (s, r) = fixture();
        let p = ok(|ctx| plan_multicam_record_pass(&s, 3, &r, &cid(1), &record_events(), ctx));
        assert_eq!(p.command_id(), "cmd.multicam.toggle.record");
        assert_eq!((p.base_revision(), p.target_revision()), (3, 4), "a record pass is one step (STU-VID-040)");
        assert_eq!(clips_on(p.snapshot(), 1), vec![(cid(1), 0, 8), (cid(11), 8, 16), (cid(21), 16, 24)]);
        assert_eq!(clips_on(p.snapshot(), 2), vec![(cid(2), 0, 8), (cid(12), 8, 16), (cid(22), 16, 24)]);
        let cut_list: Vec<(u32, u32)> = [1, 11, 21].iter().map(|n| angles(p.records(), *n)).collect();
        assert_eq!(cut_list, vec![(3, 3), (1, 1), (4, 4)]);
        let sound_list: Vec<(u32, u32)> = [2, 12, 22].iter().map(|n| angles(p.records(), *n)).collect();
        assert_eq!(sound_list, cut_list, "audio follows video");
        assert_eq!(p.splits().len(), 4);
        assert!(p.sequence_change().is_some());
        assert_one_step(&p, &s, &r);
        // The edits of the cut list are navigable (next/previous edit).
        let seq = main_seq(p.snapshot());
        let cancel = CancellationToken::default();
        assert_eq!(multicam_adjacent_edit(seq, Ticks::new(0), true, &cancel).unwrap(), Some(Ticks::new(8 * F)));
        assert_eq!(multicam_adjacent_edit(seq, Ticks::new(9 * F), true, &cancel).unwrap(), Some(Ticks::new(10 * F)), "V2's clip end is an edit too");
        assert_eq!(multicam_adjacent_edit(seq, Ticks::new(16 * F), false, &cancel).unwrap(), Some(Ticks::new(10 * F)));
    });
}

#[test]
fn paging_and_navigation_are_bounded() {
    let page = MulticamPage::new(9, 0, 20).unwrap();
    assert_eq!((page.page_count(), page.angles()), (3, 1..=9));
    assert_eq!(page.next().angles(), 10..=18);
    assert_eq!(page.last().angles(), 19..=20);
    assert_eq!(page.last().next(), page.last(), "next page clamps at the last");
    assert_eq!(page.previous(), page.first(), "previous page clamps at the first");
    assert_eq!(page.last().previous().page(), 1);
    assert_eq!(MulticamPage::containing(9, 19, 20).unwrap().page(), 2);
    assert_eq!(MulticamPage::containing(16, 16, 16).unwrap().angles(), 1..=16, "sixteen angles on one page");
    for (size, p, count, code) in [(0, 0, 20, "invalid_input"), (65, 0, 20, "invalid_input"), (9, 0, 0, "invalid_input"), (9, 3, 20, "outside_range"), (9, 0, 1025, "invalid_input")] {
        assert_eq!(MulticamPage::new(size, p, count).unwrap_err().code(), code, "{size} {p} {count}");
    }
    assert_eq!(MulticamPage::containing(9, 21, 20).unwrap_err().code(), "angle_out_of_range");
    assert_eq!(MulticamPage::new(1, 1023, 1024).unwrap().angles(), 1024..=1024);
    assert_eq!(multicam_step_angle(1, 16, false).unwrap(), 1);
    assert_eq!(multicam_step_angle(1, 16, true).unwrap(), 2);
    assert_eq!(multicam_step_angle(16, 16, true).unwrap(), 16);
    assert_eq!(multicam_step_angle(0, 16, true).unwrap_err().code(), "angle_out_of_range");
    assert_eq!(multicam_step_angle(17, 16, false).unwrap_err().code(), "angle_out_of_range");
    let ids: Vec<&str> = [
        MulticamNav::PageFirst,
        MulticamNav::PageLast,
        MulticamNav::PageNext,
        MulticamNav::PagePrevious,
        MulticamNav::NextCamera,
        MulticamNav::PreviousCamera,
        MulticamNav::NextEdit,
        MulticamNav::PreviousEdit,
    ]
    .iter()
    .map(|n| n.command_id())
    .collect();
    assert_eq!(
        ids,
        vec![
            "cmd.multicam.page.first",
            "cmd.multicam.page.last",
            "cmd.multicam.page.next",
            "cmd.multicam.page.previous",
            "cmd.multicam.next.camera",
            "cmd.multicam.previous.camera",
            "cmd.multicam.next.edit",
            "cmd.multicam.prev.edit"
        ]
    );
    assert_eq!(multicam_camera_command(16, true), "cmd.multicam.choose.camera.16");
    assert_eq!(multicam_camera_command(9, false), "cmd.multicam.choosenocut.camera9");
    // Edit navigation at the ends of a sequence.
    let (s, _) = fixture();
    let cancel = CancellationToken::default();
    assert_eq!(multicam_adjacent_edit(main_seq(&s), Ticks::new(24 * F), true, &cancel).unwrap(), None);
    assert_eq!(multicam_adjacent_edit(main_seq(&s), Ticks::new(0), false, &cancel).unwrap(), None);
    println!("NOT_PROVEN: the paged preview grid in the window (GUI lane follow-up); paging is the engine model only");
}

#[test]
fn flatten_renders_the_same_cut_list_as_ordinary_clips() {
    bounded_case("flatten", || {
        let (s, r) = fixture();
        let pass = ok(|ctx| plan_multicam_record_pass(&s, 3, &r, &cid(1), &record_events(), ctx));
        // A third segment on angle 3 crosses angle 3's gap: switch clip 21 (16..24, source 20..28) to 3.
        let gap = ok(|ctx| plan_multicam_switch(pass.snapshot(), 4, pass.records(), &SwitchAngle { clip_id: cid(21), angle: 3, cut_at: None, cut_ids: None }, ctx));
        let (s1, r1) = (gap.snapshot().clone(), gap.records().clone());
        let mc: Vec<String> = [1, 11, 21, 2, 12, 22].iter().map(|n| cid(*n)).collect();
        let fresh: Vec<String> = (500..540).map(cid).collect();
        // Expected picture and sound per frame, read through the multicam records before flatten.
        let sample = |id: u32, frame: u64| ok(|ctx| resolve_active_angle(&s1, &r1, &cid(id), Ticks::new(frame * F), ctx));
        let segment_of = |frame: u64| if frame < 8 { (1, 2) } else if frame < 16 { (11, 12) } else { (21, 22) };
        let p = ok(|ctx| plan_multicam_flatten(&s1, 4, &r1, &mc, &fresh, ctx));
        assert_eq!((p.command_id(), p.target_revision()), ("cmd.clip.multicam.flatten", 5));
        assert!(p.records().clips.is_empty(), "flatten discards the multicam mode");
        let seq = main_seq(p.snapshot());
        let at = |track: usize, frame: u64| {
            seq.tracks[track]
                .clips
                .iter()
                .find(|c| c.timeline_start.value() <= frame * F && frame * F < c.timeline_end().unwrap().value())
                .map(|c| {
                    assert!(matches!(c.source_ref, ClipSource::Media { .. }), "an ordinary media clip");
                    (c.source_ref.clone(), c.source_in.value() + (frame * F - c.timeline_start.value()))
                })
        };
        let source_of = |clip_id: &str| {
            s1.document().sequences[0].tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == clip_id).unwrap().source_ref.clone()
        };
        for frame in 0..24 {
            let (v, a) = segment_of(frame);
            let picture = sample(v, frame).picture.map(|(id, t)| (source_of(&id), t.value()));
            let sound = sample(a, frame).sound.map(|(id, t)| (source_of(&id), t.value()));
            assert_eq!(at(0, frame), picture, "picture at frame {frame}");
            assert_eq!(at(2, frame), sound, "sound at frame {frame}");
        }
        // Angle 3's gap (source 20..30) stays a gap: frames 16..24 read source 20..28.
        assert_eq!(at(0, 17), None);
        // Angle 3 and 4 have no sound: those segments are silent after flatten.
        assert_eq!(at(2, 20), None);
        assert_eq!(at(2, 2), None, "segment 1 is angle 3: silent");
        assert_eq!(at(2, 9).map(|(src, _)| src), Some(media(90)), "segment 2 is angle 1: track 200");
        assert!(seq.tracks[0].clips.iter().all(|c| c.link_group.is_none()));
        assert_one_step(&p, &s1, &r1);

        // Refusals: an unknown or ordinary clip, duplicates, missing or taken ids.
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &[cid(3)], &fresh, ctx)), "not_multicam");
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &[cid(1), cid(1)], &fresh, ctx)), "duplicate_id");
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &mc, &fresh[..2], ctx)), "insufficient_ids");
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &mc, &[cid(3), cid(600)], ctx)), "duplicate_id");
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &[], &fresh, ctx)), "no_change");
        let bomb = vec![cid(700); MAX_FLATTEN_PIECES + 1];
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s1, 4, &r1, &mc, &bomb, ctx)), "limit_exceeded");
        println!("NOT_PROVEN: program-monitor playback and composite render of the active angle (no nested-sequence resolution in pulse playback or composite); flatten gives the renderable result");
    });
}

#[test]
fn hostile_record_passes_are_refused_or_bounded() {
    bounded_case("hostile_record_pass", || {
        let (s, r) = fixture();
        let pass = |events: &[RecordEvent]| refused(|ctx| plan_multicam_record_pass(&s, 3, &r, &cid(1), events, ctx));
        let e = |at: u64, angle: u32, ids: Option<MulticamCutIds>| RecordEvent { at: Ticks::new(at), angle, cut_ids: ids };
        let flood: Vec<RecordEvent> = (0..=MAX_RECORD_PASS_EVENTS as u64).map(|i| e(i * F, 1, None)).collect();
        assert_eq!(pass(&flood), "too_many_events");
        assert_eq!(pass(&[]), "no_change");
        assert_eq!(pass(&[e(24 * F, 1, Some(cut_ids(11, (2, 12), "lg-2")))]), "event_outside_clip");
        assert_eq!(pass(&[e(u64::MAX, 1, None)]), "event_outside_clip");
        assert_eq!(
            pass(&[e(8 * F, 1, Some(cut_ids(11, (2, 12), "lg-2"))), e(4 * F, 3, Some(cut_ids(21, (12, 22), "lg-3")))]),
            "events_unordered"
        );
        assert_eq!(
            pass(&[e(8 * F, 1, Some(cut_ids(11, (2, 12), "lg-2"))), e(8 * F, 3, Some(cut_ids(21, (12, 22), "lg-3")))]),
            "events_unordered"
        );
        assert_eq!(pass(&[e(0, 0, None)]), "angle_out_of_range");
        assert_eq!(pass(&[e(0, u32::MAX, None)]), "angle_out_of_range");
        assert_eq!(pass(&[e(8 * F, 1, None)]), "insufficient_ids");
        assert_eq!(pass(&[e(0, 1, Some(cut_ids(11, (2, 12), "lg-2")))]), "invalid_input");
        assert_eq!(pass(&[e(8 * F, 1, Some(cut_ids(3, (2, 12), "lg-2")))]), "duplicate_id");
        assert_eq!(refused(|ctx| plan_multicam_record_pass(&s, 3, &r, &cid(3), &record_events(), ctx)), "not_multicam");
        assert_eq!(refused(|ctx| plan_multicam_record_pass(&s, 4, &r, &cid(1), &record_events(), ctx)), "stale_revision");
        // Records that do not validate against the document are refused before any work.
        let mut foreign = r.clone();
        foreign.document_id = ident("SDOC", 9);
        assert_eq!(refused(|ctx| plan_multicam_record_pass(&s, 3, &foreign, &cid(1), &record_events(), ctx)), "multicam_records_rejected");
        // A canceled token stops the pass.
        let cancel = CancellationToken::default();
        cancel.cancel();
        let canceled = with_ctx(&cancel, |ctx| plan_multicam_record_pass(&s, 3, &r, &cid(1), &record_events(), ctx));
        assert_eq!(canceled.unwrap_err(), MulticamError::Pulse(PulseError::Canceled));
        // The longest legal pass on a long clip stays bounded: one cut per frame for 256 frames.
        let mut long = s.document().clone();
        let main = long.sequences.iter_mut().find(|q| q.sequence_id == "seq-main").unwrap();
        main.tracks[0].clips[0].link_group = None;
        main.tracks[2].clips.clear();
        main.duration = main.content_end().unwrap();
        let long = validate_document(long, Budget::default(), &CancellationToken::default(), &Media).unwrap();
        let mut lr = r.clone();
        lr.clips.retain(|m| m.clip_id == cid(1));
        let events: Vec<RecordEvent> = (0..24u64)
            .map(|i| e(i * F, 1 + (i % 4) as u32, (i > 0).then(|| MulticamCutIds { new_right_clip_id: cid(1000 + i as u32), linked: None })))
            .collect();
        let p = ok(|ctx| plan_multicam_record_pass(&long, 3, &lr, &cid(1), &events, ctx));
        assert_eq!(p.target_revision(), 4);
        assert_eq!(main_seq(p.snapshot()).tracks[0].clips.len(), 24);
    });
}
