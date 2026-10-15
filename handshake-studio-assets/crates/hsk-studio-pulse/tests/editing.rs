//! Lane SMB-PULSE-EDITING: three-point insert/overwrite, undo/redo of every edit kind, markers,
//! transitions and hostile input. Expected timelines are hand-built in each test.
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
fn tid(n: u32) -> String {
    id("STRK", n)
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

/// Every source is 1000 frames long.
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}

fn media() -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: "manifest-a".into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "abc123".into(),
        },
    }
}

/// Clip `n`: source frames `[si, so)` at timeline frame `start` (unit speed).
fn clip(n: u32, si: u64, so: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: media(),
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
    }
}
fn track(
    n: u32,
    kind: TrackKind,
    index: u32,
    sync_locked: bool,
    clips: Vec<StudioClip>,
) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
    }
}
fn sequence(tracks: Vec<StudioTrack>) -> StudioSequence {
    let mut seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks,
    };
    seq.duration = seq.content_end().unwrap();
    seq
}
fn snapshot_of(seq: StudioSequence, revision: u64) -> Snapshot {
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision,
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
        sequences: vec![seq],
    };
    validate_document(
        document,
        Budget::default(),
        &CancellationToken::default(),
        &Media,
    )
    .expect("fixture is a valid Folio document")
}

/// V1 (video, sync-locked): A src 0..10 @0, B src 100..110 @10.
/// A1 (audio, sync-locked): C src 0..10 @5.   V2 (video, NOT sync-locked): D src 0..2 @12.
fn base() -> StudioSequence {
    sequence(vec![
        track(
            1,
            TrackKind::Video,
            0,
            true,
            vec![clip(1, 0, 10, 0), clip(2, 100, 110, 10)],
        ),
        track(2, TrackKind::Audio, 0, true, vec![clip(3, 0, 10, 5)]),
        track(3, TrackKind::Video, 1, false, vec![clip(4, 0, 2, 12)]),
    ])
}

fn ctx_run<T>(f: impl FnOnce(&EditContext) -> T) -> T {
    let cancel = CancellationToken::default();
    f(&EditContext {
        budget: Budget::default(),
        resolver: &Media,
        extents: &Extent,
        cancel: &cancel,
    })
}
fn assemble(snap: &Snapshot, edit: &AssemblyEdit) -> Result<EditProposal> {
    ctx_run(|ctx| plan_assembly_edit(snap, "seq-main", snap.document().revision, edit, ctx))
}
fn edit(snap: &Snapshot, e: &TimelineEdit) -> Result<EditProposal> {
    ctx_run(|ctx| plan_timeline_edit(snap, "seq-main", snap.document().revision, e, ctx))
}
fn revert(snap: &Snapshot, inverse: &TimelineInverse) -> Result<EditProposal> {
    ctx_run(|ctx| plan_timeline_revert(snap, "seq-main", snap.document().revision, inverse, ctx))
}
fn seq_of(p: &EditProposal) -> &StudioSequence {
    &p.snapshot().document().sequences[0]
}
fn pts(si: Option<u64>, so: Option<u64>, qi: Option<u64>, qo: Option<u64>) -> AssemblyPoints {
    let t = |v: Option<u64>| v.map(|f| Ticks::new(f * F));
    AssemblyPoints {
        source_in: t(si),
        source_out: t(so),
        sequence_in: t(qi),
        sequence_out: t(qo),
    }
}
fn assembly(
    mode: AssemblyMode,
    points: AssemblyPoints,
    patches: &[(u32, u32)],
    fresh: &[u32],
) -> AssemblyEdit {
    AssemblyEdit {
        mode,
        source: media(),
        speed: Speed::UNIT,
        points,
        fit: None,
        patches: patches
            .iter()
            .map(|(t, c)| SourcePatch {
                track_id: tid(*t),
                clip_id: cid(*c),
            })
            .collect(),
        link_group: None,
        fresh_clip_ids: fresh.iter().map(|n| cid(*n)).collect(),
    }
}
fn with_id(mut c: StudioClip, n: u32) -> StudioClip {
    c.clip_id = cid(n);
    c
}

