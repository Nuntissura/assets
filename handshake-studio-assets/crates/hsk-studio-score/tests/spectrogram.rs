//! SMB-SCORE-SPECTRAL slice 1: spectral display data (magnitude grid, tiles, reassignment).
mod common_dsp;
use common_dsp::*;
use hsk_studio_score::*;

const P: SpectrogramParams = SpectrogramParams { fft_size: 2048, hop: 512 };

fn full_grid(s: &mut Spectrogram, x: &[f32]) -> Vec<f32> {
    let frames = s.frame_count(x.len() as u64) as usize;
    let mut out = vec![0.0_f32; frames * s.bins()];
    s.tile(x, 0, x.len() as u64, 0, frames, &mut out).unwrap();
    out
}

#[test]
fn grid_geometry_and_amplitude_calibration() {
    bounded_case("spectrogram_calibration", || {
        // Bin 100 of 2048 at 48 kHz = 2343.75 Hz; amplitude 0.5 reads 0.5 at that bin.
        let mut s = Spectrogram::new(RATE, P).unwrap();
        let f = 100.0 * f64::from(RATE) / 2048.0;
        let x = sine(f, 0.5, 0.4, RATE, 48_000);
        assert_eq!(s.frame_count(48_000), 94); // ceil(48000 / 512)
        assert_eq!(s.bins(), 1025);
        let grid = full_grid(&mut s, &x);
        for k in 4..90 {
            let row = &grid[k * 1025..(k + 1) * 1025];
            assert!((f64::from(row[100]) - 0.5).abs() < 1e-6, "frame {k}: {}", row[100]);
            // Hann: neighbours read A/2, bins 3+ away are below -100 dB of the peak.
            assert!((f64::from(row[99]) - 0.25).abs() < 1e-6);
            assert!(row[..97].iter().chain(&row[104..]).all(|v| f64::from(*v) < 0.5e-5), "frame {k}");
        }
        // DC calibration: a constant 0.25 reads 0.25 at bin 0 inside the clip.
        let dc = vec![0.25_f32; 20_000];
        let grid = full_grid(&mut s, &dc);
        assert!((f64::from(grid[10 * 1025]) - 0.25).abs() < 1e-7);
    });
}

#[test]
fn chunked_tiles_are_bit_identical_to_the_whole_grid() {
    bounded_case("spectrogram_tiles", || {
        let x = noise(4, 100_000);
        for params in [P, SpectrogramParams { fft_size: 1024, hop: 1000 }, SpectrogramParams { fft_size: 256, hop: 1 }] {
            let x = if params.hop == 1 { &x[..3000] } else { &x[..] };
            let len = x.len() as u64;
            let mut s = Spectrogram::new(RATE, params).unwrap();
            let whole = full_grid(&mut s, x);
            let frames = s.frame_count(len);
            let bins = s.bins();
            let mut first = 0;
            for count in [1u64, 7, 13, 40].iter().cycle() {
                if first >= frames {
                    break;
                }
                let count = (*count).min(frames - first);
                // Only the samples this tile reads, as a file chunk would supply them.
                let (start, end) = s.tile_input_range(len, first, count);
                let chunk = &x[start as usize..end as usize];
                let mut out = vec![0.0_f32; count as usize * bins];
                s.tile(chunk, start, len, first, count as usize, &mut out).unwrap();
                let at = first as usize * bins;
                assert_eq!(bits(&out), bits(&whole[at..at + out.len()]), "{params:?} frames {first}+{count}");
                first += count;
            }
        }
    });
}

