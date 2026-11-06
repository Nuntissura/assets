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

// F-PR59-1: two clips aligned by marker. The oracle is not a mapping function: after the alignment the source
// tick's sound, measured in the rendered mix, is heard while the monitor presents that tick's frame. The monitor
// reads a timeline frame's first tick and holds it (`Playback::scrub` floors to the frame), so the frame presented
// at a moment is `frame_layers` at the start of its timeline frame. Video is on track 1 here unless a row says.

/// How the pair is aligned: synchronize to a named clip, or merge, whose anchor is its one video clip.
#[derive(Clone, Copy, Debug)]
enum Op {
    Synchronize(u32),
    Merge,
}
const OPS: [Op; 3] = [Op::Synchronize(1), Op::Synchronize(2), Op::Merge];
const SPEEDS: [(i64, u64); 3] = [(-1, 1), (-2, 1), (-1, 2)];
/// Source ticks checked through a clip, in quarter frames: clear of the pulse's half-width at the window's edges.
const THROUGH: [u64; 7] = [5, 8, 10, 12, 15, 26, 38];

fn marked(mut c: StudioClip, n: u32, at: u64) -> StudioClip {
    c.markers.push(point(&format!("mk-{n}"), "clap", Ticks::new(at)));
    c
}
/// A second track of one kind: Folio counts indexes per kind.
fn track_at(n: u32, kind: TrackKind, index: u32, clips: Vec<StudioClip>) -> StudioTrack {
    let mut t = track(n, kind, clips);
    t.index = index;
    t
}
fn align(tracks: Vec<StudioTrack>, clips: &[u32], op: Op) -> std::result::Result<StudioSequence, &'static str> {
    let snap = snapshot(tracks);
    let cancel = token();
    let extents = Known;
    let ctx = EditContext { budget: Budget::default(), resolver: &Media, extents: &extents, cancel: &cancel };
    let rev = snap.document().revision;
    let point = SyncPoint::Marker { name: "clap".into() };
    let clip_ids: Vec<String> = clips.iter().map(|n| id("SCLP", *n)).collect();
    let planned = match op {
        Op::Synchronize(anchor) => plan_synchronize(&snap, SEQ, rev, &Synchronize { clip_ids, anchor_clip_id: id("SCLP", anchor), point, link_group: "sync-1".into() }, &NoSourceTimecodes, &ctx),
        Op::Merge => plan_merge_clips(&snap, SEQ, rev, &MergeClips { clip_ids, link_group: "av-1".into(), align: Some(point) }, &NoSourceTimecodes, &ctx),
    };
    planned.map(|p| p.proposal().snapshot().document().sequences[0].clone()).map_err(|e| e.code())
}
fn on_track(seq: &StudioSequence, n: u32) -> &StudioClip {
    &seq.tracks.iter().find(|t| t.track_id == id("STRK", n)).expect("the track").clips[0]
}
fn start_of(seq: &StudioSequence, n: u32) -> u64 {
    on_track(seq, n).timeline_start.value()
}
fn starts(seq: &StudioSequence, n: u32) -> String {
    format!("{:.3}F", frames(on_track(seq, n).timeline_start))
}
/// The frame a video track presents from timeline frame `at`, if any.
fn shown(seq: &StudioSequence, n: u32, at: u64) -> Option<u64> {
    frame_layers(seq, Ticks::new(at * F), &UnknownExtents, &token()).unwrap().iter().find(|l| l.track_id == id("STRK", n)).map(|l| l.answer.frame().index)
}
/// The frame presented at a moment, in frames from the head.
fn presented(seq: &StudioSequence, n: u32, at: f64) -> Option<u64> {
    shown(seq, n, at.floor() as u64)
}
/// The frame a source frame's sound belongs under: itself, or at a speed that skips it (-2 presents every other
/// frame) the next one presented, the one on screen while the mix plays through it. Read off `frame_layers`.
fn frame_under(seq: &StudioSequence, n: u32, own: u64) -> Option<u64> {
    let c = on_track(seq, n);
    let (from, to) = (c.timeline_start.value() / F, c.timeline_end().unwrap().value().div_ceil(F));
    let all: std::collections::BTreeSet<u64> = (from..to).filter_map(|at| shown(seq, n, at)).collect();
    all.range(own..).next().copied()
}
/// Where one audio track's sound is, in frames from the head, the other audio tracks left out.
fn heard_on(seq: &StudioSequence, n: u32, channels: &Arc<Vec<Vec<f32>>>) -> f64 {
    let mut only = seq.clone();
    only.tracks.retain(|t| t.kind != TrackKind::Audio || t.track_id == id("STRK", n));
    heard_at(&only, channels)
}
/// The source tick `q` quarter frames in is heard under its own frame: video on track 1, sound on track 2.
fn sound_under_frame(seq: &StudioSequence, q: u64, what: &str, off: &mut Vec<String>) {
    let heard = heard_on(seq, 2, &pulse(14 * FS, q * FS / 4));
    let (at, want) = (presented(seq, 1, heard), frame_under(seq, 1, q / 4));
    println!("PR59 {what}: source {:.2}F heard {heard:.4}F under frame {at:?}, wanted {want:?}", q as f64 / 4.0);
    if at != want {
        off.push(format!("{what}: source {:.2}F heard {heard:.4}F under frame {at:?}, wanted {want:?}", q as f64 / 4.0));
    }
}
fn pair(v: StudioClip, a: StudioClip) -> Vec<StudioTrack> {
    vec![track(1, TrackKind::Video, vec![v]), track(2, TrackKind::Audio, vec![a])]
}