#[test]
fn three_point_insert_and_overwrite_match_hand_built_timelines() {
    bounded_case("three_point", || {
        let snap = snapshot_of(base(), 3);

        // Insert 3 frames (source 50..53) at sequence frame 4 on V1; fourth point derived.
        let insert = assembly(
            AssemblyMode::Insert,
            pts(Some(50), Some(53), Some(4), None),
            &[(1, 20)],
            &[30, 31],
        );
        let p = assemble(&snap, &insert).expect("insert");
        let expected = sequence(vec![
            track(
                1,
                TrackKind::Video,
                0,
                true,
                vec![
                    clip(1, 0, 4, 0),               // A split at the in point
                    clip(20, 50, 53, 4),            // incoming clip
                    with_id(clip(1, 4, 10, 7), 30), // A's right half, shifted by 3
                    clip(2, 100, 110, 13),          // B shifted by 3
                ],
            ),
            track(2, TrackKind::Audio, 0, true, vec![clip(3, 0, 10, 8)]), // sync-locked: shifts
            track(3, TrackKind::Video, 1, false, vec![clip(4, 0, 2, 12)]), // not sync-locked: stays
        ]);
        assert_eq!(seq_of(&p), &expected);
        assert_eq!((p.base_revision(), p.target_revision()), (3, 4));

        // Overwrite: source in 50, sequence in 8 / out 12 -> source out 54 derived. A loses its
        // tail, B its head (source in moves by the same 2 frames); nothing downstream moves.
        let over = assembly(
            AssemblyMode::Overwrite,
            pts(Some(50), None, Some(8), Some(12)),
            &[(1, 21)],
            &[],
        );
        let p = assemble(&snap, &over).expect("overwrite");
        let expected = sequence(vec![
            track(
                1,
                TrackKind::Video,
                0,
                true,
                vec![clip(1, 0, 8, 0), clip(21, 50, 54, 8), clip(2, 102, 110, 12)],
            ),
            track(2, TrackKind::Audio, 0, true, vec![clip(3, 0, 10, 5)]),
            track(3, TrackKind::Video, 1, false, vec![clip(4, 0, 2, 12)]),
        ]);
        assert_eq!(seq_of(&p), &expected);

        // Overwrite inside A on V1 and on A1 at once, linked: A is cut in two (fresh id 32), C too (33).
        let mut both = assembly(
            AssemblyMode::Overwrite,
            pts(Some(50), Some(51), Some(6), None),
            &[(1, 22), (2, 23)],
            &[32, 33],
        );
        both.link_group = Some("lg-new".into());
        let p = assemble(&snap, &both).expect("linked two-track overwrite");
        let seq = seq_of(&p);
        let v1: Vec<_> = seq.tracks[0]
            .clips
            .iter()
            .map(|c| {
                (
                    c.clip_id.clone(),
                    c.source_in.value() / F,
                    c.timeline_start.value() / F,
                    c.duration.value() / F,
                )
            })
            .collect();
        assert_eq!(
            v1,
            vec![
                (cid(1), 0, 0, 6),
                (cid(22), 50, 6, 1),
                (cid(32), 7, 7, 3),
                (cid(2), 100, 10, 10)
            ]
        );
        let a1: Vec<_> = seq.tracks[1]
            .clips
            .iter()
            .map(|c| {
                (
                    c.clip_id.clone(),
                    c.source_in.value() / F,
                    c.timeline_start.value() / F,
                    c.duration.value() / F,
                )
            })
            .collect();
        assert_eq!(
            a1,
            vec![(cid(3), 0, 5, 1), (cid(23), 50, 6, 1), (cid(33), 2, 7, 8)]
        );
        assert_eq!(seq.tracks[0].clips[1].link_group.as_deref(), Some("lg-new"));
        assert_eq!(seq.tracks[1].clips[1].link_group.as_deref(), Some("lg-new"));

        // Backtiming (source in/out + sequence out) and the four-point fit contract.
        let r = resolve_points(pts(Some(50), Some(53), None, Some(10)), Speed::UNIT, None).unwrap();
        assert_eq!((r.sequence_in.value(), r.duration.value()), (7 * F, 3 * F));
        let four = pts(Some(50), Some(54), Some(8), Some(10));
        assert_eq!(
            resolve_points(four, Speed::UNIT, None),
            Err(PulseError::FitRequired)
        );
        let r = resolve_points(four, Speed::UNIT, Some(FourPointFit::TrimTail)).unwrap();
        assert_eq!(
            (
                r.source_in.value(),
                r.source_out.value(),
                r.duration.value()
            ),
            (50 * F, 52 * F, 2 * F)
        );
        let r = resolve_points(four, Speed::UNIT, Some(FourPointFit::TrimHead)).unwrap();
        assert_eq!(
            (r.source_in.value(), r.source_out.value()),
            (52 * F, 54 * F)
        );
        let r = resolve_points(four, Speed::UNIT, Some(FourPointFit::ChangeSpeed)).unwrap();
        assert_eq!(
            (r.speed, r.duration.value()),
            (
                Speed {
                    numerator: 2,
                    denominator: 1
                },
                2 * F
            )
        );
        let r = resolve_points(four, Speed::UNIT, Some(FourPointFit::IgnoreSequenceOut)).unwrap();
        assert_eq!(r.duration.value(), 4 * F);
        assert_eq!(
            resolve_points(pts(Some(50), None, Some(8), None), Speed::UNIT, None),
            Err(PulseError::InvalidInput)
        );
    });
}

