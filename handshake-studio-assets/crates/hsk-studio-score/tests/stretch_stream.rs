//! SMB-SCORE-SPECTRAL slice 3: streaming time-stretch with transient detection and phase reset.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

/// Streams `x` through a stretcher in push chunks of `chunk` and pull buffers of `pull` samples.
fn stream(s: &mut StreamingStretch, x: &[f32], chunk: usize, pull: usize) -> Vec<f32> {
    let token = CancellationToken::default();
    let mut out = Vec::new();
    let mut buf = vec![0.0_f32; pull];
    let mut at = 0;
    loop {
        if at < x.len() {
            let end = (at + chunk).min(x.len());
            at += s.push(&x[at..end]).unwrap();
            if at == x.len() {
                s.finish();
            }
        }
        let got = s.pull(&mut buf, &token).unwrap();
        out.extend_from_slice(&buf[..got]);
        if got == 0 && at == x.len() {
            // Finished input and nothing more to pull.
            let again = s.pull(&mut buf, &token).unwrap();
            out.extend_from_slice(&buf[..again]);
            if again == 0 {
                break;
            }
        }
    }
    out
}

fn params() -> StretchParams {
    StretchParams::for_rate(RATE)
}

#[test]
fn streaming_without_transients_is_bit_identical_to_the_offline_stretch() {
    bounded_case("stretch_stream_equivalence", || {
        let token = CancellationToken::default();
        let tone = sine(330.0, 0.4, 0.2, RATE, 3 * RATE as usize);
        let x: Vec<f32> = tone.iter().zip(noise(8, tone.len())).map(|(t, n)| t + 0.05 * n).collect();
        for factor in [0.6, 1.5, 3.0] {
            let offline = time_stretch(&x, RATE, factor, params(), &token).unwrap();
            for (chunk, pull) in [(1usize, 7usize), (333, 4096), (4096, 1), (100_000, 100_000)] {
                let mut s = StreamingStretch::new(RATE, factor, params(), false).unwrap();
                let got = stream(&mut s, &x, chunk, pull);
                assert_eq!(got.len(), offline.len(), "{factor} {chunk}/{pull}");
                assert_eq!(bits(&got), bits(&offline), "{factor} {chunk}/{pull}");
            }
        }
    });
}

#[test]
fn phase_reset_at_onsets_bounds_the_smear_of_a_click_train() {
    bounded_case("stretch_stream_clicks", || {
        // 19 clicks every 4800 samples on a -80 dB floor, stretched by 1.5.
        let frames = 2 * RATE as usize;
        let mut x: Vec<f32> = noise(3, frames).iter().map(|v| v * 1e-4).collect();
        let clicks: Vec<usize> = (1..20).map(|i| i * 4800).collect();
        for c in &clicks {
            x[*c] += 1.0;
        }
        let measure = |transients: bool| {
            let mut s = StreamingStretch::new(RATE, 1.5, params(), transients).unwrap();
            let y = stream(&mut s, &x, 512, 512);
            assert_eq!(y.len(), (frames as f64 * 1.5).round() as usize);
            let (mut worst_conc, mut worst_shift) = (1.0_f64, 0_i64);
            for c in &clicks[1..clicks.len() - 1] {
                let expected = (*c as f64 * 1.5) as i64;
                let lo = (expected - 3000).max(0) as usize;
                let hi = ((expected + 3000) as usize).min(y.len());
                let seg = &y[lo..hi];
                let peak = (0..seg.len()).max_by(|a, b| seg[*a].abs().total_cmp(&seg[*b].abs())).unwrap();
                let energy = |r: std::ops::Range<usize>| r.map(|i| f64::from(seg[i]).powi(2)).sum::<f64>();
                let near = energy(peak.saturating_sub(32)..(peak + 33).min(seg.len()));
                worst_conc = worst_conc.min(near / energy(0..seg.len()));
                worst_shift = worst_shift.max(((lo + peak) as i64 - expected).abs());
            }
            (worst_conc, worst_shift, s.onsets())
        };
        let (with_conc, with_shift, onsets) = measure(true);
        let (plain_conc, plain_shift, _) = measure(false);
        println!("phase reset: worst energy within +-32 samples {with_conc:.4}, worst shift {with_shift}, onsets {onsets}");
        println!("plain vocoder: worst energy within +-32 samples {plain_conc:.4}, worst shift {plain_shift}");
        // Every click is an onset (the stream start from digital silence may add one).
        assert!((clicks.len() as u64..=clicks.len() as u64 + 1).contains(&onsets), "{onsets}");
        assert!(with_conc > 0.99, "{with_conc}");
        assert!(with_conc > plain_conc + 0.2, "{with_conc} vs {plain_conc}");
        assert!(with_shift <= 1024, "{with_shift}");
    });
}

#[test]
fn stationary_material_sees_no_onsets_and_hostile_streams_are_refused() {
    bounded_case("stretch_stream_hostile", || {
        let token = CancellationToken::default();
        let x = sine(440.0, 0.5, 0.0, RATE, RATE as usize);
        let mut on = StreamingStretch::new(RATE, 1.25, params(), true).unwrap();
        let mut off = StreamingStretch::new(RATE, 1.25, params(), false).unwrap();
        let (a, b) = (stream(&mut on, &x, 1000, 1000), stream(&mut off, &x, 1000, 1000));
        // A steady tone has no onset after its start from silence, and keeps its amplitude.
        println!("steady tone onsets {} last {:?}", on.onsets(), on.last_onset());
        assert!(on.onsets() <= 1, "{}", on.onsets());
        for y in [&a, &b] {
            let mid = &y[y.len() / 3..y.len() / 3 + 24_000];
            let amp = rms(mid) * 2f64.sqrt();
            assert!((db(amp / 0.5)).abs() < 0.2, "{amp}");
        }

        assert_eq!(StreamingStretch::new(RATE, f64::NAN, params(), true).err(), Some(ScoreError::NotFinite));
        assert_eq!(StreamingStretch::new(RATE, 5.0, params(), true).err(), Some(ScoreError::OutOfRange));
        assert_eq!(StreamingStretch::new(RATE, 1.5, StretchParams { fft_size: 1000, phase_locking: true }, true).err(), Some(ScoreError::OutOfRange));
        assert_eq!(StreamingStretch::new(44_101, 1.5, params(), true).err(), Some(ScoreError::InvalidRate));
        let mut s = StreamingStretch::new(RATE, 1.5, params(), true).unwrap();
        assert_eq!(s.push(&[0.0, f32::NAN]), Err(ScoreError::NotFinite));
        // Bounded ring: a huge push is only partly taken until output is pulled.
        let big = vec![0.1_f32; 1_000_000];
        let taken = s.push(&big).unwrap();
        assert!(taken < 100_000, "{taken}");
        let canceled = CancellationToken::default();
        canceled.cancel();
        let mut buf = vec![0.0_f32; 64];
        assert_eq!(s.pull(&mut buf, &canceled), Err(ScoreError::Canceled));
        assert!(s.pull(&mut buf, &token).unwrap() > 0);
        s.finish();
        assert_eq!(s.push(&[0.0]), Err(ScoreError::OutOfRange));
    });
}
