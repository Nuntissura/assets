//! SMB-SCORE-EFFECTS-5 slice 3: click repair on noisy and dithered floors. Abrupt starts and stops
//! from or into a floor are edges (left alone); clicks on the floor and in the music are repaired.
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

const L: usize = CLICK_HOP + CLICK_CONTEXT;

fn tones(frames: usize, seed: u64) -> Vec<f32> {
    let a = sine(440.0, 0.3, 0.0, RATE, frames);
    let b = sine(1234.0, 0.2, 0.7, RATE, frames);
    let n = noise(seed, frames);
    (0..frames).map(|i| a[i] + b[i] + 0.003 * n[i]).collect()
}

/// 16-bit TPDF dither: the sum of two uniform values of +-0.5 LSB (LSB = 2^-15).
fn dither(frames: usize, seed: u64) -> Vec<f32> {
    let (a, b) = (noise(seed, frames), noise(seed + 1000, frames));
    let lsb = 1.0 / 32_768.0;
    (0..frames).map(|i| (a[i] + b[i]) * 0.5 * lsb).collect()
}

/// Floor, abrupt music, floor, abrupt music, floor (floor = `floor(frames, seed)`).
fn material(floor: &dyn Fn(usize, u64) -> Vec<f32>) -> Vec<f32> {
    let mut x = floor(4000, 1);
    x.extend(tones(16_000, 2));
    x.extend(floor(6000, 3));
    x.extend(tones(16_000, 4).iter().map(|v| -v));
    x.extend(floor(16_000, 5));
    x
}

fn white_floor(frames: usize, seed: u64) -> Vec<f32> {
    noise(seed, frames).iter().map(|v| v * 0.001).collect()
}

#[test]
fn abrupt_edges_on_dithered_and_noisy_floors_pass_bit_exact() {
    bounded_case("floor_edges_clean", || {
        for (name, floor) in [("dither", &dither as &dyn Fn(usize, u64) -> Vec<f32>), ("white -60 dB", &white_floor)] {
            let x = material(floor);
            let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
            let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&x), &[1000]);
            println!("{name}: edges {} repaired {} skipped {}", cr.silence_edges(), cr.repaired_gaps(), cr.skipped_gaps());
            assert_eq!(cr.repaired_gaps(), 0, "{name}");
            assert!(cr.silence_edges() >= 1, "{name}");
            assert_eq!(bits(&out[0][L..]), bits(&x[..x.len() - L]), "{name}");
        }
    });
}

#[test]
fn clicks_on_the_floor_and_in_the_music_are_repaired() {
    bounded_case("floor_clicks", || {
        for (name, floor) in [("dither", &dither as &dyn Fn(usize, u64) -> Vec<f32>), ("white -60 dB", &white_floor)] {
            let clean = material(floor);
            // Floor clicks at 1500, 23_000, 50_000 (>= 1000 samples from any edge); music clicks
            // at 9000, 14_000, 31_000, 37_000.
            let clicks: [(usize, usize, f32); 7] = [
                (1500, 2, 0.05),
                (23_000, 3, -0.08),
                (50_000, 1, 0.1),
                (9000, 4, 0.5),
                (14_000, 2, -0.4),
                (31_000, 6, 0.45),
                (37_000, 1, -0.6),
            ];
            let mut corrupted = clean.clone();
            for (start, len, amp) in clicks {
                for k in 0..len {
                    corrupted[start + k] += if k % 2 == 0 { amp } else { -0.6 * amp };
                }
            }
            let mut cr = ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap();
            let out = run_blocks(&mut cr, &mono(), RATE, std::slice::from_ref(&corrupted), &[777]);
            assert_eq!(cr.repaired_gaps(), clicks.len() as u64, "{name}");
            let valid = clean.len() - L;
            let before: f64 = (0..valid).map(|n| (f64::from(corrupted[n]) - f64::from(clean[n])).powi(2)).sum();
            let after: f64 = (0..valid).map(|n| (f64::from(out[0][n + L]) - f64::from(clean[n])).powi(2)).sum();
            let improvement = 10.0 * (before / after).log10();
            // Floor clicks alone: the repaired floor stays at floor level.
            let floor_err = (1490..1520).chain(23_000..23_030).chain(50_000..50_030).map(|n| (out[0][n + L] - clean[n]).abs()).fold(0.0, f32::max);
            println!("{name}: SNR improvement {improvement:.2} dB, worst floor-click error {floor_err:.2e}");
            assert!(improvement > 20.0, "{name}: {improvement}");
            assert!(floor_err < 0.005, "{name}: {floor_err}");
            // Edges untouched: 3990..4300, 19_900..20_300, 25_900..26_300, 41_900..42_300.
            for n in (3990..4300).chain(19_900..20_300).chain(25_900..26_300).chain(41_900..42_300) {
                assert_eq!(out[0][n + L].to_bits(), corrupted[n].to_bits(), "{name} edge sample {n}");
            }
        }
    });
}
