//! Clip colour grade on the GPU (lane SMB-PR-SOURCE-MONITOR; composite `SceneContent::Graded`):
//! pigment's `RenderBackend::execute_grade` port. A Prism `GradeProgram` (composite
//! `ClipGrade::program`: the same prepared stages composite runs on the host) is packed into two
//! storage buffers (an index header and an `f32` program: scalars, wheels, secondaries, LUT rows,
//! spline knots) and `grade.wgsl` runs one invocation per texel over the straight-alpha surface in
//! place; the graded colour is read back into the caller's surface. Alpha is never changed and
//! texels whose alpha is not above 0 are left as they are (as composite's host loop).
//!
//! Refusals before any device work: an invalid program (Prism `GradeProgram::validate`: LUT sizes
//! and rows, spline knots, scalar bounds, at most 8 secondaries) is `Unsupported`
//! (`clip_grade_program`); a surface whose planes do not match `width x height` is `Unsupported`
//! (`clip_grade_surface`); buffers above the device cap are `BudgetExceeded`. Device faults are the
//! typed `clip_grade_submit_fault` / `clip_grade_readback_failed`. Agreement with the host grade:
//! `tests/composition_graded.rs` (binary32 transcription of binary32/binary64 host maths; the
//! stated tolerance is the keying kernel's 1e-4, `tests/gpu_keying.rs`).
#![deny(clippy::arithmetic_side_effects)]
use super::{DEFAULT_POLL_LIMIT, overflow, unsupported};
use crate::device::GpuContext;
use crate::error::GpuError;
use crate::pipeline::{ErrorScopes, Stage, guarded};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::{BackendError, GradeSurface};
use hsk_studio_prism::CubeLut;
use hsk_studio_prism::grade::{GradeProgram, GradeSpline, Wheels};

const LABEL: &str = "hsk-studio-render-gpu clip grade";
/// Refusal key of an invalid program.
pub(crate) const CLIP_GRADE_PROGRAM: &str = "clip_grade_program";
/// Refusal key of a surface whose planes do not match its size.
pub(crate) const CLIP_GRADE_SURFACE: &str = "clip_grade_surface";

const HEADER_WORDS: usize = 32;
const HEADER_BYTES: u64 = 128;
const PRIMARY: usize = 24;
const WHEELS: usize = 73;
const SECONDARIES: usize = 122;
const SECONDARY_STRIDE: usize = 60;
const FIXED_FLOATS: usize = 602;

const F_INPUT_LUT: u32 = 1;
const F_EXPOSURE: u32 = 2;
const F_PRIMARY: u32 = 4;
const F_TONE: u32 = 8;
const F_SATURATION: u32 = 16;
const F_VIBRANCE: u32 = 32;
const F_LOOK: u32 = 64;
const F_FADED: u32 = 128;
const F_RGB_CURVES: u32 = 256;
const F_HSL_CURVES: u32 = 512;
const F_WHEELS: u32 = 1024;
const F_VIGNETTE: u32 = 2048;

fn narrow(v: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let f = v as f32;
    f
}

/// The packed program: index header and `f32` body.
struct Packed {
    header: Vec<u32>,
    body: Vec<f32>,
}

fn put(body: &mut [f32], at: usize, v: f32) -> Result<(), GpuError> {
    *body.get_mut(at).ok_or_else(overflow)? = v;
    Ok(())
}
fn word(header: &mut [u32], at: usize, v: usize) -> Result<(), GpuError> {
    *header.get_mut(at).ok_or_else(overflow)? = u32::try_from(v).map_err(|_| overflow())?;
    Ok(())
}

fn pack_wheels(body: &mut [f32], base: usize, w: &Wheels) -> Result<(), GpuError> {
    let mut at = base;
    let mut push = |v: f32| -> Result<(), GpuError> {
        put(body, at, v)?;
        at = at.checked_add(1).ok_or_else(overflow)?;
        Ok(())
    };
    for band in [&w.overall, &w.shadows, &w.midtones, &w.highlights] {
        for v in band.offset.iter().chain(&band.gamma).chain(&band.gain) {
            push(*v)?;
        }
    }
    for v in [w.temperature, w.magenta, w.contrast, w.pivot, w.input_saturation] {
        push(v)?;
    }
    for v in w.post_saturation {
        push(v)?;
    }
    push(w.low_mid)?;
    push(w.mid_high)?;
    for v in w.luma {
        push(v)?;
    }
    Ok(())
}

/// Appends a LUT's 3D then 1D rows; returns (3D offset, 3D size, 1D offset, 1D size).
fn pack_lut(body: &mut Vec<f32>, l: &CubeLut) -> Result<[usize; 4], GpuError> {
    let o3 = body.len();
    for r in &l.lut_3d {
        body.extend_from_slice(r);
    }
    let o1 = body.len();
    let n1 = l.lut_1d.as_ref().map_or(0, Vec::len);
    for r in l.lut_1d.iter().flatten() {
        body.extend_from_slice(r);
    }
    Ok([o3, l.size_3d, o1, n1])
}

