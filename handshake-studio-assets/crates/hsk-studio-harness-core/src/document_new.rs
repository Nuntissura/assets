//! New Document and the rules of artboards and pages (lane SMB-DOC-NEW-1; [STU-SHL-023]
//! `studio.document.new`, [STU-SHL-026] Artboards and Pages & Spreads, [STU-DOC-001], [STU-VEC-028],
//! [STU-VEC-165], [STU-RAS-145]): the typed request, the blank document of each profile, the hard
//! bounds and the [STU-VEC-165] option records. Every refusal here is a stable code and happens before
//! anything is allocated from the request. [`crate::Run`] commits (module `harness::containers`).
//!
//! A profile is what a new document starts with, nothing more: the document stores no profile
//! ([STU-DOC-001]: one document type). `vector`: one artboard, no layers. `raster`: one artboard, one
//! empty raster layer, whole pixels only. `design`: no container, or one artboard when a size is
//! stated. `layout`: one spread of one page, points canonical (STU-LAY-100): a size in `px` takes one
//! point per pixel, so a 1920 by 1080 px deck frames at 1920 by 1080.
//!
//! `board` (lane SMB-BOARD-1, [STU-WB-001], [STU-WB-120]): one artboard, the document's first board, at
//! the stated size or [`BOARD_DEFAULT_SIZE`] (pixels, in the document's unit), and whiteboard mode, set once at
//! creation in the [`ARTBOARD_OPTIONS_ENTRY`] entry ([`whiteboard_options_bytes`], written by
//! `Run::new_document` for the command line and the viewer alike). Reading (lead ruling 2026-11-13, review F4
//! option B): the mode lives in that document-level entry, not in a `StudioDocument` field (48 struct-literal
//! sites); a document without it is not a whiteboard and never becomes one.
//!
//! Studio readings, declared (vendor parity UNVERIFIED, D5): a new artboard goes to the
//! right of the others at [`ARTBOARD_GAP`]; a raster document starts transparent; ids are minted here
//! as a standalone stand-in for a host `IdMint`, like `import::SourceMint`.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{self as folio, Bounds, GeometryUnit, LayoutEdges, Length, PageSide, PageSpreadSchema, StudioPage, StudioPageSpread};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

/// [STU-VEC-115]: the hard maximum of an artboard side, in points. A result past it fails closed.
pub const MAX_SIDE_PT: f64 = 16348.0;
/// The largest raster canvas side: a raster document is not bound by the vector art domain. A render
/// past `MAX_VIEW_PIXELS` is still refused by the viewer, typed.
pub const MAX_RASTER_SIDE_PX: f64 = 300_000.0;
/// The largest coordinate of a container, in document units.
pub const MAX_POSITION: f64 = 10_000_000.0;
/// The space between artboards when a new one states no position, in document units.
pub const ARTBOARD_GAP: f64 = 64.0;
/// Most artboards in one document (a typed refusal, `too_many_artboards`).
pub const MAX_ARTBOARDS: usize = 1000;
/// Most pages in one document (a typed refusal, `too_many_pages`).
pub const MAX_PAGES: usize = 9999;
/// Longest artboard or spread name in bytes.
pub const MAX_NAME_BYTES: usize = 1024;
/// The size of a new board when none is stated (lane SMB-BOARD-1; Studio-declared), in pixels (converted to
/// the document's unit).
pub const BOARD_DEFAULT_SIZE: (f64, f64) = (1920.0, 1080.0);
/// The top-level key of the [`ARTBOARD_OPTIONS_ENTRY`] entry that marks a whiteboard document ([STU-WB-001]).
pub const WHITEBOARD_MODE_KEY: &str = "whiteboard_mode";
/// The margins of a new page when they leave a positive margin box, in points.
pub const DEFAULT_MARGIN_PT: f64 = 36.0;
/// The session document entry holding [`ArtboardOptions`] (`hsk_studio_session::DOCUMENT_ENTRY_TOKENS`).
pub const ARTBOARD_OPTIONS_ENTRY: &str = "artboard_options";
/// Schema of that entry.
pub const ARTBOARD_OPTIONS_SCHEMA: &str = "hsk.studio.artboard_options@1";
/// [STU-VEC-165]: the hard bounds of `ruler_pixel_aspect_ratio`.
pub const RULER_ASPECT_RANGE: (f64, f64) = (0.1, 10.0);
/// Largest stored options entry in bytes, checked before parsing.
pub const MAX_OPTIONS_BYTES: usize = 4 * 1024 * 1024;

