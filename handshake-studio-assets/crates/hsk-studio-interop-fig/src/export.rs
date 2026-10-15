//! Frame export of an imported Figma document (lane SMB-INTEROP-FIG-EXPORT, parity row "export
//! frames as PNG/PDF"): a frame (artboard, frame group or instance expansion) of the
//! [`NativeProposal`] is lowered to one page of `hsk-studio-press`'s `PdfExport` (vector paths,
//! solid and gradient fills, strokes, clips, transparency groups, images, Figma's own glyph
//! outlines), and the page is rendered back by press's independent interpreter (`render_page`) for a
//! raster export. The frame's `.fig` export settings choose format, scale and file name
//! ([`plan_frame_exports`]).
//!
//! Units: 1 document px = 1 pt at scale 1 (Figma's PDF export convention, UNVERIFIED); a setting's
//! scale multiplies the page. PDF user space is y-up: a document point (x, y) of a frame at
//! (x0, y0, w, h) goes to ((x - x0) s, (h - (y - y0)) s).
//!
//! Nothing is dropped silently: every setting, layer kind, row or record the PDF page cannot carry
//! is a typed loss of the export (`FigPdfExport::loss`): effects (shadows, blurs), dashed or
//! outside strokes, tiled image paints, masks other than a vector shape (a vector mask is written
//! as a clip of its geometry, noted), blend modes PDF lacks, text without stored glyph outlines.
//! Text is written as Figma's own laid-out glyph outlines (exact shapes, not extractable text:
//! loss note `text_as_outlines`). Tests: fig_export.
#![deny(clippy::arithmetic_side_effects)]
use crate::import::SourceId;
use crate::native::{FigTextLayout, NativeProposal};
use crate::nodes::{FigDocument, NodeLoss};
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    BLEND_NORMAL, BLEND_PASS_THROUGH, NodeKind, NodeRef, Parent, Payload, StrokeAlign as FolioAlign,
    StrokeCap as FolioCap, StrokeJoin as FolioJoin, StudioAppearanceRow, StudioGradientGeometry,
    StudioImagePaint, StudioImageScale, StudioImageSource, StudioLayer, StudioPaint,
};
use hsk_studio_nib::{FillRule as NibRule, OwnedPath, PathElement};
use hsk_studio_press::{
    BlendMode, Colour, DrawState, ExportOptions, ExportReceipt, FillRule, Gradient, GradientGeometry,
    GradientSpread, GradientStop, GroupStyle, ImageColour, ImageSpec, LineCap, LineJoin, PageHandle,
    PageSpec, PathCommand, PdfExport, RenderedPage, StrokeStyle,
};
use std::collections::{BTreeMap, HashMap};

/// Largest export scale accepted (Figma offers up to 4x; larger values are refused, typed).
pub const MAX_EXPORT_SCALE: f64 = 16.0;
/// Largest page side in points after scaling (press's coordinate bound is far larger; this keeps
/// the render-back raster bounded before allocation).
pub const MAX_EXPORT_SIDE_PT: f64 = 16_384.0;

/// The export file format of a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FigExportFormat {
    Png,
    Jpeg,
    Pdf,
}

impl FigExportFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Pdf => "pdf",
        }
    }
}

/// One export of one frame, from one `.fig` export setting (or the default PNG 1x when the frame
/// has none).
#[derive(Clone, Debug, PartialEq)]
pub struct FigExportPlan {
    pub source: SourceId,
    pub frame: NodeRef,
    /// `<frame name><suffix>.<ext>` (path separators in the name become `_`).
    pub file_name: String,
    pub format: FigExportFormat,
    pub scale: f64,
    /// Frame bounds in document px: x, y, width, height.
    pub bounds: [f64; 4],
    /// `true` when the frame had no export setting and Figma's default (PNG, 1x) was used.
    pub default_setting: bool,
}

