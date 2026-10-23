//! SMB-SCORE-EFFECTS-5 slice 1: clip speed by band-limited windowed-sinc interpolation. Positions
//! are checked against the analytic source, and band-limiting against the stated stopband
//! (design 100 dB; the asserted floor is 95 dB, the measured values are printed).
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

fn speed(n: u64, d: u64, reverse: bool) -> ClipSpeed {
    ClipSpeed::new(n, d, reverse).unwrap()
}

fn render(source: &[f32], start: usize, frames: usize, s: ClipSpeed) -> Vec<f32> {
    let mut out = vec![0.0_f32; s.output_frames(frames as u64) as usize];
    render_at_speed(source, start, frames, s, &mut out).unwrap();
    out
}

/// `amplitude * sin(2 pi f n)` with `f` in cycles per source sample.
fn tone(f: f64, amplitude: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|n| (amplitude * (2.0 * PI * f * n as f64).sin()) as f32).collect()
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

#[test]
fn every_output_frame_is_the_band_limited_source_at_its_exact_rational_position() {
    bounded_case("varispeed_positions", || {
        // 100 Hz at 48 kHz = 1/480 cycle per sample (inside the passband even at speed 16, whose
        // passband ends at 0.9025/16 of the source Nyquist); window 10_000..20_000 inside 40_000
        // samples (handles on both sides).
        let f = 1.0 / 480.0;
        let source = tone(f, 0.5, 40_000);
        for (n, d, reverse) in [(3, 2, false), (1, 3, false), (5, 4, true), (7, 3, false), (16, 1, true), (1, 16, false)] {
            let s = speed(n, d, reverse);
            let out = render(&source, 10_000, 10_000, s);
            assert_eq!(out.len() as u64, (10_000 * d).div_ceil(n), "{n}/{d}");
            let mut worst = 0.0_f64;
            for (m, v) in out.iter().enumerate() {
                let p = m as f64 * n as f64 / d as f64;
                let pos = if reverse { 19_999.0 - p } else { 10_000.0 + p };
                let want = 0.5 * (2.0 * PI * f * pos).sin();
                worst = worst.max((f64::from(*v) - want).abs());
            }
            println!("speed {n}/{d} reverse {reverse}: worst error {worst:.3e}");
            assert!(worst < 2e-5, "{n}/{d}: {worst}");
        }
        // Speed 1 forward is an exact copy; speed 1 reverse an exact reversed copy.
        assert_eq!(bits(&render(&source, 10_000, 500, ClipSpeed::UNITY)), bits(&source[10_000..10_500]));
        let mut backwards = source[10_000..10_500].to_vec();
        backwards.reverse();
        assert_eq!(bits(&render(&source, 10_000, 500, speed(1, 1, true))), bits(&backwards));
    });
}

