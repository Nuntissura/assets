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
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn cid(n: u32) -> String {
    id("SCLP", n)
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

/// Every source is `.0` ticks long (or unknown).
struct Extent(Option<u64>);
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        self.0.map(Ticks::new)
    }
}

fn clip(n: u32, source_in: u64, source_out: u64, start: u64, duration: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "manifest-a".into(),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "abc123".into(),
            },
            asset_id: None,
        },
        source_in: Ticks::new(source_in),
        source_out: Ticks::new(source_out),
        timeline_start: Ticks::new(start),
        duration: Ticks::new(duration),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: hsk_studio_folio::ClipTransitions::default(),
    }
}
fn linked(mut c: StudioClip) -> StudioClip {
    c.link_group = Some("lg".into());
    c
}
fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id("STRK", n),
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
fn snapshot(tracks: Vec<StudioTrack>) -> Snapshot {
    let end = tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.timeline_start.value() + c.duration.value())
        .max()
        .unwrap_or(0);
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
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
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(end),
            tracks,
            markers: Vec::new(),
            work_area: None,
        }],
    };
    validate_document(
        document,
        Budget::default(),
        &CancellationToken::default(),
        &Media,
    )
    .expect("fixture is a valid Folio document")
}

fn plan(snapshot: &Snapshot, edit: &TimelineEdit, extent: Option<u64>) -> Result<EditProposal> {
    let cancel = CancellationToken::default();
    let extents = Extent(extent);
    plan_timeline_edit(
        snapshot,
        "seq-main",
        snapshot.document().revision,
        edit,
        &EditContext {
            budget: Budget::default(),
            resolver: &Media,
            extents: &extents,
            cancel: &cancel,
        },
    )
}
fn after(p: &EditProposal, n: u32) -> StudioClip {
    p.snapshot().document().sequences[0]
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.clip_id == cid(n))
        .expect("clip present")
        .clone()
}
fn window(c: &StudioClip) -> [u64; 4] {
    [
        c.source_in.value(),
        c.source_out.value(),
        c.timeline_start.value(),
        c.duration.value(),
    ]
}
fn trim_out(n: u32, frames: u64, ripple: bool) -> TimelineEdit {
    TimelineEdit::TrimOut {
        clip_id: cid(n),
        new_duration: Ticks::new(frames * F),
        ripple,
    }
}
fn trim_in(n: u32, head_frames: u64, ripple: bool) -> TimelineEdit {
    TimelineEdit::TrimIn {
        clip_id: cid(n),
        new_head: Ticks::new(head_frames * F),
        ripple,
    }
}
fn frames(n: i128) -> TickDelta {
    TickDelta::new(n * i128::from(F)).unwrap()
}
fn err(r: Result<EditProposal>) -> PulseError {
    r.expect_err("edit must be refused")
}

/// V1 [A linked 0..3F, B 3F..6F], A1 [AA linked 0..3F, BB 3F..6F], A2 (not sync-locked) [CC 3F..6F].
fn linked_fixture(mutate: impl FnOnce(&mut Vec<StudioTrack>)) -> Snapshot {
    let mut tracks = vec![
        track(
            1,
            TrackKind::Video,
            0,
            vec![
                linked(clip(1, 0, 3 * F, 0, 3 * F)),
                clip(2, 0, 3 * F, 3 * F, 3 * F),
            ],
        ),
        track(
            2,
            TrackKind::Audio,
            0,
            vec![
                linked(clip(3, 0, 3 * F, 0, 3 * F)),
                clip(4, 0, 3 * F, 3 * F, 3 * F),
            ],
        ),
        track(
            3,
            TrackKind::Audio,
            1,
            vec![clip(5, 0, 3 * F, 3 * F, 3 * F)],
        ),
    ];
    tracks[2].sync_locked = false;
    mutate(&mut tracks);
    snapshot(tracks)
}

