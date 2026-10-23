//! Gradient meshes of the vector record set (lane SMB-AI-TYPE-PATH-RECOLOR; STU-VEC-034,
//! STU-COL-036, STU-VEC-059). Member `gradient_meshes` of `StudioVectorRecords`, skipped when empty,
//! so record sets written before it are byte-identical.
//!
//! `StudioGradientMesh` paints vector layer `layer_id` with a `rows x columns` patch mesh in document
//! units: `(rows + 1) x (columns + 1)` mesh points (row major) each with a colour and an opacity
//! percent (the point transparency), the two inner control points of every horizontal
//! (`h_handles`, `(rows + 1) x columns`) and vertical (`v_handles`, `rows x (columns + 1)`) mesh
//! line, and optionally four tensor interior points per patch (`tensor`, empty = Coons patches).
//! `source_path_id` keeps the `SVPT` path the mesh was created from (STU-VEC-059 source
//! reference). Folio stores and validates only: Nib owns the mesh geometry
//! (`hsk_studio_nib::mesh`), render-cpu rasterises it. Declared gap: the spec names the primitive
//! `StudioGradient` kind `gradient.mesh` (STU-VEC-034, STU-COL-036); adding a variant to
//! `StudioGradientGeometry` breaks exhaustive matches in composite, harness and interop, so the mesh
//! is a vector-records member until that is decided; conversion from a linear/radial gradient is
//! not written.
use super::*;
use crate::StudioColor;

/// Most gradient meshes of one record set.
pub const MESH_MAX_ENTRIES: usize = 4096;
/// Most patch rows and most patch columns of one mesh.
pub const MESH_MAX_LINES: u32 = 256;
/// Most patches of one mesh.
pub const MESH_MAX_PATCHES: usize = 16_384;

/// One mesh point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMeshPoint {
    pub x: f64,
    pub y: f64,
    pub colour: StudioColor,
    /// Percent, `0..=100`.
    pub opacity: f64,
}

/// A gradient mesh (module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradientMesh {
    /// Set-local key.
    pub mesh_id: String,
    /// `SLYR` vector layer the mesh paints.
    pub layer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path_id: Option<String>,
    pub rows: u32,
    pub columns: u32,
    pub points: Vec<StudioMeshPoint>,
    pub h_handles: Vec<[[f64; 2]; 2]>,
    pub v_handles: Vec<[[f64; 2]; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tensor: Vec<[[f64; 2]; 4]>,
}

fn finite_pts<'a>(mut it: impl Iterator<Item = &'a [f64; 2]>) -> bool {
    it.all(|p| p[0].is_finite() && p[1].is_finite())
}

pub(super) fn validate_meshes(r: &StudioVectorRecords, layers: &BTreeMap<&str, &StudioLayer>, cx: &mut Ctx) -> Result<()> {
    check(r.gradient_meshes.len() <= MESH_MAX_ENTRIES, Code::BudgetExceeded, "gradient_meshes", "count")?;
    cx.nodes(r.gradient_meshes.len(), "gradient_meshes")?;
    let (mut ids, mut painted) = (BTreeSet::new(), BTreeSet::new());
    for m in &r.gradient_meshes {
        cx.tick()?;
        let t = m.mesh_id.as_str();
        set_id(t, t, "mesh_id")?;
        unique(&mut ids, t, t)?;
        let l = layer(layers, &m.layer_id, &format!("{t}/layer_id"))?;
        check(matches!(l.payload, Payload::Vector { .. }), Code::PayloadMismatch, t, "layer_id")?;
        unique(&mut painted, m.layer_id.as_str(), &format!("{t}/layer_id"))?;
        if let Some(p) = &m.source_path_id {
            id(p, "SVPT", &format!("{t}/source_path_id"))?;
        }
        check(m.rows >= 1 && m.columns >= 1, Code::InvalidField, t, "grid")?;
        check(m.rows <= MESH_MAX_LINES && m.columns <= MESH_MAX_LINES, Code::BudgetExceeded, t, "grid")?;
        let (rows, cols) = (m.rows as usize, m.columns as usize);
        let patches = rows.saturating_mul(cols);
        check(patches <= MESH_MAX_PATCHES, Code::BudgetExceeded, t, "patches")?;
        let points = rows.saturating_add(1).saturating_mul(cols.saturating_add(1));
        check(m.points.len() == points, Code::InvalidField, t, "points")?;
        check(m.h_handles.len() == rows.saturating_add(1).saturating_mul(cols), Code::InvalidField, t, "h_handles")?;
        check(m.v_handles.len() == rows.saturating_mul(cols.saturating_add(1)), Code::InvalidField, t, "v_handles")?;
        check(m.tensor.is_empty() || m.tensor.len() == patches, Code::InvalidField, t, "tensor")?;
        cx.nodes(points.saturating_add(m.h_handles.len()).saturating_add(m.v_handles.len()).saturating_add(m.tensor.len()), t)?;
        for (n, p) in m.points.iter().enumerate() {
            let target = format!("{t}/points/{n}");
            check(p.x.is_finite() && p.y.is_finite(), Code::InvalidField, &target, "position")?;
            check(p.opacity.is_finite() && (0.0..=100.0).contains(&p.opacity), Code::InvalidField, &target, "opacity")?;
            p.colour.validate(&target, cx)?;
        }
        let handles = m.h_handles.iter().chain(&m.v_handles).flat_map(|h| h.iter());
        check(finite_pts(handles), Code::InvalidField, t, "handles")?;
        check(finite_pts(m.tensor.iter().flat_map(|q| q.iter())), Code::InvalidField, t, "tensor")?;
    }
    Ok(())
}
