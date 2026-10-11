use hsk_studio_accord::{CancellationToken, DomainId, FrameRate, TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::{
    Budget, ClipSource, ContentDigest, DocumentSchema, GeometryUnit, GraphSchema, ProfileBinding,
    Resolution, Resolver, Snapshot, StudioDocument, StudioLayerGraph, TileRef, validate_document,
};
use hsk_studio_interop_media::*;
use std::collections::BTreeMap;

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

fn rate(text: &str) -> FrameRate {
    parse_frame_rate(text).unwrap().0
}

fn import(text: &str, fps: &str) -> Result<TimelineProposal> {
    import_cmx3600(
        text.as_bytes(),
        &ImportRequest::new(rate(fps)),
        &Limits::default(),
        &CancellationToken::default(),
    )
}

fn kind(result: Result<TimelineProposal>) -> (ErrorKind, u32) {
    let e = result.expect_err("import must be refused");
    (e.kind, e.line)
}

const FIXTURE: &str = "TITLE: FIXTURE
FCM: NON-DROP FRAME

001  AX       V     C        01:00:00:00 01:00:02:00 01:00:00:00 01:00:02:00
* FROM CLIP NAME: WIDE
002  AX       V     C        01:00:02:00 01:00:02:00 01:00:02:00 01:00:02:00
002  BL       V     D    025 02:00:00:00 02:00:02:00 01:00:02:00 01:00:04:00
* TO CLIP NAME: CLOSE
003  AX       V     C        03:00:00:00 03:00:04:00 01:00:04:00 01:00:06:00
M2   AX       050.0          03:00:00:00
* FROM CLIP NAME: FAST
004  AX       A     C        01:00:00:00 01:00:02:00 01:00:00:00 01:00:02:00
";

struct Library;
impl Resolver for Library {
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
    fn media(&self, manifest: &str, _: &ContentDigest) -> Resolution {
        if manifest == "manifest-ax" {
            Resolution::Available
        } else {
            Resolution::Absent
        }
    }
}

fn base_snapshot() -> Snapshot {
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: "SDOC-019abcde-0000-7000-8000-000000000001".into(),
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
        sequences: vec![],
    };
    validate_document(
        document,
        Budget::default(),
        &CancellationToken::default(),
        &Library,
    )
    .unwrap()
}