fn pack_spline(body: &mut Vec<f32>, s: &GradeSpline) -> usize {
    let o = body.len();
    body.extend(s.xs.iter().chain(&s.ys).chain(&s.m).map(|v| narrow(*v)));
    o
}

/// Packs `p` (already validated) for `width` x `height`.
fn pack(p: &GradeProgram, width: u32, height: u32) -> Result<Packed, GpuError> {
    let mut header = vec![0u32; HEADER_WORDS];
    let mut body = vec![0.0f32; FIXED_FLOATS];
    let mut flags = 0u32;
    header[0] = width;
    header[1] = height;
    word(&mut header, 3, p.secondaries.len())?;
    for (i, v) in p.luma.iter().enumerate() {
        put(&mut body, i, *v)?;
    }
    #[allow(clippy::float_cmp)]
    if p.exposure_gain != 1.0 {
        flags |= F_EXPOSURE;
    }
    put(&mut body, 3, p.exposure_gain)?;
    #[allow(clippy::float_cmp)]
    if p.saturation != 1.0 {
        flags |= F_SATURATION;
    }
    put(&mut body, 4, p.saturation)?;
    if let Some(v) = p.vibrance {
        flags |= F_VIBRANCE;
        put(&mut body, 5, v)?;
    }
    if p.faded_floor > 0.0 {
        flags |= F_FADED;
    }
    put(&mut body, 7, p.faded_floor)?;
    for (which, lut) in [(0usize, p.input_lut.as_deref()), (1, p.look.as_ref().map(|(l, _)| &**l))] {
        let Some(l) = lut else { continue };
        let words = pack_lut(&mut body, l)?;
        for (k, w) in words.iter().enumerate() {
            word(&mut header, 4usize.saturating_add(which.saturating_mul(4)).saturating_add(k), *w)?;
        }
        let pb = 8usize.saturating_add(which.saturating_mul(6));
        for k in 0..3 {
            put(&mut body, pb.saturating_add(k), narrow(l.domain_min[k]))?;
            put(&mut body, pb.saturating_add(3).saturating_add(k), narrow(l.domain_max[k]))?;
        }
        flags |= if which == 0 { F_INPUT_LUT } else { F_LOOK };
    }
    if let Some((_, k)) = &p.look {
        put(&mut body, 6, *k)?;
    }
    if let Some(v) = &p.vignette {
        flags |= F_VIGNETTE;
        for (k, x) in [v.amount, v.midpoint, v.roundness, v.feather].iter().enumerate() {
            put(&mut body, 20usize.saturating_add(k), *x)?;
        }
    }
    if let Some(w) = &p.primary {
        flags |= F_PRIMARY;
        pack_wheels(&mut body, PRIMARY, w)?;
    }
    if let Some(w) = &p.wheels {
        flags |= F_WHEELS;
        pack_wheels(&mut body, WHEELS, w)?;
    }
    for (i, s) in p.secondaries.iter().enumerate() {
        let sb = SECONDARIES.checked_add(i.checked_mul(SECONDARY_STRIDE).ok_or_else(overflow)?).ok_or_else(overflow)?;
        let k = &s.key;
        for (j, v) in [
            k.hue.lo,
            k.hue.hi,
            k.hue.soft,
            k.saturation.lo,
            k.saturation.hi,
            k.saturation.soft,
            k.luminance.lo,
            k.luminance.hi,
            k.luminance.soft,
            if k.invert { 1.0 } else { 0.0 },
            s.saturation,
        ]
        .iter()
        .enumerate()
        {
            put(&mut body, sb.saturating_add(j), *v)?;
        }
        pack_wheels(&mut body, sb.saturating_add(11), &s.correction)?;
    }
    // Splines: 0 tone, 1..=4 RGB (composite, red, green, blue), 5..=9 HSL curves.
    let splines: Vec<Option<&GradeSpline>> = std::iter::once(p.tone.as_ref())
        .chain(p.rgb_curves.iter().map(Option::as_ref))
        .chain(p.hsl_curves.iter().map(Option::as_ref))
        .collect();
    for (k, s) in splines.iter().enumerate() {
        if let Some(s) = s {
            let o = pack_spline(&mut body, s);
            word(&mut header, 12usize.saturating_add(k), o)?;
            word(&mut header, 22usize.saturating_add(k), s.xs.len())?;
        }
    }
    if p.tone.is_some() {
        flags |= F_TONE;
    }
    if p.rgb_curves.iter().any(Option::is_some) {
        flags |= F_RGB_CURVES;
    }
    if p.hsl_curves.iter().any(Option::is_some) {
        flags |= F_HSL_CURVES;
    }
    header[2] = flags;
    // Every offset the kernel reads is a u32 index into the body.
    u32::try_from(body.len()).map_err(|_| overflow())?;
    Ok(Packed { header, body })
}

