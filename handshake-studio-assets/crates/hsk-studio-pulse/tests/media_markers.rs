//! Lane SMB-PR-MEDIA-MARKERS-1, brief item 4 (STU-VID-034: "A PROJECT-ITEM marker lives on a media item
//! and appears on every clip cut from it"). Oracles: positions worked out by hand in frames of 25 fps
//! (F ticks), and `clip_source_at`, the function playback reads, which the read must agree with at every
//! position it reports. Vendor parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

const F: u64 = 10_160_640_000;

fn token() -> CancellationToken {
    CancellationToken::default()
}
fn t(frames: u64) -> Ticks {
    Ticks::new(frames * F)
}
fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}
fn digest(name: &str) -> ContentDigest {
    ContentDigest { algorithm: "sha256".into(), digest: name.into() }
}
fn media_source(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: digest(name), asset_id: None }
}

/// Clip `n` cut from `item`: source frames `[si, so)` at timeline frame `start`, speed `num/den`.
fn clip_at(n: u32, item: &str, si: u64, so: u64, start: u64, num: i64, den: u64) -> StudioClip {
    let duration = (u128::from((so - si) * F) * u128::from(den) / u128::from(num.unsigned_abs())) as u64;
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: media_source(item),
        source_in: t(si),
        source_out: t(so),
        timeline_start: t(start),
        duration: Ticks::new(duration),
        speed: Speed { numerator: num, denominator: den },
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}
fn clip(n: u32, item: &str, si: u64, so: u64, start: u64) -> StudioClip {
    clip_at(n, item, si, so, start, 1, 1)
}
/// A point at `frame`, or a span of `frames` frames.
fn marker(id: &str, frame: u64, frames: u64) -> StudioMarker {
    StudioMarker { marker_id: id.into(), name: format!("name of {id}"), comment: String::new(), color: LabelColor::Rose, start: t(frame), duration: t(frames), role: None }
}
fn records(items: Vec<(&str, Vec<StudioMarker>)>) -> StudioMediaMarkerRecords {
    let mut r = StudioMediaMarkerRecords::new("SDOC-019abcde-0000-7000-8000-000000000001");
    r.media_items = items.into_iter().map(|(name, markers)| StudioMediaMarkerEntry { content_digest: digest(name), markers }).collect();
    r
}
fn sequence(clips: Vec<StudioClip>) -> StudioSequence {
    let track = |n: u32, clips: Vec<StudioClip>| StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind: TrackKind::Video,
        index: n - 1,
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
    };
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks: vec![track(1, clips)],
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}

/// `(scope, marker id, start, end)` in ticks.
type Seen = (AppearingMarkerScope, String, u64, u64);
fn seen(appearing: &[AppearingMarker]) -> Vec<Seen> {
    appearing.iter().map(|a| (a.scope, a.marker.marker_id.clone(), a.span.start().value(), a.span.end().value())).collect()
}
use AppearingMarkerScope::{Clip as OnClip, MediaItem as OnItem, Sequence as OnSequence};
fn at(scope: AppearingMarkerScope, id: &str, from: u64, to: u64) -> Seen {
    (scope, id.to_owned(), from, to)
}

