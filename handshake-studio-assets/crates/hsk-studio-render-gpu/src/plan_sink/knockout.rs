//! Knockout on the GPU backend (lane SMB-PS-ADJUST-STYLES-3): the placements render-cpu runs
//! (lanes SMB-PS-ADJUST-STYLES and -2, pigment `knockout_math`, the single owner of the rule) as
//! three passes of `kernel_knockout.wgsl`:
//!
//! - shallow in an isolated group and at the plan root: the initial backdrop is transparent;
//! - shallow in a pass-through group: the initial backdrop is the running result of the enclosing
//!   buffer just before the group began, a device copy taken when the group is entered (one
//!   snapshot buffer per pass-through group holding a shallow knockout, an upper bound of their
//!   nesting);
//! - deep: the initial backdrop is the transparent document base, the shape is united into the
//!   plane of the nearest enclosing isolated buffer, and when that isolated group finishes its plane
//!   clears the enclosing running result and carries on to the next enclosing plane (one f32 plane
//!   per destination level below the root when the plan holds a deep knockout).
//!
//! A knockout layer: `knockout_s` writes the shape (content alpha x mask x LAYER opacity) and S
//! (the content over the initial backdrop at opacity x fill, the composite kernel's arithmetic),
//! `knockout_mix` lerps the running result toward S by the shape. Pigment's admission restricts the
//! layer (unclipped, not pass-through, no other envelope operation but a mask feather or a
//! transform) and the placements that read outside the layer's own buffer to a plain ancestry, so
//! the contexts below are pushed only by the plain group paths (`eval_layer` pass-through,
//! `source_pass` isolated). Every buffer is charged to the residency before it is created (`run`).
//! Class Approximate (pigment's, lowered by Fitted); parity with render-cpu is tested in
//! tests/gpu_knockout.rs. Photoshop parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{Exec, PLayer, PNode, Prepared, Src, eval_group, intersect, offset, overflow, texel_bytes, unsupported};
use crate::error::GpuError;
use crate::kernel::{KO_EXTENDED, KO_OUTER, KO_RECORD, KoPass, LayerParams, SourceKind, ko_params_bytes};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{ExtOperator, Knockout, StudioBlendMode};
use std::collections::{BTreeMap, BTreeSet};

/// Knockout facts of a prepared plan (empty without knockout layers).
#[derive(Debug, Default)]
pub(super) struct KoPlan {
    /// Some reachable layer has a knockout: the knockout buffers exist.
    pub(super) any: bool,
    /// Pass-through group nodes holding a shallow knockout layer: each snapshots its start.
    pub(super) snapshot_groups: BTreeSet<usize>,
    /// Isolated non-root group nodes enclosing a deep knockout: each keeps a shape plane while it
    /// is evaluated.
    pub(super) deep_groups: BTreeSet<usize>,
}

impl KoPlan {
    /// From the knockout layers `(group node, depth)` and the group-layer parent of every node
    /// (`parent[node] = the group whose layer uses it`). Pigment's plain-ancestry admission makes
    /// the parent of every node on a deep chain unique; the walk is bounded by the node count.
    pub(super) fn new(
        layers: &[(usize, Knockout)],
        isolated: &BTreeMap<usize, bool>,
        parent: &BTreeMap<usize, usize>,
        root: usize,
    ) -> Self {
        let mut plan = Self {
            any: !layers.is_empty(),
            ..Self::default()
        };
        for (group, depth) in layers {
            match depth {
                Knockout::Shallow if isolated.get(group) == Some(&false) => {
                    plan.snapshot_groups.insert(*group);
                }
                Knockout::Deep => {
                    let mut node = *group;
                    for _ in 0..=parent.len() {
                        if node == root {
                            break;
                        }
                        if isolated.get(&node) == Some(&true) {
                            plan.deep_groups.insert(node);
                        }
                        match parent.get(&node) {
                            Some(p) => node = *p,
                            None => break,
                        }
                    }
                }
                _ => {}
            }
        }
        plan
    }
}

/// An enclosing buffer context of the layer being evaluated, innermost last.
#[derive(Clone, Copy, Debug)]
pub(super) enum KoCtx {
    /// A pass-through group holding a shallow knockout: its start snapshot slot.
    Snapshot { slot: usize, group: usize },
    /// An isolated group enclosing a deep knockout, evaluated into `levels[level]`.
    Isolated { level: usize },
}

