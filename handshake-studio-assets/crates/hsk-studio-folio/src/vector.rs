//! `StudioVectorRecords`: the pattern paint kind (STU-VEC-134 `paint.pattern`, STU-VEC-017,
//! STU-VEC-138, STU-COL-195, STU-COL-196), the vector clip and vector mask records (STU-VEC-053,
//! STU-VEC-153) and the clip/mask reference of a layer. Folio keeps them beside the document, like
//! `StudioRasterRecords` and `StudioDesignRecords`, because adding a variant to `StudioPaint` or a
//! field to `StudioLayer`/`StudioMask` would break the dependents that match `StudioPaint`
//! exhaustively or build those structs by literal (interop-vector matches `StudioPaint` with no
//! wildcard in `svg_export.rs` and `svg_adapter.rs`). One record set annotates one document
//! (`document_id`). Decision record: work-folder `builders/lanes/hsk-studio-folio-eng-7.json`.
//!
//! Members:
//! - `StudioPattern` (`hsk.studio.pattern@1`): `tile_type`, spacing and one of two models. The
//!   `definition` model is the tile-artwork model (STU-COL-195: tile size, brick offset, overlap
//!   order, move-with-art, tile content as the `StudioLayer` subtree rooted at `content_layer_id`).
//!   The `live_source` model tiles another in-document layer (STU-VEC-138: `source_layer_id`,
//!   scaling factor, horizontal alignment) and is a separate model, not a copy of the first.
//! - `StudioPatternPaint`: the `paint.pattern` kind. It binds one row (`row`, an index into
//!   `rows`) of a `PAINT` style of the design records (`style_id`) to a pattern together with the
//!   placement transform that STU-COL-196 puts on the fill. The row's own paint is the stand-in for
//!   a consumer that does not read this set (SVG `url(#p) fallback`), so the row stays a valid
//!   `Solid` or `Gradient` paint. Reordering rows moves the index; the editing code that reorders
//!   rows must update the bindings (rows carry no id of their own and cannot gain one without
//!   breaking literal constructors).
//! - `StudioVectorClip`: a clip whose region is the vector geometry of one layer
//!   (`source_layer_id`, a layer with a `vector` or `group` payload) and a `clip_rule`.
//! - `StudioVectorMask`: a mask of one layer's content with a `mode` (STU-VEC-053 tokens `ALPHA`,
//!   `VECTOR`, `LUMINANCE`), the STU-VEC-153 `mask_clip` and `invert_mask` booleans (default
//!   `false`) and an optional rectangular `region`.
//! - `StudioMaskAttachment`: the reference field of a layer: at most one entry per layer naming a
//!   clip, a mask or both. The consumer applies them; Folio stores and validates only.
//!
//! Units: every `Length` is in the document unit (STU-VEC-138 `document_unit`); the validator
//! refuses another unit as `InvalidUnit`, like artboard bounds. SVG `objectBoundingBox` units are
//! not a Folio mode (the spec has none); the caller converts to document units, as the gradient
//! import does.
//!
//! Reference cycles: a layer depends on its children, on the source layer of each clip or mask
//! attached to it and on the patterns its fills use; a pattern depends on its content or source
//! layer. A cycle in that graph is refused as `RenderCycle` (test
//! `reference_cycles_are_refused` in `tests/vector_records_contract.rs`).
//!
//! Declared gaps, not invented here:
//! - STU-VEC-053 states a mask mode and a "region" but no clip rule, no region geometry and no units:
//!   `clip_rule` reuses the STU-VEC-110 tokens `NONZERO` and `EVENODD` (`NONE` is not a rule and is
//!   unrepresentable), and `region` is one document-unit rectangle, the minimal form SVG
//!   `x/y/width/height` needs. The meaning of `ALPHA`, `VECTOR` and `LUMINANCE` is not specified
//!   beyond the tokens, so the mode is stored only.
//! - The sibling "use-as-mask" marker of STU-VEC-053 (a mask object clips the objects above it in
//!   its container) is not stored; the attachment names each clipped layer explicitly.
//! - Accord defines no identity prefix for `hsk.studio.pattern@1`, nor for clips; `pattern_id`,
//!   `clip_id` and `mask_id` are set-local ids in the existing key grammar (ASCII alphanumerics,
//!   `_`, `-`, at most 128 bytes). `SMSK` belongs to the raster `StudioMask` and is not reused.
//! - `overlap_order` members are not enumerated (STU-COL-195): stored as an opaque label.
//! - STU-COL-195 `grid` and STU-VEC-138 `tile.rectangular` are read as one member (the five-member
//!   union of STU-VEC-138 needs it); Folio spells it `grid`. `brick_by_row` and `brick_by_column`
//!   have no captured interchange token, and their offset parameters are not recovered by the spec.
//! - STU-COL-196 gives every transform field a neutral default of 0.0; a 0 percent scale is not
//!   invertible, so `scale_factor` (percent, STU-VEC-138) is explicit, must be positive, and
//!   `StudioPatternTransform::identity` uses 100. The composition order of the eight parameters is
//!   not stated. `StudioPatternPlacement::Matrix` is a Folio addition for an exact affine
//!   (SVG `patternTransform`), like `StudioGradient::transform`.
//! - Raster tile-bitmap patterns (STU-RAS-161, artifact tier) are a different content kind and are
//!   not stored here.
//! - Bounds the spec marks `unknown` are checked for finiteness and for their physical domain only
//!   (tile size and scaling factor non-negative resp. positive); no bound is invented.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied here (workflow
//! V4-01); counts are charged through the checked `Ctx` budget.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Bounds, Budget, Code, GeometryUnit, Length, NodeKind, Parent, Payload, Result,
    StudioDesignRecords, StudioDocument, StudioLayer, StudioPaintStyle, Unresolved, bounds,
    canceled, err, finite_length, id, key,
    values::{Ctx, label, percent},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(VectorRecordsSchema, "hsk.studio.vector_records@1");