/// The profiles of `document.new`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentProfile {
    Vector,
    Raster,
    Design,
    Layout,
    /// [STU-WB-001] whiteboard mode (lane SMB-BOARD-1).
    Board,
}

impl DocumentProfile {
    pub const TOKENS: [&'static str; 5] = ["vector", "raster", "design", "layout", "board"];

    /// A token as a profile: `profile_unknown`.
    pub fn parse(token: &str) -> Result<Self, &'static str> {
        match token {
            "vector" => Ok(Self::Vector),
            "raster" => Ok(Self::Raster),
            "design" => Ok(Self::Design),
            "layout" => Ok(Self::Layout),
            "board" => Ok(Self::Board),
            _ => Err("profile_unknown"),
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Raster => "raster",
            Self::Design => "design",
            Self::Layout => "layout",
            Self::Board => "board",
        }
    }
}

/// A unit token as Folio's geometry unit: `unit_unknown`.
pub fn parse_unit(token: &str) -> Result<GeometryUnit, &'static str> {
    match token {
        "px" => Ok(GeometryUnit::Px),
        "pt" => Ok(GeometryUnit::Pt),
        "mm" => Ok(GeometryUnit::Mm),
        "in" => Ok(GeometryUnit::In),
        _ => Err("unit_unknown"),
    }
}

/// A colour mode token as its [STU-RAS-145] value: `colour_mode_unknown`. A document enters duotone
/// only through the duotone command ([STU-RAS-145]), so a new one is refused `colour_mode_not_creatable`.
pub fn parse_colour_mode(token: &str) -> Result<u8, &'static str> {
    match token {
        "grayscale" => Ok(1),
        "rgb" => Ok(2),
        "cmyk" => Ok(3),
        "lab" => Ok(4),
        "bitmap" => Ok(5),
        "indexed" => Ok(6),
        "multichannel" => Ok(7),
        "duotone" => Err("colour_mode_not_creatable"),
        _ => Err("colour_mode_unknown"),
    }
}

/// What `document.new` is asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDocument {
    pub profile: DocumentProfile,
    /// The stated size in `unit`: both or neither ([`DocumentProfile::Design`] alone may state none).
    pub size: Option<(f64, f64)>,
    pub unit: GeometryUnit,
    /// [STU-RAS-145] value, from [`parse_colour_mode`].
    pub colour_mode: u8,
    pub bits_per_channel: u8,
    /// The first artboard's name; `Artboard 1` when absent.
    pub artboard_name: Option<String>,
}

impl NewDocument {
    /// The default request of a profile: px, RGB, 8 bits.
    pub fn new(profile: DocumentProfile, size: Option<(f64, f64)>) -> Self {
        Self { profile, size, unit: GeometryUnit::Px, colour_mode: 2, bits_per_channel: 8, artboard_name: None }
    }

    /// Every refusal `document.new` has before a document exists.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=7).contains(&self.colour_mode) {
            return Err("colour_mode_unknown");
        }
        if ![1, 8, 16, 32].contains(&self.bits_per_channel) || (self.bits_per_channel == 1 && self.colour_mode != 5) {
            return Err("bit_depth_invalid");
        }
        if let Some(name) = &self.artboard_name {
            check_name(name)?;
        }
        match (self.profile, self.size) {
            (DocumentProfile::Design | DocumentProfile::Board, None) => Ok(()),
            (_, None) => Err("size_required"),
            (DocumentProfile::Raster, Some(_)) if self.unit != GeometryUnit::Px => Err("raster_needs_px"),
            (DocumentProfile::Raster, Some((w, h))) => {
                if !side_fits(w, MAX_RASTER_SIDE_PX) || !side_fits(h, MAX_RASTER_SIDE_PX) {
                    return Err("dimension_out_of_range");
                }
                if w.fract() != 0.0 || h.fract() != 0.0 { Err("size_not_whole_pixels") } else { Ok(()) }
            }
            (_, Some((w, h))) => check_size(w, h, self.unit),
        }
    }
}