/// Reversed picture and reversed sound of one source, in sync before the alignment: nothing moves, by either anchor
/// or by merge, whether the marker sits on or inside a frame, and the whole clip stays in sync, not only the clap.
/// Windows one frame apart are the same source, in sync after a one-frame move. -2 presents only odd frames, so the
/// sound of an even frame is under the next one: `frame_under`.
#[test]
fn a_reversed_picture_and_its_sound_stay_in_sync_through_a_marker_alignment_pr59_f1() {
    let mut off = Vec::new();
    for (num, den) in SPEEDS {
        for clap in [2 * F, 2 * F + F / 2] {
            for op in OPS {
                let what = format!("same source {num}/{den} at 20F, clap {:.1}F, {op:?}", clap as f64 / F as f64);
                match align(pair(marked(clip(1, 0, 10, 20, num, den), 1, clap), marked(clip(2, 0, 10, 20, num, den), 2, clap)), &[1, 2], op) {
                    Err(code) => off.push(format!("{what}: refused {code}")),
                    Ok(seq) => {
                        println!("PR59 {what}: V {}, A {}", starts(&seq, 1), starts(&seq, 2));
                        if (start_of(&seq, 1), start_of(&seq, 2)) != (20 * F, 20 * F) {
                            off.push(format!("{what}: moved, V {}, A {}", starts(&seq, 1), starts(&seq, 2)));
                        }
                        for q in THROUGH {
                            sound_under_frame(&seq, q, &what, &mut off);
                        }
                    }
                }
            }
        }
    }
    for (op, v_from, v_at, a_at) in [(Op::Synchronize(1), 20, 20, 19), (Op::Synchronize(2), 50, 41, 40), (Op::Merge, 20, 20, 19)] {
        let what = format!("V 0..10F, A 1..11F, -1, {op:?}");
        let a_from = if v_from == 50 { 40 } else { 45 };
        match align(pair(marked(clip(1, 0, 10, v_from, -1, 1), 1, 2 * F), marked(clip(2, 1, 11, a_from, -1, 1), 2, 2 * F)), &[1, 2], op) {
            Err(code) => off.push(format!("{what}: refused {code}")),
            Ok(seq) => {
                println!("PR59 {what}: V {}, A {}", starts(&seq, 1), starts(&seq, 2));
                if (start_of(&seq, 1), start_of(&seq, 2)) != (v_at * F, a_at * F) {
                    off.push(format!("{what}: V {}, A {}, wanted V {v_at}F, A {a_at}F", starts(&seq, 1), starts(&seq, 2)));
                }
                for q in THROUGH {
                    sound_under_frame(&seq, q, &what, &mut off);
                }
            }
        }
    }
    assert!(off.is_empty(), "the pair left sync: {off:#?}");
}