/// A pipeline fault: its detail recorded on the context (`GpuBackend::submit_fault` after the call).
fn fault(ctx: &GpuContext, e: crate::pipeline::PipelineFault) -> GpuError {
    let error = e.error();
    ctx.record_fault(e);
    error
}

fn bytes_of_u32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_le_bytes()).collect()
}
fn bytes_of_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Runs `program` over `surface` on `ctx` (module docs).
pub(super) fn run(
    ctx: &GpuContext,
    program: &GradeProgram,
    surface: &mut GradeSurface<'_>,
    cancel: &CancellationToken,
) -> Result<(), GpuError> {
    cancel.check().map_err(|_| GpuError::Backend(BackendError::Canceled))?;
    program.validate().map_err(|_| unsupported(CLIP_GRADE_PROGRAM))?;
    let bad_surface = || unsupported(CLIP_GRADE_SURFACE);
    let n = u64::from(surface.width).checked_mul(u64::from(surface.height)).ok_or_else(bad_surface)?;
    let colour_bytes = n.checked_mul(12).ok_or_else(bad_surface)?;
    let alpha_bytes = n.checked_mul(4).ok_or_else(bad_surface)?;
    if n == 0
        || u64::try_from(surface.colour.len()).ok() != Some(colour_bytes)
        || u64::try_from(surface.alpha.len()).ok() != Some(alpha_bytes)
    {
        return Err(unsupported(CLIP_GRADE_SURFACE));
    }
    let packed = pack(program, surface.width, surface.height)?;
    let body_bytes = u64::try_from(packed.body.len()).ok().and_then(|v| v.checked_mul(4)).ok_or_else(overflow)?;
    for requested in [colour_bytes, body_bytes] {
        if requested > ctx.max_buffer_bytes {
            return Err(GpuError::Backend(BackendError::BudgetExceeded { requested, limit: ctx.max_buffer_bytes }));
        }
    }
    let groups = (surface.width.div_ceil(8), surface.height.div_ceil(8));
    let device = &ctx.device;
    let module = guarded(device, LABEL, Stage::Module, || {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("../grade.wgsl"))),
        })
    })
    .map_err(|e| fault(ctx, e))?;
    let entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    };
    let layout = guarded(device, LABEL, Stage::Pipeline, || {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(LABEL),
            entries: &[entry(0, true), entry(1, true), entry(2, false), entry(3, true)],
        })
    })
    .map_err(|e| fault(ctx, e))?;
    let pipeline = guarded(device, LABEL, Stage::Pipeline, || {
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(LABEL),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(LABEL),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("grade"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        })
    })
    .map_err(|e| fault(ctx, e))?;
    cancel.check().map_err(|_| GpuError::Backend(BackendError::Canceled))?;
    let scopes = ErrorScopes::push(device);
    let buffer = |label: &'static str, size: u64, usage: wgpu::BufferUsages| {
        device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(16), usage, mapped_at_creation: false })
    };
    let read = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let header = buffer("clip grade header", HEADER_BYTES, read);
    let body = buffer("clip grade program", body_bytes, read);
    let colour = buffer("clip grade colour", colour_bytes, read | wgpu::BufferUsages::COPY_SRC);
    let alpha = buffer("clip grade alpha", alpha_bytes, read);
    ctx.queue.write_buffer(&header, 0, &bytes_of_u32(&packed.header));
    ctx.queue.write_buffer(&body, 0, &bytes_of_f32(&packed.body));
    ctx.queue.write_buffer(&colour, 0, surface.colour);
    ctx.queue.write_buffer(&alpha, 0, surface.alpha);
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(LABEL),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: header.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: body.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: colour.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: alpha.as_entire_binding() },
        ],
    });
    let staging = buffer("clip grade staging", colour_bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(LABEL) });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(LABEL), timestamp_writes: None });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
    }
    enc.copy_buffer_to_buffer(&colour, 0, &staging, 0, colour_bytes);
    ctx.queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |o| {
        let _ = tx.send(o);
    });
    let polled = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(DEFAULT_POLL_LIMIT) });
    ctx.check_alive()?;
    if polled.is_err() || !matches!(rx.try_recv(), Ok(Ok(()))) {
        let captured = scopes.finish();
        return Err(GpuError::Gpu(if captured.is_some() { "clip_grade_submit_fault" } else { "clip_grade_readback_failed" }));
    }
    {
        let data = staging.slice(..).get_mapped_range();
        let len = surface.colour.len();
        let src = data.get(..len).ok_or_else(overflow)?;
        surface.colour.copy_from_slice(src);
    }
    staging.unmap();
    if scopes.finish().is_some() {
        return Err(GpuError::Gpu("clip_grade_submit_fault"));
    }
    Ok(())
}
