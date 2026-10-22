//! Align, distribute and the transform tools in the viewer (lane SMB-ILLUSTRATOR-TOOLS-2;
//! STU-VEC-029, STU-VEC-157, STU-VEC-166). Nib computes the geometry (`nib::arrange`); every
//! command moves the selected vector layers' path records (anchor positions by the affine,
//! handles and network tangents by its linear part, mesh-envelope points too) into fresh `SVPT`
//! records and commits all layers' `vector_paths` and the entry in ONE undo step
//! (`Run::set_vector_paths_and_entry`). A transformed live shape is no longer live (its new path
//! has no live record).
//!
//! Bounds are each layer's exact path bounds (nib `path_bounds`, all of its paths); the artboard
//! target is the document's first artboard. Rotate, scale, reflect and shear act about the
//! reference point of the selection's union bounds; transform-each about each layer's own.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::{ENTRY, PathRecord, VectorEntry, mint_path_id};
use hsk_studio_folio::{self as folio, Payload};
use hsk_studio_nib::PathPoint;
use hsk_studio_nib::anchor_path::AnchorPath;
pub use hsk_studio_nib::arrange::{Align, Bounds, Distribute, ReferencePoint, TransformEach};
use hsk_studio_nib::arrange::{self as nib_arrange, AlignTo};
use hsk_studio_nib::raster::Affine;

/// Align / distribute target (STU-VEC-166 `align_to`).
#[derive(Clone, Debug, PartialEq)]
pub enum AlignTarget {
    Selection,
    KeyObject(String),
    Artboard,
}

/// A transform tool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformOp {
    /// Degrees, clockwise on screen.
    Rotate(f64),
    /// Percent.
    Scale { horizontal: f64, vertical: f64 },
    /// Mirror across the axis at `axis_deg` from the horizontal.
    Reflect { axis_deg: f64 },
    /// Shear by `angle_deg` along the axis at `axis_deg`.
    Shear { angle_deg: f64, axis_deg: f64 },
    /// Document units.
    Move { dx: f64, dy: f64 },
}

fn point(m: &Affine, q: PathPoint) -> PathPoint {
    PathPoint::new(m[0] * q.x + m[2] * q.y + m[4], m[1] * q.x + m[3] * q.y + m[5])
}
fn vector(m: &Affine, q: PathPoint) -> PathPoint {
    PathPoint::new(m[0] * q.x + m[2] * q.y, m[1] * q.x + m[3] * q.y)
}

/// The anchor path under `m` (positions by the affine, handles by its linear part).
pub fn transform_anchor_path(p: &AnchorPath, m: &Affine) -> AnchorPath {
    let mut out = p.clone();
    for c in &mut out.contours {
        for a in &mut c.anchors {
            a.position = point(m, a.position);
            a.incoming = vector(m, a.incoming);
            a.outgoing = vector(m, a.outgoing);
        }
    }
    out
}

/// The network record under `m` (vertices by the affine, tangents by its linear part).
pub fn transform_network(n: &folio::StudioVectorNetwork, m: &Affine) -> folio::StudioVectorNetwork {
    let mut out = n.clone();
    for v in &mut out.vertices {
        let q = point(m, PathPoint::new(v.x, v.y));
        (v.x, v.y) = (q.x, q.y);
    }
    for s in &mut out.segments {
        for t in [&mut s.tangent_start, &mut s.tangent_end].into_iter().flatten() {
            let q = vector(m, PathPoint::new(t[0], t[1]));
            *t = [q.x, q.y];
        }
    }
    out
}

impl Viewer {
    /// The exact bounds of vector layer `layer_id` (all of its paths).
    pub fn layer_bounds(&self, layer_id: &str) -> Result<Bounds, String> {
        let entry = self.vector_entry()?;
        bounds_of(self.vdoc()?, &entry, layer_id)
    }

    /// Aligns the layers' edges or centres to the target (STU-VEC-166): ONE undo step.
    pub fn align_layers(&mut self, layers: &[String], to: &AlignTarget, how: Align) -> Result<(), String> {
        let r = (|| {
            let (bounds, target) = self.targets(layers, to)?;
            let moves = nib_arrange::align(&bounds, target, how).map_err(|e| format!("align: {e:?}"))?;
            self.apply_affines("align", layers, &moves.iter().map(|d| [1.0, 0.0, 0.0, 1.0, d[0], d[1]]).collect::<Vec<_>>())
        })();
        self.report(r)
    }