/// Both clips at the head of the sequence: nothing has to move, so nothing underflows.
#[test]
fn a_reversed_pair_at_the_head_has_nothing_to_move_pr59_f1() {
    let mut off = Vec::new();
    for (num, den) in SPEEDS {
        for op in OPS {
            let what = format!("same source {num}/{den} at 0F, {op:?}");
            match align(pair(marked(clip(1, 0, 10, 0, num, den), 1, 2 * F), marked(clip(2, 0, 10, 0, num, den), 2, 2 * F)), &[1, 2], op) {
                Err(code) => off.push(format!("{what}: refused {code}")),
                Ok(seq) => {
                    println!("PR59 {what}: V {}, A {}", starts(&seq, 1), starts(&seq, 2));
                    if (start_of(&seq, 1), start_of(&seq, 2)) != (0, 0) {
                        off.push(format!("{what}: moved, V {}, A {}", starts(&seq, 1), starts(&seq, 2)));
                    }
                    for q in THROUGH {
                        sound_under_frame(&seq, q, &what, &mut off);
                    }
                }
            }
        }
    }
    assert!(off.is_empty(), "the pair at the head: {off:#?}");
}

/// Both reversed at different speeds: there is no whole clip to keep in sync, only the clap, heard under its own
/// frame through the continuous mapping on both sides. Video speed, audio speed, clap frame, anchor, then where each
/// clip starts in quarter frames. A picture stays on its frame grid: at -2 an odd clap reads half a frame in, so
/// moving the picture there is refused; the picture as anchor aligns that pair.
#[test]
fn a_reversed_pair_at_different_speeds_reads_the_continuous_mapping_on_both_sides_pr59_f1() {
    type Row = ((i64, u64), (i64, u64), u64, u32, u64, u64);
    let rows: [Row; 7] = [
        ((-1, 1), (-2, 1), 2, 1, 80, 96),
        ((-1, 2), (-1, 1), 2, 1, 80, 112),
        ((-2, 1), (-1, 1), 3, 1, 80, 66),
        ((-1, 1), (-1, 2), 2, 1, 80, 48),
        ((-1, 1), (-3, 2), 1, 1, 80, 92),
        ((-1, 2), (-1, 1), 2, 2, 48, 80),
        ((-2, 1), (-1, 1), 2, 2, 96, 80),
    ];
    let mut off = Vec::new();
    for ((vn, vd), (an, ad), at, anchor, v_at, a_at) in rows {
        let what = format!("V {vn}/{vd}, A {an}/{ad}, clap {at}F, anchor {anchor}");
        let (v_from, a_from) = if anchor == 1 { (20, 60) } else { (60, 20) };
        match align(pair(marked(clip(1, 0, 10, v_from, vn, vd), 1, at * F), marked(clip(2, 0, 10, a_from, an, ad), 2, at * F)), &[1, 2], Op::Synchronize(anchor)) {
            Err(code) => off.push(format!("{what}: refused {code}")),
            Ok(seq) => {
                println!("PR59 {what}: V {}, A {}", starts(&seq, 1), starts(&seq, 2));
                if (start_of(&seq, 1), start_of(&seq, 2)) != (v_at * F / 4, a_at * F / 4) {
                    off.push(format!("{what}: V {}, A {}, wanted V {:.2}F, A {:.2}F", starts(&seq, 1), starts(&seq, 2), v_at as f64 / 4.0, a_at as f64 / 4.0));
                }
                sound_under_frame(&seq, at * 4, &what, &mut off);
            }
        }
    }
    let moved = align(pair(marked(clip(1, 0, 10, 60, -2, 1), 1, 3 * F), marked(clip(2, 0, 10, 20, -1, 1), 2, 3 * F)), &[1, 2], Op::Synchronize(2)).map(|seq| start_of(&seq, 1));
    println!("PR59 V -2/1, A -1/1, clap 3F, anchor 2: {moved:?}");
    if moved != Err("not_frame_aligned") {
        off.push(format!("V -2 onto A -1 by an odd clap: {moved:?}, wanted the picture to refuse leaving its grid"));
    }
    assert!(off.is_empty(), "the clap at different speeds: {off:#?}");
}