/// The table of brief item 4. What it is, the clip, the item's markers, what appears (ticks), and how
/// many of the item's markers are hidden.
#[test]
fn the_media_items_markers_appear_on_a_clip_through_the_mapping_playback_uses_mt_33736() {
    let item = "item-a";
    let reversed = clip_at(1, item, 10, 20, 100, -1, 1);
    let double = clip_at(1, item, 10, 30, 100, 2, 1);
    let half = clip_at(1, item, 10, 20, 100, 1, 2);
    let reverse_double = clip_at(1, item, 10, 30, 100, -2, 1);
    let third = clip_at(1, item, 0, 3, 0, 3, 1);
    let cases: Vec<(&str, StudioClip, Vec<StudioMarker>, Vec<Seen>, usize)> = vec![
        ("a point inside the window", clip(1, item, 10, 20, 100), vec![marker("m", 15, 0)], vec![at(OnItem, "m", 105 * F, 105 * F)], 0),
        ("a span inside the window", clip(1, item, 10, 20, 100), vec![marker("m", 12, 3)], vec![at(OnItem, "m", 102 * F, 105 * F)], 0),
        ("at the first source tick", clip(1, item, 10, 20, 100), vec![marker("m", 10, 0)], vec![at(OnItem, "m", 100 * F, 100 * F)], 0),
        ("a point at the out point: the window is half open", clip(1, item, 10, 20, 100), vec![marker("m", 20, 0)], vec![], 1),
        ("a point one tick before the in point", clip(1, item, 10, 20, 100), vec![StudioMarker { start: Ticks::new(10 * F - 1), ..marker("m", 0, 0) }], vec![], 1),
        ("a span ending at the in point", clip(1, item, 10, 20, 100), vec![marker("m", 5, 5)], vec![], 1),
        ("a span starting at the out point", clip(1, item, 10, 20, 100), vec![marker("m", 20, 5)], vec![], 1),
        ("a span across the in point is clipped", clip(1, item, 10, 20, 100), vec![marker("m", 8, 4)], vec![at(OnItem, "m", 100 * F, 102 * F)], 0),
        ("a span across the out point is clipped", clip(1, item, 10, 20, 100), vec![marker("m", 18, 4)], vec![at(OnItem, "m", 108 * F, 110 * F)], 0),
        ("a span over the whole window", clip(1, item, 10, 20, 100), vec![marker("m", 0, 40)], vec![at(OnItem, "m", 100 * F, 110 * F)], 0),
        ("two markers, one hidden", clip(1, item, 10, 20, 100), vec![marker("in", 11, 0), marker("out", 21, 0)], vec![at(OnItem, "in", 101 * F, 101 * F)], 1),
        ("speed 2: twice as fast, half the distance", double.clone(), vec![marker("m", 20, 4)], vec![at(OnItem, "m", 105 * F, 107 * F)], 0),
        ("speed 1/2: half as fast, twice the distance", half.clone(), vec![marker("m", 12, 1)], vec![at(OnItem, "m", 104 * F, 106 * F)], 0),
        ("reverse: the window plays backwards", reversed.clone(), vec![marker("m", 12, 2)], vec![at(OnItem, "m", 106 * F, 108 * F)], 0),
        ("reverse: a point", reversed.clone(), vec![marker("m", 12, 0)], vec![at(OnItem, "m", 108 * F, 108 * F)], 0),
        ("reverse: the in point plays last, at the clip's end tick", reversed.clone(), vec![marker("m", 10, 0)], vec![at(OnItem, "m", 110 * F, 110 * F)], 0),
        ("reverse: a span across the out point is clipped", reversed.clone(), vec![marker("m", 18, 4)], vec![at(OnItem, "m", 100 * F, 102 * F)], 0),
        ("reverse: outside the window", reversed, vec![marker("m", 20, 1)], vec![], 1),
        ("reverse at speed 2", reverse_double, vec![marker("m", 14, 2)], vec![at(OnItem, "m", 107 * F, 108 * F)], 0),
        ("between ticks a position rounds toward the clip start", third, vec![StudioMarker { start: Ticks::new(2), duration: Ticks::new(0), ..marker("m", 0, 0) }], vec![at(OnItem, "m", 0, 0)], 0),
    ];
    for (what, c, held, expected, hidden) in cases {
        let total = held.len();
        let r = records(vec![(item, held), ("item-other", vec![marker("other", 15, 0)])]);
        let answer = clip_appearing_markers(&c, Some(&r), &token()).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        assert_eq!(seen(&answer.appearing), expected, "{what}");
        assert_eq!((answer.hidden_media_markers, answer.hidden_clip_markers), (hidden, 0), "{what}");
        assert_eq!(answer.appearing.len() + hidden, total, "{what}: nothing is lost and nothing invented");
        assert_eq!(answer.media_item, ClipMediaItem::Media(digest(item)), "{what}");
        assert_eq!(answer.clip_id, c.clip_id, "{what}");
        for a in &answer.appearing {
            assert_eq!(a.clip_id.as_deref(), Some(c.clip_id.as_str()), "{what}");
            assert_eq!(a.marker, *r.entry(&digest(item)).unwrap().markers.iter().find(|m| m.marker_id == a.marker.marker_id).unwrap(), "{what}: the stored record, in its own ticks");
        }
    }
}

