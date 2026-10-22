//! Real consumer of the public Score API: synthesizes the MT-35541 fixture (stereo, L repeating
//! [0, 0.25, -0.25, 0.5], R = -L), processes one block offline and prints the receipt.
//! No device, no window, no file I/O. Usage:
//! `score-consumer [--descriptor] [--frames N] [--capacity N] [--rate HZ] [--limiter] [--cancel]`
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Observe, SinkPort};
use hsk_studio_score::{
    BlockSpec, ChannelLayout, DESCRIPTOR, Engine, HardLimiter, HardLimiterParams, InputBlock,
    OutputBlock, Processor, RequestContext, Unity, report, sample_to_ticks,
};
use std::env;

struct FrameSink(Vec<(DeliveryClass, usize)>);
impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.len()));
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return Ok(());
    }
    let (mut frames, mut capacity, mut rate) = (256_usize, 256_usize, 48_000_u32);
    let (mut limiter, mut cancel_first) = (false, false);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limiter" => limiter = true,
            "--cancel" => cancel_first = true,
            flag @ ("--frames" | "--capacity" | "--rate") => {
                i += 1;
                let value = args.get(i).ok_or("missing_argument")?;
                match flag {
                    "--frames" => frames = value.parse().map_err(|_| "bad_number")?,
                    "--capacity" => capacity = value.parse().map_err(|_| "bad_number")?,
                    _ => rate = value.parse().map_err(|_| "bad_number")?,
                }
            }
            _ => return Err("unknown_argument".into()),
        }
        i += 1;
    }

    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let left: Vec<f32> = (0..frames).map(|n| pattern[n % 4]).collect();
    let right: Vec<f32> = left.iter().map(|s| -s).collect();
    let spec = BlockSpec {
        sample_rate_hz: rate,
        layout: ChannelLayout::stereo(),
        frames,
        start_sample: 0,
    };
    let channels: [&[f32]; 2] = [&left, &right];
    let input = InputBlock::new(&spec, &channels);

    let resource = DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001")
        .map_err(|_| "invalid_resource")?;
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .map_err(|_| "invalid_context")?;
    let engine = Engine::new(resource.clone(), 7);
    let ctx = RequestContext {
        resource: resource.clone(),
        actor: Some(actor.clone()),
        expected_revision: 7,
    };
    let mut out =
        OutputBlock::with_capacity(ChannelLayout::stereo(), capacity).map_err(|e| e.code())?;
    let mut unity = Unity;
    let mut hard;
    let processor: &mut dyn Processor = if limiter {
        hard = HardLimiter::new(rate, 2, HardLimiterParams::default()).map_err(|e| e.code())?;
        &mut hard
    } else {
        &mut unity
    };
    let cancel = CancellationToken::default();
    if cancel_first {
        cancel.cancel();
    }

    let result = engine.process(&ctx, &input, processor, &mut out, &cancel);
    let mut observe = Observe::new(1, 7, resource, actor, Budget::new(0, 0).map_err(|e| e.code())?);
    let mut sink = FrameSink(Vec::new());
    let delivered = report(&result, 1, 7, &cancel, &mut observe, &mut sink);
    eprintln!(
        "diagnostic_frames={} delivered={:?} max_frame_bytes={MAX_FRAME_BYTES}",
        sink.0.len(),
        delivered.map(|r| r.outcome.wire()).map_err(|e| e.code())
    );

    match result {
        Ok(receipt) => {
            let rms: Vec<f64> = (0..2)
                .map(|ch| {
                    let s = out.published_channel(ch).unwrap_or(&[]);
                    (s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / s.len().max(1) as f64)
                        .sqrt()
                })
                .collect();
            println!(
                "processed_frames={} latency_samples={} labels={:?} start_sample={} end_sample={} start_ticks={} peak={:?} rms={:?} published_frames={}",
                receipt.processed_frames,
                receipt.latency_samples,
                receipt.labels(),
                receipt.start_sample,
                receipt.end_sample,
                sample_to_ticks(receipt.start_sample, rate).map_err(|e| e.code())?.value(),
                receipt.peak(),
                rms,
                out.published_frames()
            );
            Ok(())
        }
        Err(e) => Err(e.code().into()),
    }
}

fn main() {
    if let Err(code) = run() {
        eprintln!("rejected:{code}; see --descriptor; nothing was published");
        std::process::exit(2)
    }
}
