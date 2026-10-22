//! Envelope distort and warp in the viewer (lane SMB-ILLUSTRATOR-TOOLS-2; STU-VEC-055, STU-VEC-168,
//! STU-VEC-169). An envelope is a Folio `StudioEnvelope` in the entry's vector records; composite
//! draws it live through nib (the layer's source geometry stays editable: edit-contents is every
//! ordinary path tool). Make (warp preset, mesh grid over the layer bounds, or top object: that
//! layer is consumed and drawn only as the envelope), move a mesh point, release (the record goes;
//! a top object draws again) and expand (the wrapped geometry becomes the layer's path, the record
//! goes) are each ONE undo step. Warp percentages are authored and stored as ratios (/ 100).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::arrange::bounds_of;
use super::{PathRecord, mint_path_id};
use hsk_studio_folio::{self as folio, Payload, StudioEnvelope, StudioEnvelopeShape, StudioWarpAxis, StudioWarpStyle};
use hsk_studio_nib::anchor_path::AnchorPath;
use hsk_studio_nib::warp::{Envelope, Mesh, Warp, WarpAxis, WarpStyle, envelope_paths};
use hsk_studio_nib::{OwnedPath, PathPoint};

/// What an envelope is made from.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvelopeSpec {
    /// Bend and distortions in percent (-100..=100).
    Warp { style: StudioWarpStyle, axis: StudioWarpAxis, bend: f64, horizontal: f64, vertical: f64 },
    /// A mesh grid over the layer bounds (1..=64 rows and columns).
    Mesh { rows: u32, columns: u32 },
    /// Another vector layer's outline.
    TopObject { layer: String },
}

fn nib_envelope(e: &StudioEnvelope, top: impl Fn(&str) -> Result<OwnedPath, String>) -> Result<Envelope, String> {
    Ok(match &e.envelope {
        StudioEnvelopeShape::Warp { warp_style, warp_axis, warp_bend, warp_distortion_horizontal, warp_distortion_vertical } => Envelope::Warp(Warp {
            style: WarpStyle::ALL[StudioWarpStyle::ALL.iter().position(|s| s == warp_style).unwrap_or(0)],
            axis: if *warp_axis == StudioWarpAxis::Vertical { WarpAxis::Vertical } else { WarpAxis::Horizontal },
            bend: *warp_bend,
            distortion_horizontal: *warp_distortion_horizontal,
            distortion_vertical: *warp_distortion_vertical,
        }),
        StudioEnvelopeShape::Mesh { rows, columns, points } => Envelope::Mesh(Mesh {
            rows: usize::try_from(*rows).map_err(|_| "envelope: rows")?,
            columns: usize::try_from(*columns).map_err(|_| "envelope: columns")?,
            points: points.iter().map(|q| PathPoint::new(q[0], q[1])).collect(),
        }),
        StudioEnvelopeShape::TopObject { source_layer_id } => Envelope::Shape(top(source_layer_id)?),
    })
}

/// The fifteen styles in nib order.
trait AllStyles {
    const ALL: [StudioWarpStyle; 15];
}
impl AllStyles for StudioWarpStyle {
    const ALL: [StudioWarpStyle; 15] = [
        Self::Arc, Self::ArcLower, Self::ArcUpper, Self::Arch, Self::Bulge, Self::ShellLower, Self::ShellUpper, Self::Flag, Self::Wave, Self::Fish, Self::Rise, Self::Fisheye, Self::Inflate, Self::Squeeze, Self::Twist,
    ];
}

impl Viewer {
    /// The envelope of `layer_id`, if any.
    pub fn envelope(&self, layer_id: &str) -> Result<Option<StudioEnvelope>, String> {
        Ok(self.vector_entry()?.records.envelope(layer_id).cloned())
    }