#[test]
fn ripple_trim_out_links_and_sync_lock() {
    bounded_case("ripple_trim_out_links_and_sync_lock", || {
        let snap = linked_fixture(|_| {});
        let before = snap.document().clone();
        let p = plan(&snap, &trim_out(1, 2, true), Some(10 * F)).unwrap();
        // MT-35540 oracle: 2 * 10_160_640_000 = 20_321_280_000, linked audio trims with it.
        for n in [1, 3] {
            assert_eq!(
                window(&after(&p, n)),
                [0, 20_321_280_000, 0, 20_321_280_000],
                "clip {n}"
            );
        }
        // Later content on the edited tracks ripples; the non-sync-locked track stays put.
        for n in [2, 4] {
            assert_eq!(
                after(&p, n).timeline_start.value(),
                20_321_280_000,
                "clip {n}"
            );
            assert_eq!(after(&p, n).duration.value(), 30_481_920_000);
        }
        assert_eq!(
            window(&after(&p, 5)),
            [0, 30_481_920_000, 30_481_920_000, 30_481_920_000]
        );
        assert_eq!(
            p.snapshot().document().sequences[0].duration.value(),
            60_963_840_000
        );
        assert_eq!(
            (
                p.base_revision(),
                p.target_revision(),
                p.snapshot().document().revision
            ),
            (3, 4, 4)
        );
        assert_eq!(p.changes().len(), 4);
        assert!(p.change_for(&cid(5)).is_none());
        for n in [1, 2, 3, 4] {
            assert!(p.write_footprint().contains(&cid(n)));
        }
        assert!(
            !p.write_footprint().contains(&"seq-main".to_owned()),
            "sequence end unchanged"
        );
        assert!(p.read_footprint().contains(&"seq-main".to_owned()));
        assert_eq!(*snap.document(), before, "input snapshot unchanged");
        // Plain trim: both linked clips shorten, nothing ripples, a gap opens.
        let plain = plan(&snap, &trim_out(1, 2, false), None).unwrap();
        assert_eq!(after(&plain, 3).duration.value(), 20_321_280_000);
        assert_eq!(after(&plain, 2).timeline_start.value(), 30_481_920_000);
        // Extending ripples later content later: 2F -> 4F moves B/BB to 4F only with handle info.
        assert_eq!(
            err(plan(&snap, &trim_out(1, 4, true), None)),
            PulseError::SourceExtentUnknown
        );
        let longer = plan(&snap, &trim_out(1, 4, true), Some(10 * F)).unwrap();
        assert_eq!(
            window(&after(&longer, 1)),
            [0, 40_642_560_000, 0, 40_642_560_000]
        );
        assert_eq!(after(&longer, 4).timeline_start.value(), 40_642_560_000);
        // A second sync-locked track also shifts; locked or colliding ones refuse the ripple.
        let synced = linked_fixture(|t| t[2].sync_locked = true);
        let moved = plan(&synced, &trim_out(1, 2, true), None).unwrap();
        assert_eq!(after(&moved, 5).timeline_start.value(), 20_321_280_000);
        assert!(moved.write_footprint().contains(&cid(5)));
        let locked = linked_fixture(|t| {
            t[2].sync_locked = true;
            t[2].locked = true;
        });
        assert_eq!(
            err(plan(&locked, &trim_out(1, 2, true), None)),
            PulseError::SyncLockBlocked
        );
        let collide = linked_fixture(|t| {
            t[2].sync_locked = true;
            t[2].clips.insert(0, clip(6, 0, 2 * F, F, 2 * F));
        });
        assert_eq!(
            err(plan(&collide, &trim_out(1, 2, true), None)),
            PulseError::SyncLockBlocked
        );
        let edit_locked = linked_fixture(|t| t[1].locked = true);
        assert_eq!(
            err(plan(&edit_locked, &trim_out(1, 2, true), None)),
            PulseError::TrackLocked
        );
        // Two members of one link group on the same track cannot trim together.
        let same_track =
            linked_fixture(|t| t[0].clips[1] = linked(clip(2, 0, 3 * F, 3 * F, 3 * F)));
        assert_eq!(
            err(plan(&same_track, &trim_out(1, 2, true), None)),
            PulseError::LinkedEditUnsupported
        );
    });
}

