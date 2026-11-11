//! Layout graphic frames (lane SMB-LAYOUT-PLACE-1; [STU-LAY-028], [STU-LAY-029], [STU-LAY-131], [STU-LAY-132],
//! [STU-RAS-012], [STU-ASSET-007], [STU-ASSET-009]): a picture placed into a graphic frame on a layout page, its
//! fit, its link state on export. A child module of `view::place` (it shares that module's raster records entry,
//! decode, source sharing and tile staging); the manual is `view::place`'s module docs, section "Layout pages".
//! egui-free: every entry point is a [`Viewer`] method.
//!
//! Records: the picture is one raster layer whose parent is the page's spread, holding the SOURCE at its own pixels
//! (tiles from (0, 0), no resample, no canvas clip) and the Folio `placed_asset` record with the identity transform
//! (the frame is the one placement authority, no second transform); the frame is one Folio `StudioGraphicFrame` in
//! the layout records' long-document records. Both commit as ONE step (`Run::submit_layout_and_entry`).
#![deny(clippy::arithmetic_side_effects)]

use super::super::layout_export::PAGE_SIDE_PT;
use super::super::layers::new_layer;
use super::{Entry, Instance, MAX_PLACE_PIXELS, MAX_SOURCE_BYTES, PlaceOptions, PlaceRefusal, PlacementKind, Res, SourceImage, ENTRY};
use super::{folio_refusal, label, mint_placed_layer_id, no, relative_uri};
use super::Viewer;
use hsk_studio_folio::{
    self as folio, Bounds, GeometryUnit, LayerAppearance, LayoutEdges, Length, NodeKind, NodeRef, Parent, Payload, StudioDocument, StudioFit, StudioFitAnchor, StudioFrameShape,
    StudioGraphicFrame, StudioPlacedAsset, StudioPlacedMode, StudioRasterLayerRecord, TileRef,
};
use hsk_studio_layout::{FitError, PlacedGraphic, place_graphic};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_session::{LAYOUT_RECORDS_ENTRY, LayoutRecords};
use serde_json::{Value, json};
use std::path::Path;

/// The transform of a layout placement's record and instance: the frame places the pixels.
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// Content size at 100 % when the source states no resolution (pt per pixel = 72 / ppi).
pub const DEFAULT_PPI: f64 = 72.0;
/// Stated resolutions taken from a source: 1 to 100000 ppi.
const PPI_RANGE: (f64, f64) = (1.0, 100_000.0);

/// The frame of `asset.place` on a document with pages only; every field optional (`page` and the four numbers
/// are required there: `frame_required`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameOptions {
    pub page: Option<String>,
    /// Frame bounds, pt, page coordinates.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub shape: Option<FrameShape>,
    pub fit: Option<StudioFit>,
    /// The stored rule `apply_frame_fitting_options` re-applies.
    pub frame_fitting: Option<StudioFit>,
    pub anchor: Option<StudioFitAnchor>,
}

/// [STU-LAY-028] frame shape tokens of `asset.place`; `path` needs a vector path id and refuses in this slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameShape {
    Rectangle,
    Ellipse,
    Path,
}