/// Tile layout (STU-COL-195 members; the STU-VEC-138 captured tokens are in
/// [`StudioTileType::capture_token`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTileType {
    Grid,
    BrickByRow,
    BrickByColumn,
    HexByRow,
    HexByColumn,
}
impl StudioTileType {
    /// The STU-VEC-138 captured token, or `None` for the two brick members (interchange value
    /// UNKNOWN; a writer must not invent one).
    pub const fn capture_token(self) -> Option<&'static str> {
        match self {
            Self::Grid => Some("RECTANGULAR"),
            Self::HexByRow => Some("HORIZONTAL_HEXAGONAL"),
            Self::HexByColumn => Some("VERTICAL_HEXAGONAL"),
            Self::BrickByRow | Self::BrickByColumn => None,
        }
    }
    /// The member of a STU-VEC-138 captured token.
    pub fn from_capture_token(token: &str) -> Option<Self> {
        match token {
            "RECTANGULAR" => Some(Self::Grid),
            "HORIZONTAL_HEXAGONAL" => Some(Self::HexByRow),
            "VERTICAL_HEXAGONAL" => Some(Self::HexByColumn),
            _ => None,
        }
    }
}

/// Live-source `horizontal_alignment` (STU-VEC-138: `align.start = START`, `align.center = CENTER`,
/// `align.end = END`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTileAlignment {
    Start,
    Center,
    End,
}
impl StudioTileAlignment {
    /// The STU-VEC-138 captured token.
    pub const fn capture_token(self) -> &'static str {
        match self {
            Self::Start => "START",
            Self::Center => "CENTER",
            Self::End => "END",
        }
    }
}

/// The two pattern models of STU-VEC-138 (tag `pattern_model`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "pattern_model", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioPatternSource {
    /// Tile artwork (STU-COL-195): the tile is the layer subtree rooted at `content_layer_id`.
    Definition {
        /// `SLYR` id of the root layer of the tile artwork.
        content_layer_id: String,
        /// Document unit, hard minimum 0 (STU-COL-195).
        tile_width: Length,
        tile_height: Length,
        /// Percent `0..=100` (STU-COL-195 `pattern.brick_offset`); absent = none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brick_offset: Option<f64>,
        /// Which tile draws on top at an overlap; the members are not enumerated by the spec.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overlap_order: Option<String>,
        move_tile_with_art: bool,
    },
    /// Tiles another in-document layer (STU-VEC-138 live-source pattern).
    LiveSource {
        /// `SLYR` id of the layer being tiled (`source_node_id`).
        source_layer_id: String,
        /// Ratio, positive (`scaling_factor`).
        scaling_factor: f64,
        horizontal_alignment: StudioTileAlignment,
    },
}

