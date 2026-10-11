use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;
use std::f64::consts::PI;

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

const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";

fn ctx(revision: u64) -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(
            ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .unwrap(),
        ),
        expected_revision: revision,
    }
}
fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 1)
}
fn spec(layout: ChannelLayout, frames: usize, start_sample: u64, rate: u32) -> BlockSpec {
    BlockSpec {
        sample_rate_hz: rate,
        layout,
        frames,
        start_sample,
    }
}
/// Runs one processor over planar `channels`; returns the published channels.
fn run(
    proc: &mut dyn Processor,
    in_layout: ChannelLayout,
    out_layout: ChannelLayout,
    channels: &[&[f32]],
) -> Vec<Vec<f32>> {
    let frames = channels[0].len();
    let sp = spec(in_layout, frames, 0, 48_000);
    let mut out = OutputBlock::with_capacity(out_layout.clone(), frames).unwrap();
    engine()
        .process(&ctx(1), &InputBlock::new(&sp, channels), proc, &mut out, &CancellationToken::default())
        .unwrap();
    (0..out_layout.channels())
        .map(|c| out.published_channel(c).unwrap().to_vec())
        .collect()
}

#[test]
fn gain_pan_mix_exact_values() {
    bounded_case("gain_pan_mix_exact_values", || {
        let stereo = ChannelLayout::stereo;
        // Gain: powers of two are exact in f32.
        let l = [0.0_f32, 0.25, -0.25, 0.5];
        let r = [0.0_f32, -0.25, 0.25, -0.5];
        let half = run(&mut Gain::new(0.5).unwrap(), stereo(), stereo(), &[&l, &r]);
        assert_eq!(half[0], vec![0.0, 0.125, -0.125, 0.25]);
        assert_eq!(half[1], vec![0.0, -0.125, 0.125, -0.25]);
        let twice = run(&mut Gain::new(2.0).unwrap(), stereo(), stereo(), &[&l, &r]);
        assert_eq!(twice[0], vec![0.0, 0.5, -0.5, 1.0]);
        assert_eq!(Gain::new(f32::NAN).unwrap_err(), ScoreError::NotFinite);
        assert!((Gain::from_db(-6.0).unwrap().linear() - 0.501_187_2).abs() < 1e-6);

        // Pan: mono -> stereo, exact endpoints, -3 dB centre, constant power across the range.
        let mono = || ChannelLayout::mono(7);
        let src = [1.0_f32, -0.5, 0.25];
        let left = run(&mut Pan::new(-1.0).unwrap(), mono(), stereo(), &[&src]);
        assert_eq!(left[0], vec![1.0, -0.5, 0.25]);
        assert!(left[1].iter().all(|s| *s == 0.0));
        let right = run(&mut Pan::new(1.0).unwrap(), mono(), stereo(), &[&src]);
        assert!(right[0].iter().all(|s| *s == 0.0));
        assert_eq!(right[1], vec![1.0, -0.5, 0.25]);
        let centre = run(&mut Pan::new(0.0).unwrap(), mono(), stereo(), &[&src]);
        for (side, expected) in centre.iter().flat_map(|c| c.iter().zip(src)) {
            assert!((side - expected * std::f32::consts::FRAC_1_SQRT_2).abs() < 2e-7);
        }
        for step in -8..=8 {
            let (gl, gr) = Pan::new(step as f32 / 8.0).unwrap().gains();
            assert!((gl * gl + gr * gr - 1.0).abs() < 1e-6, "pan {step}/8");
        }
        assert_eq!(Pan::new(1.01).unwrap_err(), ScoreError::OutOfRange);

        // ChannelMix: stereo -> mono = 0.5 L + 0.5 R; mono -> stereo; custom swap matrix.
        let (ml, mr) = ([1.0_f32, 0.5], [0.5_f32, -0.5]);
        let down = run(&mut ChannelMix::stereo_to_mono(), stereo(), mono(), &[&ml, &mr]);
        assert_eq!(down[0], vec![0.75, 0.0]);
        let up = run(&mut ChannelMix::mono_to_stereo(), mono(), stereo(), &[&[0.25_f32, -0.5]]);
        assert_eq!((&up[0], &up[1]), (&vec![0.25, -0.5], &vec![0.25, -0.5]));
        let swap = ChannelMix::new(2, 2, &[0.0, 1.0, 1.0, 0.0]).unwrap();
        let swapped = run(&mut swap.clone(), stereo(), stereo(), &[&ml, &mr]);
        assert_eq!((&swapped[0], &swapped[1]), (&mr.to_vec(), &ml.to_vec()));
        assert_eq!(ChannelMix::new(2, 2, &[1.0; 3]).unwrap_err(), ScoreError::LengthMismatch);
        assert_eq!(ChannelMix::new(2, 2, &[1.0, 0.0, 0.0, f32::INFINITY]).unwrap_err(), ScoreError::NotFinite);
    });
}

