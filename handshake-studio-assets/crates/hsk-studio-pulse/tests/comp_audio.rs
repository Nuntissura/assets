//! Lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 2: composition audio (STU-MOT-016 `audio_levels`, a
//! keyframable stereo-pair level stream on the ordinary keyframe model) mixed through score, the
//! per-frame amplitude series of "convert audio to keyframes", and composition preview clocked by
//! the audio master through pulse's scheduler (PR-19). Every sample is checked against hand values
//! (`10^(dB/20)` gains, linear in dB between keys, held keys); After Effects parity UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_pulse::*;
use std::sync::Arc;

const RATE: u32 = 48_000;
const PER: u64 = TICKS_PER_SECOND / 48_000; // 5_292_000 ticks per sample
const TPF: u64 = TICKS_PER_SECOND / 30; // 1600 samples per frame

fn s(sample: u64) -> Ticks {
    Ticks::new(sample * PER)
}

fn db(v: f64) -> f64 {
    10f64.powf(v / 20.0)
}

fn layers() -> Vec<CompAudioLayer> {
    // A: stereo (L 0.5, R -0.25), 2 s of source at 0.5 s, audible to 2.0 s; levels (L, R): at
    // 1.0 s (0, -12) linear, at 1.5 s (-20, 0) held, at 1.75 s (-6, -6).
    let mut a = CompAudioLayer::new("SLYR-a", vec![vec![0.5; 96_000], vec![-0.25; 96_000]], 24_000 * PER, RATE).unwrap();
    a.out_point = s(96_000);
    a.levels = vec![
        LevelKey { tick: s(48_000), db: [0.0, -12.0], hold: false },
        LevelKey { tick: s(72_000), db: [-20.0, 0.0], hold: true },
        LevelKey { tick: s(84_000), db: [-6.0, -6.0], hold: false },
    ];
    // B: mono 0.25, 0.5 s at 1.0 s, unity.
    let b = CompAudioLayer::new("SLYR-b", vec![vec![0.25; 24_000]], 48_000 * PER, RATE).unwrap();
    vec![a, b]
}

fn render_all(stream: &mut CompAudioStream, n: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let c = CancellationToken::default();
    for at in (0..n).step_by(480) {
        let k = 480.min(n - at);
        stream.render(&mut l[at..at + k], &mut r[at..at + k], &c).unwrap();
    }
    (l, r)
}

fn close(got: f32, want: f64, what: &str) {
    assert!((f64::from(got) - want).abs() < 1e-6, "{what}: {got} vs {want}");
}

#[test]
fn composition_audio_layers_mix_through_score_with_stereo_level_keyframes() {
    let c = CancellationToken::default();
    let mut st = CompAudioStream::new(&layers(), RATE, 480, &c).unwrap();
    assert_eq!(st.layer_count(), 2);
    assert_eq!(st.length_samples(), 96_000);
    let (l, r) = render_all(&mut st, 100_800);
    // Before A's start: silence.
    close(l[1000], 0.0, "l 1000");
    // A only, before the first key: the first key's value holds (L 0 dB, R -12 dB).
    close(l[30_000], 0.5, "l 30000");
    close(r[30_000], -0.25 * db(-12.0), "r 30000");
    // A + B at 50000: linear in dB over 48000..72000 (L 0 -> -20, R -12 -> 0); B at unity on both
    // sides (a mono layer feeds both sides).
    let dl = -20.0 * 2000.0 / 24_000.0;
    let dr = -12.0 + 12.0 * 2000.0 / 24_000.0;
    close(l[50_000], 0.5 * db(dl) + 0.25, "l 50000");
    close(r[50_000], -0.25 * db(dr) + 0.25, "r 50000");
    // A alone from 72000 (B ended): the held key holds to one sample before the next key.
    close(l[72_000], 0.5 * db(-20.0), "l 72000");
    close(r[72_000], -0.25, "r 72000");
    close(l[83_999], 0.5 * db(-20.0), "l 83999 held");
    close(r[83_999], -0.25, "r 83999 held");
    close(l[84_000], 0.5 * db(-6.0), "l 84000");
    close(r[95_999], -0.25 * db(-6.0), "r 95999 last key holds");
    // Out point: silence after 96000.
    close(l[96_000], 0.0, "l 96000");
    close(r[100_000], 0.0, "r 100000");

    // Seek: a stream moved to 50000 renders what continuous play rendered there.
    st.seek(50_000).unwrap();
    assert_eq!(st.position(), 50_000);
    let (sl, sr) = render_all(&mut st, 960);
    for (got, want) in sl.iter().zip(&l[50_000..]).chain(sr.iter().zip(&r[50_000..])) {
        close(*got, f64::from(*want), "seek");
    }

    // Audio switch off on A: only B; solo on A: only A.
    let mut ls = layers();
    ls[0].audio_enabled = false;
    let mut st = CompAudioStream::new(&ls, RATE, 480, &c).unwrap();
    let (l2, _) = render_all(&mut st, 52_800);
    close(l2[30_000], 0.0, "A off");
    close(l2[50_000], 0.25, "B alone");
    let mut ls = layers();
    ls[0].solo = true;
    let mut st = CompAudioStream::new(&ls, RATE, 480, &c).unwrap();
    assert_eq!(st.layer_count(), 1);
    let (l3, _) = render_all(&mut st, 52_800);
    close(l3[50_000], 0.5 * db(dl), "A solo");
}