/// `clip_source_at` is the function playback reads. At the start a marker reports, the clip plays that
/// source position, up to the rounding of a position between ticks.
#[test]
fn every_reported_position_agrees_with_clip_source_at() {
    let held = [marker("m-a", 11, 0), marker("m-b", 13, 2), marker("m-c", 17, 0), marker("m-d", 19, 1)];
    for (what, c) in [
        ("unit", clip(1, "item-a", 10, 20, 100)),
        ("speed 2", clip_at(1, "item-a", 10, 20, 100, 2, 1)),
        ("speed 3/2", clip_at(1, "item-a", 10, 22, 100, 3, 2)),
        ("speed 1/4", clip_at(1, "item-a", 10, 20, 100, 1, 4)),
        ("reverse", clip_at(1, "item-a", 10, 20, 100, -1, 1)),
        ("reverse at 3/2", clip_at(1, "item-a", 10, 22, 100, -3, 2)),
    ] {
        let r = records(vec![("item-a", held.to_vec())]);
        let answer = clip_appearing_markers(&c, Some(&r), &token()).unwrap();
        assert_eq!(answer.appearing.len(), held.len(), "{what}");
        let step = c.speed.numerator.unsigned_abs().div_ceil(c.speed.denominator);
        for a in &answer.appearing {
            let (source, past) = clip_source_at(&c, a.span.start(), &UnknownExtents, false).unwrap();
            assert!(!past, "{what}");
            // Reversed, the span's start is the marker's end; forward, its start.
            let m = if c.speed.is_reversed() { a.marker.start.checked_add(a.marker.duration).unwrap() } else { a.marker.start };
            assert!(source.value().abs_diff(m.value()) <= step, "{what}: {} plays {} at {}, not {}", a.marker.marker_id, source.value(), a.span.start().value(), m.value());
        }
    }
}

/// A frame-held clip plays one source tick at every timeline tick (`clip_source_at`), so a marker holding
/// that tick appears over the whole clip, and one that does not appears nowhere. Studio's reading.
#[test]
fn a_frame_held_clip_shows_only_the_markers_holding_its_frame() {
    let held_clip = |tick: u64| {
        let mut c = clip(1, "item-a", 10, 20, 100);
        c.retime.frame_hold = Some(ClipFrameHold { source_tick: t(tick), anchor: FrameHoldAnchor::Playhead });
        c
    };
    let whole = (100 * F, 110 * F);
    let cases: Vec<(&str, StudioClip, StudioMarker, Option<(u64, u64)>)> = vec![
        ("a point on the held tick", held_clip(15), marker("m", 15, 0), Some(whole)),
        ("a span holding it", held_clip(15), marker("m", 13, 4), Some(whole)),
        ("a span starting on it", held_clip(15), marker("m", 15, 1), Some(whole)),
        ("a span ending on it: half open", held_clip(15), marker("m", 13, 2), None),
        ("a point elsewhere in the window, which plays no more", held_clip(15), marker("m", 16, 0), None),
        ("a span elsewhere in the window", held_clip(15), marker("m", 16, 2), None),
        ("inside the held frame, past its first tick: no grid here", held_clip(15), StudioMarker { start: Ticks::new(15 * F + 1), ..marker("m", 0, 0) }, None),
        ("a held frame outside the window still plays", held_clip(40), marker("m", 40, 0), Some(whole)),
        ("and a marker in the window does not", held_clip(40), marker("m", 15, 0), None),
    ];
    for (what, c, m, span) in cases {
        let r = records(vec![("item-a", vec![m.clone()])]);
        let answer = clip_appearing_markers(&c, Some(&r), &token()).unwrap();
        match span {
            Some((from, to)) => {
                assert_eq!(seen(&answer.appearing), vec![at(OnItem, "m", from, to)], "{what}");
                assert_eq!(answer.hidden_media_markers, 0, "{what}");
                // What playback shows at both ends of the span is the marker's tick.
                let last = Ticks::new(to - 1);
                for tick in [Ticks::new(from), last] {
                    assert_eq!(clip_source_at(&c, tick, &UnknownExtents, false).unwrap().0, c.retime.frame_hold.unwrap().source_tick, "{what}");
                }
            }
            None => {
                assert!(answer.appearing.is_empty(), "{what}");
                assert_eq!(answer.hidden_media_markers, 1, "{what}");
            }
        }
        // The clip's own marker reads the same way.
        let mut own = c.clone();
        own.markers.push(m);
        let answer = clip_appearing_markers(&own, None, &token()).unwrap();
        assert_eq!((answer.appearing.len(), answer.hidden_clip_markers), if span.is_some() { (1, 0) } else { (0, 1) }, "{what}: on the clip");
    }
}

