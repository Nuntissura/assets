use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use hsk_studio_score::ChannelLayout;
use hsk_studio_score_device::*;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .unwrap()
}
fn resource() -> DomainId {
    DomainId::parse(RESOURCE).unwrap()
}
fn grant(directions: &[Direction]) -> DeviceGrant {
    DeviceGrant::new(actor(), resource(), directions, None)
}
fn config(block_frames: u32, ring_blocks: u32) -> StreamConfig {
    StreamConfig {
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        block_frames,
        ring_blocks,
    }
}
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}
/// Block of `len` samples holding the ramp `start, start+1, ...` (exact in f32 below 2^24).
fn ramp(start: f32, len: usize) -> Vec<f32> {
    (0..len).map(|i| start + i as f32).collect()
}
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}
/// Pushes `count` ramp blocks, retrying on `Full`; returns blocks accepted (independent tally).
fn push_blocks(stream: &mut OutputStream, count: u64, next: &mut f32) -> u64 {
    let samples = stream.producer().block_samples();
    let mut accepted = 0;
    while accepted < count {
        let block = ramp(*next, samples);
        match stream.producer().try_push_block(&block) {
            Ok(()) => {
                accepted += 1;
                *next += samples as f32;
            }
            Err(PushError::Full) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => panic!("unexpected push error {e}"),
        }
    }
    accepted
}

struct FrameSink(Vec<DeliveryClass>);
impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(class);
        Ok(())
    }
}

#[test]
fn ring_fill_and_overflow_discontinuity() {
    bounded_case("ring_fill_and_overflow_discontinuity", || {
        // 2 channels x 8 frames per block, 4 blocks: capacity 64 samples = 256 bytes.
        let g = config(8, 4).validate(&Limits::default()).unwrap();
        assert_eq!((g.channels, g.block_samples, g.capacity_samples, g.ring_bytes), (2, 16, 64, 256));
        let counters = Arc::new(Counters::default());
        let (mut producer, mut consumer) = ring(&g, Arc::clone(&counters));

        let (mut next, mut produced) = (1.0_f32, Vec::new());
        for _ in 0..4 {
            let block = ramp(next, 16);
            producer.try_push_block(&block).unwrap();
            produced.extend_from_slice(&block);
            next += 16.0;
        }
        // capacity + 1: refused, nothing written, exactly one overrun and one discontinuity.
        let extra = vec![9999.0_f32; 16];
        assert_eq!(producer.try_push_block(&extra), Err(PushError::Full));
        let c = counters.snapshot();
        assert_eq!((c.overruns, c.discontinuities, c.dropped_frames, c.submitted_frames), (1, 1, 0, 32));
        // A wrong-length offer is a typed refusal that counts nothing.
        assert_eq!(producer.try_push_block(&extra[..15]), Err(PushError::WrongLength));
        assert_eq!(counters.snapshot(), c);

        // Drain two blocks in 4-frame chunks, then wrap the cursors with two more blocks.
        let (mut consumed, mut chunk) = (Vec::new(), [0.0_f32; 8]);
        for _ in 0..4 {
            assert_eq!(consumer.pop_into(&mut chunk), 8);
            consumed.extend_from_slice(&chunk);
        }
        for _ in 0..2 {
            let block = ramp(next, 16);
            producer.try_push_block(&block).unwrap();
            produced.extend_from_slice(&block);
            next += 16.0;
        }
        assert_eq!(producer.try_push_block(&extra), Err(PushError::Full));
        // Drain the rest through a 3-frame chunk; the last pop is short.
        loop {
            let n = consumer.pop_into(&mut chunk[..6]);
            if n == 0 {
                break;
            }
            consumed.extend_from_slice(&chunk[..n]);
        }
        // Order and content equal what the producer side independently recorded.
        assert_eq!(bits(&consumed), bits(&produced));
        assert_eq!(consumer.pop_into(&mut chunk), 0);
        let c = counters.snapshot();
        assert_eq!((c.submitted_frames, c.consumed_frames), (48, 48));
        assert_eq!((producer.accepted_frames(), consumer.popped_frames()), (48, 48));
        assert_eq!((c.overruns, c.discontinuities), (2, 2));

        // Capture direction: 40 offered frames into a 32-frame ring keep the first 32 in order
        // and record the 8 lost frames as one overrun plus one discontinuity.
        let capture_counters = Arc::new(Counters::default());
        let (mut cap_p, mut cap_c) = ring(&g, Arc::clone(&capture_counters));
        let input = ramp(1.0, 80);
        assert_eq!(capture_callback(&mut cap_p, &input), 32);
        let k = capture_counters.snapshot();
        assert_eq!(
            (k.callbacks, k.submitted_frames, k.dropped_frames, k.overruns, k.discontinuities),
            (1, 32, 8, 1, 1)
        );
        let mut kept = vec![0.0_f32; 64];
        assert_eq!(cap_c.pop_into(&mut kept), 64);
        assert_eq!(bits(&kept), bits(&input[..64]));
    });
}

