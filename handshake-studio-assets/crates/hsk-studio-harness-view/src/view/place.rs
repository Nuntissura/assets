//! Place embedded or linked assets into a raster or vector document, and raster Move and Free transform
//! (lane SMB-PLACE-ASSET-1; [STU-RAS-012], [STU-RAS-013], [STU-RAS-014], [STU-RAS-029], [STU-RAS-141],
//! [STU-RAS-142], [STU-ASSET-005]..[STU-ASSET-007], the [STU-ASSET-010] command names). Engine and commands
//! only: no draw code and no panel; a panel lane calls the typed rows `asset.*` and `transform.*`
//! (`view::commands::place`) or these methods.
//!
//! # Manual (no-context operator or model)
//!
//! Every method needs an open document with an artboard (raster or vector), or with pages only (a layout:
//! section "Layout pages" below). Each mutating call is
//! ONE undo step named by its command id, attributed to the viewer's run context (the caller's), and
//! survives save and reopen: the placed records live in the session document entry [`ENTRY`] beside the
//! layer's tiles. A refusal is a typed code and changes nothing.
//!
//! - [`Viewer::place_asset`] (`asset.place`): reads `path` (PNG, JPEG, TIFF, PSD/PSB or camera raw, at most
//!   [`MAX_SOURCE_BYTES`]) through the import route of `hsk-studio-harness-core::import` (its colour route),
//!   renders the source's composite and adds a raster layer above the selected layer holding a Folio
//!   `placed_asset` instance of a shared `StudioPlacedSource`. `placement_kind` `embedded` stages the file's
//!   bytes in the package (`StudioPlacedMode::Embedded`, a minted `SDOC` id, the bytes' sha256); `linked`
//!   records the path relative to the opened document's folder (`StudioPlacedMode::Linked`; the file must lie
//!   in that folder or below it, the rule of `hsk-studio-package`'s `LinkedFileResolver`). The same bytes
//!   (embedded) or the same file (linked) placed again share their source ([STU-RAS-013]). Without
//!   `scale_pct` the source is placed at 100 %, or scaled to fit the canvas when it is larger; without `x`,
//!   `y` (the top-left corner, document units) it is centred. `opacity_pct` and `name` set the new layer's.
//!   The layer's tiles are its cached display pixels: the source resampled through the instance transform
//!   ([STU-RAS-141] `resample`, default `bicubic`, recorded per instance) and clipped to the canvas.
//! - [`Viewer::free_transform`] (`transform.free_transform`): `scale_x`, `scale_y` (percent; negative flips),
//!   `angle` (degrees, -360..=360, clockwise on screen), `skew_x`, `skew_y` (degrees, -89.9..=89.9),
//!   `translate_x`, `translate_y` (document units), about `reference_point` of the layer's bounds (default
//!   `middle_centre`; a placed layer: the source's mapped bounds; a pixel layer: the bounds of its texels
//!   with alpha > 0, `layer_empty` without one). On a placed layer the transform accumulates on the record
//!   and the tiles are rendered again from the source, so the source is resampled once whatever the history
//!   ([STU-RAS-014]); on a pixel layer the pixels are resampled once into new tiles (no canvas clip). A
//!   filtered resample keeps linear values outside 0..=1 when the source holds any (float layers and
//!   sources; the tiles are `rgb_f32le`).
//!
//! Placed layers take ids from their own block (`7ede-9200`), so a layer another tool creates after a
//! placed layer is deleted never takes its id. Every structure step that removes layers takes their rows and placed
//! instances out of [`ENTRY`] in its own undo step (undo brings them back with the layers; lane
//! SMB-PS-FILTER-ADJUST-COMMANDS-1, lead decision 2026-11-13; lane SMB-FIX-LAYER-RECORD-PRUNE-1,
//! [`Viewer::raster_records_without`] through `view::layer_records`); rows of absent layers a package still holds are
//! dropped at open with a loss note ([`Viewer::place_opened`]) and are ignored at render and in validation
//! (pruned to the document's layers).
//! - [`Viewer::move_content`] (`transform.move`): a translation by `dx`, `dy`; on a pixel layer whole pixels
//!   only (an exact copy, `move_not_whole_pixels` otherwise).
//! - [`Viewer::reset_transform`] (`transform.reset`): a placed layer back to its transform as placed.
//! - [`Viewer::list_placements`] (`asset.list_placements`; `state_json()["placed"]` holds the same list with
//!   the link states of the last check): every placement with its source, mode, transform and link state
//!   ([STU-ASSET-007] `resolved`, `modified`, `missing`, `unauthorized`, `unsupported`; `embedded` when the
//!   package carries the bytes; `unresolved` without a document folder), checked now.
//! - [`Viewer::update_all_modified`] (`asset.update_all_modified`): every `modified` linked source is read
//!   again (digest and size recorded) and each of its instances rendered again; a `missing` one is recorded
//!   missing. One step for all of it; no step when nothing changed.
//! - [`Viewer::embed_link`] (`asset.embed_link`, FILE > Place > Embed Link): the linked source of the layer
//!   becomes embedded (its current bytes staged), for every instance of it.
//!
//! # Layout pages (lane SMB-LAYOUT-PLACE-1; [STU-LAY-028], [STU-LAY-029], [STU-LAY-131], [STU-LAY-132])
//!
//! On a document with pages and no artboard, [`Viewer::place_asset`] puts the picture into a graphic frame:
//! [`FrameOptions`] `page` and the frame `x`, `y`, `width`, `height` (pt, page coordinates; `frame_x`, ... on the
//! row) are required (`frame_required`: no hidden page-origin rule), `shape` `rectangle` (default) or `ellipse`
//! (`path` refuses `frame_shape_unsupported`), `fit` (Folio `StudioFit` tokens; none = 100 % at the frame's
//! top-left), `frame_fitting` (the stored rule), `anchor` (default `center`). `x`, `y` and `scale_pct` are
//! artboard parameters (`parameter_not_for_frame`); frame parameters on an artboard document refuse
//! `frame_needs_pages`. The picture is one raster layer of the page's spread holding the source at its own pixels
//! (no resample, no canvas clip: the PDF carries the full resolution) with the identity `placed_asset` transform,
//! and one Folio `StudioGraphicFrame` in the layout records; content size at 100 % = pixels x 72 / ppi, the ppi
//! the source states to the import route (a TIFF's resolution) else 72 (`ppi_source` `decoder` or `default`; PNG
//! pHYs and JPEG JFIF or Exif density are not read: the raster decoder exposes none, a declared cut).
//! Layer, frame and [`ENTRY`] are ONE undo step (`Run::submit_layout_and_entry`). [`Viewer::fit_graphic`]
//! (`asset.fit`): `fit`, `anchor`, crop amounts (pt; a side not given keeps its value) and `frame_fitting`
//! re-fit the frame through layout's `fit_content`; `frame_to_content` resizes the frame record at its anchor,
//! `apply_frame_fitting_options` applies the stored rule (`no_stored_fitting` without one) and records the member
//! applied; one layout step. `content_aware` refuses `content_aware_unavailable`, `honour_existing_style`
//! `fit_not_legal_here`, a crop leaving no content `crop_empties_content`, a layer without a frame
//! `layer_not_graphic_frame`. [`Viewer::list_placements`] adds `page_id`, `frame`, `shape`, `fit`,
//! `frame_fitting`, `anchor`, content size and the derived `actual_ppi` and `effective_ppi` (x, y). Update All
//! Links re-reads a layout source at its own pixels; its frames take the new content size (new pixels or ppi)
//! and their stored fit again (`frame_to_content` resizes the frame), tiles, frames and entry as ONE layout step;
//! a frame that cannot refit (`crop_empties_content`, `frame_out_of_range`, ...) leaves its source modified and is
//! listed in `refused`, the other sources update. Layers > Delete of a frame layer removes the layer and its frame
//! record as one layout step (undo restores both). Move, Free transform and Reset refuse
//! `graphic_frame_transform_unsupported` (the frame record places the pixels; frame move is a later slice). The
//! layout PDF draws every graphic frame and fails closed on a modified, missing, unauthorized or unsupported link
//! (`view::layout_export`).
//! Hostile bounds: frame sides 0 < side <= 14400 pt, |x|, |y| <= 14400 pt (`frame_out_of_range`), an unknown page
//! `unknown_page`, a PNG or PSD header declaring more than [`MAX_PLACE_PIXELS`] refuses `budget_exceeded` before
//! decode (any source after decode), at most `MAX_GRAPHIC_FRAMES` frames. Studio readings (UNVERIFIED against
//! InDesign, D5): 100 % at the source's ppi, a frame required, the modified-link export refusal. The viewer canvas
//! does not draw the picture: a layout document's layer graph is empty (`document.new` makes none and a frame layer
//! adds no operation), and the canvas renders the graph (UNVERIFIED by a run); the layout PDF is the placed
//! picture's output.
//!
//! Refusal codes: `not_open`, `canvas_unavailable`, `no_working_profile`, `resource_unreadable`,
//! `resource_too_large`, the import route's codes (`format_unrecognised`, ...), render codes
//! (`canvas_too_large`, ...), `placed_source_empty`, `link_base_unknown` (no opened file to resolve a link
//! against), `link_outside_document_folder`, `link_uri_invalid`, `layer_unknown`, `layer_not_pixel`,
//! `layer_has_mask`, `layer_empty`, `layer_not_placed`, `layer_not_linked`, `move_not_whole_pixels`,
//! `parameter_out_of_range`, pigment's `transform_*`, `budget_exceeded`, `placed_link_missing`,
//! `placed_link_modified` (run `asset.update_all_modified` first), `placed_link_unauthorized`,
//! `placed_link_unsupported`, `placed_source_size_mismatch`, `placed_source_unresolved`,
//! `raster_records_invalid`, `folio_<code>`, the layout codes above (`frame_required`, `frame_out_of_range`,
//! `frame_needs_pages`, `frame_shape_unsupported`, `parameter_not_for_frame`, `unknown_page`,
//! `frame_geometry_invalid`, `no_stored_fitting`, `content_aware_unavailable`, `fit_not_legal_here`,
//! `crop_empties_content`, `layer_not_graphic_frame`, `graphic_frame_transform_unsupported`,
//! `canceled`, `layout_editor_unavailable`), and the Run's own codes.
//!
//! Studio readings (UNVERIFIED against Photoshop and Illustrator, D5): 100 % unless larger than the canvas;
//! the reference point is on the axis-aligned bounds of the layer; skew before rotation; cached pixels
//! clipped to the canvas (the source keeps everything); a link resolves against the opened document's
//! folder, so a save into another folder leaves it `missing`. Cuts: Place from Asset Library (no CKC),
//! Relink, Unembed, Edit Original, Convert to Layers, Export Contents, statistical stacks, warp, perspective,
//! a native `.handshake` package as a source, the text and vector layers of a source document (drawn
//! without them), a duplicate of a placed layer as a shared instance (`duplicate_layer` copies the pixels
//! only), the export fail-closed gate for missing links ([STU-ASSET-009]) on artboard exports (the layout PDF
//! has it).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::commands::CommandRefusal;
use super::edit;
use super::layers::{new_layer, operation_for};
use super::render::{self, Assets};
use crate::PlacementWrite;
use hsk_studio_composite::lower_extensions::placed::placed_matrix;
use hsk_studio_folio::{
    self as folio, ContentDigest, LayerAppearance, Payload, Resolution, StudioDocument, StudioImageSource, StudioLinkHealth, StudioPlacedAsset, StudioPlacedMode,
    StudioPlacedSource, StudioRasterLayerRecord, StudioRasterRecords, TileRef,
};
use hsk_studio_package::{AssetSource, ImportEntry, ImportRole, LinkedFileResolver};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::SampleRange;
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, PreparedTransform, ResampleFilter, exact_sin_cos};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Layout graphic frames (lane SMB-LAYOUT-PLACE-1): a child module, so it shares this module's records and
/// pipeline without widening their visibility.
#[path = "layout_place.rs"]
mod layout_place;
pub use layout_place::{FitOptions, FrameOptions, FrameShape};

/// Session document entry holding the authored raster records (`hsk-studio-session` token).
pub const ENTRY: &str = "raster_records";
/// Schema of [`ENTRY`]: `{schema, records, sources[{source_id, name, bytes}], instances[{layer_id,
/// placed_transform, resample}]}` (records: the Folio `StudioRasterRecords`; `bytes`: a package-asset image
/// source of an embedded source's file, so the package keeps it).
pub const SCHEMA: &str = "hsk.harness.raster_records@1";
/// Largest source file read.
pub const MAX_SOURCE_BYTES: u64 = crate::import::MAX_SOURCE_BYTES;
/// Largest pixel area one transform reads or writes.
pub const MAX_PLACE_PIXELS: u64 = render::MAX_VIEW_PIXELS;
/// Most instances one entry holds.
pub const MAX_INSTANCES: usize = 65_536;
/// Longest source name kept.
const MAX_NAME_BYTES: usize = 128;

