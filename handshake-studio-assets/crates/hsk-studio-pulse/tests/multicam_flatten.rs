//! Lane SMB-PR-MULTICAM-2 (STU-VID-033 flatten, STU-VID-031 rational speed, STU-VID-034 clip
//! markers, STU-VID-090 named refusals): flatten and resolve at non-unit forward speed and flatten of
//! clips with markers. Every flatten is proven ONE step by planning its inverse and comparing the
//! exact prior sequences and records; every flattened frame is compared with the multicam reading
//! of the same frame through `resolve_active_angle`.
//! NOT_PROVEN: Premiere's flatten of speed-changed angles and its marker placement after flatten
//! (no Premiere oracle); program-monitor playback / composite render of the active angle.
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
/// Clip `n` of `source`: source frames `[si, si + frames)` at timeline frame `start`, unit speed.
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
/// Sets a speed and derives the duration Folio requires.
fn set_speed(c: &mut StudioClip, numerator: i64, denominator: u64) {
    c.speed = Speed { numerator, denominator };
    c.duration = c.derived_duration().expect("fixture speed divides the window");
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
fn document(sequences: Vec<StudioSequence>, budget: Budget) -> Snapshot {
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
    validate_document(document, budget, &CancellationToken::default(), &Media).expect("fixture is a valid Folio document")
}

/// `seq-mc` (as lane MULTICAM-1's fixture): V idx0 cam 100 src 0..48 @0; V idx1 cam 101 src 0..46 @2;
/// V idx2 102 src 0..20 @0 and 103 src 100..118 @30 (a gap 20..30); V idx3 104 src 10..58 @0;
/// A idx0 200 src 0..48 @0; A idx1 201 src 0..48 @0.
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
fn records_for(snapshot: &Snapshot, clips: &[(u32, u32)]) -> StudioMulticamRecords {
    let source = snapshot.document().sequences.iter().find(|q| q.sequence_id == "seq-mc").unwrap();
    let mut records = StudioMulticamRecords::new(ident("SDOC", 1));
    records.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: "seq-mc".into(),
        selection_order: MulticamSelectionOrder::SourceOrder,
        angle_order: multicam_angle_order(source, MulticamSelectionOrder::SourceOrder),
    });
    for (n, angle) in clips {
        records.clips.push(StudioMulticamClip {
            clip_id: cid(*n),
            active_angle: *angle,
            audio_angle: *angle,
            audio_follows_video: true,
            follows_nest_setting: false,
            enabled: true,
        });
    }
    validate_multicam_records(&records, snapshot.document(), Budget::default(), &CancellationToken::default()).unwrap();
    records
}
/// `seq-main`: V1 multicam clip 1 (window 4..28 @0, then `shape`) linked with A1 multicam clip 2
/// (same window and shape); both on angle 2.
fn fixture_with(shape: impl Fn(&mut StudioClip)) -> (Snapshot, StudioMulticamRecords) {
    let mut v = clip(1, nest(), 4, 24, 0);
    let mut a = clip(2, nest(), 4, 24, 0);
    for c in [&mut v, &mut a] {
        c.link_group = Some("lg-1".into());
        shape(c);
    }
    let main = sequence("seq-main", vec![track(1, TrackKind::Video, 0, vec![v]), track(2, TrackKind::Audio, 0, vec![a])]);
    let snapshot = document(vec![source_sequence(), main], Budget::default());
    let records = records_for(&snapshot, &[(1, 2), (2, 2)]);
    (snapshot, records)
}
/// A two-angle source whose angle 1 clip 110 is `angle_clip`, angle 2 clip 111 media(1) 0..24 @0;
/// `seq-main` V1 holds multicam clip 1 (`main_clip`) on angle 1.
fn custom_fixture(angle_clip: StudioClip, main_clip: StudioClip) -> (Snapshot, StudioMulticamRecords) {
    let source = sequence(
        "seq-mc",
        vec![track(100, TrackKind::Video, 0, vec![angle_clip]), track(101, TrackKind::Video, 1, vec![clip(111, media(1), 0, 24, 0)])],
    );
    let main = sequence("seq-main", vec![track(1, TrackKind::Video, 0, vec![main_clip])]);
    let snapshot = document(vec![source, main], Budget::default());
    let records = records_for(&snapshot, &[(1, 1)]);
    (snapshot, records)
}

