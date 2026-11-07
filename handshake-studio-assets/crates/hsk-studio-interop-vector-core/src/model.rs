//! Neutral proposal model of vector import. Plain data: it is a proposal for the host to adopt,
//! never a private vector or style store (ENG-DAG-FORBID-003).
use crate::error::{Result, VectorError};
use hsk_studio_folio::{Snapshot, StudioDesignRecords, StudioPaintStyle, StudioVectorRecords};
use hsk_studio_nib::{Anchor, OwnedPath as GeometryPath, PathLimits, Winding};

/// Hostile-input budgets, all checked before the corresponding work is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    /// XML nodes, and also the element budget of the walk. The Folio design records an import
    /// keeps (styles, rows, gradient stops, dash pairs, node entries) are capped at 16 per node
    /// (`BudgetExceeded("design_records")`). The content of the clip, mask and pattern
    /// definitions built for references has an allowance of the same size on top (and
    /// `max_segments` segments): running out of it leaves that reference a typed loss
    /// (`definition_budget`), it does not fail the import.
    pub max_nodes: u32,
    pub max_depth: usize,
    /// Path segments and shape anchors summed over the document.
    pub max_segments: usize,
    /// Decompression-bomb cap: largest SVGZ payload after inflating (gzip is never unwrapped
    /// twice). `max_bytes` bounds the input as given, compressed or not.
    pub max_decompressed_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_nodes: 20_000,
            max_depth: 32,
            max_segments: 500_000,
            max_decompressed_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Import options. There is deliberately no switch that allows external fetching: no code path
/// in this crate opens a network connection, a file or a link target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImportPolicy {
    pub limits: Limits,
    /// Pixels per inch used for the CSS pixel to point conversion (96 gives 0.75 pt per px).
    pub pixels_per_inch: f64,
}

impl Default for ImportPolicy {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            pixels_per_inch: 96.0,
        }
    }
}

impl ImportPolicy {
    #[doc(hidden)]
    pub fn validate(&self) -> Result<()> {
        if !self.pixels_per_inch.is_finite() || self.pixels_per_inch <= 0.0 {
            return Err(VectorError::InvalidPolicy("pixels_per_inch"));
        }
        if self.limits.max_nodes == 0 || self.limits.max_depth == 0 || self.limits.max_segments == 0
        {
            return Err(VectorError::InvalidPolicy("limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LossKind {
    /// Geometry or value is a close but not exact representation (arc to cubic, ellipse cubics).
    Approximated,
    /// A `transform` was applied to the points and is no longer an editable attribute.
    TransformBaked,
    /// A feature of the source has no counterpart in the proposal and was not carried over.
    Unsupported,
    /// A reference to something outside the file was denied; nothing was fetched.
    ExternalDenied,
    /// Content without visual meaning (metadata, foreign namespaces, degenerate subpaths).
    Ignored,
}

impl LossKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Approximated => "approximated",
            Self::TransformBaked => "transform_baked",
            Self::Unsupported => "unsupported",
            Self::ExternalDenied => "external_denied",
            Self::Ignored => "ignored",
        }
    }
}

/// One reported loss. `path` is an element locator such as `/svg[1]/g[2]/path[1]`; it holds tag
/// names and ordinals only, never attribute values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossEntry {
    pub path: String,
    pub kind: LossKind,
    pub code: &'static str,
}

/// How the gradient geometry numbers were authored. Folio's gradient record has no member for it:
/// the record's `transform` already includes the bounding-box mapping, so this is provenance, not
/// an input to rendering (see [`VectorProposal::gradient_units`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

/// Provenance of one gradient row of a paint style: the SVG `gradientUnits` it was authored with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradientProvenance {
    /// `SSTY` id of the style in [`VectorProposal::design`].
    pub style_id: String,
    /// Index into that style's `rows`.
    pub row: usize,
    pub units: GradientUnits,
}

/// Style-reference field of a vector layer (STU-DS-136) that names the `PAINT` style holding the
/// layer's whole appearance stack (fill and stroke rows, bottom to top). One style per layer.
pub const LAYER_STYLE_REF: &str = "fill_style_id";

/// One subpath in the `hsk.studio.vector_path@1` shape: points are lengths in points, handles are
/// vectors relative to their anchor. A compound SVG path becomes several `VectorPath`s that share
/// `layer_id`.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorPath {
    /// `SVPT` id.
    pub path_id: String,
    /// `SLYR` id of the Folio layer that lists this path in its `Payload::Vector`.
    pub layer_id: String,
    pub closed: bool,
    pub anchors: Vec<Anchor>,
    /// `Winding::None` means the path has no fill.
    pub winding: Winding,
}

impl VectorPath {
    /// Geometry in points as Nib's owned path (fill rule `NonZero` when the path has no fill).
    pub fn to_geometry(
        &self,
        limits: &PathLimits,
    ) -> std::result::Result<GeometryPath, VectorError> {
        let winding = if self.winding == Winding::None {
            Winding::NonZero
        } else {
            self.winding
        };
        GeometryPath::from_anchors(
            &self.anchors,
            self.closed,
            winding,
            hsk_studio_accord::Unit::Points,
            None,
            limits,
        )
        .map_err(|_| VectorError::InvalidGeometry)
    }
}

/// Every unit conversion is recorded once here; coordinates in the proposal are already points.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub source_format: &'static str,
    pub source_unit: &'static str,
    pub pixels_per_inch: f64,
    /// Points per source pixel (`72 / pixels_per_inch`).
    pub px_to_pt: f64,
    /// Length of the file as given (compressed for SVGZ).
    pub source_len: usize,
    /// Length of the SVG text that was parsed (equals `source_len` unless SVGZ).
    pub decoded_len: usize,
    /// `viewBox` as written, `[min_x, min_y, width, height]` in user units.
    pub view_box: Option<[f64; 4]>,
    /// Viewport size in source pixels.
    pub viewport_px: [f64; 2],
}