fn lose(loss: &mut BTreeMap<(String, &'static str), (u32, Option<SourceId>)>, path: impl Into<String>, code: &'static str, at: Option<SourceId>) {
    let slot = loss.entry((path.into(), code)).or_insert((0, at));
    slot.0 = slot.0.saturating_add(1);
}

fn aggregate(loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>) -> Vec<NodeLoss> {
    loss.into_iter()
        .map(|((path, code), (count, first))| NodeLoss { path, code, count, first })
        .collect()
}

fn bbox(paths: &[&OwnedPath]) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut any = false;
    for p in paths {
        for e in &p.elements {
            let pts: &[&hsk_studio_nib::PathPoint] = match e {
                PathElement::MoveTo(a) | PathElement::LineTo(a) => &[a],
                PathElement::QuadTo(a, c) => &[a, c],
                PathElement::CubicTo(a, c, d) => &[a, c, d],
                PathElement::Close => &[],
            };
            for q in pts {
                any = true;
                b = [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)];
            }
        }
    }
    any.then_some(b)
}

/// Bounds of a frame node in document px: an artboard's bounds, else the geometry of the frame's
/// background (or clip-source) layer.
fn frame_bounds(p: &NativeProposal, frame: &NodeRef) -> Option<[f64; 4]> {
    if frame.node_kind == NodeKind::Artboard {
        let a = p.snapshot.document().artboards.iter().find(|a| a.artboard_id == frame.node_id)?;
        return Some([a.bounds.x.value, a.bounds.y.value, a.bounds.width.value, a.bounds.height.value]);
    }
    let bg = p.frame_backgrounds.iter().find(|b| &b.frame == frame)?;
    let paths: Vec<&OwnedPath> = p.paths.iter().filter(|x| x.layer_id == bg.layer_id).map(|x| &x.geometry).collect();
    let b = bbox(&paths)?;
    Some([b[0], b[1], b[2] - b[0], b[3] - b[1]])
}

/// The exports a frame asks for: each `.fig` export setting with a supported format (PNG, JPEG,
/// PDF), scale constraint (`CONTENT_SCALE`, `CONTENT_WIDTH`, `CONTENT_HEIGHT`) and options; every
/// other setting or option is a typed loss and produces no plan (never a substitute export). A
/// frame without settings gets Figma's default PNG 1x (`default_setting`).
pub fn plan_frame_exports(doc: &FigDocument, p: &NativeProposal, source: SourceId) -> (Vec<FigExportPlan>, Vec<NodeLoss>) {
    let mut loss = BTreeMap::new();
    let mut out = Vec::new();
    let Some(node) = doc.node(source) else {
        lose(&mut loss, "export.frame", "export_frame_not_in_document", Some(source));
        return (out, aggregate(loss));
    };
    let Some(frame) = p.id_map.iter().find(|e| e.source == source).map(|e| e.node.clone()) else {
        lose(&mut loss, "export.frame", "export_frame_not_mapped", Some(source));
        return (out, aggregate(loss));
    };
    // A document instance draws through its expansion group.
    let frame = p
        .expansions
        .iter()
        .find(|e| e.binding_layer_id.as_deref() == Some(frame.node_id.as_str()))
        .map(|e| NodeRef { node_kind: NodeKind::Layer, node_id: e.group_layer_id.clone() })
        .unwrap_or(frame);
    let Some(bounds) = frame_bounds(p, &frame) else {
        lose(&mut loss, "export.frame", "export_frame_bounds_unknown", Some(source));
        return (out, aggregate(loss));
    };
    let name: String = node
        .name
        .clone()
        .unwrap_or_default()
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':') { '_' } else { c })
        .collect();
    let default = [crate::nodes::ExportSetting { image_type: Some("PNG".into()), ..Default::default() }];
    let (settings, default_setting) = if node.export_settings.is_empty() {
        (&default[..], true)
    } else {
        (&node.export_settings[..], false)
    };
    for s in settings {
        let mut refused = false;
        let format = match s.image_type.as_deref() {
            None | Some("PNG") => Some(FigExportFormat::Png),
            Some("JPEG") => Some(FigExportFormat::Jpeg),
            Some("PDF") => Some(FigExportFormat::Pdf),
            Some(other) => {
                lose(&mut loss, format!("export.format={other}"), "export_format_unsupported", Some(source));
                None
            }
        };
        let scale = match s.constraint.as_ref() {
            None => Some(1.0),
            Some((kind, v)) => {
                let v = crate::native::wide(*v);
                match kind.as_str() {
                    "CONTENT_SCALE" => Some(v),
                    "CONTENT_WIDTH" if bounds[2] > 0.0 => Some(v / bounds[2]),
                    "CONTENT_HEIGHT" if bounds[3] > 0.0 => Some(v / bounds[3]),
                    other => {
                        lose(&mut loss, format!("export.constraint={other}"), "export_constraint_unsupported", Some(source));
                        None
                    }
                }
            }
        };
        let scale = scale.filter(|s| {
            let ok = s.is_finite()
                && *s > 0.0
                && *s <= MAX_EXPORT_SCALE
                && bounds[2] * *s <= MAX_EXPORT_SIDE_PT
                && bounds[3] * *s <= MAX_EXPORT_SIDE_PT;
            if !ok {
                lose(&mut loss, "export.constraint", "export_scale_out_of_range", Some(source));
            }
            ok
        });
        if !matches!(s.color_profile.as_deref(), None | Some("DOCUMENT") | Some("SRGB")) {
            lose(&mut loss, format!("export.color_profile={}", s.color_profile.as_deref().unwrap_or("?")), "export_color_profile_unsupported", Some(source));
            refused = true;
        }
        if s.contents_only == Some(false) {
            lose(&mut loss, "export.contents_only", "export_with_overlapping_content_unsupported", Some(source));
            refused = true;
        }
        if s.use_absolute_bounds == Some(true) {
            lose(&mut loss, "export.use_absolute_bounds", "export_absolute_bounds_unsupported", Some(source));
            refused = true;
        }
        if s.quality.is_some() && format != Some(FigExportFormat::Jpeg) {
            lose(&mut loss, "export.quality", "export_quality_ignored_for_format", Some(source));
        }
        for f in &s.unsupported {
            lose(&mut loss, format!("export.{f}"), "export_setting_unsupported", Some(source));
            refused = true;
        }
        let (Some(format), Some(scale)) = (format, scale) else { continue };
        if refused {
            continue;
        }
        out.push(FigExportPlan {
            source,
            frame: frame.clone(),
            file_name: format!("{name}{}.{}", s.suffix.clone().unwrap_or_default(), format.extension()),
            format,
            scale,
            bounds,
            default_setting,
        });
    }
    (out, aggregate(loss))
}