    /// Distributes the layers (edges, centres or spacing; STU-VEC-166): ONE undo step.
    pub fn distribute_layers(&mut self, layers: &[String], to: &AlignTarget, how: Distribute) -> Result<(), String> {
        let r = (|| {
            let (bounds, target) = self.targets(layers, to)?;
            let moves = nib_arrange::distribute(&bounds, target, how).map_err(|e| format!("distribute: {e:?}"))?;
            self.apply_affines("distribute", layers, &moves.iter().map(|d| [1.0, 0.0, 0.0, 1.0, d[0], d[1]]).collect::<Vec<_>>())
        })();
        self.report(r)
    }

    /// Rotates, scales, reflects, shears or moves the layers about `reference` of their union
    /// bounds: ONE undo step.
    pub fn transform_layers(&mut self, layers: &[String], op: TransformOp, reference: ReferencePoint) -> Result<(), String> {
        let r = (|| {
            let entry = self.vector_entry()?;
            let doc = self.vdoc()?;
            let union = layers.iter().map(|l| bounds_of(doc, &entry, l)).collect::<Result<Vec<_>, _>>()?.into_iter().reduce(|a, b| a.union(&b)).ok_or("transform: no layers")?;
            let c = reference.of(&union);
            let code = |e: hsk_studio_nib::Error| format!("transform: {e:?}");
            let m = match op {
                TransformOp::Rotate(deg) => nib_arrange::rotate_about(c, deg).map_err(code)?,
                TransformOp::Scale { horizontal, vertical } => {
                    if ![horizontal, vertical].iter().all(|v| v.is_finite() && (-4000.0..=4000.0).contains(v) && *v != 0.0) {
                        return Err("transform: scale percent".into());
                    }
                    nib_arrange::scale_about(c, horizontal / 100.0, vertical / 100.0).map_err(code)?
                }
                TransformOp::Reflect { axis_deg } => nib_arrange::reflect_about(c, axis_deg).map_err(code)?,
                TransformOp::Shear { angle_deg, axis_deg } => nib_arrange::shear_about(c, angle_deg, axis_deg).map_err(code)?,
                TransformOp::Move { dx, dy } => {
                    if !(dx.is_finite() && dy.is_finite()) {
                        return Err("transform: move".into());
                    }
                    [1.0, 0.0, 0.0, 1.0, dx, dy]
                }
            };
            self.apply_affines("transform", layers, &vec![m; layers.len()])
        })();
        self.report(r)
    }

    /// Transform-each (STU-VEC-157): every layer about its own reference point: ONE undo step.
    pub fn transform_each_layers(&mut self, layers: &[String], p: &TransformEach) -> Result<(), String> {
        let r = (|| {
            let entry = self.vector_entry()?;
            let doc = self.vdoc()?;
            let bounds = layers.iter().map(|l| bounds_of(doc, &entry, l)).collect::<Result<Vec<_>, _>>()?;
            let mats = nib_arrange::transform_each(&bounds, p).map_err(|e| format!("transform_each: {e:?}"))?;
            self.apply_affines("transform_each", layers, &mats)
        })();
        self.report(r)
    }

    fn targets(&self, layers: &[String], to: &AlignTarget) -> Result<(Vec<Bounds>, AlignTo), String> {
        let entry = self.vector_entry()?;
        let doc = self.vdoc()?;
        let bounds = layers.iter().map(|l| bounds_of(doc, &entry, l)).collect::<Result<Vec<_>, _>>()?;
        let target = match to {
            AlignTarget::Selection => AlignTo::Selection,
            AlignTarget::KeyObject(k) => AlignTo::KeyObject(layers.iter().position(|l| l == k).ok_or("align: the key object is not among the layers")?),
            AlignTarget::Artboard => {
                let b = &doc.artboards.first().ok_or("align: the document has no artboard")?.bounds;
                AlignTo::Artboard(Bounds { min: [b.x.value, b.y.value], max: [b.x.value + b.width.value, b.y.value + b.height.value] })
            }
        };
        Ok((bounds, target))
    }