fn with_ctx<T>(cancel: &CancellationToken, budget: Budget, f: impl FnOnce(&EditContext) -> T) -> T {
    f(&EditContext { budget, resolver: &Media, extents: &UnknownExtents, cancel })
}
fn ok<T>(f: impl FnOnce(&EditContext) -> MulticamResult<T>) -> T {
    let cancel = CancellationToken::default();
    with_ctx(&cancel, Budget::default(), f).expect("planner accepts")
}
fn refused<T: std::fmt::Debug>(f: impl FnOnce(&EditContext) -> MulticamResult<T>) -> String {
    let cancel = CancellationToken::default();
    with_ctx(&cancel, Budget::default(), f).expect_err("planner refuses").code().to_owned()
}
fn main_seq(s: &Snapshot) -> &StudioSequence {
    s.document().sequences.iter().find(|q| q.sequence_id == "seq-main").unwrap()
}
fn fresh() -> Vec<String> {
    (500..540).map(cid).collect()
}
/// Plans the revert of `p` and checks it restores exactly `(snapshot, records)`.
fn assert_one_step(p: &MulticamProposal, snapshot: &Snapshot, records: &StudioMulticamRecords) {
    let undo = ok(|ctx| plan_multicam_revert(p.snapshot(), p.target_revision(), p.records(), p.inverse(), ctx));
    assert_eq!(undo.snapshot().document().sequences, snapshot.document().sequences, "undo restores the sequences");
    assert_eq!(undo.records(), records, "undo restores the records");
}
/// `(source_ref, source_in, source_out, timeline_start, duration, speed)` in ticks.
fn shape(c: &StudioClip) -> (ClipSource, u64, u64, u64, u64, Speed) {
    (c.source_ref.clone(), c.source_in.value(), c.source_out.value(), c.timeline_start.value(), c.duration.value(), c.speed)
}
/// The media and media tick a flattened track plays at timeline tick `t` (the piece's own speed).
fn play(seq: &StudioSequence, track: usize, t: u64) -> Option<(ClipSource, u64)> {
    seq.tracks[track].clips.iter().find(|c| c.timeline_start.value() <= t && t < c.timeline_end().unwrap().value()).map(|c| {
        let offset = u128::from(t - c.timeline_start.value()) * u128::from(c.speed.numerator.unsigned_abs()) / u128::from(c.speed.denominator);
        (c.source_ref.clone(), c.source_in.value() + u64::try_from(offset).unwrap())
    })
}
/// The media and media tick the multicam clip shows (`picture`) or plays at timeline tick `t`.
fn reading(s: &Snapshot, r: &StudioMulticamRecords, clip_id: u32, t: u64, picture: bool) -> Option<(ClipSource, u64)> {
    let resolved = ok(|ctx| resolve_active_angle(s, r, &cid(clip_id), Ticks::new(t), ctx));
    let hit = if picture { resolved.picture } else { resolved.sound };
    hit.map(|(id, tick)| {
        let c = s.document().sequences.iter().flat_map(|q| q.tracks.iter()).flat_map(|k| k.clips.iter()).find(|c| c.clip_id == id).unwrap();
        (c.source_ref.clone(), tick.value())
    })
}