#[test]
fn band_limiting_meets_the_stated_stopband_and_passband() {
    bounded_case("varispeed_band_limit", || {
        let frames = 30_000;
        let rms_ratio_db = |out: &[f32], amplitude: f64| db(rms(&out[2000..out.len() - 2000]) * 2f64.sqrt() / amplitude);
        // Speed 2: output Nyquist is 0.5 of the source Nyquist. A tone at 0.6 source Nyquist
        // (0.3 cycles/sample) would alias; it must fall at least 95 dB (design 100 dB).
        let out = render(&tone(0.3, 0.5, 3 * frames), frames, frames, speed(2, 1, false));
        let alias = rms_ratio_db(&out, 0.5);
        println!("speed 2: 0.6-Nyquist tone at {alias:.1} dB");
        assert!(alias < -95.0, "{alias}");
        // Passband edge 0.9025 of the output Nyquist: a tone at 0.84 of it (0.42 source Nyquist)
        // keeps its amplitude within 2e-4 dB (Kaiser ripple 1e-5).
        let out = render(&tone(0.21, 0.5, 3 * frames), frames, frames, speed(2, 1, false));
        let pass = rms_ratio_db(&out, 0.5);
        println!("speed 2: 0.84-output-Nyquist tone at {pass:.6} dB");
        assert!(pass.abs() < 2e-4, "{pass}");
        // Speed 1/2: the interpolation images of a 0.8-Nyquist tone (output 0.2 cycles/sample,
        // image at 0.3) are suppressed: everything but the 0.2 component is below -95 dB.
        let out = render(&tone(0.4, 0.5, 3 * frames), frames, frames, speed(1, 2, false));
        let body = &out[2000..out.len() - 2000];
        let fitted: Vec<f32> = (0..body.len())
            .map(|k| {
                let m = (k + 2000) as f64;
                (0.5 * (2.0 * PI * 0.4 * (frames as f64 + m / 2.0)).sin()) as f32
            })
            .collect();
        let residual: Vec<f32> = body.iter().zip(&fitted).map(|(a, b)| a - b).collect();
        let image = db(rms(&residual) * 2f64.sqrt() / 0.5);
        println!("speed 1/2: residual beside the wanted tone at {image:.1} dB");
        assert!(image < -90.0, "{image}");
        // Speed 1/2 stopband: a tone at 0.999 source Nyquist is removed (stopband from 0.9976).
        let out = render(&tone(0.4995, 0.5, 3 * frames), frames, frames, speed(1, 2, false));
        let stop = rms_ratio_db(&out, 0.5);
        println!("speed 1/2: 0.999-Nyquist tone at {stop:.1} dB");
        assert!(stop < -95.0, "{stop}");
        // The stated cutoffs.
        assert_eq!(speed(2, 1, false).cutoff(), 0.475);
        assert_eq!(speed(1, 2, false).cutoff(), VARISPEED_CUTOFF);
    });
}

#[test]
fn session_clips_at_speed_and_refusals() {
    bounded_case("varispeed_session", || {
        let src = vec![noise(1, 20_000), noise(2, 20_000)];
        let s = speed(3, 4, false);
        let mut t = TrackSpec::from_source_at_speed(&src, 5000, 9000, s, 100).unwrap();
        assert_eq!(t.clip[0].len(), 12_000);
        let clip = t.clip.clone();
        t.fade_out = Some(Fade { frames: 500, curve: FadeCurve::Linear });
        let mut session = Session::new(RATE, 1000, vec![t]).unwrap();
        let token = CancellationToken::default();
        let (mut l, mut r) = (vec![0.0_f32; 1000], vec![0.0_f32; 1000]);
        session.render(0, &mut l, &mut r, &token).unwrap();
        assert_eq!(bits(&l[100..]), bits(&clip[0][..900]));
        assert_eq!(bits(&r[100..]), bits(&clip[1][..900]));

        for (n, d) in [(0, 1), (1, 0), (17, 1), (1, 17), (65_537, 65_537), (u64::MAX, 1)] {
            assert_eq!(ClipSpeed::new(n, d, false).err(), Some(ScoreError::OutOfRange), "{n}/{d}");
        }
        assert_eq!((speed(6, 4, false).numerator(), speed(6, 4, false).denominator()), (3, 2));
        let x = vec![0.1_f32; 100];
        let mut out = vec![0.0_f32; 67];
        assert_eq!(render_at_speed(&x, 0, 100, speed(3, 2, false), &mut out[..66]), Err(ScoreError::LengthMismatch));
        assert_eq!(render_at_speed(&x, 50, 51, speed(3, 2, false), &mut out), Err(ScoreError::OutOfRange));
        assert_eq!(render_at_speed(&x, 0, 0, speed(3, 2, false), &mut out[..0]), Err(ScoreError::OutOfRange));
        assert_eq!(render_at_speed(&x, usize::MAX, 2, speed(3, 2, false), &mut out), Err(ScoreError::OutOfRange));
        let mut bad = x.clone();
        bad[99] = f32::NAN;
        assert_eq!(render_at_speed(&bad, 0, 100, speed(3, 2, false), &mut out), Err(ScoreError::NotFinite));
        assert_eq!(TrackSpec::from_source_at_speed(&[], 0, 1, s, 0).err(), Some(ScoreError::InvalidLayout));
        assert_eq!(TrackSpec::from_source_at_speed(&[x.clone(), x.clone(), x], 0, 1, s, 0).err(), Some(ScoreError::InvalidLayout));
    });
}