    /// Moves every layer's path records by its affine into fresh records; ONE undo step.
    pub(super) fn apply_affines(&mut self, op: &str, layers: &[String], mats: &[Affine]) -> Result<(), String> {
        if layers.len() != mats.len() || mats.iter().any(|m| m.iter().any(|v| !v.is_finite())) {
            return Err(format!("{op}: transforms"));
        }
        let doc = self.vdoc()?.clone();
        let mut entry = self.vector_entry()?;
        let mut writes = Vec::with_capacity(layers.len());
        for (layer_id, m) in layers.iter().zip(mats) {
            let layer = doc.layers.iter().find(|l| &l.layer_id == layer_id).ok_or_else(|| format!("{op}: no layer {layer_id}"))?;
            let Payload::Vector { path_ids } = &layer.payload else {
                return Err(format!("{op}: {layer_id} is not a vector layer"));
            };
            let mut ids = Vec::with_capacity(path_ids.len());
            for id in path_ids {
                let record = entry.path(id).cloned().ok_or_else(|| format!("{op}: vector_path_unresolved {id}"))?;
                if entry.paths.len() >= super::MAX_PATHS {
                    return Err(format!("{op}: vector_entry_over_budget"));
                }
                let new_id = mint_path_id(&doc, &entry);
                let path = transform_anchor_path(&record.path, m);
                path.validate().map_err(|e| format!("{op}: {}", e.code()))?;
                let network = record.network.as_ref().map(|n| {
                    let mut t = transform_network(n, m);
                    t.path_id.clone_from(&new_id);
                    t
                });
                if let Some(n) = &network {
                    folio::validate_vector_network(n).map_err(|d| format!("{op}: network {:?} {}", d.code, d.target))?;
                }
                entry.paths.push(PathRecord { path_id: new_id.clone(), path, style: record.style, network, width: record.width.clone() });
                ids.push(new_id);
            }
            for e in entry.records.envelopes.iter_mut().filter(|e| &e.layer_id == layer_id) {
                if let folio::StudioEnvelopeShape::Mesh { points, .. } = &mut e.envelope {
                    for q in points.iter_mut() {
                        let t = point(m, PathPoint::new(q[0], q[1]));
                        *q = [t.x, t.y];
                    }
                }
            }
            writes.push((layer_id.clone(), ids));
        }
        self.commit_paths(op, &writes, &entry)
    }

    /// Commits the layers' new path ids and `entry` as ONE undo step.
    pub(super) fn commit_paths(&mut self, op: &str, writes: &[(String, Vec<String>)], entry: &VectorEntry) -> Result<(), String> {
        if !super::entry_persistable() {
            return Err(format!("{op}: vector_entry_not_persistable"));
        }
        let doc = self.vdoc()?;
        entry.validate(doc, self.cancel)?;
        let bytes = entry.to_bytes(&doc.document_id)?;
        let ok = self.run.set_vector_paths_and_entry(op, writes, Some((ENTRY, bytes)));
        self.after_edit(ok, format!("{op} {} layers", writes.len())).map(|_| ())
    }
}

/// Union bounds of a vector layer's paths.
pub(super) fn bounds_of(doc: &folio::StudioDocument, entry: &VectorEntry, layer_id: &str) -> Result<Bounds, String> {
    let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("bounds: no layer {layer_id}"))?;
    let Payload::Vector { path_ids } = &layer.payload else {
        return Err(format!("bounds: {layer_id} is not a vector layer"));
    };
    let mut out: Option<Bounds> = None;
    for id in path_ids {
        let record = entry.path(id).ok_or_else(|| format!("bounds: vector_path_unresolved {id}"))?;
        let owned = record.path.to_owned_path().map_err(|e| format!("bounds: {}", e.code()))?;
        if let Some(b) = nib_arrange::path_bounds(&owned) {
            out = Some(out.map_or(b, |o| o.union(&b)));
        }
    }
    out.ok_or_else(|| format!("bounds: {layer_id} has no geometry"))
}
