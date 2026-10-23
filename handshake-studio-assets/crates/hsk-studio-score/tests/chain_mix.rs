use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";

fn ctx(revision: u64) -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(
            ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .unwrap(),
        ),
        expected_revision: revision,
    }
}
fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 1)
}
fn mono() -> ChannelLayout {
    ChannelLayout::mono(7)
}
fn stage(processor: impl Processor + Send + 'static, output: ChannelLayout) -> ChainStage {
    ChainStage {
        processor: Box::new(processor),
        output,
    }
}
fn spec(layout: ChannelLayout, frames: usize, start_sample: u64) -> BlockSpec {
    BlockSpec {
        sample_rate_hz: 48_000,
        layout,
        frames,
        start_sample,
    }
}

#[test]
fn chain_stack_exact_values_latency_and_bypass() {
    bounded_case("chain_stack_exact_values_latency_and_bypass", || {
        let (engine, token) = (engine(), CancellationToken::default());
        let l = [1.0_f32, 0.5, 0.0, -0.5];
        let r = [0.5_f32, -0.25, 0.0, 0.75];
        let sp = spec(ChannelLayout::stereo(), 4, 0);
        let channels: [&[f32]; 2] = [&l, &r];
        let input = InputBlock::new(&sp, &channels);

        // Gain 0.5 -> stereo-to-mono -> Gain 2.0 over the exact fixture.
        let mut chain = Chain::new(
            ChannelLayout::stereo(),
            256,
            vec![
                stage(Gain::new(0.5).unwrap(), ChannelLayout::stereo()),
                stage(ChannelMix::stereo_to_mono(), mono()),
                stage(Gain::new(2.0).unwrap(), mono()),
            ],
        )
        .unwrap();
        let mut out = OutputBlock::with_capacity(mono(), 256).unwrap();
        let receipt = engine.process(&ctx(1), &input, &mut chain, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.75, 0.125, 0.0, 0.125]);
        assert_eq!((receipt.latency_samples, receipt.labels()), (0, &[7][..]));

        // Bypass: the trailing Gain disappears; the layout-changing mix cannot be bypassed.
        chain.set_bypass(2, true).unwrap();
        assert_eq!(chain.set_bypass(1, true).unwrap_err(), ScoreError::ChannelMismatch);
        assert_eq!(chain.set_bypass(9, true).unwrap_err(), ScoreError::OutOfRange);
        engine.process(&ctx(1), &input, &mut chain, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.375, 0.0625, 0.0, 0.0625]);
        chain.set_bypass(2, false).unwrap();
        engine.process(&ctx(1), &input, &mut chain, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[0.75, 0.125, 0.0, 0.125]);

        // Algebra: Gain(0.5) -> Gain(0.5) is bit-identical to Gain(0.25).
        let mut two = Chain::new(
            ChannelLayout::stereo(),
            256,
            vec![
                stage(Gain::new(0.5).unwrap(), ChannelLayout::stereo()),
                stage(Gain::new(0.5).unwrap(), ChannelLayout::stereo()),
            ],
        )
        .unwrap();
        let mut a = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        let mut b = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        engine.process(&ctx(1), &input, &mut two, &mut a, &token).unwrap();
        engine.process(&ctx(1), &input, &mut Gain::new(0.25).unwrap(), &mut b, &token).unwrap();
        for ch in 0..2 {
            assert_eq!(a.published_channel(ch), b.published_channel(ch));
        }

        // Latency is the live sum over active stages: limiter 341 samples, 0 when bypassed.
        let mut limited = Chain::new(
            ChannelLayout::stereo(),
            256,
            vec![
                stage(Gain::new(1.0).unwrap(), ChannelLayout::stereo()),
                stage(HardLimiter::new(48_000, 2, HardLimiterParams::default()).unwrap(), ChannelLayout::stereo()),
            ],
        )
        .unwrap();
        assert_eq!(limited.latency_samples(), 341);
        let receipt = engine.process(&ctx(1), &input, &mut limited, &mut a, &token).unwrap();
        assert_eq!(receipt.latency_samples, 341);
        limited.set_bypass(1, true).unwrap();
        let receipt = engine.process(&ctx(1), &input, &mut limited, &mut a, &token).unwrap();
        assert_eq!(receipt.latency_samples, 0);
        assert_eq!(a.published_channel(0).unwrap(), &l);

        // Refusals: block larger than the stack's scratch, wrong output layout, bad stacks.
        let big = vec![0.0_f32; 300];
        let big_spec = spec(ChannelLayout::stereo(), 300, 0);
        let big_channels: [&[f32]; 2] = [&big, &big];
        let mut wide = OutputBlock::with_capacity(ChannelLayout::stereo(), 512).unwrap();
        let before = wide.backing().to_vec();
        let refused = engine.process(&ctx(1), &InputBlock::new(&big_spec, &big_channels), &mut limited, &mut wide, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::CapacityExceeded);
        assert_eq!((wide.backing(), wide.published_frames()), (&before[..], 0));
        let refused = engine.process(&ctx(1), &input, &mut limited, &mut out, &token);
        assert_eq!(refused.unwrap_err(), ScoreError::ChannelMismatch);
        let pan_first = Chain::new(ChannelLayout::stereo(), 64, vec![stage(Pan::new(0.0).unwrap(), ChannelLayout::stereo())]);
        assert_eq!(pan_first.err(), Some(ScoreError::ChannelMismatch));
        let lim = |rate| HardLimiter::new(rate, 2, HardLimiterParams::default()).unwrap();
        let mixed_rates = Chain::new(
            ChannelLayout::stereo(),
            64,
            vec![stage(lim(48_000), ChannelLayout::stereo()), stage(lim(44_100), ChannelLayout::stereo())],
        );
        assert_eq!(mixed_rates.err(), Some(ScoreError::InvalidRate));
        assert_eq!(Chain::new(ChannelLayout::stereo(), 64, vec![]).err(), Some(ScoreError::OutOfRange));
    });
}

