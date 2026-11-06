//! Lane SMB-PR-MEDIA-MARKERS-1, F-R58-1: where a zero-length marker on a reversed clip appears depends on what
//! the clip's track plays. Oracles outside pulse's own mapping: for an audio clip the rendered mix, the energy
//! centroid of a Hann pulse written into a silent source; for a video clip `frame_layers`. Caption and submix
//! clips reach neither, so their rows pin the stated answer and print `NOT_PROVEN:`. Positions in frames of
//! 25 fps; vendor parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use std::sync::Arc;

const RATE: u32 = 48_000;
/// 25 fps ticks per frame.
const F: u64 = 10_160_640_000;
/// Ticks per sample at 48 kHz.
const TPS: u64 = 5_292_000;
/// Samples per frame.
const FS: u64 = 1920;
const SEQ: &str = "seq-main";
/// How far, in frames, a read may stand from the mix: a reversed mix plays a source sample one sample before
/// `source_out - a`, two mix samples at speed -1/2 (0.001 frame). The defect this holds off is 0.25 frame and up.
const WITHIN: f64 = 0.002;
/// Points as quarter frames into the window: whole-frame and mid-frame.
const QUARTERS: [u64; 6] = [12, 28, 44, 46, 61, 88];
const REVERSED: [(i64, u64); 4] = [(-1, 1), (-2, 1), (-1, 2), (-3, 2)];
const FORWARD: [(i64, u64); 4] = [(1, 1), (2, 1), (1, 2), (3, 2)];

fn token() -> CancellationToken {
    CancellationToken::default()
}
fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn digest() -> ContentDigest {
    ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }
}

#[derive(Clone)]
struct Audio(Arc<Vec<Vec<f32>>>);
impl AudioSources for Audio {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        Some(&self.0)
    }
}
struct Known;
impl SourceExtents for Known {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(u64::MAX / 2))
    }
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

/// Source frames `[si, so)` at timeline frame `start`, speed `num/den`.
fn clip(n: u32, si: u64, so: u64, start: u64, num: i64, den: u64) -> StudioClip {
    let duration = (u128::from((so - si) * F) * u128::from(den) / u128::from(num.unsigned_abs())) as u64;
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: id("SCLP", n),
        source_ref: ClipSource::Media { artifact_manifest_id: format!("manifest-{n}"), content_digest: digest(), asset_id: None },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(duration),
        speed: Speed { numerator: num, denominator: den },
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: ClipRetime::default(),
    }
}
fn point(marker_id: &str, name: &str, start: Ticks) -> StudioMarker {
    StudioMarker { marker_id: marker_id.into(), name: name.into(), comment: String::new(), color: LabelColor::Rose, start, duration: Ticks::new(0), role: None }
}
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id("STRK", n),
        kind,
        index: 0,
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
fn sequence(tracks: Vec<StudioTrack>) -> StudioSequence {
    let end = tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_start.value() + c.duration.value()).max().unwrap_or(0);
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: SEQ.into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(end),
        tracks,
        markers: Vec::new(),
        work_area: None,
    }
}
fn snapshot(tracks: Vec<StudioTrack>) -> Snapshot {
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
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![sequence(tracks)],
    };
    validate_document(document, Budget::default(), &token(), &Media).expect("fixture is a valid Folio document")
}