/// `StudioPattern` (`hsk.studio.pattern@1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPattern {
    /// Set-local id in the key grammar; unique among the patterns of the set.
    pub pattern_id: String,
    /// Label (the id is authoritative, STU-RAS-161).
    pub name: String,
    pub tile_type: StudioTileType,
    /// Document unit; any sign (the spec states no bound).
    pub horizontal_spacing: Length,
    pub vertical_spacing: Length,
    /// The model and its members.
    pub source: StudioPatternSource,
}

/// The eight placement parameters of STU-VEC-138 / STU-COL-196: angles in degrees, `scale_factor`
/// in percent, `shift_distance` in the document unit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPatternTransform {
    pub rotation: f64,
    /// Percent, positive; `100` is the identity.
    pub scale_factor: f64,
    pub shear_angle: f64,
    pub shear_axis: f64,
    pub shift_angle: f64,
    pub shift_distance: Length,
    pub reflect: bool,
    pub reflect_angle: f64,
}
impl StudioPatternTransform {
    /// The neutral placement, with the shift distance in `unit`.
    pub fn identity(unit: GeometryUnit) -> Self {
        Self {
            rotation: 0.0,
            scale_factor: 100.0,
            shear_angle: 0.0,
            shear_axis: 0.0,
            shift_angle: 0.0,
            shift_distance: Length { value: 0.0, unit },
            reflect: false,
            reflect_angle: 0.0,
        }
    }
}

/// Placement of a pattern fill, independent of the object it fills (STU-COL-196).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "placement_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioPatternPlacement {
    /// The spec parameters.
    Parametric { transform: StudioPatternTransform },
    /// `[a, b, c, d, e, f]`: point = `[a c e; b d f] * [x y 1]`, finite and invertible; maps pattern
    /// space to the filled object's space (Folio addition, see the module note).
    Matrix { matrix: [f64; 6] },
}

/// The `paint.pattern` kind (STU-VEC-134, value 5): row `row` of `PAINT` style `style_id` paints
/// with pattern `pattern_id`. At most one binding per `(style_id, row)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPatternPaint {
    /// `SSTY` id of a paint style of the design records the set is validated with.
    pub style_id: String,
    /// Index into the style's `rows`; the row may be a fill or a stroke.
    pub row: u32,
    pub pattern_id: String,
    pub placement: StudioPatternPlacement,
}

/// Clip rule of a clip region; the STU-VEC-110 tokens (`winding.none` is not a rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioClipRule {
    #[serde(rename = "NONZERO")]
    NonZero,
    #[serde(rename = "EVENODD")]
    EvenOdd,
}

/// A vector clip: the region is the geometry of `source_layer_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVectorClip {
    /// Set-local id in the key grammar.
    pub clip_id: String,
    /// `SLYR` id of the layer whose vector geometry is the clip region (`vector` or `group` payload).
    pub source_layer_id: String,
    pub clip_rule: StudioClipRule,
}

/// STU-VEC-053 mask-mode tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioMaskMode {
    Alpha,
    Vector,
    Luminance,
}

/// A vector mask: the content of `source_layer_id` masks the layers it is attached to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVectorMask {
    /// Set-local id in the key grammar.
    pub mask_id: String,
    /// `SLYR` id of the layer providing the mask content (any payload).
    pub source_layer_id: String,
    pub mode: StudioMaskMode,
    /// STU-VEC-153 `mask_clip`, default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mask_clip: bool,
    /// STU-VEC-153 `invert_mask`, default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert_mask: bool,
    /// Rectangle in the document unit outside which the mask is fully transparent; absent = none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Bounds>,
}
fn is_false(v: &bool) -> bool {
    !*v
}

/// The clip/mask reference of one layer. At least one of `clip_id` and `mask_id` is present.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMaskAttachment {
    /// `SLYR` id of the clipped or masked layer.
    pub layer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_id: Option<String>,
}

