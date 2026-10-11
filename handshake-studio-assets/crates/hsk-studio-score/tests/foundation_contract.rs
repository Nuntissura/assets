use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, Error as ObserveError, FailureCode, MAX_FRAME_BYTES,
    Observe, Outcome, SinkPort,
};
use hsk_studio_score::*;
use std::sync::mpsc;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
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
const OTHER_RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000002";

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .unwrap()
}
fn ctx(revision: u64) -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(actor()),
        expected_revision: revision,
    }
}
fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 7)
}
/// MT-35541 oracle fixture: L repeating [0, 0.25, -0.25, 0.5], R = -L.
fn fixture(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let left: Vec<f32> = (0..frames).map(|n| pattern[n % 4]).collect();
    let right = left.iter().map(|s| -s).collect();
    (left, right)
}
fn spec(frames: usize, start_sample: u64, rate: u32) -> BlockSpec {
    BlockSpec {
        sample_rate_hz: rate,
        layout: ChannelLayout::stereo(),
        frames,
        start_sample,
    }
}
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

/// Processor test double that copies input, then runs a side effect.
struct Copier<'a> {
    after_copy: Box<dyn FnMut() + 'a>,
    resets: u32,
}
impl Processor for Copier<'_> {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, i: &ChannelLayout, o: &ChannelLayout) -> bool {
        i == o
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        _cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        for (src, dst) in input.channels().iter().zip(out.channels_mut()) {
            dst[..src.len()].copy_from_slice(src);
        }
        (self.after_copy)();
        Ok(())
    }
    fn reset(&mut self) {
        self.resets += 1;
    }
}

#[test]
fn unity_block_is_bit_exact() {
    bounded_case("unity_block_is_bit_exact", || {
        let (left, right) = fixture(256);
        let spec = spec(256, 0, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&spec, &channels);
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        let receipt = engine()
            .process(&ctx(7), &input, &mut Unity, &mut out, &CancellationToken::default())
            .unwrap();
        assert_eq!(bits(out.published_channel(0).unwrap()), bits(&left));
        assert_eq!(bits(out.published_channel(1).unwrap()), bits(&right));
        assert_eq!(
            (receipt.processed_frames, receipt.latency_samples, receipt.start_sample, receipt.end_sample),
            (256, 0, 0, 256)
        );
        assert_eq!(receipt.labels(), &[100, 101]);
        assert_eq!(receipt.peak(), &[0.5, 0.5]);
        assert_eq!((out.published_frames(), out.published_start_sample()), (256, Some(0)));
        // Independent tick arithmetic: 254_016_000_000 / 48_000 = 5_292_000 ticks per sample.
        assert_eq!(receipt.start_ticks().unwrap().value(), 0);
        assert_eq!(receipt.end_ticks().unwrap().value(), 256 * 5_292_000);
        assert_eq!(ticks_per_sample(44_100).unwrap(), 5_760_000);
        assert_eq!(ticks_per_sample(32_000).unwrap(), 7_938_000);
        assert_eq!(ticks_per_sample(47_999), Err(ScoreError::InvalidRate));
        assert_eq!(ChannelLayout::new(&[100, 100]), Err(ScoreError::InvalidLayout));
    });
}

