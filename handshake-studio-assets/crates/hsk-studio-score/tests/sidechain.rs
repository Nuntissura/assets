//! SMB-SCORE-EFFECTS-2 slice 1: lookahead + external sidechain dynamics. Expected values come from
//! the Giannoulis-Massberg-Reiss static curve and the one-pole ballistics in dB
//! (`g_k = G (1 - a^k)`, `a = exp(-1 / (tau * rate))`); derivations are in the comments.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn layout(labels: &[u16]) -> ChannelLayout {
    ChannelLayout::new(labels).unwrap()
}

/// Streams planar `signal` (main channels then key channels) through `proc` with distinct input
/// and output layouts; returns the planar output.
fn run_io(
    proc: &mut dyn Processor,
    input_layout: &ChannelLayout,
    output_layout: &ChannelLayout,
    rate: u32,
    signal: &[Vec<f32>],
    blocks: &[usize],
) -> Vec<Vec<f32>> {
    let frames = signal[0].len();
    let eng = engine();
    let token = CancellationToken::default();
    let mut result = vec![Vec::with_capacity(frames); output_layout.channels()];
    let (mut at, mut i) = (0, 0);
    while at < frames {
        let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
        i += 1;
        let sp = BlockSpec { sample_rate_hz: rate, layout: input_layout.clone(), frames: len, start_sample: at as u64 };
        let slices: Vec<&[f32]> = signal.iter().map(|c| &c[at..at + len]).collect();
        let mut out = OutputBlock::with_capacity(output_layout.clone(), len).unwrap();
        eng.process(&ctx(), &InputBlock::new(&sp, &slices), proc, &mut out, &token).unwrap();
        for (ch, dst) in result.iter_mut().enumerate() {
            dst.extend_from_slice(out.published_channel(ch).unwrap());
        }
        at += len;
    }
    result
}

fn hard_compressor(attack_ms: f64, release_ms: f64) -> DynamicsParams {
    DynamicsParams { knee_db: 0.0, attack_ms, release_ms, ..DynamicsParams::compressor(-20.0, 4.0) }
}

#[test]
fn external_key_drives_the_static_curve_and_leaves_main_untouched_when_silent() {
    bounded_case("sidechain_key_static", || {
        let half = RATE as usize / 2;
        let main = sine(1000.0, 0.1, 0.0, RATE, 2 * half);
        let mut key = vec![0.0_f32; half];
        key.extend(std::iter::repeat_n(0.5_f32, half));
        // Attack 0 => coefficient 0 => the gain jumps to the static target every sample.
        let params = SidechainParams {
            dynamics: hard_compressor(0.0, 1.0),
            key: KeySource::External { channels: 1 },
            lookahead_ms: 0.0,
        };
        let mut sc = SidechainDynamics::new(RATE, 1, params).unwrap();
        let (input, output) = (layout(&[7, 9]), layout(&[7]));
        let out = run_io(&mut sc, &input, &output, RATE, &[main.clone(), key], &[1000]);
        // Silent key: reduction 0 dB, gain exactly 1.0, output bit-identical to main.
        assert_eq!(bits(&out[0][..half]), bits(&main[..half]));
        // Loud key DC 0.5 (-6.0206 dB): reduction (x - T)(1/R - 1) = -0.75 (x + 20) on the MAIN.
        let g = 10f64.powf(-0.75 * (db(0.5) + 20.0) / 20.0);
        for n in half..2 * half {
            let want = f64::from(main[n]) * g;
            assert!((f64::from(out[0][n]) - want).abs() < 1e-7, "frame {n}");
        }

        // Unlinked stereo key: key L loud, key R silent -> only main L is reduced.
        let params = SidechainParams {
            dynamics: DynamicsParams { link: false, ..hard_compressor(0.0, 1.0) },
            key: KeySource::External { channels: 2 },
            lookahead_ms: 0.0,
        };
        let mut sc = SidechainDynamics::new(RATE, 2, params).unwrap();
        let signal = vec![main.clone(), main.clone(), vec![0.5; 2 * half], vec![0.0; 2 * half]];
        let out = run_io(&mut sc, &layout(&[1, 2, 3, 4]), &layout(&[1, 2]), RATE, &signal, &[4096]);
        assert_eq!(bits(&out[1]), bits(&main));
        assert!((rms(&out[0]) / rms(&main) - g).abs() < 1e-6);

        // Keyed gate (range 80 dB, attack 0, hold 0, release 1 ms): key silent -> closes to -80 dB;
        // key loud -> fully open (bit-exact main).
        let gate = DynamicsParams { attack_ms: 0.0, hold_ms: 0.0, release_ms: 1.0, ..DynamicsParams::gate(-40.0) };
        let params = SidechainParams { dynamics: gate, key: KeySource::External { channels: 1 }, lookahead_ms: 0.0 };
        let mut sc = SidechainDynamics::new(RATE, 1, params).unwrap();
        let mut key = vec![0.0_f32; half];
        key.extend(std::iter::repeat_n(0.5_f32, half));
        let out = run_io(&mut sc, &input, &output, RATE, &[main.clone(), key], &[333]);
        for n in half / 2..half {
            assert!((f64::from(out[0][n]) - f64::from(main[n]) * 1e-4).abs() < 1e-9, "closed {n}");
        }
        assert_eq!(bits(&out[0][half..]), bits(&main[half..]));
    });
}