fn side_fits(value: f64, max: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= max
}

/// `value` in `unit` as points. A pixel is one point (the viewer frames a document unit as one pixel).
pub fn to_points(value: f64, unit: GeometryUnit) -> f64 {
    match unit {
        GeometryUnit::Px | GeometryUnit::Pt => value,
        GeometryUnit::In => value * 72.0,
        GeometryUnit::Mm => value * 72.0 / 25.4,
    }
}

/// `points` in `unit`, the inverse of [`to_points`].
pub fn from_points(points: f64, unit: GeometryUnit) -> f64 {
    match unit {
        GeometryUnit::Px | GeometryUnit::Pt => points,
        GeometryUnit::In => points / 72.0,
        GeometryUnit::Mm => points * 25.4 / 72.0,
    }
}

/// [STU-VEC-115] and whole pixels: `dimension_out_of_range`, `size_not_whole_pixels`.
pub fn check_size(width: f64, height: f64, unit: GeometryUnit) -> Result<(), &'static str> {
    for side in [width, height] {
        if !side.is_finite() || side <= 0.0 || to_points(side, unit) > MAX_SIDE_PT {
            return Err("dimension_out_of_range");
        }
        if unit == GeometryUnit::Px && side.fract() != 0.0 {
            return Err("size_not_whole_pixels");
        }
    }
    Ok(())
}

/// A position in document units: `position_out_of_range`, `size_not_whole_pixels`.
pub fn check_position(value: f64, unit: GeometryUnit) -> Result<(), &'static str> {
    if !value.is_finite() || value.abs() > MAX_POSITION {
        return Err("position_out_of_range");
    }
    if unit == GeometryUnit::Px && value.fract() != 0.0 {
        return Err("size_not_whole_pixels");
    }
    Ok(())
}

/// An artboard or spread name: `name_too_long`, `name_invalid` (a control character).
pub fn check_name(name: &str) -> Result<(), &'static str> {
    if name.len() > MAX_NAME_BYTES {
        return Err("name_too_long");
    }
    if name.chars().any(char::is_control) {
        return Err("name_invalid");
    }
    Ok(())
}

// ----- ids --------------------------------------------------------------------------------------

static MINTED: AtomicU64 = AtomicU64::new(0);

/// A fresh `SDOC` id, UUIDv7-shaped: `now_ms`, then the run's correlation, a process counter and the
/// process id, so two processes in one millisecond mint two ids.
pub fn mint_document_id(now_ms: u64, correlation: u64) -> String {
    let n = MINTED.fetch_add(1, Ordering::Relaxed);
    let high = now_ms.wrapping_shr(16) & 0xffff_ffff;
    let low = now_ms & 0xffff;
    let a = correlation & 0xfff;
    let b = n & 0xfff;
    let tail = u64::from(std::process::id()).wrapping_shl(16) ^ correlation.wrapping_shr(12) ^ n.wrapping_shr(12);
    format!("SDOC-{high:08x}-{low:04x}-7{a:03x}-8{b:03x}-{:012x}", tail & 0xffff_ffff_ffff)
}