#[test]
fn every_edit_kind_is_one_step_that_undoes_and_redoes_exactly() {
    bounded_case("undo_redo", || {
        // V1: A src 0..10 @0, B src 100..110 @10, E src 200..205 @20; A1 C src 0..10 @5.
        let start = sequence(vec![
            track(
                1,
                TrackKind::Video,
                0,
                true,
                vec![
                    clip(1, 0, 10, 0),
                    clip(2, 100, 110, 10),
                    clip(5, 200, 205, 20),
                ],
            ),
            track(2, TrackKind::Audio, 0, true, vec![clip(3, 0, 10, 5)]),
        ]);
        let snap = snapshot_of(start.clone(), 7);
        let d = |f: i128| TickDelta::new(f * i128::from(F)).unwrap();
        let w = |p: &EditProposal, t: usize, c: usize| {
            let k = &seq_of(p).tracks[t].clips[c];
            [
                k.source_in.value() / F,
                k.source_out.value() / F,
                k.timeline_start.value() / F,
                k.duration.value() / F,
            ]
        };
        type Check = Box<dyn Fn(&EditProposal)>;
        let cases: Vec<(&str, Result<EditProposal>, Check)> = vec![
            (
                "insert",
                assemble(
                    &snap,
                    &assembly(
                        AssemblyMode::Insert,
                        pts(Some(0), Some(2), Some(20), None),
                        &[(1, 40)],
                        &[],
                    ),
                ),
                Box::new(move |p| {
                    assert_eq!(w(p, 0, 2), [0, 2, 20, 2]);
                    assert_eq!(w(p, 0, 3), [200, 205, 22, 5]);
                }),
            ),
            (
                "ripple trim out",
                edit(
                    &snap,
                    &TimelineEdit::TrimOut {
                        clip_id: cid(2),
                        new_duration: Ticks::new(6 * F),
                        ripple: true,
                    },
                ),
                Box::new(move |p| {
                    assert_eq!(w(p, 0, 1), [100, 106, 10, 6]);
                    assert_eq!(w(p, 0, 2), [200, 205, 16, 5]);
                    assert_eq!(w(p, 1, 0), [0, 10, 5, 10]);
                }),
            ),
            (
                "roll",
                edit(
                    &snap,
                    &TimelineEdit::Roll {
                        left_clip_id: cid(1),
                        delta: d(-2),
                    },
                ),
                Box::new(move |p| {
                    assert_eq!(w(p, 0, 0), [0, 8, 0, 8]);
                    assert_eq!(w(p, 0, 1), [98, 110, 8, 12]);
                }),
            ),
            (
                "slip",
                edit(
                    &snap,
                    &TimelineEdit::Slip {
                        clip_id: cid(2),
                        delta: d(5),
                    },
                ),
                Box::new(move |p| assert_eq!(w(p, 0, 1), [105, 115, 10, 10])),
            ),
            (
                "slide",
                edit(
                    &snap,
                    &TimelineEdit::Slide {
                        clip_id: cid(2),
                        delta: d(3),
                    },
                ),
                Box::new(move |p| {
                    assert_eq!(w(p, 0, 0), [0, 13, 0, 13]);
                    assert_eq!(w(p, 0, 1), [100, 110, 13, 10]);
                    assert_eq!(w(p, 0, 2), [203, 205, 23, 2]);
                }),
            ),
            (
                "ripple delete",
                edit(&snap, &TimelineEdit::ExtractClip { clip_id: cid(2) }),
                Box::new(move |p| {
                    assert_eq!(seq_of(p).tracks[0].clips.len(), 2);
                    assert_eq!(w(p, 0, 1), [200, 205, 10, 5]);
                    assert_eq!(w(p, 1, 0), [0, 10, 5, 10]);
                }),
            ),
        ];
        for (name, proposal, check) in cases {
            let p = proposal.unwrap_or_else(|e| panic!("{name}: {e}"));
            check(&p);
            assert_eq!(p.target_revision(), 8, "{name}: one revision step");
            // Undo at revision 8 restores the exact pre-edit sequence at revision 9.
            let applied = p.snapshot().clone();
            let undo = revert(&applied, p.inverse()).unwrap_or_else(|e| panic!("{name} undo: {e}"));
            assert_eq!(seq_of(&undo), &start, "{name}: undo");
            assert_eq!(undo.target_revision(), 9);
            // Redo is the undo's own inverse and lands exactly on the edited sequence.
            let redo = revert(undo.snapshot(), undo.inverse())
                .unwrap_or_else(|e| panic!("{name} redo: {e}"));
            assert_eq!(seq_of(&redo), seq_of(&p), "{name}: redo");
            // The precondition: an inverse never applies over content it did not produce.
            assert_eq!(
                revert(&snap, p.inverse()).unwrap_err(),
                PulseError::RevertMismatch,
                "{name}"
            );
        }
    });
}