fn sine(frames: usize, hz: f64, rate: f64, amplitude: f64) -> Vec<f32> {
    (0..frames)
        .map(|n| (amplitude * (2.0 * PI * hz * n as f64 / rate).sin()) as f32)
        .collect()
}

struct ToneFit {
    amplitude: f64,
    residual_rms: f64,
    lead_frames: f64,
}
/// Least-squares fit of `a sin(w n) + b cos(w n)` over `range`; `lead_frames` > 0 means the
/// output is ahead of the ideal (zero-phase) tone by that many output frames.
fn fit_tone(got: &[f32], hz: f64, rate: f64, range: std::ops::Range<usize>) -> ToneFit {
    let w = 2.0 * PI * hz / rate;
    let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for n in range.clone() {
        let (s, c) = ((w * n as f64).sin(), (w * n as f64).cos());
        let y = f64::from(got[n]);
        (ss, cc, sc, ys, yc) = (ss + s * s, cc + c * c, sc + s * c, ys + y * s, yc + y * c);
    }
    let det = ss * cc - sc * sc;
    let a = (ys * cc - yc * sc) / det;
    let b = (yc * ss - ys * sc) / det;
    let residual: f64 = range
        .clone()
        .map(|n| (f64::from(got[n]) - a * (w * n as f64).sin() - b * (w * n as f64).cos()).powi(2))
        .sum();
    ToneFit {
        amplitude: (a * a + b * b).sqrt(),
        residual_rms: (residual / range.len() as f64).sqrt(),
        lead_frames: b.atan2(a) / w,
    }
}