/// A `prefix` id under `document_id`'s time fields, the first counter from `first` that `taken` does
/// not answer. A foreign id's time fields are used as they are: Folio validated their shape.
pub fn mint_child_id(document_id: &str, prefix: &str, first: u64, taken: impl Fn(&str) -> bool) -> String {
    let uuid = document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = first;
    loop {
        let id = format!("{prefix}-{high}-{low}-7d0c-8000-{n:012x}");
        if !taken(&id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// The first document-local page key from `page-{first}` that the document does not hold; `None` when the
/// numbers run out past `u64::MAX` before one is free (`page_number_exhausted`). The mint never wraps and
/// never repeats a number, so it ends after at most one more step than the document has pages (lane
/// SMB-R60-FOLLOWUPS-1 item 4: a saturating step at `u64::MAX` looped forever).
pub fn mint_page_id(document: &folio::StudioDocument, first: u64) -> Option<String> {
    let mut n = first;
    loop {
        let id = format!("page-{n}");
        if !document.page_spreads.iter().flat_map(|s| &s.pages).any(|p| p.page_id == id) {
            return Some(id);
        }
        n = n.checked_add(1)?;
    }
}

/// Every page of the document.
pub fn page_count(document: &folio::StudioDocument) -> usize {
    document.page_spreads.iter().map(|s| s.pages.len()).fold(0, usize::saturating_add)
}

// ----- blank documents ----------------------------------------------------------------------------

/// A spread of one page, margins [`DEFAULT_MARGIN_PT`] (a quarter of the short side when that would
/// leave no margin box), bounds in `unit`. `width` and `height` are points, positive.
pub fn one_page_spread(spread_id: String, name: String, page_id: String, width: f64, height: f64, unit: GeometryUnit) -> StudioPageSpread {
    let pt = |value: f64| Length { value, unit: GeometryUnit::Pt };
    let margin = DEFAULT_MARGIN_PT.min(width.min(height) / 4.0);
    let edge = || pt(margin);
    let length = |value: f64| Length { value: from_points(value, unit), unit };
    StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: spread_id,
        name,
        parent: folio::Parent::Root(()),
        bounds: Bounds { x: Length { value: 0.0, unit }, y: Length { value: 0.0, unit }, width: length(width), height: length(height) },
        pages: vec![StudioPage {
            page_id,
            width: pt(width),
            height: pt(height),
            side: PageSide::RightHand,
            margins: LayoutEdges { top: edge(), right: edge(), bottom: edge(), left: edge() },
            bleed: None,
            applied_trap_preset: None,
        }],
    }
}

/// The blank document of `spec`, revision 0 (`document_id` from [`mint_document_id`]): no profile
/// field, no entry, nothing but the containers and the working space the viewer needs. `spec` is
/// validated first.
pub fn blank_document(spec: &NewDocument, document_id: &str) -> Result<folio::StudioDocument, &'static str> {
    spec.validate()?;
    let child = |prefix: &str, n: u64| mint_child_id(document_id, prefix, n, |_| false);
    let profile = child("SCPF", 0xffff);
    let unit = if spec.profile == DocumentProfile::Layout { GeometryUnit::Pt } else { spec.unit };
    let mut artboards = Vec::new();
    let mut page_spreads = Vec::new();
    match (spec.profile, spec.size) {
        (DocumentProfile::Layout, Some((w, h))) => {
            page_spreads.push(serde_json::to_value(one_page_spread(child("SPGS", 1), "spread 1".into(), "page-1".into(), to_points(w, spec.unit), to_points(h, spec.unit), GeometryUnit::Pt)).map_err(|_| "document_encode")?);
        }
        (_, Some((w, h))) => {
            let first = if spec.profile == DocumentProfile::Board { "Board 1" } else { "Artboard 1" };
            let name = spec.artboard_name.clone().unwrap_or_else(|| first.to_owned());
            let token = unit_token(unit);
            let len = |value: f64| json!({"value": value, "unit": token});
            artboards.push(json!({
                "schema_id": "hsk.studio.artboard@1", "artboard_id": child("SART", 1), "name": name, "parent": null,
                "bounds": {"x": len(0.0), "y": len(0.0), "width": len(w), "height": len(h)},
            }));
        }
        (DocumentProfile::Board, None) => {
            let (w, h) = (from_points(BOARD_DEFAULT_SIZE.0, unit), from_points(BOARD_DEFAULT_SIZE.1, unit));
            let token = unit_token(unit);
            let len = |value: f64| json!({"value": value, "unit": token});
            artboards.push(json!({
                "schema_id": "hsk.studio.artboard@1", "artboard_id": child("SART", 1), "name": spec.artboard_name.clone().unwrap_or_else(|| "Board 1".to_owned()), "parent": null,
                "bounds": {"x": len(0.0), "y": len(0.0), "width": len(w), "height": len(h)},
            }));
        }
        (_, None) => {}
    }
    let (layers, operations, outputs) = if spec.profile == DocumentProfile::Raster {
        let layer = child("SLYR", 1);
        (
            json!([{"schema_id": "hsk.studio.layer@1", "layer_id": layer, "name": "Background", "parent": null, "kind": "raster", "payload": {"payload_kind": "raster", "tiles": []}, "visible": true}]),
            json!([{"layer_id": layer, "operation_key": "source", "operation_kind": "source", "inputs": [], "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}]),
            json!([{"layer_id": layer, "operation_key": "source", "port_key": "out"}]),
        )
    } else {
        (json!([]), json!([]), json!([]))
    };
    let wire = json!({
        "schema_id": "hsk.studio.document@1", "document_id": document_id, "revision": 0, "geometry_unit": unit_token(unit),
        "colour_mode": spec.colour_mode, "bits_per_channel": spec.bits_per_channel,
        "working_space_rgb": profile, "working_space_cmyk": null, "working_space_gray": null, "working_space_spot": null, "blending_space": profile,
        "artboards": artboards, "page_spreads": page_spreads, "layers": layers,
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": operations, "edges": [], "outputs": outputs},
    });
    folio_document(wire)
}

fn folio_document(wire: Value) -> Result<folio::StudioDocument, &'static str> {
    serde_json::from_value(wire).map_err(|_| "document_encode")
}

const fn unit_token(unit: GeometryUnit) -> &'static str {
    match unit {
        GeometryUnit::Px => "px",
        GeometryUnit::Pt => "pt",
        GeometryUnit::Mm => "mm",
        GeometryUnit::In => "in",
    }
}

// ----- artboard options ([STU-VEC-165]) -----------------------------------------------------------

/// [STU-VEC-165] `background_fill`. The captured value 3 is absent and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundFill {
    White,
    Black,
    Transparent,
    Custom,
}

impl BackgroundFill {
    pub const TOKENS: [&'static str; 4] = ["artboard_fill.white", "artboard_fill.black", "artboard_fill.transparent", "artboard_fill.custom"];

    pub fn parse(token: &str) -> Result<Self, &'static str> {
        match token {
            "artboard_fill.white" => Ok(Self::White),
            "artboard_fill.black" => Ok(Self::Black),
            "artboard_fill.transparent" => Ok(Self::Transparent),
            "artboard_fill.custom" => Ok(Self::Custom),
            _ => Err("background_fill_unknown"),
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::White => "artboard_fill.white",
            Self::Black => "artboard_fill.black",
            Self::Transparent => "artboard_fill.transparent",
            Self::Custom => "artboard_fill.custom",
        }
    }
}

/// The [STU-VEC-165] fields not on `StudioArtboard` itself. `None` is the spec's `unknown` default:
/// the field has never been set.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtboardOptions {
    pub artboard_id: String,
    /// Relative to the artboard's lower-left corner; the default is that corner.
    pub ruler_origin: [f64; 2],
    pub ruler_pixel_aspect_ratio: Option<f64>,
    pub show_center_mark: Option<bool>,
    pub show_cross_hairs: Option<bool>,
    pub show_safe_areas: Option<bool>,
    pub fade_region_outside_artboard: Option<bool>,
    pub update_while_dragging: Option<bool>,
    pub background_fill: BackgroundFill,
    pub preset: Option<String>,
}