    /// Wraps vector layer `layer_id` in an envelope (replacing its envelope): ONE undo step.
    pub fn make_envelope(&mut self, layer_id: &str, spec: &EnvelopeSpec, fidelity: f64, distort_appearance: bool) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let envelope = match spec {
                EnvelopeSpec::Warp { style, axis, bend, horizontal, vertical } => {
                    if ![bend, horizontal, vertical].iter().all(|v| v.is_finite() && (-100.0..=100.0).contains(*v)) {
                        return Err("envelope: warp percent".into());
                    }
                    StudioEnvelopeShape::Warp { warp_style: *style, warp_axis: *axis, warp_bend: bend / 100.0, warp_distortion_horizontal: horizontal / 100.0, warp_distortion_vertical: vertical / 100.0 }
                }
                EnvelopeSpec::Mesh { rows, columns } => {
                    let b = bounds_of(doc, &entry, layer_id)?;
                    let grid = Mesh::grid(&b, usize::try_from(*rows).map_err(|_| "envelope: rows")?, usize::try_from(*columns).map_err(|_| "envelope: columns")?).map_err(|e| format!("envelope: {e:?}"))?;
                    StudioEnvelopeShape::Mesh { rows: *rows, columns: *columns, points: grid.points.iter().map(|q| [q.x, q.y]).collect() }
                }
                EnvelopeSpec::TopObject { layer } => {
                    let vector = doc.layers.iter().any(|l| &l.layer_id == layer && matches!(l.payload, Payload::Vector { .. }));
                    if !vector || layer == layer_id || entry.records.envelope(layer).is_some() {
                        return Err(format!("envelope: {layer} is not an unwrapped vector layer"));
                    }
                    StudioEnvelopeShape::TopObject { source_layer_id: layer.clone() }
                }
            };
            entry.records.envelopes.retain(|e| e.layer_id != layer_id);
            entry.records.envelopes.push(StudioEnvelope {
                layer_id: layer_id.to_string(),
                envelope,
                envelope_fidelity: fidelity,
                envelope_raster_mode: folio::StudioEnvelopeRaster::ClippingMask,
                anti_alias: false,
                distort_appearance,
                distort_linear_gradient_fills: false,
                distort_pattern_fills: false,
            });
            self.validate_records(&entry)?;
            self.commit_entry("make_envelope", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Moves mesh point `index` (row-major) of `layer_id`'s mesh envelope to `to`: ONE undo step.
    pub fn move_envelope_point(&mut self, layer_id: &str, index: usize, to: [f64; 2]) -> Result<String, String> {
        let r = (|| {
            if !to.iter().all(|v| v.is_finite()) {
                return Err("envelope: non_finite".into());
            }
            let mut entry = self.vector_entry()?;
            let e = entry.records.envelopes.iter_mut().find(|e| e.layer_id == layer_id).ok_or_else(|| format!("envelope: {layer_id} has none"))?;
            let StudioEnvelopeShape::Mesh { points, .. } = &mut e.envelope else {
                return Err("envelope: not a mesh".into());
            };
            *points.get_mut(index).ok_or("envelope: point index")? = to;
            self.commit_entry("move_envelope_point", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Removes `layer_id`'s envelope (the source geometry draws as it is): ONE undo step.
    pub fn release_envelope(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let mut entry = self.vector_entry()?;
            let before = entry.records.envelopes.len();
            entry.records.envelopes.retain(|e| e.layer_id != layer_id);
            if entry.records.envelopes.len() == before {
                return Err(format!("envelope: {layer_id} has none"));
            }
            self.commit_entry("release_envelope", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Bakes `layer_id`'s envelope into its path geometry and removes it: ONE undo step. Networks
    /// and `distort_appearance` envelopes are refused (declared gap).
    pub fn expand_envelope(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?.clone();
            let mut entry = self.vector_entry()?;
            let e = entry.records.envelope(layer_id).cloned().ok_or_else(|| format!("envelope: {layer_id} has none"))?;
            if e.distort_appearance {
                return Err("expand_envelope: distort_appearance envelopes are not expanded (gap)".into());
            }
            let Some(Payload::Vector { path_ids }) = doc.layers.iter().find(|l| l.layer_id == layer_id).map(|l| &l.payload) else {
                return Err(format!("expand_envelope: {layer_id} is not a vector layer"));
            };
            let records: Vec<PathRecord> = path_ids.iter().map(|id| entry.path(id).cloned().ok_or_else(|| format!("expand_envelope: vector_path_unresolved {id}"))).collect::<Result<_, _>>()?;
            if records.iter().any(|r| r.network.is_some()) {
                return Err("expand_envelope: network layers keep their envelope (gap)".into());
            }
            let owned: Vec<OwnedPath> = records.iter().map(|r| r.path.to_owned_path().map_err(|e| format!("expand_envelope: {}", e.code()))).collect::<Result<_, _>>()?;
            let top = |id: &str| -> Result<OwnedPath, String> {
                let Some(Payload::Vector { path_ids }) = doc.layers.iter().find(|l| l.layer_id == id).map(|l| &l.payload) else {
                    return Err("expand_envelope: top object".into());
                };
                let first = path_ids.first().and_then(|p| entry.path(p)).ok_or("expand_envelope: top object path")?;
                first.path.to_owned_path().map_err(|e| format!("expand_envelope: {}", e.code()))
            };
            let env = nib_envelope(&e, top)?;
            let wrapped = envelope_paths(&owned, &env, e.envelope_fidelity).map_err(|err| format!("expand_envelope: {err:?}"))?;
            let mut ids = Vec::with_capacity(wrapped.len());
            for (record, path) in records.iter().zip(wrapped) {
                let anchor = AnchorPath::from_owned_path(&path).map_err(|err| format!("expand_envelope: {}", err.code()))?;
                let id = mint_path_id(&doc, &entry);
                entry.paths.push(PathRecord { path_id: id.clone(), path: anchor, style: record.style, network: None, width: record.width.clone() });
                ids.push(id);
            }
            entry.records.envelopes.retain(|x| x.layer_id != layer_id);
            self.commit_paths("expand_envelope", &[(layer_id.to_string(), ids)], &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }
}