/// The key is the bytes: every clip cut from them, through whichever handle.
#[test]
fn every_clip_cut_from_the_item_shows_it_whatever_its_handle_mt_33736() {
    let r = records(vec![("item-a", vec![marker("m", 15, 0)])]);
    let mut other_handle = clip(1, "item-a", 10, 20, 0);
    other_handle.source_ref = ClipSource::Media { artifact_manifest_id: "another-manifest".into(), content_digest: digest("item-a"), asset_id: Some("019abcde-0000-7000-8000-000000000009".into()) };
    let mut same_handle_other_bytes = clip(2, "item-b", 10, 20, 0);
    same_handle_other_bytes.source_ref = ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: digest("item-b"), asset_id: None };
    let mut other_algorithm = clip(3, "item-a", 10, 20, 0);
    other_algorithm.source_ref = ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: ContentDigest { algorithm: "sha512".into(), digest: "item-a".into() }, asset_id: None };
    let mut disabled = clip(4, "item-a", 10, 20, 0);
    disabled.enabled = false;
    for (what, c, count) in [
        ("the same bytes through another handle and an asset id", other_handle, 1),
        ("the same handle over other bytes", same_handle_other_bytes, 0),
        ("the same digest under another algorithm", other_algorithm, 0),
        ("a disabled clip is still cut from it", disabled, 1),
    ] {
        assert_eq!(clip_appearing_markers(&c, Some(&r), &token()).unwrap().appearing.len(), count, "{what}");
    }
    // No records stored: nothing of this scope, and that is no error.
    let c = clip(1, "item-a", 10, 20, 0);
    let answer = clip_appearing_markers(&c, None, &token()).unwrap();
    assert!(answer.appearing.is_empty());
    assert_eq!((answer.hidden_clip_markers, answer.hidden_media_markers), (0, 0));
    assert_eq!(answer.media_item, ClipMediaItem::Media(digest("item-a")));
}

