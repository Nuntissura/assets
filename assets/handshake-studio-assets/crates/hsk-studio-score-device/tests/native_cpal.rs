//! Native device test for feature `cpal-backend`. It touches the real default output device, so
//! it is `#[ignore]` and additionally requires `STUDIO_NATIVE_AUDIO=1`. It streams DIGITAL
//! SILENCE only (zero samples): nothing audible, no window, no focus change.
//!
//! Run: `STUDIO_NATIVE_AUDIO=1 cargo test -p hsk-studio-score-device --features cpal-backend
//! --test native_cpal -- --ignored`.
//!
//! Result semantics: when no usable device exists or the variable is unset the test PANICS with
//! a `NOT_PROVEN:` message, so it can never be read as a pass. A green run proves that the real
//! OS API opened an `f32` stream, took the pushed blocks through the ring, drained them with
//! the shared callback and closed quiescent with reconciling counters. It does NOT supply the
//! independent oracle of MT-35559 (an external loopback capture of the exact submitted blocks);
//! that stays NOT_PROVEN until such a device run exists.
#![cfg(feature = "cpal-backend")]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::ChannelLayout;
use hsk_studio_score_device::{
    CPAL_DEFAULT_OUTPUT, CpalProvider, DeviceGrant, DeviceId, DeviceProvider, Direction, Limits,
    PushError, StreamConfig,
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "native device: set STUDIO_NATIVE_AUDIO=1 and pass --ignored"]
fn native_output_streams_silence_and_reconciles() {
    assert!(
        std::env::var("STUDIO_NATIVE_AUDIO").as_deref() == Ok("1"),
        "NOT_PROVEN: STUDIO_NATIVE_AUDIO=1 is not set; no native device was touched"
    );
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .unwrap();
    let resource = DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap();
    let grant = DeviceGrant::new(actor, resource, &[Direction::Output], None);
    let provider = CpalProvider::new(Limits::default());

    let format = provider
        .default_output_format(Some(&grant))
        .unwrap_or_else(|e| panic!("NOT_PROVEN: no usable default output device ({e})"));
    assert!(format.is_f32, "NOT_PROVEN: default output format is not f32 ({format:?})");
    let labels: Vec<u16> = (100..100 + format.channels).collect();
    let config = StreamConfig {
        sample_rate_hz: format.sample_rate_hz,
        layout: ChannelLayout::new(&labels).unwrap(),
        block_frames: 512,
        ring_blocks: 8,
    };
    let device = DeviceId::parse(CPAL_DEFAULT_OUTPUT).unwrap();
    let mut stream = provider
        .open_output(&device, &config, Some(&grant), &CancellationToken::default())
        .unwrap_or_else(|e| panic!("NOT_PROVEN: native open refused ({e}) for {format:?}"));

    let silence = vec![0.0_f32; stream.producer().block_samples()];
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pushed = 0_u64;
    while pushed < 16 {
        assert!(Instant::now() < deadline, "timed out pushing silence");
        match stream.producer().try_push_block(&silence) {
            Ok(()) => pushed += 1,
            Err(PushError::Full) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => panic!("push failed: {e}"),
        }
    }
    while stream.counters().consumed_frames() < pushed * 512 {
        assert!(Instant::now() < deadline, "timed out waiting for the device to drain");
        std::thread::sleep(Duration::from_millis(5));
    }
    stream.stop();
    while !stream.stop_acknowledged() {
        assert!(Instant::now() < deadline, "timed out waiting for stop acknowledgement");
        std::thread::sleep(Duration::from_millis(2));
    }
    let report = stream.close();
    println!(
        "NATIVE_EVIDENCE format={format:?} callbacks={} submitted={} consumed={} residual={} underruns={} underrun_frames={} overruns={} device_lost={} thread_joined={} permits_released={}",
        report.callbacks,
        report.frames_submitted,
        report.frames_consumed,
        report.residual_frames,
        report.underruns,
        report.underrun_frames,
        report.overruns,
        report.device_lost,
        report.thread_joined,
        report.permits_released
    );
    assert!(report.thread_joined && report.permits_released && !report.device_lost);
    assert!(report.reconciles());
    assert_eq!((report.frames_submitted, report.frames_consumed), (pushed * 512, pushed * 512));
    assert!(report.callbacks > 0);
    assert_eq!(provider.admission().open_streams(), 0);
}