/// Three clips, one anchor: the anchor's position is read per pair. From the picture, a reversed partner stays in
/// sync and a forward partner lands by the frame law, both inside the clap frame's presentation. From reversed
/// sound, a forward partner aligns sound to sound: both claps on one moment, at the end of the clap frame.
#[test]
fn a_third_clip_reads_its_own_pair_against_the_anchor_pr59_f1() {
    let tracks = |v_at: u64, a_at: u64, b_at: u64| {
        vec![
            track(1, TrackKind::Video, vec![marked(clip(1, 0, 10, v_at, -1, 1), 1, 2 * F)]),
            track(2, TrackKind::Audio, vec![marked(clip(2, 0, 10, a_at, -1, 1), 2, 2 * F)]),
            track_at(3, TrackKind::Audio, 1, vec![marked(clip(3, 0, 10, b_at, 1, 1), 3, 2 * F)]),
        ]
    };
    let channels = pulse(14 * FS, 2 * FS);
    for op in [Op::Synchronize(1), Op::Merge] {
        let seq = align(tracks(20, 20, 60), &[1, 2, 3], op).unwrap();
        assert_eq!((start_of(&seq, 1), start_of(&seq, 2), start_of(&seq, 3)), (20 * F, 20 * F, 25 * F), "{op:?}");
        let (reversed, forward) = (heard_on(&seq, 2, &channels), heard_on(&seq, 3, &channels));
        println!("PR59 three clips, {op:?}: reversed sound {reversed:.4}F, forward sound {forward:.4}F, frame 2 presented 27F..28F");
        assert_eq!((presented(&seq, 1, reversed), shown(&seq, 1, 27), shown(&seq, 1, 28)), (Some(2), Some(2), Some(1)), "{op:?}");
        assert!((forward - 27.0).abs() < WITHIN, "{op:?}: forward sound {forward:.4}F");
    }
    let seq = align(tracks(60, 20, 60), &[1, 2, 3], Op::Synchronize(2)).unwrap();
    assert_eq!((start_of(&seq, 1), start_of(&seq, 2), start_of(&seq, 3)), (20 * F, 20 * F, 26 * F));
    let (reversed, forward) = (heard_on(&seq, 2, &channels), heard_on(&seq, 3, &channels));
    println!("PR59 three clips, reversed sound anchor: reversed sound {reversed:.4}F, forward sound {forward:.4}F, frame 2 presented 27F..28F");
    assert!((reversed - forward).abs() < WITHIN, "{reversed:.4}F, {forward:.4}F");
    assert_eq!(presented(&seq, 1, reversed), Some(2));
}

/// Not a both-reversed pair: each clip reads by its own kind. A reversed anchor with a picture keeps the frame law,
/// F-PR59-4's missing row: the clap of forward sound is heard on the first tick the monitor presents the frame.
/// Video speed, video clap, audio speed, audio clap, then the mark and where the audio goes; quarter frames.
#[test]
fn a_reversed_picture_and_forward_sound_read_the_frame_law_on_the_picture_pr59_f4() {
    type Row = ((i64, u64), u64, (i64, u64), u64, u64, u64);
    let rows: [Row; 5] = [
        ((-1, 1), 8, (1, 1), 8, 108, 100),
        ((-1, 2), 8, (1, 1), 8, 136, 128),
        ((-2, 1), 12, (1, 1), 12, 92, 80),
        ((-1, 1), 10, (1, 1), 10, 108, 98),
        ((-1, 1), 8, (2, 1), 16, 108, 100),
    ];
    for ((vn, vd), vq, (an, ad), aq, mark, a_at) in rows {
        for op in [Op::Synchronize(1), Op::Merge] {
            let what = format!("V {vn}/{vd} clap {:.2}F, A {an}/{ad} clap {:.2}F, {op:?}", vq as f64 / 4.0, aq as f64 / 4.0);
            let seq = align(pair(marked(clip(1, 0, 10, 20, vn, vd), 1, vq * F / 4), marked(clip(2, 0, 10, 60, an, ad), 2, aq * F / 4)), &[1, 2], op).unwrap();
            assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (20 * F, a_at * F / 4), "{what}");
            let heard = heard_on(&seq, 2, &pulse(14 * FS, aq * FS / 4));
            println!("PR59 {what}: A {}, clap heard {heard:.4}F, frame {} from {:.2}F", starts(&seq, 2), vq / 4, mark as f64 / 4.0);
            assert!((heard - mark as f64 / 4.0).abs() < WITHIN, "{what}: heard {heard:.4}F");
            let at = mark / 4;
            assert_eq!(shown(&seq, 1, at), Some(vq / 4), "{what}: on the mark");
            assert_ne!(shown(&seq, 1, at - 1), Some(vq / 4), "{what}: its first tick");
        }
    }
    // The anchor the other way: forward sound stays, the reversed picture lands by the frame law.
    let seq = align(pair(marked(clip(1, 0, 10, 60, -1, 1), 1, 2 * F), marked(clip(2, 0, 10, 40, 1, 1), 2, 2 * F)), &[1, 2], Op::Synchronize(2)).unwrap();
    assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (35 * F, 40 * F));
    assert_eq!((shown(&seq, 1, 42), shown(&seq, 1, 41)), (Some(2), Some(3)));
    // Reversed sound as anchor, a forward picture: the mark is where the sound's continuous mapping reads it.
    let seq = align(pair(marked(clip(1, 0, 10, 60, 1, 1), 1, 2 * F), marked(clip(2, 0, 10, 20, -1, 1), 2, 2 * F)), &[1, 2], Op::Synchronize(2)).unwrap();
    assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (26 * F, 20 * F));
    assert_eq!(shown(&seq, 1, 28), Some(2));
    let heard = heard_on(&seq, 2, &pulse(14 * FS, 2 * FS));
    assert!((heard - 28.0).abs() < WITHIN, "heard {heard:.4}F");
}

