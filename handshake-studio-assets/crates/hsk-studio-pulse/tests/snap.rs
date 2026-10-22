//! Lane SMB-TIMELINE-UI-1 (parity row PR-12, STU-VID-035): snap targets of a hand-built sequence,
//! nearest-target choice within a distance, exclusion of the dragged clip, next / previous edit,
//! and hostile counts refused before collection. Expected tick values are hand-computed.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

fn media() -> ClipSource {
    ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}

/// Clip `n`: source frames `[si, so)` at timeline frame `start` (unit speed).
fn clip(n: u32, si: u64, so: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: id("SCLP", n),
        source_ref: media(),
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

fn track(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id("STRK", n),
        kind,
        index,
        name: format!("T{n}"),
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

fn marker(id: &str, start: u64, frames: u64) -> StudioMarker {
    StudioMarker { marker_id: id.into(), name: "m".into(), comment: String::new(), color: LabelColor::Rose, start: Ticks::new(start * F), duration: Ticks::new(frames * F), role: None }
}

/// V1: clip 1 at frames [10, 30) (a clip marker at source frame 5 -> timeline 15), clip 2 at
/// [30, 50). A1: clip 3 at [12, 40). Sequence markers at frame 20 (point) and [60, 64) (span).
/// Work area [8, 70).
fn sequence() -> StudioSequence {
    let mut c1 = clip(1, 0, 20, 10);
    c1.markers.push(marker("mk-clip", 5, 0));
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![track(1, TrackKind::Video, 0, vec![c1, clip(2, 100, 120, 30)]), track(2, TrackKind::Audio, 0, vec![clip(3, 0, 28, 12)])],
        markers: vec![marker("mk-point", 20, 0), marker("mk-span", 60, 4)],
        work_area: Some(WorkArea { start: Ticks::new(8 * F), end: Ticks::new(70 * F) }),
    }
}

fn at(frames: u64) -> Ticks {
    Ticks::new(frames * F)
}

#[test]
fn snap_targets_list_edit_points_markers_work_area_and_playhead() {
    let token = CancellationToken::default();
    let seq = sequence();
    let targets = snap_targets(&seq, Some(at(33)), &[], &token).unwrap();
    let got: Vec<(u64, &str, Option<String>)> = targets.iter().map(|t| (t.at.value() / F, t.kind.key(), t.owner.clone())).collect();
    let c = |n| Some(id("SCLP", n));
    assert_eq!(
        got,
        vec![
            (0, "sequence_start", None),
            (8, "work_area", None),
            (10, "edit_point", c(1)),
            (12, "edit_point", c(3)),
            (15, "marker", Some("mk-clip".to_owned())),
            (20, "marker", Some("mk-point".to_owned())),
            // Clip 1's end and clip 2's start meet at frame 30: one target, the first owner.
            (30, "edit_point", c(1)),
            (33, "playhead", None),
            (40, "edit_point", c(3)),
            (50, "edit_point", c(2)),
            (60, "marker", Some("mk-span".to_owned())),
            (64, "marker", Some("mk-span".to_owned())),
            (70, "work_area", None),
        ]
    );
    // Excluding the dragged clip drops its edges and its clip marker, nothing else.
    let c1 = id("SCLP", 1);
    let without = snap_targets(&seq, None, &[c1.as_str()], &token).unwrap();
    let frames: Vec<u64> = without.iter().map(|t| t.at.value() / F).collect();
    assert_eq!(frames, vec![0, 8, 12, 20, 30, 40, 50, 60, 64, 70]);
    assert_eq!(without.iter().find(|t| t.at == at(30)).unwrap().owner.as_deref(), Some(id("SCLP", 2).as_str()), "frame 30 is now clip 2's start");
}

#[test]
fn nearest_snap_picks_the_closest_target_within_the_distance() {
    let token = CancellationToken::default();
    let targets = snap_targets(&sequence(), None, &[], &token).unwrap();
    let d = Ticks::new(2 * F);
    // 31 frames: frame 30 is one frame away, nothing else within two.
    assert_eq!(nearest_snap(&targets, at(31), d).map(|t| t.at), Some(at(30)));
    // Exactly at the distance is inside (inclusive).
    assert_eq!(nearest_snap(&targets, at(52), d).map(|t| t.at), Some(at(50)));
    // Out of reach.
    assert!(nearest_snap(&targets, at(55), d).is_none());
    // Halfway between frame 10 and frame 12 (frame 11): equal distance, the earlier tick wins.
    assert_eq!(nearest_snap(&targets, at(11), d).map(|t| t.at), Some(at(10)));
    // Sub-frame position snaps to the nearer edit point: 15 frames + 1 tick -> marker at 15.
    assert_eq!(nearest_snap(&targets, Ticks::new(15 * F + 1), d).map(|t| (t.at, t.kind)), Some((at(15), SnapKind::Marker)));
    // Distance 0 snaps only on an exact hit; huge distances never overflow.
    assert_eq!(nearest_snap(&targets, at(20), Ticks::new(0)).map(|t| t.kind), Some(SnapKind::Marker));
    assert!(nearest_snap(&targets, at(21), Ticks::new(0)).is_none());
    assert_eq!(nearest_snap(&targets, Ticks::new(u64::MAX), Ticks::new(u64::MAX)).map(|t| t.at), Some(at(70)));
    assert!(nearest_snap(&[], at(1), d).is_none());
    // Next / previous edit point from the playhead.
    assert_eq!(adjacent_edit(&targets, at(12), true), Some(at(30)));
    assert_eq!(adjacent_edit(&targets, at(12), false), Some(at(10)));
    assert_eq!(adjacent_edit(&targets, at(0), false), None);
    assert_eq!(adjacent_edit(&targets, at(50), true), None);
}

#[test]
fn hostile_sequences_are_refused_or_bounded() {
    let token = CancellationToken::default();
    // Overflowing clip end: typed, no panic.
    let mut seq = sequence();
    seq.tracks[0].clips[1].timeline_start = Ticks::new(u64::MAX - F);
    assert_eq!(snap_targets(&seq, None, &[], &token).unwrap_err(), PulseError::Overflow);
    // More markers than Folio admits: refused before any target is collected.
    let mut seq = sequence();
    seq.markers = (0..=MAX_MARKERS_PER_SEQUENCE as u64).map(|i| marker(&format!("m{i}"), i, 0)).collect();
    assert_eq!(snap_targets(&seq, None, &[], &token).unwrap_err(), PulseError::LimitExceeded);
    // Overlapping and zero-length clips (never valid in Folio) still yield bounded, sorted targets.
    let mut seq = sequence();
    let mut zero = clip(9, 5, 5, 31);
    zero.duration = Ticks::new(0);
    seq.tracks[0].clips.push(zero);
    seq.tracks[0].clips.push(clip(10, 0, 40, 0));
    let targets = snap_targets(&seq, None, &[], &token).unwrap();
    assert!(targets.windows(2).all(|w| (w[0].at, w[0].kind) < (w[1].at, w[1].kind)), "sorted, deduplicated");
    assert!(targets.iter().any(|t| t.at == at(31)));
    // A canceled token produces nothing.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(snap_targets(&sequence(), None, &[], &canceled).unwrap_err(), PulseError::Canceled);
}
