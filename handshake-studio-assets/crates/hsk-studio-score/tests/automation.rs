//! SMB-SCORE-EFFECTS-4 slice 2: every processor is Automatable. The central check: setting a
//! parameter on a fresh processor gives the same bits as constructing it with that value.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

type Make = Box<dyn Fn(&[f64]) -> Box<dyn Automatable + Send>>;

/// One processor kind: constructor from the full automatable value vector, base values, an
/// alternative value per parameter, and the input/output channel counts.
struct Kind {
    name: &'static str,
    make: Make,
    base: Vec<f64>,
    alt: Vec<f64>,
    inputs: usize,
    outputs: usize,
}

fn layout(channels: usize) -> ChannelLayout {
    if channels == 1 { mono() } else { ChannelLayout::stereo() }
}

fn run(proc: &mut dyn Processor, inputs: usize, outputs: usize, signal: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let frames = signal[0].len();
    let (eng, token) = (engine(), CancellationToken::default());
    let mut result = vec![Vec::new(); outputs];
    let mut at = 0;
    while at < frames {
        let len = 700.min(frames - at);
        let sp = BlockSpec { sample_rate_hz: RATE, layout: layout(inputs), frames: len, start_sample: at as u64 };
        let slices: Vec<&[f32]> = signal.iter().map(|c| &c[at..at + len]).collect();
        let mut out = OutputBlock::with_capacity(layout(outputs), len).unwrap();
        eng.process(&ctx(), &InputBlock::new(&sp, &slices), proc, &mut out, &token).unwrap();
        for (ch, dst) in result.iter_mut().enumerate() {
            dst.extend_from_slice(out.published_channel(ch).unwrap());
        }
        at += len;
    }
    result
}

fn dyn_params(v: &[f64]) -> DynamicsParams {
    DynamicsParams { threshold_db: v[0], ratio: v[1], makeup_db: v[2], ..DynamicsParams::compressor(-20.0, 4.0) }
}

