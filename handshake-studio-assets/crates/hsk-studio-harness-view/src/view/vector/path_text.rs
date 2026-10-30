//! Type on a path in the viewer (lane SMB-AI-TYPE-PATH-RECOLOR; STU-TYP-004, STU-TYP-120/121,
//! STU-VEC-038). Type lays the text out (`hsk_studio_type::layout_path_text`); the baseline is the
//! first path of a vector layer (Nib geometry, as `BaselineCommand`s); the text is shaped as the
//! text tool shapes point text (`text::shaped_glyphs`, one line, 1 pt = 1 document px).
//!
//! [`Viewer::path_text_layout`] returns the layout (placed glyph transforms, overset report);
//! [`Viewer::text_on_path_to_outlines`] converts the placed glyph outlines into one new vector
//! layer (text to outlines, STU-VEC-038) filled with the vector tool's fill, as ONE undo step.
//! Run metrics: the run's ascent and descent are the ink extents of its glyph outlines (Type
//! exposes no font ascender/descender tables to the harness; a declared approximation), and the
//! em and ICF boxes equal those extents. One font, one size, one run per call. Illustrator parity
//! UNVERIFIED (D5).
//!
//! This module is the one-shot path: a live (editable) story bound through Folio `StudioPathText`
//! / `StudioAreaText` is drawn by the window and redrawn on every path or story edit (lane
//! SMB-AI-AREA-TYPE-2: `Viewer::bind_live_text`, composite `VectorAssets::shaped_story`), and its
//! Create Outlines is `Viewer::create_outlines` (lane SMB-AI-AREA-TYPE-3).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::super::text::shaped_glyphs;
use super::VectorStyle;
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::anchor_path::AnchorPath;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_type::{self as ty, BaselineCommand, PathTextLayout, PathTextLimits, PathTextOptions, PathTextRun};

/// Longest text laid on a path (bytes).
pub const MAX_PATH_TEXT_BYTES: usize = 4096;

/// What to lay on the path.
#[derive(Clone, Debug, PartialEq)]
pub struct PathTextSpec {
    pub text: String,
    /// A registered font family (`Viewer::add_font`; the bundled `Ubuntu-Light` by default).
    pub family: String,
    pub size_pt: f64,
    pub options: PathTextOptions,
}

fn baseline(path: &OwnedPath) -> Vec<BaselineCommand> {
    let p = |q: PathPoint| [q.x, q.y];
    path.elements
        .iter()
        .map(|e| match *e {
            PathElement::MoveTo(a) => BaselineCommand::MoveTo(p(a)),
            PathElement::LineTo(a) => BaselineCommand::LineTo(p(a)),
            PathElement::QuadTo(a, b) => BaselineCommand::QuadTo(p(a), p(b)),
            PathElement::CubicTo(a, b, c) => BaselineCommand::CubicTo(p(a), p(b), p(c)),
            PathElement::Close => BaselineCommand::Close,
        })
        .collect()
}

impl Viewer {
    /// Shapes and lays out `spec` on the path of vector layer `path_layer` (module docs); also
    /// returns the shaped glyph ids and the glyph outlines (font units).
    fn path_text_parts(&self, path_layer: &str, spec: &PathTextSpec) -> Result<(PathTextLayout, Vec<ty::GlyphOutline>), String> {
        if spec.text.is_empty() || spec.text.len() > MAX_PATH_TEXT_BYTES {
            return Err("path_text: text length".into());
        }
        if !(spec.size_pt.is_finite() && spec.size_pt > 0.0 && spec.size_pt <= 1000.0) {
            return Err("path_text: size".into());
        }
        let font = self.fonts.iter().find(|f| f.family == spec.family).ok_or_else(|| format!("path_text: font {} is not registered", spec.family))?;
        let cancel = CancellationToken::default();
        let glyphs = shaped_glyphs(font, &spec.text, spec.size_pt, &cancel)?;
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        let resource = ty::FontResource { identity: &font.family, location: &font.family, content_hash: font.hash, face_index: 0, resource_revision: 1, grant_revision: 1 };
        let outlines = ty::glyph_outlines(&ty::FontBytes { bytes: &font.bytes, resource }, &ids, &ty::OutlineLimits::default(), &cancel).map_err(|e| format!("path_text: {}", e.code()))?;
        // Ink extents of the run (module docs).
        let (mut top, mut bottom) = (0.0_f64, 0.0_f64);
        for o in &outlines {
            if let Some([_, y0, _, y1]) = o.bounds() {
                let k = spec.size_pt / f64::from(o.units_per_em().max(1));
                top = top.max(f64::from(y1) * k);
                bottom = bottom.min(f64::from(y0) * k);
            }
        }
        let run = PathTextRun { glyph_count: glyphs.len(), baseline_offset_pt: 0.0, ascent_pt: top, descent_pt: -bottom, em_top_pt: top, em_bottom_pt: bottom, icf_top_pt: top, icf_bottom_pt: bottom };
        let advances: Vec<f64> = glyphs.iter().map(|g| g.x_advance_pt).collect();
        let record = self.vector_path(path_layer)?;
        let path = record.path.to_owned_path().map_err(|e| format!("path_text: path {}", e.code()))?;
        let layout = ty::layout_path_text(&baseline(&path), &[run], &advances, &spec.options, &PathTextLimits::default()).map_err(|e| format!("path_text: {}", e.code()))?;
        Ok((layout, outlines))
    }

    /// The layout of `spec` on the path of vector layer `path_layer` (module docs).
    pub fn path_text_layout(&self, path_layer: &str, spec: &PathTextSpec) -> Result<PathTextLayout, String> {
        self.path_text_parts(path_layer, spec).map(|(l, _)| l)
    }

    /// Text on a path converted to outlines: one new vector layer holding every placed glyph's
    /// outline (non-zero fill), filled with the vector tool's fill: ONE undo step. Returns the new
    /// layer id and the layout (overset glyphs are reported there, not drawn).
    pub fn text_on_path_to_outlines(&mut self, path_layer: &str, spec: &PathTextSpec) -> Result<(String, PathTextLayout), String> {
        let r = (|| {
            let (layout, outlines) = self.path_text_parts(path_layer, spec)?;
            let mut elements = Vec::new();
            for g in &layout.glyphs {
                let Some(outline) = outlines.get(g.index) else { continue };
                for c in ty::place_outline(g, outline, spec.size_pt).map_err(|e| format!("path_text: {}", e.code()))? {
                    let q = |a: [f64; 2]| PathPoint::new(a[0], a[1]);
                    elements.push(match c {
                        BaselineCommand::MoveTo(a) => PathElement::MoveTo(q(a)),
                        BaselineCommand::LineTo(a) => PathElement::LineTo(q(a)),
                        BaselineCommand::QuadTo(a, b) => PathElement::QuadTo(q(a), q(b)),
                        BaselineCommand::CubicTo(a, b, d) => PathElement::CubicTo(q(a), q(b), q(d)),
                        BaselineCommand::Close => PathElement::Close,
                    });
                }
            }
            if elements.is_empty() {
                return Err("path_text: nothing placed (all glyphs overset or blank)".into());
            }
            let path = AnchorPath::from_owned_path(&OwnedPath { elements, fill_rule: FillRule::NonZero }).map_err(|e| format!("path_text: {}", e.code()))?;
            let t = &self.edit.vector;
            let style = VectorStyle { fill: Some(t.fill), stroke: None, stroke_width: 0.0 };
            let id = self.create_vector_layer("text_on_path", "Text on path", path, style, None)?;
            Ok((id, layout))
        })();
        self.report(r)
    }
}
