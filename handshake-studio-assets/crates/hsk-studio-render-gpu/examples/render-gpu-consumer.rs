//! Render-gpu consumer (headless: no window, no surface, no focus).
//! `--descriptor` prints the module descriptor. `--probe [auto|vulkan|dx12|metal|none]` acquires a device and prints one JSON
//! line with the backend/adapter kind/generation, or the typed unavailable reason (exit 3), or the
//! typed pipeline failure of a present adapter (exit 1: a defect, reported separately from "no GPU", exit 3).
//! `--selftest` renders the hand-computed oracle (backdrop 0.2/0.4/0.6, red source at opacity 0.5;
//! Normal, Multiply, Screen) on the device and fails (exit 1) on mismatch or on a pipeline
//! failure; only with no adapter at all it prints NOT_PROVEN and exits 3, which is not a pass (the exit code is non-zero).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, Group, Layer, NoVector,
    OutputTarget, PlanNode, PlanNodeId, PlanSources, RenderBackend, RenderPlan, SampleFormat,
    SinkError, SourceKey, StudioBlendMode, TilePlanes, TileSink,
};
use hsk_studio_render_gpu::{BackendChoice, DESCRIPTOR, DeviceOptions, GpuBackend};
use std::env;
use std::process::ExitCode;

struct NoSources;
impl PlanSources for NoSources {
    fn tile(&self, _key: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, _key: SourceKey) -> Option<hsk_studio_pigment::Mask<'_>> {
        None
    }
}
struct Collect(Vec<f32>);
impl TileSink for Collect {
    fn accept(&mut self, _rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        self.0 = vec![f(colour, 0), f(colour, 4), f(colour, 8), f(alpha, 0)];
        Ok(())
    }
}

fn json_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c if c.is_control() => vec!['?'],
            c => vec![c],
        })
        .collect()
}

fn probe(choice: Option<&str>) -> ExitCode {
    let backend = match choice {
        None | Some("auto") => BackendChoice::Auto,
        Some("vulkan") => BackendChoice::Vulkan,
        Some("dx12") => BackendChoice::Dx12,
        Some("metal") => BackendChoice::Metal,
        Some("none") => BackendChoice::None,
        Some(_) => return ExitCode::from(2),
    };
    let backend = GpuBackend::new(DeviceOptions {
        backend,
        ..DeviceOptions::default()
    });
    if let Some(failure) = backend.failure() {
        println!("{{\"pipeline_failed\":\"{}\"}}", failure.code());
        return ExitCode::from(1);
    }
    match (backend.device_info(), backend.unavailable_reason()) {
        (Some(info), _) => {
            println!(
                "{{\"backend\":\"{}\",\"adapter_kind\":\"{}\",\"adapter\":\"{}\",\"generation\":{}}}",
                info.backend,
                info.adapter_kind.key(),
                json_text(&info.adapter_name),
                info.generation
            );
            ExitCode::SUCCESS
        }
        (None, reason) => {
            let code = reason.map_or("unknown", |r| r.code());
            println!("{{\"unavailable\":\"{code}\"}}");
            ExitCode::from(3)
        }
    }
}

fn selftest() -> ExitCode {
    let backend = GpuBackend::new(DeviceOptions::default());
    if let Some(failure) = backend.failure() {
        println!("FAIL pipeline: {failure}");
        return ExitCode::from(1);
    }
    if backend.device_info().is_none() {
        println!(
            "NOT_PROVEN: no adapter ({:?})",
            backend.unavailable_reason()
        );
        return ExitCode::from(3);
    }
    let pixel = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    for (mode, expect) in [
        (StudioBlendMode::Normal, [0.6_f32, 0.2, 0.3, 1.0]),
        (StudioBlendMode::Multiply, [0.2, 0.2, 0.3, 1.0]),
        (StudioBlendMode::Screen, [0.6, 0.4, 0.6, 1.0]),
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
            min_exactness: Exactness::Fitted,
            deadline: None,
            max_working_bytes: 1 << 24,
        };
        let mut sink = Collect(Vec::new());
        if let Err(error) = backend.execute(
            &plan,
            &request,
            &NoSources,
            &CancellationToken::default(),
            &mut sink,
        ) {
            println!("FAIL {}: {}", mode.key(), error.code());
            return ExitCode::from(1);
        }
        let err = sink
            .0
            .iter()
            .zip(expect)
            .map(|(g, e)| (g - e).abs())
            .fold(0.0_f32, f32::max);
        println!(
            "{} got {:?} want {:?} max_err {:e}",
            mode.key(),
            sink.0,
            expect,
            err
        );
        if err > 2.0e-6 {
            return ExitCode::from(1);
        }
    }
    println!("PASS (backend-qualified: see --probe for the adapter)");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("--descriptor") => {
            println!("{DESCRIPTOR}");
            ExitCode::SUCCESS
        }
        Some("--probe") => probe(env::args().nth(2).as_deref()),
        Some("--selftest") => selftest(),
        _ => {
            eprintln!("usage: render-gpu-consumer --descriptor | --probe | --selftest");
            ExitCode::from(2)
        }
    }
}