/// A silent source of `len` samples with a 129-sample Hann pulse centred on sample `at`.
fn pulse(len: u64, at: u64) -> Arc<Vec<Vec<f32>>> {
    let mut x = vec![0.0f32; len as usize];
    for k in 0..129usize {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * k as f64 / 128.0).cos();
        x[at as usize + k - 64] = w as f32;
    }
    Arc::new(vec![x.clone(), x])
}
/// A silent source holding one sample at `at`.
fn impulse(len: u64, at: u64) -> Arc<Vec<Vec<f32>>> {
    let mut x = vec![0.0f32; len as usize];
    x[at as usize] = 1.0;
    Arc::new(vec![x.clone(), x])
}
fn mix(seq: &StudioSequence, channels: &Arc<Vec<Vec<f32>>>) -> Vec<f32> {
    let cancel = token();
    let mut s = TimelineMixStream::new(seq, RATE, 1024, Audio(channels.clone()), &Known, StreamLimits::default(), &cancel).expect("mix");
    let n = s.length_samples() as usize;
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut at = 0;
    while at < n {
        let k = 1024.min(n - at);
        s.render(&mut l[at..at + k], &mut r[at..at + k], &cancel).expect("render");
        at += k;
    }
    l
}
/// Where the mix holds its energy, in frames from the head of the mix.
fn heard_at(seq: &StudioSequence, channels: &Arc<Vec<Vec<f32>>>) -> f64 {
    let (mut e, mut m) = (0.0f64, 0.0f64);
    for (i, v) in mix(seq, channels).iter().enumerate() {
        let p = f64::from(*v) * f64::from(*v);
        e += p;
        m += p * i as f64;
    }
    assert!(e > 0.0, "the mix holds the pulse");
    m / e / FS as f64
}
fn frames(t: Ticks) -> f64 {
    t.value() as f64 / F as f64
}
/// Source tick `q` quarter frames into a window from frame `si`, on a sample.
fn tick_of(si: u64, q: u64) -> u64 {
    (si * 4 + q) * (FS / 4) * TPS
}

/// The mix is what an audio clip plays. Every read that shares the mapping answers one tick per row, and that
/// tick is where the mix plays the marker's sample: spans, both scopes of the appearing read, playhead
/// navigation and snapping, reversed, whole-frame and mid-frame. Forward speeds are the control.
#[test]
fn a_point_on_an_audio_clip_reads_where_the_mix_plays_it_r58_f1() {
    let (si, so, start) = (2u64, 26u64, 2u64);
    let mut off = Vec::new();
    for (num, den) in REVERSED.into_iter().chain(FORWARD) {
        let mut worst = 0.0f64;
        for q in QUARTERS {
            let tick = Ticks::new(tick_of(si, q));
            let mut c = clip(1, si, so, start, num, den);
            c.markers.push(point("on-clip", "clap", tick));
            let seq = sequence(vec![track(1, TrackKind::Audio, vec![c])]);
            let spans = marker_spans(&seq, &token()).unwrap();
            let reported = spans[0].1.start();
            assert_eq!(spans[0].1.end(), reported, "{num}/{den} q{q}");
            let mut r = StudioMediaMarkerRecords::new(&id("SDOC", 1));
            r.media_items = vec![StudioMediaMarkerEntry { content_digest: digest(), markers: vec![point("on-item", "clap", tick)] }];
            let read = sequence_appearing_markers(&seq, Some(&r), &token()).unwrap();
            assert_eq!(read.appearing.len(), 2, "{num}/{den} q{q}");
            assert!(read.appearing.iter().all(|a| a.span.start() == reported), "{num}/{den} q{q}: both scopes read one tick: {:?}", read.appearing);
            assert_eq!(next_marker(&seq, Ticks::new(0), &token()).unwrap(), Some(reported), "{num}/{den} q{q}");
            assert_eq!(prev_marker(&seq, Ticks::new(u64::MAX), &token()).unwrap(), Some(reported), "{num}/{den} q{q}");
            let targets = snap_targets(&seq, None, &[], &token()).unwrap();
            assert!(targets.iter().any(|x| x.kind == SnapKind::Marker && x.at == reported), "{num}/{den} q{q}");
            let channels = pulse((so + 4) * FS, tick.value() / TPS);
            let heard = heard_at(&seq, &channels);
            let d = frames(reported) - heard;
            worst = worst.max(d.abs());
            if d.abs() > WITHIN {
                off.push(format!("{num}/{den} source +{}F: read {:.3}F, heard {heard:.3}F, off {d:+.3}F", q as f64 / 4.0, frames(reported)));
            }
        }
        println!("R58 audio against the mix: speed {num}/{den}, window {si}..{so}, 6 points: worst {worst:.3}F");
    }
    assert!(off.is_empty(), "a point stands away from the sample the mix plays: {off:#?}");
}

