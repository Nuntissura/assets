//! Lane SMB-AUDITION-SESSION-2: the timeline mixed through submix buses, sends and the master from
//! a Folio `StudioAudioRouting`, with clip gain envelopes. Oracles: hand-computed sample values of
//! constant and impulse clips through the stated laws (stereo balance, mono equal-power, linear-in-dB
//! envelopes), and pulse's own `TimelineMixStream` for an all-to-master routing.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use hsk_studio_score::{EffectSlot, LimiterParams, LookaheadLimiter};

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

fn source(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() } }
}
fn clip(n: u32, src: &str, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: source(src),
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>, volume: f64, pan: f64) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind,
        index: 0,
        name: format!("T{n}"),
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
fn id(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}

/// "dc": stereo 0.25; "imp": mono, 0.5 at sample 1000, else 0; "dcm": mono 0.125.
#[derive(Clone)]
struct Sources(std::sync::Arc<Vec<(String, Vec<Vec<f32>>)>>);
impl Sources {
    fn new() -> Self {
        let n = 12 * SPF;
        let mut imp = vec![0.0; n];
        imp[1000] = 0.5;
        Self(std::sync::Arc::new(vec![
            ("dc".into(), vec![vec![0.25; n], vec![0.25; n]]),
            ("imp".into(), vec![imp]),
            ("dcm".into(), vec![vec![0.125; n]]),
        ]))
    }
}
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else { return None };
        self.0.iter().find(|(n, _)| n == id).map(|(_, v)| v.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(12 * F))
    }
}

fn sequence(tracks: Vec<StudioTrack>) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(10 * F),
        tracks,
        markers: Vec::new(),
        work_area: None,
    }
}

/// The limiter far above the signal is a pure delay (gain exactly 1).
fn factory() -> impl FnMut(&StudioAudioEffectInstance, u32) -> std::result::Result<EffectSlot, PulseError> {
    |e: &StudioAudioEffectInstance, rate: u32| {
        assert_eq!(e.filter_kind, "lookahead_limiter");
        Ok(EffectSlot::new(Box::new(LookaheadLimiter::new(rate, 2, LimiterParams { ceiling_db: 0.0, lookahead_ms: 3.0, release_ms: 50.0, true_peak: false }).unwrap())))
    }
}