/// A refusal: a stable code and a detail for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceRefusal {
    pub code: String,
    pub detail: String,
}

impl PlaceRefusal {
    pub(super) fn new(code: &str, detail: impl Into<String>) -> Self {
        Self { code: code.to_owned(), detail: detail.into() }
    }
}

impl std::fmt::Display for PlaceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

type Res<T> = Result<T, PlaceRefusal>;

fn no<T>(code: &str, detail: impl Into<String>) -> Res<T> {
    Err(PlaceRefusal::new(code, detail))
}

/// `Code::DanglingReference` -> `folio_dangling_reference`.
fn folio_refusal(d: &folio::Diagnostic) -> PlaceRefusal {
    let mut code = String::from("folio");
    for ch in format!("{:?}", d.code).chars() {
        if ch.is_ascii_uppercase() {
            code.push('_');
        }
        code.push(ch.to_ascii_lowercase());
    }
    PlaceRefusal::new(&code, d.target.clone())
}

/// The failure code of an in-place job's settlement (`JobOutcome::Failed` takes a static code): the code itself when
/// it is one a map or the pixel path refuses with, else `adjustment_apply_failed` (the caller still gets the full
/// refusal).
fn static_code(code: &str) -> &'static str {
    const CODES: [&str; 9] =
        ["adjust_out_of_domain", "adjust_invalid_parameter", "adjust_unsupported", "adjust_blend", "budget_exceeded", "tile_invalid", "no_working_profile", "canceled", "transform_invalid"];
    CODES.iter().copied().find(|c| *c == code).unwrap_or("adjustment_apply_failed")
}

/// A refusal of this module as a command refusal (code kept, detail as `message`).
fn command_refusal(r: PlaceRefusal) -> CommandRefusal {
    CommandRefusal::new(&r.code, json!({"message": r.detail}))
}

/// Embedded or linked ([STU-ASSET-006] `placement_kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementKind {
    Linked,
    Embedded,
}

impl PlacementKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Embedded => "embedded",
        }
    }
}

/// Options of [`Viewer::place_asset`]; every field optional.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaceOptions {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub scale_pct: Option<f64>,
    pub name: Option<String>,
    pub opacity_pct: Option<f64>,
    pub resample: Option<ResampleFilter>,
    /// The graphic frame on a document with pages only (lane SMB-LAYOUT-PLACE-1; module docs).
    pub frame: FrameOptions,
}

/// [STU-RAS-141] reference point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    TopCentre,
    TopRight,
    MiddleLeft,
    #[default]
    MiddleCentre,
    MiddleRight,
    BottomLeft,
    BottomCentre,
    BottomRight,
}

impl Anchor {
    pub const ALL: [(&'static str, Anchor); 9] = [
        ("top_left", Self::TopLeft),
        ("top_centre", Self::TopCentre),
        ("top_right", Self::TopRight),
        ("middle_left", Self::MiddleLeft),
        ("middle_centre", Self::MiddleCentre),
        ("middle_right", Self::MiddleRight),
        ("bottom_left", Self::BottomLeft),
        ("bottom_centre", Self::BottomCentre),
        ("bottom_right", Self::BottomRight),
    ];

    /// Position in the bounds as fractions of width and height.
    fn fractions(self) -> (f64, f64) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::TopCentre => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::MiddleLeft => (0.0, 0.5),
            Self::MiddleCentre => (0.5, 0.5),
            Self::MiddleRight => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::BottomCentre => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

/// [STU-RAS-142] free transform parameters; the default changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeTransform {
    pub scale_x: f64,
    pub scale_y: f64,
    pub angle: f64,
    pub skew_x: f64,
    pub skew_y: f64,
    pub translate_x: f64,
    pub translate_y: f64,
    pub reference_point: Anchor,
    /// `None`: the instance's recorded method (a placed layer), else bicubic.
    pub resample: Option<ResampleFilter>,
}

impl Default for FreeTransform {
    fn default() -> Self {
        Self { scale_x: 100.0, scale_y: 100.0, angle: 0.0, skew_x: 0.0, skew_y: 0.0, translate_x: 0.0, translate_y: 0.0, reference_point: Anchor::MiddleCentre, resample: None }
    }
}

/// The viewer's place state: the import records of the open document (what the viewer renders while the
/// session holds no [`ENTRY`]), the link states of the last check by source id, and the opened document's
/// folder, canonical, captured at its open (review F3 of 9d83222c: `Viewer::source` also names an audio file
/// opened beside the document; `None` after `document.new` and close).
#[derive(Clone, Debug, Default)]
pub struct PlaceState {
    import: Option<StudioRasterRecords>,
    links: BTreeMap<String, &'static str>,
    base: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
struct SourceMeta {
    source_id: String,
    name: String,
    /// The embedded file's bytes in the package.
    bytes: Option<ContentDigest>,
}

#[derive(Clone, Debug, PartialEq)]
struct Instance {
    layer_id: String,
    /// The transform as placed ([`Viewer::reset_transform`]).
    placed: [f64; 6],
    resample: ResampleFilter,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    records: StudioRasterRecords,
    sources: Vec<SourceMeta>,
    instances: Vec<Instance>,
}

const RESAMPLE: [(&str, ResampleFilter); 3] =
    [("nearest_neighbour", ResampleFilter::NearestNeighbour), ("bilinear", ResampleFilter::Bilinear), ("bicubic", ResampleFilter::Bicubic)];

fn invalid_entry(what: &str) -> PlaceRefusal {
    PlaceRefusal::new("raster_records_invalid", what.to_owned())
}

impl Entry {
    fn new(records: StudioRasterRecords) -> Self {
        Self { records, sources: Vec::new(), instances: Vec::new() }
    }

    fn to_bytes(&self) -> Res<Vec<u8>> {
        let records = serde_json::to_value(&self.records).map_err(|e| invalid_entry(&e.to_string()))?;
        let mut sources = Vec::with_capacity(self.sources.len());
        for s in &self.sources {
            let bytes = match &s.bytes {
                Some(d) => serde_json::to_value(StudioImageSource::PackageAsset { content_digest: d.clone() }).map_err(|e| invalid_entry(&e.to_string()))?,
                None => Value::Null,
            };
            sources.push(json!({"source_id": s.source_id, "name": s.name, "bytes": bytes}));
        }
        let instances: Vec<Value> = self.instances.iter().map(|i| json!({"layer_id": i.layer_id, "placed_transform": i.placed, "resample": i.resample.key()})).collect();
        serde_json::to_vec(&json!({"schema": SCHEMA, "records": records, "sources": sources, "instances": instances})).map_err(|e| invalid_entry(&e.to_string()))
    }

    fn parse(bytes: &[u8]) -> Res<Self> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| invalid_entry("not JSON"))?;
        if v["schema"] != SCHEMA {
            return Err(invalid_entry("schema"));
        }
        let records: StudioRasterRecords = serde_json::from_value(v["records"].clone()).map_err(|_| invalid_entry("records"))?;
        let list = |key: &str, max: usize| -> Res<Vec<Value>> {
            let items = v[key].as_array().ok_or_else(|| invalid_entry(key))?;
            if items.len() > max {
                return Err(invalid_entry(key));
            }
            Ok(items.clone())
        };
        let mut sources = Vec::new();
        for s in list("sources", folio::MAX_PLACED_SOURCES)? {
            let text = |k: &str| s[k].as_str().filter(|t| t.len() <= MAX_NAME_BYTES).map(str::to_owned).ok_or_else(|| invalid_entry("sources"));
            let bytes = match &s["bytes"] {
                Value::Null => None,
                other => match serde_json::from_value::<StudioImageSource>(other.clone()) {
                    Ok(StudioImageSource::PackageAsset { content_digest }) => Some(content_digest),
                    _ => return Err(invalid_entry("sources/bytes")),
                },
            };
            sources.push(SourceMeta { source_id: text("source_id")?, name: text("name")?, bytes });
        }
        let mut instances = Vec::new();
        for i in list("instances", MAX_INSTANCES)? {
            let layer_id = i["layer_id"].as_str().ok_or_else(|| invalid_entry("instances"))?.to_owned();
            let numbers: Vec<f64> = i["placed_transform"].as_array().into_iter().flatten().filter_map(Value::as_f64).filter(|n| n.is_finite()).collect();
            let placed = <[f64; 6]>::try_from(numbers).map_err(|_| invalid_entry("instances/placed_transform"))?;
            let token = i["resample"].as_str().unwrap_or_default();
            let resample = RESAMPLE.iter().find(|(t, _)| *t == token).map(|(_, f)| *f).ok_or_else(|| invalid_entry("instances/resample"))?;
            instances.push(Instance { layer_id, placed, resample });
        }
        Ok(Self { records, sources, instances })
    }

    /// Drops the records and instances of layers `doc` does not hold (inert, as every entry's ids are).
    fn prune(&mut self, doc: &StudioDocument) {
        let present: BTreeSet<&str> = doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
        self.records.layers.retain(|l| present.contains(l.layer_id.as_str()));
        self.instances.retain(|i| present.contains(i.layer_id.as_str()));
    }

    fn placed(&self, layer_id: &str) -> Option<&StudioPlacedAsset> {
        self.records.layer(layer_id).and_then(|r| r.placed_asset.as_ref())
    }
}

/// A decoded source: straight linear-light RGBA in the working RGB primaries, row-major.
struct SourceImage {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
    /// Pixels per inch (x, y) when the source states them to the import route (lane SMB-LAYOUT-PLACE-1).
    ppi: Option<[f64; 2]>,
}

/// Pigment's row-major affine as Folio's `[a, b, c, d, e, f]` (inverse of `placed_matrix`).
fn folio_matrix(a: Affine2) -> [f64; 6] {
    let [m0, m1, m2, m3, m4, m5] = a.m;
    // `+ 0.0` turns a negative zero into zero, so a stored transform reads as it was computed.
    [m0, m3, m1, m4, m2, m5].map(|v| v + 0.0)
}

/// The axis-aligned bounds `[x0, y0, x1, y1]` of `src` mapped by `a`.
fn mapped_bounds(a: Affine2, src: Rect) -> [f64; 4] {
    let (x0, y0) = (src.x as f64, src.y as f64);
    let (x1, y1) = (x0 + f64::from(src.width), y0 + f64::from(src.height));
    let corners = [a.apply(x0, y0), a.apply(x1, y0), a.apply(x0, y1), a.apply(x1, y1)];
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in corners {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    b
}

/// [STU-RAS-142] free transform as a document-space affine about the reference point of `bounds`.
fn delta_matrix(t: &FreeTransform, bounds: [f64; 4]) -> Res<Affine2> {
    let all = [t.scale_x, t.scale_y, t.angle, t.skew_x, t.skew_y, t.translate_x, t.translate_y];
    if !all.iter().all(|v| v.is_finite()) || !(-360.0..=360.0).contains(&t.angle) || ![t.skew_x, t.skew_y].iter().all(|s| (-89.9..=89.9).contains(s)) {
        return no("parameter_out_of_range", "angle -360..=360, skew -89.9..=89.9, every value finite");
    }
    let (fx, fy) = t.reference_point.fractions();
    let ax = bounds[0] + (bounds[2] - bounds[0]) * fx;
    let ay = bounds[1] + (bounds[3] - bounds[1]) * fy;
    let (sin, cos) = exact_sin_cos(t.angle);
    let scale = Affine2 { m: [t.scale_x / 100.0, 0.0, 0.0, 0.0, t.scale_y / 100.0, 0.0] };
    let skew = Affine2 { m: [1.0, t.skew_x.to_radians().tan(), 0.0, t.skew_y.to_radians().tan(), 1.0, 0.0] };
    let rotate = Affine2 { m: [cos, -sin, 0.0, sin, cos, 0.0] };
    Ok(Affine2::translate(-ax, -ay).then(scale).then(skew).then(rotate).then(Affine2::translate(ax + t.translate_x, ay + t.translate_y)))
}

/// `path` relative to `base` (both canonical), forward slashes, inside `base`.
fn relative_uri(base: &Path, path: &Path) -> Res<String> {
    let target = std::fs::canonicalize(path).map_err(|e| PlaceRefusal::new("resource_unreadable", format!("{:?}", e.kind())))?;
    let rest = target.strip_prefix(base).map_err(|_| PlaceRefusal::new("link_outside_document_folder", "a linked file lies in the document's folder or below it"))?;
    let mut parts = Vec::new();
    for c in rest.components() {
        let Component::Normal(s) = c else { return no("link_uri_invalid", "path component") };
        parts.push(s.to_str().ok_or_else(|| PlaceRefusal::new("link_uri_invalid", "not UTF-8"))?.to_owned());
    }
    let uri = parts.join("/");
    if uri.is_empty() || uri.len() > MAX_NAME_BYTES || uri.contains([':', '\\']) || uri.chars().any(char::is_control) {
        return no("link_uri_invalid", "a relative path of at most 128 bytes without ':' or '\\'");
    }
    Ok(uri)
}

fn state_token(r: Resolution) -> &'static str {
    match r {
        Resolution::Available => "resolved",
        Resolution::HashMismatch => "modified",
        Resolution::Absent => "missing",
        Resolution::Unauthorized => "unauthorized",
        Resolution::Unsupported => "unsupported",
    }
}

