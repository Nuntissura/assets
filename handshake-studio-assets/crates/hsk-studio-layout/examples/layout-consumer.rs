//! Layout-local real consumer with a fixed-advance toy font (not shaping): flows `--text` through the
//! `--frame WxH` chain and prints the placement proposal as JSON on stdout; the stable error code and
//! recovery hint go to stderr (exit 2).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_layout::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Observe, SinkPort,
};
use std::env;

struct CollectSink(Vec<Vec<u8>>);
impl SinkPort for CollectSink {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn run(args: &[String], cancel: &CancellationToken) -> Result<String, LayoutError> {
    let advance: f64 = arg(args, "--advance")
        .map_or(Ok(6.0), str::parse)
        .map_err(|_| LayoutError::InvalidInput)?;
    let text = arg(args, "--text").unwrap_or("aaaa bbbb cccc dddd");
    let composer = match arg(args, "--composer").unwrap_or("first-fit") {
        "first-fit" => Composer::FirstFit,
        "knuth-plass" => Composer::KnuthPlass(KnuthPlassParams::default()),
        _ => return Err(LayoutError::InvalidInput),
    };
    let mut sizes = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a == "--frame" {
            let (w, h) = args
                .get(i + 1)
                .and_then(|v| v.split_once('x'))
                .ok_or(LayoutError::InvalidInput)?;
            sizes.push((
                w.parse::<f64>().map_err(|_| LayoutError::InvalidInput)?,
                h.parse::<f64>().map_err(|_| LayoutError::InvalidInput)?,
            ));
        }
    }
    if sizes.is_empty() {
        sizes = vec![(60.0, 24.0), (60.0, 24.0)];
    }
    let spreads = vec![Spread {
        id: "spread-1".into(),
        pages: vec![Page {
            id: "page-1".into(),
            width_pt: 1000.0,
            height_pt: 1000.0,
            margins: Margins {
                top_pt: 0.0,
                right_pt: 0.0,
                bottom_pt: 0.0,
                left_pt: 0.0,
            },
        }],
    }];
    let frames: Vec<TextFrame> = sizes
        .iter()
        .enumerate()
        .map(|(i, (w, h))| TextFrame {
            id: format!("frame-{}", i + 1),
            spread: 0,
            page: 0,
            x_pt: 0.0,
            y_pt: 0.0,
            width_pt: *w,
            height_pt: *h,
            inset_pt: 0.0,
            first_baseline_pt: 10.0,
            obstacles: Vec::new(),
        })
        .collect();
    let source = SourceSpan {
        start: 0,
        end: text.len() as u64,
    };
    let paragraphs = vec![
        ShapedParagraph::from_fixed_advance(text, source, advance, 12.0, 10.0)
            .ok_or(LayoutError::InvalidInput)?,
    ];
    let thread: Vec<usize> = (0..frames.len()).collect();
    let proposal = reflow(
        &ReflowRequest {
            text,
            paragraphs: &paragraphs,
            spreads: &spreads,
            frames: &frames,
            thread: &thread,
            composer,
            space: SpaceParams::default(),
            expected_revision: 1,
            current_revision: 1,
            limits: Limits::default(),
        },
        cancel,
    )?;
    let frames_json: Vec<String> = proposal
        .frames
        .iter()
        .map(|f| {
            let lines: Vec<String> = f
                .lines
                .iter()
                .map(|l| {
                    format!(
                        "{{\"baseline_y_pt\":{},\"x_start_pt\":{},\"width_pt\":{},\"available_pt\":{},\"hang_pt\":{},\"source\":[{},{}],\"glyphs\":[{},{}],\"overfull\":{},\"ratio\":{},\"text\":{}}}",
                        l.baseline_y_pt,
                        l.x_start_pt,
                        l.width_pt,
                        l.available_pt,
                        l.hang_pt,
                        l.source.start,
                        l.source.end,
                        l.glyph_range.start,
                        l.glyph_range.end,
                        l.overfull,
                        l.ratio.map_or("null".to_string(), |r| r.to_string()),
                        json_string(&text[l.source.start as usize..l.source.end as usize]),
                    )
                })
                .collect();
            format!(
                "{{\"frame_id\":{},\"origin\":[{},{}],\"lines\":[{}]}}",
                json_string(&f.frame_id),
                f.origin_x_pt,
                f.origin_y_pt,
                lines.join(",")
            )
        })
        .collect();
    let overset = proposal
        .overset
        .map_or("null".to_string(), |o| format!("[{},{}]", o.start, o.end));
    Ok(format!(
        "{{\"frames\":[{}],\"overset\":{overset}}}",
        frames_json.join(",")
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