#[test]
fn flatten_maps_a_double_speed_clip_exactly() {
    bounded_case("flatten_double_speed", || {
        let (s, r) = fixture_with(|c| set_speed(c, 2, 1));
        assert_eq!(main_seq(&s).tracks[0].clips[0].duration.value(), 12 * F, "24 source frames at 2x last 12 frames");
        let p = ok(|ctx| plan_multicam_flatten(&s, 3, &r, &[cid(1), cid(2)], &fresh(), ctx));
        let seq = main_seq(p.snapshot());
        let two = Speed { numerator: 2, denominator: 1 };
        // Angle 2: camera 101 (src 0.. @2) read from source-sequence tick 4: media 2..26, 12 frames at 2x.
        assert_eq!(seq.tracks[0].clips.iter().map(shape).collect::<Vec<_>>(), vec![(media(1), 2 * F, 26 * F, 0, 12 * F, two)]);
        assert_eq!(seq.tracks[1].clips.iter().map(shape).collect::<Vec<_>>(), vec![(media(91), 4 * F, 28 * F, 0, 12 * F, two)]);
        for frame in 0..12 {
            assert_eq!(play(seq, 0, frame * F), reading(&s, &r, 1, frame * F, true), "picture at frame {frame}");
            assert_eq!(play(seq, 1, frame * F), reading(&s, &r, 2, frame * F, false), "sound at frame {frame}");
        }
        assert_one_step(&p, &s, &r);

        // Angle 3 has a gap at source 20..30: the flattened 2x clip plays 0..8 and leaves 8..12 empty.
        let mut r3 = r.clone();
        for n in [1, 2] {
            let m = r3.clip_mut(&cid(n)).unwrap();
            (m.active_angle, m.audio_angle) = (3, 3);
        }
        let p3 = ok(|ctx| plan_multicam_flatten(&s, 3, &r3, &[cid(1), cid(2)], &fresh(), ctx));
        let seq3 = main_seq(p3.snapshot());
        assert_eq!(seq3.tracks[0].clips.iter().map(shape).collect::<Vec<_>>(), vec![(media(2), 4 * F, 20 * F, 0, 8 * F, two)]);
        assert!(seq3.tracks[1].clips.is_empty(), "angle 3 has no sound: silent after flatten");
        for frame in 0..12 {
            assert_eq!(play(seq3, 0, frame * F), reading(&s, &r3, 1, frame * F, true), "angle 3 picture at frame {frame}");
        }
        assert_one_step(&p3, &s, &r3);
    });
}

#[test]
fn angle_clip_speed_multiplies_into_the_piece() {
    bounded_case("angle_clip_speed", || {
        // Angle 1 is slow motion: media 0..10 at 1/2 fills source-sequence frames 0..20.
        let mut slow = clip(110, media(0), 0, 10, 0);
        set_speed(&mut slow, 1, 2);
        let (s, r) = custom_fixture(slow.clone(), clip(1, nest(), 4, 12, 0));
        let p = ok(|ctx| plan_multicam_flatten(&s, 3, &r, &[cid(1)], &fresh(), ctx));
        let half = Speed { numerator: 1, denominator: 2 };
        let seq = main_seq(p.snapshot());
        assert_eq!(seq.tracks[0].clips.iter().map(shape).collect::<Vec<_>>(), vec![(media(0), 2 * F, 8 * F, 0, 12 * F, half)]);
        for frame in 0..12 {
            assert_eq!(play(seq, 0, frame * F), reading(&s, &r, 1, frame * F, true), "frame {frame}");
        }
        assert_one_step(&p, &s, &r);
        // The same window played at 2x: speeds multiply to 1 (2 * 1/2), the piece lasts 6 frames.
        let mut fast = clip(1, nest(), 4, 12, 0);
        set_speed(&mut fast, 2, 1);
        let (s2, r2) = custom_fixture(slow, fast);
        let p2 = ok(|ctx| plan_multicam_flatten(&s2, 3, &r2, &[cid(1)], &fresh(), ctx));
        let seq2 = main_seq(p2.snapshot());
        assert_eq!(seq2.tracks[0].clips.iter().map(shape).collect::<Vec<_>>(), vec![(media(0), 2 * F, 8 * F, 0, 6 * F, Speed::UNIT)]);
        for frame in 0..6 {
            assert_eq!(play(seq2, 0, frame * F), reading(&s2, &r2, 1, frame * F, true), "2x frame {frame}");
        }
        assert_one_step(&p2, &s2, &r2);
    });
}

