//! Static evaluation of a lowered plan against the immutable snapshot it was lowered from.
//! Same semantics for previews and export: extents come from authored tile records only, hidden
//! layers never contribute to a parent, and anything without a verified route (vector/text to
//! image, unresolved or unsupported payloads, unassigned profiles) is reported `Unavailable`,
//! never faked, skipped or treated as Normal.
use crate::{
    CompositeError,
    blend::{Applicability, BlendError, BlendReceipt},
    lower_plan::{LoweredPlan, NodeAddr, SourceValue, Step},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Length, Payload, ProfileBinding, Resolution, Snapshot, TileRef};
use hsk_studio_pigment::{Grid, Rect};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Ready,
    Unavailable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepEval {
    pub addr: NodeAddr,
    pub visible: bool,
    /// Bounding union of tile / visible-child rectangles in pixels; `None` when empty or unavailable.
    pub extent: Option<Rect>,
    pub status: StepStatus,
}

/// A document-level or step-level reason the render cannot be complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub target: String,
    pub reason: String,
}

/// Receipt of a static evaluation (STU-ARC-015 shape, minimal): revision, declared blend space,
/// whether the blending profile is bound, and the lowest operator fidelity class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderReceipt {
    pub revision: u64,
    pub blend_profile_bound: bool,
    pub blend: BlendReceipt,
    /// Always true: this evaluator never pulls temporal, audio or native providers (STU-CMP-093).
    pub static_closure: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalReport {
    pub steps: Vec<StepEval>,
    pub unavailable: Vec<Unavailable>,
    pub receipt: RenderReceipt,
}

impl EvalReport {
    /// `Ok` only when nothing is unavailable; otherwise the first reason as a typed error.
    pub fn require_ready(&self) -> Result<(), CompositeError> {
        if let Some(u) = self.unavailable.first() {
            return Err(CompositeError::Unavailable(format!("{}:{}", u.target, u.reason)));
        }
        for s in &self.steps {
            if let StepStatus::Unavailable(reason) = &s.status {
                return Err(CompositeError::Unavailable(format!("{}:{reason}", s.addr.text())));
            }
        }
        Ok(())
    }
    pub fn step(&self, addr: &NodeAddr) -> Option<&StepEval> {
        self.steps.iter().find(|s| &s.addr == addr)
    }
}

fn px(l: &Length) -> Option<u32> {
    let v = l.value;
    if v.is_finite() && v >= 1.0 && v.fract() == 0.0 && v <= f64::from(u32::MAX) {
        Some(v as u32)
    } else {
        None
    }
}

fn union(current: Option<Rect>, next: Rect) -> Result<Option<Rect>, CompositeError> {
    let Some(a) = current else {
        return Ok(Some(next));
    };
    let (ax, ay) = a.end().map_err(|_| CompositeError::Overflow)?;
    let (bx, by) = next.end().map_err(|_| CompositeError::Overflow)?;
    let x = a.x.min(next.x);
    let y = a.y.min(next.y);
    let width = ax.max(bx).checked_sub(x).and_then(|w| u32::try_from(w).ok());
    let height = ay.max(by).checked_sub(y).and_then(|h| u32::try_from(h).ok());
    match (width, height) {
        (Some(width), Some(height)) => Ok(Some(Rect {
            x,
            y,
            width,
            height,
        })),
        _ => Err(CompositeError::Overflow),
    }
}

/// Tile (column, row) lies on a grid of the layer's largest tile size; each tile keeps its own size.
fn raster_extent(tiles: &[TileRef]) -> Result<Option<Rect>, CompositeError> {
    let mut grid_w = 0u32;
    let mut grid_h = 0u32;
    for t in tiles {
        let (Some(w), Some(h)) = (px(&t.width), px(&t.height)) else {
            return Err(CompositeError::UnsupportedDescriptor("tile_size".into()));
        };
        grid_w = grid_w.max(w);
        grid_h = grid_h.max(h);
    }
    let grid = Grid {
        origin_x: 0,
        origin_y: 0,
        tile_width: grid_w,
        tile_height: grid_h,
    };
    let mut extent = None;
    for t in tiles {
        let (Some(w), Some(h)) = (px(&t.width), px(&t.height)) else {
            return Err(CompositeError::UnsupportedDescriptor("tile_size".into()));
        };
        let cell = grid
            .bounds(t.column, t.row)
            .map_err(|_| CompositeError::Overflow)?;
        extent = union(
            extent,
            Rect {
                x: cell.x,
                y: cell.y,
                width: w,
                height: h,
            },
        )?;
    }
    Ok(extent)
}

fn resolution_key(r: Resolution) -> &'static str {
    match r {
        Resolution::Available => "available",
        Resolution::Absent => "absent",
        Resolution::HashMismatch => "hash_mismatch",
        Resolution::Unauthorized => "unauthorized",
        Resolution::Unsupported => "unsupported",
    }
}