#[test]
fn mix_sends_sum_exactly_and_refuse_before_write() {
    bounded_case("mix_sends_sum_exactly_and_refuse_before_write", || {
        let (engine, token) = (engine(), CancellationToken::default());
        let (a, b) = ([1.0_f32, 2.0, 3.0, 4.0], [4.0_f32; 4]);
        let sp = spec(mono(), 4, 100);
        let (ca, cb): ([&[f32]; 1], [&[f32]; 1]) = ([&a], [&b]);
        let sends = [
            SendRoute { input: InputBlock::new(&sp, &ca), level: 1.0 },
            SendRoute { input: InputBlock::new(&sp, &cb), level: 0.5 },
        ];
        let mut out = OutputBlock::with_capacity(mono(), 8).unwrap();
        let receipt = engine.mix(&ctx(1), &sends, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[3.0, 4.0, 5.0, 6.0]);
        assert_eq!((receipt.processed_frames, receipt.start_sample, receipt.end_sample), (4, 100, 104));
        assert_eq!(receipt.peak(), &[6.0]);
        // Submix = level 0 for one send leaves only the other.
        let muted = [SendRoute { level: 0.0, ..sends[0] }, sends[1]];
        engine.mix(&ctx(1), &muted, &mut out, &token).unwrap();
        assert_eq!(out.published_channel(0).unwrap(), &[2.0; 4]);

        // Every refusal leaves the previously published submix untouched.
        let before = (out.backing().to_vec(), out.published_frames());
        let other_start = spec(mono(), 4, 101);
        let other_frames = spec(mono(), 3, 100);
        let other_layout = spec(ChannelLayout::mono(8), 4, 100);
        let short: [&[f32]; 1] = [&a[..3]];
        let nan = [SendRoute { level: f32::NAN, ..sends[0] }];
        let skew = [sends[0], SendRoute { input: InputBlock::new(&other_start, &cb), level: 1.0 }];
        let frames = [sends[0], SendRoute { input: InputBlock::new(&other_frames, &short), level: 1.0 }];
        let layout = [sends[0], SendRoute { input: InputBlock::new(&other_layout, &cb), level: 1.0 }];
        let mut no_actor = ctx(1);
        no_actor.actor = None;
        let held = engine.reserve().unwrap();
        let busy = engine.mix(&ctx(1), &sends, &mut out, &token);
        drop(held);
        let canceled = CancellationToken::default();
        canceled.cancel();
        let results = [
            ("nan", engine.mix(&ctx(1), &nan, &mut out, &token), ScoreError::NotFinite),
            ("start", engine.mix(&ctx(1), &skew, &mut out, &token), ScoreError::TimelineMismatch),
            ("frames", engine.mix(&ctx(1), &frames, &mut out, &token), ScoreError::TimelineMismatch),
            ("layout", engine.mix(&ctx(1), &layout, &mut out, &token), ScoreError::ChannelMismatch),
            ("empty", engine.mix(&ctx(1), &[], &mut out, &token), ScoreError::OutOfRange),
            ("actor", engine.mix(&no_actor, &sends, &mut out, &token), ScoreError::MissingContext),
            ("stale", engine.mix(&ctx(2), &sends, &mut out, &token), ScoreError::StaleRevision),
            ("busy", busy, ScoreError::Busy),
            ("cancel", engine.mix(&ctx(1), &sends, &mut out, &canceled), ScoreError::Canceled),
        ];
        for (name, got, expected) in results {
            assert_eq!(got.unwrap_err(), expected, "case {name}");
        }
        let mut tiny = OutputBlock::with_capacity(mono(), 3).unwrap();
        assert_eq!(engine.mix(&ctx(1), &sends, &mut tiny, &token).unwrap_err(), ScoreError::CapacityExceeded);
        assert_eq!((out.backing().to_vec(), out.published_frames()), before);
    });
}
