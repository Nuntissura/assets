//! Lane SMB-PULSE-PLAYBACK-2: the timeline mix without score's 64-clip session cap. Stated
//! tolerance: every streamed sample within 1e-6 of the hand mix (score sums in f64 per session
//! and publishes f32; the stream adds the f32 group outputs), and within 1e-6 of the
//! single-session `build_timeline_mix` for a timeline that fits one session.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;

fn source(name: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: name.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "abc123".into(),
        },
        asset_id: None,
    }
}
fn clip(n: u32, src: &str, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source(src),
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
fn track(n: u32, clips: Vec<StudioClip>, volume: f64, pan: f64) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind: TrackKind::Audio,
        index: 0,
        name: format!("A{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume,
        pan,
    }
}
fn sequence(tracks: Vec<StudioTrack>, frames: u64) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(frames * F),
        tracks,
        markers: Vec::new(),
        work_area: None,
    }
}

/// Source "s{n}" is the constant (n + 1) / 1000, mono, 4 frames long.
struct Constants(Vec<Vec<Vec<f32>>>);
impl Constants {
    fn new(count: usize) -> Self {
        Self((0..count).map(|n| vec![vec![(n as f32 + 1.0) / 1000.0; 4 * SPF]]).collect())
    }
}
impl AudioSources for Constants {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else {
            return None;
        };
        let n: usize = id.strip_prefix('s')?.parse().ok()?;
        self.0.get(n).map(|v| v.as_slice())
    }
}
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(4 * F))
    }
}

fn render_all<S: AudioSources>(m: &mut TimelineMixStream<S>, total: usize, block: usize) -> (Vec<f32>, Vec<f32>) {
    let cancel = CancellationToken::default();
    let (mut l, mut r) = (Vec::new(), Vec::new());
    let (mut bl, mut br) = (vec![0.0; block], vec![0.0; block]);
    while l.len() < total {
        let n = block.min(total - l.len());
        m.render(&mut bl[..n], &mut br[..n], &cancel).unwrap();
        l.extend_from_slice(&bl[..n]);
        r.extend_from_slice(&br[..n]);
    }
    (l, r)
}

#[test]
fn streamed_mix_of_more_than_64_clips_matches_the_hand_mix() {
    let cancel = CancellationToken::default();
    // Three audio tracks x 70 one-frame clips = 210 clips, three sounding at any time.
    // Track gains 1, 0.5, 0.25; pans 0 (equal power), -1 (left), 1 (right).
    let (volumes, pans) = ([1.0, 0.5, 0.25], [0.0, -1.0, 1.0]);
    let mut tracks = Vec::new();
    let mut n = 0u32;
    for t in 0..3 {
        let clips = (0..70u64)
            .map(|k| {
                n += 1;
                clip(n, &format!("s{}", n - 1), 1, 1, k)
            })
            .collect();
        tracks.push(track(t + 1, clips, volumes[t as usize], pans[t as usize]));
    }
    let seq = sequence(tracks, 70);
    let sources = Constants::new(210);
    // The single-session path refuses it (score MAX_TRACKS = 64) before copying samples.
    assert_eq!(
        build_timeline_mix(&seq, 48_000, 480, &sources, &Extent, &cancel).err(),
        Some(PulseError::LimitExceeded)
    );
    let mut m = TimelineMixStream::new(&seq, 48_000, 480, Constants::new(210), &Extent, StreamLimits::default(), &cancel).unwrap();
    assert_eq!(m.clip_count(), 210);
    let total = 70 * SPF;
    let (l, r) = render_all(&mut m, total, 480);
    let c = std::f64::consts::FRAC_1_SQRT_2;
    let mut worst = 0.0f64;
    for i in 0..total {
        let k = i / SPF; // the frame: clip k of each track
        let v = |t: usize| ((t * 70 + k) as f64 + 1.0) / 1000.0;
        let want_l = v(0) * 1.0 * c + v(1) * 0.5;
        let want_r = v(0) * 1.0 * c + v(2) * 0.25;
        worst = worst.max((f64::from(l[i]) - want_l).abs()).max((f64::from(r[i]) - want_r).abs());
    }
    assert!(worst <= 1e-6, "worst {worst}");
    let st = m.stats();
    // Sorted by start, 64 per group: 4 groups, built lazily, at most two resident at once.
    assert_eq!(st.groups, 4);
    assert_eq!(st.sessions_built, 4);
    assert!(st.max_active_sessions <= 2, "{st:?}");
    assert_eq!(m.position(), total as u64);
    assert_eq!(m.sample_position(), total as u64);

    // Seek into the middle: the groups are rebuilt and the samples equal the linear render.
    m.seek(30 * SPF as u64 + 7);
    let (l2, _) = render_all(&mut m, 5 * SPF, 333);
    for (i, s) in l2.iter().enumerate() {
        assert_eq!(*s, l[30 * SPF + 7 + i], "sample {}", 30 * SPF + 7 + i);
    }
    assert!(m.stats().discarded_frames > 0);

    // Hostile limits: clip cap, resident cap, group size, block size.
    let small = StreamLimits { max_clips: 100, ..StreamLimits::default() };
    assert_eq!(TimelineMixStream::new(&seq, 48_000, 480, Constants::new(210), &Extent, small, &cancel).err().map(|e| e.code()), Some("limit_exceeded"));
    let tight = StreamLimits { max_resident_frames: 1000, ..StreamLimits::default() };
    assert_eq!(TimelineMixStream::new(&seq, 48_000, 480, Constants::new(210), &Extent, tight, &cancel).err().map(|e| e.code()), Some("limit_exceeded"));
    for group_clips in [0, 65] {
        let bad = StreamLimits { group_clips, ..StreamLimits::default() };
        assert_eq!(TimelineMixStream::new(&seq, 48_000, 480, Constants::new(210), &Extent, bad, &cancel).err().map(|e| e.code()), Some("invalid_input"));
    }
    let mut big = vec![0.0; 481];
    let mut big2 = vec![0.0; 481];
    assert_eq!(m.render(&mut big, &mut big2, &cancel), Err(PulseError::InvalidInput));
    // A missing source is refused while planning, before any session exists.
    assert_eq!(TimelineMixStream::new(&seq, 48_000, 480, Constants::new(10), &Extent, StreamLimits::default(), &cancel).err().map(|e| e.code()), Some("source_extent_unknown"));
}