#[test]
fn admission_rejects_before_write() {
    bounded_case("admission_rejects_before_write", || {
        let engine = engine();
        let token = CancellationToken::default();
        let (left, right) = fixture(256);
        let good = spec(256, 0, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];

        // Capacity 255 rejects 256 frames; the zeroed block is untouched byte-for-byte.
        let mut small = OutputBlock::with_capacity(ChannelLayout::stereo(), 255).unwrap();
        let before = bits(small.backing());
        let r = engine.process(&ctx(7), &InputBlock::new(&good, &channels), &mut Unity, &mut small, &token);
        assert_eq!(r.unwrap_err(), ScoreError::CapacityExceeded);
        assert_eq!((bits(small.backing()), small.published_frames()), (before, 0));

        // A previously published block must also survive every later refusal untouched.
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        engine
            .process(&ctx(7), &InputBlock::new(&good, &channels), &mut Unity, &mut out, &token)
            .unwrap();
        let (before, published) = (bits(out.backing()), (out.published_frames(), out.published_start_sample()));

        let zero_rate = spec(256, 0, 0);
        let overflow = spec(256, u64::MAX - 10, 48_000);
        let short_right = &right[..255];
        let short: [&[f32]; 2] = [&left, short_right];
        let mono_only: [&[f32]; 1] = [&left];
        let mut nan_left = left.clone();
        nan_left[3] = f32::NAN;
        let nan: [&[f32]; 2] = [&nan_left, &right];
        let mut no_actor = ctx(7);
        no_actor.actor = None;
        let mut wrong_resource = ctx(7);
        wrong_resource.resource = DomainId::parse(OTHER_RESOURCE).unwrap();

        let mut pan = Pan::new(0.0).unwrap();
        let cases: Vec<(&str, ScoreError, Result<BlockReceipt, ScoreError>)> = vec![
            ("rate0", ScoreError::InvalidRate, engine.process(&ctx(7), &InputBlock::new(&zero_rate, &channels), &mut Unity, &mut out, &token)),
            ("short_channel", ScoreError::LengthMismatch, engine.process(&ctx(7), &InputBlock::new(&good, &short), &mut Unity, &mut out, &token)),
            ("channel_count", ScoreError::ChannelMismatch, engine.process(&ctx(7), &InputBlock::new(&good, &mono_only), &mut Unity, &mut out, &token)),
            ("overflow", ScoreError::PositionOverflow, engine.process(&ctx(7), &InputBlock::new(&overflow, &channels), &mut Unity, &mut out, &token)),
            ("non_finite", ScoreError::NotFinite, engine.process(&ctx(7), &InputBlock::new(&good, &nan), &mut Unity, &mut out, &token)),
            ("missing_actor", ScoreError::MissingContext, engine.process(&no_actor, &InputBlock::new(&good, &channels), &mut Unity, &mut out, &token)),
            ("stale", ScoreError::StaleRevision, engine.process(&ctx(6), &InputBlock::new(&good, &channels), &mut Unity, &mut out, &token)),
            ("resource", ScoreError::ResourceMismatch, engine.process(&wrong_resource, &InputBlock::new(&good, &channels), &mut Unity, &mut out, &token)),
            ("layout", ScoreError::ChannelMismatch, engine.process(&ctx(7), &InputBlock::new(&good, &channels), &mut pan, &mut out, &token)),
        ];
        for (name, expected, got) in cases {
            assert_eq!(got.unwrap_err(), expected, "case {name}");
        }
        assert_eq!(bits(out.backing()), before);
        assert_eq!((out.published_frames(), out.published_start_sample()), published);
    });
}

#[test]
fn busy_and_cancel() {
    bounded_case("busy_and_cancel", || {
        let engine = engine();
        let token = CancellationToken::default();
        let (left, right) = fixture(256);
        let sp = spec(256, 0, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();

        // A held reservation makes process return Busy at once; releasing it restores service.
        let held = engine.reserve().unwrap();
        assert_eq!(engine.reserve().unwrap_err(), ScoreError::Busy);
        assert_eq!(
            engine.process(&ctx(7), &input, &mut Unity, &mut out, &token).unwrap_err(),
            ScoreError::Busy
        );
        assert_eq!(out.published_frames(), 0);
        drop(held);

        // A genuinely concurrent second request is refused while the first block is in flight.
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            let (engine_ref, input_ref, token_ref) = (&engine, &input, &token);
            let worker = scope.spawn(move || {
                let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
                let mut gate = Copier {
                    after_copy: Box::new(|| {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }),
                    resets: 0,
                };
                engine_ref
                    .process(&ctx(7), input_ref, &mut gate, &mut out, token_ref)
                    .map(|r| r.processed_frames)
            });
            started_rx.recv().unwrap();
            let second = engine.process(&ctx(7), &input, &mut Unity, &mut out, &token);
            assert_eq!(second.unwrap_err(), ScoreError::Busy);
            release_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), 256);
        });

        // Cancel before admission: nothing is written or published.
        let canceled = CancellationToken::default();
        canceled.cancel();
        let zeros = bits(out.backing());
        assert_eq!(
            engine.process(&ctx(7), &input, &mut Unity, &mut out, &canceled).unwrap_err(),
            ScoreError::Canceled
        );
        assert_eq!((bits(out.backing()), out.published_frames()), (zeros, 0));

        // Cancel after begin: the processor wrote the block, then cancel arrives; it is discarded
        // unpublished and the processor history is reset.
        let mid = CancellationToken::default();
        let mut copier = Copier { after_copy: Box::new(|| mid.cancel()), resets: 0 };
        assert_eq!(
            engine.process(&ctx(7), &input, &mut copier, &mut out, &mid).unwrap_err(),
            ScoreError::Canceled
        );
        assert_eq!((out.published_frames(), copier.resets), (0, 1));

        // Revision advanced mid-block: stale blocks cannot publish.
        let mut bump = Copier { after_copy: Box::new(|| engine.set_revision(8)), resets: 0 };
        assert_eq!(
            engine.process(&ctx(7), &input, &mut bump, &mut out, &token).unwrap_err(),
            ScoreError::StaleRevision
        );
        assert_eq!((out.published_frames(), bump.resets), (0, 1));
        assert_eq!(engine.revision(), 8);
    });
}

