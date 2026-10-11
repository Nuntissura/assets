//! Real consumer of the public Score Device API. `--software` streams a ramp through the pure
//! Rust clocked software device (no operating-system audio, nothing audible) and prints the close
//! report. `--cpal` is the optional native backend and exists only with feature `cpal-backend`.
//! Usage:
//! `score-device-consumer [--descriptor] [--software [--blocks N] [--block-frames F] [--inject-loss]] [--cpal]`
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, SinkPort};
use hsk_studio_score::ChannelLayout;
use hsk_studio_score_device::{
    CloseReport, DESCRIPTOR, DeviceError, DeviceGrant, DeviceId, DeviceProvider, Direction,
    Limits, PushError, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions, SoftwareProvider, StreamConfig,
    outcome_of, report,
};
use std::env;
use std::time::{Duration, Instant};

struct FrameSink(Vec<(DeliveryClass, usize)>);
impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.len()));
        Ok(())
    }
}

fn run_software(blocks: u32, block_frames: u32, inject_loss: bool) -> Result<(), String> {
    let resource = DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001")
        .map_err(|_| "invalid_resource")?;
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .map_err(|_| "invalid_context")?;
    let grant = DeviceGrant::new(actor.clone(), resource.clone(), &[Direction::Output], None);
    let provider = SoftwareProvider::with_options(
        Limits::default(),
        SoftwareOptions {
            loopback_frames: (blocks as usize + 64) * block_frames as usize,
        },
    );
    let config = StreamConfig {
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        block_frames,
        ring_blocks: 8,
    };
    let cancel = CancellationToken::default();
    let device = DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).map_err(|e| e.code())?;
    let mut stream = provider
        .open_output(&device, &config, Some(&grant), &cancel)
        .map_err(|e| e.code())?;

    let block_samples = block_frames as usize * config.layout.channels();
    let mut block = vec![0.0_f32; block_samples];
    let (mut next, mut pushed, mut full_waits) = (1_u32, 0_u32, 0_u64);
    let deadline = Instant::now() + Duration::from_secs(30);
    while pushed < blocks && Instant::now() < deadline {
        if inject_loss && pushed == blocks / 2 && !stream.device_lost() {
            stream.inject_device_loss();
        }
        for (i, sample) in block.iter_mut().enumerate() {
            *sample = (next + i as u32) as f32;
        }
        match stream.producer().try_push_block(&block) {
            Ok(()) => {
                pushed += 1;
                next += block_samples as u32;
            }
            Err(PushError::Full) => {
                full_waits += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(PushError::DeviceLost) => break,
            Err(PushError::WrongLength) => return Err("wrong_length".into()),
        }
    }
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    while !stream.device_lost()
        && stream.counters().consumed_frames() < stream.counters().submitted_frames()
        && Instant::now() < drain_deadline
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    stream.stop();
    let close = stream.close();
    let closed: Result<CloseReport, DeviceError> = Ok(close.clone());

    let mut observe = Observe::new(1, 1, resource, actor, Budget::new(0, 0).map_err(|e| e.code())?);
    let mut sink = FrameSink(Vec::new());
    let delivered = report(&closed, 1, 1, &cancel, &mut observe, &mut sink);
    eprintln!(
        "diagnostic_frames={} delivered={:?}",
        sink.0.len(),
        delivered.map(|r| r.outcome.wire()).map_err(|e| e.code())
    );
    println!(
        "proof_class=software_lifecycle_not_native outcome={:?} pushed_blocks={pushed} full_waits={full_waits} \
         clean={} reconciles={} permits_released={} thread_joined={} device_lost={} callbacks={} \
         frames_submitted={} frames_consumed={} residual_frames={} underruns={} underrun_frames={} \
         overruns={} discontinuities={} loopback_samples={}",
        outcome_of(&closed),
        close.is_clean(),
        close.reconciles(),
        close.permits_released,
        close.thread_joined,
        close.device_lost,
        close.callbacks,
        close.frames_submitted,
        close.frames_consumed,
        close.residual_frames,
        close.underruns,
        close.underrun_frames,
        close.overruns,
        close.discontinuities,
        close.loopback_samples.len(),
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut software, mut cpal, mut inject_loss) = (false, false, false);
    let (mut blocks, mut block_frames) = (64_u32, 256_u32);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--software" => software = true,
            "--cpal" => cpal = true,
            "--inject-loss" => inject_loss = true,
            flag @ ("--blocks" | "--block-frames") => {
                i += 1;
                let value = args.get(i).ok_or("missing_argument")?;
                let value: u32 = value.parse().map_err(|_| "bad_number")?;
                if flag == "--blocks" {
                    blocks = value;
                } else {
                    block_frames = value;
                }
            }
            _ => return Err("unknown_argument".into()),
        }
        i += 1;
    }
    if cpal {
        return Err("feature_cpal_backend_off".into());
    }
    if software {
        return run_software(blocks, block_frames, inject_loss);
    }
    Err("nothing_requested".into())
}

fn main() {
    if let Err(code) = run() {
        eprintln!("rejected:{code}; see --descriptor; nothing was opened");
        std::process::exit(2)
    }
}