#[test]
fn streamed_groups_equal_the_single_session_mix_with_crossfades() {
    let cancel = CancellationToken::default();
    // 40 clips on two tracks, 2-frame clips abutting, every cut on track 1 a 2-frame linear
    // audio crossfade; grouped 7 at a time the stream must equal the one-session mix.
    let mut n = 0u32;
    let mut a1 = Vec::new();
    for k in 0..20u64 {
        n += 1;
        let mut c = clip(n, &format!("s{}", n - 1), 1, 2, 2 * k);
        if k < 19 {
            c.transitions.tail = Some(TimelineTransition {
                schema_id: TransitionSchema::V1,
                transition_id: format!("xf{k}"),
                transition_kind: AUDIO_CROSSFADE.into(),
                duration: Ticks::new(2 * F),
                alignment: TransitionAlignment::CenteredOnCut,
                center_offset: 0,
                parameters: vec![TransitionParameter {
                    param_key: CROSSFADE_CURVE.into(),
                    param_value: TransitionParameterValue::Enumeration("linear".into()),
                }],
                repeat_end_frame: false,
            });
        }
        a1.push(c);
    }
    let a2: Vec<StudioClip> = (0..20u64)
        .map(|k| {
            n += 1;
            clip(n, &format!("s{}", n - 1), 0, 2, 2 * k)
        })
        .collect();
    let seq = sequence(vec![track(1, a1, 0.8, 0.0), track(2, a2, 1.0, 0.3)], 40);
    let sources = Constants::new(40);
    let mut one = build_timeline_mix(&seq, 48_000, 480, &sources, &Extent, &cancel).unwrap();
    let total = 40 * SPF;
    let (mut l1, mut r1) = (vec![0.0f32; total], vec![0.0f32; total]);
    for start in (0..total).step_by(480) {
        one.render(start as u64, &mut l1[start..start + 480], &mut r1[start..start + 480], &cancel).unwrap();
    }
    let limits = StreamLimits { group_clips: 7, ..StreamLimits::default() };
    let mut m = TimelineMixStream::new(&seq, 48_000, 480, Constants::new(40), &Extent, limits, &cancel).unwrap();
    assert_eq!(m.stats().groups, 6);
    let (l2, r2) = render_all(&mut m, total, 480);
    let worst = l1.iter().zip(&l2).chain(r1.iter().zip(&r2)).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst <= 1e-6, "worst {worst}");
    assert!(l1.iter().any(|s| *s != 0.0));
}