#[test]
fn reassignment_pins_tones_and_impulses() {
    bounded_case("spectrogram_reassignment", || {
        let mut s = Spectrogram::new(RATE, P).unwrap();
        let bins = s.bins();
        let run = |s: &mut Spectrogram, x: &[f32]| {
            let frames = s.frame_count(x.len() as u64) as usize;
            let (mut m, mut t, mut f) = (vec![0.0_f32; frames * bins], vec![0.0; frames * bins], vec![0.0; frames * bins]);
            s.tile_reassigned(x, 0, x.len() as u64, 0, frames, ReassignedTile { magnitude: &mut m, time: &mut t, frequency: &mut f }).unwrap();
            (m, t, f)
        };
        // A tone between bins (1000.3 Hz): the peak cell of every interior frame reassigns to it.
        let x = sine(1000.3, 0.5, 0.2, RATE, 48_000);
        let (m, _, f) = run(&mut s, &x);
        let mut worst = 0.0_f64;
        for k in 5..88 {
            let row = &m[k * bins..(k + 1) * bins];
            let peak = (0..bins).max_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap();
            for b in peak - 1..=peak + 1 {
                worst = worst.max((f[k * bins + b] - 1000.3).abs());
            }
        }
        println!("tone 1000.3 Hz: worst reassigned frequency error {worst:.4} Hz (bin width 23.4 Hz)");
        assert!(worst < 0.05, "{worst}");
        // An impulse at sample 10_007: every cell holding energy reassigns to that time.
        let mut x = vec![0.0_f32; 30_000];
        x[10_007] = 1.0;
        let (m, t, _) = run(&mut s, &x);
        let mut worst = 0.0_f64;
        let mut cells = 0;
        for (i, v) in m.iter().enumerate() {
            if f64::from(*v) > 1e-4 && (i % bins) > 2 && (i % bins) < bins - 3 {
                worst = worst.max((t[i] - 10_007.0).abs());
                cells += 1;
            }
        }
        println!("impulse at 10007: worst reassigned time error {worst:.4} samples over {cells} cells");
        assert!(cells > 1000 && worst < 0.05, "{worst} over {cells}");
    });
}

#[test]
fn hostile_parameters_budgets_and_ranges_are_refused() {
    bounded_case("spectrogram_hostile", || {
        for p in [
            SpectrogramParams { fft_size: 1000, hop: 100 },
            SpectrogramParams { fft_size: 128, hop: 64 },
            SpectrogramParams { fft_size: 32_768, hop: 64 },
            SpectrogramParams { fft_size: 2048, hop: 0 },
            SpectrogramParams { fft_size: 2048, hop: 2049 },
        ] {
            assert_eq!(Spectrogram::new(RATE, p).err(), Some(ScoreError::OutOfRange), "{p:?}");
        }
        assert_eq!(Spectrogram::new(44_101, P).err(), Some(ScoreError::InvalidRate));
        let mut s = Spectrogram::new(RATE, P).unwrap();
        let x = noise(1, 10_000);
        let len = x.len() as u64;
        let frames = s.frame_count(len) as usize; // 20
        let mut out = vec![0.0_f32; 1025];
        assert_eq!(s.tile(&x, 0, len, 20, 1, &mut out), Err(ScoreError::OutOfRange));
        assert_eq!(s.tile(&x, 0, len, 0, 0, &mut out[..0]), Err(ScoreError::OutOfRange));
        assert_eq!(s.tile(&x, 0, len, 0, 2, &mut out), Err(ScoreError::LengthMismatch));
        // A chunk that does not cover the tile's range.
        assert_eq!(s.tile(&x[600..], 600, len, 1, 1, &mut out), Err(ScoreError::OutOfRange));
        // Budget: (2^24 / 1025 + 1) frames is refused before any work or allocation by the tile.
        let too_many = MAX_TILE_CELLS / 1025 + 1;
        assert_eq!(s.tile(&x, 0, len, 0, too_many, &mut out), Err(ScoreError::CapacityExceeded));
        assert_eq!(s.tile(&x, 0, len, 0, usize::MAX, &mut out), Err(ScoreError::CapacityExceeded));
        let mut bad = x.clone();
        bad[5] = f32::NAN;
        assert_eq!(s.tile(&bad, 0, len, 0, 1, &mut out), Err(ScoreError::NotFinite));
        // The same NaN outside the tile's range does not matter.
        assert!(s.tile(&bad, 0, len, frames as u64 - 1, 1, &mut out).is_ok());
    });
}
