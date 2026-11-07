//! Place embedded or linked assets into a raster or vector document, and raster Move and Free transform
//! (lane SMB-PLACE-ASSET-1; [STU-RAS-012], [STU-RAS-013], [STU-RAS-014], [STU-RAS-029], [STU-RAS-141],
//! [STU-RAS-142], [STU-ASSET-005]..[STU-ASSET-007], the [STU-ASSET-010] command names). Engine and commands
//! only: no draw code and no panel; a panel lane calls the typed rows `asset.*` and `transform.*`
//! (`view::commands::place`) or these methods.
//!
//! # Manual (no-context operator or model)
//!
//! Every method needs an open document with an artboard (raster or vector; a document with pages only
//! refuses `canvas_unavailable`: layout graphic frames are lane SMB-LAYOUT-PLACE-1). Each mutating call is
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
//!   `middle_centre`). On a placed layer the transform accumulates on the record and the tiles are rendered
//!   again from the source, so the source is resampled once whatever the history ([STU-RAS-014]); on a
//!   pixel layer the pixels are resampled once into new tiles (no canvas clip).
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
//! Refusal codes: `not_open`, `canvas_unavailable`, `no_working_profile`, `resource_unreadable`,
//! `resource_too_large`, the import route's codes (`format_unrecognised`, ...), render codes
//! (`canvas_too_large`, ...), `placed_source_empty`, `link_base_unknown` (no opened file to resolve a link
//! against), `link_outside_document_folder`, `link_uri_invalid`, `layer_unknown`, `layer_not_pixel`,
//! `layer_has_mask`, `layer_empty`, `layer_not_placed`, `layer_not_linked`, `move_not_whole_pixels`,
//! `parameter_out_of_range`, pigment's `transform_*`, `budget_exceeded`, `placed_link_missing`,
//! `placed_link_modified` (run `asset.update_all_modified` first), `placed_link_unauthorized`,
//! `placed_link_unsupported`, `placed_source_size_mismatch`, `placed_source_unresolved`,
//! `raster_records_invalid`, `folio_<code>`, and the Run's own codes.
//!
//! Studio readings (UNVERIFIED against Photoshop and Illustrator, D5): 100 % unless larger than the canvas;
//! the reference point is on the axis-aligned bounds of the layer; skew before rotation; cached pixels
//! clipped to the canvas (the source keeps everything); a link resolves against the opened document's
//! folder, so a save into another folder leaves it `missing`. Cuts: Place from Asset Library (no CKC),
//! Relink, Unembed, Edit Original, Convert to Layers, Export Contents, statistical stacks, warp, perspective,
//! a native `.handshake` package as a source, the text and vector layers of a source document (drawn
//! without them), a duplicate of a placed layer as a shared instance (`duplicate_layer` copies the pixels
//! only), the export fail-closed gate for missing links ([STU-ASSET-009]).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::edit;
use super::layers::{mint_layer_id, new_layer, operation_for};
use super::render::{self, Assets};
use crate::PlacementWrite;
use hsk_studio_composite::lower_extensions::placed::placed_matrix;
use hsk_studio_folio::{
    self as folio, ContentDigest, LayerAppearance, Payload, Resolution, StudioDocument, StudioImageSource, StudioLinkHealth, StudioPlacedAsset, StudioPlacedMode,
    StudioPlacedSource, StudioRasterLayerRecord, StudioRasterRecords, TileRef,
};
use hsk_studio_package::{AssetSource, ImportEntry, ImportRole, LinkedFileResolver};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, PreparedTransform, ResampleFilter, exact_sin_cos};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

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
/// Red commit of lane SMB-PLACE-ASSET-1: the commands refuse `not_built` until the engine lands.
const BUILT: bool = false;

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
/// session holds no [`ENTRY`]) and the link states of the last check by source id.
#[derive(Clone, Debug, Default)]
pub struct PlaceState {
    import: Option<StudioRasterRecords>,
    links: BTreeMap<String, &'static str>,
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

    /// After an open: the import records it read, then the effective set and the link states.
    pub(super) fn place_opened(&mut self, import: Option<StudioRasterRecords>) {
        self.place = PlaceState { import, links: BTreeMap::new() };
        self.place_sync();
        if let Ok(Some(entry)) = self.read_place_entry() {
            self.check_links(&entry);
        }
    }

