//! SMB-SCORE-EFFECTS-4 slice 3: click repair at the edges of digital silence. No fade-in: the
//! material starts, stops and restarts abruptly, and only real clicks may be touched.
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

const L: usize = CLICK_HOP + CLICK_CONTEXT;

fn tones(frames: usize, seed: u64) -> Vec<f32> {
    let a = sine(440.0, 0.3, 0.0, RATE, frames);
    let b = sine(1234.0, 0.2, 0.7, RATE, frames);
    let c = sine(3100.0, 0.1, 1.9, RATE, frames);
    let n = noise(seed, frames);
    (0..frames).map(|i| a[i] + b[i] + c[i] + 0.003 * n[i]).collect()
}

/// Abrupt start at stream sample 0, stop at 20 000, 5 000 samples of digital silence, abrupt
/// restart, stop at 45 000, silence to the end.
fn abrupt_material() -> Vec<f32> {
    let mut x = tones(20_000, 1);
    x.extend(std::iter::repeat_n(0.0, 5000));
    x.extend(tones(20_000, 2).iter().map(|v| -v));
    x.extend(std::iter::repeat_n(0.0, 15_000));
    x
}

fn with_clicks(clean: &[f32], clicks: &[(usize, usize, f32)]) -> Vec<f32> {
    let mut out = clean.to_vec();
    for (start, len, amp) in clicks {
        for k in 0..*len {
            out[start + k] += if k % 2 == 0 { *amp } else { -0.7 * amp };
        }
    }
    out
}

#[test]
fn abrupt_starts_and_stops_at_digital_silence_pass_bit_exact() {
    bounded_case("silence_edges_clean", || {
        let x = abrupt_material();
        assert_ne!(x[0], 0.0, "the material really starts abruptly");
        let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
        let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&x), &[1000]);
        println!("silence edges {} repaired {} skipped {}", cr.silence_edges(), cr.repaired_gaps(), cr.skipped_gaps());
        assert_eq!((cr.repaired_gaps(), cr.skipped_gaps()), (0, 0));
        assert!(cr.silence_edges() >= 1, "the edges were detected and deliberately left alone");
        assert_eq!(bits(&out[0][L..]), bits(&x[..x.len() - L]));
    });
}

#[test]
fn clicks_and_dropouts_inside_abrupt_material_are_still_repaired() {
    bounded_case("silence_edges_clicks", || {
        let clean = abrupt_material();
        // Clicks well inside both signal stretches, plus a 10-sample dropout (digital zeros
        // shorter than the AR order) at 33 000, which is a defect and must be repaired.
        let clicks: Vec<(usize, usize, f32)> = [2500, 7000, 11_500, 16_000, 28_000, 37_500, 42_000]
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, 1 + 2 * i, 0.4 + 0.05 * i as f32))
            .collect();
        let mut corrupted = with_clicks(&clean, &clicks);
        corrupted[33_000..33_010].fill(0.0);
        let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
        let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&corrupted), &[777]);
        assert_eq!(cr.repaired_gaps(), clicks.len() as u64 + 1);
        let valid = clean.len() - L;
        let err = |y: &dyn Fn(usize) -> f32| (0..valid).map(|n| (f64::from(y(n)) - f64::from(clean[n])).powi(2)).sum::<f64>();
        let before = err(&|n| corrupted[n]);
        let after = err(&|n| out[0][n + L]);
        let improvement = 10.0 * (before / after).log10();
        println!("SNR improvement {improvement:.2} dB");
        assert!(improvement > 25.0, "{improvement}");
        // The silence and the abrupt edges themselves are untouched.
        for n in (0..200).chain(19_900..25_200).chain(44_900..valid) {
            if !(33_000..33_100).contains(&n) {
                assert_eq!(out[0][n + L].to_bits(), corrupted[n].to_bits(), "edge sample {n}");
            }
        }
    });
}