/// A marker on the window's first source tick, on a reversed audio clip, reads the clip's end: `source_out - a`.
/// The mix plays that sample last, one sample before it. A clip with a picture answers inside the clip; audio
/// keeps the continuous mapping (lead ruling, after R58).
#[test]
fn a_reversed_audio_clips_in_point_reads_the_end_the_mix_plays_it_at_r58_f1() {
    let mut c = clip(1, 2, 26, 2, -1, 1);
    c.markers.push(point("in", "clap", Ticks::new(2 * F)));
    let end = c.timeline_end().unwrap();
    let seq = sequence(vec![track(1, TrackKind::Audio, vec![c])]);
    let reported = marker_spans(&seq, &token()).unwrap()[0].1.start();
    assert_eq!(reported, end);
    let heard = heard_at(&seq, &impulse(30 * FS, 2 * FS));
    assert!((frames(reported) - heard).abs() < WITHIN, "read {:.3}F, heard {heard:.3}F", frames(reported));
}

/// A lone clip does not say what its track plays. With no grid it answers the continuous mapping, the audio
/// answer, and the mix hears it there; given the sequence's grid it answers the frame law, the video answer,
/// and `frame_layers` shows it there. Clip at 2F, source 2..26F, on each speed.
#[test]
fn a_lone_clip_answers_by_the_grid_it_is_given_r58_f1() {
    let grid = SourceFrameGrid::new(F).unwrap();
    for (num, den) in REVERSED {
        for q in QUARTERS {
            let tick = Ticks::new(tick_of(2, q));
            let mut c = clip(1, 2, 26, 2, num, den);
            c.markers.push(point("m", "clap", tick));
            let no_grid = clip_appearing_markers(&c, None, None, &token()).unwrap().appearing[0].span;
            let audio = sequence(vec![track(1, TrackKind::Audio, vec![c.clone()])]);
            assert_eq!(no_grid, sequence_appearing_markers(&audio, None, &token()).unwrap().appearing[0].span, "{num}/{den} q{q}");
            let heard = heard_at(&audio, &pulse(30 * FS, tick.value() / TPS));
            assert!((frames(no_grid.start()) - heard).abs() < WITHIN, "{num}/{den} q{q}: read {:.3}F, heard {heard:.3}F", frames(no_grid.start()));
            let stated = clip_appearing_markers(&c, None, Some(grid), &token()).unwrap().appearing[0].span;
            let video = sequence(vec![track(1, TrackKind::Video, vec![c.clone()])]);
            assert_eq!(stated, sequence_appearing_markers(&video, None, &token()).unwrap().appearing[0].span, "{num}/{den} q{q}");
            let layers = frame_layers(&video, stated.start(), &UnknownExtents, &token()).unwrap();
            assert_eq!(layers[0].answer.frame().index, tick.value() / F, "{num}/{den} q{q}");
        }
    }
}