#[test]
fn edl_fixture_ticks_losses_and_folio_sequence() {
    bounded_case("edl_fixture_ticks_losses_and_folio_sequence", || {
        let bytes = FIXTURE.as_bytes().to_vec();
        let p = import_cmx3600(
            &bytes,
            &ImportRequest::new(rate("25")),
            &Limits::default(),
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(bytes, FIXTURE.as_bytes(), "input bytes unchanged");
        assert_eq!(
            (p.title.as_deref(), p.source_len, p.drop_frame),
            (Some("FIXTURE"), bytes.len() as u64, false)
        );
        assert_eq!(p.clips.len(), 4);
        // 01:00:00:00 at 25 fps is frame 90000 = 914_457_600_000_000 ticks.
        let wide = &p.clips[0];
        assert_eq!(wide.record_in.value(), 914_457_600_000_000);
        assert_eq!(wide.record_out.value(), 90_050 * F);
        assert_eq!(
            (wide.source_in.value(), wide.source_out.value()),
            (90_000 * F, 90_050 * F)
        );
        assert_eq!(
            (wide.clip_name.as_deref(), wide.disposition),
            (Some("WIDE"), Disposition::Editable)
        );
        // The dissolve keeps the incoming clip as a cut; the transition is a typed loss.
        let close = &p.clips[1];
        assert_eq!(
            (close.reel.as_str(), close.clip_name.as_deref()),
            ("BL", Some("CLOSE"))
        );
        assert_eq!(
            (close.record_in.value(), close.record_out.value()),
            (90_050 * F, 90_100 * F)
        );
        assert!(p.losses.iter().any(|l| l.entity == "event 002"
            && l.code == "dissolve_dropped"
            && l.disposition == Disposition::Unsupported));
        assert!(p.losses.iter().any(|l| l.code == "zero_length_cut_dropped"));
        // M2 050.0 on a 25 fps edit is an exact 2/1 speed; the source runs 100 frames in 50.
        let fast = &p.clips[2];
        assert_eq!((fast.speed.numerator, fast.speed.denominator), (2, 1));
        assert_eq!(fast.disposition, Disposition::Translated);
        assert_eq!(
            (
                fast.record_out.value() - fast.record_in.value(),
                fast.source_out.value() - fast.source_in.value()
            ),
            (508_032_000_000, 1_016_064_000_000)
        );
        assert_eq!(
            p.clips[3].lane,
            Lane {
                kind: LaneKind::Audio,
                index: 0
            }
        );
        assert!(p.losses_with(Disposition::Approximate).next().is_none());
        // Into a Folio sequence validated by Folio: BL has no mapped media, so it is offline.
        let reels = BTreeMap::from([(
            "AX".to_owned(),
            ClipSource::Media {
                artifact_manifest_id: "manifest-ax".into(),
                content_digest: ContentDigest {
                    algorithm: "sha256".into(),
                    digest: "ax".into(),
                },
            },
        )]);
        let options = SequenceOptions {
            sequence_id: "seq-edl".into(),
            name: "imported".into(),
            reels: &reels,
        };
        let built = build_sequence(&p, &options, &mut SequentialIds::new(7)).unwrap();
        assert!(built.losses.iter().any(|l| l.entity == "reel BL"
            && l.code == "offline_placeholder"
            && l.disposition == Disposition::Missing));
        let cancel = CancellationToken::default();
        let base = base_snapshot();
        let ctx = FolioContext {
            budget: Budget::default(),
            resolver: &Library,
            cancel: &cancel,
        };
        let proposal = propose_document_revision(&base, 3, built.clone(), &ctx).unwrap();
        assert_eq!(
            (
                proposal.base_revision,
                proposal.target_revision,
                proposal.replaced
            ),
            (3, 4, false)
        );
        assert_eq!(base.document().revision, 3);
        let sequence = &proposal.snapshot.document().sequences[0];
        assert_eq!(sequence.duration.value(), 90_150 * F);
        assert_eq!(sequence.frame_rate.ticks_per_frame, F);
        assert_eq!(sequence.tracks.len(), 2);
        let video = &sequence.tracks[0].clips;
        assert_eq!(video.len(), 3);
        assert_eq!(
            (video[2].speed.numerator, video[2].duration.value()),
            (2, 508_032_000_000)
        );
        assert_eq!(video[2].timeline_start.value(), 90_100 * F);
        assert_eq!(
            proposal.snapshot.resolution_issues().len(),
            1,
            "only the offline BL clip is unresolved"
        );
        // Same proposal and namespace give the same ids; a second proposal replaces by sequence id.
        let again = build_sequence(&p, &options, &mut SequentialIds::new(7)).unwrap();
        assert_eq!(again.sequence, built.sequence);
        let next = propose_document_revision(&proposal.snapshot, 4, again, &ctx).unwrap();
        assert!(next.replaced);
        assert_eq!(next.snapshot.document().sequences.len(), 1);
        let stale = propose_document_revision(&base, 2, built, &ctx).unwrap_err();
        assert_eq!(stale.kind, ErrorKind::StaleRevision);
    });
}

#[test]
fn timecode_rational_and_rates() {
    bounded_case("timecode_rational_and_rates", || {
        let (r25, r2997, r24) = (rate("25"), rate("29.97"), rate("24"));
        assert_eq!(
            timecode_to_frames("01:00:00:00", r25, false).unwrap(),
            90_000
        );
        assert_eq!(
            frames_to_ticks(90_000, r25).unwrap().value(),
            914_457_600_000_000
        );
        assert_eq!(
            timecode_to_frames("01:00:00;00", r2997, true).unwrap(),
            107_892
        );
        assert_eq!(
            timecode_to_frames("00:10:00;00", r2997, true).unwrap(),
            17_982
        );
        assert_eq!(
            timecode_to_frames("00:01:00;02", r2997, true).unwrap(),
            1_800
        );
        assert_eq!(
            timecode_to_frames("01:00:00:00", r24, false).unwrap(),
            86_400
        );
        assert_eq!(timecode_to_frames("00:00:01:00", r24, false).unwrap(), 24);
        let code = |r: Result<u64>| r.unwrap_err().kind;
        assert_eq!(
            code(timecode_to_frames("00:01:00;00", r2997, true)),
            ErrorKind::DroppedTimecode
        );
        assert_eq!(
            code(timecode_to_frames("00:01:00;01", r2997, true)),
            ErrorKind::DroppedTimecode
        );
        assert_eq!(
            code(timecode_to_frames("00:01:00:00", r25, true)),
            ErrorKind::DropFrameUnsupported
        );
        assert_eq!(
            code(timecode_to_frames("00:01:00;00", r25, false)),
            ErrorKind::DropMismatch
        );
        assert_eq!(
            code(timecode_to_frames("00:00:00:25", r25, false)),
            ErrorKind::InvalidTimecode
        );
        assert_eq!(
            code(timecode_to_frames("00:60:00:00", r25, false)),
            ErrorKind::InvalidTimecode
        );
        assert_eq!(
            code(timecode_to_frames("1:2:3", r25, false)),
            ErrorKind::InvalidTimecode
        );
        // 12.5 fps has no integer timecode base.
        assert_eq!(
            code(timecode_to_frames("00:00:00:00", rate("25/2"), false)),
            ErrorKind::TimecodeUnsupported
        );
        assert_eq!(
            frames_to_timecode(107_892, r2997, true).unwrap(),
            "01:00:00;00"
        );
        assert_eq!(
            frames_to_timecode(1_800, r2997, true).unwrap(),
            "00:01:00;02"
        );
        assert_eq!(
            frames_to_timecode(90_000, r25, false).unwrap(),
            "01:00:00:00"
        );
        // Rational time: exact, or rounded half away from zero with the rule named.
        let exact = (Ticks::new(508_032_000_000), Exactness::Exact);
        assert_eq!(rational_to_ticks(48, 24, 1).unwrap(), exact);
        assert_eq!(
            rational_to_ticks(1, 24_000, 1001).unwrap(),
            (Ticks::new(10_594_584_000), Exactness::Exact)
        );
        let half_away = Exactness::Approximate(RoundingRule::HalfAwayFromZero);
        assert_eq!(
            rational_to_ticks(1, 13, 1).unwrap(),
            (Ticks::new(19_539_692_308), half_away)
        );
        assert_eq!(
            rational_to_ticks(1, 11, 1).unwrap(),
            (Ticks::new(23_092_363_636), half_away)
        );
        let rt = |v, n, d| rational_to_ticks(v, n, d).unwrap_err().kind;
        assert_eq!(rt(1, 0, 1), ErrorKind::InvalidRational);
        assert_eq!(rt(1, 24, 0), ErrorKind::InvalidRational);
        assert_eq!(rt(-1, 24, 1), ErrorKind::NegativeTime);
        assert_eq!(rt(i64::MAX, 1, u32::MAX), ErrorKind::TimecodeOverflow);
        assert_eq!(rt(i64::MAX, 1, 1), ErrorKind::TimecodeOverflow);
        // Frame-rate spellings and legacy normalization (STU-VID-013a).
        let spelled = |text: &str| parse_frame_rate(text).map(|(r, s)| (r.ticks_per_frame(), s));
        assert_eq!(spelled("25").unwrap(), (F, RateSpelling::Exact));
        assert_eq!(
            spelled("24000/1001").unwrap(),
            (10_594_584_000, RateSpelling::Exact)
        );
        assert_eq!(
            spelled("29.97").unwrap(),
            (8_475_667_200, RateSpelling::DecimalAlias)
        );
        assert_eq!(
            spelled("23.976").unwrap(),
            (10_594_584_000, RateSpelling::DecimalAlias)
        );
        assert_eq!(
            spelled("59.94").unwrap(),
            (4_237_833_600, RateSpelling::DecimalAlias)
        );
        assert_eq!(
            spelled("30000/1002").unwrap_err().kind,
            ErrorKind::UnsupportedFrameRate
        );
        for bad in ["abc", "0/1", "25.5", "", "1/0"] {
            assert_eq!(
                spelled(bad).unwrap_err().kind,
                ErrorKind::InvalidFrameRate,
                "{bad:?}"
            );
        }
        let legacy = |t| frame_rate_from_ticks(t).map(|(r, e)| (r.ticks_per_frame(), e));
        assert_eq!(
            legacy(10_594_594_594).unwrap(),
            (10_594_584_000, Exactness::NormalizedLegacy)
        );
        assert_eq!(
            legacy(8_475_675_675).unwrap(),
            (8_475_667_200, Exactness::NormalizedLegacy)
        );
        assert_eq!(legacy(F).unwrap(), (F, Exactness::Exact));
        assert_eq!(legacy(7).unwrap_err().kind, ErrorKind::UnsupportedFrameRate);
        assert_eq!(fps_fraction(r2997), (30_000, 1001));
        assert_eq!(TICKS_PER_SECOND / 25, F);
        assert_eq!(
            ticks_to_frames_exact(Ticks::new(F + 1), r25)
                .unwrap_err()
                .kind,
            ErrorKind::NotFrameAligned
        );
    });
}

fn edl(events: &str) -> String {
    format!("FCM: NON-DROP FRAME\n{events}")
}
const E1: &str = "001  AX       V     C        01:00:00:00 01:00:02:00 01:00:00:00 01:00:02:00\n";

#[test]
fn edl_negative_paths_and_dispositions() {
    bounded_case("edl_negative_paths_and_dispositions", || {
        // Every refusal is typed and carries the 1-based source line.
        let dup = edl(&format!("{E1}{E1}"));
        assert_eq!(kind(import(&dup, "25")), (ErrorKind::DuplicateEvent, 3));
        let back = edl(&format!(
            "002  AX       V     C        01:00:00:00 01:00:02:00 01:00:00:00 01:00:02:00\n{E1}"
        ));
        assert_eq!(kind(import(&back, "25")), (ErrorKind::NonMonotonicEvent, 3));
        let reversed =
            edl("001  AX       V     C        01:00:02:00 01:00:00:00 01:00:00:00 01:00:02:00\n");
        assert_eq!(kind(import(&reversed, "25")), (ErrorKind::InvalidRange, 2));
        let backwards =
            edl("001  AX       V     C        01:00:00:00 01:00:02:00 01:00:02:00 01:00:00:00\n");
        assert_eq!(kind(import(&backwards, "25")), (ErrorKind::InvalidRange, 2));
        let overlap = edl(&format!(
            "{E1}002  AX       V     C        01:00:05:00 01:00:07:00 01:00:01:00 01:00:03:00\n"
        ));
        assert_eq!(
            kind(import(&overlap, "25")),
            (ErrorKind::OverlappingEvents, 3)
        );
        let mismatch =
            edl("001  AX       V     C        01:00:00:00 01:00:05:00 01:00:00:00 01:00:02:00\n");
        assert_eq!(
            kind(import(&mismatch, "25")),
            (ErrorKind::LengthMismatch, 2)
        );
        assert_eq!(kind(import(&edl(""), "25")), (ErrorKind::NoEvents, 0));
        assert_eq!(
            kind(import(&edl("FOO 1 2 3\n"), "25")).0,
            ErrorKind::NoEvents
        );
        let junk = edl("001  AX       V\n");
        assert_eq!(kind(import(&junk, "25")), (ErrorKind::InvalidLine, 2));
        // Timecode rules.
        let drop_file = "FCM: DROP FRAME\n001  AX       V     C        00:01:00;00 00:01:01;00 00:01:00;00 00:01:01;00\n";
        assert_eq!(
            kind(import(drop_file, "29.97")),
            (ErrorKind::DroppedTimecode, 2)
        );
        assert_eq!(
            kind(import(drop_file, "25")),
            (ErrorKind::DropFrameUnsupported, 2)
        );
        let semicolon =
            edl("001  AX       V     C        00:00:00;00 00:00:01;00 00:00:00;00 00:00:01;00\n");
        assert_eq!(
            kind(import(&semicolon, "29.97")),
            (ErrorKind::DropMismatch, 2)
        );
        // Limits, encoding, cancellation.
        let request = ImportRequest::new(rate("25"));
        let cancel = CancellationToken::default();
        let run = |bytes: &[u8], limits: &Limits| {
            import_cmx3600(bytes, &request, limits, &cancel)
                .map(|_| ())
                .unwrap_err()
        };
        let small = Limits {
            max_bytes: 10,
            ..Limits::default()
        };
        assert_eq!(run(FIXTURE.as_bytes(), &small).kind, ErrorKind::InputLimit);
        let one_event = Limits {
            max_events: 1,
            ..Limits::default()
        };
        assert_eq!(
            run(edl(&format!("{E1}{E1}")).as_bytes(), &one_event).kind,
            ErrorKind::EventLimit
        );
        let narrow = Limits {
            max_line_len: 20,
            ..Limits::default()
        };
        assert_eq!(
            run(edl(E1).as_bytes(), &narrow),
            InteropError::at(ErrorKind::LineLimit, 2)
        );
        assert_eq!(
            run(&[0x46, 0xFF, 0xFE, 0x0A], &Limits::default()).kind,
            ErrorKind::InvalidUtf8
        );
        let canceled = CancellationToken::default();
        canceled.cancel();
        let r = import_cmx3600(FIXTURE.as_bytes(), &request, &Limits::default(), &canceled);
        assert_eq!(r.unwrap_err().kind, ErrorKind::Canceled);
        // Origin shifts record time, never source time; a record before the origin is refused.
        let mut shifted = ImportRequest::new(rate("25"));
        shifted.record_origin_frames = 90_000;
        let p = import_cmx3600(edl(E1).as_bytes(), &shifted, &Limits::default(), &cancel).unwrap();
        assert_eq!(
            (p.clips[0].record_in.value(), p.clips[0].record_out.value()),
            (0, 50 * F)
        );
        assert_eq!(p.clips[0].source_in.value(), 90_000 * F);
        shifted.record_origin_frames = 90_001;
        let r = import_cmx3600(edl(E1).as_bytes(), &shifted, &Limits::default(), &cancel);
        assert_eq!(r.unwrap_err().kind, ErrorKind::BeforeOrigin);
        // Dispositions: unknown reel, unknown track, freeze frame, M2 disagreeing with spans,
        // linked B event, collapsed stereo pair, other directives.
        let known = ["AX"];
        let mut with_known = ImportRequest::new(rate("25"));
        with_known.known_reels = Some(&known);
        let text = edl(&format!(
            "{E1}002  XX       V     C        01:00:00:00 01:00:02:00 01:00:02:00 01:00:04:00\n\
             003  AX       V9    C        01:00:00:00 01:00:02:00 01:00:04:00 01:00:06:00\n\
             004  AX       V     C        01:00:00:00 01:00:02:00 01:00:06:00 01:00:08:00\nM2   AX       000.0          01:00:00:00\n\
             005  AX       V     C        01:00:00:00 01:00:04:00 01:00:08:00 01:00:10:00\nM2   AX       030.0          01:00:00:00\n\
             006  AX       B     C        01:00:00:00 01:00:02:00 01:00:10:00 01:00:12:00\n\
             007  AX       AA    C        01:00:00:00 01:00:02:00 01:00:12:00 01:00:14:00\n\
             LOC: 01:00:00:00 RED marker\n"
        ));
        let p = import_cmx3600(text.as_bytes(), &with_known, &Limits::default(), &cancel).unwrap();
        let has = |entity: &str, code: &str| {
            p.losses
                .iter()
                .any(|l| l.entity == entity && l.code == code)
        };
        assert!(has("event 002", "reel_unknown"));
        assert!(has("event 003", "track_unsupported"));
        assert!(has("event 004", "freeze_frame_dropped"));
        assert!(has("event 005", "m2_speed_mismatch_spans_used"));
        assert!(has("event 007", "channel_pair_collapsed"));
        assert!(has("directive LOC:", "directive_dropped"));
        assert_eq!(
            p.clips.iter().filter(|c| c.event == 6).count(),
            2,
            "B event = video + audio"
        );
        let linked: Vec<_> = p.clips.iter().filter(|c| c.link == Some(6)).collect();
        assert_eq!(linked.len(), 2);
        let m2 = p.clips.iter().find(|c| c.event == 5).unwrap();
        assert_eq!(
            (m2.speed.numerator, m2.speed.denominator),
            (2, 1),
            "speed comes from the spans"
        );
        assert!(p.clips.iter().all(|c| c.event != 3 && c.event != 4));
        let pair = p.clips.iter().find(|c| c.event == 7).unwrap();
        assert_eq!(pair.disposition, Disposition::Approximate);
        // Reverse speed from a negative M2.
        let rev = edl(
            "001  AX       V     C        01:00:00:00 01:00:02:00 01:00:00:00 01:00:02:00\nM2   AX       -025.0         01:00:00:00\n",
        );
        let p = import(&rev, "25").unwrap();
        assert_eq!(
            (p.clips[0].speed.numerator, p.clips[0].speed.denominator),
            (-1, 1)
        );
    });
}

#[test]
fn round_trip_export_and_native_formats_blocked() {
    bounded_case("round_trip_export_and_native_formats_blocked", || {
        // Two-roundtrip mapping: import -> export -> import keeps the exportable subset, and the
        // export text is a fixed point.
        let first = import(FIXTURE, "25").unwrap();
        let text = export_cmx3600(&first).unwrap();
        assert!(text.starts_with(
            "TITLE: FIXTURE\nFCM: NON-DROP FRAME\n\n001  AX       V     C        01:00:00:00"
        ));
        assert!(text.contains("M2   AX       050.0   03:00:00:00\n"));
        let second = import(&text, "25").unwrap();
        let key = |p: &TimelineProposal| {
            let mut v = p.clips.clone();
            v.sort_by_key(|c| (c.record_in, c.lane));
            v
        };
        assert_eq!(key(&first).len(), key(&second).len());
        for (a, b) in key(&first).iter().zip(key(&second).iter()) {
            assert!(a.same_mapping(b), "{a:?} != {b:?}");
        }
        assert_eq!(export_cmx3600(&second).unwrap(), text);
        // Drop-frame files keep their labels and rate: 00:01:00;02 is frame 1800.
        let df = "TITLE: DF\nFCM: DROP FRAME\n\n001  A1       V     C        00:01:00;02 00:01:02;02 00:01:00;02 00:01:02;02\n";
        let p = import(df, "29.97").unwrap();
        assert!(p.drop_frame);
        assert_eq!(p.clips[0].record_in.value(), 1_800 * 8_475_667_200);
        assert_eq!(p.clips[0].record_out.value(), 1_860 * 8_475_667_200);
        assert_eq!(export_cmx3600(&p).unwrap(), df);
        // Video lanes above V1 have no CMX3600 spelling: refused, not silently merged.
        let mut multi = first.clone();
        multi.clips[0].lane.index = 1;
        assert_eq!(
            export_cmx3600(&multi).unwrap_err().kind,
            ErrorKind::TrackUnsupported
        );
        // A speed that is not an exact decimal fps value cannot be written back.
        let mut odd = first.clone();
        odd.clips[2].speed = hsk_studio_folio::Speed {
            numerator: 1,
            denominator: 3,
        };
        assert_eq!(
            export_cmx3600(&odd).unwrap_err().kind,
            ErrorKind::SpeedNotRepresentable
        );
        // PRPROJ / AEP / SESX: magic and structure only, editable import stays blocked loudly.
        let gz = [0x1F, 0x8B, 0x08, 0x00, 1, 2, 3, 4];
        assert_eq!(sniff_native(&gz).format, NativeFormat::Prproj);
        assert_eq!(
            sniff_native(b"RIFX\0\0\0\x10Egg!").format,
            NativeFormat::AepBinary
        );
        let sesx = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- c -->\n<sesx version=\"1.9\"></sesx>";
        assert_eq!(sniff_native(sesx).format, NativeFormat::Sesx);
        let aepx = b"<?xml version=\"1.0\"?><AfterEffectsProject bits=\"32\"/>";
        assert_eq!(sniff_native(aepx).format, NativeFormat::Aepx);
        assert_eq!(sniff_native(b"<html></html>").format, NativeFormat::Unknown);
        assert_eq!(sniff_native(&[1, 2, 3]).format, NativeFormat::Unknown);
        assert_eq!(sniff_native(&[]).evidence, "too_short");
        assert!(sniff_native(&gz).evidence.ends_with("unverified"));
        let blocked = import_native(sesx);
        assert_eq!(
            (blocked.format, blocked.len),
            (NativeFormat::Sesx, sesx.len() as u64)
        );
        assert_eq!(blocked.losses.len(), NATIVE_ENTITY_CLASSES.len());
        assert!(
            blocked.losses.iter().all(
                |l| l.disposition == Disposition::Missing && l.code == "native_decoder_missing"
            )
        );
        let error = InteropError::from(blocked);
        assert_eq!(error.kind, ErrorKind::NativeDecoderMissing);
        assert_eq!(
            error.failure_code(),
            Some(hsk_studio_observe::FailureCode::Unsupported)
        );
        assert_eq!(InteropError::new(ErrorKind::Canceled).failure_code(), None);
        // The descriptor is balanced JSON naming the owner.
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for ch in DESCRIPTOR.chars() {
            match (in_str, esc, ch) {
                (true, true, _) => esc = false,
                (true, false, '\\') => esc = true,
                (true, false, '"') => in_str = false,
                (true, false, c) => assert!(!c.is_control()),
                (false, _, '"') => in_str = true,
                (false, _, '{' | '[') => depth += 1,
                (false, _, '}' | ']') => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert!(
            depth == 0
                && !in_str
                && DESCRIPTOR.contains("\"owner\":\"STUDIO-MODULE-INTEROP_MEDIA\"")
        );
        let _ = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    });
}