impl ArtboardOptions {
    /// The spec's defaults for `artboard_id`.
    pub fn new(artboard_id: &str) -> Self {
        Self {
            artboard_id: artboard_id.to_owned(),
            ruler_origin: [0.0, 0.0],
            ruler_pixel_aspect_ratio: None,
            show_center_mark: None,
            show_cross_hairs: None,
            show_safe_areas: None,
            fade_region_outside_artboard: None,
            update_while_dragging: None,
            background_fill: BackgroundFill::Transparent,
            preset: None,
        }
    }

    fn check(&self) -> Result<(), &'static str> {
        if !self.ruler_origin.iter().all(|v| v.is_finite() && v.abs() <= MAX_POSITION) {
            return Err("position_out_of_range");
        }
        if self.ruler_pixel_aspect_ratio.is_some_and(|r| !(RULER_ASPECT_RANGE.0..=RULER_ASPECT_RANGE.1).contains(&r)) {
            return Err("ruler_aspect_out_of_range");
        }
        if let Some(preset) = &self.preset {
            check_name(preset)?;
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("artboard_id".into(), json!(self.artboard_id));
        o.insert("ruler_origin".into(), json!(self.ruler_origin));
        o.insert("background_fill".into(), json!(self.background_fill.token()));
        for (key, value) in self.flags() {
            if let Some(flag) = value {
                o.insert(key.into(), json!(flag));
            }
        }
        if let Some(r) = self.ruler_pixel_aspect_ratio {
            o.insert("ruler_pixel_aspect_ratio".into(), json!(r));
        }
        if let Some(p) = &self.preset {
            o.insert("preset".into(), json!(p));
        }
        Value::Object(o)
    }

