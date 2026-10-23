//! Render-cpu consumer: `--descriptor` prints the module descriptor; `--selftest` renders the
//! hand-computed oracle (backdrop 0.2/0.4/0.6, red source at opacity 0.5) through the pigment
//! `RenderBackend` port and fails on mismatch.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer, NoVector,
    OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan, SampleFormat,
    SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_cpu::{DESCRIPTOR, Renderer};
use std::env;

struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _key: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _key: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct Collect {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl TileSink for Collect {
    fn accept(&mut self, _rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        self.colour.extend_from_slice(colour);
        self.alpha.extend_from_slice(alpha);
        Ok(())
    }
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn selftest() -> Result<(), String> {
    let pixel = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    for (mode, expect) in [
        (StudioBlendMode::Normal, [0.6_f32, 0.2, 0.3]),
        (StudioBlendMode::Multiply, [0.2, 0.2, 0.3]),
    ] {
        let layer = |input, mode, opacity| Layer {
            input: PlanNodeId(input),
            mode,
            opacity,
            fill_opacity: 1.0,
            mask: None,
        };
        let plan: RenderPlan<NoVector> = RenderPlan {
            revision: 1,
            target: OutputTarget {
                extent: pixel,
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                space: ColourTag {
                    profile_sha256: [0; 32],
                    blend_space: BlendSpace::LinearLight,
                },
            },
            nodes: vec![
                PlanNode::Solid {
                    rect: pixel,
                    rgba: [0.2, 0.4, 0.6, 1.0],
                },
                PlanNode::Solid {
                    rect: pixel,
                    rgba: [1.0, 0.0, 0.0, 1.0],
                },
                PlanNode::Group(Group {
                    isolated: true,
                    layers: vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, mode, 0.5)],
                }),
            ],
            root: PlanNodeId(2),
        };
        let request = ExecuteRequest {
            expected_revision: 1,
            min_exactness: Exactness::Exact,
            deadline: None,
            max_working_bytes: 1 << 20,
        };
        let mut sink = Collect {
            colour: vec![],
            alpha: vec![],
        };
        let receipt = Renderer::default()
            .execute(
                &plan,
                &request,
                &NoSources,
                &CancellationToken::default(),
                &mut sink,
            )
            .map_err(|e| e.code().to_owned())?;
        let (c, a) = (f32s(&sink.colour), f32s(&sink.alpha));
        let ok = c
            .iter()
            .zip(expect)
            .all(|(got, want)| (got - want).abs() < 1e-6)
            && (a[0] - 1.0).abs() < 1e-6;
        println!(
            "{} colour={c:?} alpha={a:?} chunks={} nodes={} lowest={}",
            mode.key(),
            receipt.chunks,
            receipt.nodes_executed,
            receipt.exactness.lowest.key()
        );
        if !ok {
            return Err(format!("{} oracle mismatch", mode.key()));
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--descriptor"] | ["--help"] => println!("{DESCRIPTOR}"),
        ["--selftest"] => {
            if let Err(error) = selftest() {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: render-cpu-consumer --descriptor | --selftest");
            std::process::exit(2);
        }
    }
}