/// Device buffers of the knockout passes (only when the plan has a knockout layer).
pub(super) struct KoBuffers {
    /// The `Ko` uniform block.
    pub(super) params: wgpu::Buffer,
    /// S, frame-sized RGBA f32 (prefilled with the initial backdrop per knockout layer).
    pub(super) s: wgpu::Buffer,
    /// The layer's shape, frame-sized f32.
    pub(super) k: wgpu::Buffer,
    /// A 16-byte placeholder bound as the accumulated shape when nothing accumulates.
    pub(super) none: wgpu::Buffer,
    /// Start snapshots, frame-sized RGBA f32.
    pub(super) snapshots: Vec<wgpu::Buffer>,
    /// Deep shape planes of destination levels `1..`, frame-sized f32 (`planes[level - 1]`).
    pub(super) planes: Vec<wgpu::Buffer>,
    pub(super) stack: Vec<KoCtx>,
}

impl KoBuffers {
    fn plane(&self, level: usize) -> Result<&wgpu::Buffer, GpuError> {
        level
            .checked_sub(1)
            .and_then(|i| self.planes.get(i))
            .ok_or_else(|| unsupported("knockout_scratch"))
    }
    /// Destination level of the nearest enclosing isolated context (deep record target).
    fn nearest_isolated(&self) -> Option<usize> {
        self.stack.iter().rev().find_map(|c| match c {
            KoCtx::Isolated { level } => Some(*level),
            KoCtx::Snapshot { .. } => None,
        })
    }
}

/// Byte counts of the knockout buffers for frames of `texels` texels: `(per texel, fixed)`.
pub(super) fn ko_bytes(ko: &KoPlan, levels: usize) -> Result<(u64, u64), GpuError> {
    if !ko.any {
        return Ok((0, 0));
    }
    let snaps = u64::try_from(ko.snapshot_groups.len()).map_err(|_| overflow())?;
    let planes = if ko.deep_groups.is_empty() {
        0
    } else {
        u64::try_from(levels.saturating_sub(1)).map_err(|_| overflow())?
    };
    // S (16) + shape (4) + snapshots (16 each) + planes (4 each) per texel; Ko block + placeholder.
    let per = snaps
        .checked_add(1)
        .and_then(|n| n.checked_mul(16))
        .and_then(|b| planes.checked_add(1).and_then(|p| p.checked_mul(4)).and_then(|p| b.checked_add(p)))
        .ok_or_else(overflow)?;
    Ok((per, crate::kernel::KO_PARAMS_BYTES.saturating_add(16)))
}