/// `frame_layers` is what a video clip shows. The reversed rows, whole-frame and mid-frame, and the forward
/// control on whole frames and at unit speed; a mid-frame point at another forward speed keeps its tick and is
/// F-R58-4, not asserted here.
#[test]
fn a_point_on_a_video_clip_reads_where_frame_layers_shows_its_frame_r58_f1() {
    let (si, so, start) = (2u64, 26u64, 2u64);
    for (num, den, quarters) in [
        (-1i64, 1u64, &QUARTERS[..]),
        (-2, 1, &QUARTERS[..]),
        (-1, 2, &QUARTERS[..]),
        (1, 1, &QUARTERS[..]),
        (2, 1, &[12, 28, 44, 88][..]),
        (1, 2, &[12, 28, 44, 88][..]),
    ] {
        for q in quarters {
            let tick = tick_of(si, *q);
            let mut c = clip(1, si, so, start, num, den);
            c.markers.push(point("m", "clap", Ticks::new(tick)));
            let seq = sequence(vec![track(1, TrackKind::Video, vec![c])]);
            let reported = marker_spans(&seq, &token()).unwrap()[0].1.start();
            let layers = frame_layers(&seq, reported, &UnknownExtents, &token()).unwrap();
            assert_eq!(layers.len(), 1, "{num}/{den} q{q}: inside the clip");
            assert_eq!(layers[0].answer.frame().index, tick / F, "{num}/{den} q{q}: at {:.3}F", frames(reported));
            if num < 0 {
                let before = Ticks::new(reported.value() - F * den / num.unsigned_abs());
                if before >= seq.tracks[0].clips[0].timeline_start {
                    let layers = frame_layers(&seq, before, &UnknownExtents, &token()).unwrap();
                    assert_eq!(layers[0].answer.frame().index, tick / F + 1, "{num}/{den} q{q}: the frame played before it");
                }
            }
        }
    }
}

/// The kind alone says which law: a disabled or muted track is still its kind. Clip at 2F, source 2..26F, -1.
/// Video by hand from the window's last frame, 25F: frame 13 is 12F in. Audio: 26F - 13F = 13F in, and
/// 26F - 13.5F = 12.5F in. A forward clip does not care.
#[test]
fn the_law_goes_by_the_tracks_kind_alone_r58_f1() {
    let read = |kind: TrackKind, at: u64, num: i64, edit: &dyn Fn(&mut StudioTrack)| {
        let mut c = clip(1, 2, 26, 2, num, 1);
        c.markers.push(point("m", "clap", Ticks::new(at)));
        let mut t = track(1, kind, vec![c]);
        edit(&mut t);
        marker_spans(&sequence(vec![t]), &token()).unwrap()[0].1.start().value()
    };
    let same = |_: &mut StudioTrack| {};
    let disabled = |t: &mut StudioTrack| t.enabled = false;
    let muted = |t: &mut StudioTrack| t.muted = true;
    let clip_off = |t: &mut StudioTrack| t.clips[0].enabled = false;
    let rows: [(&str, TrackKind, &dyn Fn(&mut StudioTrack)); 8] = [
        ("video", TrackKind::Video, &same),
        ("video, track disabled", TrackKind::Video, &disabled),
        ("video, track muted", TrackKind::Video, &muted),
        ("video, clip disabled", TrackKind::Video, &clip_off),
        ("audio", TrackKind::Audio, &same),
        ("audio, track disabled", TrackKind::Audio, &disabled),
        ("audio, track muted", TrackKind::Audio, &muted),
        ("audio, clip disabled", TrackKind::Audio, &clip_off),
    ];
    for (what, kind, edit) in rows {
        let video = kind == TrackKind::Video;
        let want = if video { 14 * F } else { 15 * F };
        assert_eq!(read(kind, 13 * F, -1, edit), want, "{what}: a whole frame");
        let want = if video { 14 * F } else { 14 * F + F / 2 };
        assert_eq!(read(kind, 13 * F + F / 2, -1, edit), want, "{what}: inside a frame");
        assert_eq!(read(kind, 13 * F, 1, edit), 13 * F, "{what}: forward");
    }
}

