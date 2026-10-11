//! The WGSL compute pipeline (private: no wgpu type leaves the crate). One pipeline composites
//! one layer (solid fill, uploaded region, or a finished group buffer, optionally through a
//! coverage mask) into a chunk destination buffer.
//!
//! The list of blend modes is NOT written here: `gpu_blend_qualifies` derives it from pigment's
//! `MODES` facts (layer-applicable, class Exact, separable family, plus Normal) and the WGSL
//! switch is generated from that list, so pigment is the only owner of "which modes exist". This
//! module owns only the per-mode WGSL formula bodies (`FORMULAS`, transcribed from
//! `blend_math`); a qualifying mode without a body is simply unsupported (admission refuses it)
//! and the drift test in `tests/gpu_plan.rs` fails until a body with a parity case exists.
use crate::error::GpuError;
use hsk_studio_pigment::blend_math::{Applicability, Exactness, Family, MODES, StudioBlendMode};

pub(crate) const WORKGROUP_SIDE: u32 = 8;
/// Size of the uniform block in `kernel.wgsl` (`Params`).
pub(crate) const PARAMS_BYTES: u64 = 64;
/// Bytes per destination/source texel: RGBA f32.
pub(crate) const TEXEL_BYTES: u64 = 16;
/// Bytes per mask texel: coverage f32.
pub(crate) const MASK_TEXEL_BYTES: u64 = 4;

/// WGSL bodies of `fn blend_<key>(b: f32, s: f32) -> f32` (b = backdrop, s = source), copied
/// operation-for-operation from `hsk_studio_pigment::blend_math` (W3C Compositing 1 s10.1).
const FORMULAS: &[(StudioBlendMode, &str)] = &[
    (StudioBlendMode::Darken, "return min(b, s);"),
    (StudioBlendMode::Multiply, "return b * s;"),
    (
        StudioBlendMode::ColourBurn,
        "if (b == 1.0) { return 1.0; } if (s == 0.0) { return 0.0; } return 1.0 - min((1.0 - b) / s, 1.0);",
    ),
    (StudioBlendMode::Lighten, "return max(b, s);"),
    (StudioBlendMode::Screen, "return b + s - b * s;"),
    (
        StudioBlendMode::ColourDodge,
        "if (b == 0.0) { return 0.0; } if (s == 1.0) { return 1.0; } return min(b / (1.0 - s), 1.0);",
    ),
    // overlay(b, s) = hard_light(s, b)
    (
        StudioBlendMode::Overlay,
        "let hb = s; let hs = b; if (hs <= 0.5) { return hb * (2.0 * hs); } let t = 2.0 * hs - 1.0; return hb + t - hb * t;",
    ),
    (
        StudioBlendMode::SoftLight,
        "if (s <= 0.5) { return b - (1.0 - 2.0 * s) * b * (1.0 - b); } var d: f32; if (b <= 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; } else { d = sqrt(b); } return b + (2.0 * s - 1.0) * (d - b);",
    ),
    (
        StudioBlendMode::HardLight,
        "if (s <= 0.5) { return b * (2.0 * s); } let t = 2.0 * s - 1.0; return b + t - b * t;",
    ),
    (StudioBlendMode::Difference, "return abs(b - s);"),
    (StudioBlendMode::Exclusion, "return b + s - 2.0 * b * s;"),
];

fn formula(mode: StudioBlendMode) -> Option<&'static str> {
    FORMULAS
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, body)| *body)
}

/// Does pigment's fact table say the GPU backend should implement `mode`? Layer-applicable, class
/// Exact, and `Normal` or the separable family (non-separable and Approximate modes are later
/// slices). This is the single source of the supported list.
pub fn gpu_blend_qualifies(mode: StudioBlendMode) -> bool {
    let facts = mode.facts();
    facts.applicability == Applicability::Layer
        && facts.exactness == Exactness::Exact
        && (mode == StudioBlendMode::Normal || facts.family == Family::Separable)
}

/// Qualifies by pigment's facts AND has a WGSL body here (`Normal` needs none). Only these modes
/// pass `exactness()` and are compiled into the shader.
pub fn gpu_blend_supported(mode: StudioBlendMode) -> bool {
    gpu_blend_qualifies(mode) && (mode == StudioBlendMode::Normal || formula(mode).is_some())
}