    const fn flags(&self) -> [(&'static str, Option<bool>); 5] {
        [
            ("show_center_mark", self.show_center_mark),
            ("show_cross_hairs", self.show_cross_hairs),
            ("show_safe_areas", self.show_safe_areas),
            ("fade_region_outside_artboard", self.fade_region_outside_artboard),
            ("update_while_dragging", self.update_while_dragging),
        ]
    }

    fn from_json(value: &Value) -> Result<Self, &'static str> {
        const BAD: &str = "artboard_options_invalid";
        let o = value.as_object().ok_or(BAD)?;
        let flag = |key: &str| match o.get(key) {
            None => Ok(None),
            Some(v) => v.as_bool().map(Some).ok_or(BAD),
        };
        for key in o.keys() {
            if !["artboard_id", "ruler_origin", "background_fill", "show_center_mark", "show_cross_hairs", "show_safe_areas", "fade_region_outside_artboard", "update_while_dragging", "ruler_pixel_aspect_ratio", "preset"].contains(&key.as_str()) {
                return Err(BAD);
            }
        }
        let origin = o.get("ruler_origin").and_then(Value::as_array).ok_or(BAD)?;
        let [x, y] = origin.as_slice() else { return Err(BAD) };
        let (x, y) = (x.as_f64().ok_or(BAD)?, y.as_f64().ok_or(BAD)?);
        let options = Self {
            artboard_id: o.get("artboard_id").and_then(Value::as_str).ok_or(BAD)?.to_owned(),
            ruler_origin: [x, y],
            ruler_pixel_aspect_ratio: match o.get("ruler_pixel_aspect_ratio") {
                None => None,
                Some(v) => Some(v.as_f64().ok_or(BAD)?),
            },
            show_center_mark: flag("show_center_mark")?,
            show_cross_hairs: flag("show_cross_hairs")?,
            show_safe_areas: flag("show_safe_areas")?,
            fade_region_outside_artboard: flag("fade_region_outside_artboard")?,
            update_while_dragging: flag("update_while_dragging")?,
            background_fill: BackgroundFill::parse(o.get("background_fill").and_then(Value::as_str).ok_or(BAD)?).map_err(|_| BAD)?,
            preset: match o.get("preset") {
                None => None,
                Some(v) => Some(v.as_str().ok_or(BAD)?.to_owned()),
            },
        };
        options.check().map_err(|_| BAD)?;
        Ok(options)
    }
}

/// A partial write of `artboard.set_options`: `None` leaves the field as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtboardOptionsEdit {
    pub ruler_origin: Option<[f64; 2]>,
    pub ruler_pixel_aspect_ratio: Option<f64>,
    pub show_center_mark: Option<bool>,
    pub show_cross_hairs: Option<bool>,
    pub show_safe_areas: Option<bool>,
    pub fade_region_outside_artboard: Option<bool>,
    pub update_while_dragging: Option<bool>,
    pub background_fill: Option<BackgroundFill>,
    pub preset: Option<String>,
}

