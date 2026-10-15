//! Lane SMB-AUDITION-SESSION: S-curve and logarithmic fades, loudness normalisation to a BS.1770
//! integrated target with a true-peak ceiling (score's own meters), and the session routing graph
//! (pre/post-fader sends, aux returns, bus effects, master bus) with latency compensation across
//! buses. Oracles: the stated fade formulas, the EBU Tech 3341 fact that a stereo 1 kHz sine at
//! -23 dBFS per channel reads -23.0 LUFS (+-0.1), and hand-placed impulses whose arrival samples and
//! levels follow from the routing.
mod common_dsp;
use common_dsp::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::*;

fn render(graph: &mut MixGraph, frames: usize, block: usize) -> [Vec<f32>; 2] {
    let token = CancellationToken::default();
    let (mut l, mut r) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
    let mut at = 0;
    while at < frames {
        let n = block.min(frames - at);
        graph.render(at as u64, &mut l[at..at + n], &mut r[at..at + n], &token).unwrap();
        at += n;
    }
    [l, r]
}

/// S-curve: (1 - cos(p pi)) / 2 in, (1 + cos(p pi)) / 2 out, the pair sums to 1. Logarithmic: linear
/// in dB over 60 dB, silent exactly at its silent end. Through a session on a DC clip the faded
/// samples are the formulas.
#[test]
fn s_curve_and_logarithmic_fades_follow_their_formulas() {
    bounded_case("fades", || {
        let pi = std::f64::consts::PI;
        for frames in [1u64, 2, 100, 4801] {
            let s = Fade { frames, curve: FadeCurve::SCurve };
            let g = Fade { frames, curve: FadeCurve::Logarithmic };
            assert_eq!((s.in_gain(0), s.in_gain(frames), s.out_gain(0), s.out_gain(frames)), (0.0, 1.0, 1.0, 0.0));
            assert_eq!((g.in_gain(0), g.in_gain(frames), g.out_gain(0), g.out_gain(frames)), (0.0, 1.0, 1.0, 0.0));
            for k in 0..=frames {
                let p = k as f64 / frames as f64;
                assert!((s.in_gain(k) - (1.0 - (p * pi).cos()) / 2.0).abs() < 1e-15);
                assert!((s.in_gain(k) + s.out_gain(k) - 1.0).abs() < 1e-12, "s pair {k}");
                if k > 0 {
                    let want_db = -FADE_LOG_RANGE_DB * (1.0 - p);
                    assert!((20.0 * g.in_gain(k).log10() - want_db).abs() < 1e-9, "log {k}");
                }
                if k > 0 && k < frames {
                    assert!(s.in_gain(k) > s.in_gain(k - 1) && g.in_gain(k) > g.in_gain(k - 1), "rising {k}");
                }
                assert!((g.out_gain(k) - g.in_gain(frames - k)).abs() < 1e-15, "log out mirrors in");
            }
        }
        for curve in [FadeCurve::SCurve, FadeCurve::Logarithmic] {
            let mut t = TrackSpec::new(vec![vec![0.5; 1000], vec![0.5; 1000]], 0);
            t.fade_in = Some(Fade { frames: 300, curve });
            t.fade_out = Some(Fade { frames: 200, curve });
            let (fi, fo) = (t.fade_in.unwrap(), t.fade_out.unwrap());
            let mut session = Session::new(48_000, 512, vec![t]).unwrap();
            let token = CancellationToken::default();
            let (mut l, mut r) = (vec![0.0; 1000], vec![0.0; 1000]);
            for at in (0..1000).step_by(500) {
                session.render(at as u64, &mut l[at..at + 500], &mut r[at..at + 500], &token).unwrap();
            }
            for k in 0..1000u64 {
                let mut g = 1.0;
                if k < 300 {
                    g *= fi.in_gain(k);
                }
                if k >= 800 {
                    g *= fo.out_gain(k - 800);
                }
                let want = if g == 1.0 { 0.5 } else { (0.5 * g) as f32 };
                assert_eq!(l[k as usize], want, "{curve:?} {k}");
                assert_eq!(r[k as usize], want);
            }
        }
    });
}

