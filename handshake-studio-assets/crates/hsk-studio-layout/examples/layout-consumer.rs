//! Layout-local real consumer with a fixed-advance toy font (not shaping): builds a Folio document
//! (one page, a chain of text frames, one story) from `--text` and the `--frame WxH` chain, flows it
//! and prints the placement proposal as JSON on stdout; the stable error code and recovery hint go
//! to stderr (exit 2).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema,
    Length, LayoutEdges, NodeKind, NodeRef, PageSide, PageSpreadSchema, Parent, Payload,
    ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind,
    StudioDocument, StudioLayer, StudioLayerGraph, StudioPage, StudioPageSpread, StudioTextFrame,
    StudioTextStory, TextStorySchema, VerticalJustification,
};
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

fn id(prefix: &str, n: usize) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn pt(value: f64) -> Length {
    Length {
        value,
        unit: GeometryUnit::Pt,
    }
}
fn edges(value: f64) -> LayoutEdges {
    LayoutEdges {
        top: pt(value),
        right: pt(value),
        bottom: pt(value),
        left: pt(value),
    }
}
/// One 1000 x 1000 pt page and a chain of text frames (`WxH` each, at the page origin, first
/// baseline 10 pt below the frame top, no inset) linked in order, all showing the one story.
fn folio_document(sizes: &[(f64, f64)]) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    let spread_id = id("SPGS", 1);
    let count = sizes.len();
    let layers = sizes
        .iter()
        .enumerate()
        .map(|(i, (w, h))| StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: id("SLYR", i + 1),
            name: format!("frame-{}", i + 1),
            parent: Parent::Node(NodeRef {
                node_kind: NodeKind::PageSpread,
                node_id: spread_id.clone(),
            }),
            kind: "text".into(),
            payload: Payload::Text {
                story_id: id("STXT", 1),
            },
            visible: true,
            appearance: None,
            text_frame: Some(StudioTextFrame {
                page_id: "page-1".into(),
                bounds: Bounds {
                    x: pt(0.0),
                    y: pt(0.0),
                    width: pt(*w),
                    height: pt(*h),
                },
                inset_spacing: edges(0.0),
                text_column_count: 1,
                text_column_gutter: pt(0.0),
                first_baseline_offset: FirstBaseline::Fixed,
                minimum_first_baseline_offset: pt(10.0),
                vertical_justification: VerticalJustification::Top,
                auto_sizing_type: AutoSizing::Off,
                previous_text_frame: (i > 0).then(|| id("SLYR", i)),
                next_text_frame: (i + 1 < count).then(|| id("SLYR", i + 2)),
            }),
        })
        .collect();
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: spread_id,
            name: "spread-1".into(),
            parent: Parent::Root(()),
            bounds: Bounds {
                x: pt(0.0),
                y: pt(0.0),
                width: pt(1000.0),
                height: pt(1000.0),
            },
            pages: vec![StudioPage {
                page_id: "page-1".into(),
                width: pt(1000.0),
                height: pt(1000.0),
                side: PageSide::SingleSided,
                margins: edges(0.0),
                bleed: None,
            }],
        }],
        layers,
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}
fn folio_story(text: &str) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline {
            text_utf8: text.into(),
        },
        paragraphs: vec![StoryParagraph {
            start: 0,
            end,
            style_ref: None,
            paragraph_direction: None,
        }],
        character_ranges: vec![StoryRange {
            start: 0,
            end,
            style_ref: None,
            applied_language: None,
        }],
    }
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
    let source = SourceSpan {
        start: 0,
        end: text.len() as u64,
    };
    let paragraphs = vec![
        ShapedParagraph::from_fixed_advance(text, source, advance, 12.0, 10.0)
            .ok_or(LayoutError::InvalidInput)?,
    ];
    let document = folio_document(&sizes);
    let story = folio_story(text);
    let proposal = reflow(
        &ReflowRequest {
            document: &document,
            story: &story,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer,
            space: SpaceParams::default(),
            expected_revision: 1,
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
