//! SMB-SCORE-EFFECTS-4 slice 1: latency compensation in the multitrack session. A latent stack is
//! fed ahead by its total latency after a priming pre-roll, so every track lines up sample-exact.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;
use std::f64::consts::PI;

fn render_all(session: &mut Session, frames: usize, blocks: &[usize]) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
    let (mut at, mut i) = (0, 0);
    while at < frames {
        let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
        i += 1;
        session.render(at as u64, &mut l[at..at + len], &mut r[at..at + len], &token).unwrap();
        at += len;
    }
    [l, r]
}

/// Lookahead dynamics that never reduces (threshold 0 dB, hard knee, |x| < 1): a pure delay of
/// `ms` milliseconds.
fn transparent_lookahead(channels: usize, ms: f64) -> SidechainDynamics {
    let dynamics = DynamicsParams { knee_db: 0.0, ..DynamicsParams::compressor(0.0, 4.0) };
    SidechainDynamics::new(RATE, channels, SidechainParams { dynamics, key: KeySource::Internal, lookahead_ms: ms }).unwrap()
}

fn faded_tones(frames: usize) -> Vec<f32> {
    let a = sine(440.0, 0.3, 0.0, RATE, frames);
    let b = sine(1234.0, 0.2, 0.7, RATE, frames);
    (0..frames)
        .map(|i| {
            // Raised-cosine fade in and out over 1000 samples (an abrupt edge is SMB-SCORE-EFFECTS-4
            // slice 3's subject, not this test's).
            let edge = i.min(frames - 1 - i).min(1000) as f64;
            (0.5 - 0.5 * (PI * edge / 1000.0).cos()) as f32 * (a[i] + b[i])
        })
        .collect()
}

#[test]
fn latent_stacks_line_up_sample_exact_with_dry_tracks() {
    bounded_case("pdc_alignment", || {
        let frames = 12_000;
        // Track 0: stereo noise through two transparent lookahead stages (240 + 144 samples).
        let (l0, r0) = (noise(1, 8000), noise(2, 8000));
        let mut t0 = TrackSpec::new(vec![l0.clone(), r0.clone()], 500);
        t0.effects = vec![
            EffectSlot::new(Box::new(transparent_lookahead(2, 5.0))),
            EffectSlot::new(Box::new(transparent_lookahead(2, 3.0))),
        ];
        // Track 1: dry stereo noise. Track 2: mono faded tones through ClickRepair (latency 1792).
        let (l1, r1) = (noise(3, 9000), noise(4, 9000));
        let t1 = TrackSpec::new(vec![l1.clone(), r1.clone()], 1000);
        let tones = faded_tones(10_000);
        let mut t2 = TrackSpec::new(vec![tones.clone()], 0);
        t2.effects = vec![EffectSlot::new(Box::new(ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap()))];
        let mut session = Session::new(RATE, 1024, vec![t0, t1, t2]).unwrap();
        assert_eq!((session.track_latency(0), session.track_latency(1), session.track_latency(2)), (Some(384), Some(0), Some(1792)));
        let out = render_all(&mut session, frames, &[1024]);
        let centre = pan_gains(0.0);
        for t in 0..frames {
            let (mut l, mut r) = (0.0_f64, 0.0_f64);
            if (500..8500).contains(&t) {
                l += f64::from(l0[t - 500]);
                r += f64::from(r0[t - 500]);
            }
            if (1000..10_000).contains(&t) {
                l += f64::from(l1[t - 1000]);
                r += f64::from(r1[t - 1000]);
            }
            if t < 10_000 {
                l += f64::from(tones[t]) * centre.0;
                r += f64::from(tones[t]) * centre.1;
            }
            assert_eq!((out[0][t].to_bits(), out[1][t].to_bits()), ((l as f32).to_bits(), (r as f32).to_bits()), "t {t}");
        }
        // Any block partition, including single frames (priming then runs in 1-frame chunks).
        for blocks in [&[1usize][..], &[7, 1000, 333]] {
            let (l0, r0) = (noise(1, 8000), noise(2, 8000));
            let mut t0 = TrackSpec::new(vec![l0, r0], 500);
            t0.effects = vec![EffectSlot::new(Box::new(transparent_lookahead(2, 5.0))), EffectSlot::new(Box::new(transparent_lookahead(2, 3.0)))];
            let t1 = TrackSpec::new(vec![noise(3, 9000), noise(4, 9000)], 1000);
            let mut t2 = TrackSpec::new(vec![faded_tones(10_000)], 0);
            t2.effects = vec![EffectSlot::new(Box::new(ClickRepair::new(RATE, 1, ClickRepairParams::default()).unwrap()))];
            let max_block = blocks.iter().copied().max().unwrap();
            let mut s = Session::new(RATE, max_block, vec![t0, t1, t2]).unwrap();
            let got = render_all(&mut s, frames, blocks);
            assert_eq!(bits(&got[0]), bits(&out[0]), "{blocks:?}");
            assert_eq!(bits(&got[1]), bits(&out[1]), "{blocks:?}");
        }
    });
}

#[test]
fn compensated_stack_equals_the_stages_run_by_hand_shifted_by_their_latency() {
    bounded_case("pdc_by_hand", || {
        // Stage 0: compressing lookahead (latency 240); stage 1: Dynamics whose threshold steps
        // from -10 to -30 dB for the audio of timeline time 3000 onward (stage 1 reads audio
        // 240 samples late, so by hand the step lands on its input index 3240).
        let frames = 9000;
        let clip = sine(220.0, 0.7, 0.0, RATE, 6000);
        let lookahead = || SidechainDynamics::new(RATE, 1, SidechainParams { dynamics: DynamicsParams::compressor(-12.0, 3.0), key: KeySource::Internal, lookahead_ms: 5.0 }).unwrap();
        let stage1 = || Dynamics::new(RATE, 1, DynamicsParams::compressor(-10.0, 4.0)).unwrap();
        let lane = AutomationLane::new(&[(0, -10.0), (3000, -30.0)], Interpolation::Step, (-100.0, 0.0)).unwrap();
        let mut t = TrackSpec::new(vec![clip.clone()], 1000);
        t.effects = vec![EffectSlot::new(Box::new(lookahead())), EffectSlot::new(Box::new(stage1())).with_lane(0, lane)];
        let mut session = Session::new(RATE, 512, vec![t]).unwrap();
        let out = render_all(&mut session, frames, &[512]);

        // By hand: the timeline signal (clip at 1000, zeros elsewhere), padded by the latency.
        let l = 240;
        let mut timeline = vec![0.0_f32; frames + l];
        timeline[1000..7000].copy_from_slice(&clip);
        let s0 = run_blocks(&mut lookahead(), &mono(), RATE, &[timeline], &[4096]);
        let mut by_hand = stage1();
        let a = run_blocks(&mut by_hand, &mono(), RATE, &[s0[0][..3000 + l].to_vec()], &[4096]);
        by_hand.set_automated(0, -30.0).unwrap();
        let b = run_blocks(&mut by_hand, &mono(), RATE, &[s0[0][3000 + l..].to_vec()], &[4096]);
        let stack: Vec<f32> = a[0].iter().chain(&b[0]).copied().collect();
        let centre = pan_gains(0.0).0;
        for t in 0..frames {
            let want = (f64::from(stack[t + l]) * centre) as f32;
            assert_eq!(out[0][t].to_bits(), want.to_bits(), "t {t}");
        }
        // reset() re-primes: a second render from 0 repeats every bit.
        session.reset();
        assert_eq!(bits(&render_all(&mut session, frames, &[300])[0]), bits(&out[0]));
    });
}