#[test]
fn hard_limiter_spec_ranges() {
    bounded_case("hard_limiter_spec_ranges", || {
        assert_eq!(normalised_to_value(0.5, -100.0, 0.0), -50.0);
        let defaults = HardLimiterParams::from_normalised([0.5, 0.8, 0.14, 0.375]).unwrap();
        let d = HardLimiterParams::default();
        assert!((defaults.max_amp_db - d.max_amp_db).abs() < 1e-9);
        assert!((defaults.input_boost_db - d.input_boost_db).abs() < 1e-9);
        assert!((defaults.lookahead_ms - d.lookahead_ms).abs() < 1e-9);
        assert!((defaults.release_ms - d.release_ms).abs() < 1e-9);
        for bad in [
            HardLimiterParams { max_amp_db: 0.1, ..d },
            HardLimiterParams { max_amp_db: -100.1, ..d },
            HardLimiterParams { input_boost_db: 50.1, ..d },
            HardLimiterParams { lookahead_ms: 4.9, ..d },
            HardLimiterParams { lookahead_ms: 20.1, ..d },
            HardLimiterParams { release_ms: 39.9, ..d },
            HardLimiterParams { release_ms: 200.1, ..d },
        ] {
            assert_eq!(HardLimiter::new(48_000, 2, bad).unwrap_err(), ScoreError::OutOfRange);
        }
        let nan = HardLimiterParams { release_ms: f64::NAN, ..d };
        assert_eq!(HardLimiter::new(48_000, 2, nan).unwrap_err(), ScoreError::NotFinite);
        assert_eq!(HardLimiter::new(0, 2, d).unwrap_err(), ScoreError::InvalidRate);
        assert_eq!(HardLimiter::new(48_000, 0, d).unwrap_err(), ScoreError::InvalidLayout);

        // 7.1 ms at 48 kHz = round(340.8) = 341 samples, reported through the engine receipt.
        let engine = engine();
        let token = CancellationToken::default();
        assert_eq!(HardLimiter::new(48_000, 2, d).unwrap().latency_samples(), 341);

        // Ceiling -6 dB, 5 ms lookahead: full-scale onset after silence stays at or below it.
        let params = HardLimiterParams { max_amp_db: -6.0, input_boost_db: 0.0, lookahead_ms: 5.0, release_ms: 40.0 };
        let mut limiter = HardLimiter::new(48_000, 2, params).unwrap();
        let ceiling = 10.0_f32.powf(-6.0 / 20.0);
        assert!((limiter.ceiling() - ceiling).abs() < 1e-7);
        let frames = 4800;
        let left: Vec<f32> = (0..frames)
            .map(|n| if n < 300 { 0.0 } else { (2.0 * std::f64::consts::PI * 440.0 * n as f64 / 48_000.0).sin() as f32 })
            .collect();
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        let mut max_out = 0.0_f32;
        let mut emitted = 0;
        for (start, len) in [(0_usize, 2000_usize), (2000, 2800)] {
            let sp = spec(len, start as u64, 48_000);
            let channels: [&[f32]; 2] = [&left[start..start + len], &right[start..start + len]];
            let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), len).unwrap();
            let receipt = engine
                .process(&ctx(7), &InputBlock::new(&sp, &channels), &mut limiter, &mut out, &token)
                .unwrap();
            assert_eq!(receipt.latency_samples, 240);
            for ch in 0..2 {
                let samples = out.published_channel(ch).unwrap();
                max_out = samples.iter().fold(max_out, |m, s| m.max(s.abs()));
            }
            emitted += out.published_frames();
        }
        assert_eq!(emitted, frames);
        assert!(max_out <= ceiling + 1e-6, "max {max_out} ceiling {ceiling}");
        assert!(max_out >= ceiling * 0.9, "limiter must actually pass signal up to the ceiling: {max_out}");
        assert_eq!(limiter.overshoot_events(), 0, "gain smoothing alone must bound the output");

        // Below the ceiling the limiter is a bit-exact pure delay of `latency` samples.
        let mut quiet = HardLimiter::new(48_000, 2, HardLimiterParams { max_amp_db: 0.0, input_boost_db: 0.0, ..params }).unwrap();
        let (ql, qr) = fixture(1000);
        let sp = spec(1000, 0, 48_000);
        let channels: [&[f32]; 2] = [&ql, &qr];
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 1000).unwrap();
        engine.process(&ctx(7), &InputBlock::new(&sp, &channels), &mut quiet, &mut out, &token).unwrap();
        let got = out.published_channel(0).unwrap();
        assert!(got[..240].iter().all(|s| *s == 0.0));
        assert_eq!(bits(&got[240..]), bits(&ql[..760]));
    });
}