/// A clip whose source is a nested sequence or a primitive has no media item: the case is stated, its own
/// markers appear, and no item's marker does, even one naming the same words.
#[test]
fn a_clip_with_no_media_item_says_so_and_shows_its_own_markers() {
    let r = records(vec![("seq-inner", vec![marker("item-marker", 15, 0)])]);
    let mut nested = clip(1, "ignored", 10, 20, 100);
    nested.source_ref = ClipSource::Sequence { sequence_id: "seq-inner".into() };
    nested.markers.push(marker("own", 12, 0));
    let answer = clip_appearing_markers(&nested, Some(&r), &token()).unwrap();
    assert_eq!(answer.media_item, ClipMediaItem::NestedSequence);
    assert_eq!(seen(&answer.appearing), vec![at(OnClip, "own", 102 * F, 102 * F)]);
    assert_eq!((answer.hidden_clip_markers, answer.hidden_media_markers), (0, 0), "no item, so nothing of an item is hidden");

    let mut primitive = nested.clone();
    primitive.source_ref = ClipSource::Primitive { schema_id: "hsk.studio.composition@1".into(), object_id: "item-a".into() };
    assert_eq!(clip_appearing_markers(&primitive, Some(&r), &token()).unwrap().media_item, ClipMediaItem::Primitive);

    let seq = sequence(vec![nested, primitive_after(primitive)]);
    let whole = sequence_appearing_markers(&seq, Some(&r), &token()).unwrap();
    assert_eq!(whole.clips_without_media_item, 2);
    assert_eq!(whole.appearing.len(), 2);
    assert!(whole.appearing.iter().all(|a| a.scope == OnClip));
}
fn primitive_after(mut c: StudioClip) -> StudioClip {
    c.clip_id = cid(2);
    c.timeline_start = t(200);
    c.markers[0].marker_id = "own-2".into();
    c
}

/// The three scopes side by side: each labelled, none the same object, and each moving as the clause says.
#[test]
fn the_three_scopes_appear_labelled_and_move_as_the_clause_says_mt_33736() {
    let mut a = clip(1, "item-a", 10, 20, 0);
    a.markers.push(marker("on-a", 11, 0));
    let mut b = clip(2, "item-a", 30, 40, 10);
    b.markers.push(marker("on-b", 32, 1));
    let c = clip(3, "item-b", 0, 10, 20);
    let mut seq = sequence(vec![a, b, c]);
    seq.markers.push(marker("in-sequence", 5, 2));
    let r = records(vec![("item-a", vec![marker("early", 12, 0), marker("late", 33, 2), marker("nowhere", 25, 0)]), ("item-b", vec![marker("of-b", 4, 0)])]);

    let answer = sequence_appearing_markers(&seq, Some(&r), &token()).unwrap();
    assert_eq!(
        seen(&answer.appearing),
        vec![
            at(OnClip, "on-a", F, F),
            at(OnItem, "early", 2 * F, 2 * F),
            at(OnSequence, "in-sequence", 5 * F, 7 * F),
            at(OnClip, "on-b", 12 * F, 13 * F),
            at(OnItem, "late", 13 * F, 15 * F),
            at(OnItem, "of-b", 24 * F, 24 * F),
        ],
        "by position, with its scope"
    );
    // A media marker is hidden on each clip it misses: `early`, `late` and `nowhere` on the two clips of
    // item-a give 4 misses.
    assert_eq!((answer.hidden_media_markers, answer.hidden_clip_markers, answer.clips_without_media_item), (4, 0, 0));
    let of = |id: &str| answer.appearing.iter().find(|a| a.marker.marker_id == id).unwrap();
    assert_eq!(of("in-sequence").clip_id, None);
    assert_eq!(of("on-a").clip_id, Some(cid(1)));
    assert_eq!(of("late").clip_id, Some(cid(2)));

    // A clip moved 100 frames later: its own and its item's markers go with it, the sequence's stays.
    let mut moved = seq.clone();
    let clip_b = &mut moved.tracks[0].clips[1];
    clip_b.timeline_start = t(110);
    moved.duration = moved.content_end().unwrap();
    let after = sequence_appearing_markers(&moved, Some(&r), &token()).unwrap();
    let start_of = |id: &str| after.appearing.iter().find(|a| a.marker.marker_id == id).unwrap().span.start().value();
    assert_eq!((start_of("on-b"), start_of("late"), start_of("in-sequence"), start_of("early")), (112 * F, 113 * F, 5 * F, 2 * F));
    // The stored record does not change with the clip: the item's marker keeps its source ticks.
    assert_eq!(after.appearing.iter().find(|a| a.marker.marker_id == "late").unwrap().marker.start, t(33));

    // Clip read agrees with the sequence read for one clip.
    let one = clip_appearing_markers(&seq.tracks[0].clips[1], Some(&r), &token()).unwrap();
    assert_eq!(seen(&one.appearing), vec![at(OnClip, "on-b", 12 * F, 13 * F), at(OnItem, "late", 13 * F, 15 * F)]);
    assert_eq!(one.hidden_media_markers, 2);
}