#[test]
fn trims_bounds_reverse_speed_and_grid() {
    bounded_case("trims_bounds_reverse_speed_and_grid", || {
        // Y 2F..5F (source 1F..4F), Z 5F..8F, both video; source length 5F.
        let snap = snapshot(vec![track(
            1,
            TrackKind::Video,
            0,
            vec![
                clip(1, F, 4 * F, 2 * F, 3 * F),
                clip(2, 0, 3 * F, 5 * F, 3 * F),
            ],
        )]);
        let five = Some(5 * F);
        let head = plan(&snap, &trim_in(1, 3, false), five).unwrap();
        assert_eq!(
            window(&after(&head, 1)),
            [
                20_321_280_000,
                40_642_560_000,
                30_481_920_000,
                20_321_280_000
            ]
        );
        assert_eq!(
            after(&head, 2).timeline_start.value(),
            5 * F,
            "plain head trim moves nothing else"
        );
        let ripple_head = plan(&snap, &trim_in(1, 3, true), five).unwrap();
        assert_eq!(
            window(&after(&ripple_head, 1)),
            [2 * F, 4 * F, 2 * F, 2 * F]
        );
        assert_eq!(
            after(&ripple_head, 2).timeline_start.value(),
            40_642_560_000
        );
        let earlier = plan(&snap, &trim_in(1, 1, false), five).unwrap();
        assert_eq!(window(&after(&earlier, 1)), [0, 4 * F, F, 4 * F]);
        assert_eq!(
            err(plan(&snap, &trim_in(1, 0, false), five)),
            PulseError::PastSourceStart
        );
        // Tail: source end is a hard bound; abutting neighbour blocks a plain extension.
        assert_eq!(
            err(plan(&snap, &trim_out(1, 4, false), five)),
            PulseError::Overlap
        );
        let ripple_out = plan(&snap, &trim_out(1, 4, true), five).unwrap();
        assert_eq!(window(&after(&ripple_out, 1)), [F, 5 * F, 2 * F, 4 * F]);
        assert_eq!(after(&ripple_out, 2).timeline_start.value(), 6 * F);
        assert_eq!(
            err(plan(&snap, &trim_out(1, 5, true), five)),
            PulseError::PastSourceEnd
        );
        assert_eq!(
            err(plan(&snap, &trim_out(1, 0, false), five)),
            PulseError::ZeroDuration
        );
        assert_eq!(
            err(plan(&snap, &trim_out(1, 3, false), five)),
            PulseError::NoChange
        );
        // Video edits move by whole frames; audio is free.
        let off_grid = TimelineEdit::TrimOut {
            clip_id: cid(1),
            new_duration: Ticks::new(2 * F + 1),
            ripple: false,
        };
        assert_eq!(
            err(plan(&snap, &off_grid, five)),
            PulseError::NotFrameAligned
        );
        let mut reversed = clip(3, 0, 4 * F, 0, 4 * F);
        reversed.speed = Speed {
            numerator: -1,
            denominator: 1,
        };
        let mut double = clip(4, 0, 4 * F, 5 * F, 2 * F);
        double.speed = Speed {
            numerator: 2,
            denominator: 1,
        };
        let audio = snapshot(vec![track(1, TrackKind::Audio, 0, vec![reversed, double])]);
        // Reversed: the timeline tail reads from source_in, so trimming the tail raises source_in.
        let r = plan(&audio, &trim_out(3, 3, false), None).unwrap();
        assert_eq!(window(&after(&r, 3)), [F, 4 * F, 0, 3 * F]);
        let free = TimelineEdit::TrimOut {
            clip_id: cid(3),
            new_duration: Ticks::new(3 * F + 1),
            ripple: false,
        };
        let r = plan(&audio, &free, None).unwrap();
        assert_eq!(window(&after(&r, 3)), [F - 1, 4 * F, 0, 3 * F + 1]);
        // 2x speed: one timeline frame consumes two source frames.
        let d = plan(&audio, &trim_out(4, 1, false), None).unwrap();
        assert_eq!(window(&after(&d, 4)), [0, 2 * F, 5 * F, F]);
        assert_eq!(d.snapshot().document().sequences[0].duration.value(), 6 * F);
    });
}