#[test]
fn hostile_assembly_input_is_refused_typed_before_work() {
    bounded_case("hostile", || {
        let snap = snapshot_of(base(), 3);
        let code = |e: &AssemblyEdit| assemble(&snap, e).unwrap_err().code();
        let ok = || {
            assembly(
                AssemblyMode::Insert,
                pts(Some(0), Some(2), Some(4), None),
                &[(1, 20)],
                &[30],
            )
        };

        // Two streams on one track would overlap; unknown and locked tracks.
        let mut e = ok();
        e.patches.push(SourcePatch {
            track_id: tid(1),
            clip_id: cid(21),
        });
        assert_eq!(code(&e), "overlap");
        let mut e = ok();
        e.patches[0].track_id = tid(99);
        assert_eq!(code(&e), "unknown_track");
        assert_eq!(
            code(&AssemblyEdit {
                patches: vec![],
                ..ok()
            }),
            "invalid_input"
        );

        // Huge counts are refused on their length alone (String::new does not allocate).
        let mut e = ok();
        e.fresh_clip_ids = vec![String::new(); MAX_CLIPS_PER_SEQUENCE + 1];
        assert_eq!(code(&e), "limit_exceeded");
        let mut e = ok();
        e.patches = vec![
            SourcePatch {
                track_id: String::new(),
                clip_id: String::new()
            };
            MAX_TRACKS_PER_SEQUENCE + 1
        ];
        assert_eq!(code(&e), "limit_exceeded");

        // Tick overflow: a sequence in point near u64::MAX (audio track: no frame grid).
        let mut e = assembly(
            AssemblyMode::Overwrite,
            pts(Some(0), Some(2), None, None),
            &[(2, 20)],
            &[],
        );
        e.points.sequence_in = Some(Ticks::new(u64::MAX - F));
        assert_eq!(code(&e), "overflow");
        // A sync-locked clip pushed past u64::MAX by an insert.
        let mut far = base();
        far.tracks[1].clips[0].timeline_start = Ticks::new(u64::MAX - 20 * F);
        far.duration = far.content_end().unwrap();
        let far_snap = snapshot_of(far, 3);
        let e = assembly(
            AssemblyMode::Insert,
            pts(Some(0), Some(30), Some(4), None),
            &[(1, 20)],
            &[30],
        );
        assert_eq!(assemble(&far_snap, &e).unwrap_err().code(), "overflow");
        // i64 tick overflow: a ChangeSpeed fit whose source span exceeds i64::MAX.
        let huge = AssemblyPoints {
            source_in: Some(Ticks::new(0)),
            source_out: Some(Ticks::new(u64::MAX)),
            sequence_in: Some(Ticks::new(0)),
            sequence_out: Some(Ticks::new(F)),
        };
        assert_eq!(
            resolve_points(huge, Speed::UNIT, Some(FourPointFit::ChangeSpeed)),
            Err(PulseError::InvalidSpeed)
        );
        assert_eq!(
            resolve_points(huge, Speed::UNIT, None),
            Err(PulseError::FitRequired)
        );

        // Video in point and duration must be whole frames; source past its extent.
        let mut e = ok();
        e.points.sequence_in = Some(Ticks::new(4 * F + 1));
        assert_eq!(code(&e), "not_frame_aligned");
        let e = assembly(
            AssemblyMode::Insert,
            pts(Some(999), Some(1001), Some(4), None),
            &[(1, 20)],
            &[30],
        );
        assert_eq!(code(&e), "past_source_end");

        // Locked patch track; locked sync-locked track that would have to shift.
        let mut locked = base();
        locked.tracks[0].locked = true;
        assert_eq!(
            assemble(&snapshot_of(locked, 3), &ok()).unwrap_err().code(),
            "track_locked"
        );
        let mut blocked = base();
        blocked.tracks[1].locked = true;
        assert_eq!(
            assemble(&snapshot_of(blocked, 3), &ok())
                .unwrap_err()
                .code(),
            "sync_lock_blocked"
        );

        // Ids: duplicate new id, missing fresh id for the split, link group already in use.
        let e = assembly(
            AssemblyMode::Insert,
            pts(Some(0), Some(2), Some(4), None),
            &[(1, 2)],
            &[30],
        );
        assert_eq!(code(&e), "duplicate_id");
        assert_eq!(
            code(&AssemblyEdit {
                fresh_clip_ids: vec![],
                ..ok()
            }),
            "insufficient_ids"
        );
        // Stale revision.
        let stale = ctx_run(|ctx| plan_assembly_edit(&snap, "seq-main", 2, &ok(), ctx));
        assert_eq!(stale.unwrap_err(), PulseError::StaleRevision);
    });
}

fn marker(id: &str, scope: MarkerScope, start: u64, frames: u64) -> Marker {
    Marker {
        marker_id: id.into(),
        scope,
        name: "note".into(),
        comment: String::new(),
        color: LabelColor::Rose,
        start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        role: None,
    }
}