impl ArtboardOptionsEdit {
    /// `options` with this edit applied and checked.
    pub fn apply(&self, options: &ArtboardOptions) -> Result<ArtboardOptions, &'static str> {
        let next = ArtboardOptions {
            artboard_id: options.artboard_id.clone(),
            ruler_origin: self.ruler_origin.unwrap_or(options.ruler_origin),
            ruler_pixel_aspect_ratio: self.ruler_pixel_aspect_ratio.or(options.ruler_pixel_aspect_ratio),
            show_center_mark: self.show_center_mark.or(options.show_center_mark),
            show_cross_hairs: self.show_cross_hairs.or(options.show_cross_hairs),
            show_safe_areas: self.show_safe_areas.or(options.show_safe_areas),
            fade_region_outside_artboard: self.fade_region_outside_artboard.or(options.fade_region_outside_artboard),
            update_while_dragging: self.update_while_dragging.or(options.update_while_dragging),
            background_fill: self.background_fill.unwrap_or(options.background_fill),
            preset: self.preset.clone().or_else(|| options.preset.clone()),
        };
        next.check()?;
        Ok(next)
    }
}

/// The body of the [`ARTBOARD_OPTIONS_ENTRY`] entry: `document_id` binds it, and an artboard the
/// document no longer holds is inert (an undo brings it back live). `artboard.delete` takes its row in its own
/// step and `artboard.set_options` writes the live artboards' rows only, so the entry stays bounded by the
/// live artboards over any number of add, set and delete cycles (R60-a3 probe lead, SMB-DOC-NEW-1). Parsed
/// strictly: an entry that does not parse is `artboard_options_invalid`, on read and on any write over it.
pub fn parse_options(bytes: &[u8], document_id: &str) -> Result<Vec<ArtboardOptions>, &'static str> {
    const BAD: &str = "artboard_options_invalid";
    if bytes.len() > MAX_OPTIONS_BYTES {
        return Err(BAD);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| BAD)?;
    let o = value.as_object().ok_or(BAD)?;
    // Lane SMB-BOARD-1: a fourth key is only `whiteboard_mode: true`.
    let marked = o.get(WHITEBOARD_MODE_KEY).is_some();
    if marked && o.get(WHITEBOARD_MODE_KEY) != Some(&Value::Bool(true)) {
        return Err(BAD);
    }
    if o.len() != if marked { 4 } else { 3 } || o.get("schema").and_then(Value::as_str) != Some(ARTBOARD_OPTIONS_SCHEMA) || o.get("document_id").and_then(Value::as_str) != Some(document_id) {
        return Err(BAD);
    }
    let rows = o.get("artboards").and_then(Value::as_array).ok_or(BAD)?;
    if rows.len() > MAX_ARTBOARDS.saturating_mul(2) {
        return Err(BAD);
    }
    let parsed = rows.iter().map(ArtboardOptions::from_json).collect::<Result<Vec<_>, _>>()?;
    for (i, row) in parsed.iter().enumerate() {
        if parsed[..i].iter().any(|other| other.artboard_id == row.artboard_id) {
            return Err(BAD);
        }
    }
    Ok(parsed)
}

/// The entry body of `rows`.
pub fn options_bytes(rows: &[ArtboardOptions], document_id: &str) -> Result<Vec<u8>, &'static str> {
    options_bytes_in_mode(rows, document_id, false)
}

/// The entry body of `rows`, marked as a whiteboard document when `whiteboard` (lane SMB-BOARD-1).
pub fn options_bytes_in_mode(rows: &[ArtboardOptions], document_id: &str, whiteboard: bool) -> Result<Vec<u8>, &'static str> {
    let mut wire = json!({"schema": ARTBOARD_OPTIONS_SCHEMA, "document_id": document_id, "artboards": rows.iter().map(ArtboardOptions::to_json).collect::<Vec<_>>()});
    if whiteboard {
        wire[WHITEBOARD_MODE_KEY] = Value::Bool(true);
    }
    serde_json::to_vec(&wire).map_err(|_| "artboard_options_invalid")
}

/// Whether the entry body marks a whiteboard document; `artboard_options_invalid` when it does not parse.
pub fn options_whiteboard_mode(bytes: &[u8], document_id: &str) -> Result<bool, &'static str> {
    parse_options(bytes, document_id)?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "artboard_options_invalid")?;
    Ok(value.get(WHITEBOARD_MODE_KEY) == Some(&Value::Bool(true)))
}

/// The entry body of a new whiteboard document: no artboard options, the mode set ([STU-WB-001]).
pub fn whiteboard_options_bytes(document_id: &str) -> Result<Vec<u8>, &'static str> {
    options_bytes_in_mode(&[], document_id, true)
}
