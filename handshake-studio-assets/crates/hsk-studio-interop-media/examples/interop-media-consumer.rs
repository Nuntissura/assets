//! Interop-local real consumer: import a CMX3600 EDL into a neutral proposal (printed as JSON, or
//! re-exported as canonical EDL text with `--export`), or sniff a native Adobe project file.
//! A refusal prints a stable error code on stderr and exits 2. Nothing is committed or written.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_interop_media::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, SinkPort};
use std::{env, fs};

struct CollectSink(Vec<Vec<u8>>);
impl SinkPort for CollectSink {
    fn try_send(
        &mut self,
        _: DeliveryClass,
        bytes: &[u8],
    ) -> std::result::Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn loss_json(l: &LossNote) -> String {
    format!(
        "{{\"entity\":{},\"property\":\"{}\",\"disposition\":\"{}\",\"code\":\"{}\"}}",
        json_string(&l.entity),
        l.property,
        l.disposition.as_str(),
        l.code
    )
}

fn read_capped(path: &str, cap: usize) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|_| InteropError::new(ErrorKind::InvalidLine))?;
    if metadata.len() > cap as u64 {
        return Err(InteropError::new(ErrorKind::InputLimit));
    }
    fs::read(path).map_err(|_| InteropError::new(ErrorKind::InvalidLine))
}

fn proposal_json(p: &TimelineProposal) -> String {
    let clips: Vec<String> = p
        .clips
        .iter()
        .map(|c| {
            format!(
                "{{\"event\":{},\"reel\":{},\"name\":{},\"lane\":\"{}{}\",\"source\":[{},{}],\"record\":[{},{}],\"speed\":[{},{}],\"disposition\":\"{}\"}}",
                c.event,
                json_string(&c.reel),
                c.clip_name.as_deref().map_or("null".into(), json_string),
                if c.lane.kind == LaneKind::Video { "V" } else { "A" },
                c.lane.index + 1,
                c.source_in.value(),
                c.source_out.value(),
                c.record_in.value(),
                c.record_out.value(),
                c.speed.numerator,
                c.speed.denominator,
                c.disposition.as_str()
            )
        })
        .collect();
    let losses: Vec<String> = p.losses.iter().map(loss_json).collect();
    format!(
        "{{\"format\":\"cmx3600\",\"ticks_per_frame\":{},\"drop_frame\":{},\"source_len\":{},\"ignored_comment_lines\":{},\"clips\":[{}],\"losses\":[{}]}}",
        p.frame_rate.ticks_per_frame(),
        p.drop_frame,
        p.source_len,
        p.ignored_comment_lines,
        clips.join(","),
        losses.join(",")
    )
}

fn run(args: &[String], cancel: &CancellationToken) -> Result<String> {
    cancel.check()?;
    let limits = Limits::default();
    if let Some(path) = arg(args, "--sniff") {
        let blocked = import_native(&read_capped(path, limits.max_bytes)?);
        let losses: Vec<String> = blocked.losses.iter().map(loss_json).collect();
        return Ok(format!(
            "{{\"format\":\"{}\",\"evidence\":\"{}\",\"len\":{},\"editable_import\":\"blocked\",\"losses\":[{}]}}",
            blocked.format.as_str(),
            blocked.evidence,
            blocked.len,
            losses.join(",")
        ));
    }
    let path = arg(args, "--edl").ok_or(InteropError::new(ErrorKind::InvalidLine))?;
    let (rate, _) = parse_frame_rate(arg(args, "--rate").unwrap_or("25"))?;
    let mut request = ImportRequest::new(rate);
    request.assume_drop = args.iter().any(|a| a == "--drop");
    request.record_origin_frames = match arg(args, "--origin") {
        Some(v) => v
            .parse()
            .map_err(|_| InteropError::new(ErrorKind::InvalidTimecode))?,
        None => 0,
    };
    let proposal = import_cmx3600(
        &read_capped(path, limits.max_bytes)?,
        &request,
        &limits,
        cancel,
    )?;
    if args.iter().any(|a| a == "--export") {
        return export_cmx3600(&proposal);
    }
    Ok(proposal_json(&proposal))
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
    match (result, receipt) {
        (Ok(text), Ok(r)) => {
            println!("{text}");
            eprintln!("outcome_code={} frames={}", r.outcome.wire(), sink.0.len());
        }
        (Err(e), _) => {
            eprintln!(
                "rejected:{e}; consult --descriptor and correct only the caller input; no host proof"
            );
            std::process::exit(2)
        }
        (Ok(_), Err(e)) => {
            eprintln!("rejected:{}", e.code());
            std::process::exit(2)
        }
    }
}
