//! Lane SMB-SCORE-EFFECTS-6 item 2: the graphic equalizer (spec rows GraphicEQ10Bands/20/30).
//! Oracles: bit-exact identity at all-zero commands, the analytic response `magnitude_db` against
//! the commands at the band centres (bound 1.0 dB at +-12 dB commands, the DAFx-17 figure; a set
//! over the bound stops the lane, it is never loosened), real sine gains against the analytic
//! response, pole radii below 1, block-partition and checkpoint bit identity, and the design hop.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn layout(channels: usize) -> ChannelLayout {
    if channels == 1 { mono() } else { ChannelLayout::stereo() }
}

fn eq(rate: u32, channels: usize, bands: usize, amps: &[f64], gain_db: f64) -> GraphicEq {
    let mut p = GraphicEqParams::flat(bands);
    p.amps_db[..amps.len()].copy_from_slice(amps);
    p.gain_db = gain_db;
    GraphicEq::new(rate, channels, p).unwrap_or_else(|e| panic!("{bands} bands at {rate}: {e:?}"))
}

/// Worst `|response - command|` over the active band centres.
fn worst_centre_error(e: &GraphicEq, bands: usize, amps: &[f64]) -> f64 {
    (0..e.active_bands()).map(|k| (e.magnitude_db(graphic_eq_centre_hz(bands, k).unwrap()) - amps[k]).abs()).fold(0.0, f64::max)
}

/// Seeded +-12 dB binary settings (xorshift64*).
fn seeded(bands: usize, count: usize, seed: u64) -> Vec<Vec<f64>> {
    let mut s = seed | 1;
    (0..count)
        .map(|_| {
            (0..bands)
                .map(|_| {
                    s ^= s >> 12;
                    s ^= s << 25;
                    s ^= s >> 27;
                    if s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 63 == 1 { 12.0 } else { -12.0 }
                })
                .collect()
        })
        .collect()
}

#[test]
fn all_zero_commands_are_bit_exact_identity() {
    bounded_case("geq_identity", || {
        let signal = [noise(1, 3000), noise(2, 3000)];
        for bands in GRAPHIC_EQ_BANDS {
            for rate in [44_100, 48_000] {
                let mut e = eq(rate, 2, bands, &[], 0.0);
                let out = run_blocks(&mut e, &layout(2), rate, &signal, &[777]);
                for ch in 0..2 {
                    assert_eq!(bits(&out[ch]), bits(&signal[ch]), "{bands} bands at {rate} ch {ch}");
                }
                assert!(e.band_gains_db().iter().all(|g| *g == 0.0), "{bands}: {:?}", e.band_gains_db());
            }
        }
    });
}

#[test]
fn band_centre_error_is_at_most_one_db_at_12_db_commands() {
    bounded_case("geq_accuracy", || {
        let mut report = Vec::new();
        for rate in [44_100, 48_000] {
            // 10 bands: every one of the 1024 settings of +-12 dB (the paper's own search).
            let mut worst10 = 0.0_f64;
            for code in 0..1024u32 {
                let amps: Vec<f64> = (0..10).map(|k| if code >> k & 1 == 1 { 12.0 } else { -12.0 }).collect();
                worst10 = worst10.max(worst_centre_error(&eq(rate, 1, 10, &amps, 0.0), 10, &amps));
            }
            report.push(format!("10@{rate}={worst10:.4}"));
            for bands in [20, 30] {
                let mut sets = vec![vec![12.0; bands], vec![-12.0; bands], (0..bands).map(|k| if k % 2 == 0 { 12.0 } else { -12.0 }).collect()];
                sets.extend(seeded(bands, 200, 0x5EED + bands as u64));
                let worst = sets.iter().map(|amps| worst_centre_error(&eq(rate, 1, bands, amps, 0.0), bands, amps)).fold(0.0, f64::max);
                report.push(format!("{bands}@{rate}={worst:.4}"));
            }
        }
        println!("MEASURED: graphic_eq worst band-centre error dB {}", report.join(" "));
        for r in &report {
            let worst: f64 = r.split('=').nth(1).unwrap().parse().unwrap();
            assert!(worst <= 1.0, "stop rule: worst band-centre error over 1.0 dB ({}); send the numbers, never loosen", report.join(" "));
        }
    });
}