fn render(stream: &mut TimelineBusStream, frames: usize, block: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    let mut at = 0;
    while at < frames {
        let n = block.min(frames - at);
        stream.render(&mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
        at += n;
    }
    [l, r]
}

#[test]
fn timeline_plays_through_submix_buses_sends_and_clip_gain_envelopes() {
    let token = CancellationToken::default();
    let seq = sequence(vec![
        // A: stereo DC 0.25 at volume 0.5 -> submix 3 (latent bus effect).
        track(1, TrackKind::Audio, vec![clip(1, "dc", 10, 0)], 0.5, 0.0),
        // B: mono impulse to the master, pre-fader send at level 0.5 to submix 3.
        track(2, TrackKind::Audio, vec![clip(2, "imp", 10, 0)], 1.0, 0.0),
        track(3, TrackKind::Submix, Vec::new(), 1.0, 0.0),
    ]);
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.routes = vec![
        StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] },
        StudioTrackRoute {
            track_id: id(2),
            output: None,
            sends: vec![StudioAudioSend { send_id: "s1".into(), target: id(3), level: 0.5, pre_fader: true, pan: 0.0 }],
        },
    ];
    routing.bus_effects = vec![StudioBusEffects {
        track_id: id(3),
        effects: vec![StudioAudioEffectInstance { instance_id: "fx".into(), filter_kind: "lookahead_limiter".into(), enabled: true, params: vec![], keyframes: vec![] }],
    }];
    // Clip A: 0 dB at its start falling to -12 dB at 5 frames, linear in dB, then held.
    routing.clip_gains = vec![StudioClipGainEnvelope {
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: 0.0 }, StudioClipGainPoint { offset_ticks: 5 * F, gain_db: -12.0 }],
    }];
    let mut f = factory();
    let mut stream = TimelineBusStream::new(&seq, &routing, RATE, 960, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    assert!(stream.latency().total > 0);
    let frames = 10 * SPF;
    let [l, r] = render(&mut stream, frames, 960);
    let c = std::f64::consts::FRAC_1_SQRT_2;
    let a_track_db = 20.0 * 0.5f64.log10();
    for k in [0usize, 999, 1000, 4800, 9600, 12_000, 18_000] {
        let env = if k < 5 * SPF { -12.0 * k as f64 / (5 * SPF) as f64 } else { -12.0 };
        let a = 0.25 * 10f64.powf((a_track_db + env) / 20.0);
        let b = if k == 1000 { 0.5 * c + 0.5 * 0.5 * c } else { 0.0 };
        assert!((f64::from(l[k]) - (a + b)).abs() < 2e-6, "L {k}: {} vs {}", l[k], a + b);
        assert!((f64::from(r[k]) - (a + b)).abs() < 2e-6, "R {k}");
    }
    // Seek lands on the continuous render.
    let mut f = factory();
    let mut again = TimelineBusStream::new(&seq, &routing, RATE, 960, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    again.seek(9000, &token).unwrap();
    let (mut sl, mut sr) = (vec![0.0; 960], vec![0.0; 960]);
    again.render(&mut sl, &mut sr, &token).unwrap();
    assert_eq!(sl, l[9000..9960]);
    assert_eq!(sr, r[9000..9960]);
    assert_eq!(AudioClock::sample_position(&again), 9960);
    // A muted submix silences what reaches it.
    let mut muted = seq.clone();
    muted.tracks[2].muted = true;
    let mut f = factory();
    let mut m = TimelineBusStream::new(&muted, &routing, RATE, 960, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    let [ml, _] = render(&mut m, 2000, 960);
    assert!((f64::from(ml[1000]) - (0.5 * c + 0.25 * 10f64.powf((a_track_db - 96.0 - 12.0 * 1000.0 / 9600.0) / 20.0) + 0.25 * c * 10f64.powf(-96.0 / 20.0))).abs() < 1e-5, "{}", ml[1000]);
}

#[test]
fn all_to_master_equals_the_plain_mix_and_refusals_are_typed() {
    let token = CancellationToken::default();
    // Mixed mono and stereo clips on one track, pan and volume, solo on another.
    let seq = sequence(vec![
        track(1, TrackKind::Audio, vec![clip(1, "dc", 3, 0), clip(2, "dcm", 3, 4)], 0.7, 0.4),
        track(2, TrackKind::Audio, vec![clip(3, "imp", 8, 1)], 0.9, -0.3),
    ]);
    let routing = StudioAudioRouting::new("seq-main");
    let mut f = factory();
    let mut buses = TimelineBusStream::new(&seq, &routing, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    let mut plain = TimelineMixStream::new(&seq, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &token).unwrap();
    let frames = 9 * SPF;
    let [bl, br] = render(&mut buses, frames, 480);
    let (mut pl, mut pr) = (vec![0.0; frames], vec![0.0; frames]);
    for at in (0..frames).step_by(480) {
        plain.render(&mut pl[at..at + 480], &mut pr[at..at + 480], &token).unwrap();
    }
    let worst = bl.iter().zip(&pl).chain(br.iter().zip(&pr)).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst <= 1e-6, "bus graph vs plain mix: {worst}");
    assert!(pl.iter().any(|v| *v != 0.0));
    // Refusals: routing of another sequence, unknown bus, mono/stereo crossfade.
    let mut f = factory();
    let other = StudioAudioRouting::new("seq-other");
    assert_eq!(
        TimelineBusStream::new(&seq, &other, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).err(),
        Some(PulseError::InvalidInput)
    );
    let mut unknown = StudioAudioRouting::new("seq-main");
    unknown.routes = vec![StudioTrackRoute { track_id: id(1), output: Some(id(9)), sends: vec![] }];
    assert_eq!(
        TimelineBusStream::new(&seq, &unknown, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).err(),
        Some(PulseError::InvalidInput)
    );
    let mut xf = seq.clone();
    xf.tracks[0].clips[1].timeline_start = Ticks::new(3 * F);
    xf.tracks[0].clips[0].transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "x1".into(),
        transition_kind: "audio_crossfade".into(),
        duration: Ticks::new(F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: vec![],
        repeat_end_frame: false,
    });
    assert_eq!(
        TimelineBusStream::new(&xf, &routing, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).err(),
        Some(PulseError::AudioUnsupported)
    );
    // Nothing audible: a silent, clocked graph.
    let mut silent = seq.clone();
    silent.tracks.iter_mut().for_each(|t| t.muted = true);
    let mut s = TimelineBusStream::new(&silent, &routing, RATE, 480, Sources::new(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    let [sl, _] = render(&mut s, 960, 480);
    assert!(sl.iter().all(|v| *v == 0.0));
    assert_eq!(s.position(), 960);
}