#[test]
fn razor_roll_slip_slide_exact_ticks() {
    bounded_case("razor_roll_slip_slide_exact_ticks", || {
        // V1 P 0..3F (src 0..3F), Q 3F..6F (src 2F..5F), R 6F..9F (src 1F..4F); A1 reversed X 0..4F.
        let mut reversed = clip(5, 0, 4 * F, 0, 4 * F);
        reversed.speed = Speed {
            numerator: -1,
            denominator: 1,
        };
        let snap = snapshot(vec![
            track(
                1,
                TrackKind::Video,
                0,
                vec![
                    clip(1, 0, 3 * F, 0, 3 * F),
                    clip(2, 2 * F, 5 * F, 3 * F, 3 * F),
                    clip(3, F, 4 * F, 6 * F, 3 * F),
                ],
            ),
            track(2, TrackKind::Audio, 0, vec![reversed]),
        ]);
        let ten = Some(10 * F);
        let razor = |clip: u32, frame: u64, new: u32| TimelineEdit::Razor {
            clip_id: cid(clip),
            at: Ticks::new(frame * F),
            new_right_clip_id: cid(new),
            linked: None,
        };
        let p = plan(&snap, &razor(2, 4, 9), ten).unwrap();
        assert_eq!(window(&after(&p, 2)), [2 * F, 3 * F, 3 * F, F]);
        assert_eq!(
            window(&after(&p, 9)),
            [3 * F, 5 * F, 4 * F, 2 * F],
            "right half continues the left source out"
        );
        assert!(matches!(
            p.change_for(&cid(9)),
            Some(ClipChange::Insert { .. })
        ));
        assert!(p.write_footprint().contains(&id("STRK", 1)));
        assert_eq!(p.snapshot().document().sequences[0].duration.value(), 9 * F);
        let p = plan(&snap, &razor(5, 1, 8), ten).unwrap();
        assert_eq!(
            window(&after(&p, 5)),
            [3 * F, 4 * F, 0, F],
            "reversed plays source_out first"
        );
        assert_eq!(window(&after(&p, 8)), [0, 3 * F, F, 3 * F]);
        assert_eq!(
            err(plan(&snap, &razor(2, 3, 9), ten)),
            PulseError::OutsideRange
        );
        assert_eq!(
            err(plan(&snap, &razor(2, 4, 1), ten)),
            PulseError::DuplicateId
        );
        let off = TimelineEdit::Razor {
            clip_id: cid(2),
            at: Ticks::new(4 * F + 1),
            new_right_clip_id: cid(9),
            linked: None,
        };
        assert_eq!(err(plan(&snap, &off, ten)), PulseError::NotFrameAligned);
        // Roll: P gains what Q loses; Q's source head moves with the edit.
        let roll = |left: u32, d: i128| TimelineEdit::Roll {
            left_clip_id: cid(left),
            delta: frames(d),
        };
        let p = plan(&snap, &roll(1, 1), ten).unwrap();
        assert_eq!(window(&after(&p, 1)), [0, 4 * F, 0, 4 * F]);
        assert_eq!(window(&after(&p, 2)), [3 * F, 5 * F, 4 * F, 2 * F]);
        assert_eq!(
            window(&after(&plan(&snap, &roll(1, -1), ten).unwrap(), 2)),
            [F, 5 * F, 2 * F, 4 * F]
        );
        assert_eq!(err(plan(&snap, &roll(1, 3), ten)), PulseError::ZeroDuration);
        assert_eq!(err(plan(&snap, &roll(3, 1), ten)), PulseError::OutsideRange);
        // R's source head would pass source 0 (R starts at source F).
        assert_eq!(
            err(plan(&snap, &roll(2, -2), ten)),
            PulseError::PastSourceStart
        );
        // Slip: only the source window moves.
        let slip = |d: i128| TimelineEdit::Slip {
            clip_id: cid(2),
            delta: frames(d),
        };
        let p = plan(&snap, &slip(1), ten).unwrap();
        assert_eq!(window(&after(&p, 2)), [3 * F, 6 * F, 3 * F, 3 * F]);
        assert!(p.write_footprint() == [cid(2)]);
        assert_eq!(
            err(plan(&snap, &slip(-3), ten)),
            PulseError::PastSourceStart
        );
        assert_eq!(err(plan(&snap, &slip(6), ten)), PulseError::PastSourceEnd);
        assert_eq!(err(plan(&snap, &slip(0), ten)), PulseError::NoChange);
        // Slide: Q keeps its source window, P and R absorb the move.
        let slide = |c: u32, d: i128| TimelineEdit::Slide {
            clip_id: cid(c),
            delta: frames(d),
        };
        let p = plan(&snap, &slide(2, 1), ten).unwrap();
        assert_eq!(window(&after(&p, 1)), [0, 4 * F, 0, 4 * F]);
        assert_eq!(window(&after(&p, 2)), [2 * F, 5 * F, 4 * F, 3 * F]);
        assert_eq!(window(&after(&p, 3)), [2 * F, 4 * F, 7 * F, 2 * F]);
        let p = plan(&snap, &slide(2, -1), ten).unwrap();
        assert_eq!(window(&after(&p, 1)), [0, 2 * F, 0, 2 * F]);
        assert_eq!(window(&after(&p, 3)), [0, 4 * F, 5 * F, 4 * F]);
        assert_eq!(
            err(plan(&snap, &slide(2, -2), ten)),
            PulseError::PastSourceStart
        );
        assert_eq!(err(plan(&snap, &slide(1, 1), ten)), PulseError::NotAdjacent);
        assert_eq!(
            err(plan(&snap, &slide(3, 1), ten)),
            PulseError::OutsideRange
        );
    });
}