#[test]
fn real_sine_gain_agrees_with_the_analytic_response() {
    bounded_case("geq_sine", || {
        let rate = 48_000;
        let amps: Vec<f64> = (0..10).map(|k| if k % 2 == 0 { 6.0 } else { -4.0 }).collect();
        let reference = eq(rate, 1, 10, &amps, 1.5);
        // Bands 3, 5, 7: 250, 1000, 4000 Hz, whole periods in one second at 48 kHz.
        for band in [3usize, 5, 7] {
            let f = graphic_eq_centre_hz(10, band).unwrap();
            let x = sine(f, 0.25, 0.0, rate, 2 * rate as usize);
            let mut e = eq(rate, 1, 10, &amps, 1.5);
            let y = run_blocks(&mut e, &mono(), rate, std::slice::from_ref(&x), &[4096]);
            let tail = rate as usize..;
            let measured = db(rms(&y[0][tail.clone()]) / rms(&x[tail]));
            let analytic = reference.magnitude_db(f);
            assert!((measured - analytic).abs() <= 0.01, "band {band} {f} Hz: sine {measured} dB vs analytic {analytic} dB");
        }
    });
}

#[test]
fn every_section_is_stable_at_70_db_at_every_test_rate() {
    bounded_case("geq_stability", || {
        let mut longest = (0.0_f64, 0usize, 0u32);
        for bands in GRAPHIC_EQ_BANDS {
            for rate in [8_000, 32_000, 44_100, 48_000, 96_000, 768_000] {
                let mid = bands / 2;
                let settings: Vec<(Vec<f64>, f64)> = vec![
                    ((0..bands).map(|k| if k == mid { 70.0 } else { 0.0 }).collect(), 0.0),
                    ((0..bands).map(|k| if k == mid { -70.0 } else { 0.0 }).collect(), 0.0),
                    ((0..bands).map(|k| if k % 2 == 0 { 70.0 } else { -70.0 }).collect(), 0.0),
                    (vec![70.0; bands], 64.0),
                ];
                for (amps, gain) in settings {
                    let mut e = eq(rate, 1, bands, &amps, gain);
                    let r = e.max_pole_radius();
                    assert!(r < 1.0, "{bands} bands at {rate}: pole radius {r}");
                    if r > longest.0 {
                        longest = (r, bands, rate);
                    }
                    // A bounded impulse run stays finite and its envelope does not grow.
                    let mut x = vec![0.0f32; 8192];
                    x[0] = 1e-4;
                    let y = run_blocks(&mut e, &mono(), rate, &[x], &[8192]);
                    assert!(y[0].iter().all(|v| v.is_finite()), "{bands} bands at {rate}");
                    let peak = y[0].iter().fold(0.0f32, |a, v| a.max(v.abs()));
                    let late = y[0][6144..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
                    assert!(late <= peak, "{bands} bands at {rate}");
                }
            }
        }
        // The pole radius above is the proof of stability; the decay below 1e-6 of the peak takes
        // ln(1e-6)/ln(r) samples, millions at these Q values (see the ignored test below).
        let samples = (1e-6f64).ln() / longest.0.ln();
        println!("MEASURED: graphic_eq slowest pole radius {} ({} bands at {} Hz); decay below 1e-6 of peak takes about {samples:.0} samples", longest.0, longest.1, longest.2);
    });
}

/// The impulse decay below 1e-6 of its peak at +-70 dB is not run: at 768 kHz the slowest section
/// needs about 5e8 samples. Stability is proven by the pole radii in the test above.
#[test]
#[ignore = "NOT_PROVEN by design: the decay below 1e-6 at +-70 dB needs about 5e8 samples at 768 kHz; stability is proven by every pole radius < 1 (every_section_is_stable_at_70_db_at_every_test_rate)"]
fn impulse_decay_below_1e6_of_peak_at_70_db_is_not_run() {
    println!("NOT_PROVEN: impulse decay below 1e-6 of peak at +-70 dB not run (about 5e8 samples at 768 kHz); stability rests on the pole radii");
}

#[test]
fn any_block_partition_and_a_checkpoint_continue_bit_for_bit() {
    bounded_case("geq_partition", || {
        let rate = 48_000;
        let signal = [noise(3, 9000), noise(4, 9000)];
        let amps: Vec<f64> = (0..20).map(|k| (k as f64 - 9.5) * 1.3).collect();
        let reference = run_blocks(&mut eq(rate, 2, 20, &amps, -2.0), &layout(2), rate, &signal, &[9000]);
        for blocks in [&[1usize][..], &[7, 255, 256, 1000], &[4096]] {
            let got = run_blocks(&mut eq(rate, 2, 20, &amps, -2.0), &layout(2), rate, &signal, blocks);
            assert_eq!(bits(&got[0]), bits(&reference[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&reference[1]), "{blocks:?}");
        }
        // Checkpoint mid-hop with a pending design: the restored copy continues bit for bit.
        let mut a = eq(rate, 2, 20, &amps, -2.0);
        let head: Vec<Vec<f32>> = signal.iter().map(|c| c[..3000].to_vec()).collect();
        let tail: Vec<Vec<f32>> = signal.iter().map(|c| c[3000..].to_vec()).collect();
        run_blocks(&mut a, &layout(2), rate, &head, &[3000]);
        a.set_automated(4, 9.0).unwrap();
        let state = a.save_state().expect("checkpointable");
        let mut b = eq(rate, 2, 20, &amps, -2.0);
        b.set_automated(4, 9.0).unwrap();
        b.restore_state(&state).unwrap();
        let ya = run_blocks(&mut a, &layout(2), rate, &tail, &[512]);
        let yb = run_blocks(&mut b, &layout(2), rate, &tail, &[333]);
        assert_eq!(bits(&ya[0]), bits(&yb[0]));
        assert_eq!(bits(&ya[1]), bits(&yb[1]));
        assert_eq!(b.restore_state(&state[..state.len() - 1]), Err(ScoreError::LengthMismatch));
    });
}

#[test]
fn automation_designs_at_the_hop_and_a_ramp_designs_at_most_once_per_hop() {
    bounded_case("geq_design_hop", || {
        let rate = 48_000;
        let x = noise(9, 2048);
        // A band command set at frame 300 takes effect at frame 512 (the next multiple of 256).
        let mut plain = eq(rate, 1, 10, &[], 0.0);
        let base = run_blocks(&mut plain, &mono(), rate, std::slice::from_ref(&x), &[2048]);
        let mut e = eq(rate, 1, 10, &[], 0.0);
        let first = run_blocks(&mut e, &mono(), rate, &[x[..300].to_vec()], &[300]);
        e.set_automated(4, 12.0).unwrap();
        let rest = run_blocks(&mut e, &mono(), rate, &[x[300..].to_vec()], &[100]);
        let mut got = first[0].clone();
        got.extend_from_slice(&rest[0]);
        assert_eq!(bits(&got[..512]), bits(&base[0][..512]), "unchanged before the hop");
        assert_ne!(bits(&got[512..]), bits(&base[0][512..]), "changed from the hop");
        // The master gain applies at once.
        let mut g = eq(rate, 1, 10, &[], 0.0);
        let a = run_blocks(&mut g, &mono(), rate, &[x[..300].to_vec()], &[300]);
        g.set_automated(0, -6.0).unwrap();
        let b = run_blocks(&mut g, &mono(), rate, &[x[300..301].to_vec()], &[1]);
        assert_eq!(a[0], base[0][..300]);
        assert_eq!(b[0][0], (f64::from(x[300]) * db_to_linear(-6.0)) as f32);

        // A ramp on every band over 8000 frames through a session: at most 32 designs.
        let mut slot = EffectSlot::new(Box::new(eq(rate, 1, 30, &[], 0.0)));
        for i in 1..=30 {
            slot = slot.with_lane(i, AutomationLane::new(&[(0, -12.0), (8000, 12.0)], Interpolation::Linear, GRAPHIC_EQ_AMP_DB).unwrap());
        }
        let mut t = TrackSpec::new(vec![noise(10, 8000)], 0);
        t.effects = vec![slot];
        let mut session = Session::new(rate, 1024, vec![t]).unwrap();
        let (mut l, mut r) = (vec![0.0f32; 8000], vec![0.0f32; 8000]);
        for at in (0..8000).step_by(1000) {
            session.render(at as u64, &mut l[at..at + 1000], &mut r[at..at + 1000], &CancellationToken::default()).unwrap();
        }
        assert!(l.iter().all(|v| v.is_finite()));
        // Designs: same ramp driven directly, counted on the processor.
        let mut direct = eq(rate, 1, 30, &[], 0.0);
        let before = direct.designs();
        let y = noise(10, 8000);
        let (eng, token) = (engine(), CancellationToken::default());
        for n in 0..8000usize {
            let v = -12.0 + 24.0 * n as f64 / 8000.0;
            for i in 1..=30 {
                direct.set_automated(i, v).unwrap();
            }
            let sp = BlockSpec { sample_rate_hz: rate, layout: mono(), frames: 1, start_sample: n as u64 };
            let mut out = OutputBlock::with_capacity(mono(), 1).unwrap();
            eng.process(&ctx(), &InputBlock::new(&sp, &[&y[n..n + 1]]), &mut direct, &mut out, &token).unwrap();
        }
        let designs = direct.designs() - before;
        assert!(designs <= 32, "{designs} designs over 8000 frames");
        println!("MEASURED: graphic_eq designs over an 8000-frame 30-band ramp = {designs}");
    });
}

#[test]
fn design_time_is_measured() {
    bounded_case("geq_design_time", || {
        for bands in GRAPHIC_EQ_BANDS {
            let mut e = eq(48_000, 1, bands, &[], 0.0);
            let x = vec![0.0f32; GRAPHIC_EQ_DESIGN_HOP];
            let rounds = 50;
            let start = std::time::Instant::now();
            for i in 0..rounds {
                e.set_automated(1, if i % 2 == 0 { 6.0 } else { -6.0 }).unwrap();
                run_blocks(&mut e, &mono(), 48_000, std::slice::from_ref(&x), &[GRAPHIC_EQ_DESIGN_HOP]);
            }
            let per = start.elapsed().as_secs_f64() / f64::from(rounds) * 1e6;
            let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
            println!("MEASURED: graphic_eq {bands} bands design plus one hop of processing = {per:.1} us ({profile})");
        }
    });
}

#[test]
fn accuracy_and_range_are_stored_range_checked_and_silent() {
    bounded_case("geq_stored_rows", || {
        let rate = 48_000;
        let x = [noise(5, 2000)];
        let mut p = GraphicEqParams::flat(10);
        p.amps_db[2] = 5.0;
        let a = run_blocks(&mut GraphicEq::new(rate, 1, p).unwrap(), &mono(), rate, &x, &[2000]);
        p.accuracy = 10.0;
        p.range = 120.0;
        let mut e = GraphicEq::new(rate, 1, p).unwrap();
        assert_eq!((e.params().accuracy, e.params().range), (10.0, 120.0));
        let b = run_blocks(&mut e, &mono(), rate, &x, &[2000]);
        assert_eq!(bits(&a[0]), bits(&b[0]), "accuracy and range change no sound");
        // Hostile values: every field NaN, infinite and past its range; bad counts and rates.
        let base = GraphicEqParams::flat(30);
        type Set = fn(&mut GraphicEqParams, f64);
        let fields: [(Set, (f64, f64)); 4] = [
            (|p, v| p.accuracy = v, GRAPHIC_EQ_ACCURACY),
            (|p, v| p.gain_db = v, GRAPHIC_EQ_GAIN_DB),
            (|p, v| p.range = v, GRAPHIC_EQ_RANGE),
            (|p, v| p.amps_db[29] = v, GRAPHIC_EQ_AMP_DB),
        ];
        for (i, (set, (lo, hi))) in fields.iter().enumerate() {
            for (v, want) in [(f64::NAN, ScoreError::NotFinite), (f64::INFINITY, ScoreError::NotFinite), (hi + 1e-9, ScoreError::OutOfRange), (lo - 1e-9, ScoreError::OutOfRange)] {
                let mut q = base;
                set(&mut q, v);
                assert_eq!(GraphicEq::new(rate, 1, q).unwrap_err(), want, "field {i} value {v}");
            }
        }
        let mut q = base;
        q.bands = 15;
        assert_eq!(GraphicEq::new(rate, 1, q).unwrap_err(), ScoreError::OutOfRange);
        for bad in [0, 44_101] {
            assert_eq!(GraphicEq::new(bad, 1, base).unwrap_err(), ScoreError::InvalidRate);
        }
        assert_eq!(GraphicEq::new(rate, 0, base).unwrap_err(), ScoreError::InvalidLayout);
        assert_eq!(GraphicEq::new(rate, MAX_CHANNELS + 1, base).unwrap_err(), ScoreError::InvalidLayout);
        // Automation refusals change nothing.
        let mut e = GraphicEq::new(rate, 1, base).unwrap();
        assert_eq!(e.set_automated(31, 0.0), Err(ScoreError::OutOfRange));
        assert_eq!(e.set_automated(3, f64::NAN), Err(ScoreError::NotFinite));
        assert_eq!(e.set_automated(3, 70.5), Err(ScoreError::OutOfRange));
        assert_eq!(*e.params(), base);
    });
}

#[test]
fn bands_at_or_above_095_nyquist_are_dropped_and_stay_stored() {
    bounded_case("geq_nyquist", || {
        // 8 kHz: Nyquist 4 kHz, 0.95 of it 3.8 kHz; 10 bands keep 31.25 .. 2000 Hz (7).
        let amps = [3.0; 10];
        let e = eq(8_000, 1, 10, &amps, 0.0);
        assert_eq!(e.active_bands(), 7);
        assert_eq!(e.params().amps_db[9], 3.0, "a dropped band's command stays stored");
        for (bands, rate, want) in [(10, 44_100, 10), (10, 32_000, 9), (20, 8_000, 15), (30, 8_000, 22), (30, 32_000, 28), (30, 44_100, 30)] {
            assert_eq!(eq(rate, 1, bands, &[], 0.0).active_bands(), want, "{bands} bands at {rate}");
        }
        assert_eq!(graphic_eq_interaction(20), Some(0.37));
        assert_eq!(graphic_eq_centre_hz(10, 0), Some(31.25));
        assert_eq!(graphic_eq_centre_hz(30, 16), Some(1000.0));
        assert_eq!(graphic_eq_centre_hz(10, 10), None);
    });
}

#[test]
fn from_normalised_maps_the_sheet_rows() {
    bounded_case("geq_normalised", || {
        for bands in GRAPHIC_EQ_BANDS {
            let rows = spec_parameter_rows(&format!("GraphicEQ{bands}Bands")).unwrap();
            let slots: Vec<f64> = rows.iter().map(|r| r.default_normalised.unwrap()).collect();
            let p = GraphicEqParams::from_normalised(bands, &slots).unwrap();
            let flat = GraphicEqParams::flat(bands);
            assert!((p.accuracy - flat.accuracy).abs() < 1e-3 && (p.range - flat.range).abs() < 1e-3, "{p:?}");
            assert_eq!((p.gain_db, p.amps_db), (flat.gain_db, flat.amps_db));
            assert_eq!(GraphicEqParams::from_normalised(bands, &slots[1..]), Err(ScoreError::LengthMismatch));
            let mut bad = slots.clone();
            bad[4] = 1.5;
            assert_eq!(GraphicEqParams::from_normalised(bands, &bad), Err(ScoreError::OutOfRange));
            bad[4] = f64::NAN;
            assert_eq!(GraphicEqParams::from_normalised(bands, &bad), Err(ScoreError::NotFinite));
        }
    });
}