/// Caption and submix tracks have no source frame on screen (lead ruling after R58): a reversed point keeps
/// `source_out - a`. These rows prove the answer by hand and that neither oracle reaches the clip: `frame_layers`
/// draws nothing and the mix plays nothing of it. They do not prove the position against anything rendered.
#[test]
fn a_point_on_a_caption_or_submix_clip_keeps_the_continuous_mapping_r58_f1() {
    let (si, so, start) = (2u64, 26u64, 2u64);
    for kind in [TrackKind::Caption, TrackKind::Submix] {
        for (num, den) in REVERSED {
            for q in QUARTERS {
                let tick = tick_of(si, q);
                let mut c = clip(1, si, so, start, num, den);
                c.markers.push(point("m", "cue", Ticks::new(tick)));
                let seq = sequence(vec![track(1, kind, vec![c])]);
                let want = start * F + ((so * F - tick) * den / num.unsigned_abs());
                let reported = marker_spans(&seq, &token()).unwrap()[0].1.start();
                assert_eq!(reported.value(), want, "{kind:?} {num}/{den} q{q}");
                assert!(frame_layers(&seq, reported, &UnknownExtents, &token()).unwrap().is_empty(), "{kind:?}: no picture");
                let silent = mix(&seq, &pulse((so + 4) * FS, tick / TPS)).iter().all(|v| *v == 0.0);
                assert!(silent, "{kind:?}: the mix plays nothing of it");
            }
        }
    }
    println!("NOT_PROVEN: the position of a marker on a reversed caption or submix clip against rendered output: frame_layers and the mix do not play these kinds, so there is no oracle; the rows pin the lead's reading, source_out - a");
}

/// Synchronize and merge by marker, against what is heard. A video mark at 23F (source 8F of a 5..15F clip at
/// 20F). The audio clip's clap lands on it, by hand and by the mix: the merge_sync row itself is the first.
#[test]
fn a_clap_is_heard_on_the_mark_after_a_synchronize_or_merge_by_marker_r58_f1() {
    // Speed, clap on source frame, where the clip goes: 23F less the clap's distance into the clip.
    let rows: [(i64, u64, u64, u64); 7] = [(-1, 1, 2, 15), (-1, 1, 5, 18), (-2, 1, 4, 20), (-1, 2, 3, 9), (-3, 2, 4, 19), (1, 1, 2, 21), (2, 1, 4, 21)];
    for (num, den, at, placed) in rows {
        let mut v = clip(1, 5, 15, 20, 1, 1);
        v.markers.push(point("mk-1", "clap", Ticks::new(8 * F)));
        let mut a = clip(2, 0, 10, 40, num, den);
        a.markers.push(point("mk-2", "clap", Ticks::new(at * F)));
        let snap = snapshot(vec![track(1, TrackKind::Video, vec![v]), track(2, TrackKind::Audio, vec![a])]);
        let cancel = token();
        let extents = Known;
        let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &extents, cancel: &cancel };
        let rev = snap.document().revision;
        let marker = SyncPoint::Marker { name: "clap".into() };
        let clip_ids = vec![id("SCLP", 1), id("SCLP", 2)];
        let synced = plan_synchronize(&snap, SEQ, rev, &Synchronize { clip_ids: clip_ids.clone(), anchor_clip_id: id("SCLP", 1), point: marker.clone(), link_group: "sync-1".into() }, &NoSourceTimecodes, &ctx).unwrap();
        let merged = plan_merge_clips(&snap, SEQ, rev, &MergeClips { clip_ids, link_group: "av-1".into(), align: Some(marker) }, &NoSourceTimecodes, &ctx).unwrap();
        for (what, p) in [("synchronize", synced), ("merge", merged)] {
            let seq = &p.proposal().snapshot().document().sequences[0];
            assert_eq!(seq.tracks[1].clips[0].timeline_start.value(), placed * F, "{what} {num}/{den} at {at}F");
            let heard = heard_at(seq, &pulse(14 * FS, at * FS));
            println!("R58 {what} by marker {num}/{den}, clap on source {at}F: placed {placed}F, heard {heard:.3}F, mark 23F");
            assert!((heard - 23.0).abs() < WITHIN, "{what} {num}/{den} at {at}F: heard {heard:.3}F");
        }
    }
}