#[test]
fn refusals_cancel_and_folio_validation() {
    bounded_case("refusals_cancel_and_folio_validation", || {
        let snap = linked_fixture(|_| {});
        let cancel = CancellationToken::default();
        let extents = Extent(Some(10 * F));
        let ctx = |c: &'static CancellationToken, budget: Budget| EditContext {
            budget,
            resolver: &Media,
            extents: &extents,
            cancel: c,
        };
        let live: &'static CancellationToken = Box::leak(Box::new(cancel));
        let edit = trim_out(2, 2, false);
        let at = |rev: u64, seq: &str, e: &TimelineEdit| {
            plan_timeline_edit(&snap, seq, rev, e, &ctx(live, Budget::default()))
        };
        assert_eq!(err(at(99, "seq-main", &edit)), PulseError::StaleRevision);
        assert_eq!(err(at(3, "missing", &edit)), PulseError::UnknownSequence);
        assert_eq!(
            err(at(3, "seq-main", &trim_out(77, 2, false))),
            PulseError::UnknownClip
        );
        // A linked razor without caller-supplied right-half ids is refused with a typed code.
        let razor = TimelineEdit::Razor {
            clip_id: cid(1),
            at: Ticks::new(F),
            new_right_clip_id: cid(9),
            linked: None,
        };
        assert_eq!(err(at(3, "seq-main", &razor)), PulseError::InsufficientIds);
        assert_eq!(
            PulseError::LinkedEditUnsupported.failure_code(),
            Some(hsk_studio_observe::FailureCode::Unsupported)
        );
        // Folio's own validator runs on the result: a budget that cannot hold the document is
        // reported by Folio and mapped, not re-checked by pulse.
        let tight = Budget {
            nodes: 2,
            ..Budget::default()
        };
        assert_eq!(
            err(plan_timeline_edit(
                &snap,
                "seq-main",
                3,
                &edit,
                &ctx(live, tight)
            )),
            PulseError::LimitExceeded
        );
        // Cancellation produces nothing.
        let canceled = CancellationToken::default();
        canceled.cancel();
        let canceled: &'static CancellationToken = Box::leak(Box::new(canceled));
        assert_eq!(
            err(plan_timeline_edit(
                &snap,
                "seq-main",
                3,
                &edit,
                &ctx(canceled, Budget::default())
            )),
            PulseError::Canceled
        );
        // A refused edit leaves the base snapshot reusable at the same revision.
        let ok = at(3, "seq-main", &edit).unwrap();
        assert_eq!(snap.document().revision, 3);
        assert_eq!(ok.snapshot().document().revision, 4);
        assert!(ok.snapshot().resolution_issues().is_empty());
    });
}