/// The vector records of one document. Members are optional; an empty member is not encoded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct StudioVectorRecords {
    pub schema_id: VectorRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<StudioPattern>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pattern_paints: Vec<StudioPatternPaint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<StudioVectorClip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<StudioVectorMask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<StudioMaskAttachment>,
}
impl StudioVectorRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: VectorRecordsSchema::V1,
            document_id: document_id.into(),
            patterns: Vec::new(),
            pattern_paints: Vec::new(),
            clips: Vec::new(),
            masks: Vec::new(),
            attachments: Vec::new(),
        }
    }
    /// The pattern with this id.
    pub fn pattern(&self, pattern_id: &str) -> Option<&StudioPattern> {
        self.patterns.iter().find(|p| p.pattern_id == pattern_id)
    }
    /// The pattern binding of row `row` of style `style_id`.
    pub fn pattern_paint(&self, style_id: &str, row: u32) -> Option<&StudioPatternPaint> {
        self.pattern_paints
            .iter()
            .find(|p| p.style_id == style_id && p.row == row)
    }
    /// The clip with this id.
    pub fn clip(&self, clip_id: &str) -> Option<&StudioVectorClip> {
        self.clips.iter().find(|c| c.clip_id == clip_id)
    }
    /// The mask with this id.
    pub fn mask(&self, mask_id: &str) -> Option<&StudioVectorMask> {
        self.masks.iter().find(|m| m.mask_id == mask_id)
    }
    /// The clip/mask reference of a layer.
    pub fn attachment(&self, layer_id: &str) -> Option<&StudioMaskAttachment> {
        self.attachments.iter().find(|a| a.layer_id == layer_id)
    }
}

/// JSON schema (draft 2020-12) of the vector record set.
pub fn vector_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioVectorRecords>()
}

fn check(ok: bool, code: Code, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(code, format!("{target}/{field}")))
    }
}
fn set_id(s: &str, target: &str, field: &str) -> Result<()> {
    check(key(s), Code::InvalidId, target, field)
}
fn unique<'a>(seen: &mut BTreeSet<&'a str>, v: &'a str, target: &str) -> Result<()> {
    if seen.insert(v) {
        Ok(())
    } else {
        Err(err(Code::DuplicateIdentity, target))
    }
}
/// A `SLYR` id naming a layer of the document.
fn layer<'a>(
    layers: &BTreeMap<&str, &'a StudioLayer>,
    v: &str,
    target: &str,
) -> Result<&'a StudioLayer> {
    id(v, "SLYR", target)?;
    layers
        .get(v)
        .copied()
        .ok_or_else(|| err(Code::DanglingReference, target))
}
fn matrix(m: &[f64; 6]) -> bool {
    let [a, b, c, d, _, _] = *m;
    let det = a * d - b * c;
    m.iter().all(|v| v.is_finite()) && det.is_finite() && det != 0.0
}

fn validate_pattern(
    p: &StudioPattern,
    unit: GeometryUnit,
    layers: &BTreeMap<&str, &StudioLayer>,
) -> Result<()> {
    let t = p.pattern_id.as_str();
    set_id(t, t, "pattern_id")?;
    check(label(&p.name), Code::InvalidField, t, "name")?;
    finite_length(&p.horizontal_spacing, unit, false, t)?;
    finite_length(&p.vertical_spacing, unit, false, t)?;
    match &p.source {
        StudioPatternSource::Definition {
            content_layer_id,
            tile_width,
            tile_height,
            brick_offset,
            overlap_order,
            ..
        } => {
            layer(layers, content_layer_id, &format!("{t}/content_layer_id"))?;
            finite_length(tile_width, unit, true, t)?;
            finite_length(tile_height, unit, true, t)?;
            if let Some(o) = brick_offset {
                check(percent(*o), Code::InvalidField, t, "brick_offset")?;
            }
            if let Some(o) = overlap_order {
                check(label(o), Code::InvalidField, t, "overlap_order")?;
            }
        }
        StudioPatternSource::LiveSource {
            source_layer_id,
            scaling_factor,
            ..
        } => {
            layer(layers, source_layer_id, &format!("{t}/source_layer_id"))?;
            check(
                scaling_factor.is_finite() && *scaling_factor > 0.0,
                Code::InvalidField,
                t,
                "scaling_factor",
            )?;
        }
    }
    Ok(())
}