/// Evaluate `plan` against `snap`. Fails with `StaleRevision` when the plan and snapshot revisions
/// differ, `Cycle` / `DanglingInput` when the (possibly hand-built) plan is not a topologically
/// ordered closure, and `Canceled` when the token fires (checked per step).
pub fn evaluate(
    plan: &LoweredPlan,
    snap: &Snapshot,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    cancel.check().map_err(|_| CompositeError::Canceled)?;
    let doc = snap.document();
    if plan.revision != doc.revision {
        return Err(CompositeError::StaleRevision {
            expected: plan.revision,
            actual: doc.revision,
        });
    }
    let layers: BTreeMap<&str, &hsk_studio_folio::StudioLayer> =
        doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let all: BTreeSet<&NodeAddr> = plan.steps.iter().map(Step::addr).collect();
    if all.len() != plan.steps.len() {
        return Err(CompositeError::UnsupportedDescriptor("duplicate_step".into()));
    }

    let mut blend = BlendReceipt::new(plan.blend_space);
    let mut evals: Vec<StepEval> = Vec::with_capacity(plan.steps.len());
    let mut seen: BTreeMap<&NodeAddr, usize> = BTreeMap::new();
    for step in &plan.steps {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let eval = match step {
            Step::Source {
                addr,
                value,
                visible,
                ..
            } => {
                blend.record("source", step.fidelity());
                let payload = layers.get(addr.layer_id.as_str()).map(|l| &l.payload);
                let (extent, status) = match (value, payload) {
                    (SourceValue::Image, Some(Payload::Raster { tiles })) => {
                        (raster_extent(tiles)?, StepStatus::Ready)
                    }
                    (SourceValue::Vector, _) => (
                        None,
                        StepStatus::Unavailable("vector_to_image_conversion".into()),
                    ),
                    (SourceValue::Text, _) => (
                        None,
                        StepStatus::Unavailable("text_to_image_conversion".into()),
                    ),
                    _ => (None, StepStatus::Unavailable("unsupported_payload".into())),
                };
                StepEval {
                    addr: addr.clone(),
                    visible: *visible,
                    extent,
                    status,
                }
            }
            Step::Composite {
                addr,
                inputs,
                visible,
                mode,
                ..
            } => {
                let info = mode.info();
                match info.applicability {
                    Applicability::Layer | Applicability::GroupOnly => {}
                    Applicability::ToolOnly => {
                        return Err(BlendError::ToolOnly(*mode).into());
                    }
                    Applicability::Unspecified => {
                        return Err(BlendError::Unsupported(*mode).into());
                    }
                }
                blend.record(info.key, info.fidelity);
                let mut extent = None;
                let mut blocked = None;
                for input in inputs {
                    let Some(&i) = seen.get(input) else {
                        return Err(if all.contains(input) {
                            CompositeError::Cycle(input.text())
                        } else {
                            CompositeError::DanglingInput(input.text())
                        });
                    };
                    let child = &evals[i];
                    if !child.visible {
                        continue;
                    }
                    match &child.status {
                        StepStatus::Ready => {
                            if let Some(e) = child.extent {
                                extent = union(extent, e)?;
                            }
                        }
                        StepStatus::Unavailable(_) => {
                            blocked.get_or_insert_with(|| child.addr.text());
                        }
                    }
                }
                match blocked {
                    Some(child) => StepEval {
                        addr: addr.clone(),
                        visible: *visible,
                        extent: None,
                        status: StepStatus::Unavailable(format!("input_unavailable:{child}")),
                    },
                    None => StepEval {
                        addr: addr.clone(),
                        visible: *visible,
                        extent,
                        status: StepStatus::Ready,
                    },
                }
            }
        };
        seen.insert(step.addr(), evals.len());
        evals.push(eval);
    }

    let mut unavailable = Vec::new();
    for issue in snap.resolution_issues() {
        unavailable.push(Unavailable {
            target: issue.target.clone(),
            reason: resolution_key(issue.disposition).into(),
        });
    }
    for (name, binding) in [
        ("working_space_rgb", &doc.working_space_rgb),
        ("working_space_cmyk", &doc.working_space_cmyk),
        ("working_space_gray", &doc.working_space_gray),
        ("working_space_spot", &doc.working_space_spot),
        ("blending_space", &doc.blending_space),
    ] {
        if matches!(binding, ProfileBinding::Unassigned(())) {
            unavailable.push(Unavailable {
                target: name.into(),
                reason: "unassigned_profile".into(),
            });
        }
    }
    Ok(EvalReport {
        steps: evals,
        unavailable,
        receipt: RenderReceipt {
            revision: plan.revision,
            blend_profile_bound: plan.blend_profile_bound,
            blend,
            static_closure: true,
        },
    })
}
