//! Lane SMB-AU-MULTITRACK-3: clip pan envelopes and master volume automation in the timeline mix.
//! Oracles: every output sample of constant clips compared with score's stated pan laws computed by
//! hand here (mono equal-power `(cos a, sin a)`, `a = (pan + 1) pi/4`; stereo balance
//! `L min(1, 1 - pan)`, `R min(1, 1 + pan)`) with the pan linear between the envelope points and
//! held outside them; the master lane by `10^(dB/20)` linear in dB.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use hsk_studio_score::EffectSlot;

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

fn source(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}
fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}
fn tid(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn clip(n: u32, src: &str, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
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
fn track(n: u32, clips: Vec<StudioClip>, pan: f64) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind: TrackKind::Audio,
        index: n,
        name: format!("A{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan,
    }
}
fn sequence(tracks: Vec<StudioTrack>) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(12 * F),
        tracks,
        markers: Vec::new(),
        work_area: None,
    }
}

/// "dc": stereo 0.25; "dcm": mono 0.125.
#[derive(Clone)]
struct Sources(std::sync::Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let n = 12 * SPF;
    Sources(std::sync::Arc::new(vec![("dc".into(), vec![vec![0.25; n], vec![0.25; n]]), ("dcm".into(), vec![vec![0.125; n]])]))
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

fn no_effects() -> impl FnMut(&StudioAudioEffectInstance, u32) -> std::result::Result<EffectSlot, PulseError> {
    |_: &StudioAudioEffectInstance, _: u32| Err(PulseError::InvalidInput)
}

fn pans(clip: u32, points: &[(u64, f64)]) -> StudioClipPanEnvelope {
    StudioClipPanEnvelope { clip_id: cid(clip), points: points.iter().map(|&(offset_ticks, pan)| StudioClipPanPoint { offset_ticks, pan }).collect() }
}

fn bus(seq: &StudioSequence, routing: &StudioAudioRouting, frames: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let mut f = no_effects();
    let mut s = TimelineBusStream::new(seq, routing, RATE, 960, sources(), &Ext, StreamLimits::default(), &mut f, &token).unwrap();
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    let mut at = 0;
    while at < frames {
        let n = 960.min(frames - at);
        s.render(&mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
        at += n;
    }
    [l, r]
}

/// The envelope -1 at offset 0 to +1 at offset 4 frames (held after) of a clip starting at frame 1,
/// at timeline sample `k` (`None` outside the clip of `frames` frames).
fn ramp(k: usize, frames: usize) -> Option<f64> {
    let local = k.checked_sub(SPF)?;
    if local >= frames * SPF {
        return None;
    }
    Some(if local < 4 * SPF { -1.0 + 2.0 * local as f64 / (4 * SPF) as f64 } else { 1.0 })
}

fn equal_power(pan: f64) -> (f64, f64) {
    let a = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}
fn balance(pan: f64) -> (f64, f64) {
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// Compares every sample of `[l, r]` with `want(k)`.
fn every_sample(got: &[Vec<f32>; 2], want: impl Fn(usize) -> (f64, f64), what: &str) {
    let mut worst = 0.0f64;
    for (k, (l, r)) in got[0].iter().zip(&got[1]).enumerate() {
        let (wl, wr) = want(k);
        let e = (f64::from(*l) - wl).abs().max((f64::from(*r) - wr).abs());
        assert!(e < 2e-6, "{what} sample {k}: ({l}, {r}) vs ({wl}, {wr})");
        worst = worst.max(e);
    }
    assert!(worst < 2e-6, "{what}: {worst}");
}

/// A stereo clip's pan envelope moves its balance sample by sample (stereo balance law), and a mono
/// clip's moves its equal-power position; the track pan then acts on the panned strip as a balance.
/// Points sit at offsets from the clip start; before the first point the first value holds (here the
/// clip starts at frame 1 with the point at its start), after the last the last value.
#[test]
fn clip_pan_envelopes_pan_the_mixdown_sample_by_sample() {
    let frames = 8;
    let total = 10 * SPF;
    let env = [(0, -1.0), (4 * F, 1.0)];
    // Stereo DC 0.25 on a centred track.
    let seq = sequence(vec![track(1, vec![clip(1, "dc", frames as u64, 1)], 0.0)]);
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.clip_pans = vec![pans(1, &env)];
    let out = bus(&seq, &routing, total);
    every_sample(&out, |k| ramp(k, frames).map_or((0.0, 0.0), |p| (0.25 * balance(p).0, 0.25 * balance(p).1)), "stereo balance");
    assert!(f64::from(out[1][SPF]).abs() < 1e-9 && f64::from(out[0][SPF + 7 * SPF]).abs() < 1e-9, "hard left at the head, hard right after frame 4");

    // Mono DC 0.125: equal-power, the same numbers score's pan_gains gives.
    let seq = sequence(vec![track(1, vec![clip(1, "dcm", frames as u64, 1)], 0.0)]);
    let out = bus(&seq, &routing, total);
    every_sample(&out, |k| ramp(k, frames).map_or((0.0, 0.0), |p| (0.125 * equal_power(p).0, 0.125 * equal_power(p).1)), "mono equal-power");
    let (gl, gr) = hsk_studio_score::pan_gains(0.25);
    assert!((equal_power(0.25).0 - gl).abs() < 1e-15 && (equal_power(0.25).1 - gr).abs() < 1e-15, "the hand law is score's pan law");

    // The same mono clip on a track panned 0.5: the track's balance on the panned strip.
    let seq = sequence(vec![track(1, vec![clip(1, "dcm", frames as u64, 1)], 0.5)]);
    let out = bus(&seq, &routing, total);
    let (bl, br) = balance(0.5);
    every_sample(&out, |k| ramp(k, frames).map_or((0.0, 0.0), |p| (0.125 * equal_power(p).0 * bl, 0.125 * equal_power(p).1 * br)), "mono clip pan then track balance");

    // Without an envelope the routing mixes as before (the unpanned mono strip at equal-power).
    routing.clip_pans.clear();
    let out = bus(&seq, &routing, total);
    let (el, er) = equal_power(0.5);
    every_sample(&out, |k| ramp(k, frames).map_or((0.0, 0.0), |_| (0.125 * el, 0.125 * er)), "no envelope");
}

/// The plain timeline mix: the clip lane is `clamp(track pan + envelope, -1, 1)` at the points, and
/// an empty envelope list renders exactly as `with_clip_gains`.
#[test]
fn plain_mix_adds_the_track_pan_and_empty_envelopes_change_nothing() {
    let token = CancellationToken::default();
    let frames = 8;
    let total = 10 * SPF;
    let seq = sequence(vec![track(1, vec![clip(1, "dc", frames as u64, 1)], 0.5)]);
    let render = |pans: &[StudioClipPanEnvelope]| {
        let mut s = TimelineMixStream::with_clip_envelopes(&seq, RATE, 480, sources(), &Ext, StreamLimits::default(), &[], pans, &token).unwrap();
        let (mut l, mut r) = (vec![0.0; total], vec![0.0; total]);
        for at in (0..total).step_by(480) {
            s.render(&mut l[at..at + 480], &mut r[at..at + 480], &token).unwrap();
        }
        [l, r]
    };
    // Points -1 -> +1 plus track 0.5: -0.5 at the head, clamped +1 at frame 4; linear between.
    let out = render(&[pans(1, &[(0, -1.0), (4 * F, 1.0)])]);
    every_sample(
        &out,
        |k| {
            ramp(k, frames).map_or((0.0, 0.0), |_| {
                let local = (k - SPF) as f64;
                let p = if local < (4 * SPF) as f64 { -0.5 + 1.5 * local / (4 * SPF) as f64 } else { 1.0 };
                (0.25 * balance(p).0, 0.25 * balance(p).1)
            })
        },
        "track pan + clip pan",
    );
    let mut old = TimelineMixStream::with_clip_gains(&seq, RATE, 480, sources(), &Ext, StreamLimits::default(), &[], &token).unwrap();
    let (mut l, mut r) = (vec![0.0; total], vec![0.0; total]);
    for at in (0..total).step_by(480) {
        old.render(&mut l[at..at + 480], &mut r[at..at + 480], &token).unwrap();
    }
    assert_eq!(render(&[]), [l, r], "no envelope: bit-identical to with_clip_gains");
}

/// Refusals: an envelope point off the sample grid; a crossfade between a panned and an unpanned
/// mono clip (two strips). A master volume lane ramps the master fader linearly in dB and replaces
/// the static master volume; its points must sit on the sample grid.
#[test]
fn clip_pan_refusals_and_the_master_volume_lane() {
    let token = CancellationToken::default();
    let seq = sequence(vec![track(1, vec![clip(1, "dcm", 4, 0), clip(2, "dcm", 4, 4)], 0.0)]);
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.clip_pans = vec![pans(1, &[(1, 0.0)])];
    let mut f = no_effects();
    assert_eq!(TimelineBusStream::new(&seq, &routing, RATE, 960, sources(), &Ext, StreamLimits::default(), &mut f, &token).err(), Some(PulseError::NotFrameAligned));
    let mut xf = seq.clone();
    // Clip 2 plays source [4, 8) so the crossfade has its head handle.
    xf.tracks[0].clips[1].source_in = Ticks::new(4 * F);
    xf.tracks[0].clips[1].source_out = Ticks::new(8 * F);
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
    routing.clip_pans = vec![pans(1, &[(0, 0.5)])];
    assert_eq!(TimelineBusStream::new(&xf, &routing, RATE, 960, sources(), &Ext, StreamLimits::default(), &mut f, &token).err(), Some(PulseError::AudioUnsupported));
    routing.clip_pans.push(pans(2, &[(0, 0.5)]));
    assert!(TimelineBusStream::new(&xf, &routing, RATE, 960, sources(), &Ext, StreamLimits::default(), &mut f, &token).is_ok(), "both panned: one strip");

    // Master: static 0.25 replaced by 1.0 at 0 falling to 0.5 at frame 4 (linear in dB), held after.
    let seq = sequence(vec![track(1, vec![clip(1, "dc", 8, 0)], 0.0)]);
    let mut routing = StudioAudioRouting::new("seq-main");
    routing.master.volume = 0.25;
    routing.master.volume_automation = vec![StudioAutomationPoint { offset_ticks: 0, value: 1.0 }, StudioAutomationPoint { offset_ticks: 4 * F, value: 0.5 }];
    let out = bus(&seq, &routing, 8 * SPF);
    let half_db = 20.0 * 0.5f64.log10();
    every_sample(
        &out,
        |k| {
            let db = if k < 4 * SPF { half_db * k as f64 / (4 * SPF) as f64 } else { half_db };
            let v = 0.25 * 10f64.powf(db / 20.0);
            (v, v)
        },
        "master lane",
    );
    routing.master.volume_automation = vec![StudioAutomationPoint { offset_ticks: 1, value: 1.0 }];
    let mut f = no_effects();
    assert_eq!(TimelineBusStream::new(&seq, &routing, RATE, 960, sources(), &Ext, StreamLimits::default(), &mut f, &token).err(), Some(PulseError::NotFrameAligned));
}