/// `name` cut to [`MAX_NAME_BYTES`] at a character boundary, control characters replaced.
fn label(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().map(|c| if c.is_control() { '_' } else { c }) {
        if out.len().saturating_add(c.len_utf8()) > MAX_NAME_BYTES {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { "placed".to_owned() } else { out }
}

impl Viewer {
    fn place_doc(&self) -> Res<StudioDocument> {
        self.run.snapshot().map(|s| s.document().clone()).ok_or_else(|| PlaceRefusal::new("not_open", "nothing open"))
    }

    fn read_place_entry(&self) -> Res<Option<Entry>> {
        self.run.document_entry(ENTRY).map(Entry::parse).transpose()
    }

    /// The entry to edit: the stored one, else one holding the import records; pruned to the document.
    fn editable_entry(&self, doc: &StudioDocument) -> Res<Entry> {
        let mut entry = match self.read_place_entry()? {
            Some(e) => e,
            None => Entry::new(self.place.import.clone().unwrap_or_else(|| StudioRasterRecords::new(doc.document_id.clone()))),
        };
        entry.prune(doc);
        Ok(entry)
    }

    /// The raster record of `layer_id` in the editable entry (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, the reads of
    /// `view::raster_effects`); `None` when the layer has none.
    pub(super) fn editable_raster_record(&self, layer_id: &str) -> Result<Option<StudioRasterLayerRecord>, CommandRefusal> {
        let doc = self.place_doc().map_err(command_refusal)?;
        let entry = self.editable_entry(&doc).map_err(command_refusal)?;
        Ok(entry.records.layer(layer_id).cloned())
    }

    /// ONE entry-only undo step named `op` (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, the entry-edit helper of
    /// `view::raster_effects`): `edit` changes the records of the editable entry (it refuses an edit that changes
    /// nothing), Folio validates the result against the document, and the entry is committed with
    /// `Run::set_tiles_and_entry` (no tile change), attributed to the viewer's run context. A refusal changes nothing.
    /// The result is `edit`'s value plus `revision`.
    pub(super) fn edit_raster_records(
        &mut self,
        op: &str,
        layer_id: &str,
        edit: impl FnOnce(&mut StudioRasterRecords, &StudioDocument) -> Result<Value, CommandRefusal>,
    ) -> Result<Value, CommandRefusal> {
        let result = (|| -> Result<Value, CommandRefusal> {
            let doc = self.place_doc().map_err(command_refusal)?;
            let mut entry = self.editable_entry(&doc).map_err(command_refusal)?;
            let mut out = edit(&mut entry.records, &doc)?;
            folio::validate_raster_records(&entry.records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| command_refusal(folio_refusal(&d)))?;
            let body = entry.to_bytes().map_err(command_refusal)?;
            if !self.run.set_tiles_and_entry(op, layer_id, None, ENTRY, body) {
                return Err(command_refusal(self.run_refusal(op)));
            }
            out["revision"] = json!(self.revision_now());
            Ok(out)
        })();
        self.effects_reported(op, result)
    }

    /// ONE undo step creating a layer and its raster record together (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, the
    /// adjustment layer of `view::raster_effects`): `make` gets a fresh layer id from this module's own block
    /// (`7ede-9300`, never one the document or the entry's records hold, so a deleted layer's record is never
    /// inherited; the review F1 rule of [`mint_placed_layer_id`]) and returns the layer's kind, payload,
    /// appearance and record. The layer goes above the selected layer (`layers` insertion point), Folio validates
    /// the records against the document with the layer, and layer and entry commit as one step
    /// (`Run::restructure_and_entry`). The new layer is selected. Result: `{layer_id, revision}`.
    pub(super) fn create_layer_with_record(
        &mut self,
        op: &str,
        name: &str,
        make: impl FnOnce(&str) -> Result<(&'static str, Value, Option<LayerAppearance>, StudioRasterLayerRecord), CommandRefusal>,
    ) -> Result<Value, CommandRefusal> {
        let result = (|| -> Result<Value, CommandRefusal> {
            let doc = self.place_doc().map_err(command_refusal)?;
            let mut entry = self.editable_entry(&doc).map_err(command_refusal)?;
            let layer_id = mint_entry_layer_id(&doc, &entry, "9300");
            let (kind, payload, appearance, record) = make(&layer_id)?;
            let (parent, place) = self.insertion_point().map_err(|e| CommandRefusal::new("not_open", json!({"message": e})))?;
            let mut layer = new_layer(&layer_id, name, &parent, kind, payload).map_err(|e| CommandRefusal::new("layer_invalid", json!({"message": e})))?;
            layer.appearance = appearance;
            entry.records.layers.push(record);
            let mut successor = doc.clone();
            successor.layers.push(layer.clone());
            folio::validate_raster_records(&entry.records, &successor, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| command_refusal(folio_refusal(&d)))?;
            let body = entry.to_bytes().map_err(command_refusal)?;
            let operation = operation_for(&layer_id, false).map_err(|e| CommandRefusal::new("layer_invalid", json!({"message": e})))?;
            let write = PlacementWrite { layer_id: layer_id.clone(), to: Some(place), create: Some((layer, operation)) };
            let before = self.before_structure();
            if !self.run.restructure_and_entry(op, &[write], ENTRY, body) {
                return Err(command_refusal(self.run_refusal(op)));
            }
            self.prune_removed(op, before).map_err(|_| command_refusal(self.run_refusal(op)))?;
            self.selected = Some(layer_id.clone());
            Ok(json!({"layer_id": layer_id, "revision": self.revision_now()}))
        })();
        self.effects_reported(op, result)
    }

    /// The layer a smart filter stack renders over (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, review F1): the pixels of
    /// raster layer `layer_id` over its tiles' bounds (straight linear RGBA, `layer_rgba`) and those bounds cut to
    /// the canvas when the document has one (the content plane render-cpu filters over, inside the plan extent);
    /// `None` for a layer without tiles. Tiles in a format other than `rgb_f32le` (which neither this reader nor the
    /// render decodes: the render refuses the tile itself, with or without a stack) give the bounds and no pixels.
    /// Refused: `not_open`, `layer_unknown`, `layer_not_pixel`, `budget_exceeded`.
    pub(super) fn layer_content(&self, layer_id: &str) -> Result<Option<super::raster_effects::LayerContent>, CommandRefusal> {
        let doc = self.place_doc().map_err(command_refusal)?;
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| CommandRefusal::new("layer_unknown", json!({"layer": layer_id})))?;
        let Payload::Raster { tiles } = &layer.payload else {
            return Err(CommandRefusal::new("layer_not_pixel", json!({"layer": layer_id, "kind": layer.kind})));
        };
        if tiles.is_empty() {
            return Ok(None);
        }
        if tiles.iter().any(|t| t.format != "rgb_f32le") {
            return Ok(Some(super::raster_effects::LayerContent { plane: None, bounds: self.layer_content_bounds(layer_id) }));
        }
        let (region, pixels) = self.layer_rgba(layer_id).map_err(command_refusal)?;
        let bounds = match self.canvas() {
            Ok(canvas) => region.intersect(canvas).ok().flatten(),
            Err(_) => Some(region),
        };
        Ok(Some(super::raster_effects::LayerContent { plane: Some(hsk_studio_pigment::retouch::RgbaPlane { bounds: region, data: pixels }), bounds }))
    }

    /// The content bounds of [`Self::layer_content`] without reading the pixels (`effect.read`); `None` without tiles.
    pub(super) fn layer_content_bounds(&self, layer_id: &str) -> Option<Rect> {
        let snap = self.run.snapshot()?;
        let layer = snap.document().layers.iter().find(|l| l.layer_id == layer_id)?;
        let Payload::Raster { tiles } = &layer.payload else { return None };
        let px = |l: &folio::Length| (l.value.is_finite() && l.value >= 1.0 && l.value <= 4096.0 && l.value.fract() == 0.0).then_some(l.value as u32);
        let (gw, gh) = (tiles.iter().filter_map(|t| px(&t.width)).max()?, tiles.iter().filter_map(|t| px(&t.height)).max()?);
        let mut union: Option<Rect> = None;
        for t in tiles {
            let r = Rect { x: t.column.checked_mul(i64::from(gw))?, y: t.row.checked_mul(i64::from(gh))?, width: px(&t.width)?, height: px(&t.height)? };
            union = Some(match union {
                None => r,
                Some(u) => {
                    let (x0, y0) = (u.x.min(r.x), u.y.min(r.y));
                    let x1 = u.x.saturating_add(i64::from(u.width)).max(r.x.saturating_add(i64::from(r.width)));
                    let y1 = u.y.saturating_add(i64::from(u.height)).max(r.y.saturating_add(i64::from(r.height)));
                    Rect { x: x0, y: y0, width: u32::try_from(x1.saturating_sub(x0)).ok()?, height: u32::try_from(y1.saturating_sub(y0)).ok()? }
                }
            });
        }
        let union = union?;
        match self.canvas() {
            Ok(canvas) => union.intersect(canvas).ok().flatten(),
            Err(_) => Some(union),
        }
    }

    /// ONE tile step rewriting a pixel layer's colour in place (lane SMB-PS-FILTER-ADJUST-COMMANDS-1, the
    /// destructive apply of `view::raster_effects`), run as ONE job admitted through the viewer's bound JobPort
    /// (review Q5, HOST INTEGRATION (6): `Action::Edit`, its cost the layer's texel bytes; `overloaded`,
    /// `canceled`, `stale_revision` refuse before any pixel is read; the result carries the settlement as `job`).
    /// `map` gets each texel's straight linear colour and alpha (alpha > 0 only; alpha is kept). The tiles are
    /// written again on the 256-pixel grid at the same pixels (identity, nearest neighbour: an exact copy of what
    /// `map` returns) and committed with `Run::set_tiles`. Refused first, unadmitted: `layer_unknown`,
    /// `layer_not_pixel` (not a raster layer, or a placed asset layer: its tiles are the source rendered, V4-06
    /// sibling of [`Viewer::refuse_placed_pixels`]), `layer_empty`, `tile_format_unsupported`; then
    /// `budget_exceeded`, `canceled` and `map`'s own refusal. Result: `{layer_id, tiles, pixels, revision, job}`.
    pub(super) fn map_layer_pixels(&mut self, op: &str, layer_id: &str, mut map: impl FnMut([f32; 3], f32) -> Result<[f32; 3], CommandRefusal>) -> Result<Value, CommandRefusal> {
        let result = (|| -> Result<Value, CommandRefusal> {
            let doc = self.place_doc().map_err(command_refusal)?;
            let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| CommandRefusal::new("layer_unknown", json!({"layer": layer_id})))?;
            let Payload::Raster { tiles } = &layer.payload else {
                return Err(CommandRefusal::new("layer_not_pixel", json!({"layer": layer_id, "kind": layer.kind})));
            };
            let entry = self.editable_entry(&doc).map_err(command_refusal)?;
            if entry.placed(layer_id).is_some() || self.is_graphic_frame(layer_id) {
                return Err(CommandRefusal::new("layer_not_pixel", json!({"layer": layer_id, "message": "a placed asset layer: rasterize it first (STU-RAS-117)"})));
            }
            if tiles.is_empty() {
                return Err(CommandRefusal::new("layer_empty", json!({"layer": layer_id})));
            }
            if tiles.iter().any(|t| t.format != "rgb_f32le") {
                return Err(CommandRefusal::new("tile_format_unsupported", json!({"layer": layer_id, "expected": "rgb_f32le"})));
            }
            // The job's cost: the texel bytes it reads and writes (16 per texel), counted saturating.
            let texels = tiles.iter().fold(0u64, |acc, t| acc.saturating_add((t.width.value.max(0.0) as u64).saturating_mul(t.height.value.max(0.0) as u64)));
            let admission = self.job_admission();
            let head = self.document_head();
            let correlation = admission.next_correlation();
            let job = admission
                .admit(admission.request(hsk_studio_session::Action::Edit, texels.saturating_mul(16), head, correlation), head, self.cancel)
                .map_err(|r| CommandRefusal::new(r.code(), super::job_admission::refusal_json(r)))?;
            let work = (|| -> Result<Value, CommandRefusal> {
                let (bounds, mut pixels) = self.layer_rgba(layer_id).map_err(command_refusal)?;
                let mut count: u64 = 0;
                for (k, p) in pixels.iter_mut().enumerate() {
                    if k.trailing_zeros() >= 16 {
                        self.cancel.check().map_err(|_| CommandRefusal::new("canceled", json!({})))?;
                    }
                    // NaN alpha compares false: not content, as everywhere in this module.
                    if p[3] > 0.0 {
                        let [r, g, b] = map([p[0], p[1], p[2]], p[3])?;
                        *p = [r, g, b, p[3]];
                        count = count.saturating_add(1);
                    }
                }
                let new_tiles = self.resampled_tiles(layer_id, bounds, &pixels, Affine2::IDENTITY, ResampleFilter::NearestNeighbour, None).map_err(command_refusal)?;
                let n = new_tiles.len();
                if !self.run.set_tiles(op, layer_id, new_tiles) {
                    return Err(command_refusal(self.run_refusal(op)));
                }
                Ok(json!({"layer_id": layer_id, "tiles": n, "pixels": count, "revision": self.revision_now()}))
            })();
            match work {
                Ok(mut v) => {
                    v["job"] = super::job_admission::settlement_json(&job.settle(hsk_studio_session::JobOutcome::Completed));
                    Ok(v)
                }
                Err(e) => {
                    let outcome = if e.code == "canceled" { hsk_studio_session::JobOutcome::Canceled } else { hsk_studio_session::JobOutcome::Failed(static_code(&e.code)) };
                    job.settle(outcome);
                    Err(e)
                }
            }
        })();
        self.effects_reported(op, result)
    }

    /// `result` reported like [`Self::place_reported`]: a success marks the document dirty and re-reads the
    /// records; a refusal goes to the status line.
    fn effects_reported(&mut self, op: &str, result: Result<Value, CommandRefusal>) -> Result<Value, CommandRefusal> {
        match &result {
            Ok(_) => {
                self.dirty = true;
                self.place_sync();
            }
            Err(e) => self.say(format!("{op}: {} {}", e.code, e.detail)),
        }
        result
    }

    /// The records the viewer renders: the entry's when the session holds one, else the import records,
    /// pruned to the document and validated by Folio.
    fn effective_records(&self) -> Res<Option<StudioRasterRecords>> {
        let Some(doc) = self.run.snapshot().map(|s| s.document()) else { return Ok(None) };
        let mut records = match self.read_place_entry()? {
            Some(e) => e.records,
            None => match &self.place.import {
                Some(r) => r.clone(),
                None => return Ok(None),
            },
        };
        let present: BTreeSet<&str> = doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
        records.layers.retain(|l| present.contains(l.layer_id.as_str()));
        folio::validate_raster_records(&records, doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        Ok(Some(records))
    }

    /// Re-reads the records the viewer renders (open, undo, redo and every edit of this module).
    pub(super) fn place_sync(&mut self) {
        match self.effective_records() {
            Ok(r) => self.records = r,
            Err(e) => {
                self.records = None;
                self.say(format!("raster records not applied: {e}"));
            }
        }
    }

    /// Refuses a pixel edit of a placed-asset layer: its tiles are the source rendered through the instance
    /// transform, so paint there would be lost at the next transform (lane SMB-PLACE-ASSET-1, V4-06 sibling
    /// of the fill tools' refusal).
    pub(super) fn refuse_placed_pixels(&self, label: &str, layer_id: &str) -> Result<(), String> {
        if self.records.as_ref().and_then(|r| r.layer(layer_id)).is_some_and(|r| r.placed_asset.is_some()) {
            return Err(format!("{label}: layer_not_pixel: {layer_id} is a placed asset layer; rasterize it first (STU-RAS-117)"));
        }
        Ok(())
    }

    /// After an open: the import records it read, then the effective set and the link states.
    pub(super) fn place_opened(&mut self, import: Option<StudioRasterRecords>) {
        let base = self.source.as_deref().and_then(Path::parent).map(|p| if p.as_os_str().is_empty() { Path::new(".") } else { p }).and_then(|p| std::fs::canonicalize(p).ok());
        self.place = PlaceState { import, links: BTreeMap::new(), base };
        self.drop_absent_records();
        self.place_sync();
        if let Some(entry) = self.run.snapshot().map(|s| s.document().clone()).and_then(|doc| self.editable_entry(&doc).ok()) {
            self.check_links(&entry);
        }
    }

    /// Lane SMB-PS-FILTER-ADJUST-COMMANDS-1 (lead decision 2026-11-13, records never outlive their layer): at open,
    /// the rows and placed instances of [`ENTRY`] that name a layer the document does not hold (a package written
    /// before the delete took them along, or by another writer) are dropped, written outside the history as the
    /// design states of absent layers are, with one loss note per layer on the status log
    /// (`raster_records_dropped: <layer id>`), so a layer that later takes such an id inherits nothing. An entry that
    /// does not parse is left as it is (it is not applied at render).
    fn drop_absent_records(&mut self) {
        let Some(doc) = self.run.snapshot().map(|s| s.document().clone()) else { return };
        let Ok(Some(mut entry)) = self.read_place_entry() else { return };
        let present: BTreeSet<&str> = doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
        let mut gone: Vec<String> =
            entry.records.layers.iter().map(|r| r.layer_id.clone()).chain(entry.instances.iter().map(|i| i.layer_id.clone())).filter(|id| !present.contains(id.as_str())).collect();
        if gone.is_empty() {
            return;
        }
        gone.sort();
        gone.dedup();
        entry.prune(&doc);
        let written = entry.to_bytes().map_err(|e| e.to_string()).and_then(|b| self.run.set_document_entry(ENTRY, Some(b)).map_err(str::to_owned));
        match written {
            Ok(()) => {
                for id in gone {
                    self.say(format!("raster_records_dropped: {id} (a record of a layer the document does not hold)"));
                }
            }
            Err(e) => self.say(format!("raster records of absent layers not dropped: {e}")),
        }
    }

    /// Lead decision 2026-11-13 (records never outlive their layer): [`ENTRY`] (else the import records) without the
    /// rows and placed instances of `removed` (the layers a structure step removed; lane SMB-FIX-LAYER-RECORD-PRUNE-1,
    /// `view::layer_records`, which puts it into that step), read after the step, so it is not pruned to the document;
    /// `None` when it holds none of them or cannot be read (an unreadable entry is not applied at render and never
    /// blocks a step).
    pub(super) fn raster_records_without(&self, removed: &BTreeSet<String>) -> Option<Vec<u8>> {
        let mut entry = match self.read_place_entry().ok()? {
            Some(e) => e,
            None => Entry::new(self.place.import.clone()?),
        };
        let held = |id: &String| removed.contains(id);
        if !entry.records.layers.iter().any(|r| held(&r.layer_id)) && !entry.instances.iter().any(|i| held(&i.layer_id)) {
            return None;
        }
        entry.records.layers.retain(|r| !held(&r.layer_id));
        entry.instances.retain(|i| !held(&i.layer_id));
        entry.to_bytes().ok()
    }

    /// The opened document's folder, canonical, as captured at its open ([`PlaceState`]).
    fn link_base(&self) -> Res<PathBuf> {
        self.place.base.clone().ok_or_else(|| PlaceRefusal::new("link_base_unknown", "a link resolves against the opened document's folder: open or save and reopen the document first"))
    }

    /// The [STU-ASSET-007] state of a linked source now.
    fn link_state(&self, uri: &str, content_digest: &str) -> &'static str {
        match self.link_base() {
            Ok(base) => state_token(LinkedFileResolver::new(&crate::harness::TrustAll, base).with_max_bytes(MAX_SOURCE_BYTES).check(uri, content_digest)),
            Err(_) => "unresolved",
        }
    }

    fn check_links(&mut self, entry: &Entry) {
        let mut links = BTreeMap::new();
        for s in &entry.records.placed_sources {
            if let StudioPlacedMode::Linked { uri, content_digest, .. } = &s.location {
                links.insert(s.source_id.clone(), self.link_state(uri, content_digest));
            }
        }
        self.place.links = links;
    }

    /// The bytes of a linked file after its state was read: only a resolved or modified link reads.
    fn read_link(&self, uri: &str) -> Res<Vec<u8>> {
        let base = self.link_base()?;
        super::commands::read_bounded(&base.join(uri), MAX_SOURCE_BYTES).map_err(|r| PlaceRefusal::new(&r.code, r.detail.to_string()))
    }

    /// The current bytes of `source_id` (embedded: the package; linked: the file, which must match its
    /// recorded digest).
    fn source_bytes(&self, entry: &Entry, source_id: &str) -> Res<Vec<u8>> {
        let source = entry.records.placed_source(source_id).ok_or_else(|| PlaceRefusal::new("placed_source_unresolved", source_id.to_owned()))?;
        match &source.location {
            StudioPlacedMode::Embedded { .. } => {
                let digest = entry.sources.iter().find(|s| s.source_id == source_id).and_then(|s| s.bytes.clone()).ok_or_else(|| PlaceRefusal::new("placed_source_unresolved", source_id.to_owned()))?;
                self.run.asset_bytes(&digest).map_err(|c| PlaceRefusal::new(c, source_id.to_owned()))
            }
            StudioPlacedMode::Linked { uri, content_digest, .. } => match self.link_state(uri, content_digest) {
                "resolved" => {
                    // Read once more and compared again: the file may change between the check and the read.
                    let bytes = self.read_link(uri)?;
                    if format!("sha256:{}", hsk_studio_package::sha256_hex_of(&bytes)) != *content_digest {
                        return no("placed_link_modified", uri.clone());
                    }
                    Ok(bytes)
                }
                "unresolved" => no("link_base_unknown", uri.clone()),
                state => no(&format!("placed_link_{state}"), uri.clone()),
            },
        }
    }

    /// Imports `bytes` and renders their composite in linear light, unclipped.
    fn decode_source(&self, bytes: &[u8]) -> Res<SourceImage> {
        let imported = crate::import::import_as(bytes, &crate::harness::TrustAll, self.run.actor(), self.cancel).map_err(|e| PlaceRefusal::new(e.code(), "the source does not import"))?;
        let ppi = layout_place::stated_ppi(&imported.receipt);
        let held: Vec<_> = imported.imports.iter().map(ImportEntry::to_opaque).collect();
        let referenced = hsk_studio_package::referenced_assets(imported.snapshot.document(), &held).map_err(|e| PlaceRefusal::new(e.code(), "source assets"))?;
        let mut assets = Assets::new();
        for digest in referenced.keys() {
            let d = ContentDigest { algorithm: "sha256".into(), digest: digest.clone() };
            if let Ok(body) = imported.assets.read(&d, MAX_SOURCE_BYTES) {
                assets.insert(digest.clone(), body);
            }
        }
        let records = imported.imports.iter().find(|e| e.role == ImportRole::Records).and_then(|e| serde_json::from_slice::<StudioRasterRecords>(&e.bytes).ok());
        // Unclipped (review F4): a float source keeps its values outside 0..=1 into the resample.
        let frame = render::render_scene_linear_with_records(&imported.snapshot, &assets, records.as_ref(), self.cancel).map_err(|f| PlaceRefusal::new(f.code, f.detail))?;
        if frame.width == 0 || frame.height == 0 || frame.pixels.is_empty() {
            return no("placed_source_empty", "the source renders no pixel");
        }
        Ok(SourceImage { width: frame.width, height: frame.height, pixels: frame.pixels, ppi })
    }

    /// `pixels` (row-major over `src`, straight linear RGBA) mapped by `affine` into new 256-pixel tiles of
    /// `layer_id`, over `clip` when given; every cell with any alpha is staged in the session.
    fn resampled_tiles(&mut self, layer_id: &str, src: Rect, pixels: &[[f32; 4]], affine: Affine2, filter: ResampleFilter, clip: Option<Rect>) -> Res<Vec<TileRef>> {
        let doc = self.place_doc()?;
        let profile = edit::new_tile_profile(&doc, &[]).ok_or_else(|| PlaceRefusal::new("no_working_profile", "no raster tile and no assigned RGB working space"))?;
        let prepared = PreparedTransform::new(&LayerTransform::new(affine, filter), src).map_err(|e| PlaceRefusal::new(e.code(), "transform"))?;
        let region = match clip {
            Some(c) => match prepared.bounds().intersect(c).ok().flatten() {
                Some(r) => r,
                None => return Ok(Vec::new()),
            },
            None => prepared.bounds(),
        };
        if region.area() > MAX_PLACE_PIXELS {
            return no("budget_exceeded", format!("{} x {} pixels", region.width, region.height));
        }
        let width = usize::try_from(src.width).map_err(|_| PlaceRefusal::new("budget_exceeded", "width"))?;
        let cell = i64::try_from(edit::CELL).unwrap_or(256);
        let (ex, ey) = region.end().map_err(|_| PlaceRefusal::new("budget_exceeded", "bounds"))?;
        let first = |v: i64| v.checked_div_euclid(cell).unwrap_or(0);
        let last = |v: i64| v.checked_sub(1).and_then(|v| v.checked_div_euclid(cell)).unwrap_or(0);
        let texel = |i: u32, j: u32| -> Result<[f32; 4], ()> {
            let k = (j as usize).checked_mul(width).and_then(|k| k.checked_add(i as usize)).ok_or(())?;
            pixels.get(k).copied().ok_or(())
        };
        let range = sample_range(pixels);
        let mut tiles: Vec<TileRef> = Vec::new();
        let mut staged = Vec::new();
        for row in first(region.y)..=last(ey) {
            for column in first(region.x)..=last(ex) {
                self.cancel.check().map_err(|_| PlaceRefusal::new("canceled", "canceled"))?;
                let (Some(x0), Some(y0)) = (column.checked_mul(cell), row.checked_mul(cell)) else { continue };
                let cell_rect = Rect { x: x0, y: y0, width: edit::CELL as u32, height: edit::CELL as u32 };
                let Some(hit) = cell_rect.intersect(region).ok().flatten() else { continue };
                let mut body = vec![0u8; edit::COLOUR_CELL.saturating_add(edit::ALPHA_CELL)];
                let mut any = false;
                let (hx, hy) = hit.end().map_err(|_| PlaceRefusal::new("budget_exceeded", "cell"))?;
                for y in hit.y..hy {
                    for x in hit.x..hx {
                        let px = prepared.sample_rgba_in(range, x, y, texel).unwrap_or([0.0; 4]);
                        if px[3].is_nan() || px[3] <= 0.0 {
                            continue;
                        }
                        any = true;
                        let (dx, dy) = (x.saturating_sub(x0) as usize, y.saturating_sub(y0) as usize);
                        let idx = dy.saturating_mul(edit::CELL).saturating_add(dx);
                        let at = idx.saturating_mul(12);
                        for (c, v) in px[..3].iter().enumerate() {
                            let o = at.saturating_add(c.saturating_mul(4));
                            if let Some(slot) = body.get_mut(o..o.saturating_add(4)) {
                                slot.copy_from_slice(&v.to_le_bytes());
                            }
                        }
                        let o = edit::COLOUR_CELL.saturating_add(idx.saturating_mul(4));
                        if let Some(slot) = body.get_mut(o..o.saturating_add(4)) {
                            slot.copy_from_slice(&px[3].to_le_bytes());
                        }
                    }
                }
                if !any {
                    continue;
                }
                let digest = self.run.stage_tile(body.clone()).map_err(|c| PlaceRefusal::new(c, "stage tile"))?;
                let tile = edit::new_tile(layer_id, column, row, &profile, &digest, &tiles).map_err(|e| PlaceRefusal::new("tile_invalid", e))?;
                staged.push((digest.digest.clone(), body));
                tiles.push(tile);
            }
        }
        self.assets.extend(staged);
        Ok(tiles)
    }

    /// The cached display tiles of a placement, clipped to the canvas (a layout graphic frame's are
    /// `source_tiles`: lane SMB-LAYOUT-PLACE-1, Update All Links picks them).
    fn placement_tiles(&mut self, layer_id: &str, source: &SourceImage, transform: [f64; 6], filter: ResampleFilter) -> Res<Vec<TileRef>> {
        let canvas = self.canvas().map_err(|e| PlaceRefusal::new("canvas_unavailable", e))?;
        let src = Rect { x: 0, y: 0, width: source.width, height: source.height };
        self.resampled_tiles(layer_id, src, &source.pixels, placed_matrix(transform), filter, Some(canvas))
    }

    /// The pixels of raster layer `layer_id` over its tiles' bounds, as straight linear RGBA (the reader of
    /// `select_liquify`).
    fn layer_rgba(&self, layer_id: &str) -> Res<(Rect, Vec<[f32; 4]>)> {
        let (bounds, px) = self.layer_pixels("transform", layer_id, None).map_err(|e| PlaceRefusal::new(if e.contains("budget_exceeded") { "budget_exceeded" } else { "tile_invalid" }, e))?;
        let f = |b: &[u8]| <[u8; 4]>::try_from(b).map_or(0.0, f32::from_le_bytes);
        let rgba = px.colour.chunks_exact(12).zip(px.alpha.chunks_exact(4)).map(|(c, a)| [f(&c[0..4]), f(&c[4..8]), f(&c[8..12]), f(a)]).collect();
        Ok((bounds, rgba))
    }

    /// Runs `f` and reports its refusal on the status line.
    fn place_reported(&mut self, op: &str, f: impl FnOnce(&mut Self) -> Res<Value>) -> Res<Value> {
        let r = f(self);
        match &r {
            Ok(_) => {
                self.dirty = true;
                self.place_sync();
            }
            Err(e) => self.say(format!("{op}: {e}")),
        }
        r
    }

    fn run_refusal(&self, op: &str) -> PlaceRefusal {
        PlaceRefusal::new(&self.op_error(), format!("{op} refused by the run"))
    }

    fn revision_now(&self) -> u64 {
        self.run.snapshot().map_or(0, |s| s.document().revision)
    }

    /// `asset.place` (module docs). Returns `{layer_id, source_id, placement_kind, uri, tiles, scale_pct,
    /// transform, resample, shared, revision}`.
    pub fn place_asset(&mut self, path: &Path, kind: PlacementKind, opts: &PlaceOptions) -> Result<Value, PlaceRefusal> {
        self.place_reported("asset.place", |v| v.place_inner(path, kind, opts))
    }

    fn place_inner(&mut self, path: &Path, kind: PlacementKind, opts: &PlaceOptions) -> Res<Value> {
        const OP: &str = "asset.place";
        let doc = self.place_doc()?;
        // Lane SMB-LAYOUT-PLACE-1: a document with pages only places into a graphic frame.
        if layout_place::pages_only(&doc) {
            return self.place_in_frame(path, kind, opts, doc);
        }
        if opts.frame != FrameOptions::default() {
            return no("frame_needs_pages", "the frame parameters place into a layout page; this document has an artboard");
        }
        let canvas = self.canvas().map_err(|e| PlaceRefusal::new("canvas_unavailable", e))?;
        if opts.scale_pct.is_some_and(|s| !(s.is_finite() && s > 0.0 && s <= 100_000.0))
            || opts.opacity_pct.is_some_and(|o| !(0.0..=100.0).contains(&o))
            || [opts.x, opts.y].iter().flatten().any(|v| !(v.is_finite() && v.abs() <= 1.0e9))
        {
            return no("parameter_out_of_range", "scale_pct 0 <..=100000, opacity_pct 0..=100, |x|, |y| <= 1e9");
        }
        let filter = opts.resample.unwrap_or(ResampleFilter::Bicubic);
        let uri = match kind {
            PlacementKind::Linked => Some(relative_uri(&self.link_base()?, path)?),
            PlacementKind::Embedded => None,
        };
        let bytes = super::commands::read_bounded(path, MAX_SOURCE_BYTES).map_err(|r| PlaceRefusal::new(&r.code, r.detail.to_string()))?;
        let digest = format!("sha256:{}", hsk_studio_package::sha256_hex_of(&bytes));
        let image = self.decode_source(&bytes)?;
        let mut entry = self.editable_entry(&doc)?;
        let (w, h) = (f64::from(image.width), f64::from(image.height));
        let name = label(&opts.name.clone().unwrap_or_else(|| path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        let (source_id, shared) = self.source_for(&mut entry, &doc, uri.as_ref(), &digest, bytes, (w, h), &name)?;
        let scale = match opts.scale_pct {
            Some(p) => p / 100.0,
            None => 1.0f64.min(f64::from(canvas.width) / w).min(f64::from(canvas.height) / h),
        };
        let x = opts.x.unwrap_or(canvas.x as f64 + (f64::from(canvas.width) - w * scale) / 2.0);
        let y = opts.y.unwrap_or(canvas.y as f64 + (f64::from(canvas.height) - h * scale) / 2.0);
        let transform = [scale, 0.0, 0.0, scale, x, y];
        let layer_id = mint_placed_layer_id(&doc, &entry);
        let tiles = self.placement_tiles(&layer_id, &image, transform, filter)?;
        let count = tiles.len();
        let (parent, place) = self.insertion_point().map_err(|e| PlaceRefusal::new("not_open", e))?;
        let mut layer = new_layer(&layer_id, &name, &parent, "raster", json!({"payload_kind": "raster", "tiles": tiles})).map_err(|e| PlaceRefusal::new("layer_invalid", e))?;
        if let Some(o) = opts.opacity_pct {
            layer.appearance = Some(LayerAppearance { opacity: o, ..LayerAppearance::identity(false) });
        }
        let mut record = StudioRasterLayerRecord::new(layer_id.clone());
        record.placed_asset = Some(StudioPlacedAsset { source_id: source_id.clone(), transform });
        entry.records.layers.push(record);
        entry.instances.push(Instance { layer_id: layer_id.clone(), placed: transform, resample: filter });
        let mut successor = doc.clone();
        successor.layers.push(layer.clone());
        folio::validate_raster_records(&entry.records, &successor, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let body = entry.to_bytes()?;
        let operation = operation_for(&layer_id, false).map_err(|e| PlaceRefusal::new("layer_invalid", e))?;
        let write = PlacementWrite { layer_id: layer_id.clone(), to: Some(place), create: Some((layer, operation)) };
        let before = self.before_structure();
        if !self.run.restructure_and_entry(OP, &[write], ENTRY, body) {
            return Err(self.run_refusal(OP));
        }
        self.prune_removed(OP, before).map_err(|_| self.run_refusal(OP))?;
        self.selected = Some(layer_id.clone());
        if uri.is_some() {
            self.place.links.insert(source_id.clone(), "resolved");
        }
        let revision = self.revision_now();
        self.say(format!("{OP} {} as {layer_id} ({count} tile(s), revision {revision})", kind.token()));
        Ok(json!({
            "layer_id": layer_id, "source_id": source_id, "placement_kind": kind.token(), "uri": uri, "tiles": count, "scale_pct": scale * 100.0,
            "transform": transform, "resample": filter.key(), "shared": shared, "revision": revision,
        }))
    }

    /// The source of a placement in `entry` ([STU-RAS-013]): the same bytes (embedded) or the same file and digest
    /// (linked, `uri`) at the same size share their source; else a new one is added, an embedded one's bytes staged.
    /// Returns the source id and whether it was shared.
    #[allow(clippy::too_many_arguments)]
    fn source_for(&mut self, entry: &mut Entry, doc: &StudioDocument, uri: Option<&String>, digest: &str, bytes: Vec<u8>, (w, h): (f64, f64), name: &str) -> Res<(String, bool)> {
        let shared = entry.records.placed_sources.iter().find(|s| {
            s.width == w
                && s.height == h
                && match (&s.location, uri) {
                    (StudioPlacedMode::Linked { uri: u, content_digest, .. }, Some(want)) => u == want && content_digest == digest,
                    (StudioPlacedMode::Embedded { content_digest, .. }, None) => content_digest == digest,
                    _ => false,
                }
        });
        if let Some(s) = shared {
            return Ok((s.source_id.clone(), true));
        }
        let taken: BTreeSet<&str> = entry.records.placed_sources.iter().map(|s| s.source_id.as_str()).collect();
        let id = (1u64..).map(|n| format!("src-{n}")).find(|id| !taken.contains(id.as_str())).unwrap_or_default();
        let (location, staged) = match uri {
            Some(u) => (StudioPlacedMode::Linked { uri: u.clone(), content_digest: digest.to_owned(), health: StudioLinkHealth::UpToDate }, None),
            None => {
                let staged = self.run.stage_tile(bytes).map_err(|c| PlaceRefusal::new(c, "stage the source"))?;
                (StudioPlacedMode::Embedded { document_id: mint_document_id(doc, &entry.records), content_digest: digest.to_owned() }, Some(staged))
            }
        };
        entry.records.placed_sources.push(StudioPlacedSource { source_id: id.clone(), location, width: w, height: h, uses: Vec::new(), asset_id: None });
        entry.sources.push(SourceMeta { source_id: id.clone(), name: name.to_owned(), bytes: staged });
        Ok((id, false))
    }

    /// `transform.free_transform` (module docs). Returns `{layer_id, placed, transform?, tiles, resample, revision}`.
    pub fn free_transform(&mut self, layer_id: &str, t: &FreeTransform) -> Result<Value, PlaceRefusal> {
        self.place_reported("transform.free_transform", |v| v.transform_inner("transform.free_transform", layer_id, Edit::Free(t)))
    }

    /// `transform.move` (module docs).
    pub fn move_content(&mut self, layer_id: &str, dx: f64, dy: f64) -> Result<Value, PlaceRefusal> {
        self.place_reported("transform.move", |v| v.transform_inner("transform.move", layer_id, Edit::Move(dx, dy)))
    }

    /// `transform.reset` (module docs).
    pub fn reset_transform(&mut self, layer_id: &str) -> Result<Value, PlaceRefusal> {
        self.place_reported("transform.reset", |v| v.transform_inner("transform.reset", layer_id, Edit::Reset))
    }

    fn transform_inner(&mut self, op: &str, layer_id: &str, change: Edit<'_>) -> Res<Value> {
        let doc = self.place_doc()?;
        // Lane SMB-LAYOUT-PLACE-1: the frame record places a graphic frame's pixels; moving the pixels alone
        // would draw them wrong (frame move is a later slice).
        if self.is_graphic_frame(layer_id) {
            return no("graphic_frame_transform_unsupported", format!("{layer_id} is a layout graphic frame: fit it with asset.fit"));
        }
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| PlaceRefusal::new("layer_unknown", layer_id.to_owned()))?;
        let Payload::Raster { tiles } = &layer.payload else {
            return no("layer_not_pixel", format!("{layer_id} is a {} layer", layer.kind));
        };
        if layer.appearance.as_ref().is_some_and(|a| a.mask.is_some()) {
            return no("layer_has_mask", "the mask would not follow the content");
        }
        if let Edit::Move(dx, dy) = change
            && !(dx.is_finite() && dy.is_finite() && dx.abs() <= 1.0e9 && dy.abs() <= 1.0e9)
        {
            return no("parameter_out_of_range", "dx, dy finite, at most 1e9");
        }
        let entry = self.editable_entry(&doc)?;
        if let Some(asset) = entry.placed(layer_id).cloned() {
            return self.transform_placed(op, layer_id, change, entry, asset);
        }
        let filter = match change {
            Edit::Reset => return no("layer_not_placed", layer_id.to_owned()),
            Edit::Move(dx, dy) if dx.fract() != 0.0 || dy.fract() != 0.0 => return no("move_not_whole_pixels", "a pixel layer moves by whole pixels"),
            Edit::Move(..) => ResampleFilter::NearestNeighbour,
            Edit::Free(t) => t.resample.unwrap_or(ResampleFilter::Bicubic),
        };
        if tiles.is_empty() {
            return no("layer_empty", layer_id.to_owned());
        }
        if tiles.iter().any(|t| t.format != "rgb_f32le") {
            return no("tile_format_unsupported", "a pixel layer of rgb_f32le tiles");
        }
        let (tile_bounds, tile_pixels) = self.layer_rgba(layer_id)?;
        // The reference point and the source rect are the layer's pixels, not its tile cells (review F2).
        let (bounds, pixels) = opaque_crop(tile_bounds, &tile_pixels).ok_or_else(|| PlaceRefusal::new("layer_empty", layer_id.to_owned()))?;
        drop(tile_pixels);
        let delta = match change {
            Edit::Free(t) => delta_matrix(t, mapped_bounds(Affine2::IDENTITY, bounds))?,
            Edit::Move(dx, dy) => Affine2::translate(dx, dy),
            Edit::Reset => Affine2::IDENTITY,
        };
        let new_tiles = self.resampled_tiles(layer_id, bounds, &pixels, delta, filter, None)?;
        let count = new_tiles.len();
        if !self.run.set_tiles(op, layer_id, new_tiles) {
            return Err(self.run_refusal(op));
        }
        let revision = self.revision_now();
        self.say(format!("{op} on {layer_id} ({count} tile(s), revision {revision})"));
        Ok(json!({"layer_id": layer_id, "placed": false, "tiles": count, "resample": filter.key(), "revision": revision}))
    }

    fn transform_placed(&mut self, op: &str, layer_id: &str, change: Edit<'_>, mut entry: Entry, asset: StudioPlacedAsset) -> Res<Value> {
        let source = entry.records.placed_source(&asset.source_id).cloned().ok_or_else(|| PlaceRefusal::new("placed_source_unresolved", asset.source_id.clone()))?;
        let at = entry.instances.iter().position(|i| i.layer_id == layer_id);
        let instance = at.and_then(|i| entry.instances.get(i).cloned()).unwrap_or(Instance { layer_id: layer_id.to_owned(), placed: asset.transform, resample: ResampleFilter::Bicubic });
        let current = placed_matrix(asset.transform);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let src = Rect { x: 0, y: 0, width: source.width as u32, height: source.height as u32 };
        let next = match change {
            Edit::Free(t) => current.then(delta_matrix(t, mapped_bounds(current, src))?),
            Edit::Move(dx, dy) => current.then(Affine2::translate(dx, dy)),
            Edit::Reset => placed_matrix(instance.placed),
        };
        let filter = match change {
            Edit::Free(FreeTransform { resample: Some(f), .. }) => *f,
            _ => instance.resample,
        };
        let transform = folio_matrix(next);
        let bytes = self.source_bytes(&entry, &asset.source_id)?;
        let image = self.decode_source(&bytes)?;
        if f64::from(image.width) != source.width || f64::from(image.height) != source.height {
            return no("placed_source_size_mismatch", asset.source_id.clone());
        }
        let tiles = self.placement_tiles(layer_id, &image, transform, filter)?;
        let count = tiles.len();
        if let Some(r) = entry.records.layers.iter_mut().find(|r| r.layer_id == layer_id) {
            r.placed_asset = Some(StudioPlacedAsset { source_id: asset.source_id.clone(), transform });
        }
        let updated = Instance { resample: filter, ..instance };
        match at {
            Some(i) => {
                if let Some(slot) = entry.instances.get_mut(i) {
                    *slot = updated;
                }
            }
            None => entry.instances.push(updated),
        }
        let doc = self.place_doc()?;
        folio::validate_raster_records(&entry.records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let body = entry.to_bytes()?;
        if !self.run.set_tiles_and_entry(op, layer_id, Some(tiles), ENTRY, body) {
            return Err(self.run_refusal(op));
        }
        let revision = self.revision_now();
        self.say(format!("{op} on placed {layer_id} ({count} tile(s), revision {revision})"));
        Ok(json!({"layer_id": layer_id, "placed": true, "transform": transform, "tiles": count, "resample": filter.key(), "revision": revision}))
    }

    /// One row of [`Viewer::list_placements`]; `state` is the link state.
    fn placement_json(entry: &Entry, record: &StudioRasterLayerRecord, asset: &StudioPlacedAsset, state: &str) -> Value {
        let source = entry.records.placed_source(&asset.source_id);
        let meta = entry.sources.iter().find(|s| s.source_id == asset.source_id);
        let instance = entry.instances.iter().find(|i| i.layer_id == record.layer_id);
        let (kind, uri, document_id, digest, health) = match source.map(|s| &s.location) {
            Some(StudioPlacedMode::Linked { uri, content_digest, health }) => ("linked", Some(uri.clone()), None, Some(content_digest.clone()), serde_json::to_value(health).unwrap_or(Value::Null)),
            Some(StudioPlacedMode::Embedded { document_id, content_digest }) => ("embedded", None, Some(document_id.clone()), Some(content_digest.clone()), Value::Null),
            None => ("unresolved", None, None, None, Value::Null),
        };
        json!({
            "layer_id": record.layer_id, "source_id": asset.source_id, "placement_kind": kind, "uri": uri, "document_id": document_id, "content_digest": digest,
            "name": meta.map(|m| m.name.clone()), "width": source.map(|s| s.width), "height": source.map(|s| s.height), "transform": asset.transform,
            "placed_transform": instance.map(|i| i.placed), "resample": instance.map(|i| i.resample.key()), "health": health, "link_state": state,
        })
    }

    fn placements(&self) -> Value {
        let Some(entry) = self.run.snapshot().and_then(|s| self.editable_entry(s.document()).ok()) else {
            return json!({"placements": [], "sources": 0});
        };
        let frames = self.graphic_frames_now();
        let rows: Vec<Value> = entry
            .records
            .layers
            .iter()
            .filter_map(|r| r.placed_asset.as_ref().map(|a| (r, a)))
            .map(|(r, a)| {
                let state = match entry.records.placed_source(&a.source_id).map(|s| &s.location) {
                    Some(StudioPlacedMode::Embedded { .. }) => "embedded",
                    Some(StudioPlacedMode::Linked { .. }) => self.place.links.get(&a.source_id).copied().unwrap_or("unresolved"),
                    None => "unresolved",
                };
                let mut row = Self::placement_json(&entry, r, a, state);
                // Lane SMB-LAYOUT-PLACE-1: a layout placement's page, frame and resolutions.
                self.frame_fields(&frames, &entry, &r.layer_id, &mut row);
                row
            })
            .collect();
        json!({"placements": rows, "sources": entry.records.placed_sources.len()})
    }

    /// `asset.list_placements` (module docs): every placement, link states checked now.
    pub fn list_placements(&mut self) -> Value {
        if let Some(entry) = self.run.snapshot().map(|s| s.document().clone()).and_then(|doc| self.editable_entry(&doc).ok()) {
            self.check_links(&entry);
        }
        self.placements()
    }

    /// `state_json()["placed"]`: the placements with the link states of the last check.
    pub fn placed_json(&self) -> Value {
        self.placements()
    }

    /// `asset.update_all_modified` (module docs). Returns `{updated: [source_id], missing: [source_id],
    /// layers: [layer_id], refused: [{source_id, layer_id, code, detail}], changed, revision}`.
    pub fn update_all_modified(&mut self) -> Result<Value, PlaceRefusal> {
        self.place_reported("asset.update_all_modified", Self::update_inner)
    }

    fn update_inner(&mut self) -> Res<Value> {
        const OP: &str = "asset.update_all_modified";
        let doc = self.place_doc()?;
        let Some(mut entry) = self.read_place_entry()? else {
            return Ok(json!({"updated": [], "missing": [], "layers": [], "changed": false, "revision": self.revision_now()}));
        };
        entry.prune(&doc);
        let (mut updated, mut missing, mut writes, mut changed) = (Vec::new(), Vec::new(), Vec::new(), false);
        // Lane SMB-LAYOUT-PLACE-1: the graphic frames, decoded once (review F3 of fbf7f0cd), and on a layout the
        // layout state, whose frames are refitted to a source's new size in the same step.
        let framed = self.graphic_frame_ids();
        let mut layout = if framed.is_empty() { None } else { Some(self.layout_state()?) };
        let mut refused = Vec::new();
        let linked: Vec<(usize, String, String, StudioLinkHealth)> = entry
            .records
            .placed_sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match &s.location {
                StudioPlacedMode::Linked { uri, content_digest, health } => Some((i, uri.clone(), content_digest.clone(), *health)),
                StudioPlacedMode::Embedded { .. } => None,
            })
            .collect();
        for (index, uri, digest, health) in linked {
            let state = self.link_state(&uri, &digest);
            let Some(source) = entry.records.placed_sources.get(index).cloned() else { continue };
            let new_health = match state {
                "modified" => {
                    let bytes = self.read_link(&uri)?;
                    let image = self.decode_source(&bytes)?;
                    let new_digest = format!("sha256:{}", hsk_studio_package::sha256_hex_of(&bytes));
                    let instances: Vec<(String, [f64; 6], ResampleFilter)> = entry
                        .records
                        .layers
                        .iter()
                        .filter_map(|r| r.placed_asset.as_ref().filter(|a| a.source_id == source.source_id).map(|a| (r.layer_id.clone(), a.transform)))
                        .map(|(l, t)| {
                            let f = entry.instances.iter().find(|i| i.layer_id == l).map_or(ResampleFilter::Bicubic, |i| i.resample);
                            (l, t, f)
                        })
                        .collect();
                    // Lane SMB-LAYOUT-PLACE-1: the source's graphic frames take its new content size and their stored
                    // fit again; a frame that cannot refit refuses this source (typed, in `refused`), the others go on.
                    let frames: Vec<String> = instances.iter().map(|(l, _, _)| l.clone()).filter(|l| framed.contains(l)).collect();
                    if let Some((document, records)) = layout.as_mut()
                        && !frames.is_empty()
                    {
                        let ppi = image.ppi.unwrap_or([layout_place::DEFAULT_PPI; 2]);
                        let content = [f64::from(image.width) * layout_place::DEFAULT_PPI / ppi[0], f64::from(image.height) * layout_place::DEFAULT_PPI / ppi[1]];
                        if let Err((layer_id, e)) = Self::refit_frames(document, records, &frames, content) {
                            refused.push(json!({"source_id": source.source_id, "layer_id": layer_id, "code": e.code, "detail": e.detail}));
                            continue;
                        }
                    }
                    for (layer_id, transform, filter) in instances {
                        let tiles = if framed.contains(&layer_id) { self.source_tiles(&layer_id, &image)? } else { self.placement_tiles(&layer_id, &image, transform, filter)? };
                        writes.push((layer_id, tiles));
                    }
                    if let Some(s) = entry.records.placed_sources.get_mut(index) {
                        s.width = f64::from(image.width);
                        s.height = f64::from(image.height);
                        s.location = StudioPlacedMode::Linked { uri: uri.clone(), content_digest: new_digest, health: StudioLinkHealth::UpToDate };
                    }
                    updated.push(source.source_id.clone());
                    changed = true;
                    continue;
                }
                "missing" => {
                    missing.push(source.source_id.clone());
                    StudioLinkHealth::Missing
                }
                "resolved" => StudioLinkHealth::UpToDate,
                _ => health,
            };
            if new_health != health {
                if let Some(StudioPlacedSource { location: StudioPlacedMode::Linked { health: h, .. }, .. }) = entry.records.placed_sources.get_mut(index) {
                    *h = new_health;
                }
                changed = true;
            }
        }
        let layers: Vec<String> = writes.iter().map(|(l, _)| l.clone()).collect();
        if !changed {
            return Ok(json!({"updated": updated, "missing": missing, "layers": layers, "refused": refused, "changed": false, "revision": self.revision_now()}));
        }
        folio::validate_raster_records(&entry.records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let body = entry.to_bytes()?;
        let committed = match layout {
            // A layout: the new tiles, the refitted frames and the entry as ONE layout step.
            Some((mut document, records)) => {
                for (layer_id, tiles) in &writes {
                    if let Some(l) = document.layers.iter_mut().find(|l| l.layer_id == *layer_id) {
                        l.payload = Payload::Raster { tiles: tiles.clone() };
                    }
                }
                self.run.submit_layout_and_entry(OP, OP, document, records, ENTRY, body)
            }
            None => self.run.set_layers_tiles_and_entry(OP, &writes, ENTRY, body),
        };
        if !committed {
            return Err(self.run_refusal(OP));
        }
        self.check_links(&entry);
        self.layout_sync();
        let revision = self.revision_now();
        self.say(format!("{OP}: {} updated, {} missing, {} refused (revision {revision})", updated.len(), missing.len(), refused.len()));
        Ok(json!({"updated": updated, "missing": missing, "layers": layers, "refused": refused, "changed": true, "revision": revision}))
    }

    /// `asset.embed_link` (module docs). Returns `{layer_id, source_id, document_id, revision}`.
    pub fn embed_link(&mut self, layer_id: &str) -> Result<Value, PlaceRefusal> {
        self.place_reported("asset.embed_link", |v| v.embed_inner(layer_id))
    }

    fn embed_inner(&mut self, layer_id: &str) -> Res<Value> {
        const OP: &str = "asset.embed_link";
        let doc = self.place_doc()?;
        let mut entry = self.editable_entry(&doc)?;
        let asset = entry.placed(layer_id).cloned().ok_or_else(|| PlaceRefusal::new("layer_not_placed", layer_id.to_owned()))?;
        let index = entry.records.placed_sources.iter().position(|s| s.source_id == asset.source_id).ok_or_else(|| PlaceRefusal::new("placed_source_unresolved", asset.source_id.clone()))?;
        let Some(StudioPlacedSource { location: StudioPlacedMode::Linked { content_digest, .. }, .. }) = entry.records.placed_sources.get(index).cloned() else {
            return no("layer_not_linked", layer_id.to_owned());
        };
        let bytes = self.source_bytes(&entry, &asset.source_id)?;
        let staged = self.run.stage_tile(bytes).map_err(|c| PlaceRefusal::new(c, "stage the source"))?;
        let document_id = mint_document_id(&doc, &entry.records);
        if let Some(s) = entry.records.placed_sources.get_mut(index) {
            s.location = StudioPlacedMode::Embedded { document_id: document_id.clone(), content_digest };
        }
        match entry.sources.iter_mut().find(|s| s.source_id == asset.source_id) {
            Some(meta) => meta.bytes = Some(staged),
            None => entry.sources.push(SourceMeta { source_id: asset.source_id.clone(), name: "placed".into(), bytes: Some(staged) }),
        }
        folio::validate_raster_records(&entry.records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let body = entry.to_bytes()?;
        if !self.run.set_tiles_and_entry(OP, layer_id, None, ENTRY, body) {
            return Err(self.run_refusal(OP));
        }
        self.place.links.remove(&asset.source_id);
        let revision = self.revision_now();
        self.say(format!("{OP}: {} embedded (revision {revision})", asset.source_id));
        Ok(json!({"layer_id": layer_id, "source_id": asset.source_id, "document_id": document_id, "revision": revision}))
    }
}

#[derive(Clone, Copy)]
enum Edit<'a> {
    Free(&'a FreeTransform),
    Move(f64, f64),
    Reset,
}

/// The document id's two time fields (`SDOC-{high}-{low}-...`), zeros when they are not hex.
fn time_fields(doc: &StudioDocument) -> (String, String) {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let hex = |s: Option<&str>, n: usize| s.filter(|v| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit())).map_or_else(|| "0".repeat(n), str::to_owned);
    (hex(uuid.get(0..8), 8), hex(uuid.get(9..13), 4))
}

/// A fresh `SLYR` id for a placed layer, from the block `7ede-9200` that no other minter uses (`layers`
/// mints `7ede-9000`, [`mint_document_id`] `7ede-9100`): the first counter neither the document nor the
/// entry's records or instances hold. Review F1 of 9d83222c: a placed layer minted from the shared block
/// freed its id on delete, the next new layer of any tool took that id and inherited the stale record. A
/// later place may take a pruned placed id again (the prune runs before the push; the history is linear).
fn mint_placed_layer_id(doc: &StudioDocument, entry: &Entry) -> String {
    mint_entry_layer_id(doc, entry, "9200")
}

/// [`mint_placed_layer_id`] over the id block `7ede-<block>` (lane SMB-PS-FILTER-ADJUST-COMMANDS-1: adjustment
/// layers mint from `9300`).
fn mint_entry_layer_id(doc: &StudioDocument, entry: &Entry, block: &str) -> String {
    let (high, low) = time_fields(doc);
    let taken: BTreeSet<&str> = doc
        .layers
        .iter()
        .map(|l| l.layer_id.as_str())
        .chain(entry.records.layers.iter().map(|r| r.layer_id.as_str()))
        .chain(entry.instances.iter().map(|i| i.layer_id.as_str()))
        .collect();
    let mut n: u64 = 1;
    loop {
        let id = format!("SLYR-{high}-{low}-7ede-{block}-{n:012x}");
        if !taken.contains(id.as_str()) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// The tight bounds of the texels with alpha > 0 of `pixels` (row-major over `bounds`) and those texels;
/// `None` when no texel has alpha. Review F2 of 9d83222c: a pixel layer's reference point is on its
/// pixels, not on its 256-pixel tile cells.
fn opaque_crop(bounds: Rect, pixels: &[[f32; 4]]) -> Option<(Rect, Vec<[f32; 4]>)> {
    let width = usize::try_from(bounds.width).ok().filter(|w| *w > 0)?;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (k, p) in pixels.iter().enumerate() {
        // NaN alpha compares false: not content.
        if p[3] > 0.0 {
            let (x, y) = (k.checked_rem(width)?, k.checked_div(width)?);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x.saturating_add(1));
            y1 = y1.max(y.saturating_add(1));
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (w, h) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    let mut out = Vec::with_capacity(w.checked_mul(h)?);
    for y in y0..y1 {
        let start = y.checked_mul(width)?.checked_add(x0)?;
        out.extend_from_slice(pixels.get(start..start.checked_add(w)?)?);
    }
    let rect = Rect {
        x: bounds.x.checked_add(i64::try_from(x0).ok()?)?,
        y: bounds.y.checked_add(i64::try_from(y0).ok()?)?,
        width: u32::try_from(w).ok()?,
        height: u32::try_from(h).ok()?,
    };
    Some((rect, out))
}

/// The colour range a filtered resample keeps: extended when the source holds a colour outside 0..=1
/// (float data, whose highlights a unit clamp would lose: review F4 of 9d83222c), else unit (no Catmull-Rom
/// overshoot on unit-range data). The tiles written are always `rgb_f32le`; nothing integer is written here.
fn sample_range(pixels: &[[f32; 4]]) -> SampleRange {
    if pixels.iter().any(|p| p[..3].iter().any(|c| *c > 1.0 || *c < 0.0)) { SampleRange::Extended } else { SampleRange::Unit }
}

/// A fresh `SDOC` id for an embedded source: the document id's time fields, a fixed block, the first
/// counter neither the document nor another embedded source holds.
fn mint_document_id(doc: &StudioDocument, records: &StudioRasterRecords) -> String {
    let (high, low) = time_fields(doc);
    let taken: BTreeSet<&str> = records
        .placed_sources
        .iter()
        .filter_map(|s| match &s.location {
            StudioPlacedMode::Embedded { document_id, .. } => Some(document_id.as_str()),
            StudioPlacedMode::Linked { .. } => None,
        })
        .collect();
    let mut n: u64 = 1;
    loop {
        let id = format!("SDOC-{high}-{low}-7ede-9100-{n:012x}");
        if id != doc.document_id && !taken.contains(id.as_str()) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // test fixtures: small pixel indices and ramps
mod in_place_tests {
    //! Review F7 and F1 of d05d2699 (lead rulings 2026-11-13, lane SMB-PS-FILTER-ADJUST-COMMANDS-1) over a layer whose
    //! straight linear pixels the test sets exactly (in the crate, so both forms are read in linear light at full
    //! float precision): `adjustment.apply` in place against the adjustment layer's own render, kind by kind for the
    //! 20 Folio adjustment kinds, over opaque, partly transparent and transparent texels; and a layer holding values
    //! outside 0..=1, which both forms and the smart filter stack refuse by the one range rule.
    use super::*;
    use crate::view::render::{Prefer, render_linear_with_records};
    use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
    use serde_json::Map;

    const W: u32 = 16;
    const H: u32 = 8;

    /// Rows 0-1 opaque ramps; rows 2-3 alpha 0.5 and 0.25 (powers of two: a premultiplied round trip is exact);
    /// rows 4-5 alpha 0.3 (not a power of two); rows 6-7 alpha 0.
    fn fixture() -> Vec<[f32; 4]> {
        let mut px = Vec::new();
        for y in 0..H {
            for x in 0..W {
                let t = x as f32 / (W - 1) as f32;
                let rgb = if y % 2 == 0 { [0.05 + 0.9 * t, 0.9 - 0.8 * t, 0.35] } else { [0.6, 0.1 + 0.7 * t, 0.95 - 0.9 * t] };
                let a = match y {
                    0 | 1 => 1.0,
                    2 => 0.5,
                    3 => 0.25,
                    4 | 5 => 0.3,
                    _ => 0.0,
                };
                px.push([rgb[0], rgb[1], rgb[2], a]);
            }
        }
        px
    }

    /// A new raster document holding only layer `px` with `pixels` (straight linear RGBA over `W x H` at the origin).
    fn seeded(pixels: &[[f32; 4]]) -> (Viewer, String) {
        // A test actor of its own (Viewer::new is the asset-local test host's, behind its feature).
        let actor = hsk_studio_accord::ActorContext::new("lane-fx-account", "lane-fx-principal", "owner", "owner-principal", "space", "lane-fx-session").unwrap();
        let context = hsk_studio_harness_core::RunContext::new(
            actor,
            hsk_studio_harness_core::ClientSessionId::parse("lane-fx-window").unwrap(),
            std::sync::Arc::new(hsk_studio_harness_core::TransientCorrelation::starting_at(1)),
        );
        let mut v = Viewer::with_context(Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context);
        v.new_document(&NewDocument::new(DocumentProfile::Raster, Some((f64::from(W), f64::from(H)))), None).unwrap();
        let id = v.new_raster_layer("px").unwrap();
        let others: Vec<String> = v.run.snapshot().unwrap().document().layers.iter().filter(|l| l.layer_id != id).map(|l| l.layer_id.clone()).collect();
        for other in others {
            v.delete_layer(&other).unwrap();
        }
        let tiles = v.resampled_tiles(&id, Rect { x: 0, y: 0, width: W, height: H }, pixels, Affine2::IDENTITY, ResampleFilter::NearestNeighbour, None).unwrap();
        assert!(v.run.set_tiles("seed", &id, tiles), "seed tiles");
        let (bounds, read) = v.layer_rgba(&id).unwrap();
        // The seeded texels read back exactly over the tiles' bounds (alpha-0 texels read as transparent black).
        for y in 0..H {
            for x in 0..W {
                let want = pixels[usize::try_from(y * W + x).unwrap()];
                if want[3] > 0.0 {
                    assert_eq!(texel(bounds, &read, x, y), want, "texel ({x}, {y})");
                }
            }
        }
        (v, id)
    }

    /// The texel at document pixel `(x, y)` of a plane over `bounds`.
    fn texel(bounds: Rect, plane: &[[f32; 4]], x: u32, y: u32) -> [f32; 4] {
        let k = (i64::from(y) - bounds.y) * i64::from(bounds.width) + (i64::from(x) - bounds.x);
        plane[usize::try_from(k).unwrap()]
    }

    /// The document rendered by the read-back path in linear light: `(frame rect, straight RGBA)`.
    fn linear(v: &Viewer) -> Result<(Rect, Vec<[f32; 4]>), String> {
        let snap = v.run.snapshot().ok_or("no document")?;
        let f = render_linear_with_records(snap, &v.assets, None, None, v.records.as_ref(), v.cancel).map_err(|e| format!("{}: {}", e.code, e.detail))?;
        Ok((Rect { x: f.x, y: f.y, width: f.width, height: f.height }, f.pixels))
    }

    fn command(v: &mut Viewer, id: &str, params: Value) -> Result<Value, CommandRefusal> {
        let map: Map<String, Value> = params.as_object().cloned().unwrap_or_default();
        v.run_command(id, &map)
    }

    fn colour(r: f64, g: f64, b: f64) -> Value {
        json!({"r": r, "g": g, "b": b, "colour_profile_id": null})
    }

    /// One record of each of the 20 Folio `StudioAdjustment` kinds, parameters off the identity (colours black, white
    /// or primaries: a linear-light plan takes no other adjustment colour).
    fn kinds() -> Vec<Value> {
        let stop = |p: f64, c: Value| json!({"position": p, "midpoint": null, "opacity": 100.0, "colour": c});
        let q = |c: f64| json!([c, 0.0, 0.0, 0.0]);
        let cube: Vec<Value> = (0..8).map(|i: i32| json!([f64::from(i >> 2), f64::from(i & 1), f64::from((i >> 1) & 1)])).collect();
        let zero = json!({"hue": 0.0, "saturation": 0.0, "lightness": 0.0});
        vec![
            json!({"kind": "levels", "records": [{"channel": "composite", "input_black": 0.1, "input_white": 0.9, "output_black": 0.0, "output_white": 1.0, "gamma": 1.4}]}),
            json!({"kind": "curves", "curves": [{"channel": "composite", "points": [{"input": 0.0, "output": 0.0, "corner": false}, {"input": 128.0, "output": 160.0, "corner": false}, {"input": 255.0, "output": 255.0, "corner": false}]}]}),
            json!({"kind": "hue_saturation", "master": {"hue": 30.0, "saturation": 20.0, "lightness": -10.0}, "colorize": false, "colorization": zero}),
            json!({"kind": "brightness_contrast", "brightness": 20.0, "contrast": 30.0, "use_legacy": true}),
            json!({"kind": "exposure", "exposure": 0.5, "offset": 0.0, "gamma": 1.0, "use_legacy": false}),
            json!({"kind": "invert"}),
            json!({"kind": "threshold", "level": 128.0}),
            json!({"kind": "posterize", "levels": 4}),
            json!({"kind": "vibrance", "vibrance": 40.0, "saturation": -10.0}),
            json!({"kind": "colour_balance", "shadows": [0.0, 0.0, 0.0], "midtones": [50.0, 0.0, -20.0], "highlights": [0.0, 10.0, 0.0], "preserve_luminosity": true}),
            json!({"kind": "photo_filter", "filter_colour": colour(1.0, 0.0, 0.0), "density": 25.0, "preserve_luminosity": false}),
            json!({"kind": "black_and_white", "red": 40.0, "yellow": 60.0, "green": 40.0, "cyan": 60.0, "blue": 20.0, "magenta": 80.0, "tint_enabled": true, "tint_hue": 35.0, "tint_saturation": 25.0}),
            json!({"kind": "channel_mixer", "outputs": [{"weights": [100.0, 50.0, 0.0], "constant": 10.0}, {"weights": [0.0, 100.0, 0.0], "constant": 0.0}, {"weights": [0.0, 0.0, 100.0], "constant": 0.0}], "monochrome": false}),
            json!({"kind": "selective_colour", "method": "absolute", "reds": q(-25.0), "yellows": q(0.0), "greens": q(0.0), "cyans": q(0.0), "blues": q(0.0), "magentas": q(0.0), "whites": q(0.0), "neutrals": q(0.0), "blacks": q(5.0)}),
            json!({"kind": "gradient_map", "stops": [stop(0.0, colour(0.0, 0.0, 0.0)), stop(100.0, colour(1.0, 0.0, 0.0))], "reverse": false, "dither": false}),
            json!({"kind": "colour_lookup", "lut": "luts/rotate.cube", "lut_kind": "3dlut", "dither": false, "table": {"side": 2, "values": cube}}),
            json!({"kind": "shadows_highlights", "shadow_amount": 35.0, "shadow_tone_width": 50.0, "shadow_radius": 0.0, "highlight_amount": 0.0, "highlight_tone_width": 50.0, "highlight_radius": 0.0, "colour_correction": 20.0, "midtone_contrast": 0.0, "black_clip": 0.01, "white_clip": 0.01}),
            json!({"kind": "tone_compression", "method": "exposure_gamma", "exposure": 1.0, "gamma": 2.0, "compression": 0.0}),
            json!({"kind": "match_colour", "luminance": 100.0, "colour_intensity": 120.0, "fade": 10.0, "neutralize": true, "source": {"source_mean": [0.6, 0.5, 0.4], "source_std": [0.2, 0.2, 0.2], "target_mean": [0.4, 0.4, 0.4], "target_std": [0.1, 0.1, 0.1]}}),
            json!({"kind": "replace_colour", "fuzziness": 40.0, "samples": [colour(1.0, 0.0, 0.0)], "shift": {"hue": 120.0, "saturation": 0.0, "lightness": 0.0}}),
        ]
    }

    /// Largest channel difference between the two renders over the texels of `rows`; alpha must match exactly
    /// everywhere and colour is compared only where alpha > 0.
    fn max_diff(frame: Rect, a: &[[f32; 4]], b: &[[f32; 4]], rows: std::ops::Range<u32>) -> f32 {
        let mut worst = 0.0_f32;
        for y in rows {
            for x in 0..W {
                let k = usize::try_from((i64::from(y) - frame.y) * i64::from(frame.width) + (i64::from(x) - frame.x)).unwrap();
                let (p, q) = (a[k], b[k]);
                assert_eq!(p[3], q[3], "alpha at ({x}, {y})");
                if p[3] > 0.0 {
                    for c in 0..3 {
                        worst = worst.max((p[c] - q[c]).abs());
                    }
                }
            }
        }
        worst
    }

    /// Review F7: for every kind both forms take the record or both refuse it with the same code; when taken, the
    /// in-place layer renders as the adjustment layer over it renders. Exact over opaque texels and over alpha 0.5
    /// and 0.25 (a premultiplied round trip by a power of two is exact). Over alpha 0.3, for every kind, at most
    /// 2.4e-7 (2^-22, two f32 steps at values in 0.5..1): the render composites premultiplied, so the colour the
    /// adjustment layer reads back is (c a) / a, one f32 rounding from the texel's c, and the record's slope (below
    /// 2 for these records) carries that difference; measured at most 1.2e-7 (the test prints each kind's figure).
    /// The modern brightness/contrast is refused by both forms alike (pigment `adjust_unsupported`).
    #[test]
    fn apply_in_place_renders_as_the_adjustment_layer_for_every_kind() {
        let pixels = fixture();
        let mut report = Vec::new();
        for record in kinds() {
            let kind = record["kind"].as_str().unwrap().to_owned();
            let (mut layered, px) = seeded(&pixels);
            layered.select(Some(px.clone()));
            let created = command(&mut layered, "adjustment.create", json!({"adjustment": record}));
            let (mut applied, px2) = seeded(&pixels);
            let done = command(&mut applied, "adjustment.apply", json!({"layer": px2, "adjustment": record}));
            match (created, done) {
                (Err(a), Err(b)) => {
                    assert_eq!(a.code, b.code, "{kind}: both forms refuse alike");
                    report.push(format!("{kind}: refused {}", a.code));
                }
                (Ok(_), Ok(out)) => {
                    assert_eq!(out["pixels"].as_u64(), Some(u64::from(W * 6)), "{kind}: every texel with alpha > 0: {out}");
                    let (frame, layer_form) = linear(&layered).unwrap_or_else(|e| panic!("{kind}: the adjustment layer renders: {e}"));
                    let (frame2, in_place) = linear(&applied).unwrap_or_else(|e| panic!("{kind}: the applied layer renders: {e}"));
                    assert_eq!(frame, frame2);
                    let exact = max_diff(frame, &layer_form, &in_place, 0..4);
                    let partial = max_diff(frame, &layer_form, &in_place, 4..6);
                    max_diff(frame, &layer_form, &in_place, 6..8);
                    report.push(format!("{kind}: exact rows {exact:e}, alpha 0.3 {partial:e}"));
                    assert_eq!(exact, 0.0, "{kind}: opaque and power-of-two alpha texels are exact");
                    assert!(partial <= 2.4e-7, "{kind}: alpha 0.3 within one rounding times the slope: {partial:e}");
                }
                (c, d) => panic!("{kind}: the forms disagree: create {:?} apply {:?}", c.err().map(|e| e.code), d.err().map(|e| e.code)),
            }
        }
        assert!(report.iter().all(|r| !r.contains("refused")), "every kind is taken by both forms: {report:?}");
        let modern = json!({"kind": "brightness_contrast", "brightness": 20.0, "contrast": 30.0, "use_legacy": false});
        let (mut layered, px) = seeded(&pixels);
        layered.select(Some(px));
        let (mut applied, px2) = seeded(&pixels);
        let refusals = (
            command(&mut layered, "adjustment.create", json!({"adjustment": modern})).map(|_| ()).unwrap_err().code,
            command(&mut applied, "adjustment.apply", json!({"layer": px2, "adjustment": modern})).map(|_| ()).unwrap_err().code,
        );
        assert_eq!(refusals, ("adjust_unsupported".to_owned(), "adjust_unsupported".to_owned()));
        eprintln!("{}", report.join("\n"));
        assert_eq!(report.len(), 20);
    }

    /// Review F7 and F1: a layer holding a value outside 0..=1 (a float layer) is refused by the one range rule: the
    /// adjustment layer's composite refuses to render over it, `adjustment.apply` refuses `adjust_out_of_domain`
    /// with nothing written, and a smart filter entry refuses `effect_value_range_unsupported` (the stack renders
    /// unit-range content only) while the layer itself still renders.
    #[test]
    fn a_layer_outside_the_unit_range_is_refused_by_both_forms_and_the_stack() {
        let mut pixels = fixture();
        pixels[3] = [1.5, 0.25, 0.5, 1.0];
        let (mut layered, px) = seeded(&pixels);
        assert!(linear(&layered).is_ok(), "the plain float layer renders");
        layered.select(Some(px.clone()));
        command(&mut layered, "adjustment.create", json!({"adjustment": {"kind": "invert"}})).unwrap();
        let fail = linear(&layered).expect_err("the adjustment layer does not render over values outside 0..=1");
        let (mut applied, px2) = seeded(&pixels);
        let before = applied.layer_rgba(&px2).unwrap();
        let steps = applied.run.history();
        let r = command(&mut applied, "adjustment.apply", json!({"layer": px2, "adjustment": {"kind": "invert"}})).unwrap_err();
        assert_eq!(r.code, "adjust_out_of_domain", "{} (layer form: {fail})", r.detail);
        assert_eq!(applied.layer_rgba(&px2).unwrap(), before, "nothing written");
        assert_eq!(applied.run.history(), steps, "no step");
        let r = command(&mut applied, "effect.apply", json!({"layer": px2, "filter_kind": "sharpen"})).unwrap_err();
        assert_eq!(r.code, "effect_value_range_unsupported", "{}", r.detail);
        assert!(linear(&applied).is_ok(), "the layer still renders");
    }
}