/// Decoded pixels of an image paint source (8-bit RGB, optional 8-bit alpha), supplied by the
/// caller (the importer keeps image bytes opaque).
pub struct FigDecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub alpha: Option<Vec<u8>>,
}

/// Caller-side image decoding for image paints.
pub trait FigImageSource {
    fn decoded(&self, source: &StudioImageSource) -> Option<FigDecodedImage>;
}

/// A frame written as a one-page PDF, with the export's typed losses.
#[derive(Clone, Debug)]
pub struct FigPdfExport {
    pub bytes: Vec<u8>,
    pub receipt: ExportReceipt,
    pub loss: Vec<NodeLoss>,
}

struct Lower<'a> {
    p: &'a NativeProposal,
    ex: PdfExport,
    page: PageHandle,
    origin: [f64; 2],
    height: f64,
    scale: f64,
    images: Option<&'a dyn FigImageSource>,
    children: HashMap<&'a str, Vec<&'a StudioLayer>>,
    paths: HashMap<&'a str, &'a OwnedPath>,
    texts: HashMap<&'a str, &'a FigTextLayout>,
    loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>,
    cancel: &'a CancellationToken,
}

fn press_err(e: hsk_studio_press::ExportError) -> FigError {
    FigError::ExportRefused(e.code())
}

fn blend_of(mode: u16) -> Option<BlendMode> {
    Some(match mode {
        1 | 2 => BlendMode::Normal,
        4 => BlendMode::Darken,
        5 => BlendMode::Multiply,
        6 => BlendMode::ColorBurn,
        8 => BlendMode::Lighten,
        9 => BlendMode::Screen,
        10 => BlendMode::ColorDodge,
        12 => BlendMode::Overlay,
        13 => BlendMode::SoftLight,
        14 => BlendMode::HardLight,
        18 => BlendMode::Difference,
        19 => BlendMode::Exclusion,
        20 => BlendMode::Hue,
        21 => BlendMode::Saturation,
        22 => BlendMode::Color,
        23 => BlendMode::Luminosity,
        _ => return None,
    })
}

