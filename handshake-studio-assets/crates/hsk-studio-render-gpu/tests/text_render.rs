//! SMB-COMPOSITE-TEXT (decision B1: render-gpu dev-depends on composite): the GPU executes the plan
//! composite lowers for a text layer (`text_layer_render_plan`) and matches the CPU result within
//! the established per-layer tolerance. render-gpu may not depend on render-cpu, so the comparison
//! goes through the expectation render-cpu tests/text_render.rs asserts within 1e-6 (alpha = fill
//! alpha * coverage / 255, colour = fill where inked). GPU proof policy: tests/gpu-proof.toml
//! (`require_gpu = true`: no adapter FAILS with `NOT_PROVEN: no adapter`, never green).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    PreparedText, TextLayerInput, TextRenderLowering, TextResolution, TextRun, prepare_text,
    text_layer_render_plan,
};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, OutputTarget, PlanSources,
    RenderBackend, SampleFormat, SinkError, SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::profile_hash;
use hsk_studio_render_gpu::{AdmissionRequest, DeviceOptions, GpuBackend, Reason, UnavailablePort};
use std::sync::Arc;
use hsk_studio_type::FontBytes;

#[path = "../../hsk-studio-composite/tests/common/text_shaping.rs"]
mod shaping;
use shaping::{FontFile, Seg, shape};

const LAYER: &str = "SLYR-019abcde-0000-7000-8000-000000000003";
const FILL: [f32; 4] = [0.8, 0.1, 0.2, 1.0];
/// tests/gpu_plan.rs per-layer parity tolerance; the text plan has 2 layer records.
const TOL_PER_LAYER: f32 = 1.0e-6;
const LAYER_RECORDS: f32 = 2.0;
const GPU_PROOF_POLICY: &str = include_str!("gpu-proof.toml");

/// Same parse as tests/gpu_plan.rs (which proves malformed policies are refused).
fn require_gpu() -> bool {
    let mut require = None;
    for line in GPU_PROOF_POLICY.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
            Some(("require_gpu", "true")) => require = Some(true),
            Some(("require_gpu", "false")) => require = Some(false),
            Some(("schema", v)) => assert_eq!(v, "\"hsk.studio.render-gpu-proof@1\""),
            other => panic!("tests/gpu-proof.toml: unexpected line {other:?}"),
        }
    }
    require.expect("tests/gpu-proof.toml require_gpu")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}