impl Exec<'_> {
    fn ko_buffers(&self) -> Result<&KoBuffers, GpuError> {
        self.ko.as_ref().ok_or_else(|| unsupported("knockout_scratch"))
    }

    /// Entering pass-through group `group`, which composites into `levels[level]`: snapshot its
    /// start (the running result) for the shallow knockouts inside.
    pub(super) fn ko_push_snapshot(&mut self, level: usize, group: usize, chunk: Rect) -> Result<(), GpuError> {
        let bytes = texel_bytes(chunk.width, chunk.height);
        let ctx = self.ctx;
        let from = self.levels.get(level).ok_or_else(|| unsupported("plan_shape"))?;
        let ko = self.ko.as_mut().ok_or_else(|| unsupported("knockout_scratch"))?;
        let slot = ko.stack.iter().filter(|c| matches!(c, KoCtx::Snapshot { .. })).count();
        let to = ko.snapshots.get(slot).ok_or_else(|| unsupported("knockout_scratch"))?;
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hsk-studio-render-gpu knockout snapshot"),
        });
        encoder.copy_buffer_to_buffer(from, 0, to, 0, bytes);
        ctx.queue.submit(Some(encoder.finish()));
        ko.stack.push(KoCtx::Snapshot { slot, group });
        Ok(())
    }

    /// Leave the innermost snapshot context.
    pub(super) fn ko_pop(&mut self) {
        if let Some(ko) = self.ko.as_mut() {
            ko.stack.pop();
        }
    }

    /// Entering an isolated group enclosing a deep knockout, evaluated into `levels[level]`: a
    /// zeroed shape plane.
    pub(super) fn ko_push_isolated(&mut self, level: usize, chunk: Rect) -> Result<(), GpuError> {
        let bytes = u64::from(chunk.width)
            .checked_mul(u64::from(chunk.height))
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(overflow)?;
        let ctx = self.ctx;
        let ko = self.ko.as_mut().ok_or_else(|| unsupported("knockout_scratch"))?;
        let plane = ko.plane(level)?;
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hsk-studio-render-gpu knockout plane"),
        });
        encoder.clear_buffer(plane, 0, Some(bytes));
        ctx.queue.submit(Some(encoder.finish()));
        ko.stack.push(KoCtx::Isolated { level });
        Ok(())
    }

    /// Leave the isolated group of `levels[parent + 1]` before it composites into
    /// `levels[parent]`: clear that running result by the group's accumulated deep shape (pigment
    /// `knockout_clear`) and unite the shape into the next enclosing isolated plane (pigment
    /// `knockout_union`); at the root there is no further plane.
    pub(super) fn ko_pop_isolated(&mut self, parent: usize, chunk: Rect) -> Result<(), GpuError> {
        let Some(KoCtx::Isolated { level }) = self.ko.as_mut().and_then(|k| k.stack.pop()) else {
            return Err(unsupported("knockout_scratch"));
        };
        let ko = self.ko_buffers()?;
        let outer = ko.nearest_isolated();
        let flags = if outer.is_some() { KO_OUTER } else { 0 } | if self.extended { KO_EXTENDED } else { 0 };
        let full = Rect {
            x: chunk.x,
            y: chunk.y,
            width: chunk.width,
            height: chunk.height,
        };
        self.ko_dispatch(KoPass::Clear, full, chunk, parent, 1.0, flags, level, outer)
    }

    /// Params, `Ko` and one mix or clear dispatch over `region` into `levels[level]` with the shape
    /// plane `k_level` (`0` = the layer shape buffer) and the accumulated plane of `acc`.
    #[allow(clippy::too_many_arguments)]
    fn ko_dispatch(
        &mut self,
        pass: KoPass,
        region: Rect,
        chunk: Rect,
        level: usize,
        layer_opacity: f32,
        flags: u32,
        k_level: usize,
        acc: Option<usize>,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let params = LayerParams {
            chunk_w: chunk.width,
            chunk_h: chunk.height,
            region: [offset(region.x, chunk.x)?, offset(region.y, chunk.y)?, region.width, region.height],
            mode: StudioBlendMode::Normal,
            source: SourceKind::Chunk,
            has_mask: false,
            opacity: 1.0,
            unfilled: 1.0,
            fill: 1.0,
            replace: false,
            clip: false,
            opaque_source: false,
            mix: false,
            density: 1.0,
            extended: false,
        };
        ctx.queue.write_buffer(&self.params, 0, &params.to_bytes());
        let ko = self.ko_buffers()?;
        ctx.queue.write_buffer(&ko.params, 0, &ko_params_bytes(layer_opacity, flags));
        let dst = self.levels.get(level).ok_or_else(|| unsupported("plan_shape"))?;
        let k = if k_level == 0 { &ko.k } else { ko.plane(k_level)? };
        let acc = match acc {
            Some(l) => ko.plane(l)?,
            None => &ko.none,
        };
        let bind = ctx.kernel.bind_ko_mix(&ctx.device, &self.params, dst, &ko.params, &ko.s, k, acc);
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hsk-studio-render-gpu knockout"),
        });
        ctx.kernel.dispatch_ko(pass, &mut encoder, &bind, region.width, region.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }
}