#[test]
fn markers_keep_their_scope_through_edits_with_spans_and_undo() {
    bounded_case("markers", || {
        let cancel = CancellationToken::default();
        let mut reversed = base();
        reversed.tracks[2].clips[0].speed = Speed {
            numerator: -1,
            denominator: 1,
        };
        let snap = snapshot_of(reversed, 3);
        let seq = &snap.document().sequences[0];
        let spans = |s: &StudioSequence, a: &TimelineAnnotations| -> Vec<(String, u64, u64)> {
            marker_spans(s, a, &cancel)
                .unwrap()
                .into_iter()
                .map(|(id, r)| (id, r.start().value() / F, r.end().value() / F))
                .collect()
        };

        // A sequence chapter marker spanning frames 12..15, a clip marker on A at source 6..8 and
        // a point marker on the reversed clip D (source 0..2 plays at 12..14 backwards).
        let mut chapter = marker("mk-seq", MarkerScope::Sequence, 12, 3);
        chapter.role = Some(MarkerRole::Chapter);
        let on_a = marker("mk-a", MarkerScope::Clip { clip_id: cid(1) }, 6, 2);
        let on_d = marker("mk-d", MarkerScope::Clip { clip_id: cid(4) }, 0, 1);
        let mut ann = TimelineAnnotations::new("seq-main");
        for (rev, m) in [chapter.clone(), on_a, on_d].into_iter().enumerate() {
            let p = plan_annotation_edit(
                &ann,
                seq,
                rev as u64,
                &AnnotationEdit::AddMarker(m),
                &Extent,
                &cancel,
            )
            .unwrap();
            ann = p.annotations().clone();
        }
        assert_eq!(ann.revision(), 3);
        let want = |v: &[(&str, u64, u64)]| {
            v.iter()
                .map(|(i, a, b)| ((*i).to_owned(), *a, *b))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            spans(seq, &ann),
            want(&[("mk-a", 6, 8), ("mk-seq", 12, 15), ("mk-d", 13, 14)])
        );

        // Insert 3 frames at 4: A splits (right half 30 = source 4..10 at 7), so the clip marker
        // moves to the piece and lands at 9..11; the sequence marker does not move with clips.
        let insert = assembly(
            AssemblyMode::Insert,
            pts(Some(50), Some(53), Some(4), None),
            &[(1, 20)],
            &[30],
        );
        let p = assemble(&snap, &insert).unwrap();
        assert_eq!(p.splits(), &[(cid(1), cid(30))]);
        let f = follow_timeline_edit(&ann, &p, 3, &Extent, &cancel).unwrap();
        let moved = f.annotations();
        assert_eq!(
            moved.marker("mk-a").unwrap().scope,
            MarkerScope::Clip { clip_id: cid(30) }
        );
        assert!(f.dropped_markers().is_empty());
        let s1 = seq_of(&p);
        assert_eq!(
            spans(s1, moved),
            want(&[("mk-a", 9, 11), ("mk-seq", 12, 15), ("mk-d", 13, 14)])
        );
        let at = |f: u64| Ticks::new(f * F);
        assert_eq!(next_marker(s1, moved, at(0), &cancel).unwrap(), Some(at(9)));
        assert_eq!(
            next_marker(s1, moved, at(9), &cancel).unwrap(),
            Some(at(12))
        );
        assert_eq!(
            prev_marker(s1, moved, at(12), &cancel).unwrap(),
            Some(at(9))
        );
        assert_eq!(next_marker(s1, moved, at(13), &cancel).unwrap(), None);

        // Ripple delete of the piece drops its marker and says so; undo brings it back exactly.
        let ext = edit(
            p.snapshot(),
            &TimelineEdit::ExtractClip { clip_id: cid(30) },
        )
        .unwrap();
        let g = follow_timeline_edit(moved, &ext, 4, &Extent, &cancel).unwrap();
        assert_eq!(g.dropped_markers(), &["mk-a".to_owned()]);
        assert!(g.annotations().marker("mk-a").is_none());
        let s2 = seq_of(&ext);
        let undo = plan_annotation_revert(g.annotations(), s2, 5, g.inverse(), &cancel).unwrap();
        assert_eq!(undo.annotations().markers(), moved.markers());
        let redo =
            plan_annotation_revert(undo.annotations(), s2, 6, undo.inverse(), &cancel).unwrap();
        assert_eq!(redo.annotations().markers(), g.annotations().markers());
        assert_eq!(
            plan_annotation_revert(&ann, seq, 3, g.inverse(), &cancel).unwrap_err(),
            PulseError::RevertMismatch
        );

        // Replace moves the chapter; remove; typed refusals.
        let mut later = chapter.clone();
        later.start = at(20);
        let r = plan_annotation_edit(
            &ann,
            seq,
            3,
            &AnnotationEdit::ReplaceMarker(later),
            &Extent,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            marker_span(seq, r.annotations().marker("mk-seq").unwrap())
                .unwrap()
                .map(|s| s.start()),
            Some(at(20))
        );
        let code = |e: AnnotationEdit| {
            plan_annotation_edit(&ann, seq, 3, &e, &Extent, &cancel)
                .unwrap_err()
                .code()
        };
        assert_eq!(
            code(AnnotationEdit::AddMarker(chapter.clone())),
            "duplicate_id"
        );
        assert_eq!(
            code(AnnotationEdit::RemoveMarker {
                marker_id: "nope".into()
            }),
            "unknown_marker"
        );
        assert_eq!(
            code(AnnotationEdit::ReplaceMarker(chapter.clone())),
            "no_change"
        );
        assert_eq!(
            code(AnnotationEdit::AddMarker(marker(
                "x",
                MarkerScope::Clip { clip_id: cid(99) },
                0,
                0
            ))),
            "unknown_clip"
        );
        let mut odd = marker("x", MarkerScope::Sequence, 1, 0);
        odd.start = Ticks::new(F + 1);
        assert_eq!(code(AnnotationEdit::AddMarker(odd)), "not_frame_aligned");
        let mut far = marker("x", MarkerScope::Clip { clip_id: cid(1) }, 0, 0);
        far.start = Ticks::new(u64::MAX);
        far.duration = Ticks::new(1);
        assert_eq!(code(AnnotationEdit::AddMarker(far)), "overflow");
        let mut bad = marker("x\u{7}", MarkerScope::Sequence, 0, 0);
        assert_eq!(
            code(AnnotationEdit::AddMarker(bad.clone())),
            "invalid_input"
        );
        bad.marker_id = "x".into();
        bad.name = "n".repeat(MAX_ANNOTATION_TEXT_BYTES + 1);
        assert_eq!(code(AnnotationEdit::AddMarker(bad)), "invalid_input");
        assert_eq!(
            plan_annotation_edit(
                &ann,
                seq,
                2,
                &AnnotationEdit::RemoveMarker {
                    marker_id: "mk-seq".into()
                },
                &Extent,
                &cancel
            )
            .unwrap_err(),
            PulseError::StaleRevision
        );
        // A stored set over the bound is refused on its length before any marker is checked.
        let many = vec![marker("same", MarkerScope::Sequence, 0, 0); MAX_MARKERS + 1];
        assert_eq!(
            TimelineAnnotations::from_parts(seq, 0, many, vec![], &Extent, &cancel).unwrap_err(),
            PulseError::LimitExceeded
        );
        let dup = vec![marker("same", MarkerScope::Sequence, 0, 0); 2];
        assert_eq!(
            TimelineAnnotations::from_parts(seq, 0, dup, vec![], &Extent, &cancel).unwrap_err(),
            PulseError::DuplicateId
        );
    });
}

