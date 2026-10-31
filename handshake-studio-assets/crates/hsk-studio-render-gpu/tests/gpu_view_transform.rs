//! Stage-6a view transform on render-gpu (lane SMB-PRISM-VIEW-TRANSFORM): the read-back path
//! applies Prism's `ViewTransformId::apply` on the host, so the GPU result is Prism's view of the
//! GPU composite; parity against the CPU reference (pigment `composite_over_extended`, then the
//! same view) within TOLERANCE. The shared-texture output (feature `shared-device`) runs Prism's
//! WGSL form of the SDR views in its output pass (lane SMB-PRISM-VIEW-TRANSFORM-2): parity with
//! the CPU reference encoded the shared way is +-1 per 8-bit channel; an HDR view has no GPU form
//! and is refused typed before any device work. The refusal half runs on the `none` backend; the
//! execution half follows `tests/gpu-proof.toml` (V4-03).
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over_extended};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, DisplayTransform, Exactness, ExecuteRequest, Group,
    Layer, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, PlanSources,
    RenderPlan, SampleFormat, SampleRange, SinkError, SourceKey, StudioBlendMode, TileInput,
    TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::{ViewTransformId, profile_hash};
use hsk_studio_render_gpu::{AdmissionRequest, DeviceOptions, GpuBackend, GpuError, Reason, UnavailablePort};
use std::sync::Arc;