/// One item, two clips over one source position: it appears on both, and each appearance is its own.
#[test]
fn a_marker_appears_once_per_clip_and_a_razor_hands_each_piece_what_it_plays() {
    let r = records(vec![("item-a", vec![marker("m", 15, 0), marker("span", 14, 4)])]);
    let two_cuts = sequence(vec![clip(1, "item-a", 10, 20, 0), clip(2, "item-a", 10, 20, 50)]);
    let answer = sequence_appearing_markers(&two_cuts, Some(&r), &token()).unwrap();
    assert_eq!(
        answer.appearing.iter().map(|a| (a.clip_id.clone().unwrap(), a.marker.marker_id.as_str(), a.span.start().value())).collect::<Vec<_>>(),
        vec![(cid(1), "span", 4 * F), (cid(1), "m", 5 * F), (cid(2), "span", 54 * F), (cid(2), "m", 55 * F)]
    );
    // A razor at source 16: the point goes to the left piece, the span appears clipped on both.
    let left = clip(1, "item-a", 10, 16, 0);
    let right = clip(2, "item-a", 16, 20, 6);
    let cut = sequence_appearing_markers(&sequence(vec![left, right]), Some(&r), &token()).unwrap();
    assert_eq!(
        cut.appearing.iter().map(|a| (a.clip_id.clone().unwrap(), a.marker.marker_id.as_str(), a.span.start().value(), a.span.end().value())).collect::<Vec<_>>(),
        vec![(cid(1), "span", 4 * F, 6 * F), (cid(1), "m", 5 * F, 5 * F), (cid(2), "span", 6 * F, 8 * F)]
    );
    assert_eq!(cut.hidden_media_markers, 1, "the point on the right piece");
}

/// Hidden, not lost: a trim hides the marker and reports nothing false; widening the window brings it back.
#[test]
fn a_marker_outside_the_window_is_hidden_not_lost() {
    let r = records(vec![("item-a", vec![marker("m", 25, 0)])]);
    let narrow = clip(1, "item-a", 10, 20, 100);
    let hidden = clip_appearing_markers(&narrow, Some(&r), &token()).unwrap();
    assert!(hidden.appearing.is_empty());
    assert_eq!(hidden.hidden_media_markers, 1);
    assert_eq!(r.entry(&digest("item-a")).unwrap().markers.len(), 1, "still on the item");
    let wide = clip(1, "item-a", 10, 30, 100);
    assert_eq!(seen(&clip_appearing_markers(&wide, Some(&r), &token()).unwrap().appearing), vec![at(OnItem, "m", 115 * F, 115 * F)]);
    // The clip's own marker behaves the same way.
    let mut own = narrow.clone();
    own.markers.push(marker("own", 25, 0));
    let hidden = clip_appearing_markers(&own, None, &token()).unwrap();
    assert_eq!((hidden.appearing.len(), hidden.hidden_clip_markers, hidden.hidden_media_markers), (0, 1, 0));
    // A sequence with no clip of the item: nothing appears and nothing is counted, it was not missed.
    let answer = sequence_appearing_markers(&sequence(vec![clip(1, "item-b", 0, 5, 0)]), Some(&r), &token()).unwrap();
    assert_eq!((answer.appearing.len(), answer.hidden_media_markers), (0, 0));
}

