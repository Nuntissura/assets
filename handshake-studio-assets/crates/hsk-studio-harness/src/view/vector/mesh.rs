//! Gradient meshes in the viewer (lane SMB-AI-TYPE-PATH-RECOLOR; STU-VEC-034, STU-COL-036,
//! STU-VEC-059). A mesh is a Folio `StudioGradientMesh` record of the entry's vector records (one
//! per vector layer, kept with the document); Nib owns its geometry (`hsk_studio_nib::mesh`),
//! render-cpu rasterises it ([`Viewer::gradient_mesh_raster`]).
//!
//! [`Viewer::create_gradient_mesh`] builds a `rows x columns` mesh conforming to the layer's path
//! (Nib `GradientMesh::from_path`; the source path is recorded), every point the layer's first
//! solid fill colour (white without one). [`Viewer::edit_gradient_mesh`] sets a point's colour and
//! opacity, moves a point, inserts or removes a mesh line. Each command is ONE undo step. Declared
//! gaps: the composite view pipeline does not draw meshes yet (the raster is a separate preview
//! surface), PDF/SVG export writes no mesh, conversion from a linear/radial gradient is not
//! written, and the GPU renderer has no mesh path. Illustrator parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::current_rows;
use hsk_studio_folio::{self as folio, StudioAppearanceRow, StudioPaint};
use hsk_studio_nib::mesh::{GradientMesh, MeshLimits, MeshPoint};
use hsk_studio_render_cpu::mesh::{MeshFrame, MeshRaster, MeshRasterLimits, rasterize_gradient_mesh};

/// One mesh edit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeshEdit {
    /// Colour (encoded RGB `0..=1`) and opacity percent of point `(row, column)`.
    SetPoint { row: u32, column: u32, rgb: [f64; 3], opacity: f64 },
    MovePoint { row: u32, column: u32, dx: f64, dy: f64 },
    InsertColumn { column: u32, at: f64 },
    InsertRow { row: u32, at: f64 },
    RemoveColumn(u32),
    RemoveRow(u32),
}

/// Nib mesh of a Folio record.
pub fn nib_mesh(m: &folio::StudioGradientMesh) -> GradientMesh {
    GradientMesh {
        rows: m.rows,
        columns: m.columns,
        points: m
            .points
            .iter()
            .map(|p| MeshPoint { pos: [p.x, p.y], colour: [p.colour.r as f32, p.colour.g as f32, p.colour.b as f32, (p.opacity / 100.0) as f32] })
            .collect(),
        h_handles: m.h_handles.clone(),
        v_handles: m.v_handles.clone(),
        tensor: m.tensor.clone(),
    }
}

/// Folio record of a Nib mesh.
pub fn folio_mesh(mesh_id: &str, layer_id: &str, source_path_id: Option<String>, m: &GradientMesh) -> folio::StudioGradientMesh {
    folio::StudioGradientMesh {
        mesh_id: mesh_id.to_string(),
        layer_id: layer_id.to_string(),
        source_path_id,
        rows: m.rows,
        columns: m.columns,
        points: m
            .points
            .iter()
            .map(|p| folio::StudioMeshPoint {
                x: p.pos[0],
                y: p.pos[1],
                colour: folio::StudioColor { r: f64::from(p.colour[0]), g: f64::from(p.colour[1]), b: f64::from(p.colour[2]), colour_profile_id: folio::ProfileBinding::Unassigned(()) },
                opacity: f64::from(p.colour[3]) * 100.0,
            })
            .collect(),
        h_handles: m.h_handles.clone(),
        v_handles: m.v_handles.clone(),
        tensor: m.tensor.clone(),
    }
}

fn mesh_err(e: hsk_studio_nib::mesh::MeshError) -> String {
    format!("gradient_mesh: {}", e.code())
}

impl Viewer {
    /// The gradient mesh record painting `layer_id`.
    pub fn gradient_mesh(&self, layer_id: &str) -> Result<folio::StudioGradientMesh, String> {
        self.vector_entry()?.records.gradient_meshes.iter().find(|m| m.layer_id == layer_id).cloned().ok_or_else(|| format!("gradient_mesh: {layer_id} has no mesh"))
    }