    /// The opened document's folder, canonical.
    fn link_base(&self) -> Res<PathBuf> {
        let parent = self.source.as_deref().and_then(Path::parent).ok_or_else(|| PlaceRefusal::new("link_base_unknown", "a link resolves against the opened document's folder: open or save and reopen the document first"))?;
        let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
        std::fs::canonicalize(parent).map_err(|e| PlaceRefusal::new("link_base_unknown", format!("{:?}", e.kind())))
    }

    /// The [STU-ASSET-007] state of a linked source now.
    fn link_state(&self, uri: &str, content_digest: &str) -> &'static str {
        match self.link_base() {
            Ok(base) => state_token(LinkedFileResolver::new(&crate::harness::TrustAll, base).check(uri, content_digest)),
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
                "resolved" => self.read_link(uri),
                "unresolved" => no("link_base_unknown", uri.clone()),
                state => no(&format!("placed_link_{state}"), uri.clone()),
            },
        }
    }

    /// Imports `bytes` and renders their composite in linear light.
    fn decode_source(&self, bytes: &[u8]) -> Res<SourceImage> {
        let imported = crate::import::import_as(bytes, &crate::harness::TrustAll, self.run.actor(), self.cancel).map_err(|e| PlaceRefusal::new(e.code(), "the source does not import"))?;
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
        let frame = render::render_linear_with_records(&imported.snapshot, &assets, None, None, records.as_ref(), self.cancel).map_err(|f| PlaceRefusal::new(f.code, f.detail))?;
        if frame.width == 0 || frame.height == 0 || frame.pixels.is_empty() {
            return no("placed_source_empty", "the source renders no pixel");
        }
        Ok(SourceImage { width: frame.width, height: frame.height, pixels: frame.pixels })
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
                        let px = prepared.sample_rgba(x, y, texel).unwrap_or([0.0; 4]);
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

    /// The cached display tiles of a placement, clipped to the canvas.
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
        let r = if BUILT { f(self) } else { no("not_built", op.to_owned()) };
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
        let shared = entry
            .records
            .placed_sources
            .iter()
            .find(|s| {
                s.width == w
                    && s.height == h
                    && match (&s.location, &uri) {
                        (StudioPlacedMode::Linked { uri: u, content_digest, .. }, Some(want)) => u == want && *content_digest == digest,
                        (StudioPlacedMode::Embedded { content_digest, .. }, None) => *content_digest == digest,
                        _ => false,
                    }
            })
            .map(|s| s.source_id.clone());
        let name = label(&opts.name.clone().unwrap_or_else(|| path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())));
        let source_id = match &shared {
            Some(id) => id.clone(),
            None => {
                let taken: BTreeSet<&str> = entry.records.placed_sources.iter().map(|s| s.source_id.as_str()).collect();
                let id = (1u64..).map(|n| format!("src-{n}")).find(|id| !taken.contains(id.as_str())).unwrap_or_default();
                let (location, staged) = match &uri {
                    Some(u) => (StudioPlacedMode::Linked { uri: u.clone(), content_digest: digest.clone(), health: StudioLinkHealth::UpToDate }, None),
                    None => {
                        let staged = self.run.stage_tile(bytes).map_err(|c| PlaceRefusal::new(c, "stage the source"))?;
                        (StudioPlacedMode::Embedded { document_id: mint_document_id(&doc, &entry.records), content_digest: digest.clone() }, Some(staged))
                    }
                };
                entry.records.placed_sources.push(StudioPlacedSource { source_id: id.clone(), location, width: w, height: h, uses: Vec::new(), asset_id: None });
                entry.sources.push(SourceMeta { source_id: id.clone(), name: name.clone(), bytes: staged });
                id
            }
        };
        let scale = match opts.scale_pct {
            Some(p) => p / 100.0,
            None => 1.0f64.min(f64::from(canvas.width) / w).min(f64::from(canvas.height) / h),
        };
        let x = opts.x.unwrap_or(canvas.x as f64 + (f64::from(canvas.width) - w * scale) / 2.0);
        let y = opts.y.unwrap_or(canvas.y as f64 + (f64::from(canvas.height) - h * scale) / 2.0);
        let transform = [scale, 0.0, 0.0, scale, x, y];
        let layer_id = mint_layer_id(&doc, u64::try_from(doc.layers.len()).unwrap_or(0));
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
        if !self.run.restructure_and_entry(OP, &[write], ENTRY, body) {
            return Err(self.run_refusal(OP));
        }
        self.selected = Some(layer_id.clone());
        if uri.is_some() {
            self.place.links.insert(source_id.clone(), "resolved");
        }
        let revision = self.revision_now();
        self.say(format!("{OP} {} as {layer_id} ({count} tile(s), revision {revision})", kind.token()));
        Ok(json!({
            "layer_id": layer_id, "source_id": source_id, "placement_kind": kind.token(), "uri": uri, "tiles": count, "scale_pct": scale * 100.0,
            "transform": transform, "resample": filter.key(), "shared": shared.is_some(), "revision": revision,
        }))
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
        if self.records.as_ref().and_then(|r| r.layer(layer_id)).is_some_and(|r| r.fill_layer.is_some()) {
            return no("layer_not_pixel", "a fill layer");
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
        let (bounds, pixels) = self.layer_rgba(layer_id)?;
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

    fn placements(&self, live: bool) -> Value {
        if !BUILT {
            return json!({"placements": [], "sources": 0});
        }
        let (Some(doc), Ok(Some(mut entry))) = (self.run.snapshot().map(|s| s.document()), self.read_place_entry()) else {
            return json!({"placements": [], "sources": 0});
        };
        entry.prune(doc);
        let rows: Vec<Value> = entry
            .records
            .layers
            .iter()
            .filter_map(|r| r.placed_asset.as_ref().map(|a| (r, a)))
            .map(|(r, a)| {
                let state = match entry.records.placed_source(&a.source_id).map(|s| &s.location) {
                    Some(StudioPlacedMode::Embedded { .. }) => "embedded",
                    Some(StudioPlacedMode::Linked { uri, content_digest, .. }) if live => self.link_state(uri, content_digest),
                    Some(StudioPlacedMode::Linked { .. }) => self.place.links.get(&a.source_id).copied().unwrap_or("unresolved"),
                    None => "unresolved",
                };
                Self::placement_json(&entry, r, a, state)
            })
            .collect();
        json!({"placements": rows, "sources": entry.records.placed_sources.len()})
    }

    /// `asset.list_placements` (module docs): every placement, link states checked now.
    pub fn list_placements(&mut self) -> Value {
        if let Ok(Some(entry)) = self.read_place_entry() {
            self.check_links(&entry);
        }
        self.placements(true)
    }

    /// `state_json()["placed"]`: the placements with the link states of the last check.
    pub fn placed_json(&self) -> Value {
        self.placements(false)
    }

    /// `asset.update_all_modified` (module docs). Returns `{updated: [source_id], missing: [source_id],
    /// layers: [layer_id], changed, revision}`.
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
                    for (layer_id, transform, filter) in instances {
                        let tiles = self.placement_tiles(&layer_id, &image, transform, filter)?;
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
            return Ok(json!({"updated": updated, "missing": missing, "layers": layers, "changed": false, "revision": self.revision_now()}));
        }
        folio::validate_raster_records(&entry.records, &doc, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map_err(|d| folio_refusal(&d))?;
        let body = entry.to_bytes()?;
        if !self.run.set_layers_tiles_and_entry(OP, &writes, ENTRY, body) {
            return Err(self.run_refusal(OP));
        }
        self.check_links(&entry);
        let revision = self.revision_now();
        self.say(format!("{OP}: {} updated, {} missing (revision {revision})", updated.len(), missing.len()));
        Ok(json!({"updated": updated, "missing": missing, "layers": layers, "changed": true, "revision": revision}))
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

/// A fresh `SDOC` id for an embedded source: the document id's time fields, a fixed block, the first
/// counter neither the document nor another embedded source holds.
fn mint_document_id(doc: &StudioDocument, records: &StudioRasterRecords) -> String {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let hex = |s: Option<&str>, n: usize| s.filter(|v| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit())).map_or_else(|| "0".repeat(n), str::to_owned);
    let (high, low) = (hex(uuid.get(0..8), 8), hex(uuid.get(9..13), 4));
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