mod render {
    //! Composite's composition-scene path on the CPU backend (test-only edge, see the lane file).
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_composite::{
        SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits, ScenePixels,
        SceneTile, render_scene,
    };
    use hsk_studio_pigment::Rect;
    use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode};
    use hsk_studio_pigment::render_plan::{
        AlphaConvention, ColourTag, Exactness, ExecuteRequest, SampleFormat, TilePlanes,
    };
    use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
    use hsk_studio_render_cpu::Renderer;

    pub const SIDE: u32 = 4;

    fn space() -> ColourTag {
        ColourTag {
            profile_sha256: [7; 32],
            blend_space: BlendSpace::Encoded,
        }
    }

    /// One solid opaque `SIDE` x `SIDE` frame per clip (`rgb_f32le`, straight alpha).
    pub struct Frame {
        id: String,
        colour: Vec<u8>,
        alpha: Vec<u8>,
        colour_sha256: [u8; 32],
        alpha_sha256: [u8; 32],
    }
    impl Frame {
        pub fn solid(id: &str, rgb: [f32; 3]) -> Self {
            let (mut colour, mut alpha) = (Vec::new(), Vec::new());
            for _ in 0..SIDE * SIDE {
                for c in rgb {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&1.0f32.to_le_bytes());
            }
            let colour_sha256 = hsk_studio_prism::profile_hash(&colour);
            let alpha_sha256 = hsk_studio_prism::profile_hash(&alpha);
            Self {
                id: id.into(),
                colour,
                alpha,
                colour_sha256,
                alpha_sha256,
            }
        }
        fn layer(&self, n: u8, opacity: f64) -> SceneLayer {
            SceneLayer {
                layer_id: format!("SLYR-019abcde-0000-7000-8000-{n:012x}"),
                content: SceneContent::Tiles(vec![SceneTile {
                    tile_id: self.id.clone(),
                    placement: Rect {
                        x: 0,
                        y: 0,
                        width: SIDE,
                        height: SIDE,
                    },
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: self.colour_sha256,
                    alpha_sha256: self.alpha_sha256,
                }]),
                mode: StudioBlendMode::Normal,
                opacity,
                fill_opacity: 1.0,
                matrix: Affine2::IDENTITY,
                matte: None,
                clipped: false,
            }
        }
    }

    struct Pixels<'a>(&'a [&'a Frame]);
    impl ScenePixels for Pixels<'_> {
        fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>> {
            let f = self.0.iter().find(|f| f.id == tile_id)?;
            Some(TilePlanes {
                width: SIDE,
                height: SIDE,
                colour: &f.colour,
                colour_stride_bytes: u64::from(SIDE) * 12,
                alpha: &f.alpha,
                alpha_stride_bytes: u64::from(SIDE) * 4,
                colour_sha256: f.colour_sha256,
                alpha_sha256: f.alpha_sha256,
                format: SampleFormat::RgbF32Le,
                alpha_convention: AlphaConvention::Straight,
                space: space(),
            })
        }
    }

    /// The cross dissolve frame: the incoming clip's frame at opacity `incoming_weight`
    /// composited normally over the outgoing clip's frame; returns pixel (1, 2).
    pub fn dissolve(outgoing: &Frame, incoming: &Frame, incoming_weight: f64) -> [f32; 4] {
        let scene = SceneComposition {
            composition_id: "SCOM-019abcde-0000-7000-8000-000000000001".into(),
            width: SIDE,
            height: SIDE,
            background: None,
            filter: ResampleFilter::Bilinear,
            layers: vec![outgoing.layer(1, 1.0), incoming.layer(2, incoming_weight)],
        };
        let renderer = Renderer::new(64).unwrap();
        let frames = [outgoing, incoming];
        let pixels = Pixels(&frames);
        let ctx = SceneContext {
            backend: &renderer,
            pixels: &pixels,
            request: ExecuteRequest {
                expected_revision: 7,
                min_exactness: Exactness::Approximate,
                deadline: None,
                max_working_bytes: 1 << 24,
            },
            space: space(),
            limits: SceneLimits::default(),
        };
        let rendered =
            render_scene(&scene, &ctx, &CancellationToken::default()).expect("frame renders");
        rendered.pixel(1, 2).expect("pixel in frame")
    }
}