impl FrameShape {
    pub const ALL: [(&'static str, FrameShape); 3] = [("rectangle", Self::Rectangle), ("ellipse", Self::Ellipse), ("path", Self::Path)];
}

/// Options of [`Viewer::fit_graphic`] beside its fit; absent fields keep the frame's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitOptions {
    pub anchor: Option<StudioFitAnchor>,
    /// Crop amounts, pt (positive trims the content, negative adds space); a side not given keeps its value.
    pub crop_top: Option<f64>,
    pub crop_left: Option<f64>,
    pub crop_bottom: Option<f64>,
    pub crop_right: Option<f64>,
    pub frame_fitting: Option<StudioFit>,
}

/// A document with pages and no artboard: `asset.place` places into a graphic frame.
pub(super) fn pages_only(doc: &StudioDocument) -> bool {
    doc.artboards.is_empty() && !doc.page_spreads.is_empty()
}

/// The pixels per inch a source states to the import route, when it states them: a TIFF's XResolution and
/// YResolution with ResolutionUnit inch (2) or centimetre (3) (import receipt `tiff.page.resolution`). PNG, JPEG
/// and PSD sources state none there (their decoders record no density), so they take [`DEFAULT_PPI`].
pub(super) fn stated_ppi(receipt: &Value) -> Option<[f64; 2]> {
    let r = &receipt["tiff"]["page"]["resolution"];
    let per_inch = match r["unit"].as_u64()? {
        2 => 1.0,
        3 => 2.54,
        _ => return None,
    };
    let axis = |k: &str| -> Option<f64> {
        let n = r[k].get(0)?.as_f64()?;
        let d = r[k].get(1)?.as_f64()?;
        let v = n / d * per_inch;
        (v.is_finite() && (PPI_RANGE.0..=PPI_RANGE.1).contains(&v)).then_some(v)
    };
    Some([axis("x")?, axis("y")?])
}

/// Pixel count a PNG or PSD/PSB header declares, read before any decode (other formats: the decoder's own bounds,
/// then the count after decode).
fn declared_pixels(bytes: &[u8]) -> Option<u64> {
    let be = |at: usize| -> Option<u64> {
        let b = bytes.get(at..at.checked_add(4)?)?;
        Some(u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR".as_slice()) {
        return be(16)?.checked_mul(be(20)?);
    }
    if bytes.starts_with(b"8BPS") {
        // Height at 14, width at 18 (PSD and PSB share the header).
        return be(14)?.checked_mul(be(18)?);
    }
    None
}

/// The checked frame of `asset.place`.
struct Frame {
    page_id: String,
    spread_id: String,
    bounds: [f64; 4],
    shape: StudioFrameShape,
    fit: Option<StudioFit>,
    frame_fitting: Option<StudioFit>,
    anchor: StudioFitAnchor,
}

/// A fit member legal on a graphic frame ([STU-LAY-131]): `content_aware` is unavailable in this build (typed,
/// as the spec requires), `honour_existing_style` is data merge only.
fn legal(fit: Option<StudioFit>) -> Res<()> {
    match fit {
        Some(StudioFit::ContentAware) => no("content_aware_unavailable", "content-aware fitting needs content analysis this build does not have"),
        Some(StudioFit::HonourExistingStyle) => no("fit_not_legal_here", "honour_existing_style is a data-merge fit"),
        _ => Ok(()),
    }
}

/// The stored rule a frame keeps: a member `apply_frame_fitting_options` can re-apply.
fn legal_stored(fit: Option<StudioFit>) -> Res<()> {
    legal(fit)?;
    if fit == Some(StudioFit::ApplyFrameFittingOptions) {
        return no("fit_not_legal_here", "the stored rule is a fit, not apply_frame_fitting_options");
    }
    Ok(())
}

/// `apply_frame_fitting_options` is the stored rule; the record keeps the member applied.
fn resolve(fit: Option<StudioFit>, stored: Option<StudioFit>) -> Res<Option<StudioFit>> {
    match fit {
        Some(StudioFit::ApplyFrameFittingOptions) => stored.map(Some).ok_or_else(|| PlaceRefusal::new("no_stored_fitting", "the frame has no stored fitting rule (frame_fitting)")),
        other => Ok(other),
    }
}

fn fit_refusal(e: FitError) -> PlaceRefusal {
    match e {
        FitError::Layout(_) => PlaceRefusal::new("frame_geometry_invalid", e.code()),
        other => PlaceRefusal::new(other.code(), "fitting"),
    }
}

fn side_ok(v: f64) -> bool {
    v.is_finite() && v > 0.0 && v <= PAGE_SIDE_PT.1
}

fn check_frame(f: &FrameOptions, doc: &StudioDocument) -> Res<Frame> {
    let (Some(page), Some(x), Some(y), Some(w), Some(h)) = (&f.page, f.x, f.y, f.width, f.height) else {
        return no("frame_required", "a layout page takes a picture into a frame: page, frame_x, frame_y, frame_width and frame_height (pt, page coordinates)");
    };
    let at_ok = |v: f64| v.is_finite() && v.abs() <= PAGE_SIDE_PT.1;
    if !(at_ok(x) && at_ok(y) && side_ok(w) && side_ok(h)) {
        return no("frame_out_of_range", format!("frame sides 0 < side <= {} pt, |x|, |y| <= {} pt, every value finite", PAGE_SIDE_PT.1, PAGE_SIDE_PT.1));
    }
    let loc = doc.locate_page(page).ok_or_else(|| PlaceRefusal::new("unknown_page", page.clone()))?;
    let shape = match f.shape.unwrap_or(FrameShape::Rectangle) {
        FrameShape::Rectangle => StudioFrameShape::Rectangle,
        FrameShape::Ellipse => StudioFrameShape::Ellipse,
        FrameShape::Path => return no("frame_shape_unsupported", "a path frame needs a vector path id (a later slice); rectangle or ellipse"),
    };
    legal(f.fit)?;
    legal_stored(f.frame_fitting)?;
    let fit = resolve(f.fit, f.frame_fitting)?;
    Ok(Frame { page_id: page.clone(), spread_id: loc.spread.page_spread_id.clone(), bounds: [x, y, w, h], shape, fit, frame_fitting: f.frame_fitting, anchor: f.anchor.unwrap_or_default() })
}

fn pt(value: f64) -> Length {
    Length { value, unit: GeometryUnit::Pt }
}

fn bounds_json(b: &Bounds) -> Value {
    json!({"x": b.x.value, "y": b.y.value, "width": b.width.value, "height": b.height.value})
}

/// A Folio enum's wire token (`serde_json::to_value` of it), null when it does not encode.
fn token(v: serde_json::Result<Value>) -> Value {
    v.unwrap_or(Value::Null)
}

/// The extent of a layer's tile grid in pixels (x, y): the PDF lowering maps the grid's extent to the content size,
/// so a layout PDF scales the record's content size by `extent / source pixels` (tiles are 256-pixel cells).
fn tile_extent(tiles: &[TileRef]) -> Option<[f64; 2]> {
    let mut e = [0.0f64; 2];
    for t in tiles {
        let (c, r) = (t.column.checked_add(1)?, t.row.checked_add(1)?);
        #[allow(clippy::cast_precision_loss)]
        let (cx, cy) = (c as f64 * t.width.value, r as f64 * t.height.value);
        e = [e[0].max(cx), e[1].max(cy)];
    }
    (e[0] > 0.0 && e[1] > 0.0 && e.iter().all(|v| v.is_finite())).then_some(e)
}

impl Viewer {
    /// The graphic frames of the session's layout records (decoded, bounded; none without an entry).
    pub(super) fn graphic_frames_now(&self) -> Vec<StudioGraphicFrame> {
        self.run
            .document_entry(LAYOUT_RECORDS_ENTRY)
            .and_then(|b| LayoutRecords::decode(b, folio::Budget::default(), self.cancel).ok())
            .map(|r| r.longdoc.graphic_frames)
            .unwrap_or_default()
    }

    /// `layer_id` is a layout graphic frame's layer.
    pub(super) fn is_graphic_frame(&self, layer_id: &str) -> bool {
        self.graphic_frames_now().iter().any(|g| g.layer_id == layer_id)
    }

    /// The source at its own pixels as new tiles of `layer_id` (no resample, no clip).
    pub(super) fn source_tiles(&mut self, layer_id: &str, source: &SourceImage) -> Res<Vec<TileRef>> {
        let src = Rect { x: 0, y: 0, width: source.width, height: source.height };
        self.resampled_tiles(layer_id, src, &source.pixels, Affine2::IDENTITY, ResampleFilter::NearestNeighbour, None)
    }

    /// A placement row's layout fields: page, frame, fit, content size and the derived resolutions ([STU-LAY-132]:
    /// actual ppi = source pixels per inch of content at 100 %, effective ppi = actual / the fit's scale).
    pub(super) fn frame_fields(&self, frames: &[StudioGraphicFrame], entry: &Entry, layer_id: &str, row: &mut Value) {
        let Some(g) = frames.iter().find(|g| g.layer_id == layer_id) else { return };
        let Some(doc) = self.run.snapshot().map(|s| s.document()) else { return };
        let source = entry.placed(layer_id).and_then(|a| entry.records.placed_source(&a.source_id));
        let (cw, ch) = (g.content_width.value, g.content_height.value);
        let actual = source.map(|s| [s.width * DEFAULT_PPI / cw, s.height * DEFAULT_PPI / ch]);
        let scale = place_graphic(doc, g).ok().map(|p| [p.transform[0], p.transform[3]]);
        let effective = actual.zip(scale).map(|(a, s)| [a[0] / s[0], a[1] / s[1]]);
        row["page_id"] = json!(g.page_id);
        row["frame"] = bounds_json(&g.bounds);
        row["shape"] = token(serde_json::to_value(&g.shape));
        row["fit"] = token(serde_json::to_value(&g.fit));
        row["frame_fitting"] = token(serde_json::to_value(&g.frame_fitting));
        row["anchor"] = token(serde_json::to_value(&g.anchor));
        row["content_width_pt"] = json!(cw);
        row["content_height_pt"] = json!(ch);
        row["actual_ppi"] = json!(actual);
        row["effective_ppi"] = json!(effective);
    }

    /// The layout editor's state as the session holds it: its document and the layout records it commits.
    fn layout_state(&mut self) -> Res<(StudioDocument, LayoutRecords)> {
        self.layout_sync();
        let ed = self.layout_editor().ok_or_else(|| PlaceRefusal::new("layout_editor_unavailable", "the layout editor did not open"))?;
        let (doc, rec) = (ed.doc(), ed.records());
        let records = LayoutRecords {
            document_id: doc.document.document_id.clone(),
            stories: doc.stories.clone(),
            flow: doc.flow.clone(),
            styles: doc.styles.clone(),
            parents: rec.parents.clone(),
            longdoc: rec.longdoc.clone(),
        };
        Ok((doc.document.clone(), records))
    }

    /// `asset.place` on a document with pages only (module docs of `view::place`, "Layout pages").
    pub(super) fn place_in_frame(&mut self, path: &Path, kind: PlacementKind, opts: &PlaceOptions, doc: StudioDocument) -> Res<Value> {
        const OP: &str = "asset.place";
        let frame = check_frame(&opts.frame, &doc)?;
        if opts.x.is_some() || opts.y.is_some() || opts.scale_pct.is_some() {
            return no("parameter_not_for_frame", "x, y and scale_pct place on an artboard; a frame places its content by its fit");
        }
        if opts.opacity_pct.is_some_and(|o| !(0.0..=100.0).contains(&o)) {
            return no("parameter_out_of_range", "opacity_pct 0..=100");
        }
        let filter = opts.resample.unwrap_or(ResampleFilter::Bicubic);
        let uri = match kind {
            PlacementKind::Linked => Some(relative_uri(&self.link_base()?, path)?),
            PlacementKind::Embedded => None,
        };
        let bytes = super::super::commands::read_bounded(path, MAX_SOURCE_BYTES).map_err(|r| PlaceRefusal::new(&r.code, r.detail.to_string()))?;
        if let Some(n) = declared_pixels(&bytes).filter(|n| *n > MAX_PLACE_PIXELS) {
            return no("budget_exceeded", format!("the header declares {n} pixels; at most {MAX_PLACE_PIXELS}"));
        }
        let digest = format!("sha256:{}", hsk_studio_package::sha256_hex_of(&bytes));
        let image = self.decode_source(&bytes)?;
        let pixels = u64::from(image.width).saturating_mul(u64::from(image.height));
        if pixels > MAX_PLACE_PIXELS {
            return no("budget_exceeded", format!("{} x {} pixels; at most {MAX_PLACE_PIXELS}", image.width, image.height));
        }
        let (ppi, ppi_source) = match image.ppi {
            Some(p) => (p, "decoder"),
            None => ([DEFAULT_PPI; 2], "default"),
        };
        let (w, h) = (f64::from(image.width), f64::from(image.height));
        let content = [w * DEFAULT_PPI / ppi[0], h * DEFAULT_PPI / ppi[1]];
        let (mut document, mut records) = self.layout_state()?;
        if records.longdoc.graphic_frames.len() >= folio::MAX_GRAPHIC_FRAMES {
            return no("budget_exceeded", format!("a document holds at most {} graphic frames", folio::MAX_GRAPHIC_FRAMES));
        }
        let mut entry = self.editable_entry(&doc)?;
        let name = label(&opts.name.clone().unwrap_or_else(|| path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        let (source_id, shared) = self.source_for(&mut entry, &doc, uri.as_ref(), &digest, bytes, (w, h), &name)?;
        let layer_id = mint_placed_layer_id(&doc, &entry);
        let tiles = self.source_tiles(&layer_id, &image)?;
        let count = tiles.len();
        let parent = Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: frame.spread_id.clone() });
        let mut layer = new_layer(&layer_id, &name, &parent, "raster", json!({"payload_kind": "raster", "tiles": tiles})).map_err(|e| PlaceRefusal::new("layer_invalid", e))?;
        if let Some(o) = opts.opacity_pct {
            layer.appearance = Some(LayerAppearance { opacity: o, ..LayerAppearance::identity(false) });
        }
        let mut record = StudioRasterLayerRecord::new(layer_id.clone());
        record.placed_asset = Some(StudioPlacedAsset { source_id: source_id.clone(), transform: IDENTITY });
        entry.records.layers.push(record);
        entry.instances.push(Instance { layer_id: layer_id.clone(), placed: IDENTITY, resample: filter });
        document.layers.push(layer);
        folio::validate_raster_records(&entry.records, &document, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let [x, y, fw, fh] = frame.bounds;
        let g = StudioGraphicFrame {
            layer_id: layer_id.clone(),
            page_id: frame.page_id.clone(),
            bounds: Bounds { x: pt(x), y: pt(y), width: pt(fw), height: pt(fh) },
            shape: frame.shape.clone(),
            content_width: pt(content[0]),
            content_height: pt(content[1]),
            fit: frame.fit,
            frame_fitting: frame.frame_fitting,
            anchor: frame.anchor,
            crop: None,
        };
        let placed = place_graphic(&document, &g).map_err(fit_refusal)?;
        records.longdoc.graphic_frames.push(g.clone());
        let body = entry.to_bytes()?;
        if !self.run.submit_layout_and_entry(OP, OP, document, records, ENTRY, body) {
            return Err(self.run_refusal(OP));
        }
        self.selected = Some(layer_id.clone());
        if uri.is_some() {
            self.place.links.insert(source_id.clone(), "resolved");
        }
        self.layout_sync();
        let revision = self.revision_now();
        self.say(format!("{OP} {} into a frame on {} as {layer_id} ({count} tile(s), revision {revision})", kind.token(), frame.page_id));
        Ok(json!({
            "layer_id": layer_id, "source_id": source_id, "placement_kind": kind.token(), "uri": uri, "tiles": count, "shared": shared,
            "page_id": frame.page_id, "frame": bounds_json(&g.bounds), "shape": token(serde_json::to_value(&g.shape)), "fit": token(serde_json::to_value(&g.fit)), "frame_fitting": token(serde_json::to_value(&g.frame_fitting)),
            "anchor": token(serde_json::to_value(&g.anchor)), "content_width_pt": content[0], "content_height_pt": content[1], "pixel_width": image.width, "pixel_height": image.height,
            "ppi": ppi, "ppi_source": ppi_source, "transform": placed.transform, "resample": filter.key(), "revision": revision,
        }))
    }

    /// `asset.fit` (module docs of `view::place`, "Layout pages"): re-fits the graphic frame of `layer_id` through
    /// layout's `fit_content`; `frame_to_content` resizes the frame record at its anchor; one layout step. Returns
    /// `{layer_id, fit, frame, transform, content_box, revision}`.
    pub fn fit_graphic(&mut self, layer_id: &str, fit: StudioFit, opts: &FitOptions) -> Result<Value, PlaceRefusal> {
        self.place_reported("asset.fit", |v| v.fit_inner(layer_id, fit, opts))
    }

    fn fit_inner(&mut self, layer_id: &str, fit: StudioFit, opts: &FitOptions) -> Res<Value> {
        const OP: &str = "asset.fit";
        self.place_doc()?;
        let (document, mut records) = self.layout_state()?;
        let index = records.longdoc.graphic_frames.iter().position(|g| g.layer_id == layer_id).ok_or_else(|| PlaceRefusal::new("layer_not_graphic_frame", layer_id.to_owned()))?;
        let Some(mut g) = records.longdoc.graphic_frames.get(index).cloned() else { return no("layer_not_graphic_frame", layer_id.to_owned()) };
        legal(Some(fit))?;
        legal_stored(opts.frame_fitting)?;
        if let Some(ff) = opts.frame_fitting {
            g.frame_fitting = Some(ff);
        }
        if let Some(a) = opts.anchor {
            g.anchor = a;
        }
        let given = [opts.crop_top, opts.crop_right, opts.crop_bottom, opts.crop_left];
        if given.iter().any(Option::is_some) {
            if !given.iter().flatten().all(|v| v.is_finite() && v.abs() <= PAGE_SIDE_PT.1) {
                return no("parameter_out_of_range", format!("crop amounts finite, at most {} pt", PAGE_SIDE_PT.1));
            }
            let old = g.crop.as_ref().map_or([0.0; 4], |e| [e.top.value, e.right.value, e.bottom.value, e.left.value]);
            let [t, r, b, l] = [0, 1, 2, 3].map(|i| given.get(i).copied().flatten().or(old.get(i).copied()).unwrap_or(0.0));
            if g.content_width.value - l - r <= 0.0 || g.content_height.value - t - b <= 0.0 {
                return no("crop_empties_content", "the crop leaves no content box");
            }
            g.crop = Some(LayoutEdges { top: pt(t), right: pt(r), bottom: pt(b), left: pt(l) });
        }
        g.fit = resolve(Some(fit), g.frame_fitting)?;
        let placed = place_graphic(&document, &g).map_err(fit_refusal)?;
        if g.fit == Some(StudioFit::FrameToContent) {
            // The fitted frame is in spread coordinates; the record's bounds are the page's.
            let unfitted = place_graphic(&document, &StudioGraphicFrame { fit: None, ..g.clone() }).map_err(fit_refusal)?;
            let page_x = unfitted.frame.x0_pt - g.bounds.x.value;
            let f = placed.frame;
            let (w, h) = (f.x1_pt - f.x0_pt, f.y1_pt - f.y0_pt);
            if !(side_ok(w) && side_ok(h)) {
                return no("frame_out_of_range", format!("the fitted frame is {w} x {h} pt; each side at most {} pt", PAGE_SIDE_PT.1));
            }
            g.bounds = Bounds { x: pt(f.x0_pt - page_x), y: pt(f.y0_pt), width: pt(w), height: pt(h) };
        }
        if let Some(slot) = records.longdoc.graphic_frames.get_mut(index) {
            *slot = g.clone();
        }
        if !self.run.submit_layout(OP, OP, document, records) {
            return Err(self.run_refusal(OP));
        }
        self.layout_sync();
        let revision = self.revision_now();
        self.say(format!("{OP} {} on {layer_id} (revision {revision})", token(serde_json::to_value(&g.fit))));
        let b = placed.content_box;
        Ok(json!({
            "layer_id": layer_id, "fit": token(serde_json::to_value(&g.fit)), "frame_fitting": token(serde_json::to_value(&g.frame_fitting)), "anchor": token(serde_json::to_value(&g.anchor)), "frame": bounds_json(&g.bounds),
            "transform": placed.transform, "content_box": [b.x0_pt, b.y0_pt, b.x1_pt, b.y1_pt], "revision": revision,
        }))
    }

    /// What the layout PDF needs of the placed pictures ([STU-ASSET-009]): per graphic frame of `frames` its
    /// placement (layout `place_graphic`, the content size scaled to the layer's tile grid, [`tile_extent`]) and a
    /// receipt row `{layer_id, source_id, placement_kind, uri, content_digest, link_state}`. A linked placement
    /// whose file is `modified`, `missing`, `unauthorized` or `unsupported` refuses `placed_link_<state>` (an
    /// unauthorized one names its layer only, never its path), one without a document folder `link_base_unknown`.
    pub(in crate::view) fn layout_pdf_placements(&self, doc: &StudioDocument, frames: &[StudioGraphicFrame]) -> Result<(Vec<PlacedGraphic>, Vec<Value>), (&'static str, String)> {
        let entry = match self.run.snapshot().filter(|_| !frames.is_empty()) {
            Some(s) => Some(self.editable_entry(s.document()).map_err(|e| ("raster_records_invalid", e.detail))?),
            None => None,
        };
        let (mut graphics, mut rows) = (Vec::with_capacity(frames.len()), Vec::new());
        for g in frames {
            let mut placed = place_graphic(doc, g).map_err(|e| (e.code(), g.layer_id.clone()))?;
            let asset = entry.as_ref().and_then(|e| e.placed(&g.layer_id).map(|a| (e, a)));
            if let Some((e, a)) = asset {
                let source = e.records.placed_source(&a.source_id).ok_or_else(|| ("placed_source_unresolved", a.source_id.clone()))?;
                let (kind, uri, digest, state) = match &source.location {
                    StudioPlacedMode::Embedded { content_digest, .. } => ("embedded", None, content_digest, "embedded"),
                    StudioPlacedMode::Linked { uri, content_digest, .. } => {
                        let state = self.link_state(uri, content_digest);
                        let refused = match state {
                            "resolved" => None,
                            "modified" => Some("placed_link_modified"),
                            "missing" => Some("placed_link_missing"),
                            "unauthorized" => Some("placed_link_unauthorized"),
                            "unsupported" => Some("placed_link_unsupported"),
                            _ => Some("link_base_unknown"),
                        };
                        if let Some(code) = refused {
                            let detail = if state == "unauthorized" { g.layer_id.clone() } else { format!("{} ({uri}); run asset.update_all_modified for a modified link", g.layer_id) };
                            return Err((code, detail));
                        }
                        ("linked", Some(uri.clone()), content_digest, state)
                    }
                };
                let layer = doc.layers.iter().find(|l| l.layer_id == g.layer_id);
                if let Some(Payload::Raster { tiles }) = layer.map(|l| &l.payload)
                    && let Some(extent) = tile_extent(tiles)
                    && source.width > 0.0
                    && source.height > 0.0
                {
                    placed.content_size = [placed.content_size[0] * extent[0] / source.width, placed.content_size[1] * extent[1] / source.height];
                }
                rows.push(json!({"layer_id": g.layer_id, "source_id": a.source_id, "placement_kind": kind, "uri": uri, "content_digest": digest, "link_state": state}));
            }
            graphics.push(placed);
        }
        Ok((graphics, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A viewer under a test context of its own (no asset-local test host needed) and a 16 x 8 PNG.
    fn viewer_and_picture(label: &str) -> (Viewer, std::path::PathBuf) {
        use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
        let actor = ActorContext::new("layout-place-unit", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let context = hsk_studio_harness_core::RunContext::new(actor, ClientSessionId::parse("layout-place-unit").unwrap(), std::sync::Arc::new(TransientCorrelation::new()));
        let mut v = Viewer::with_context(super::super::super::render::Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context);
        use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
        v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((200.0, 200.0))), None).unwrap();
        let dir = std::env::temp_dir().join(format!("hsk-layout-place-unit-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples = hsk_studio_interop_raster::Samples::U8((0..16 * 8).flat_map(|_| [200u8, 30, 30]).collect());
        let png = hsk_studio_interop_raster::encode_png(&hsk_studio_interop_raster::PngEncodeRequest {
            width: 16,
            height: 8,
            colour: hsk_studio_interop_raster::Colour::Rgb,
            samples: &samples,
            icc: None,
            compression: hsk_studio_interop_raster::PngCompression::Fast,
        })
        .unwrap();
        let path = dir.join("hero.png");
        std::fs::write(&path, png).unwrap();
        (v, path)
    }

    /// The brief's hostile case "a canceled token during decode and during export": typed `canceled`, nothing
    /// changed, nothing written; the export gate checks the token between frames (review F1 of fbf7f0cd).
    #[test]
    fn a_canceled_token_refuses_the_decode_and_the_export_and_changes_nothing() {
        let (mut v, path) = viewer_and_picture("cancel");
        let frame = FrameOptions { page: Some("page-1".into()), x: Some(10.0), y: Some(10.0), width: Some(32.0), height: Some(16.0), ..FrameOptions::default() };
        let opts = PlaceOptions { frame, ..PlaceOptions::default() };
        v.place_asset(&path, PlacementKind::Embedded, &opts).unwrap_or_else(|e| panic!("{e}"));
        let state = |v: &Viewer| {
            let doc = serde_json::to_value(v.run.snapshot().unwrap().document()).unwrap();
            (doc, v.run.document_entry(LAYOUT_RECORDS_ENTRY).map(<[u8]>::to_vec), v.run.document_entry(ENTRY).map(<[u8]>::to_vec), v.run.history()["undo"].clone())
        };
        let frames = v.graphic_frames_now();
        assert_eq!(frames.len(), 1);
        let before = state(&v);
        v.cancel.cancel();
        assert_eq!(v.place_asset(&path, PlacementKind::Embedded, &opts).unwrap_err().code, "canceled", "the decode");
        let doc = v.run.snapshot().unwrap().document().clone();
        assert_eq!(v.layout_pdf_placements(&doc, &frames).err().map(|e| e.0), Some("canceled"), "the export gate");
        let pdf = path.with_extension("pdf");
        assert_eq!(v.export_layout_pdf(&pdf, false, &super::super::super::layout_export::LayoutPdfSettings::default()).unwrap_err().code, "canceled", "the export job");
        assert!(!pdf.exists(), "nothing written");
        assert_eq!(state(&v), before, "nothing changed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_stated_tiff_resolution_is_read_and_others_take_none() {
        let r = |x: Value, y: Value, unit: u64| json!({"tiff": {"page": {"resolution": {"x": x, "y": y, "unit": unit}}}});
        assert_eq!(stated_ppi(&r(json!([300, 1]), json!([150, 1]), 2)), Some([300.0, 150.0]));
        assert_eq!(stated_ppi(&r(json!([100, 1]), json!([100, 1]), 3)), Some([254.0, 254.0]));
        assert_eq!(stated_ppi(&r(json!([300, 1]), json!([300, 1]), 1)), None, "no unit: no ppi");
        assert_eq!(stated_ppi(&r(json!([300, 0]), json!([300, 1]), 2)), None, "a zero denominator");
        assert_eq!(stated_ppi(&r(json!([0, 1]), json!([300, 1]), 2)), None, "out of range");
        assert_eq!(stated_ppi(&json!({"source_format": "png"})), None);
    }

    #[test]
    fn declared_pixels_come_from_the_header_only() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&9000u32.to_be_bytes());
        png.extend_from_slice(&9000u32.to_be_bytes());
        assert_eq!(declared_pixels(&png), Some(81_000_000));
        assert_eq!(declared_pixels(&png[..20]), None, "a cut header");
        let mut psd = b"8BPS\0\x01\0\0\0\0\0\0\0\x03".to_vec();
        psd.extend_from_slice(&4u32.to_be_bytes());
        psd.extend_from_slice(&5u32.to_be_bytes());
        assert_eq!(declared_pixels(&psd), Some(20));
        assert_eq!(declared_pixels(b"GIF89a"), None);
    }

    #[test]
    fn frames_are_checked_before_anything_is_read() {
        use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument, blank_document};
        let doc = blank_document(&NewDocument::new(DocumentProfile::Layout, Some((100.0, 100.0))), "SDOC-019abcde-0000-7000-8000-00000000a001").unwrap();
        assert!(pages_only(&doc));
        let ok = FrameOptions { page: Some("page-1".into()), x: Some(1.0), y: Some(1.0), width: Some(10.0), height: Some(10.0), ..FrameOptions::default() };
        assert!(check_frame(&ok, &doc).is_ok());
        let code = |f: FrameOptions| check_frame(&f, &doc).err().map(|e| e.code).unwrap_or_default();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(code(FrameOptions { x: Some(bad), ..ok.clone() }), "frame_out_of_range", "x {bad}");
            assert_eq!(code(FrameOptions { height: Some(bad), ..ok.clone() }), "frame_out_of_range", "height {bad}");
        }
        assert_eq!(code(FrameOptions { fit: Some(StudioFit::ApplyFrameFittingOptions), ..ok.clone() }), "no_stored_fitting");
        assert_eq!(code(FrameOptions { frame_fitting: Some(StudioFit::ApplyFrameFittingOptions), ..ok.clone() }), "fit_not_legal_here");
        let applied = check_frame(&FrameOptions { fit: Some(StudioFit::ApplyFrameFittingOptions), frame_fitting: Some(StudioFit::Proportionally), ..ok.clone() }, &doc).ok().map(|f| f.fit);
        assert_eq!(applied, Some(Some(StudioFit::Proportionally)), "the record keeps the member applied");
        assert_eq!(code(FrameOptions { width: None, ..ok }), "frame_required");
    }
}