#[test]
fn empty_ring_underrun_is_safe_silence() {
    bounded_case("empty_ring_underrun_is_safe_silence", || {
        let g = config(8, 4).validate(&Limits::default()).unwrap();
        let counters = Arc::new(Counters::default());
        let (mut producer, mut consumer) = ring(&g, Arc::clone(&counters));

        // Empty ring: only zeros are written, the shortfall is counted, nothing is consumed.
        let mut out = vec![7.0_f32; 16];
        render_callback(&mut consumer, &mut out);
        assert!(out.iter().all(|s| s.to_bits() == 0));
        let c = counters.snapshot();
        assert_eq!(
            (c.callbacks, c.underruns, c.underrun_frames, c.consumed_frames),
            (1, 1, 8, 0)
        );

        // Partial ring: the 8 available frames are copied exactly, then zeros.
        let block = ramp(1.0, 16);
        producer.try_push_block(&block).unwrap();
        let mut out = vec![7.0_f32; 24];
        render_callback(&mut consumer, &mut out);
        assert_eq!(bits(&out[..16]), bits(&block));
        assert!(out[16..].iter().all(|s| s.to_bits() == 0));
        let c = counters.snapshot();
        assert_eq!(
            (c.callbacks, c.underruns, c.underrun_frames, c.consumed_frames),
            (2, 2, 12, 8)
        );

        // Exact fit: no underrun is counted.
        producer.try_push_block(&block).unwrap();
        let mut out = vec![7.0_f32; 16];
        render_callback(&mut consumer, &mut out);
        assert_eq!(bits(&out), bits(&block));
        let c = counters.snapshot();
        assert_eq!(
            (c.callbacks, c.underruns, c.underrun_frames, c.consumed_frames),
            (3, 2, 12, 16)
        );
    });
}