impl<'a> Lower<'a> {
    fn lose(&mut self, path: impl Into<String>, code: &'static str) {
        lose(&mut self.loss, path, code, None);
    }

    fn pt(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.origin[0]) * self.scale, (self.height - (y - self.origin[1])) * self.scale)
    }

    /// Document-px affine -> page affine (`[a b c d e f]`, x' = a x + c y + e).
    fn page_affine(&self, m: [f64; 6]) -> [f64; 6] {
        let s = self.scale;
        let [a, b, c, d, e, f] = m;
        [a * s, -b * s, c * s, -d * s, (e - self.origin[0]) * s, (self.height - (f - self.origin[1])) * s]
    }

    fn commands(&self, path: &OwnedPath) -> Vec<PathCommand> {
        let mut out = Vec::with_capacity(path.elements.len());
        let mut cur = (0.0, 0.0);
        for e in &path.elements {
            match e {
                PathElement::MoveTo(a) => {
                    cur = (a.x, a.y);
                    let (x, y) = self.pt(a.x, a.y);
                    out.push(PathCommand::MoveTo(x, y));
                }
                PathElement::LineTo(a) => {
                    cur = (a.x, a.y);
                    let (x, y) = self.pt(a.x, a.y);
                    out.push(PathCommand::LineTo(x, y));
                }
                PathElement::QuadTo(c, a) => {
                    // Exact degree elevation of the quadratic.
                    let c1 = (cur.0 + 2.0 / 3.0 * (c.x - cur.0), cur.1 + 2.0 / 3.0 * (c.y - cur.1));
                    let c2 = (a.x + 2.0 / 3.0 * (c.x - a.x), a.y + 2.0 / 3.0 * (c.y - a.y));
                    cur = (a.x, a.y);
                    let (x1, y1) = self.pt(c1.0, c1.1);
                    let (x2, y2) = self.pt(c2.0, c2.1);
                    let (x, y) = self.pt(a.x, a.y);
                    out.push(PathCommand::CubicTo(x1, y1, x2, y2, x, y));
                }
                PathElement::CubicTo(c1, c2, a) => {
                    cur = (a.x, a.y);
                    let (x1, y1) = self.pt(c1.x, c1.y);
                    let (x2, y2) = self.pt(c2.x, c2.y);
                    let (x, y) = self.pt(a.x, a.y);
                    out.push(PathCommand::CubicTo(x1, y1, x2, y2, x, y));
                }
                PathElement::Close => out.push(PathCommand::Close),
            }
        }
        out
    }

    fn rule(r: NibRule) -> FillRule {
        match r {
            NibRule::EvenOdd => FillRule::EvenOdd,
            _ => FillRule::NonZero,
        }
    }

    /// The paths of a layer by rule, commands concatenated.
    fn geometry(&self, ids: &[String]) -> Vec<(Vec<PathCommand>, FillRule, &'a OwnedPath)> {
        ids.iter()
            .filter_map(|id| self.paths.get(id.as_str()).copied())
            .map(|g| (self.commands(g), Self::rule(g.fill_rule), g))
            .collect()
    }

    fn state(&mut self, alpha: f64, blend: BlendMode) -> Result<()> {
        self.ex
            .set_draw_state(self.page, DrawState { fill_alpha: alpha, stroke_alpha: alpha, blend })
            .map_err(press_err)
    }

    fn children_of(&mut self, parent: &str) -> Result<()> {
        let kids = self.children.get(parent).cloned().unwrap_or_default();
        for l in kids {
            self.layer(l)?;
        }
        Ok(())
    }

    fn layer(&mut self, l: &StudioLayer) -> Result<()> {
        self.cancel.check().map_err(|_| FigError::Canceled)?;
        if !l.visible {
            return Ok(());
        }
        let p = self.p;
        if p.raster_records.layers.iter().any(|r| r.layer_id == l.layer_id && r.effects.is_some()) {
            self.lose("export.effects", "effect_not_exported");
        }
        // Clip and mask attachments: written as a clip of the source geometry.
        let mut clips = 0u32;
        if let Some(a) = p.vector_records.attachments.iter().find(|a| a.layer_id == l.layer_id) {
            let mut sources = Vec::new();
            if let Some(c) = a.clip_id.as_ref().and_then(|id| p.vector_records.clips.iter().find(|c| &c.clip_id == id)) {
                sources.push(c.source_layer_id.clone());
            }
            if let Some(m) = a.mask_id.as_ref().and_then(|id| p.vector_records.masks.iter().find(|m| &m.mask_id == id)) {
                self.lose("export.mask", "mask_written_as_geometry_clip");
                sources.push(m.source_layer_id.clone());
            }
            for s in sources {
                let ids = match p.snapshot.document().layers.iter().find(|x| x.layer_id == s).map(|x| &x.payload) {
                    Some(Payload::Vector { path_ids }) => path_ids.clone(),
                    _ => Vec::new(),
                };
                let geo = self.geometry(&ids);
                if geo.is_empty() {
                    self.lose("export.clip", "clip_source_without_geometry");
                    continue;
                }
                let rule = geo[0].1;
                let cmds: Vec<PathCommand> = geo.into_iter().flat_map(|g| g.0).collect();
                self.ex.push_clip(self.page, &cmds, rule).map_err(press_err)?;
                clips = clips.saturating_add(1);
            }
        }
        // Layer appearance: a non-identity opacity or blend is a transparency group.
        let appearance = l.appearance.as_ref().filter(|a| {
            (a.opacity - 100.0).abs() > 1e-9 || !(a.blend_mode == BLEND_NORMAL || a.blend_mode == BLEND_PASS_THROUGH)
        });
        let mut grouped = None;
        if let Some(a) = appearance {
            let blend = match blend_of(a.blend_mode) {
                Some(b) => b,
                None => {
                    self.lose(format!("export.blend_mode={}", a.blend_mode), "blend_mode_not_in_pdf");
                    BlendMode::Normal
                }
            };
            self.ex.begin_group(self.page).map_err(press_err)?;
            grouped = Some(GroupStyle { opacity: (a.opacity / 100.0).clamp(0.0, 1.0), blend, isolated: true, knockout: false, soft_mask: None });
        }
        let boolean = p.vector_records.boolean_group(&l.layer_id);
        match (&l.payload, boolean) {
            (_, Some(g)) => {
                let ids = g.fill_geometry_path_ids.clone();
                self.vector(l, &ids)?;
            }
            (Payload::Group, None) => self.children_of(&l.layer_id)?,
            (Payload::Vector { path_ids }, None) => {
                let ids = path_ids.clone();
                self.vector(l, &ids)?;
            }
            (Payload::Text { .. }, None) => self.text(l)?,
            _ if l.kind == "component_instance" => {}
            _ => self.lose(format!("export.layer_kind={}", l.kind), "layer_kind_not_exported"),
        }
        if let Some(style) = grouped {
            self.ex.end_group(self.page, style).map_err(press_err)?;
        }
        for _ in 0..clips {
            self.ex.pop_clip(self.page).map_err(press_err)?;
        }
        Ok(())
    }

    fn rows(&self, layer_id: &str) -> Option<(&'a str, &'a [StudioAppearanceRow])> {
        let p: &'a NativeProposal = self.p;
        let d = p.design.nodes.iter().find(|d| d.node.node_id == layer_id)?;
        let sid = d.style_refs.get("fill_style_id")?;
        let s = p.design.paint_styles.iter().find(|s| &s.style_id == sid)?;
        Some((s.style_id.as_str(), s.rows.as_slice()))
    }

    fn vector(&mut self, l: &StudioLayer, ids: &[String]) -> Result<()> {
        let geo = self.geometry(ids);
        if geo.is_empty() {
            self.lose("export.vector", "vector_without_geometry");
            return Ok(());
        }
        let Some((style_id, rows)) = self.rows(&l.layer_id) else {
            return Ok(());
        };
        let style_id = style_id.to_owned();
        let rows = rows.to_vec();
        for (i, row) in rows.iter().enumerate() {
            match row {
                StudioAppearanceRow::Fill { paint, opacity, blend_mode, visible } => {
                    if !visible {
                        continue;
                    }
                    let blend = blend_of(*blend_mode).unwrap_or_else(|| {
                        self.lose("export.fill.blend_mode", "blend_mode_not_in_pdf");
                        BlendMode::Normal
                    });
                    let image = self
                        .p
                        .vector_records
                        .image_paints
                        .iter()
                        .find(|ip| ip.style_id == style_id && usize::try_from(ip.row).ok() == Some(i))
                        .cloned();
                    if let Some(img) = image {
                        self.state(1.0, blend)?;
                        self.image(&img, &geo)?;
                        continue;
                    }
                    self.state((opacity / 100.0).clamp(0.0, 1.0), blend)?;
                    match paint {
                        StudioPaint::Solid { colour } => {
                            for (cmds, rule, _) in &geo {
                                self.ex
                                    .fill_path(self.page, cmds, *rule, Colour::DeviceRgb([colour.r, colour.g, colour.b]))
                                    .map_err(press_err)?;
                            }
                        }
                        StudioPaint::Gradient { gradient } => {
                            let geometry = match gradient.geometry {
                                StudioGradientGeometry::Linear { x1, y1, x2, y2 } => GradientGeometry::Axial { from: [x1, y1], to: [x2, y2] },
                                StudioGradientGeometry::Radial { cx, cy, r, fx, fy, fr } => {
                                    GradientGeometry::Radial { from: [fx, fy], from_radius: fr, to: [cx, cy], to_radius: r }
                                }
                            };
                            let stops: Vec<GradientStop> = gradient
                                .stops
                                .iter()
                                .map(|s| GradientStop {
                                    offset: (s.position / 100.0).clamp(0.0, 1.0),
                                    colour: [s.colour.r, s.colour.g, s.colour.b],
                                    opacity: (s.opacity / 100.0).clamp(0.0, 1.0),
                                    midpoint: None,
                                })
                                .collect();
                            if gradient.stops.iter().any(|s| s.midpoint.is_some()) {
                                self.lose("export.gradient.midpoint", "gradient_midpoint_not_exported");
                            }
                            let g = Gradient {
                                geometry,
                                stops: &stops,
                                extend: [true, true],
                                spread: GradientSpread::Pad,
                                matrix: self.page_affine(gradient.transform),
                                profile: None,
                            };
                            for (cmds, rule, _) in &geo {
                                self.ex.fill_path_gradient(self.page, cmds, *rule, &g).map_err(press_err)?;
                            }
                        }
                    }
                }
                StudioAppearanceRow::Stroke { paint, stroke, opacity, blend_mode, visible } => {
                    if !visible {
                        continue;
                    }
                    let StudioPaint::Solid { colour } = paint else {
                        self.lose("export.stroke", "stroke_paint_not_solid_not_exported");
                        continue;
                    };
                    if !stroke.dash.is_empty() {
                        self.lose("export.stroke.dash", "stroke_dash_not_exported");
                        continue;
                    }
                    if stroke.align == FolioAlign::Outside {
                        self.lose("export.stroke.align", "stroke_outside_not_exported");
                        continue;
                    }
                    let blend = blend_of(*blend_mode).unwrap_or(BlendMode::Normal);
                    self.state((opacity / 100.0).clamp(0.0, 1.0), blend)?;
                    let inside = stroke.align == FolioAlign::Inside;
                    let width = stroke.weight.value * self.scale * if inside { 2.0 } else { 1.0 };
                    let style = StrokeStyle {
                        width_pt: width,
                        cap: match stroke.cap {
                            FolioCap::Butt => LineCap::Butt,
                            FolioCap::Round => LineCap::Round,
                            FolioCap::Projecting => LineCap::Square,
                        },
                        join: match stroke.join {
                            FolioJoin::Miter => LineJoin::Miter,
                            FolioJoin::Round => LineJoin::Round,
                            FolioJoin::Bevel => LineJoin::Bevel,
                        },
                    };
                    if stroke.join == FolioJoin::Miter && (stroke.miter_limit - 10.0).abs() > 1e-9 {
                        // PDF's default miter limit (10) is used; Figma stores its own (default 4).
                        self.lose("export.stroke.miter_limit", "stroke_miter_limit_pdf_default");
                    }
                    for (cmds, rule, _) in &geo {
                        // An inside stroke: twice the width, clipped to the shape.
                        if inside {
                            self.ex.push_clip(self.page, cmds, *rule).map_err(press_err)?;
                        }
                        self.ex
                            .stroke_path(self.page, cmds, Colour::DeviceRgb([colour.r, colour.g, colour.b]), style)
                            .map_err(press_err)?;
                        if inside {
                            self.ex.pop_clip(self.page).map_err(press_err)?;
                        }
                    }
                }
            }
        }
        self.state(1.0, BlendMode::Normal)
    }

    fn image(&mut self, img: &StudioImagePaint, geo: &[(Vec<PathCommand>, FillRule, &OwnedPath)]) -> Result<()> {
        let Some(src) = self.images else {
            self.lose("export.image", "image_paint_source_not_supplied");
            return Ok(());
        };
        let Some(dec) = src.decoded(&img.image) else {
            self.lose("export.image", "image_paint_not_decoded");
            return Ok(());
        };
        let paths: Vec<&OwnedPath> = geo.iter().map(|g| g.2).collect();
        let Some(b) = bbox(&paths) else { return Ok(()) };
        let (bx, by, bw, bh) = (b[0], b[1], b[2] - b[0], b[3] - b[1]);
        let (iw, ih) = (f64::from(dec.width), f64::from(dec.height));
        if !(iw > 0.0 && ih > 0.0 && bw > 0.0 && bh > 0.0) {
            self.lose("export.image", "image_paint_empty");
            return Ok(());
        }
        // Image unit square (v up, PDF) -> document px.
        let doc: [f64; 6] = match img.scale_mode {
            StudioImageScale::Fill | StudioImageScale::Fit => {
                let k = if img.scale_mode == StudioImageScale::Fill { (bw / iw).max(bh / ih) } else { (bw / iw).min(bh / ih) };
                let (dw, dh) = (iw * k, ih * k);
                let (dx, dy) = (bx + (bw - dw) / 2.0, by + (bh - dh) / 2.0);
                [dw, 0.0, 0.0, -dh, dx, dy + dh]
            }
            StudioImageScale::Crop => {
                // Folio: T maps the unit image square (v down) to the unit square of the bounds.
                let [a, b2, c, d, e, f] = img.transform;
                // (u, v_up) -> v_down = 1 - v_up -> T -> bounds.
                let t = [a, b2, -c, -d, c + e, d + f];
                [t[0] * bw, t[1] * bh, t[2] * bw, t[3] * bh, bx + t[4] * bw, by + t[5] * bh]
            }
            StudioImageScale::Tile => {
                self.lose("export.image", "image_tile_not_exported");
                return Ok(());
            }
        };
        let placement = self.page_affine(doc);
        let spec = ImageSpec {
            width: dec.width,
            height: dec.height,
            bits: 8,
            colour: ImageColour::DeviceRgb,
            samples: &dec.rgb,
            alpha: dec.alpha.as_deref(),
        };
        for (cmds, rule, _) in geo {
            self.ex.push_clip(self.page, cmds, *rule).map_err(press_err)?;
        }
        if (img.opacity - 100.0).abs() > 1e-9 {
            self.lose("export.image.opacity", "image_opacity_not_exported");
        }
        self.ex.draw_image(self.page, spec, placement).map_err(press_err)?;
        for _ in geo {
            self.ex.pop_clip(self.page).map_err(press_err)?;
        }
        Ok(())
    }

    fn text(&mut self, l: &StudioLayer) -> Result<()> {
        let Some(t) = self.texts.get(l.layer_id.as_str()).copied() else {
            self.lose("export.text", "text_without_layout");
            return Ok(());
        };
        if t.glyphs.is_empty() {
            self.lose("export.text", "text_without_glyph_outlines");
            return Ok(());
        }
        self.lose("export.text", "text_as_outlines");
        for g in &t.glyphs {
            let Some(path) = &g.path else { continue };
            let fill = t
                .runs
                .iter()
                .find(|r| r.start <= g.start && g.start < r.end)
                .and_then(|r| r.fill_rgba);
            let Some([r, gg, b, a]) = fill else {
                self.lose("export.text.fill", "text_fill_not_solid");
                continue;
            };
            self.state(a.clamp(0.0, 1.0), BlendMode::Normal)?;
            let cmds = self.commands(path);
            self.ex
                .fill_path(self.page, &cmds, Self::rule(path.fill_rule), Colour::DeviceRgb([r, gg, b]))
                .map_err(press_err)?;
        }
        self.state(1.0, BlendMode::Normal)
    }
}