/// Bound on |GPU - CPU reference| per channel (absolute; view output is in [0, 1]): the GPU
/// composite is within 1e-6 relative of pigment (tests/gpu_extended_range.rs) and the view's
/// local gain stays below a few units. Measured values are printed.
const TOLERANCE: f32 = 1e-5;
const VIEW: ViewTransformId = ViewTransformId::Aces1SdrVideo;
const W: u32 = 9;
const H: u32 = 3;
const VIEW_KEY: &str = "display_view_transform";

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
fn tag() -> ColourTag {
    ColourTag { profile_sha256: profile_hash(b"working-profile"), blend_space: BlendSpace::LinearLight }
}
fn scene_pixel(x: u32, y: u32) -> [f32; 4] {
    let k = 2f32.powf(x as f32 - 4.0);
    match y {
        0 => [k, k, k, 1.0],
        1 => [k, 0.4 * k, 0.1 * k, 1.0],
        _ => [0.05 * k, 0.3 * k, k, 0.5],
    }
}
struct Tile {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Tile {
    fn new() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..H {
            for x in 0..W {
                let p = scene_pixel(x, y);
                for v in &p[..3] {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
    fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: W,
            height: H,
            colour: &self.colour,
            colour_stride_bytes: u64::from(W) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(W) * 4,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: tag(),
        }
    }
    fn input(&self) -> TileInput {
        TileInput {
            key: SourceKey(1),
            placement: rect(0, 0, W, H),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
}
struct Sources<'a>(TilePlanes<'a>);
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        (key.0 == 1).then_some(self.0)
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}
struct Grid {
    px: Vec<[f32; 4]>,
    calls: usize,
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        self.calls += 1;
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let s = row * r.width as usize + col;
                let i = (r.y as usize + row) * W as usize + r.x as usize + col;
                self.px[i] = [f(colour, s * 12), f(colour, s * 12 + 4), f(colour, s * 12 + 8), f(alpha, s * 4)];
            }
        }
        Ok(())
    }
}
fn grid() -> Grid {
    Grid { px: vec![[f32::NAN; 4]; (W * H) as usize], calls: 0 }
}
/// The scene tile under a Multiply solid (0.8, 0.9, 1.0) at 75 %.
fn plan(tile: &Tile) -> RenderPlan<NoVector> {
    let layer = |input: u32, mode: StudioBlendMode, opacity: f32| Layer {
        input: PlanNodeId(input),
        mode,
        opacity,
        fill_opacity: 1.0,
        mask: None,
    };
    RenderPlan {
        revision: 3,
        target: OutputTarget {
            extent: rect(0, 0, W, H),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: tag(),
        },
        nodes: vec![
            PlanNode::Tile(tile.input()),
            PlanNode::Solid { rect: rect(5, 0, 4, H), rgba: [0.8, 0.9, 1.0, 1.0] },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![layer(0, StudioBlendMode::Normal, 1.0), layer(1, StudioBlendMode::Multiply, 0.75)],
            }),
        ],
        root: PlanNodeId(2),
    }
}
/// CPU reference: pigment's extended composite, then Prism's view.
fn reference(x: i64, y: i64) -> [f32; 4] {
    reference_with(x, y, hsk_studio_prism::ViewTransform::new(VIEW, hsk_studio_prism::SceneInput::LinearSrgb))
}
fn reference_with(x: i64, y: i64, view: hsk_studio_prism::ViewTransform) -> [f32; 4] {
    let site = PixelSite { x, y, seed: 0 };
    let s = scene_pixel(x as u32, y as u32);
    let mut out =
        composite_over_extended(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new([s[0], s[1], s[2]], s[3]), 1.0, site)
            .unwrap();
    if x >= 5 {
        out = composite_over_extended(StudioBlendMode::Multiply, out, Rgba::new([0.8, 0.9, 1.0], 1.0), 0.75, site).unwrap();
    }
    let v = view.apply(out.rgb).unwrap();
    [v[0], v[1], v[2], out.a]
}
fn request() -> ExecuteRequest {
    ExecuteRequest { expected_revision: 3, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 }
}
fn view_stage() -> PlanExtensions {
    PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, Some(DisplayTransform::View(VIEW))))
}
fn strict() -> bool {
    include_str!("gpu-proof.toml").lines().any(|l| l.trim() == "require_gpu = true")
        || std::env::var("HSK_STUDIO_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// none backend: the view stage passes admission and preparation (reaches `no_adapter`), no sink
/// call. Live adapter, chunk sides 2 and 64: every pixel within TOLERANCE of the CPU reference,
/// receipt ledger names `display_view_transform`. Shared-texture output (feature
/// `shared-device`): the view stage is refused `display_view_transform` Unsupported before any
/// device work (no residency, no stats); since lane SMB-PRISM-VIEW-TRANSFORM-2 only an HDR view is
/// refused there and the SDR views run Prism's WGSL form (+-1 per 8-bit channel against the CPU
/// reference for linear sRGB and ACEScg input and the Display P3 view).
#[test]
fn gpu_view_stage_matches_the_cpu_reference_and_the_texture_path_runs_the_wgsl_view() {
    let tile = Tile::new();
    let p = plan(&tile);
    let src = Sources(tile.planes());
    let cancel = CancellationToken::default();
    let stage = view_stage();

    let none = GpuBackend::with_grant(Arc::new(UnavailablePort::new()), AdmissionRequest::asset_local(), DeviceOptions::default());
    let mut g = grid();
    assert_eq!(
        none.execute_extended_detailed(&p, &stage, &request(), &src, &cancel, &mut g).unwrap_err(),
        GpuError::Unavailable(Reason::NoAdapter)
    );
    assert_eq!(g.calls, 0);

    for chunk_side in [2u32, 64] {
        let gpu = GpuBackend::new(DeviceOptions { chunk_side, ..DeviceOptions::default() });
        if gpu.device_info().is_none() {
            if let Some(failure) = gpu.failure() {
                panic!("adapter present but the backend failed: {failure:?}");
            }
            match gpu.unavailable_reason() {
                Some(Reason::NoAdapter) if strict() => {
                    panic!("NOT_PROVEN: no adapter; gpu view transform parity requires a GPU (tests/gpu-proof.toml)")
                }
                Some(Reason::NoAdapter) => {
                    eprintln!("NOT_PROVEN: no adapter; gpu view transform parity not run");
                    return;
                }
                other => panic!("no live device ({other:?})"),
            }
        }
        let mut g = grid();
        let (receipt, _) = gpu.execute_extended_detailed(&p, &stage, &request(), &src, &cancel, &mut g).unwrap();
        assert!(receipt.exactness.operators.iter().any(|o| o.key == VIEW_KEY));
        let mut max = 0f32;
        for y in 0..H as i64 {
            for x in 0..W as i64 {
                let want = reference(x, y);
                let got = g.px[(y * W as i64 + x) as usize];
                for c in 0..4 {
                    max = max.max((got[c] - want[c]).abs());
                }
                assert!(got[..3].iter().all(|v| (0.0..=1.0).contains(v)), "({x},{y}) {got:?}");
            }
        }
        println!("chunk {chunk_side}: max |delta| GPU view vs CPU reference = {max:e} (tolerance {TOLERANCE:e})");
        assert!(max <= TOLERANCE, "chunk {chunk_side}: deviation {max:e}");
    }

    #[cfg(feature = "shared-device")]
    shared::texture_path_runs_the_sdr_views_and_refuses_hdr(&p, &src);
}

#[cfg(feature = "shared-device")]
mod shared {
    use super::*;
    use hsk_studio_pigment::render_plan::BackendError;
    use hsk_studio_render_gpu::{SharedDevice, TARGET_FORMAT};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let start = std::time::Instant::now();
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            assert!(start.elapsed().as_secs() < 30, "wgpu request timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn read_texture(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(30)) }).expect("poll");
        let view = buffer.slice(..).get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let start = (y * row) as usize;
            out.extend_from_slice(&view[start..start + (w * 4) as usize]);
        }
        out
    }
    /// The shared output encoding (shared.rs module docs) on the CPU.
    fn display(px: [f32; 4]) -> [u8; 4] {
        let unit = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        let enc = |v: f32| {
            let c = unit(v);
            if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
        };
        let byte = |v: f32| (unit(v) * 255.0 + 0.5).floor();
        let a8 = byte(px[3]);
        let pre = |c: f32| ((byte(enc(c)) * a8 / 255.0) + 0.5).floor() as u8;
        [pre(px[0]), pre(px[1]), pre(px[2]), a8 as u8]
    }

    pub(super) fn texture_path_runs_the_sdr_views_and_refuses_hdr(p: &RenderPlan<NoVector>, src: &Sources<'_>) {
        use hsk_studio_prism::{SceneInput, ViewTransform};
        let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
        let instance =
            wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, flags: wgpu::InstanceFlags::empty(), ..Default::default() });
        let adapter = match block(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        })) {
            Ok(a) => a,
            Err(_) if strict() => panic!("NOT_PROVEN: no adapter; shared-texture view requires a GPU"),
            Err(_) => {
                println!("NOT_PROVEN: no adapter; shared-texture view not run");
                return;
            }
        };
        let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("test host device"),
            required_limits: wgpu::Limits::downlevel_defaults(),
            ..Default::default()
        }))
        .expect("adapter present but no device");
        let shared =
            SharedDevice::new(&device, &queue, adapter.get_info(), DeviceOptions { chunk_side: 4, ..DeviceOptions::default() })
                .unwrap();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test target"),
            size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let cancel = CancellationToken::default();
        // SDR views: Prism's WGSL form in the output pass, +-1 per 8-bit channel.
        for vt in [
            ViewTransform::new(ViewTransformId::Aces1SdrVideo, SceneInput::LinearSrgb),
            ViewTransform::new(ViewTransformId::Aces1SdrVideo, SceneInput::AcesCg),
            ViewTransform::new(ViewTransformId::Aces1SdrVideoDisplayP3, SceneInput::LinearSrgb),
        ] {
            let stage = PlanExtensions::default()
                .with_output(OutputStage::new(SampleRange::Extended, Some(DisplayTransform::ViewFrom(vt))));
            let (receipt, _) = shared.render_to_texture(p, Some(&stage), &request(), src, &cancel, &texture).unwrap();
            assert!(receipt.exactness.operators.iter().any(|o| o.key == VIEW_KEY));
            let bytes = read_texture(&device, &queue, &texture);
            let (mut max, mut exact) = (0u8, 0usize);
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    let want = display(reference_with(x, y, vt));
                    let at = ((y * W as i64 + x) * 4) as usize;
                    for c in 0..4 {
                        let d = bytes[at + c].abs_diff(want[c]);
                        max = max.max(d);
                        exact += usize::from(d == 0);
                    }
                }
            }
            println!(
                "shared texture {} from {}: max |delta| {max} LSB, {exact}/{} channels exact",
                vt.view.key(),
                vt.input.key(),
                W * H * 4
            );
            assert!(max <= 1, "{} {}: {max} LSB", vt.view.key(), vt.input.key());
        }
        // HDR view: no GPU form, refused before any device work.
        let fresh = SharedDevice::new(&device, &queue, adapter.get_info(), DeviceOptions::default()).unwrap();
        let hdr = PlanExtensions::default().with_output(OutputStage::new(
            SampleRange::Extended,
            Some(DisplayTransform::View(ViewTransformId::Aces1HdrVideo1000Nits)),
        ));
        let err = fresh.render_to_texture(p, Some(&hdr), &request(), src, &cancel, &texture).unwrap_err();
        assert_eq!(err, GpuError::Backend(BackendError::Unsupported { operator: VIEW_KEY, exactness: Exactness::Unsupported }));
        assert_eq!(fresh.backend().resident_bytes(), 0);
        assert!(fresh.backend().last_stats().is_none());
    }
}