#[test]
fn software_device_lifecycle_and_loss() {
    bounded_case("software_device_lifecycle_and_loss", || {
        let provider = SoftwareProvider::with_options(
            Limits::default(),
            SoftwareOptions {
                loopback_frames: 65_536,
            },
        );
        let both = grant(&[Direction::Output, Direction::Input]);
        let cancel = CancellationToken::default();
        let out_dev = DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap();
        let in_dev = DeviceId::parse(SOFTWARE_INPUT_DEVICE).unwrap();

        // A: clean output stream. 30 blocks of 64 frames stream through the clocked thread.
        let mut stream = provider
            .open_output(&out_dev, &config(64, 8), Some(&both), &cancel)
            .unwrap();
        assert_eq!(provider.admission().open_streams(), 1);
        let mut next = 1.0_f32;
        let accepted = push_blocks(&mut stream, 30, &mut next);
        wait_for("ring drain", || stream.counters().consumed_frames() == 30 * 64);
        stream.stop();
        wait_for("stop acknowledgement", || stream.stop_acknowledged());
        let report_a = stream.close();
        assert!(report_a.is_clean(), "{report_a:?}");
        assert!(report_a.thread_joined && report_a.permits_released && !report_a.device_lost);
        assert_eq!(
            (report_a.frames_submitted, report_a.frames_consumed, report_a.residual_frames),
            (accepted * 64, 30 * 64, 0)
        );
        assert_eq!((provider.admission().open_streams(), provider.admission().bytes_in_flight()), (0, 0));
        // The fake hardware received exactly the submitted ramp in order; every other sample is
        // underrun silence, which the counter accounts for frame by frame.
        let played: Vec<f32> = report_a.loopback_samples.iter().copied().filter(|s| *s != 0.0).collect();
        assert_eq!(bits(&played), bits(&ramp(1.0, 30 * 64 * 2)));
        let silent = (report_a.loopback_samples.len() - played.len()) as u64;
        assert_eq!(silent, report_a.underrun_frames * 2);
        assert_eq!(report_a.loopback_samples.len() as u64, report_a.callbacks * 128);
        assert_eq!(outcome_of(&Ok(report_a)), Outcome::Success);

        // B: device loss in the middle of a transfer: the ring is full, the loss is injected.
        let mut stream = provider
            .open_output(&out_dev, &config(64, 8), Some(&both), &cancel)
            .unwrap();
        let mut next = 1.0_f32;
        let accepted = push_blocks(&mut stream, 8, &mut next);
        assert!(stream.inject_device_loss());
        assert!(stream.inject_device_loss()); // idempotent: the loss is counted once
        assert!(stream.device_lost());
        assert_eq!(stream.counters().device_lost(), 1);
        let block = ramp(next, 128);
        assert_eq!(stream.producer().try_push_block(&block), Err(PushError::DeviceLost));
        assert_eq!(stream.counters().overruns(), 0);
        wait_for("stop acknowledgement after loss", || stream.stop_acknowledged());
        let report_b = stream.close();
        assert!(report_b.device_lost && report_b.thread_joined && report_b.permits_released);
        assert!(report_b.reconciles(), "{report_b:?}");
        assert_eq!(report_b.frames_submitted, accepted * 64);
        assert!(!report_b.is_clean());
        assert_eq!(outcome_of(&Ok(report_b)), Outcome::Failure(FailureCode::Loss));
        assert_eq!(provider.admission().open_streams(), 0);

        // C: input stream nobody drains: the first 4 blocks fit, every later block is dropped
        // whole as one overrun plus one discontinuity.
        let mut input = provider
            .open_input(&in_dev, &config(64, 4), Some(&both), &cancel)
            .unwrap();
        wait_for("capture overrun", || input.counters().overruns() > 0);
        input.stop();
        wait_for("input stop acknowledgement", || input.stop_acknowledged());
        let (mut captured, mut chunk) = (Vec::new(), [0.0_f32; 128]);
        loop {
            let n = input.consumer().pop_into(&mut chunk);
            if n == 0 {
                break;
            }
            captured.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(bits(&captured), bits(&ramp(1.0, 4 * 64 * 2)));
        let report_c = input.close();
        assert!(report_c.reconciles() && report_c.thread_joined && report_c.permits_released);
        assert_eq!((report_c.frames_submitted, report_c.frames_consumed, report_c.residual_frames), (256, 256, 0));
        assert!(report_c.overruns >= 1 && report_c.discontinuities == report_c.overruns);
        assert_eq!(report_c.dropped_frames, (report_c.callbacks - 4) * 64);
        assert_eq!(report_c.frames_submitted + report_c.dropped_frames, report_c.callbacks * 64);
        assert_eq!(outcome_of(&Ok(report_c)), Outcome::Failure(FailureCode::Loss));
        assert_eq!((provider.admission().open_streams(), provider.admission().bytes_in_flight()), (0, 0));
    });
}

#[test]
fn grant_and_config_rejections() {
    bounded_case("grant_and_config_rejections", || {
        let limits = Limits::default();
        let provider = SoftwareProvider::new(limits);
        let admission = provider.admission().clone();
        let cancel = CancellationToken::default();
        let out_dev = DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).unwrap();
        let in_dev = DeviceId::parse(SOFTWARE_INPUT_DEVICE).unwrap();
        let out_grant = grant(&[Direction::Output]);
        let good = config(64, 4);
        // Every refusal happens before any permit or ring exists.
        let refused = |result: Result<OutputStream, DeviceError>, expected: DeviceError| {
            assert_eq!(result.err(), Some(expected));
            assert_eq!((admission.open_streams(), admission.bytes_in_flight()), (0, 0));
        };

        // Grant: none, wrong direction, wrong device.
        refused(provider.open_output(&out_dev, &good, None, &cancel), DeviceError::Unauthorized);
        let input_only = grant(&[Direction::Input]);
        refused(provider.open_output(&out_dev, &good, Some(&input_only), &cancel), DeviceError::Unauthorized);
        let pinned = DeviceGrant::new(actor(), resource(), &[Direction::Output], Some(DeviceId::parse("other").unwrap()));
        refused(provider.open_output(&out_dev, &good, Some(&pinned), &cancel), DeviceError::Unauthorized);
        assert_eq!(provider.enumerate(None).err(), Some(DeviceError::Unauthorized));
        assert_eq!(provider.enumerate(Some(&out_grant)).unwrap().len(), 2);

        // Config bounds (each typed, none allocates a ring).
        let with = |f: &dyn Fn(&mut StreamConfig)| {
            let mut c = good.clone();
            f(&mut c);
            provider.open_output(&out_dev, &c, Some(&out_grant), &cancel)
        };
        refused(with(&|c| c.sample_rate_hz = 0), DeviceError::InvalidRate);
        refused(with(&|c| c.sample_rate_hz = 44_101), DeviceError::InvalidRate);
        refused(with(&|c| c.block_frames = 0), DeviceError::InvalidBlockFrames);
        refused(with(&|c| c.block_frames = limits.max_block_frames + 1), DeviceError::InvalidBlockFrames);
        refused(with(&|c| c.ring_blocks = 1), DeviceError::InvalidRingBlocks);
        refused(with(&|c| c.ring_blocks = limits.max_ring_blocks + 1), DeviceError::InvalidRingBlocks);
        refused(
            with(&|c| {
                c.block_frames = limits.max_block_frames;
                c.ring_blocks = limits.max_ring_blocks;
            }),
            DeviceError::RingTooLarge,
        );
        refused(
            with(&|c| c.layout = ChannelLayout::new(&[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap()),
            DeviceError::UnsupportedLayout,
        );
        // Device: unknown id, and the input device is not an output.
        refused(
            provider.open_output(&DeviceId::parse("no-such-device").unwrap(), &good, Some(&out_grant), &cancel),
            DeviceError::UnknownDevice,
        );
        let both = grant(&[Direction::Output, Direction::Input]);
        refused(provider.open_output(&in_dev, &good, Some(&both), &cancel), DeviceError::UnknownDevice);
        // Cancellation is observed before anything else.
        let canceled = CancellationToken::default();
        canceled.cancel();
        refused(provider.open_output(&out_dev, &good, Some(&out_grant), &canceled), DeviceError::Canceled);
        // Id and direct validation errors.
        assert_eq!(DeviceId::parse("").err(), Some(DeviceError::InvalidDeviceId));
        assert_eq!(good.validate(&Limits { max_ring_bytes: 100, ..limits }).err(), Some(DeviceError::RingTooLarge));

        // Bounded admission: one stream at a time, permit released after close.
        let tight = SoftwareProvider::new(Limits { max_open_streams: 1, ..limits });
        let first = tight.open_output(&out_dev, &good, Some(&out_grant), &cancel).unwrap();
        assert_eq!(
            tight.open_output(&out_dev, &good, Some(&out_grant), &cancel).err(),
            Some(DeviceError::AdmissionDenied)
        );
        let closed = first.close();
        assert!(closed.is_clean(), "{closed:?}");
        assert_eq!((tight.admission().open_streams(), tight.admission().bytes_in_flight()), (0, 0));
        let again = tight.open_output(&out_dev, &good, Some(&out_grant), &cancel).unwrap();
        let clean = again.close();
        assert!(clean.is_clean());

        // report(): one terminal frame per outcome, matching the outcome class.
        let mut lost = clean.clone();
        lost.device_lost = true;
        let cases: Vec<(Result<CloseReport, DeviceError>, Outcome)> = vec![
            (Ok(clean), Outcome::Success),
            (Err(DeviceError::Canceled), Outcome::Canceled),
            (Err(DeviceError::UnknownDevice), Outcome::Failure(FailureCode::Unavailable)),
            (Ok(lost), Outcome::Failure(FailureCode::Loss)),
            (Err(DeviceError::Unauthorized), Outcome::Failure(FailureCode::Validation)),
            (Err(DeviceError::UnsupportedLayout), Outcome::Failure(FailureCode::Unsupported)),
        ];
        for (index, (result, expected)) in cases.iter().enumerate() {
            assert_eq!(outcome_of(result), *expected, "case {index}");
            let mut observe = Observe::new(index as u64, 3, resource(), actor(), Budget::new(0, 0).unwrap());
            let mut sink = FrameSink(Vec::new());
            let receipt = report(result, index as u64, 3, &cancel, &mut observe, &mut sink).unwrap();
            assert_eq!(receipt.outcome, *expected, "case {index}");
            assert_eq!(sink.0, vec![DeliveryClass::Terminal], "case {index}");
        }
    });
}