#[derive(Clone, Debug)]
pub struct VectorProposal {
    /// Folio document (revision 1) with one artboard, group layers and vector layers.
    pub snapshot: Snapshot,
    pub paths: Vec<VectorPath>,
    /// Folio design records of the document (`document_id` equals the snapshot's), validated by
    /// `validate_design_records`: `paint_styles` hold the appearance stacks (ordered fill and
    /// stroke rows with solid or gradient paint and stroke properties) and each vector layer's
    /// `StudioNodeDesign.style_refs[`[`LAYER_STYLE_REF`]`]` names its style. This is the single
    /// source of paint, stroke and appearance data; the proposal creates no style registry.
    pub design: StudioDesignRecords,
    /// Folio vector records of the document (`hsk.studio.vector_records@1`), validated by
    /// `validate_vector_records` against the snapshot and `design`: the patterns, clips and masks
    /// that SVG `<pattern>`, `<clipPath>` and `<mask>` references resolved to, and the clip/mask
    /// attachment of each layer that uses one. Their geometry is baked into document points
    /// (SVG `objectBoundingBox` units and every `transform` are resolved on import). The source
    /// layers of these records (clip geometry, mask content, pattern tile artwork) are hidden
    /// group layers directly under the artboard; their paths are part of [`Self::paths`].
    pub vector_records: StudioVectorRecords,
    /// SVG gradient units per gradient row (provenance Folio has no member for).
    pub gradient_provenance: Vec<GradientProvenance>,
    pub loss: Vec<LossEntry>,
    pub provenance: Provenance,
}

impl VectorProposal {
    pub fn has_loss(&self, kind: LossKind) -> bool {
        self.loss.iter().any(|l| l.kind == kind)
    }
    pub fn has_loss_code(&self, code: &str) -> bool {
        self.loss.iter().any(|l| l.code == code)
    }
    /// The paint style of a vector layer, through its design entry.
    pub fn layer_style(&self, layer_id: &str) -> Option<&StudioPaintStyle> {
        layer_style(&self.design, layer_id)
    }
    /// How SVG authored the gradient of row `row` of style `style_id`, when that row is a gradient.
    pub fn gradient_units(&self, style_id: &str, row: usize) -> Option<GradientUnits> {
        self.gradient_provenance
            .iter()
            .find(|g| g.style_id == style_id && g.row == row)
            .map(|g| g.units)
    }
}

/// The paint style a layer names through [`LAYER_STYLE_REF`], when the record set holds it.
pub fn layer_style<'a>(
    design: &'a StudioDesignRecords,
    layer_id: &str,
) -> Option<&'a StudioPaintStyle> {
    let id = design
        .node_design(layer_id)?
        .style_refs
        .get(LAYER_STYLE_REF)?;
    design.paint_styles.iter().find(|s| &s.style_id == id)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportOptions {
    pub pixels_per_inch: f64,
    /// Artboard size in points; `None` uses the document's first artboard (bounds in points), else
    /// `[max x, max y]` of all anchors and handles (at least 1 pt).
    pub artboard_pt: Option<[f64; 2]>,
    /// The named board's top-left corner in document points, y down (lane SMB-ARTBOARD-EXPORT-1):
    /// the SVG `viewBox` starts there and the EPS page is translated by it, so a board off the
    /// document origin frames its own art. `None` with `artboard_pt` set: `0 0`; with both `None`,
    /// the first artboard's origin.
    pub artboard_origin_pt: Option<[f64; 2]>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            pixels_per_inch: 96.0,
            artboard_pt: None,
            artboard_origin_pt: None,
        }
    }
}

impl ExportOptions {
    /// These options with the board frame resolved: the caller's `artboard_pt` (and origin), else the
    /// first artboard of `doc` whose bounds are in points with a positive size, its origin included.
    pub fn framed_by(&self, doc: &hsk_studio_folio::StudioDocument) -> Self {
        if self.artboard_pt.is_some() {
            return *self;
        }
        match doc.artboards.first().and_then(artboard_region_pt) {
            Some((origin, size)) => Self { artboard_pt: Some(size), artboard_origin_pt: self.artboard_origin_pt.or(Some(origin)), ..*self },
            None => *self,
        }
    }

    /// The origin, `0 0` when none is set; refused when not finite or beyond [`COORDINATE_LIMIT`].
    pub fn origin(&self) -> Result<[f64; 2]> {
        match self.artboard_origin_pt {
            None => Ok([0.0, 0.0]),
            Some(o) if o.iter().all(|v| v.is_finite() && v.abs() <= COORDINATE_LIMIT) => Ok(o),
            Some(_) => Err(VectorError::InvalidPolicy("artboard_origin_pt")),
        }
    }
}

/// The top-left corner and size in points of an artboard whose bounds are in points with a positive
/// size (the other units need a resolution the document does not carry here).
pub fn artboard_region_pt(a: &hsk_studio_folio::StudioArtboard) -> Option<([f64; 2], [f64; 2])> {
    let b = &a.bounds;
    let pt = |l: &hsk_studio_folio::Length| (l.unit == hsk_studio_folio::GeometryUnit::Pt && l.value.is_finite()).then_some(l.value);
    match (pt(&b.x), pt(&b.y), pt(&b.width), pt(&b.height)) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(([x, y], [w, h])),
        _ => None,
    }
}

/// Coordinates and handle components beyond this magnitude are refused (exact in binary64).
#[doc(hidden)]
pub const COORDINATE_LIMIT: f64 = 1.0e9;
