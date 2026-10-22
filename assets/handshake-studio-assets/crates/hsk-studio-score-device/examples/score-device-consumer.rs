//! Real consumer of the public Score Device API. `--software` streams a ramp through the pure
//! Rust clocked software device (no operating-system audio, nothing audible) and prints the close
//! report. `--cpal` is the optional native backend (feature `cpal-backend`): it opens the default
//! output device and streams digital silence only; without a usable device it reports NOT_PROVEN
//! (exit 3), never a pass.
//! Usage:
//! `score-device-consumer [--descriptor] [--software [--blocks N] [--block-frames F] [--inject-loss]] [--cpal [--blocks N]]`
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, SinkPort};
use hsk_studio_score::ChannelLayout;
use hsk_studio_score_device::{
    CloseReport, DESCRIPTOR, DeviceError, DeviceGrant, DeviceId, DeviceProvider, Direction,
    Limits, OutputStream, PushError, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions, SoftwareProvider,
    StreamConfig, outcome_of, report,
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

fn resource() -> Result<DomainId, String> {
    DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001")
        .map_err(|_| "invalid_resource".into())
}
fn actor() -> Result<ActorContext, String> {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .map_err(|_| "invalid_context".into())
}

/// Pushes `blocks` blocks (the ramp 1.0, 2.0, ... or digital silence), optionally injecting a
/// simulated loss halfway, waits for the device to drain, then closes and reports.
fn drive(
    mut stream: OutputStream,
    blocks: u32,
    silence: bool,
    inject_loss: bool,
) -> Result<(), String> {
    let block_samples = stream.producer().block_samples();
    let mut block = vec![0.0_f32; block_samples];
    let (mut next, mut pushed, mut full_waits) = (1_u32, 0_u32, 0_u64);
    let deadline = Instant::now() + Duration::from_secs(30);
    while pushed < blocks && Instant::now() < deadline {
        if inject_loss && pushed == blocks / 2 && !stream.device_lost() {
            stream.inject_device_loss();
        }
        if !silence {
            for (i, sample) in block.iter_mut().enumerate() {
                *sample = (next + i as u32) as f32;
            }
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

    let mut observe = Observe::new(1, 1, resource()?, actor()?, Budget::new(0, 0).map_err(|e| e.code())?);
    let mut sink = FrameSink(Vec::new());
    let delivered = report(&closed, 1, 1, &CancellationToken::default(), &mut observe, &mut sink);
    eprintln!(
        "diagnostic_frames={} delivered={:?}",
        sink.0.len(),
        delivered.map(|r| r.outcome.wire()).map_err(|e| e.code())
    );
    println!(
        "proof_class={} outcome={:?} pushed_blocks={pushed} full_waits={full_waits} \
         clean={} reconciles={} permits_released={} thread_joined={} device_lost={} callbacks={} \
         frames_submitted={} frames_consumed={} residual_frames={} underruns={} underrun_frames={} \
         overruns={} discontinuities={} refused_offers={} loopback_samples={}",
        if silence { "native_smoke_not_loopback_oracle" } else { "software_lifecycle_not_native" },
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
        close.refused_offers,
        close.loopback_samples.len(),
    );
    Ok(())
}

fn run_software(blocks: u32, block_frames: u32, inject_loss: bool) -> Result<(), String> {
    let grant = DeviceGrant::new(actor()?, resource()?, &[Direction::Output], None);
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
    let device = DeviceId::parse(SOFTWARE_OUTPUT_DEVICE).map_err(|e| e.code())?;
    let stream = provider
        .open_output(&device, &config, Some(&grant), &CancellationToken::default())
        .map_err(|e| e.code())?;
    drive(stream, blocks, false, inject_loss)
}

#[cfg(feature = "cpal-backend")]
fn run_cpal(blocks: u32) -> Result<(), String> {
    use hsk_studio_score_device::{CPAL_DEFAULT_OUTPUT, CpalProvider};
    let grant = DeviceGrant::new(actor()?, resource()?, &[Direction::Output], None);
    let provider = CpalProvider::new(Limits::default());
    let not_proven = |why: String| format!("NOT_PROVEN:{why}");
    let format = provider
        .default_output_format(Some(&grant))
        .map_err(|e| not_proven(format!("no_usable_default_output_device:{}", e.code())))?;
    if !format.is_f32 {
        return Err(not_proven("default_output_format_not_f32".into()));
    }
    let labels: Vec<u16> = (100..100 + format.channels).collect();
    let config = StreamConfig {
        sample_rate_hz: format.sample_rate_hz,
        layout: ChannelLayout::new(&labels).map_err(|_| "invalid_layout")?,
        block_frames: 512,
        ring_blocks: 8,
    };
    let device = DeviceId::parse(CPAL_DEFAULT_OUTPUT).map_err(|e| e.code())?;
    let stream = provider
        .open_output(&device, &config, Some(&grant), &CancellationToken::default())
        .map_err(|e| not_proven(format!("native_open_refused:{}", e.code())))?;
    drive(stream, blocks, true, false)
}

#[cfg(not(feature = "cpal-backend"))]
fn run_cpal(_blocks: u32) -> Result<(), String> {
    Err("feature_cpal_backend_off".into())
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
        return run_cpal(blocks);
    }
    if software {
        return run_software(blocks, block_frames, inject_loss);
    }
    Err("nothing_requested".into())
}

fn main() {
    if let Err(code) = run() {
        if code.starts_with("NOT_PROVEN") {
            eprintln!("{code}; no native device proof was produced");
            std::process::exit(3)
        }
        eprintln!("rejected:{code}; see --descriptor; nothing was opened");
        std::process::exit(2)
    }
}