/// Writes the plan's frame as a one-page PDF (press `PdfExport`). The page is the frame bounds
/// times the plan scale; the frame's own content (its background, children) is drawn in paint
/// order.
pub fn export_frame_pdf(
    p: &NativeProposal,
    plan: &FigExportPlan,
    images: Option<&dyn FigImageSource>,
    cancel: &CancellationToken,
) -> Result<FigPdfExport> {
    cancel.check().map_err(|_| FigError::Canceled)?;
    let [x0, y0, w, h] = plan.bounds;
    let s = plan.scale;
    if !(s.is_finite() && s > 0.0 && s <= MAX_EXPORT_SCALE && w * s <= MAX_EXPORT_SIDE_PT && h * s <= MAX_EXPORT_SIDE_PT) {
        return Err(FigError::ExportRefused("export_scale_out_of_range"));
    }
    let limits = hsk_studio_press::Limits::default();
    let title = plan.file_name.clone();
    let mut ex = PdfExport::new(ExportOptions { info_title: Some(&title), ..ExportOptions::default() }, &limits).map_err(press_err)?;
    let page = ex
        .add_page(PageSpec { media_box_pt: [0.0, 0.0, w * s, h * s], trim_box_pt: None, bleed_box_pt: None })
        .map_err(press_err)?;
    let doc = p.snapshot.document();
    let mut children: HashMap<&str, Vec<&StudioLayer>> = HashMap::new();
    for l in &doc.layers {
        if let Parent::Node(n) = &l.parent {
            children.entry(n.node_id.as_str()).or_default().push(l);
        }
    }
    let mut lower = Lower {
        p,
        ex,
        page,
        origin: [x0, y0],
        height: h,
        scale: s,
        images,
        children,
        paths: p.paths.iter().map(|x| (x.path_id.as_str(), &x.geometry)).collect(),
        texts: p.text_layouts.iter().map(|t| (t.layer_id.as_str(), t)).collect(),
        loss: BTreeMap::new(),
        cancel,
    };
    if plan.frame.node_kind == NodeKind::Layer {
        let frame = doc
            .layers
            .iter()
            .find(|l| l.layer_id == plan.frame.node_id)
            .ok_or(FigError::ExportRefused("export_frame_not_mapped"))?;
        // The frame's own appearance belongs to its parent's composition, not to its export.
        let mut own = frame.clone();
        own.appearance = None;
        own.visible = true;
        lower.layer(&own)?;
    } else {
        lower.children_of(&plan.frame.node_id)?;
    }
    let Lower { ex, loss, .. } = lower;
    let (bytes, receipt) = ex.finish(cancel).map_err(press_err)?;
    Ok(FigPdfExport { bytes, receipt, loss: aggregate(loss) })
}

/// The exported page rendered back by press's independent interpreter (`render_page`) at one
/// device pixel per point (the plan scale is already in the page size): the raster of a PNG/JPEG
/// export, opaque white where nothing draws.
pub fn render_frame_export(pdf: &FigPdfExport, cancel: &CancellationToken) -> Result<RenderedPage> {
    hsk_studio_press::render_page(&pdf.bytes, 0, 1.0, &hsk_studio_press::Limits::default(), cancel).map_err(press_err)
}