fn kinds() -> Vec<Kind> {
    let profile = {
        let mut profiler = NoiseProfiler::new(RATE, 1, 1024).unwrap();
        let n: Vec<f32> = noise(77, 20_000).iter().map(|v| v * 0.8).collect();
        profiler.push(&[&n]).unwrap();
        profiler.finish().unwrap()
    };
    let eq_sections = |v: &[f64]| {
        vec![
            BiquadSpec { kind: BiquadKind::Peaking, frequency_hz: v[1], gain_db: v[2], q: v[3] },
            BiquadSpec { kind: BiquadKind::LowPass, frequency_hz: v[4], gain_db: v[5], q: v[6] },
        ]
    };
    vec![
        Kind { name: "Gain", make: Box::new(|v| Box::new(Gain::from_db(v[0]).unwrap())), base: vec![0.0], alt: vec![-6.0], inputs: 1, outputs: 1 },
        Kind { name: "Pan", make: Box::new(|v| Box::new(Pan::new(v[0] as f32).unwrap())), base: vec![0.0], alt: vec![-0.4], inputs: 1, outputs: 2 },
        Kind {
            name: "ChannelMix",
            make: Box::new(|v| Box::new(ChannelMix::new(2, 1, &[v[0] as f32, v[1] as f32]).unwrap())),
            base: vec![0.5, 0.5],
            alt: vec![0.25, -1.5],
            inputs: 2,
            outputs: 1,
        },
        Kind {
            name: "HardLimiter",
            make: Box::new(|v| Box::new(HardLimiter::new(RATE, 1, HardLimiterParams { max_amp_db: v[0], input_boost_db: v[1], lookahead_ms: 7.1, release_ms: v[2] }).unwrap())),
            base: vec![-6.0, 6.0, 100.0],
            alt: vec![-12.0, 10.0, 60.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "LookaheadLimiter",
            make: Box::new(|v| Box::new(LookaheadLimiter::new(RATE, 1, LimiterParams { ceiling_db: v[0], lookahead_ms: 5.0, release_ms: v[1], true_peak: false }).unwrap())),
            base: vec![-1.0, 100.0],
            alt: vec![-6.0, 30.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "Equalizer",
            make: Box::new(move |v| Box::new(Equalizer::new(RATE, 1, &eq_sections(v), v[0]).unwrap())),
            base: vec![0.0, 1000.0, 6.0, 1.0, 8000.0, 0.0, 0.7],
            alt: vec![-3.0, 2500.0, -9.0, 4.0, 3000.0, 5.0, 2.0],
            inputs: 1,
            outputs: 1,
        },
        Kind { name: "Dynamics", make: Box::new(|v| Box::new(Dynamics::new(RATE, 1, dyn_params(v)).unwrap())), base: vec![-20.0, 4.0, 0.0], alt: vec![-30.0, 8.0, 3.0], inputs: 1, outputs: 1 },
        Kind {
            name: "SidechainDynamics",
            make: Box::new(|v| Box::new(SidechainDynamics::new(RATE, 1, SidechainParams { dynamics: dyn_params(v), key: KeySource::Internal, lookahead_ms: 2.0 }).unwrap())),
            base: vec![-20.0, 4.0, 0.0],
            alt: vec![-30.0, 8.0, 3.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "MultibandCompressor",
            make: Box::new(|v| {
                let mut p = MultibandParams::three_band();
                for b in 0..3 {
                    p.band_params[b] = dyn_params(&v[3 * b..3 * b + 3]);
                }
                Box::new(MultibandCompressor::new(RATE, 1, p).unwrap())
            }),
            base: vec![-20.0, 4.0, 0.0, -20.0, 4.0, 0.0, -20.0, 4.0, 0.0],
            alt: vec![-30.0, 8.0, 3.0, -10.0, 2.0, -2.0, -40.0, 20.0, 6.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "FeedbackDelay",
            make: Box::new(|v| Box::new(FeedbackDelay::new(RATE, 1, DelayParams { time_ms: 7.0, feedback: v[0], damping_hz: Some(4000.0), mix: v[1], ping_pong: false }).unwrap())),
            base: vec![0.3, 0.5],
            alt: vec![-0.6, 0.9],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "Flanger",
            make: Box::new(|v| Box::new(Flanger::new(RATE, 2, FlangerParams { initial_ms: v[0], final_ms: v[1], rate_hz: v[2], stereo_phase_deg: 90.0, feedback: v[3], mix: v[4] }).unwrap())),
            base: vec![1.0, 5.0, 0.25, 0.5, 0.5],
            alt: vec![2.0, 4.0, 1.5, -0.3, 0.8],
            inputs: 2,
            outputs: 2,
        },
        Kind {
            name: "Chorus",
            make: Box::new(|v| Box::new(Chorus::new(RATE, 2, ChorusParams { voices: 3, delay_ms: v[0], depth_ms: v[1], rate_hz: v[2], stereo_phase_deg: 90.0, mix: v[3] }).unwrap())),
            base: vec![20.0, 3.0, 0.8, 0.5],
            alt: vec![18.0, 2.0, 2.0, 0.7],
            inputs: 2,
            outputs: 2,
        },
        Kind {
            name: "Phaser",
            make: Box::new(|v| Box::new(Phaser::new(RATE, 2, PhaserParams { stages: 4, min_hz: v[0], max_hz: v[1], rate_hz: v[2], stereo_phase_deg: 90.0, feedback: v[3], mix: v[4] }).unwrap())),
            base: vec![300.0, 3000.0, 0.5, 0.3, 0.5],
            alt: vec![200.0, 4000.0, 1.0, -0.2, 0.9],
            inputs: 2,
            outputs: 2,
        },
        Kind {
            name: "ClickRepair",
            make: Box::new(|v| Box::new(ClickRepair::new(RATE, 1, ClickRepairParams { threshold: v[0], ..ClickRepairParams::default() }).unwrap())),
            base: vec![8.0],
            alt: vec![3.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "HumRemover",
            make: Box::new(|v| Box::new(HumRemover::new(RATE, 1, HumParams { fundamental_hz: v[0], harmonics: 8, bandwidth_hz: v[1] }).unwrap())),
            base: vec![50.0, 2.0],
            alt: vec![600.0, 5.0],
            inputs: 1,
            outputs: 1,
        },
        Kind {
            name: "Reverb",
            make: Box::new(|v| Box::new(Reverb::new(RATE, 2, ReverbParams { rt60_s: v[0], damping: v[1], pre_delay_ms: 10.0, width: v[2], wet: v[3], dry: v[4] }).unwrap())),
            base: vec![1.5, 0.3, 1.0, 0.3, 1.0],
            alt: vec![2.5, 0.6, 0.4, 0.5, 0.8],
            inputs: 2,
            outputs: 2,
        },
        Kind {
            name: "NoiseReducer",
            make: Box::new(move |v| {
                let p = NoiseReductionParams { reduction_db: v[0], over_subtraction: v[1], smoothing_up_ms: v[2], smoothing_down_ms: v[3], freq_smoothing_bins: 1 };
                Box::new(NoiseReducer::new(&profile, 1, p).unwrap())
            }),
            base: vec![24.0, 2.0, 40.0, 5.0],
            alt: vec![12.0, 4.0, 100.0, 0.0],
            inputs: 1,
            outputs: 1,
        },
    ]
}

#[test]
fn setting_a_parameter_equals_constructing_with_it_for_every_processor() {
    bounded_case("automation_equivalence", || {
        // Cubed noise: heavy-tailed (peaks near 1, small robust scale) so the limiters act and the
        // click detector reacts to its threshold.
        let signal = [noise(5, 12_000).iter().map(|v| v * v * v).collect::<Vec<f32>>(), noise(6, 12_000)];
        let mut covered = 0;
        for kind in kinds() {
            // The click detector needs sparse clicks: low uniform noise (never past 1.35 robust
            // sigmas) plus 0.5 spikes (about 6.7 sigmas: repaired at threshold 3, kept at 8).
            let clicks: Vec<f32> = noise(5, 12_000).iter().enumerate().map(|(n, v)| v * 0.1 + if n % 997 == 500 { 0.5 } else { 0.0 }).collect();
            let click_input = [clicks];
            let input: &[Vec<f32>] = if kind.name == "ClickRepair" { &click_input } else { &signal[..kind.inputs] };
            let names: Vec<&str> = (kind.make)(&kind.base).automation_parameters().iter().map(|p| p.name).collect();
            assert_eq!(names.len(), kind.base.len(), "{}: {names:?}", kind.name);
            for i in 0..kind.base.len() {
                let mut set = (kind.make)(&kind.base);
                set.set_automated(i, kind.alt[i]).unwrap_or_else(|e| panic!("{} {i}: {e:?}", kind.name));
                let mut values = kind.base.clone();
                values[i] = kind.alt[i];
                let mut built = (kind.make)(&values);
                let a = run(set.as_mut(), kind.inputs, kind.outputs, input);
                let b = run(built.as_mut(), kind.inputs, kind.outputs, input);
                for ch in 0..kind.outputs {
                    assert_eq!(bits(&a[ch]), bits(&b[ch]), "{} parameter {i} ({})", kind.name, names[i]);
                }
                // The change is audible (the alternative value is not a no-op on this input); a
                // low-pass section ignores its gain by definition.
                if (kind.name, i) == ("Equalizer", 5) {
                    covered += 1;
                    continue;
                }
                let base = run((kind.make)(&kind.base).as_mut(), kind.inputs, kind.outputs, input);
                assert_ne!(bits(&a[0]), bits(&base[0]), "{} parameter {i} had no effect", kind.name);
                covered += 1;
            }
        }
        assert_eq!(covered, 59);
    });
}

#[test]
fn parameterless_and_ramp_processors_and_every_refusal() {
    bounded_case("automation_refusals", || {
        // Parameterless processors: empty list, every index refused.
        let unity = Unity;
        let meter = LoudnessMeter::new(RATE, &[1.0]).unwrap();
        let chain = Chain::new(mono(), 64, vec![ChainStage { processor: Box::new(Gain::new(0.5).unwrap()), output: mono() }]).unwrap();
        let mut empties: Vec<Box<dyn Automatable>> = vec![Box::new(unity), Box::new(meter), Box::new(chain)];
        for p in &mut empties {
            assert!(p.automation_parameters().is_empty());
            assert_eq!(p.set_automated(0, 0.0), Err(ScoreError::OutOfRange));
        }
        // RampedGain: a new target ramps across the next block (frame n of N uses
        // current + (target - current) n / N), then holds.
        let mut ramp = RampedGain::new(1.0).unwrap();
        assert_eq!(ramp.automation_parameters()[0].name, "target");
        ramp.set_automated(0, 0.5).unwrap();
        let out = run_blocks(&mut ramp, &mono(), RATE, &[vec![1.0_f32; 200]], &[100]);
        assert_eq!(out[0][0], 1.0);
        assert_eq!(out[0][50], (1.0 + (0.5 - 1.0) * 50.0 / 100.0) as f32);
        assert!(out[0][100..].iter().all(|v| *v == 0.5));
        assert_eq!(ramp.set_automated(0, 17.0), Err(ScoreError::OutOfRange));

        // Every parameterised processor: unknown index, NaN, infinity and a value past its
        // declared maximum are refused and leave the processor unchanged (same output after).
        let signal = [noise(5, 3000), noise(6, 3000)];
        for kind in kinds() {
            let mut p = (kind.make)(&kind.base);
            let params = p.automation_parameters();
            assert_eq!(p.set_automated(params.len(), 0.0), Err(ScoreError::OutOfRange), "{}", kind.name);
            for (i, param) in params.iter().enumerate() {
                assert_eq!(p.set_automated(i, f64::NAN), Err(ScoreError::NotFinite), "{} {i}", kind.name);
                assert_eq!(p.set_automated(i, f64::INFINITY), Err(ScoreError::NotFinite), "{} {i}", kind.name);
                assert_eq!(p.set_automated(i, param.max + 1.0 + param.max.abs()), Err(ScoreError::OutOfRange), "{} {i} above {}", kind.name, param.max);
                assert!(param.min <= param.max, "{} {i}", kind.name);
            }
            let after = run(p.as_mut(), kind.inputs, kind.outputs, &signal[..kind.inputs]);
            let fresh = run((kind.make)(&kind.base).as_mut(), kind.inputs, kind.outputs, &signal[..kind.inputs]);
            assert_eq!(bits(&after[0]), bits(&fresh[0]), "{} changed by refused sets", kind.name);
        }
        // Cross-parameter rules: EQ frequency above Nyquist, chorus delay + depth beyond the line
        // sized at construction, phaser min above max.
        let mut eq = Equalizer::new(RATE, 1, &[BiquadSpec { kind: BiquadKind::Peaking, frequency_hz: 1000.0, gain_db: 0.0, q: 1.0 }], 0.0).unwrap();
        assert_eq!(eq.automation_parameters()[1].max, 24_000.0);
        assert_eq!(eq.set_automated(1, 24_000.5), Err(ScoreError::OutOfRange));
        let mut chorus = Chorus::new(RATE, 1, ChorusParams::default()).unwrap();
        assert_eq!(chorus.set_automated(0, 22.0), Err(ScoreError::OutOfRange)); // 22 + 3 > 23
        assert_eq!(chorus.set_automated(1, 21.0), Err(ScoreError::OutOfRange)); // depth > delay
        let mut phaser = Phaser::new(RATE, 1, PhaserParams::default()).unwrap();
        assert_eq!(phaser.set_automated(0, 5000.0), Err(ScoreError::OutOfRange)); // min > max 3000
        let mut flanger = Flanger::new(RATE, 1, FlangerParams::default()).unwrap();
        assert_eq!(flanger.set_automated(1, 5.5), Err(ScoreError::OutOfRange)); // line sized for 5 ms
    });
}

#[test]
fn ramped_automation_through_a_session_is_partition_independent_for_every_kind() {
    bounded_case("automation_session", || {
        // Each kind as a track stage with a linear ramp on every parameter from base to alt over
        // samples 2000..6000: the render is finite and identical for every block partition.
        let frames = 8000;
        for kind in kinds().into_iter().filter(|k| k.inputs == k.outputs) {
            let clip: Vec<Vec<f32>> = (0..kind.inputs).map(|c| noise(10 + c as u64, frames).iter().map(|v| v * 0.5).collect()).collect();
            let build = || {
                let mut slot = EffectSlot::new((kind.make)(&kind.base));
                let params = slot.processor.automation_parameters();
                for (i, p) in params.iter().enumerate() {
                    let (a, b) = (kind.base[i].clamp(p.min, p.max), kind.alt[i].clamp(p.min, p.max));
                    if a == b {
                        continue;
                    }
                    // Cross-constrained parameters ramp alone (one at a time is always valid here).
                    if matches!(kind.name, "Chorus" | "Phaser") && i > 0 {
                        continue;
                    }
                    slot = slot.with_lane(i, AutomationLane::new(&[(2000, a), (6000, b)], Interpolation::Linear, (p.min, p.max)).unwrap());
                }
                let mut t = TrackSpec::new(clip.clone(), 0);
                t.effects = vec![slot];
                Session::new(RATE, 1024, vec![t]).unwrap()
            };
            let render = |blocks: &[usize]| {
                let mut s = build();
                let token = CancellationToken::default();
                let (mut l, mut r) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
                let (mut at, mut i) = (0, 0);
                while at < frames {
                    let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
                    i += 1;
                    s.render(at as u64, &mut l[at..at + len], &mut r[at..at + len], &token).unwrap_or_else(|e| panic!("{}: {e:?}", kind.name));
                    at += len;
                }
                (l, r)
            };
            let reference = render(&[1024]);
            assert!(reference.0.iter().chain(&reference.1).all(|v| v.is_finite()), "{}", kind.name);
            for blocks in [&[1usize][..], &[7, 333, 1000]] {
                let got = render(blocks);
                assert_eq!(bits(&got.0), bits(&reference.0), "{} {blocks:?}", kind.name);
                assert_eq!(bits(&got.1), bits(&reference.1), "{} {blocks:?}", kind.name);
            }
        }
    });
}