#[test]
fn loudness_normalise_reaches_the_target_under_a_true_peak_ceiling() {
    bounded_case("normalise", || {
        let token = CancellationToken::default();
        let rate = 48_000;
        let frames = rate as usize * 6;
        // Tech 3341 case: stereo 1 kHz at -23 dBFS reads -23.0 LUFS.
        let a = 10f64.powf(-23.0 / 20.0);
        let tone = sine(1000.0, a, 0.0, rate, frames);
        let (i, tp) = measure_loudness(rate, &[&tone, &tone], &[1.0, 1.0], &token).unwrap();
        assert!((i + 23.0).abs() <= 0.1, "{i}");
        assert!((tp - 20.0 * a.log10()).abs() < 0.1, "{tp}");
        // 1. No limiting needed: -23 -> -16 LUFS, a gain of +7 dB.
        let req = NormalizeRequest {
            sample_rate_hz: rate,
            channels: &[&tone, &tone],
            weights: &[1.0, 1.0],
            target_lufs: -16.0,
            ceiling_dbtp: -1.0,
            policy: PeakPolicy::ReduceGain,
        };
        let (out, r) = loudness_normalize(&req, &token).unwrap();
        assert!(!r.limited && !r.gain_reduced, "{r:?}");
        assert!((r.gain_db - (-16.0 - i)).abs() < 1e-12);
        assert!((r.output_integrated_lufs + 16.0).abs() <= 0.02, "{r:?}");
        assert_eq!(out[0].len(), frames);
        // 2. A peaky signal (tone plus sparse full-scale clicks) to -12 LUFS: the gain would put the
        //    peaks over -1 dBTP. ReduceGain lands the peak on the ceiling and misses the target.
        let mut peaky = sine(440.0, 0.05, 0.3, rate, frames);
        for k in (0..frames).step_by(4800) {
            peaky[k] = 0.9;
        }
        let ch: [&[f32]; 2] = [&peaky, &peaky];
        let reduce = NormalizeRequest { channels: &ch, target_lufs: -12.0, ..req };
        let (_, r) = loudness_normalize(&reduce, &token).unwrap();
        assert!(r.gain_reduced && !r.limited, "{r:?}");
        assert!((r.output_true_peak_dbtp + 1.0).abs() <= 0.01, "{r:?}");
        assert!(r.output_integrated_lufs < -12.0 - 1.0, "{r:?}");
        // Limit keeps the target and holds the ceiling (score's true-peak limiter, compensated).
        let limit = NormalizeRequest { policy: PeakPolicy::Limit { lookahead_ms: 5.0, release_ms: 100.0 }, ..reduce };
        let (out, r) = loudness_normalize(&limit, &token).unwrap();
        assert!(r.limited && r.limiter_latency_frames > 0, "{r:?}");
        assert!((r.output_integrated_lufs + 12.0).abs() <= NORMALIZE_TOLERANCE_LU, "{r:?}");
        assert!(r.output_true_peak_dbtp <= -1.0 + 0.1, "{r:?}");
        assert!(out[0].iter().all(|v| v.abs() <= 10f32.powf(-1.0 / 20.0) + 1e-6), "sample peak under the ceiling");
        assert_eq!(out[0].len(), frames, "latency compensated: same length");
        // Alignment: the output correlates best with the input at lag 0 (latency removed).
        let corr = |lag: i64| -> f64 {
            (1000..frames - 1000).map(|k| f64::from(out[0][k]) * f64::from(peaky[(k as i64 + lag) as usize])).sum()
        };
        let best = (-40..=40).max_by(|a, b| corr(*a).total_cmp(&corr(*b))).unwrap();
        assert_eq!(best, 0, "aligned");
        // Refusals.
        let silence = vec![0.0f32; frames];
        let silent = NormalizeRequest { channels: &[&silence], weights: &[1.0], ..req };
        assert_eq!(loudness_normalize(&silent, &token).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(loudness_normalize(&NormalizeRequest { target_lufs: 3.0, ..req }, &token).unwrap_err(), ScoreError::OutOfRange);
        assert_eq!(loudness_normalize(&NormalizeRequest { target_lufs: f64::NAN, ..req }, &token).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(loudness_normalize(&NormalizeRequest { weights: &[1.0], ..req }, &token).unwrap_err(), ScoreError::ChannelMismatch);
    });
}

fn impulse_track(len: usize, at: usize, value: f32) -> TrackSpec {
    let mut c = vec![0.0f32; len];
    c[at] = value;
    TrackSpec::new(vec![c.clone(), c], 0)
}

/// A limiter far above the signal is a pure delay of its reported latency (gain exactly 1).
fn delay_effect(rate: u32) -> EffectSlot {
    EffectSlot::new(Box::new(LookaheadLimiter::new(rate, 2, LimiterParams { ceiling_db: 0.0, lookahead_ms: 3.0, release_ms: 50.0, true_peak: false }).unwrap()))
}

/// Routing and latency compensation: a direct track, a track through a latent bus, a track through
/// two chained latent buses and a pre-fader send to an aux return all arrive at the master on the
/// same output sample as the impulse's timeline position, with the levels the routing states.
#[test]
fn sends_buses_and_master_are_latency_compensated_and_levelled() {
    bounded_case("graph", || {
        let rate = 48_000;
        let len = 4000;
        let lim_latency = u64::from(delay_effect(rate).processor.latency_samples());
        assert!(lim_latency > 0);
        let strips = vec![
            // 0: direct to master at 0.25.
            StripSpec::track(impulse_track(len, 1000, 0.25), Destination::Master, vec![]),
            // 1: through bus 0 (latent), at 0.125.
            StripSpec::track(impulse_track(len, 1000, 0.125), Destination::Bus(0), vec![]),
            // 2: through bus 1 -> bus 2 (both latent), at 0.0625.
            StripSpec::track(impulse_track(len, 1000, 0.0625), Destination::Bus(1), vec![]),
            // 3: fader at -96 dB to the master, pre-fader send at -6.0206 dB to aux bus 3 (no effects).
            StripSpec::track(
                {
                    let mut t = impulse_track(len, 2000, 0.5);
                    t.volume_db = AutomationLane::constant(-96.0);
                    t
                },
                Destination::Master,
                vec![SendSpec { bus: 3, tap: SendTap::PreFader, level_db: AutomationLane::constant(-6.020_599_913_279_624), pan: 0.0 }],
            ),
        ];
        let mut b0 = BusSpec::new("bus-a");
        b0.effects = vec![delay_effect(rate)];
        let mut b1 = BusSpec::new("bus-b");
        b1.effects = vec![delay_effect(rate)];
        b1.output = Destination::Bus(2);
        let mut b2 = BusSpec::new("bus-c");
        b2.effects = vec![delay_effect(rate)];
        let mut aux = BusSpec::new("aux-return");
        aux.volume_db = AutomationLane::constant(-6.020_599_913_279_624);
        let mut graph = MixGraph::new(rate, 256, strips, vec![b0, b1, b2, aux], MasterSpec::default()).unwrap();
        let lat = graph.latency().clone();
        assert_eq!(lat.buses[0], (0, lim_latency, lim_latency));
        assert_eq!(lat.buses[1], (0, lim_latency, lim_latency));
        assert_eq!(lat.buses[2], (lim_latency, lim_latency, 2 * lim_latency));
        assert_eq!(lat.total, 2 * lim_latency);
        let [l, r] = render(&mut graph, len, 256);
        // Every path arrives at sample 1000: 0.25 + 0.125 + 0.0625.
        assert!((l[1000] - 0.4375).abs() < 1e-6 && (r[1000] - 0.4375).abs() < 1e-6, "{} {}", l[1000], r[1000]);
        // The aux: 0.5 pre-fader x 0.5 send x 0.5 aux fader = 0.125, plus the -96 dB direct path.
        let direct = 0.5 * 10f64.powf(-96.0 / 20.0);
        assert!((f64::from(l[2000]) - (0.125 + direct)).abs() < 1e-6, "{}", l[2000]);
        let elsewhere: f32 = l.iter().enumerate().filter(|(k, _)| *k != 1000 && *k != 2000).map(|(_, v)| v.abs()).sum();
        assert!(elsewhere < 1e-6, "no energy off the impulse samples: {elsewhere}");
        // Block partition does not change the output (pre-roll larger than one block of 7).
        let rebuild = || {
            let mut b = BusSpec::new("bus-a");
            b.effects = vec![delay_effect(rate)];
            MixGraph::new(rate, 7, vec![StripSpec::track(impulse_track(len, 1000, 0.125), Destination::Bus(0), vec![])], vec![b], MasterSpec::default()).unwrap()
        };
        let [a, _] = render(&mut rebuild(), len, 7);
        let [b, _] = render(&mut rebuild(), len, 5);
        assert_eq!(bits(&a), bits(&b));
        assert_eq!(a[1000], 0.125);
    });
}

/// Post-fader sends follow the fader, bus pan and master volume apply, one direct track equals the
/// one-track session, and invalid routings are refused before rendering.
#[test]
fn post_fader_sends_master_volume_and_refusals() {
    bounded_case("graph-2", || {
        let rate = 48_000;
        let token = CancellationToken::default();
        let src = vec![noise(7, 3000), noise(8, 3000)];
        // One track alone equals Session with the same track.
        let mk = || {
            let mut t = TrackSpec::new(src.clone(), 100);
            t.volume_db = AutomationLane::new(&[(0, -12.0), (2000, 3.0)], Interpolation::Linear, TRACK_VOLUME_DB).unwrap();
            t.pan = AutomationLane::constant(0.3);
            t
        };
        let mut session = Session::new(rate, 620, vec![mk()]).unwrap();
        let (mut sl, mut sr) = (vec![0.0; 3100], vec![0.0; 3100]);
        for at in (0..3100).step_by(620) {
            session.render(at as u64, &mut sl[at..at + 620], &mut sr[at..at + 620], &token).unwrap();
        }
        let mut graph = MixGraph::new(rate, 620, vec![StripSpec::track(mk(), Destination::Master, vec![])], vec![], MasterSpec::default()).unwrap();
        let [gl, gr] = render(&mut graph, 3100, 620);
        assert_eq!(bits(&gl), bits(&sl));
        assert_eq!(bits(&gr), bits(&sr));
        // Post-fader send at 0 dB to a bus panned hard right; master at -6.0206 dB.
        let mut t = impulse_track(100, 10, 0.5);
        t.volume_db = AutomationLane::constant(-6.020_599_913_279_624);
        let mut bus = BusSpec::new("fx");
        bus.pan = AutomationLane::constant(1.0);
        let master = MasterSpec { effects: vec![], volume_db: AutomationLane::constant(-6.020_599_913_279_624) };
        let send = SendSpec { bus: 0, tap: SendTap::PostFader, level_db: AutomationLane::constant(0.0), pan: 0.0 };
        let mut graph = MixGraph::new(rate, 64, vec![StripSpec::track(t, Destination::Bus(0), vec![send.clone()])], vec![bus], MasterSpec { ..master }).unwrap();
        let [l, r] = render(&mut graph, 100, 64);
        // Track post-fader 0.25 reaches the bus twice (output + send) = 0.5; hard right: L 0, R 0.5;
        // master halves it.
        assert!(l[10].abs() < 1e-7 && (r[10] - 0.25).abs() < 1e-6, "{} {}", l[10], r[10]);
        // Refusals.
        let one = || vec![StripSpec::track(impulse_track(10, 0, 0.1), Destination::Bus(0), vec![])];
        let cyc = |a: usize, b: usize| {
            let mut x = BusSpec::new("x");
            x.output = Destination::Bus(a);
            let mut y = BusSpec::new("y");
            y.output = Destination::Bus(b);
            vec![x, y]
        };
        assert_eq!(MixGraph::new(rate, 64, one(), cyc(1, 0), MasterSpec::default()).err(), Some(ScoreError::OutOfRange), "cycle");
        assert_eq!(MixGraph::new(rate, 64, one(), vec![], MasterSpec::default()).err(), Some(ScoreError::OutOfRange), "missing bus");
        let mut selfsend = BusSpec::new("s");
        selfsend.sends = vec![SendSpec { bus: 0, ..send.clone() }];
        assert_eq!(MixGraph::new(rate, 64, one(), vec![selfsend], MasterSpec::default()).err(), Some(ScoreError::OutOfRange), "self send");
        let loud = SendSpec { level_db: AutomationLane::constant(40.0), ..send.clone() };
        let mut b = BusSpec::new("b");
        b.output = Destination::Master;
        assert_eq!(
            MixGraph::new(rate, 64, vec![StripSpec::track(impulse_track(10, 0, 0.1), Destination::Master, vec![loud])], vec![b], MasterSpec::default()).err(),
            Some(ScoreError::OutOfRange),
            "send level out of range"
        );
        let mono_fx = EffectSlot::new(Box::new(LookaheadLimiter::new(rate, 1, LimiterParams::default()).unwrap()));
        let mut bad = BusSpec::new("mono-fx");
        bad.effects = vec![mono_fx];
        assert_eq!(MixGraph::new(rate, 64, one(), vec![bad], MasterSpec::default()).err(), Some(ScoreError::ChannelMismatch));
    });
}

/// Lane SMB-AUDITION-SESSION-2: a seek lands on the same samples as the continuous render, near the
/// start (warm-up reaches sample 0) and far into the timeline (bus effect memory shorter than the
/// warm-up); an outside `StripSource` drives a strip like a score track.
#[test]
fn graph_seek_matches_the_continuous_render_and_sources_drive_strips() {
    bounded_case("graph-seek", || {
        let rate = 48_000;
        let len = 3 * rate as usize;
        let build = || {
            let mut t = TrackSpec::new(vec![noise(11, len), noise(12, len)], 0);
            t.volume_db = AutomationLane::new(&[(0, -6.0), (len as u64, 0.0)], Interpolation::Linear, TRACK_VOLUME_DB).unwrap();
            let mut bus = BusSpec::new("delay-bus");
            bus.effects = vec![delay_effect(rate)];
            let send = SendSpec { bus: 0, tap: SendTap::PostFader, level_db: AutomationLane::constant(-3.0), pan: 0.2 };
            MixGraph::new(rate, 1024, vec![StripSpec::track(t, Destination::Master, vec![send])], vec![bus], MasterSpec::default()).unwrap()
        };
        let [full_l, full_r] = render(&mut build(), len, 1024);
        let token = CancellationToken::default();
        for at in [100usize, 70_000, 120_001] {
            let mut g = build();
            // Render a little first, then jump (backwards for the last case is covered by the reset).
            let mut l = vec![0.0; 1000];
            let mut r = vec![0.0; 1000];
            g.render(0, &mut l, &mut r, &token).unwrap();
            g.seek(at as u64, &token).unwrap();
            assert_eq!(g.position(), at as u64);
            let n = 4000.min(len - at);
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            for k in (0..n).step_by(1000) {
                let m = 1000.min(n - k);
                g.render((at + k) as u64, &mut l[k..k + m], &mut r[k..k + m], &token).unwrap();
            }
            assert_eq!(bits(&l), bits(&full_l[at..at + n]), "seek {at}");
            assert_eq!(bits(&r), bits(&full_r[at..at + n]), "seek {at}");
        }

        /// A mono source: an impulse train, seekable.
        struct Impulses {
            pos: u64,
            len: u64,
        }
        impl StripSource for Impulses {
            fn channels(&self) -> usize {
                1
            }
            fn length_samples(&self) -> u64 {
                self.len
            }
            fn position(&self) -> u64 {
                self.pos
            }
            fn seek(&mut self, sample: u64, _: &CancellationToken) -> Result<(), ScoreError> {
                self.pos = sample;
                Ok(())
            }
            fn render(&mut self, left: &mut [f32], _right: &mut [f32], _: &CancellationToken) -> Result<(), ScoreError> {
                for (k, v) in left.iter_mut().enumerate() {
                    *v = if (self.pos + k as u64).is_multiple_of(100) { 0.5 } else { 0.0 };
                }
                self.pos += left.len() as u64;
                Ok(())
            }
        }
        let strip = StripSpec::source(Box::new(Impulses { pos: 0, len: 1000 }), AutomationLane::constant(0.0), AutomationLane::constant(-1.0), Destination::Master, vec![]);
        let mut g = MixGraph::new(rate, 256, vec![strip], vec![], MasterSpec::default()).unwrap();
        assert_eq!(g.length_samples(), 1000);
        let [l, r] = render(&mut g, 1000, 256);
        for k in 0..1000 {
            assert_eq!(l[k], if k % 100 == 0 { 0.5 } else { 0.0 }, "mono hard left {k}");
            assert_eq!(r[k], 0.0);
        }
    });
}

/// Lane SMB-AUDITION-UI: bus effect keyframes (a gain lane on a bus, linear in dB, at the timeline
/// sample the stage reads) and exact seeks through long effect tails (a 4 s reverb on a bus: the
/// default `SeekMode::Exact` lands bit for bit on the continuous render forward and backward; a
/// 1 s `Preroll` provably does not, which is why it is opt-in). Meters: peaks and loudness.
#[test]
fn bus_keyframes_exact_seek_through_long_tails_and_meters() {
    bounded_case("graph-ui", || {
        let rate = 48_000;
        let token = CancellationToken::default();
        // Keyframed bus gain: 0 dB at 0 falling to -12 dB at 1 s, held after.
        let dc = TrackSpec::new(vec![vec![0.25; 72_000], vec![0.25; 72_000]], 0);
        let mut bus = BusSpec::new("ramp");
        let lane = AutomationLane::new(&[(0, 0.0), (48_000, -12.0)], Interpolation::Linear, GAIN_AUTOMATION_DB).unwrap();
        bus.effects = vec![EffectSlot::new(Box::new(Gain::from_db(0.0).unwrap())).with_lane(0, lane)];
        let mut g = MixGraph::new(rate, 1000, vec![StripSpec::track(dc, Destination::Bus(0), vec![])], vec![bus], MasterSpec::default()).unwrap();
        let [l, _] = render(&mut g, 72_000, 1000);
        for k in [0usize, 1, 24_000, 47_999, 48_000, 71_999] {
            let db = if k < 48_000 { -12.0 * k as f64 / 48_000.0 } else { -12.0 };
            let want = 0.25 * 10f64.powf(db / 20.0);
            assert!((f64::from(l[k]) - want).abs() < 1e-6, "{k}: {} vs {want}", l[k]);
        }
        let m = g.meters();
        assert!((m.master[0] - 0.25).abs() < 1e-6 && (m.strips[0][0] - 0.25).abs() < 1e-6 && (m.buses[0][0] - 0.25).abs() < 1e-6, "{m:?}");
        // Out-of-range lane values are refused before rendering.
        let mut bad = BusSpec::new("bad");
        let lane = AutomationLane::new(&[(0, 100.0)], Interpolation::Step, (-1000.0, 1000.0)).unwrap();
        bad.effects = vec![EffectSlot::new(Box::new(Gain::from_db(0.0).unwrap())).with_lane(0, lane)];
        let dc = TrackSpec::new(vec![vec![0.25; 10]], 0);
        assert_eq!(MixGraph::new(rate, 64, vec![StripSpec::track(dc, Destination::Bus(0), vec![])], vec![bad], MasterSpec::default()).err(), Some(ScoreError::OutOfRange));

        // A long reverb tail on a bus: noise for 1 s, then 4 s of tail.
        let len = 5 * rate as usize;
        let build = || {
            let mut src = vec![noise(21, rate as usize), noise(22, rate as usize)];
            src.iter_mut().for_each(|c| c.resize(len, 0.0));
            let t = TrackSpec::new(src, 0);
            let mut bus = BusSpec::new("verb");
            let params = ReverbParams { rt60_s: 4.0, ..ReverbParams::default() };
            bus.effects = vec![EffectSlot::new(Box::new(Reverb::new(rate, 2, params).unwrap()))];
            MixGraph::new(rate, 1000, vec![StripSpec::track(t, Destination::Bus(0), vec![])], vec![bus], MasterSpec::default()).unwrap()
        };
        let [full, _] = render(&mut build(), len, 1000);
        let at = 4 * rate as usize;
        let tail = |g: &mut MixGraph| {
            let (mut l, mut r) = (vec![0.0; 1000], vec![0.0; 1000]);
            g.render(at as u64, &mut l, &mut r, &token).unwrap();
            l
        };
        // Exact, forward from 0.
        let mut g = build();
        assert_eq!(g.seek_mode(), SeekMode::Exact);
        g.seek(at as u64, &token).unwrap();
        assert_eq!(bits(&tail(&mut g)), bits(&full[at..at + 1000]), "exact forward seek");
        // Exact, backward after rendering past the target.
        let mut g = build();
        g.seek(at as u64 + 5000, &token).unwrap();
        g.seek(at as u64, &token).unwrap();
        assert_eq!(bits(&tail(&mut g)), bits(&full[at..at + 1000]), "exact backward seek");
        // A 1 s pre-roll misses the earlier part of the tail.
        let mut g = build();
        g.set_seek_mode(SeekMode::Preroll { seconds: 1 });
        g.seek(at as u64, &token).unwrap();
        let approx = tail(&mut g);
        let worst = approx.iter().zip(&full[at..at + 1000]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst > 1e-6, "the short pre-roll is not exact here ({worst}), so Exact is the default");
        assert!(full[at..at + 1000].iter().any(|v| v.abs() > 1e-4), "the tail is audible at 4 s");
        // Loudness of the master is measured on rendered output.
        let mut g = build();
        let [_, _] = render(&mut g, rate as usize, 1000);
        assert!(g.meters().loudness.integrated_lufs.is_some_and(f64::is_finite), "{:?}", g.meters().loudness);
    });
}