fn present(p: &EditProposal, n: u32) -> bool {
    p.snapshot().document().sequences[0]
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| c.clip_id == cid(n))
}
fn range(from_frames: u64, to_frames: u64) -> TickRange {
    TickRange::new(Ticks::new(from_frames * F), Ticks::new(to_frames * F)).unwrap()
}

/// V1 (targeted) [A 0..3F, B 3F..6F, C 6F..9F]; A1 (sync-locked, not targeted) [X 0..2F, Q 7F..9F].
fn cut_fixture(mutate: impl FnOnce(&mut Vec<StudioTrack>)) -> Snapshot {
    let mut a1 = track(
        2,
        TrackKind::Audio,
        0,
        vec![clip(4, 0, 2 * F, 0, 2 * F), clip(5, 0, 2 * F, 7 * F, 2 * F)],
    );
    a1.targeted = false;
    let mut tracks = vec![
        track(
            1,
            TrackKind::Video,
            0,
            vec![
                clip(1, 0, 3 * F, 0, 3 * F),
                clip(2, 0, 3 * F, 3 * F, 3 * F),
                clip(3, 0, 3 * F, 6 * F, 3 * F),
            ],
        ),
        a1,
    ];
    mutate(&mut tracks);
    snapshot(tracks)
}

#[test]
fn lift_extract_range_and_close_gap_exact_ticks() {
    bounded_case("lift_extract_range_and_close_gap_exact_ticks", || {
        let snap = cut_fixture(|_| {});
        // Lift: B leaves a gap; nothing else moves and the sequence keeps its length.
        let lifted = plan(&snap, &TimelineEdit::LiftClip { clip_id: cid(2) }, None).unwrap();
        assert!(!present(&lifted, 2));
        assert_eq!(after(&lifted, 3).timeline_start.value(), 60_963_840_000);
        assert!(matches!(lifted.changes(), [ClipChange::Remove { .. }]));
        assert!(lifted.write_footprint().contains(&id("STRK", 1)));
        assert_eq!(
            lifted.snapshot().document().sequences[0].duration.value(),
            9 * F
        );
        // Extract (ripple delete): C closes up by 3F; the sync-locked audio track shifts Q with it.
        let extract = TimelineEdit::ExtractClip { clip_id: cid(2) };
        let extracted = plan(&snap, &extract, None).unwrap();
        assert_eq!(after(&extracted, 3).timeline_start.value(), 30_481_920_000);
        assert_eq!(after(&extracted, 5).timeline_start.value(), 40_642_560_000);
        assert_eq!(window(&after(&extracted, 4)), [0, 2 * F, 0, 2 * F]);
        assert_eq!(extracted.changes().len(), 3);
        assert_eq!(
            extracted.snapshot().document().sequences[0]
                .duration
                .value(),
            6 * F
        );
        // Content on a sync-locked track inside the removed span blocks the ripple.
        let blocked = cut_fixture(|t| t[1].clips.insert(1, clip(6, 0, 2 * F, 4 * F, 2 * F)));
        assert_eq!(
            err(plan(&blocked, &extract, None)),
            PulseError::SyncLockBlocked
        );
        // Range extract over [4F, 7F) on the targeted video track: B loses its tail, C its head.
        let span = TimelineEdit::ExtractRange {
            span: range(4, 7),
            fresh_clip_ids: vec![],
        };
        let cut = plan(&snap, &span, None).unwrap();
        assert_eq!(window(&after(&cut, 2)), [0, F, 3 * F, F]);
        assert_eq!(window(&after(&cut, 3)), [F, 3 * F, 4 * F, 2 * F]);
        assert_eq!(after(&cut, 1).duration.value(), 3 * F);
        assert_eq!(after(&cut, 5).timeline_start.value(), 40_642_560_000);
        // A straddled clip splits; the right half needs a caller-supplied unique id.
        let straddle = |ids: Vec<String>| TimelineEdit::LiftRange {
            span: range(1, 2),
            fresh_clip_ids: ids,
        };
        let split = plan(&snap, &straddle(vec![cid(11)]), None).unwrap();
        assert_eq!(window(&after(&split, 1)), [0, F, 0, F]);
        assert_eq!(window(&after(&split, 11)), [2 * F, 3 * F, 2 * F, F]);
        assert_eq!(
            err(plan(&snap, &straddle(vec![]), None)),
            PulseError::InsufficientIds
        );
        assert_eq!(
            err(plan(&snap, &straddle(vec![cid(1)]), None)),
            PulseError::DuplicateId
        );
        let off_grid = TimelineEdit::LiftRange {
            span: TickRange::new(Ticks::new(F + 1), Ticks::new(2 * F)).unwrap(),
            fresh_clip_ids: vec![cid(11)],
        };
        assert_eq!(
            err(plan(&snap, &off_grid, None)),
            PulseError::NotFrameAligned
        );
        let locked = cut_fixture(|t| t[0].locked = true);
        assert_eq!(
            err(plan(&locked, &straddle(vec![cid(11)]), None)),
            PulseError::TrackLocked
        );
        let linked_in_span = cut_fixture(|t| t[0].clips[0] = linked(clip(1, 0, 3 * F, 0, 3 * F)));
        assert_eq!(
            err(plan(&linked_in_span, &straddle(vec![cid(11)]), None)),
            PulseError::LinkedEditUnsupported
        );
        // Close gap: lift B, then close the 3F gap it left (Q on A1 follows, X stays).
        let close = |track: u32, frames: u64| TimelineEdit::CloseGap {
            track_id: id("STRK", track),
            at: Ticks::new(frames * F),
        };
        let closed = plan(lifted.snapshot(), &close(1, 4), None).unwrap();
        assert_eq!((closed.base_revision(), closed.target_revision()), (4, 5));
        assert_eq!(after(&closed, 3).timeline_start.value(), 30_481_920_000);
        assert_eq!(after(&closed, 5).timeline_start.value(), 40_642_560_000);
        assert_eq!(
            err(plan(lifted.snapshot(), &close(1, 2), None)),
            PulseError::OutsideRange
        );
        assert_eq!(
            err(plan(lifted.snapshot(), &close(1, 6), None)),
            PulseError::OutsideRange
        );
        assert_eq!(
            err(plan(lifted.snapshot(), &close(9, 4), None)),
            PulseError::UnknownTrack
        );
        // A sync-locked video track cannot absorb a shift that is not a whole number of frames.
        let mut audio = track(
            2,
            TrackKind::Audio,
            0,
            vec![clip(7, 0, 2 * F + 1, 0, 2 * F + 1)],
        );
        audio.targeted = false;
        let mixed = snapshot(vec![
            audio,
            track(
                1,
                TrackKind::Video,
                0,
                vec![clip(8, 0, 3 * F, 3 * F, 3 * F)],
            ),
        ]);
        let by_audio = plan(&mixed, &TimelineEdit::ExtractClip { clip_id: cid(7) }, None);
        assert_eq!(err(by_audio), PulseError::NotFrameAligned);
    });
}