fn validate_placement(p: &StudioPatternPlacement, unit: GeometryUnit, target: &str) -> Result<()> {
    match p {
        StudioPatternPlacement::Parametric { transform: t } => {
            check(
                [
                    t.rotation,
                    t.shear_angle,
                    t.shear_axis,
                    t.shift_angle,
                    t.reflect_angle,
                ]
                .iter()
                .all(|v| v.is_finite()),
                Code::InvalidField,
                target,
                "placement/angles",
            )?;
            check(
                t.scale_factor.is_finite() && t.scale_factor > 0.0,
                Code::InvalidField,
                target,
                "placement/scale_factor",
            )?;
            finite_length(&t.shift_distance, unit, false, target)
        }
        StudioPatternPlacement::Matrix { matrix: m } => {
            check(matrix(m), Code::InvalidField, target, "placement/matrix")
        }
    }
}

/// Node of the reference graph (layer, style and pattern ids are separate namespaces). A style is
/// a node of its own so that a style used by many layers and bound to many patterns adds the sum,
/// not the product, of the two edge counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Node<'a> {
    Layer(&'a str),
    Style(&'a str),
    Pattern(&'a str),
}
impl Node<'_> {
    fn text(self) -> String {
        match self {
            Self::Layer(v) | Self::Style(v) | Self::Pattern(v) => v.to_owned(),
        }
    }
}

/// Refuses a cycle in the reference graph described in the module note. Iterative depth-first
/// search, so a hostile chain cannot exhaust the stack.
fn check_cycles(
    r: &StudioVectorRecords,
    d: &StudioDocument,
    design: Option<&StudioDesignRecords>,
    cx: &Ctx,
) -> Result<()> {
    let mut edges: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
    for l in &d.layers {
        cx.tick()?;
        if let Parent::Node(n) = &l.parent
            && n.node_kind == NodeKind::Layer
        {
            edges
                .entry(Node::Layer(&n.node_id))
                .or_default()
                .push(Node::Layer(&l.layer_id));
        }
    }
    let clip_source: BTreeMap<&str, &str> = r
        .clips
        .iter()
        .map(|c| (c.clip_id.as_str(), c.source_layer_id.as_str()))
        .collect();
    let mask_source: BTreeMap<&str, &str> = r
        .masks
        .iter()
        .map(|m| (m.mask_id.as_str(), m.source_layer_id.as_str()))
        .collect();
    for a in &r.attachments {
        cx.tick()?;
        let sources = [
            a.clip_id
                .as_deref()
                .and_then(|c| clip_source.get(c).copied()),
            a.mask_id
                .as_deref()
                .and_then(|m| mask_source.get(m).copied()),
        ];
        for source in sources.into_iter().flatten() {
            edges
                .entry(Node::Layer(&a.layer_id))
                .or_default()
                .push(Node::Layer(source));
        }
    }
    for p in &r.patterns {
        cx.tick()?;
        let content = match &p.source {
            StudioPatternSource::Definition {
                content_layer_id, ..
            } => content_layer_id,
            StudioPatternSource::LiveSource {
                source_layer_id, ..
            } => source_layer_id,
        };
        edges
            .entry(Node::Pattern(&p.pattern_id))
            .or_default()
            .push(Node::Layer(content));
    }
    if let Some(design) = design {
        for pp in &r.pattern_paints {
            cx.tick()?;
            edges
                .entry(Node::Style(&pp.style_id))
                .or_default()
                .push(Node::Pattern(&pp.pattern_id));
        }
        for n in &design.nodes {
            cx.tick()?;
            for style in n.style_refs.values() {
                edges
                    .entry(Node::Layer(&n.node.node_id))
                    .or_default()
                    .push(Node::Style(style));
            }
        }
    }
    let mut state: BTreeMap<Node, u8> = BTreeMap::new();
    for &start in edges.keys() {
        if state.contains_key(&start) {
            continue;
        }
        state.insert(start, 1);
        let mut stack: Vec<(Node, usize)> = vec![(start, 0)];
        while let Some(&(node, next)) = stack.last() {
            cx.tick()?;
            let targets = edges.get(&node).map_or(&[][..], Vec::as_slice);
            if let Some(&target) = targets.get(next) {
                if let Some(top) = stack.last_mut() {
                    top.1 = top.1.saturating_add(1);
                }
                match state.get(&target) {
                    Some(1) => return Err(err(Code::RenderCycle, target.text())),
                    Some(_) => {}
                    None => {
                        state.insert(target, 1);
                        stack.push((target, 0));
                    }
                }
            } else {
                state.insert(node, 2);
                stack.pop();
            }
        }
    }
    Ok(())
}

/// The single vector-record validator: ids, document binding, document-unit lengths, spec ranges,
/// references to layers, patterns, clips, masks and `PAINT` style rows, uniqueness, and reference
/// cycles. `design` is the design record set of the same document; a pattern binding needs it
/// (its `style_id` must name one of its paint styles and `row` one of that style's rows). Records
/// are charged against `Budget::nodes`.
pub fn validate_vector_records(
    r: &StudioVectorRecords,
    d: &StudioDocument,
    design: Option<&StudioDesignRecords>,
    b: Budget,
    token: &CancellationToken,
) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if let Some(design) = design
        && design.document_id != d.document_id
    {
        return Err(err(Code::InvalidField, "design/document_id"));
    }
    let unit = d.geometry_unit;
    let layers: BTreeMap<&str, &StudioLayer> =
        d.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();

    cx.nodes(r.patterns.len(), "patterns")?;
    let mut patterns = BTreeSet::new();
    for p in &r.patterns {
        cx.tick()?;
        validate_pattern(p, unit, &layers)?;
        unique(&mut patterns, &p.pattern_id, &p.pattern_id)?;
    }

    cx.nodes(r.clips.len(), "clips")?;
    let mut clips = BTreeSet::new();
    for c in &r.clips {
        cx.tick()?;
        let t = c.clip_id.as_str();
        set_id(t, t, "clip_id")?;
        unique(&mut clips, t, t)?;
        let source = layer(&layers, &c.source_layer_id, &format!("{t}/source_layer_id"))?;
        check(
            matches!(source.payload, Payload::Vector { .. } | Payload::Group),
            Code::PayloadMismatch,
            t,
            "source_layer_id",
        )?;
    }

    cx.nodes(r.masks.len(), "masks")?;
    let mut masks = BTreeSet::new();
    for m in &r.masks {
        cx.tick()?;
        let t = m.mask_id.as_str();
        set_id(t, t, "mask_id")?;
        unique(&mut masks, t, t)?;
        layer(&layers, &m.source_layer_id, &format!("{t}/source_layer_id"))?;
        if let Some(region) = &m.region {
            bounds(region, unit, t)?;
        }
    }

    cx.nodes(r.attachments.len(), "attachments")?;
    let mut attached = BTreeSet::new();
    for a in &r.attachments {
        cx.tick()?;
        let t = a.layer_id.as_str();
        layer(&layers, t, t)?;
        unique(&mut attached, t, t)?;
        check(
            a.clip_id.is_some() || a.mask_id.is_some(),
            Code::InvalidField,
            t,
            "clip_id_or_mask_id",
        )?;
        if let Some(c) = &a.clip_id {
            check(
                clips.contains(c.as_str()),
                Code::DanglingReference,
                t,
                "clip_id",
            )?;
        }
        if let Some(m) = &a.mask_id {
            check(
                masks.contains(m.as_str()),
                Code::DanglingReference,
                t,
                "mask_id",
            )?;
        }
    }

    cx.nodes(r.pattern_paints.len(), "pattern_paints")?;
    let styles: BTreeMap<&str, &StudioPaintStyle> = design
        .map(|x| {
            x.paint_styles
                .iter()
                .map(|s| (s.style_id.as_str(), s))
                .collect()
        })
        .unwrap_or_default();
    let mut bound: BTreeSet<(&str, u32)> = BTreeSet::new();
    for pp in &r.pattern_paints {
        cx.tick()?;
        let t = format!("{}/rows/{}", pp.style_id, pp.row);
        id(&pp.style_id, "SSTY", &t)?;
        let style = styles
            .get(pp.style_id.as_str())
            .ok_or_else(|| err(Code::DanglingReference, &t))?;
        let in_range = usize::try_from(pp.row).is_ok_and(|row| row < style.rows.len());
        check(in_range, Code::DanglingReference, &t, "row")?;
        if !bound.insert((pp.style_id.as_str(), pp.row)) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        set_id(&pp.pattern_id, &t, "pattern_id")?;
        check(
            patterns.contains(pp.pattern_id.as_str()),
            Code::DanglingReference,
            &t,
            "pattern_id",
        )?;
        validate_placement(&pp.placement, unit, &t)?;
    }

    check_cycles(r, d, design, &cx)?;
    canceled(token)
}