/// Reversed pairs the rule leaves alone. Two pictures read the frame law: nothing moves when they match, and at
/// different speeds the frame shows on both at one tick, where the continuous mapping would ask for half a frame
/// and refuse. Two sounds read the continuous mapping, measured in the mix.
#[test]
fn a_reversed_pair_of_one_kind_reads_by_its_kind_pr59_f1() {
    for (num, den) in SPEEDS {
        for clap in [2 * F, 2 * F + F / 2] {
            for anchor in [1, 2] {
                let tracks = vec![track(1, TrackKind::Video, vec![marked(clip(1, 0, 10, 20, num, den), 1, clap)]), track_at(2, TrackKind::Video, 1, vec![marked(clip(2, 0, 10, 20, num, den), 2, clap)])];
                let seq = align(tracks, &[1, 2], Op::Synchronize(anchor)).unwrap();
                assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (20 * F, 20 * F), "{num}/{den} clap {clap} anchor {anchor}");
            }
            let tracks = vec![track(1, TrackKind::Audio, vec![marked(clip(1, 0, 10, 20, num, den), 1, clap)]), track_at(2, TrackKind::Audio, 1, vec![marked(clip(2, 0, 10, 20, num, den), 2, clap)])];
            let seq = align(tracks, &[1, 2], Op::Synchronize(1)).unwrap();
            assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (20 * F, 20 * F), "sound {num}/{den} clap {clap}");
        }
    }
    let tracks = vec![track(1, TrackKind::Video, vec![marked(clip(1, 0, 10, 20, -1, 1), 1, 3 * F)]), track_at(2, TrackKind::Video, 1, vec![marked(clip(2, 0, 10, 60, -2, 1), 2, 3 * F)])];
    let seq = align(tracks, &[1, 2], Op::Synchronize(1)).unwrap();
    assert_eq!(start_of(&seq, 2), 23 * F);
    assert_eq!((shown(&seq, 1, 26), shown(&seq, 2, 26)), (Some(3), Some(3)));
    let tracks = vec![track(1, TrackKind::Audio, vec![marked(clip(1, 0, 10, 20, -1, 1), 1, 2 * F)]), track_at(2, TrackKind::Audio, 1, vec![marked(clip(2, 0, 10, 60, -2, 1), 2, 2 * F)])];
    let seq = align(tracks, &[1, 2], Op::Synchronize(1)).unwrap();
    assert_eq!(start_of(&seq, 2), 24 * F);
    let channels = pulse(14 * FS, 2 * FS);
    let (one, two) = (heard_on(&seq, 1, &channels), heard_on(&seq, 2, &channels));
    assert!((one - two).abs() < WITHIN, "{one:.4}F, {two:.4}F");
    // One picture and a caption: the pair the rule names, with nothing to measure; the reading only.
    let tracks = vec![track(1, TrackKind::Video, vec![marked(clip(1, 0, 10, 20, -1, 1), 1, 2 * F)]), track(2, TrackKind::Caption, vec![marked(clip(2, 0, 10, 20, -1, 1), 2, 2 * F)])];
    let seq = align(tracks, &[1, 2], Op::Synchronize(1)).unwrap();
    assert_eq!((start_of(&seq, 1), start_of(&seq, 2)), (20 * F, 20 * F));
    println!("NOT_PROVEN: a reversed caption clip aligned by marker to a reversed picture against rendered output: no output plays a caption clip, so the row pins the lead's reading, the continuous mapping on both sides");
}