/// V1 [A g1, B g2, C g3] and A1 [AA g1, BB g2, CC g3], each clip 3F long with source 0..3F.
fn linked_three(mutate: impl FnOnce(&mut Vec<StudioTrack>)) -> Snapshot {
    let row = |first: u32| -> Vec<StudioClip> {
        (0..3)
            .map(|i| {
                let mut c = clip(first + i, 0, 3 * F, u64::from(i) * 3 * F, 3 * F);
                c.link_group = Some(format!("g{}", 1 + i));
                c
            })
            .collect()
    };
    let mut tracks = vec![
        track(1, TrackKind::Video, 0, row(1)),
        track(2, TrackKind::Audio, 0, row(4)),
    ];
    mutate(&mut tracks);
    snapshot(tracks)
}

#[test]
fn linked_razor_roll_slide_extract_exact_ticks() {
    bounded_case("linked_razor_roll_slide_extract_exact_ticks", || {
        let snap = linked_three(|_| {});
        let ten = Some(10 * F);
        let group = |p: &EditProposal, n: u32| after(p, n).link_group;
        // Razor splits every member; right halves form a new link group with caller-given ids.
        let razor = |linked: Option<LinkedRazor>| TimelineEdit::Razor {
            clip_id: cid(1),
            at: Ticks::new(F),
            new_right_clip_id: cid(11),
            linked,
        };
        let extra = |group: &str, partners: Vec<(u32, u32)>| LinkedRazor {
            right_link_group: group.into(),
            right_clip_ids: partners
                .into_iter()
                .map(|(a, b)| (cid(a), cid(b)))
                .collect(),
        };
        let p = plan(&snap, &razor(Some(extra("g1r", vec![(4, 14)]))), ten).unwrap();
        for (left, right) in [(1, 11), (4, 14)] {
            assert_eq!(window(&after(&p, left)), [0, F, 0, F]);
            assert_eq!(window(&after(&p, right)), [F, 3 * F, F, 2 * F]);
            assert_eq!(group(&p, left).as_deref(), Some("g1"));
            assert_eq!(group(&p, right).as_deref(), Some("g1r"));
        }
        assert_eq!(p.changes().len(), 4);
        assert_eq!(
            err(plan(&snap, &razor(None), ten)),
            PulseError::InsufficientIds
        );
        assert_eq!(
            err(plan(&snap, &razor(Some(extra("g1r", vec![]))), ten)),
            PulseError::InsufficientIds
        );
        assert_eq!(
            err(plan(&snap, &razor(Some(extra("g1", vec![(4, 14)]))), ten)),
            PulseError::DuplicateId
        );
        assert_eq!(
            err(plan(&snap, &razor(Some(extra("g2", vec![(4, 14)]))), ten)),
            PulseError::DuplicateId
        );
        assert_eq!(
            err(plan(&snap, &razor(Some(extra("g1r", vec![(4, 11)]))), ten)),
            PulseError::DuplicateId
        );
        // Roll: both members edits move together.
        let roll = TimelineEdit::Roll {
            left_clip_id: cid(1),
            delta: frames(1),
        };
        let p = plan(&snap, &roll, ten).unwrap();
        for (left, right) in [(1, 2), (4, 5)] {
            assert_eq!(window(&after(&p, left)), [0, 4 * F, 0, 4 * F]);
            assert_eq!(window(&after(&p, right)), [F, 3 * F, 4 * F, 2 * F]);
        }
        assert_eq!(after(&p, 3).timeline_start.value(), 6 * F);
        // Slide: the previous clips lengthen, the next ones shorten, on both tracks.
        let slide = TimelineEdit::Slide {
            clip_id: cid(2),
            delta: frames(1),
        };
        let p = plan(&snap, &slide, ten).unwrap();
        for (prev, mid, next) in [(1, 2, 3), (4, 5, 6)] {
            assert_eq!(window(&after(&p, prev)), [0, 4 * F, 0, 4 * F]);
            assert_eq!(window(&after(&p, mid)), [0, 3 * F, 4 * F, 3 * F]);
            assert_eq!(window(&after(&p, next)), [F, 3 * F, 7 * F, 2 * F]);
        }
        // A link group with a member outside the edit is not closed over it: refused.
        let open = linked_three(|t| {
            let mut far = track(
                3,
                TrackKind::Audio,
                1,
                vec![clip(7, 0, 3 * F, 20 * F, 3 * F)],
            );
            far.clips[0].link_group = Some("g2".into());
            t.push(far);
        });
        assert_eq!(
            err(plan(&open, &roll, ten)),
            PulseError::LinkedEditUnsupported
        );
        let locked = linked_three(|t| t[1].locked = true);
        assert_eq!(err(plan(&locked, &roll, ten)), PulseError::TrackLocked);
        // Linked lift/extract remove the whole group; extract closes both tracks.
        let lifted = plan(&snap, &TimelineEdit::LiftClip { clip_id: cid(2) }, ten).unwrap();
        assert!(!present(&lifted, 2) && !present(&lifted, 5) && present(&lifted, 3));
        assert_eq!(lifted.changes().len(), 2);
        let p = plan(&snap, &TimelineEdit::ExtractClip { clip_id: cid(2) }, ten).unwrap();
        for n in [3, 6] {
            assert_eq!(window(&after(&p, n)), [0, 3 * F, 3 * F, 3 * F]);
        }
        assert_eq!(p.snapshot().document().sequences[0].duration.value(), 6 * F);
    });
}