fn dissolve(id: &str, out: u32, inc: u32, frames: u64) -> Transition {
    Transition {
        transition_id: id.into(),
        kind: TransitionKind::CrossDissolve,
        outgoing_clip_id: cid(out),
        incoming_clip_id: cid(inc),
        duration: Ticks::new(frames * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        repeat_end_frame: false,
    }
}

#[test]
fn cross_dissolve_frame_matches_hand_computed_blend() {
    bounded_case("cross_dissolve", || {
        let cancel = CancellationToken::default();
        // V1: A src 0..20 @0, B src b_in..b_in+20 @20 (cut at frame 20);
        // A1: C src 0..10 @0, G src 50..60 @10.
        let cut_seq = |b_in: u64| {
            sequence(vec![
                track(
                    1,
                    TrackKind::Video,
                    0,
                    true,
                    vec![clip(1, 0, 20, 0), clip(2, b_in, b_in + 20, 20)],
                ),
                track(
                    2,
                    TrackKind::Audio,
                    0,
                    true,
                    vec![clip(3, 0, 10, 0), clip(6, 50, 60, 10)],
                ),
            ])
        };
        let snap = snapshot_of(cut_seq(100), 3);
        let seq = &snap.document().sequences[0];
        let ann = TimelineAnnotations::new("seq-main");
        let add = |a: &TimelineAnnotations, s: &StudioSequence, t: Transition| {
            plan_annotation_edit(
                a,
                s,
                a.revision(),
                &AnnotationEdit::AddTransition(t),
                &Extent,
                &cancel,
            )
        };

        // A 10-frame cross dissolve centred on the cut covers frames 15..25.
        let t1 = dissolve("t1", 1, 2, 10);
        let placed = transition_placement(seq, &t1).unwrap();
        assert_eq!(
            (placed.span.start().value(), placed.span.end().value()),
            (15 * F, 25 * F)
        );
        let handles = transition_handles(seq, &t1, &Extent).unwrap();
        assert_eq!(handles.outgoing_required.value(), 5 * F);
        assert_eq!(handles.incoming_available, Some(Ticks::new(100 * F)));
        let p = add(&ann, seq, t1.clone()).unwrap();
        let with_t1 = p.annotations().clone();

        // Frame 18: progress 3/10, A plays source frame 18, B plays its handle at source frame 98.
        let at = |s: &StudioSequence, a: &TimelineAnnotations, frame: u64| {
            transition_frame(
                s,
                a.transitions(),
                &tid(1),
                Ticks::new(frame * F),
                &Extent,
                &cancel,
            )
            .unwrap()
        };
        let f18 = at(seq, &with_t1, 18).expect("inside the dissolve");
        assert_eq!(
            (f18.progress_numerator, f18.progress_denominator),
            (3 * F, 10 * F)
        );
        assert_eq!(
            (f18.outgoing_source.value(), f18.incoming_source.value()),
            (18 * F, 98 * F)
        );
        assert!(!f18.outgoing_held && !f18.incoming_held);
        assert_eq!(at(seq, &with_t1, 14), None);
        assert_eq!(at(seq, &with_t1, 25), None);

        // Rendered through composite's scene path on the CPU backend vs the hand blend
        // (1 - p) * out + p * in.
        let out = render::Frame::solid("out", [0.8, 0.2, 0.1]);
        let inc = render::Frame::solid("in", [0.1, 0.4, 0.9]);
        let close = |got: [f32; 4], want: [f32; 4]| {
            assert!(
                got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 2e-6),
                "{got:?} vs {want:?}"
            );
        };
        close(
            render::dissolve(&out, &inc, f18.incoming_weight()),
            [0.59, 0.26, 0.34, 1.0],
        );
        let f15 = at(seq, &with_t1, 15).unwrap();
        close(
            render::dissolve(&out, &inc, f15.incoming_weight()),
            [0.8, 0.2, 0.1, 1.0],
        );
        let f24 = at(seq, &with_t1, 24).unwrap();
        close(
            render::dissolve(&out, &inc, f24.incoming_weight()),
            [0.17, 0.38, 0.82, 1.0],
        );

        // Audio crossfade on the audio cut; kind/track and adjacency are enforced.
        let mut fade = dissolve("t2", 3, 6, 4);
        fade.kind = TransitionKind::AudioCrossfade;
        let with_both = add(&with_t1, seq, fade).unwrap().annotations().clone();
        assert_eq!(with_both.transitions().len(), 2);
        let code = |t: Transition| add(&with_t1, seq, t).unwrap_err().code();
        assert_eq!(code(dissolve("x", 3, 6, 4)), "invalid_input");
        assert_eq!(code(dissolve("x", 2, 1, 4)), "not_adjacent");
        assert_eq!(code(dissolve("x", 1, 2, 4)), "overlap");
        assert_eq!(code(dissolve("t1", 3, 6, 4)), "duplicate_id");
        let odd = Transition {
            duration: Ticks::new(10 * F + 1),
            ..dissolve("x", 1, 2, 0)
        };
        assert_eq!(code(odd), "not_frame_aligned");
        let long = Transition {
            alignment: TransitionAlignment::EndAtCut,
            ..dissolve("x", 1, 2, 30)
        };
        assert_eq!(code(long), "outside_range");
        // Hostile ticks: a whole-frame duration near u64::MAX and an i64::MIN centre offset are
        // typed refusals.
        let huge = Transition {
            duration: Ticks::new(u64::MAX / F * F),
            ..dissolve("x", 1, 2, 0)
        };
        assert_eq!(code(huge), "outside_range");
        let wild = Transition {
            alignment: TransitionAlignment::Custom,
            center_offset: i64::MIN,
            ..dissolve("x", 1, 2, 10)
        };
        assert_eq!(code(wild), "outside_range");
        let stray = Transition {
            center_offset: 1,
            ..dissolve("x", 1, 2, 10)
        };
        assert_eq!(code(stray), "invalid_input");
        let many = vec![t1.clone(); MAX_TRANSITIONS + 1];
        assert_eq!(
            TimelineAnnotations::from_parts(seq, 0, vec![], many, &Extent, &cancel).unwrap_err(),
            PulseError::LimitExceeded
        );

        // Short handle: B starts at source frame 2 but the dissolve needs 5 frames before it.
        let short_snap = snapshot_of(cut_seq(2), 3);
        let short = &short_snap.document().sequences[0];
        assert_eq!(
            add(&ann, short, t1.clone()).unwrap_err(),
            PulseError::InsufficientHandle
        );
        assert_eq!(
            transition_handles(short, &t1, &Extent).unwrap().shortfall(),
            (Some(Ticks::new(0)), Some(Ticks::new(3 * F)))
        );
        let held = add(
            &ann,
            short,
            Transition {
                repeat_end_frame: true,
                ..t1.clone()
            },
        )
        .unwrap();
        let f15 = at(short, held.annotations(), 15).unwrap();
        assert_eq!((f15.incoming_source.value(), f15.incoming_held), (0, true));

        // Edits: a ripple trim moves the cut and the dissolve with it; ripple delete drops it.
        let trim_a = TimelineEdit::TrimOut {
            clip_id: cid(1),
            new_duration: Ticks::new(15 * F),
            ripple: true,
        };
        let trim = edit(&snap, &trim_a).unwrap();
        let kept = follow_timeline_edit(&with_t1, &trim, 1, &Extent, &cancel).unwrap();
        assert!(kept.dropped_transitions().is_empty());
        let f13 = at(seq_of(&trim), kept.annotations(), 13).unwrap();
        assert_eq!(
            (
                f13.progress_numerator,
                f13.outgoing_source.value(),
                f13.incoming_source.value()
            ),
            (3 * F, 13 * F, 98 * F)
        );
        let gone = edit(&snap, &TimelineEdit::ExtractClip { clip_id: cid(2) }).unwrap();
        let dropped = follow_timeline_edit(&with_t1, &gone, 1, &Extent, &cancel).unwrap();
        assert_eq!(dropped.dropped_transitions(), &["t1".to_owned()]);
        // Undo of the add removes the record; redo restores it.
        let undo = plan_annotation_revert(&with_t1, seq, 1, p.inverse(), &cancel).unwrap();
        assert!(undo.annotations().transitions().is_empty());
        let redo =
            plan_annotation_revert(undo.annotations(), seq, 2, undo.inverse(), &cancel).unwrap();
        assert_eq!(redo.annotations().transitions(), with_t1.transitions());
    });
}
