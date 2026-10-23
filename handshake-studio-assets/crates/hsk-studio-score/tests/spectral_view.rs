//! SMB-AU-WAVEFORM-EDITOR slice 2: spectral frequency display (window choice, linear / log rows in
//! dB, cached tiles). Oracles: hand-computed row intervals and dB values, the calibration "a
//! bin-centred sine of amplitude A reads A" for every window, and tiles equal to the whole grid.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn view(scale: FrequencyScale, rows: usize) -> SpectralViewParams {
    SpectralViewParams { fft_size: 2048, hop: 512, window: SpectralWindow::Hann, scale, rows, floor_db: -120.0 }
}

#[test]
fn rows_map_frequencies_and_read_calibrated_db() {
    bounded_case("spectral_view_rows", || {
        let log = FrequencyScale::Log { min_hz: 20.0 };
        assert_eq!(frequency_at_row(FrequencyScale::Linear, 4.0, 8, 24_000.0), 12_000.0);
        assert!((frequency_at_row(log, 4.0, 8, 24_000.0) - 20.0 * 1200f64.sqrt()).abs() < 1e-9);
        assert!((frequency_at_row(log, 8.0, 8, 24_000.0) - 24_000.0).abs() < 1e-9);
        for hz in [20.0, 440.0, 9_999.5] {
            let r = row_at_frequency(log, hz, 512, 24_000.0);
            assert!((frequency_at_row(log, r, 512, 24_000.0) - hz).abs() < 1e-9 * hz);
        }
        // 5 bins at 8 kHz (1000 Hz apart): rows of 1000 Hz take the max of the 2 bins on their edges.
        let mags = [0.1_f32, 0.5, 0.25, 0.0, 1.0];
        let p = SpectralViewParams { rows: 4, ..view(FrequencyScale::Linear, 4) };
        let mut out = [0.0_f32; 4];
        frame_to_rows(&mags, &p, 8_000, &mut out);
        let db = |a: f64| (20.0 * a.log10()) as f32;
        assert_eq!(out, [db(0.5), db(0.5), db(0.25), db(1.0)]);
        // A log row narrower than the bin spacing interpolates at its centre: row 0 of 8 from 100 Hz.
        let p = SpectralViewParams { rows: 8, ..view(FrequencyScale::Log { min_hz: 100.0 }, 8) };
        let mut out = [0.0_f32; 8];
        frame_to_rows(&mags, &p, 8_000, &mut out);
        let c = 100.0 * 40f64.powf(1.0 / 16.0) / 1000.0;
        assert_eq!(out[0], db(f64::from(0.1_f32) * (1.0 - c) + 0.5 * c));
        let mut out = [0.0_f32; 4];
        frame_to_rows(&[0.0; 5], &SpectralViewParams { rows: 4, ..view(FrequencyScale::Linear, 4) }, 8_000, &mut out);
        assert!(out.iter().all(|v| (f64::from(*v) + 120.0).abs() < 1e-4), "floor {out:?}");
        // Calibration for every window: a sine on bin 100 of 2048 at amplitude 0.5 reads -6.0206 dB
        // on the linear row holding that bin (1024 rows = one bin spacing each).
        let f = 100.0 * f64::from(RATE) / 2048.0;
        let x = sine(f, 0.5, 0.3, RATE, 48_000);
        for window in [SpectralWindow::Hann, SpectralWindow::Hamming, SpectralWindow::Blackman, SpectralWindow::BlackmanHarris, SpectralWindow::Rectangular] {
            let p = SpectralViewParams { window, ..view(FrequencyScale::Linear, 1024) };
            let mut cache = SpectralTileCache::new(RATE, p, 48_000, DEFAULT_SPECTRAL_CACHE_BYTES).unwrap();
            let tile = cache.tile(0, &x, &CancellationToken::default()).unwrap();
            let v = f64::from(tile[10 * 1024 + 100]);
            assert!((v - 20.0 * 0.5f64.log10()).abs() < 1e-4, "{window:?}: {v}");
        }
    });
}

#[test]
fn cached_tiles_equal_the_whole_grid_and_stay_in_budget() {
    bounded_case("spectral_view_cache", || {
        let x = noise(9, 50_000);
        let p = SpectralViewParams { fft_size: 1024, hop: 256, window: SpectralWindow::Blackman, scale: FrequencyScale::Log { min_hz: 30.0 }, rows: 64, floor_db: -100.0 };
        let tile_bytes = SPECTRAL_TILE_FRAMES * 64 * 4;
        let mut cache = SpectralTileCache::new(RATE, p, 50_000, 2 * tile_bytes).unwrap();
        assert_eq!((cache.frames(), cache.tile_count()), (196, 4)); // ceil(50000/256), ceil(196/64)
        // Oracle: the whole magnitude grid, mapped frame by frame.
        let mut s = Spectrogram::with_window(RATE, SpectrogramParams { fft_size: 1024, hop: 256 }, SpectralWindow::Blackman).unwrap();
        let mut grid = vec![0.0_f32; 196 * s.bins()];
        s.tile(&x, 0, 50_000, 0, 196, &mut grid).unwrap();
        let t = CancellationToken::default();
        for index in 0..4u64 {
            let tile = cache.tile(index, &x, &t).unwrap();
            for (j, row) in tile.chunks(64).enumerate() {
                let k = index as usize * 64 + j;
                let mut expect = [0.0_f32; 64];
                frame_to_rows(&grid[k * s.bins()..(k + 1) * s.bins()], &p, RATE, &mut expect);
                assert_eq!(row, expect, "frame {k}");
            }
        }
        let st = cache.stats();
        assert_eq!((st.misses, st.hits, st.evictions), (4, 0, 2));
        assert!(st.bytes <= 2 * tile_bytes && st.tiles == 2, "{st:?}");
        cache.tile(3, &x, &t).unwrap();
        assert_eq!(cache.stats().hits, 1);
        cache.invalidate(50_000);
        assert_eq!(cache.stats().tiles, 0);
        // Hostile parameters and calls.
        let bad = |q: SpectralViewParams, budget: usize| SpectralTileCache::new(RATE, q, 50_000, budget).err();
        assert_eq!(bad(SpectralViewParams { rows: 7, ..p }, 1 << 20), Some(ScoreError::OutOfRange));
        assert_eq!(bad(SpectralViewParams { rows: 4097, ..p }, 1 << 30), Some(ScoreError::OutOfRange));
        assert_eq!(bad(SpectralViewParams { floor_db: f64::NAN, ..p }, 1 << 20), Some(ScoreError::NotFinite));
        assert_eq!(bad(SpectralViewParams { scale: FrequencyScale::Log { min_hz: 0.0 }, ..p }, 1 << 20), Some(ScoreError::OutOfRange));
        assert_eq!(bad(SpectralViewParams { scale: FrequencyScale::Log { min_hz: 24_000.0 }, ..p }, 1 << 20), Some(ScoreError::OutOfRange));
        assert_eq!(bad(SpectralViewParams { fft_size: 1000, ..p }, 1 << 20), Some(ScoreError::OutOfRange));
        assert_eq!(bad(p, tile_bytes - 1), Some(ScoreError::CapacityExceeded));
        assert_eq!(cache.tile(4, &x, &t).err(), Some(ScoreError::OutOfRange));
        assert_eq!(cache.tile(0, &x[..49_999], &t).err(), Some(ScoreError::LengthMismatch));
        let c = CancellationToken::default();
        c.cancel();
        assert_eq!(cache.tile(0, &x, &c).err(), Some(ScoreError::Canceled));
    });
}