#[test]
fn inexact_speed_products_are_refused_named() {
    bounded_case("inexact_speed", || {
        // 11x over 22 source frames lasts 2 frames, but the angle edges at source frames 2 and 20
        // fall between timeline ticks (no factor 11 in the tick rate).
        let (s, r) = fixture_with(|c| {
            c.source_in = Ticks::new(0);
            c.source_out = Ticks::new(22 * F);
            set_speed(c, 11, 1);
        });
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s, 3, &r, &[cid(1)], &fresh(), ctx)), "multicam_speed_inexact");
        let mut r3 = r.clone();
        let m = r3.clip_mut(&cid(1)).unwrap();
        (m.active_angle, m.audio_angle) = (3, 3);
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s, 3, &r3, &[cid(1)], &fresh(), ctx)), "multicam_speed_inexact");
        // An angle clip at 1/11 whose media position for source frame 4 is not a whole tick.
        let mut slow = clip(110, media(0), 0, 2, 0);
        set_speed(&mut slow, 1, 11);
        let (s2, r2) = custom_fixture(slow, clip(1, nest(), 4, 12, 0));
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s2, 3, &r2, &[cid(1)], &fresh(), ctx)), "multicam_speed_inexact");
        // Resolve floors instead of refusing (a frame shows the media tick at or before it).
        let at = ok(|ctx| resolve_active_angle(&s2, &r2, &cid(1), Ticks::new(0), ctx));
        assert_eq!(at.picture, Some((cid(110), Ticks::new(4 * F / 11))));
    });
}

#[test]
fn reverse_speed_is_refused_named() {
    bounded_case("reverse_speed", || {
        let (s, r) = fixture_with(|c| set_speed(c, -1, 1));
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s, 3, &r, &[cid(1), cid(2)], &fresh(), ctx)), "multicam_reverse_unsupported");
        assert_eq!(refused(|ctx| resolve_active_angle(&s, &r, &cid(1), Ticks::new(F), ctx)), "multicam_reverse_unsupported");
        let mut back = clip(110, media(0), 0, 24, 0);
        set_speed(&mut back, -1, 1);
        let (s2, r2) = custom_fixture(back, clip(1, nest(), 4, 12, 0));
        assert_eq!(refused(|ctx| plan_multicam_flatten(&s2, 3, &r2, &[cid(1)], &fresh(), ctx)), "multicam_reverse_unsupported");
        assert_eq!(refused(|ctx| resolve_active_angle(&s2, &r2, &cid(1), Ticks::new(F), ctx)), "multicam_reverse_unsupported");
        // The refusal is per angle read: a reverse clip on another angle does not block angle 2.
        let mut r_two = r2.clone();
        let m = r_two.clip_mut(&cid(1)).unwrap();
        (m.active_angle, m.audio_angle) = (2, 2);
        ok(|ctx| plan_multicam_flatten(&s2, 3, &r_two, &[cid(1)], &fresh(), ctx));
        println!("NOT_PROVEN: Premiere reverse-speed multicam behaviour (no Premiere oracle); Studio refuses it by declared reading");
    });
}

#[test]
fn resolve_active_angle_at_non_unit_speed() {
    bounded_case("resolve_non_unit_speed", || {
        let (s, r) = fixture_with(|c| set_speed(c, 2, 1));
        let at = ok(|ctx| resolve_active_angle(&s, &r, &cid(1), Ticks::new(3 * F), ctx));
        assert_eq!(at.source_tick, Ticks::new(10 * F), "timeline frame 3 at 2x from source frame 4 reads source frame 10");
        assert_eq!(at.picture, Some((cid(101), Ticks::new(8 * F))), "camera 101 starts at source frame 2");
        let sound = ok(|ctx| resolve_active_angle(&s, &r, &cid(2), Ticks::new(3 * F), ctx));
        assert_eq!(sound.sound, Some((cid(201), Ticks::new(10 * F))));
        assert_eq!(refused(|ctx| resolve_active_angle(&s, &r, &cid(1), Ticks::new(12 * F), ctx)), "outside_range", "the 2x clip ends at frame 12");
        // Half speed: a 6-frame window lasts 12 frames; a fractional source position is floored.
        let (h, hr) = fixture_with(|c| {
            c.source_out = Ticks::new(10 * F);
            set_speed(c, 1, 2);
        });
        let half = ok(|ctx| resolve_active_angle(&h, &hr, &cid(1), Ticks::new(F + 1), ctx));
        assert_eq!(half.source_tick, Ticks::new(4 * F + F / 2));
        assert_eq!(half.picture, Some((cid(101), Ticks::new(2 * F + F / 2))));
        assert_eq!(ok(|ctx| resolve_active_angle(&h, &hr, &cid(1), Ticks::new(11 * F), ctx)).source_tick, Ticks::new(4 * F + 11 * F / 2));
    });
}