/// The shader with its blend functions and mode switch generated from `MODES`.
pub(crate) fn shader_source() -> String {
    let mut functions = String::new();
    let mut cases = String::new();
    for facts in &MODES {
        let mode = facts.mode;
        if !gpu_blend_supported(mode) {
            continue;
        }
        let d = mode.discriminant();
        if mode == StudioBlendMode::Normal {
            cases.push_str(&format!(
                "        case {d}u: {{\n            raw = cs;\n        }}\n"
            ));
            continue;
        }
        let key = facts.key;
        let body = formula(mode).unwrap_or("return s;");
        functions.push_str(&format!(
            "fn blend_{key}(b: f32, s: f32) -> f32 {{\n    {body}\n}}\n"
        ));
        cases.push_str(&format!(
            "        case {d}u: {{\n            raw = vec3<f32>(blend_{key}(cb.x, cs.x), blend_{key}(cb.y, cs.y), blend_{key}(cb.z, cs.z));\n        }}\n"
        ));
    }
    include_str!("kernel.wgsl")
        .replace("//@BLEND_FUNCTIONS@", &functions)
        .replace("//@BLEND_CASES@", &cases)
}

/// Where a layer's source texels come from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SourceKind {
    /// Straight RGBA fill.
    Solid([f32; 4]),
    /// Uploaded region, `region.width * region.height` texels, row-major.
    Region,
    /// A finished group buffer of chunk size, indexed like the destination.
    Chunk,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerParams {
    pub chunk_w: u32,
    pub chunk_h: u32,
    pub region: [u32; 4],
    pub mode: StudioBlendMode,
    pub source: SourceKind,
    pub has_mask: bool,
    pub opacity: f32,
    /// Clip-run base initialisation: replace the (cleared) destination with the source texel
    /// scaled by the mask, ignoring opacity and mode.
    pub replace: bool,
    /// Clipped layer inside a run: source alpha times the destination's alpha, alpha held.
    pub clip: bool,
}
impl LayerParams {
    pub(crate) fn to_bytes(self) -> [u8; PARAMS_BYTES as usize] {
        let (kind, solid) = match self.source {
            SourceKind::Solid(rgba) => (0u32, rgba),
            SourceKind::Region => (1, [0.0; 4]),
            SourceKind::Chunk => (2, [0.0; 4]),
        };
        let mut out = [0u8; PARAMS_BYTES as usize];
        let words: [u32; 8] = [
            self.chunk_w,
            self.chunk_h,
            self.region[0],
            self.region[1],
            self.region[2],
            self.region[3],
            u32::from(self.mode.discriminant()),
            kind,
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
        }
        out[32..36].copy_from_slice(&self.opacity.to_ne_bytes());
        out[36..40].copy_from_slice(&u32::from(self.has_mask).to_ne_bytes());
        out[40..44].copy_from_slice(
            &(u32::from(self.replace) | (u32::from(self.clip) << 1)).to_ne_bytes(),
        );
        // bytes 44..48 stay zero (padding); the solid colour starts at the 16-byte aligned 48.
        for (i, c) in solid.iter().enumerate() {
            out[48 + i * 4..52 + i * 4].copy_from_slice(&c.to_ne_bytes());
        }
        out
    }
}

pub(crate) struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl Kernel {
    /// Build the pipeline from `source` (`None` = the generated [`shader_source`]; `Some` is the
    /// fault-injection route for a broken shader). Shader/pipeline validation failures surface as
    /// the typed `GpuError::Pipeline`, never as an uncaptured-error panic and never as
    /// `Unavailable`.
    pub(crate) fn new(device: &wgpu::Device, source: Option<&str>) -> Result<Self, GpuError> {
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let wgsl = source.map_or_else(shader_source, str::to_owned);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Owned(wgsl)),
        });
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hsk-studio-render-gpu layout"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
                entry(2, wgpu::BufferBindingType::Storage { read_only: false }),
                entry(3, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hsk-studio-render-gpu pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("composite"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        match crate::device::block_on_bounded(device.pop_error_scope(), None) {
            Some(None) => Ok(Self { pipeline, layout }),
            Some(Some(_)) => Err(GpuError::Pipeline("shader_validation")),
            None => Err(GpuError::Pipeline("shader_validation_timeout")),
        }
    }

    pub(crate) fn bind(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        mask: &wgpu::Buffer,
        dst: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: src.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dst.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: mask.as_entire_binding(),
                },
            ],
        })
    }

    /// Record one dispatch covering `width` x `height` texels.
    pub(crate) fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIDE),
            height.div_ceil(WORKGROUP_SIDE),
            1,
        );
    }
}