#[test]
fn resampler_preserves_counts_tone_and_dc() {
    bounded_case("resampler_preserves_counts_tone_and_dc", || {
        let engine = engine();
        let token = CancellationToken::default();
        let mono = || ChannelLayout::mono(7);

        // 48 kHz -> 44.1 kHz: 4800 frames -> exactly 4410; a 1 kHz tone stays a 1 kHz tone.
        let tone = sine(4800, 1000.0, 48_000.0, 0.5);
        let sp = spec(mono(), 4800, 0, 48_000);
        let channels: [&[f32]; 1] = [&tone];
        let mut resampler = ClipResampler::new(48_000, 44_100, 1, 1024).unwrap();
        let mut out = OutputBlock::with_capacity(mono(), resampler.output_capacity_for(4800)).unwrap();
        let receipt = engine
            .resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut resampler, &mut out, &token)
            .unwrap();
        assert_eq!(receipt.requested_output_frames, 4410);
        assert_eq!(receipt.actual_output_frames(), 4410, "requested vs actual must be reported exactly");
        assert_eq!((receipt.input_frames, receipt.input_rate_hz, receipt.block.sample_rate_hz), (4800, 48_000, 44_100));
        assert!(receipt.delay_trimmed_frames > 0);
        assert_eq!(out.published_sample_rate_hz(), Some(44_100));
        let got = out.published_channel(0).unwrap();
        assert_eq!(got.len(), 4410);
        // The output is a pure 1 kHz tone at half amplitude: least-squares fit `a sin + b cos`
        // over the clip interior leaves (almost) no residual, so nothing was aliased, distorted
        // or rescaled. The phase lead is the uncaptured sub-frame alignment (rubato trims
        // floor(256 * ratio / 2) output frames; measured 0.4..1.02 frames across tested ratios).
        let fit = fit_tone(got, 1000.0, 44_100.0, 400..4000);
        assert!((fit.amplitude - 0.5).abs() < 1e-3, "amplitude {}", fit.amplitude);
        assert!(fit.residual_rms < 1e-3, "residual {}", fit.residual_rms);
        assert!(fit.lead_frames.abs() < 1.6, "alignment {} frames", fit.lead_frames);

        // Up-conversion 32 kHz -> 48 kHz (x1.5) of a constant keeps the constant (DC gain 1).
        let dc = vec![0.5_f32; 3200];
        let sp = spec(mono(), 3200, 0, 32_000);
        let channels: [&[f32]; 1] = [&dc];
        let mut up = ClipResampler::new(32_000, 48_000, 1, 512).unwrap();
        let mut out = OutputBlock::with_capacity(mono(), up.output_capacity_for(3200)).unwrap();
        let receipt = engine
            .resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut up, &mut out, &token)
            .unwrap();
        assert_eq!((receipt.requested_output_frames, receipt.actual_output_frames()), (4800, 4800));
        assert!(out.published_channel(0).unwrap()[400..4400].iter().all(|s| (s - 0.5).abs() < 1e-3));

        // Timeline maps exactly or is refused: 48000 -> 44100 samples is exact, sample 1 is not.
        let sp = spec(mono(), 4800, 48_000, 48_000);
        let channels: [&[f32]; 1] = [&tone];
        let receipt = engine
            .resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut resampler, &mut out, &token)
            .unwrap();
        assert_eq!(receipt.block.start_sample, 44_100);
        let before = out.published_frames();
        let sp = spec(mono(), 4800, 1, 48_000);
        let refused = engine.resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut resampler, &mut out, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(out.published_frames(), before);

        // Refusals: wrong input rate, too-small output, bad construction.
        let sp = spec(mono(), 4800, 0, 44_100);
        let refused = engine.resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut resampler, &mut out, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::InvalidRate);
        let sp = spec(mono(), 4800, 0, 48_000);
        let mut tiny = OutputBlock::with_capacity(mono(), 4409).unwrap();
        let refused = engine.resample(&ctx(1), &InputBlock::new(&sp, &channels), &mut resampler, &mut tiny, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::CapacityExceeded);
        assert_eq!(tiny.published_frames(), 0);
        assert!(ClipResampler::new(47_999, 44_100, 1, 1024).is_err());
        assert!(ClipResampler::new(48_000, 44_100, 0, 1024).is_err());
        assert!(ClipResampler::new(48_000, 44_100, 1, 8).is_err());
    });
}

#[test]
fn resample_shares_engine_gates() {
    bounded_case("resample_shares_engine_gates", || {
        let engine = engine();
        let token = CancellationToken::default();
        let tone = sine(2048, 1000.0, 48_000.0, 0.5);
        let sp = spec(ChannelLayout::mono(7), 2048, 0, 48_000);
        let channels: [&[f32]; 1] = [&tone];
        let input = InputBlock::new(&sp, &channels);
        let mut resampler = ClipResampler::new(48_000, 44_100, 1, 512).unwrap();
        let mut out = OutputBlock::with_capacity(ChannelLayout::mono(7), resampler.output_capacity_for(2048)).unwrap();

        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(engine.resample(&ctx(1), &input, &mut resampler, &mut out, &canceled).unwrap_err(), ScoreError::Canceled);
        let mut no_actor = ctx(1);
        no_actor.actor = None;
        assert_eq!(engine.resample(&no_actor, &input, &mut resampler, &mut out, &token).unwrap_err(), ScoreError::MissingContext);
        assert_eq!(engine.resample(&ctx(2), &input, &mut resampler, &mut out, &token).unwrap_err(), ScoreError::StaleRevision);
        let held = engine.reserve().unwrap();
        assert_eq!(engine.resample(&ctx(1), &input, &mut resampler, &mut out, &token).unwrap_err(), ScoreError::Busy);
        drop(held);
        assert_eq!(out.published_frames(), 0, "no refusal may publish");
        assert!(out.backing().iter().all(|s| *s == 0.0), "no refusal may write");
        // The same resampler is reusable and deterministic after refusals.
        let first = engine.resample(&ctx(1), &input, &mut resampler, &mut out, &token).unwrap();
        let again = engine.resample(&ctx(1), &input, &mut resampler, &mut out, &token).unwrap();
        assert_eq!(first, again);
    });
}