#[test]
fn attack_release_and_lookahead_follow_the_one_pole_ballistics_exactly() {
    bounded_case("sidechain_ballistics", || {
        // Internal key, hard knee T = -20, R = 4, attack 10 ms, release 100 ms, lookahead 5 ms.
        let params = SidechainParams {
            dynamics: hard_compressor(10.0, 100.0),
            key: KeySource::Internal,
            lookahead_ms: 5.0,
        };
        let mut sc = SidechainDynamics::new(RATE, 1, params).unwrap();
        let l = 240; // round(5 ms * 48 kHz)
        assert_eq!(sc.latency_samples(), l as u32);
        let (s, loud, quiet) = (4_800, 9_600, 24_000);
        // Quiet level 0.05 (-26 dB) is below T, so its hard-knee reduction is exactly 0.
        let mut signal = vec![0.05_f32; s];
        signal.extend(std::iter::repeat_n(1.0_f32, loud));
        signal.extend(std::iter::repeat_n(0.05_f32, quiet));
        let out = run_io(&mut sc, &mono(), &mono(), RATE, &[signal.clone()], &[512]);

        let target = -0.75 * 20.0; // static reduction at 0 dB
        let a = (-1.0 / (0.010 * f64::from(RATE))).exp();
        let b = (-1.0 / (0.100 * f64::from(RATE))).exp();
        let attack_db = |k: i32| target * (1.0 - a.powi(k));
        let gain_db = |m: usize| db(f64::from(out[0][m]) / f64::from(signal[m - l]));
        // Before the key moves, output = input delayed by L, gain exactly 1.
        assert_eq!(out[0][s - 1], 0.05);
        assert_eq!(out[0][..l], vec![0.0_f32; l][..]);
        // Lookahead pre-ducks: output frame s+L-1 carries input s-1 (0.05) after L loud key frames.
        assert!((gain_db(s + l - 1) - attack_db(l as i32)).abs() < 1e-5);
        // The loud onset reaches the output after L+1 attack steps; 1 tau later 63.2 percent.
        assert!((gain_db(s + l) - attack_db(l as i32 + 1)).abs() < 1e-5);
        assert!((gain_db(s + 480) - attack_db(481)).abs() < 1e-5);
        assert!((attack_db(480) - target * (1.0 - (-1.0f64).exp())).abs() < 1e-9);
        // Release from g_r = G(1 - a^loud): j quiet key frames give g_r * b^j. Output frame
        // r + L + j' has seen j = L + j' + 1 quiet key frames.
        let r = s + loud;
        let g_r = attack_db(loud as i32);
        for j in [1usize, 480, 4_800, 20_000] {
            let m = r + j - 1;
            assert!((gain_db(m) - g_r * b.powi(j as i32)).abs() < 1e-5, "release step {j}");
        }

        // Sample-rate aware: the same 5 ms is 221 samples at 44.1 kHz (220.5 rounds away) and 480 at 96 kHz.
        for (rate, want) in [(44_100, 221u32), (96_000, 480)] {
            assert_eq!(SidechainDynamics::new(rate, 1, params).unwrap().latency_samples(), want);
        }
    });
}