struct VecSink(Vec<DeliveryClass>);
impl SinkPort for VecSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        self.0.push(class);
        Ok(())
    }
}

#[test]
fn report_maps_outcomes() {
    bounded_case("report_maps_outcomes", || {
        let (left, right) = fixture(256);
        let sp = spec(256, 0, 48_000);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);
        let engine = engine();
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        let token = CancellationToken::default();
        let ok = engine.process(&ctx(7), &input, &mut Unity, &mut out, &token);
        let canceled_token = CancellationToken::default();
        canceled_token.cancel();
        let canceled = engine.process(&ctx(7), &input, &mut Unity, &mut out, &canceled_token);
        let invalid = engine.process(&ctx(6), &input, &mut Unity, &mut out, &token);
        let busy: Result<BlockReceipt, ScoreError> = Err(ScoreError::Busy);

        let new_observe = || {
            Observe::new(
                9,
                7,
                DomainId::parse(RESOURCE).unwrap(),
                actor(),
                Budget::new(0, 0).unwrap(),
            )
        };
        let mut expectations = vec![
            (&ok, token.clone(), Outcome::Success),
            (&canceled, canceled_token.clone(), Outcome::Canceled),
            (&invalid, token.clone(), Outcome::Failure(FailureCode::Validation)),
            (&busy, token.clone(), Outcome::Failure(FailureCode::Unavailable)),
        ];
        for (result, cancel, outcome) in expectations.drain(..) {
            let (mut observe, mut sink) = (new_observe(), VecSink(Vec::new()));
            let receipt = report(result, 9, 7, &cancel, &mut observe, &mut sink).unwrap();
            assert_eq!(receipt.outcome, outcome);
            assert_eq!(outcome_of(result), outcome);
            assert_eq!(sink.0, vec![DeliveryClass::Terminal], "exactly one terminal frame");
            // The terminal is final: a second report on the same emitter is refused, not duplicated.
            let again = report(result, 9, 7, &cancel, &mut observe, &mut sink);
            assert_eq!(again.unwrap_err(), ObserveError::Closed);
            assert_eq!(sink.0.len(), 1);
        }
    });
}
