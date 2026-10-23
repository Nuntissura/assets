//! Pulse-local real consumer: frame/tick/timecode labels and a two-range ripple trim fixture.
//! Prints JSON on stdout; stable error code and recovery hint on stderr (exit 2).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Observe, SinkPort,
};
use hsk_studio_pulse::*;
use std::env;

struct CollectSink(Vec<Vec<u8>>);
impl SinkPort for CollectSink {
    fn try_send(
        &mut self,
        _class: DeliveryClass,
        bytes: &[u8],
    ) -> std::result::Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn arg_u64(args: &[String], name: &str, default: u64) -> Result<u64> {
    match args.iter().position(|a| a == name) {
        None => Ok(default),
        Some(i) => args
            .get(i + 1)
            .and_then(|v| v.parse().ok())
            .ok_or(PulseError::InvalidInput),
    }
}

fn range_json(r: TickRange) -> String {
    format!("[{},{}]", r.start().value(), r.end().value())
}

fn run(args: &[String], cancel: &CancellationToken) -> Result<String> {
    cancel.check()?;
    let flag = |name: &str| args.iter().any(|a| a == name);
    let (rate, norm) = timebase_from_import(arg_u64(args, "--rate", 10_160_640_000)?)?;
    let tpf = rate.ticks_per_frame();
    let frame = arg_u64(args, "--frame", 1800)?;
    let label = ticks_to_timecode(frames_to_ticks(frame, rate)?, rate, flag("--drop"))?;
    let (a, b, to) = (
        arg_u64(args, "--a-frames", 3)?,
        arg_u64(args, "--b-frames", 3)?,
        arg_u64(args, "--trim-to-frames", 2)?,
    );
    let a_range = TickRange::from_start_duration(Ticks::new(0), frames_to_ticks(a, rate)?)?;
    let b_range = TickRange::from_start_duration(a_range.end(), frames_to_ticks(b, rate)?)?;
    let lane = [a_range, b_range];
    let new_duration = frames_to_ticks(to, rate)?;
    let after = if flag("--ripple") {
        ripple_trim_end(&lane, 0, new_duration, Grid::Frames(rate), cancel)?
    } else {
        trim_end(&lane, 0, new_duration, Grid::Frames(rate), cancel)?
    };
    let list = |l: &[TickRange]| {
        l.iter()
            .map(|r| range_json(*r))
            .collect::<Vec<_>>()
            .join(",")
    };
    let (fps_num, fps_den) = fps_fraction(rate);
    Ok(format!(
        "{{\"ticks_per_frame\":{tpf},\"normalization\":{{\"source\":{},\"canonical\":{},\"changed\":{}}},\"fps\":[{fps_num},{fps_den}],\"frame\":{frame},\"timecode\":\"{label}\",\"before\":[{}],\"after\":[{}]}}",
        norm.source_ticks_per_frame,
        norm.canonical_ticks_per_frame,
        norm.changed,
        list(&lane),
        list(&after)
    ))
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return;
    }
    let cancel = CancellationToken::default();
    if args.iter().any(|a| a == "--cancel") {
        cancel.cancel();
    }
    let result = run(&args, &cancel);
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .expect("static context");
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").expect("static id");
    let mut observe = Observe::new(1, 1, resource, actor, Budget::new(0, 0).expect("budget"));
    let mut sink = CollectSink(Vec::new());
    let receipt = report(&result, 1, 1, &cancel, &mut observe, &mut sink);
    match (&result, receipt) {
        (Ok(json), Ok(r)) => {
            println!("{json}");
            eprintln!(
                "outcome_code={} frames={} frame_bytes_max={MAX_FRAME_BYTES}",
                r.outcome.wire(),
                sink.0.len()
            );
        }
        (Err(e), _) => {
            eprintln!(
                "rejected:{}; consult --descriptor and correct only the caller input; no host proof",
                e.code()
            );
            std::process::exit(2)
        }
        (Ok(_), Err(e)) => {
            eprintln!("rejected:{}", e.code());
            std::process::exit(2)
        }
    }
}