/// A knockout layer composited into `levels[level]` (see the module doc).
pub(super) fn eval_knockout_layer(
    ex: &mut Exec<'_>,
    prep: &Prepared<'_>,
    layer: &PLayer<'_>,
    chunk: Rect,
    level: usize,
) -> Result<(), GpuError> {
    ex.check()?;
    // The shape carries the layer opacity: none at 0 (fill 0 still knocks out).
    if layer.layer_opacity <= 0.0 {
        return Ok(());
    }
    let Some(region) = prep.bound(layer.input).map(|b| intersect(b, chunk)).transpose()?.flatten() else {
        return Ok(());
    };
    let src = match prep.node(layer.input)? {
        PNode::Group { isolated: true, .. } => {
            let below = level.saturating_add(1);
            ex.clear(below, texel_bytes(chunk.width, chunk.height));
            eval_group(ex, prep, layer.input, chunk, below)?;
            Src::Chunk
        }
        PNode::Tile { planes, rect } => Src::Tile(planes, *rect),
        PNode::Transformed { planes, prep: t } => Src::Transformed(planes, t),
        PNode::Solid { rgba } if rgba[3] <= 0.0 => return Ok(()),
        PNode::Solid { rgba } => Src::Solid(*rgba),
        PNode::Group { isolated: false, .. } | PNode::Unused => {
            return Err(unsupported(ExtOperator::Knockout(Knockout::Shallow).key()));
        }
    };
    // The initial backdrop into S: the start snapshot of the layer's pass-through group when it is
    // being evaluated (shallow), else transparent (an isolated group's start, the root's base, and
    // the document base of a deep knockout).
    let ctx = ex.ctx;
    let bytes = texel_bytes(chunk.width, chunk.height);
    let ko = ex.ko_buffers()?;
    let snapshot = match layer.knockout {
        Knockout::Shallow => ko.stack.iter().rev().find_map(|c| match c {
            KoCtx::Snapshot { slot, group } if *group == layer.group => Some(*slot),
            _ => None,
        }),
        _ => None,
    };
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("hsk-studio-render-gpu knockout initial"),
    });
    match snapshot {
        Some(slot) => {
            let from = ko.snapshots.get(slot).ok_or_else(|| unsupported("knockout_scratch"))?;
            encoder.copy_buffer_to_buffer(from, 0, &ko.s, 0, bytes);
        }
        None => encoder.clear_buffer(&ko.s, 0, Some(bytes)),
    }
    ctx.queue.submit(Some(encoder.finish()));
    let record = match layer.knockout {
        Knockout::Deep => ko.nearest_isolated(),
        _ => None,
    };
    // knockout_s: the source staged like a composite source (tile region, resampled region,
    // finished group buffer or solid), the mask uploaded over the region.
    let kind = ex.stage(src, region)?;
    if let Some(mask) = layer.mask.as_ref() {
        ex.upload_mask(mask, region);
    }
    let params = LayerParams {
        chunk_w: chunk.width,
        chunk_h: chunk.height,
        region: [offset(region.x, chunk.x)?, offset(region.y, chunk.y)?, region.width, region.height],
        mode: layer.mode,
        source: kind,
        has_mask: layer.mask.is_some(),
        opacity: layer.opacity,
        // knockout_over folds fill as a product (pigment knockout_math)
        unfilled: layer.opacity,
        fill: 1.0,
        replace: false,
        clip: false,
        opaque_source: false,
        mix: false,
        density: layer.mask.as_ref().map_or(1.0, |m| m.density),
        extended: false,
    };
    ctx.queue.write_buffer(&ex.params, 0, &params.to_bytes());
    let extended = if ex.extended { KO_EXTENDED } else { 0 };
    let ko = ex.ko_buffers()?;
    ctx.queue.write_buffer(&ko.params, 0, &ko_params_bytes(layer.layer_opacity, extended));
    let source = match kind {
        SourceKind::Chunk => ex.levels.get(level.saturating_add(1)).ok_or_else(|| unsupported("plan_shape"))?,
        _ => &ex.src,
    };
    let bind = ctx.kernel.bind_ko_s(&ctx.device, &ex.params, source, &ex.mask, &ko.params, &ko.s, &ko.k);
    let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("hsk-studio-render-gpu knockout s"),
    });
    ctx.kernel.dispatch_ko(KoPass::S, &mut encoder, &bind, region.width, region.height);
    ctx.queue.submit(Some(encoder.finish()));
    ex.stats.dispatches = ex.stats.dispatches.saturating_add(1);
    // knockout_mix: lerp toward S by the shape; a deep knockout records into the nearest enclosing
    // isolated plane (none at the root level).
    let flags = if record.is_some() { KO_RECORD } else { 0 } | extended;
    ex.ko_dispatch(KoPass::Mix, region, chunk, level, layer.layer_opacity, flags, 0, record)
}