/// Hostile: what appears is bounded by the existing marker limit, typed, never a truncated list; a span
/// past the last tick is a typed error; a cancel stops the read.
#[test]
fn the_read_is_bounded_and_cancellable() {
    let many: Vec<StudioMarker> = (0..MAX_MARKERS_PER_SEQUENCE).map(|i| StudioMarker { name: String::new(), ..marker(&format!("m{i}"), 15, 0) }).collect();
    let r = records(vec![("item-a", many)]);
    // At the limit on one clip it answers; a second clip of the item goes over.
    let one = sequence(vec![clip(1, "item-a", 10, 20, 0)]);
    assert_eq!(sequence_appearing_markers(&one, Some(&r), &token()).unwrap().appearing.len(), MAX_MARKERS_PER_SEQUENCE);
    let two = sequence(vec![clip(1, "item-a", 10, 20, 0), clip(2, "item-a", 10, 20, 10)]);
    assert_eq!(sequence_appearing_markers(&two, Some(&r), &token()).unwrap_err(), PulseError::LimitExceeded);
    // The clip's own markers count with them.
    let mut crowded = clip(1, "item-a", 10, 20, 0);
    crowded.markers.push(marker("own", 15, 0));
    assert_eq!(clip_appearing_markers(&crowded, Some(&r), &token()).unwrap_err(), PulseError::LimitExceeded);
    // And the sequence's.
    let mut with_own = one.clone();
    with_own.markers.push(marker("in-sequence", 1, 0));
    assert_eq!(sequence_appearing_markers(&with_own, Some(&r), &token()).unwrap_err(), PulseError::LimitExceeded);

    // All hidden: the walk is not limited by what appears, and it still answers.
    let hidden_only = sequence(vec![clip(1, "item-a", 100, 110, 0), clip(2, "item-a", 100, 110, 10)]);
    let answer = sequence_appearing_markers(&hidden_only, Some(&r), &token()).unwrap();
    assert_eq!((answer.appearing.len(), answer.hidden_media_markers), (0, 2 * MAX_MARKERS_PER_SEQUENCE));

    // Records the host did not validate: an overflowing span is typed, not a panic.
    let overflow = records(vec![("item-a", vec![StudioMarker { start: Ticks::new(u64::MAX), duration: Ticks::new(1), ..marker("m", 0, 0) }])]);
    assert_eq!(clip_appearing_markers(&clip(1, "item-a", 10, 20, 0), Some(&overflow), &token()).unwrap_err(), PulseError::Overflow);

    let canceled = token();
    canceled.cancel();
    assert_eq!(clip_appearing_markers(&clip(1, "item-a", 10, 20, 0), Some(&r), &canceled).unwrap_err(), PulseError::Canceled);
    assert_eq!(sequence_appearing_markers(&two, Some(&r), &canceled).unwrap_err(), PulseError::Canceled);
    assert_eq!(sequence_appearing_markers(&two, None, &canceled).unwrap_err(), PulseError::Canceled);
}

/// The two earlier scopes keep their read: the new read gives what `marker_spans` gives for them, on a
/// clip that holds no frame.
#[test]
fn the_new_read_agrees_with_marker_spans_for_the_two_earlier_scopes() {
    let mut a = clip_at(1, "item-a", 10, 20, 0, 2, 1);
    a.markers.push(marker("on-a", 12, 2));
    let mut b = clip_at(2, "item-a", 10, 20, 5, -1, 1);
    b.markers.push(marker("on-b", 12, 1));
    b.markers.push(marker("off-b", 40, 0));
    let mut seq = sequence(vec![a, b]);
    seq.markers.push(marker("in-sequence", 3, 1));
    let old: Vec<(String, TickRange)> = marker_spans(&seq, &token()).unwrap();
    let new = sequence_appearing_markers(&seq, None, &token()).unwrap();
    let mut new_spans: Vec<(String, TickRange)> = new.appearing.iter().map(|a| (a.marker.marker_id.clone(), a.span)).collect();
    new_spans.sort_by(|x, y| (x.1.start(), &x.0).cmp(&(y.1.start(), &y.0)));
    assert_eq!(new_spans, old);
    assert_eq!(new.hidden_clip_markers, 1);
}