    /// Creates a `rows x columns` mesh on vector layer `layer_id` (module docs): ONE undo step.
    /// Returns the mesh id.
    pub fn create_gradient_mesh(&mut self, layer_id: &str, rows: u32, columns: u32) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            if entry.records.gradient_meshes.iter().any(|m| m.layer_id == layer_id) {
                return Err(format!("gradient_mesh: {layer_id} already has a mesh"));
            }
            if entry.records.gradient_meshes.len() >= folio::MESH_MAX_ENTRIES {
                return Err("gradient_mesh: budget".into());
            }
            let record = self.vector_path(layer_id)?;
            let (rows_style, _) = current_rows(&entry, doc, layer_id, &record)?;
            let fill = rows_style.iter().find_map(|r| match r {
                StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } => Some([colour.r as f32, colour.g as f32, colour.b as f32, 1.0]),
                _ => None,
            });
            let path = record.path.to_owned_path().map_err(|e| format!("gradient_mesh: path {e:?}"))?;
            let limits = MeshLimits { max_lines: folio::MESH_MAX_LINES, max_patches: folio::MESH_MAX_PATCHES, ..MeshLimits::default() };
            let mesh = GradientMesh::from_path(&path, rows, columns, fill.unwrap_or([1.0; 4]), &limits).map_err(mesh_err)?;
            let mut n = 0_u64;
            let id = loop {
                let id = format!("mesh-{n}");
                if !entry.records.gradient_meshes.iter().any(|m| m.mesh_id == id) {
                    break id;
                }
                n = n.checked_add(1).ok_or("gradient_mesh: id space")?;
            };
            entry.records.gradient_meshes.push(folio_mesh(&id, layer_id, Some(record.path_id.clone()), &mesh));
            self.validate_records(&entry)?;
            self.commit_entry("create_gradient_mesh", layer_id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Applies one mesh edit to the mesh of `layer_id`: ONE undo step.
    pub fn edit_gradient_mesh(&mut self, layer_id: &str, edit: MeshEdit) -> Result<String, String> {
        let r = (|| {
            let mut entry = self.vector_entry()?;
            let i = entry.records.gradient_meshes.iter().position(|m| m.layer_id == layer_id).ok_or_else(|| format!("gradient_mesh: {layer_id} has no mesh"))?;
            let record = entry.records.gradient_meshes[i].clone();
            let mut mesh = nib_mesh(&record);
            let limits = MeshLimits { max_lines: folio::MESH_MAX_LINES, max_patches: folio::MESH_MAX_PATCHES, ..MeshLimits::default() };
            match edit {
                MeshEdit::SetPoint { row, column, rgb, opacity } => {
                    if !(opacity.is_finite() && (0.0..=100.0).contains(&opacity)) {
                        return Err("gradient_mesh: opacity".into());
                    }
                    mesh.set_colour(row, column, [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, (opacity / 100.0) as f32]).map_err(mesh_err)?;
                }
                MeshEdit::MovePoint { row, column, dx, dy } => mesh.move_point(row, column, [dx, dy]).map_err(mesh_err)?,
                MeshEdit::InsertColumn { column, at } => mesh.insert_column(column, at, &limits).map_err(mesh_err)?,
                MeshEdit::InsertRow { row, at } => mesh.insert_row(row, at, &limits).map_err(mesh_err)?,
                MeshEdit::RemoveColumn(c) => mesh.remove_column(c, &limits).map_err(mesh_err)?,
                MeshEdit::RemoveRow(r) => mesh.remove_row(r, &limits).map_err(mesh_err)?,
            }
            mesh.validate(&limits).map_err(mesh_err)?;
            entry.records.gradient_meshes[i] = folio_mesh(&record.mesh_id, layer_id, record.source_path_id.clone(), &mesh);
            self.validate_records(&entry)?;
            self.commit_entry("edit_gradient_mesh", layer_id, &entry)?;
            Ok(record.mesh_id)
        })();
        self.report(r)
    }

    /// The mesh of `layer_id` rasterised by render-cpu over its bounding box (whole pixels,
    /// `px_per_unit` pixels per document unit); returns the raster and its document origin.
    pub fn gradient_mesh_raster(&self, layer_id: &str, px_per_unit: f64) -> Result<(MeshRaster, [f64; 2]), String> {
        let mesh = nib_mesh(&self.gradient_mesh(layer_id)?);
        if !(px_per_unit.is_finite() && px_per_unit > 0.0 && px_per_unit <= 64.0) {
            return Err("gradient_mesh: px_per_unit".into());
        }
        let pts = mesh.points.iter().map(|p| p.pos).chain(mesh.h_handles.iter().chain(&mesh.v_handles).flat_map(|h| h.iter().copied()));
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in pts {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        let (x0, y0) = (lo[0].floor(), lo[1].floor());
        let w = ((hi[0] - x0) * px_per_unit).ceil();
        let h = ((hi[1] - y0) * px_per_unit).ceil();
        if !(w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
            return Err("gradient_mesh: raster size".into());
        }
        let frame = MeshFrame { x0, y0, width: w as u32, height: h as u32, px_per_unit };
        let raster = rasterize_gradient_mesh(&mesh, &frame, &MeshRasterLimits::default()).map_err(|e| format!("gradient_mesh: {}", e.code()))?;
        Ok((raster, [x0, y0]))
    }
}