#[test]
fn internal_key_without_lookahead_is_bit_identical_to_dynamics_and_partition_free() {
    bounded_case("sidechain_equivalence", || {
        let stereo = ChannelLayout::stereo();
        let signal = vec![noise(11, 20_000), noise(23, 20_000)];
        let variants = [
            DynamicsParams { detector: Detector::Rms, ..DynamicsParams::compressor(-24.0, 6.0) },
            DynamicsParams { link: false, ..DynamicsParams::compressor(-30.0, 3.0) },
            DynamicsParams::gate(-20.0),
        ];
        for p in variants {
            let reference = run_blocks(&mut Dynamics::new(RATE, 2, p).unwrap(), &stereo, RATE, &signal, &[777]);
            let params = SidechainParams { dynamics: p, key: KeySource::Internal, lookahead_ms: 0.0 };
            let got = run_blocks(&mut SidechainDynamics::new(RATE, 2, params).unwrap(), &stereo, RATE, &signal, &[777]);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{p:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{p:?}");
        }
        // External key + lookahead: every block partition gives the same bits.
        let keyed = vec![noise(11, 20_000), noise(23, 20_000), noise(5, 20_000)];
        let params = SidechainParams {
            dynamics: DynamicsParams { detector: Detector::Rms, ..DynamicsParams::compressor(-30.0, 8.0) },
            key: KeySource::External { channels: 1 },
            lookahead_ms: 3.0,
        };
        let (input, output) = (layout(&[100, 101, 5]), stereo.clone());
        let reference = run_io(&mut SidechainDynamics::new(RATE, 2, params).unwrap(), &input, &output, RATE, &keyed, &[20_000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 257, 1000], &[4099]] {
            let got = run_io(&mut SidechainDynamics::new(RATE, 2, params).unwrap(), &input, &output, RATE, &keyed, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
    });
}

#[test]
fn hostile_parameters_layouts_and_blocks_are_refused() {
    bounded_case("sidechain_hostile", || {
        let ok = SidechainParams {
            dynamics: DynamicsParams::compressor(-20.0, 4.0),
            key: KeySource::External { channels: 1 },
            lookahead_ms: 5.0,
        };
        let err = |rate, main, p| SidechainDynamics::new(rate, main, p).unwrap_err();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(err(RATE, 1, SidechainParams { lookahead_ms: v, ..ok }), ScoreError::NotFinite);
            let d = DynamicsParams { ratio: v, ..ok.dynamics };
            assert_eq!(err(RATE, 1, SidechainParams { dynamics: d, ..ok }), ScoreError::NotFinite);
        }
        for v in [-1e-9, 500.000_001, 1e300] {
            assert_eq!(err(RATE, 1, SidechainParams { lookahead_ms: v, ..ok }), ScoreError::OutOfRange);
        }
        let d = DynamicsParams { attack_ms: 1e12, ..ok.dynamics };
        assert_eq!(err(RATE, 1, SidechainParams { dynamics: d, ..ok }), ScoreError::OutOfRange);
        for channels in [0, 16, usize::MAX] {
            assert_eq!(err(RATE, 1, SidechainParams { key: KeySource::External { channels }, ..ok }), ScoreError::InvalidLayout);
        }
        let unlinked = SidechainParams { dynamics: DynamicsParams { link: false, ..ok.dynamics }, ..ok };
        assert_eq!(err(RATE, 2, unlinked), ScoreError::InvalidLayout);
        assert_eq!(err(RATE, 0, ok), ScoreError::InvalidLayout);
        assert_eq!(err(RATE, usize::MAX, ok), ScoreError::InvalidLayout);
        for rate in [0, 44_101, 47_999, 1_000_000] {
            assert_eq!(err(rate, 1, ok), ScoreError::InvalidRate);
        }

        let mut sc = SidechainDynamics::new(RATE, 1, ok).unwrap();
        let eng = engine();
        let token = CancellationToken::default();
        let (input, output) = (layout(&[7, 9]), layout(&[7]));
        let main = vec![0.25_f32; 64];
        // Wrong layouts: output carrying the key channel, or main label mismatch.
        for (i, o) in [(&input, &input), (&input, &layout(&[9])), (&output, &output)] {
            let sp = BlockSpec { sample_rate_hz: RATE, layout: i.clone(), frames: 64, start_sample: 0 };
            let slices: Vec<&[f32]> = (0..i.channels()).map(|_| &main[..]).collect();
            let mut out = OutputBlock::with_capacity(o.clone(), 64).unwrap();
            let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut sc, &mut out, &token);
            assert_eq!(got.unwrap_err(), ScoreError::ChannelMismatch);
        }
        // NaN or infinity in the KEY channel is refused at admission; nothing is published.
        let mut out = OutputBlock::with_capacity(output.clone(), 64).unwrap();
        for bad in [f32::NAN, f32::INFINITY] {
            let mut key = vec![0.0_f32; 64];
            key[33] = bad;
            let sp = BlockSpec { sample_rate_hz: RATE, layout: input.clone(), frames: 64, start_sample: 0 };
            let slices: Vec<&[f32]> = vec![&main, &key];
            let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut sc, &mut out, &token);
            assert_eq!(got.unwrap_err(), ScoreError::NotFinite);
            assert_eq!(out.published_frames(), 0);
        }
        // Huge blocks: above MAX_BLOCK_FRAMES, or above the output capacity.
        let sp = BlockSpec { sample_rate_hz: RATE, layout: input.clone(), frames: MAX_BLOCK_FRAMES + 1, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&main, &main];
        let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut sc, &mut out, &token);
        assert_eq!(got.unwrap_err(), ScoreError::CapacityExceeded);
        let long = vec![0.0_f32; 65];
        let sp = BlockSpec { sample_rate_hz: RATE, layout: input.clone(), frames: 65, start_sample: 0 };
        let slices: Vec<&[f32]> = vec![&long, &long];
        let got = eng.process(&ctx(), &InputBlock::new(&sp, &slices), &mut sc, &mut out, &token);
        assert_eq!(got.unwrap_err(), ScoreError::CapacityExceeded);
        assert_eq!(out.published_frames(), 0);
    });
}