/// `HSK_STUDIO_TEST_GPU_BACKEND` (lane SMB-GPU-CLIENT-PORT-1): `none` = the wgpu-free
/// `UnavailablePort`, unset or `auto` = the transient adapter's platform order (Vulkan first on
/// Windows), `vulkan|dx12|metal` through the test-only feature `asset-local-backends`; without it
/// only `vulkan` is accepted and only when the default actually opened Vulkan (never silently
/// proven on another backend).
fn env_backend(options: DeviceOptions) -> GpuBackend {
    match std::env::var("HSK_STUDIO_TEST_GPU_BACKEND").as_deref() {
        Ok("none") => GpuBackend::with_grant(Arc::new(UnavailablePort::new()), AdmissionRequest::asset_local(), options),
        Ok("auto") | Err(_) => GpuBackend::new(options),
        Ok(other) => selected_backend(options, other),
    }
}
#[cfg(feature = "asset-local-backends")]
fn selected_backend(options: DeviceOptions, key: &str) -> GpuBackend {
    let backend = match key {
        "vulkan" => hsk_studio_render_gpu::BackendChoice::Vulkan,
        "dx12" => hsk_studio_render_gpu::BackendChoice::Dx12,
        "metal" => hsk_studio_render_gpu::BackendChoice::Metal,
        other => panic!("HSK_STUDIO_TEST_GPU_BACKEND={other:?} is not none|auto|vulkan|dx12|metal"),
    };
    GpuBackend::new(DeviceOptions { backend, ..options })
}
#[cfg(not(feature = "asset-local-backends"))]
fn selected_backend(options: DeviceOptions, key: &str) -> GpuBackend {
    let backend = GpuBackend::new(options);
    let opened = backend.device_info().map(|info| info.backend);
    match (key, opened) {
        ("vulkan", Some("vulkan") | None) => backend,
        (other, opened) => panic!(
            "HSK_STUDIO_TEST_GPU_BACKEND={other:?} needs the test-only feature asset-local-backends (the transient default opened {opened:?})"
        ),
    }
}
fn prepared() -> PreparedText {
    let files = [FontFile::inter()];
    let segs = [Seg { start: 0, end: 2, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
    shape(&files, "Hi", &segs, false, 12.0, |p| {
        let run = &p.paragraphs()[0].runs()[0];
        let runs = [TextRun { run, font: FontBytes { bytes: &files[0].bytes, resource: run.resolved() }, origin_px: [10.25, 40.0] }];
        prepare_text(
            &[TextLayerInput { layer_id: LAYER, runs: &runs, fill_rgba: FILL }],
            &TextResolution::at_ppi(192.0),
            &CancellationToken::default(),
        )
        .unwrap()
    })
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 64, height: 64 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: profile_hash(b"working-profile"), blend_space: BlendSpace::Encoded },
    }
}
struct Sources<'a> {
    low: &'a TextRenderLowering,
    text: &'a PreparedText,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, _: SourceKey) -> Option<TilePlanes<'_>> {
        None
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        self.low.text_mask(key, self.text)
    }
}
struct Image {
    px: Vec<[f32; 4]>,
}
impl TileSink for Image {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                let at = (r.y as usize + y) * 64 + r.x as usize + x;
                self.px[at] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

#[test]
fn gpu_renders_lowered_text_like_cpu() {
    let text = prepared();
    let low = text_layer_render_plan(&text, LAYER, 7, target()).unwrap();
    let backend = env_backend(DeviceOptions { chunk_side: 16, ..DeviceOptions::default() });
    if backend.device_info().is_none() {
        if let Some(failure) = backend.failure() {
            panic!("text render: adapter present but the backend failed: {failure} ({failure:?})");
        }
        match backend.unavailable_reason() {
            Some(Reason::NoAdapter) if !require_gpu() => {
                println!("NOT_PROVEN: no adapter; text render GPU parity not run");
                return;
            }
            Some(Reason::NoAdapter) => panic!("NOT_PROVEN: no adapter; text render GPU parity requires a GPU (tests/gpu-proof.toml require_gpu = true)"),
            other => panic!("text render: no live device (unavailable {other:?})"),
        }
    }
    let mut image = Image { px: vec![[f32::NAN; 4]; 64 * 64] };
    // The GPU declares its float blend Fitted (tests/gpu_plan.rs parity class); Exact is refused.
    let request = ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Fitted, deadline: None, max_working_bytes: 64 << 20 };
    backend
        .execute(&low.lowering.plan, &request, &Sources { low: &low, text: &text }, &CancellationToken::default(), &mut image)
        .unwrap_or_else(|e| panic!("GPU execute of the text plan refused: {e:?}"));
    let plane = text.plane(LAYER).unwrap();
    let p = plane.placement.unwrap();
    let (mut max_err, mut ink) = (0.0f32, 0usize);
    for y in 0..64i64 {
        for x in 0..64i64 {
            let px = image.px[(y * 64 + x) as usize];
            assert!(px.iter().all(|v| v.is_finite()), "pixel ({x},{y}) not written");
            let inside = x >= p.x && y >= p.y && x < p.x + i64::from(p.width) && y < p.y + i64::from(p.height);
            let cov = if inside {
                f32::from(plane.coverage[((y - p.y) * i64::from(p.width) + (x - p.x)) as usize]) / 255.0
            } else {
                0.0
            };
            max_err = max_err.max((px[3] - FILL[3] * cov).abs());
            if cov > 0.0 {
                ink += 1;
                for c in 0..3 {
                    max_err = max_err.max((px[c] - FILL[c]).abs());
                }
            }
        }
    }
    let tolerance = TOL_PER_LAYER * LAYER_RECORDS;
    println!("TEXT max_err {max_err:e} over {ink} inked pixels (tolerance {tolerance:e})");
    assert!(ink > 0);
    assert!(max_err <= tolerance, "GPU text vs CPU expectation {max_err:e} > {tolerance:e}");
}