#[test]
fn audio_amplitude_series_and_preview_stay_on_the_audio_clock() {
    let c = CancellationToken::default();
    // L 0.5 for 1 s from 0, R silent; 35 frames at 30 fps.
    let tone = CompAudioLayer::new("SLYR-tone", vec![vec![0.5; 48_000], vec![0.0; 48_000]], 0, RATE).unwrap();
    let mut st = CompAudioStream::new(&[tone], RATE, 400, &c).unwrap();
    let amp = audio_amplitude(&mut st, TPF, 35, &c).unwrap();
    assert_eq!(amp.left.len(), 35);
    for (f, ((l, r), b)) in amp.left.iter().zip(&amp.right).zip(&amp.both).enumerate() {
        if f < 30 {
            assert!((l - 50.0).abs() < 1e-9 && *r == 0.0 && (b - 25.0).abs() < 1e-9, "frame {f}: {l} {r} {b}");
        } else {
            assert_eq!((*l, *r, *b), (0.0, 0.0, 0.0), "frame {f}");
        }
    }
    assert_eq!(st.position(), 35 * 1600, "the series renders exactly the 35 frames");

    // Preview: the composition scheduler presents every frame within one frame of the audio clock.
    let mut sched = composition_scheduler("SCMP-main", TPF, Ticks::new(35 * TPF), DropPolicy::DropLate).unwrap();
    st.seek(0).unwrap();
    sched.play(&st).unwrap();
    let (mut l, mut r) = (vec![0.0f32; 400], vec![0.0f32; 400]);
    let mut presented = Vec::new();
    for _ in 0..(35 * 1600 / 400) {
        st.render(&mut l, &mut r, &c).unwrap();
        if let FrameAction::Present { frame, tick } = sched.poll(&st).unwrap().action {
            let master = st.position() * PER;
            assert!(master >= tick.value() && master - tick.value() < TPF, "frame {frame}");
            presented.push(frame);
        }
    }
    assert_eq!(presented, (0..35).collect::<Vec<u64>>(), "every frame, none dropped");
    // Scrub to frame 20 and seek the audio there: playback resumes at frame 20 in sync.
    sched.pause(&st).unwrap();
    sched.scrub(Ticks::new(20 * TPF), &st).unwrap();
    st.seek(20 * 1600).unwrap();
    sched.play(&st).unwrap();
    st.render(&mut l, &mut r, &c).unwrap();
    assert!(l.iter().all(|v| *v == 0.5), "audio at frame 20 is the tone");
    match sched.poll(&st).unwrap().action {
        FrameAction::Present { frame, .. } => assert_eq!(frame, 20),
        other => panic!("{other:?}"),
    }
}

#[test]
fn hostile_composition_audio_is_refused_before_mixing() {
    let c = CancellationToken::default();
    let ok = layers();
    let new = |ls: &[CompAudioLayer]| CompAudioStream::new(ls, RATE, 480, &c).err();
    let mut ls = ok.clone();
    ls[0].source = Arc::new(vec![vec![0.5, f32::NAN]]);
    assert_eq!(new(&ls), Some(PulseError::InvalidInput), "NaN sample");
    let mut ls = ok.clone();
    ls[0].levels[0].db[1] = f64::NAN;
    assert_eq!(new(&ls), Some(PulseError::InvalidInput), "NaN level");
    let mut ls = ok.clone();
    ls[0].levels[1].db[0] = 40.0;
    assert_eq!(new(&ls), Some(PulseError::InvalidInput), "level above +12 dB");
    let mut ls = ok.clone();
    ls[0].levels.swap(1, 2);
    assert_eq!(new(&ls), Some(PulseError::UnorderedRanges), "keys out of order");
    let mut ls = ok.clone();
    ls[1].source = Arc::new(vec![vec![0.1; 8]; 3]);
    assert_eq!(new(&ls), Some(PulseError::AudioUnsupported), "three channels");
    let mut ls = ok.clone();
    ls[1].start = 1;
    assert_eq!(new(&ls), Some(PulseError::NotFrameAligned), "start between samples");
    let mut ls = ok.clone();
    ls[0].levels[0].hold = false;
    ls[0].audio_enabled = false;
    ls[0].levels[1].db[0] = f64::INFINITY;
    assert_eq!(new(&ls), Some(PulseError::InvalidInput), "a disabled layer's keys are still checked");
    // 33 audible layers: more than one score session mixes.
    let many: Vec<CompAudioLayer> =
        (0..33).map(|i| CompAudioLayer::new(&format!("SLYR-{i}"), vec![vec![0.01; 16]], 0, RATE).unwrap()).collect();
    assert_eq!(new(&many), Some(PulseError::LimitExceeded));
    // Amplitude bombs and degenerate frames.
    let mut st = CompAudioStream::new(&ok, RATE, 480, &c).unwrap();
    assert_eq!(audio_amplitude(&mut st, TPF, MAX_AMPLITUDE_FRAMES + 1, &c).err(), Some(PulseError::LimitExceeded));
    assert_eq!(audio_amplitude(&mut st, 1, 4, &c).err(), Some(PulseError::InvalidInput), "frame shorter than a sample");
    let x = CancellationToken::default();
    x.cancel();
    assert_eq!(CompAudioStream::new(&ok, RATE, 480, &x).err(), Some(PulseError::Canceled));
}
