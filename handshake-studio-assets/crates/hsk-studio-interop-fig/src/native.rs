//! Mapping of a decoded [`FigDocument`] into the Studio native document (Folio): pages, artboards,
//! layer tree, layer appearance, paint styles (fills and strokes), inline text stories, components,
//! component sets, instances and overrides (instance layers through `hsk-studio-components`),
//! auto layout, prototype reactions, variable collections and variables, plus the node design
//! records that bind them. Only Folio's own types are produced (CX-CAP-001); anything without a
//! Folio home is a typed loss entry, never dropped silently (tests:
//! fig_native::real_native_losses_accounted, real_native_components_instances_overrides,
//! real_native_auto_layout_reactions_variables).
//!
//! Identity: every Folio id comes from the caller's [`IdMint`]; the Figma source id of every mapped
//! node is kept in [`NativeProposal::id_map`] (document nodes) or [`NativeProposal::record_map`]
//! (variables) and in the records import entry, so a saved and reopened package still names the
//! source node of every layer and record (test: fig_native::real_native_package_round_trip).
//!
//! Geometry (see `geometry.rs`): shape nodes become `vector` layers listing `hsk.studio.vector_path@1`
//! ids; the paths are Nib `OwnedPath`s in document px with the full node transform chain baked in
//! (Folio layers carry no transform), taken from the decoded fill geometry blobs (rectangle radii,
//! ellipse arcs, stars and polygons are already resolved there), else the vector network, else the
//! LINE segment. They are persisted in the records entry as `vector_paths`.
//!
//! Frames (lane SMB-INTEROP-FIG-INSTANCES-FRAMES): a frame fill is a background `vector` layer, the
//! frame's bottom-most child, and frame clipping a vector clip record on the frame's child layers;
//! component instances draw through an expansion group of the definition's children with the
//! overrides mapped onto them (bounded: [`MAX_INSTANCE_DEPTH`], [`MAX_OVERRIDE_PATH`],
//! [`MAX_EXPANDED_NODES`]; test fig_instances_frames::hostile_instances_refused_before_allocation);
//! text layers record their lines and runs from Figma's stored layout ([`FigTextLayout`]).
//!
//! Not mapped (typed losses): geometry of containers without fill or clip, the
//! boolean result outline, stroke outlines (derived from the path plus the stroke row), transforms
//! and sizes of nodes without emitted geometry, image paints, effects, masks, text fonts and run
//! styles, paint-to-variable bindings, flow starts, scroll behaviour. Editable import stays
//! NOT_PROVEN (`FigImport::editable`).
#![deny(clippy::arithmetic_side_effects)]
use crate::container::hex;
use crate::effects::effect;
use crate::geometry::{
    Affine, IDENTITY, compose, fill_rule, line_path, network_paths, path_from_commands, rect_path,
};
use crate::import::{FigImport, SourceId};
use crate::nodes::VariableValue;
use crate::nodes::{
    DerivedSymbolNode, FigDocument, FigNode, FramePayload, InstanceOverride, NodeLoss,
    NodePayload, Paint, PaintKind, TextPayload, TextRun,
};
use crate::semantic_decode::EditableStatus;
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::instance_layer;
use hsk_studio_folio::{
    ArtboardSchema, BLEND_NORMAL, BLEND_PASS_THROUGH, Bounds, Budget, DocumentSchema, GeometryUnit,
    GradientInterpolation, GradientSpread, Isolation, LayerAppearance, LayerSchema,
    Length, NodeKind, NodeRef, Parent, Payload, ProfileBinding, Resolver, Snapshot, StrokeAlign,
    StrokeCap, StrokeJoin, StudioAppearanceRow, StudioArtboard, StudioColor, StudioDesignRecords,
    StudioDocument, StudioGradient, StudioGradientGeometry, StudioGradientStop, StudioLayer,
    StudioNodeDesign, StudioPaint, StudioPaintStyle, StudioPublishing,
    StudioStrokeStyle, StyleKind, StyleRegistrySchema, validate_design_records, validate_document,
};
use hsk_studio_folio::{
    AutoLayoutSchema, AxisSizingMode, CounterAxisAlign, CounterAxisAlignContent, CubicBezier,
    LayoutMode, LayoutWrap, MAX_DURATION_MS, Navigation, PADDING_MAX, PrimaryAxisAlign,
    SPACING_MAX, SPACING_MIN, StudioAction, StudioAutoLayout, StudioEasing, StudioMode,
    StudioReaction, StudioRgba, StudioTransition, StudioTrigger, StudioVariable,
    StudioVariableCollection, TransitionDirection, TransitionType, VARIABLE_SCOPES, VariableCollectionSchema,
    VariableSchema, VariableType,
};
use hsk_studio_folio::{
    ContentDigest, StudioBooleanGroup, StudioBooleanOperation, StudioImagePaint, StudioImageScale,
    StudioImageSource, StudioLetterSpacing, StudioLineHeight, StudioTextAlign, StudioTypeStyle,
    StudioTypeStyleRecords, TypeStyleResolver, TypeStyleSchema, TypeStyleScope,
    check_story_style_scopes, validate_type_style_records,
};
use hsk_studio_folio::{
    StudioFlowStart, StudioFrameOverflow, StudioOverflowDirection, StudioPrototypePage, easing_preset,
};
use hsk_studio_folio::{
    OverlayPosition, StudioFrameOverlay, StudioOverlayBackground, StudioOverlayBackgroundInteraction,
    StudioOverlayPositionType,
};
use hsk_studio_folio::{
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioTextStory,
    TextStorySchema, validate_text_story,
};
use hsk_studio_folio::{
    StudioComponent, StudioComponentInstance, StudioComponentSet, StudioOverrideRecord, StudioValue,
};
use hsk_studio_folio::{
    StudioClipRule, StudioEffectStack, StudioMaskAttachment, StudioMaskMode,
    StudioRasterLayerRecord, StudioVectorClip,
    StudioRasterRecords, StudioVectorMask, StudioVectorRecords, validate_raster_records,
    validate_vector_records,
};
use hsk_studio_nib::{OwnedPath, PathLimits};
use hsk_studio_package::{ImportEntry, ImportRole};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// Identity minting is caller-owned: the mapper never invents ids on its own.
pub trait IdMint {
    /// A fresh id with the given Studio prefix (`SDOC`, `SART`, `SLYR`, `SSTY`, ...), in the
    /// `PREFIX-uuid_v7` shape Accord validates.
    fn mint(&mut self, prefix: &'static str) -> String;
}

/// Deterministic synthetic ids for standalone use and tests: the same namespace and call order
/// always yield the same ids. A host rewrites them on adoption (STU-IO-190).
#[derive(Clone, Debug)]
pub struct SequentialIdMint {
    namespace: u32,
    next: u64,
}

impl SequentialIdMint {
    pub fn new(namespace: u32) -> Self {
        Self { namespace, next: 1 }
    }
}

impl IdMint for SequentialIdMint {
    fn mint(&mut self, prefix: &'static str) -> String {
        let n = self.next & 0xFFFF_FFFF_FFFF;
        self.next = self.next.wrapping_add(1);
        format!("{prefix}-{:08x}-0000-7000-8000-{n:012x}", self.namespace)
    }
}

/// Caller inputs of the mapping: the Folio budget the document must fit and the host resolver
/// used for validation (it must accept the layer bindings the document uses).
pub struct NativeConfig<'a> {
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
}

/// One Figma node mapped to a design record (`SVCL`/`SVAR` id) instead of a document node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordMapEntry {
    pub source: SourceId,
    pub record_id: String,
}

/// One `hsk.studio.vector_path@1` path of a vector layer: geometry in document px (every node
/// transform of the Figma tree is baked in, Folio layers carry none), as Nib's owned path. The
/// layer lists `path_id` in its `Payload::Vector`; the geometry itself is persisted in the
/// package records entry (`vector_paths`), like the vector proposal of `hsk-studio-interop-vector`.
#[derive(Clone, Debug, PartialEq)]
pub struct FigVectorPath {
    /// `SVPT` id.
    pub path_id: String,
    /// `SLYR` id of the layer that lists this path.
    pub layer_id: String,
    pub geometry: OwnedPath,
}

/// Font and layout values of one `STYS` id while mapping (`character` or `paragraph`); converted
/// into a Folio `StudioTypeStyle` once the set is built.
#[derive(Clone, Debug, PartialEq)]
struct FigTextStyle {
    style_ref: String,
    kind: &'static str,
    font_family: Option<String>,
    font_style: Option<String>,
    font_size: Option<f64>,
    /// First visible solid fill (r, g, b, a in 0..=1).
    fill: Option<[f64; 4]>,
    line_height: Option<(f64, String)>,
    letter_spacing: Option<(f64, String)>,
    align_horizontal: Option<String>,
}

impl FigTextStyle {
    fn key(&self) -> String {
        format!("{self:?}")
    }

    /// Folio type style; the `bool` is true when a line height or spacing unit had no mapping.
    fn to_folio(&self, ordinal: usize) -> (StudioTypeStyle, bool) {
        let mut unit_lost = false;
        let length = |v: f64| Length {
            value: v,
            unit: GeometryUnit::Px,
        };
        let line_height = self
            .line_height
            .as_ref()
            .and_then(|(v, u)| match u.as_str() {
                "PERCENT" => Some(StudioLineHeight::Percent { value: *v }),
                "PIXELS" => Some(StudioLineHeight::Length { length: length(*v) }),
                _ => {
                    unit_lost = true;
                    None
                }
            });
        let letter_spacing = self
            .letter_spacing
            .as_ref()
            .and_then(|(v, u)| match u.as_str() {
                "PERCENT" => Some(StudioLetterSpacing::Percent { value: *v }),
                "PIXELS" => Some(StudioLetterSpacing::Length { length: length(*v) }),
                _ => {
                    unit_lost = true;
                    None
                }
            });
        let paragraph = self.kind == "paragraph";
        let style = StudioTypeStyle {
            schema_id: TypeStyleSchema::V1,
            style_id: self.style_ref.clone(),
            scope: if paragraph {
                TypeStyleScope::Paragraph
            } else {
                TypeStyleScope::Character
            },
            name: format!("fig/{}/{ordinal}", self.kind),
            based_on: None,
            next_style: None,
            font_family: self.font_family.clone(),
            font_style: self.font_style.clone(),
            font_size: self.font_size.map(length),
            fill: self.fill.map(|[r, g, b, a]| StudioRgba {
                r,
                g,
                b,
                a,
                colour_profile_id: ProfileBinding::Unassigned(()),
            }),
            line_height,
            letter_spacing,
            align: self
                .align_horizontal
                .as_deref()
                .and_then(|a| match a {
                    "LEFT" => Some(StudioTextAlign::Left),
                    "CENTER" => Some(StudioTextAlign::Center),
                    "RIGHT" => Some(StudioTextAlign::Right),
                    "JUSTIFIED" => Some(StudioTextAlign::Justified),
                    _ => None,
                })
                .filter(|_| paragraph),
        };
        (style, unit_lost)
    }
}

/// One mapped node: Figma source id -> Folio node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdMapEntry {
    pub source: SourceId,
    pub node: NodeRef,
}

/// Result of mapping: a validated Folio snapshot, the design records validated against it, the
/// source-id map and the combined loss list (decode losses of the [`FigDocument`] plus mapping
/// losses). `editable` stays NOT_PROVEN.
#[derive(Clone, Debug)]
pub struct NativeProposal {
    pub snapshot: Snapshot,
    pub design: StudioDesignRecords,
    /// One inline text story per TEXT node (`hsk.studio.text_story@1`), validated.
    pub stories: Vec<StudioTextStory>,
    /// Vector geometry of the vector layers, in document px (transforms baked in).
    pub paths: Vec<FigVectorPath>,
    /// Type styles behind the `STYS` bindings of the stories (`hsk.studio.type_style_records@1`),
    /// validated; the stories resolve against them through `TypeStyleResolver`.
    pub type_styles: StudioTypeStyleRecords,
    /// `STYS` bindings still unresolved after the type style set (expected empty).
    pub unresolved_text_styles: Vec<String>,
    /// Effects (shadows, blurs) per layer (`hsk.studio.raster_records@1`), validated.
    pub raster_records: StudioRasterRecords,
    /// Masks, attachments, image paints and boolean groups (`hsk.studio.vector_records@1`),
    /// validated.
    pub vector_records: StudioVectorRecords,
    pub id_map: Vec<IdMapEntry>,
    /// Figma nodes that became design records rather than document nodes (variables).
    pub record_map: Vec<RecordMapEntry>,
    /// Mapping losses only (`native.*` paths), aggregated like [`NodeLoss`].
    pub mapping_loss: Vec<NodeLoss>,
    /// The decode losses of the source [`FigDocument`].
    pub decode_loss: Vec<NodeLoss>,
    pub editable: EditableStatus,
    /// Frame fills drawn as background layers, with the frame clips.
    pub frame_backgrounds: Vec<FrameBackground>,
    /// Instances drawn through an expansion group.
    pub expansions: Vec<InstanceExpansion>,
    /// Nodes created by instance expansion, with their source guid paths.
    pub expanded: Vec<ExpandedNode>,
    /// Lines and runs of every text layer (font resolution: `resolve_text_outputs`).
    pub text_layouts: Vec<FigTextLayout>,
}

/// Figma blend mode name -> Folio `StudioBlendMode` discriminant (`hsk-studio-pigment`
/// `blend_math::StudioBlendMode`, values read 2026-10-14). `None`: no counterpart.
pub(crate) fn blend(name: &str) -> Option<u16> {
    Some(match name {
        "PASS_THROUGH" => 1,
        "NORMAL" => 2,
        "DARKEN" => 4,
        "MULTIPLY" => 5,
        "COLOR_BURN" => 6,
        "LINEAR_BURN" => 7,
        "LIGHTEN" => 8,
        "SCREEN" => 9,
        "COLOR_DODGE" => 10,
        "LINEAR_DODGE" => 11,
        "OVERLAY" => 12,
        "SOFT_LIGHT" => 13,
        "HARD_LIGHT" => 14,
        "DIFFERENCE" => 18,
        "EXCLUSION" => 19,
        "HUE" => 20,
        "SATURATION" => 21,
        "COLOR" => 22,
        "LUMINOSITY" => 23,
        _ => return None,
    })
}

const LEAF_KINDS: &[&str] = &[
    "RECTANGLE",
    "ROUNDED_RECTANGLE",
    "ELLIPSE",
    "STAR",
    "REGULAR_POLYGON",
    "LINE",
    "VECTOR",
];
const CONTAINER_KINDS: &[&str] = &["FRAME", "SECTION", "GROUP", "BOOLEAN_OPERATION", "SYMBOL"];
/// Folio has no layer for these: they become design records (variables) in a later slice.
const NON_LAYER_KINDS: &[&str] = &["VARIABLE", "VARIABLE_SET"];

/// Figma glyph outline space: em units with y up from the baseline (document y is down).
const GLYPH_Y: f64 = -1.0;

/// Instance expansion bound: instances nested inside expanded instances (deeper = refused).
pub const MAX_INSTANCE_DEPTH: usize = 16;
/// Instance expansion bound: guid-path length of one override (longer = refused).
pub const MAX_OVERRIDE_PATH: usize = 16;
/// Instance expansion bound: nodes all expansions of one document may create; an instance whose
/// expansion would pass it is refused before any of its layers is created.
pub const MAX_EXPANDED_NODES: u64 = 20_000;

/// One frame fill drawn as a background layer (bottom-most child of its frame), and the frame's
/// clip when its content is clipped (`frameMaskDisabled = false`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameBackground {
    /// The frame's Figma node (inside an instance expansion: the definition node).
    pub source: SourceId,
    /// The frame's Folio node (artboard, group layer or expansion group).
    pub frame: NodeRef,
    /// `vector` layer holding the frame geometry; `painted` = it carries the frame fills,
    /// otherwise it is a hidden clip-source layer.
    pub layer_id: String,
    pub painted: bool,
    /// Set-local clip id in the vector records when the frame clips its content.
    pub clip_id: Option<String>,
}

/// One node created by instance expansion: `path` is the guid path from the outermost document
/// INSTANCE (`instance`) through nested instances to the definition node (Figma's own
/// `I<instance>;<node>` identity); the definition node keeps its own id-map entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedNode {
    pub instance: SourceId,
    pub path: Vec<SourceId>,
    pub node: NodeRef,
}

/// One instance drawn through its expansion: the `component_instance` binding layer (absent for
/// instances nested in an expansion) and the group layer holding the expanded content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceExpansion {
    pub instance: SourceId,
    pub path: Vec<SourceId>,
    pub binding_layer_id: Option<String>,
    pub group_layer_id: String,
}

/// One laid-out line of a text output: byte range of the story and the baseline origin (pen
/// start) in document px, taken from Figma's stored layout (`derivedTextData.baselines`).
#[derive(Clone, Debug, PartialEq)]
pub struct FigTextLine {
    pub start: u64,
    pub end: u64,
    pub baseline_px: [f64; 2],
}

/// One style run of a text output: byte range, the requested font (Figma family and style),
/// size in px (Figma's laid-out glyph size where stored) and solid fill (straight RGBA 0..=1).
#[derive(Clone, Debug, PartialEq)]
pub struct FigTextRunSpec {
    pub start: u64,
    pub end: u64,
    pub family: Option<String>,
    pub style: Option<String>,
    pub size_px: Option<f64>,
    pub fill_rgba: Option<[f64; 4]>,
}

/// One glyph Figma laid out, as its outline in document px (`path` is `None` for an empty glyph,
/// a space, or an outline that could not be placed). `start` = byte offset of its character.
#[derive(Clone, Debug, PartialEq)]
pub struct FigGlyphOutline {
    pub start: u64,
    pub path: Option<OwnedPath>,
}

/// What a text layer needs to be drawn: its lines and runs. Fonts are resolved later, against
/// caller-granted fonts (`resolve_text_outputs`). `glyphs` are Figma's own laid-out glyph outlines
/// (from `derivedTextData`), used where a font-free exact drawing is wanted (PDF export).
#[derive(Clone, Debug, PartialEq)]
pub struct FigTextLayout {
    pub layer_id: String,
    pub story_id: String,
    pub source: SourceId,
    pub lines: Vec<FigTextLine>,
    pub runs: Vec<FigTextRunSpec>,
    pub glyphs: Vec<FigGlyphOutline>,
}

/// The active instance expansion: overrides and derived values keyed by guid path relative to
/// the symbol being expanded (`bool` = matched).
struct Expansion {
    root: SourceId,
    prefix: Vec<SourceId>,
    symbol: SourceId,
    chain: Vec<SourceId>,
    overrides: Vec<(Vec<SourceId>, InstanceOverride, bool)>,
    derived: Vec<(Vec<SourceId>, DerivedSymbolNode, bool)>,
}

struct Mapper<'a> {
    doc: &'a FigDocument,
    mint: &'a mut dyn IdMint,
    cancel: &'a CancellationToken,
    /// Every emitted node in paint order (parents first, siblings bottom-most first).
    order: Vec<NodeRef>,
    symbol_index: HashMap<SourceId, usize>,
    exp: Vec<Expansion>,
    /// Nodes all expansions may create: [`MAX_EXPANDED_NODES`], capped by the caller's Folio
    /// node budget left after the document's own nodes (two per node).
    expansion_budget: u64,
    expanded_total: u64,
    expanded: Vec<ExpandedNode>,
    expansions: Vec<InstanceExpansion>,
    backgrounds: Vec<FrameBackground>,
    /// clip id, source layer, clipped layers.
    clips: Vec<(String, String, Vec<String>)>,
    text_layouts: Vec<FigTextLayout>,
    /// The page (CANVAS root layer) being walked.
    page: Option<NodeRef>,
    /// Flow starting points: page, frame node id, name, stored position.
    flow_starts: Vec<(NodeRef, String, String, Option<String>)>,
    frame_overflow: Vec<StudioFrameOverflow>,
    /// STU-PRO-122 overlay fields of frames (lane SMB-FIG-SMART-ANIMATE).
    frame_overlay: Vec<StudioFrameOverlay>,
    layers: Vec<StudioLayer>,
    stories: Vec<StudioTextStory>,
    paths: Vec<FigVectorPath>,
    fx: Vec<StudioRasterLayerRecord>,
    image_paint_records: Vec<StudioImagePaint>,
    boolean_groups: Vec<StudioBooleanGroup>,
    /// Image paint produced by the last `paint` call, bound to its row by `style`.
    pending_image: Option<StudioImagePaint>,
    text_styles: Vec<FigTextStyle>,
    /// Canonical style JSON (without the id) -> minted `STYS` id.
    text_style_ids: HashMap<String, String>,
    /// Mask source layer id and the layer ids it masks (later siblings in the same parent).
    mask_plans: Vec<(String, Vec<String>)>,
    artboards: Vec<StudioArtboard>,
    styles: Vec<StudioPaintStyle>,
    designs: Vec<StudioNodeDesign>,
    id_map: Vec<IdMapEntry>,
    loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>,
    current: Option<SourceId>,
    /// SYMBOL nodes (node index, group layer id), state-group FRAMEs (node index, layer id) and
    /// INSTANCE nodes (node index, SCIN id), resolved into component records after the walk.
    symbols: Vec<(usize, String)>,
    sets: Vec<(usize, String)>,
    instances: Vec<(usize, String)>,
    components: Vec<StudioComponent>,
    component_sets: Vec<StudioComponentSet>,
    component_instances: Vec<StudioComponentInstance>,
    collections: Vec<StudioVariableCollection>,
    variables: Vec<StudioVariable>,
    record_map: Vec<RecordMapEntry>,
}

fn px(value: f64) -> Length {
    Length {
        value,
        unit: GeometryUnit::Px,
    }
}

/// f32 source value as the f64 of its shortest decimal spelling (0.96f32 -> 0.96, not
/// 0.9599999785423279): the authored value, and one that survives the package's JSON text without
/// drift (17-digit values do not round-trip through the workspace serde_json, which is built
/// without `float_roundtrip`; test fig_native::real_native_package_round_trip).
pub(crate) fn wide(v: f32) -> f64 {
    v.to_string().parse().unwrap_or_else(|_| f64::from(v))
}

/// A computed f64 kept to 12 significant digits, for the same JSON round-trip reason as [`wide`].
pub(crate) fn tidy(v: f64) -> f64 {
    format!("{v:.11e}").parse().unwrap_or(v)
}

fn colour(c: &crate::nodes::Color) -> StudioColor {
    StudioColor {
        r: wide(c.r).clamp(0.0, 1.0),
        g: wide(c.g).clamp(0.0, 1.0),
        b: wide(c.b).clamp(0.0, 1.0),
        colour_profile_id: ProfileBinding::Unassigned(()),
    }
}

fn percent(v: f32) -> f64 {
    (wide(v) * 100.0).clamp(0.0, 100.0)
}

/// Inverse of a 2x3 affine matrix stored as `[m00 m01 m02; m10 m11 m12]`.
fn invert(m: &crate::nodes::Matrix) -> Option<[f64; 6]> {
    let (a, c, e) = (wide(m.m00), wide(m.m01), wide(m.m02));
    let (b, d, f) = (wide(m.m10), wide(m.m11), wide(m.m12));
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    let ie = -(ia * e + ic * f);
    let if_ = -(ib * e + id * f);
    Some([ia, ib, ic, id, ie, if_])
}

/// `.fig` directional transition token -> STU-PRO-118 type and direction (the edge the incoming
/// screen enters from for `*_FROM_*`, the edge the outgoing screen leaves toward for `*_OUT_TO_*`).
pub(crate) fn directional_transition(token: &str) -> Option<(TransitionType, TransitionDirection)> {
    let (ty, side) = [
        ("MOVE_FROM_", TransitionType::MoveIn),
        ("PUSH_FROM_", TransitionType::Push),
        ("SLIDE_FROM_", TransitionType::SlideIn),
        ("MOVE_OUT_TO_", TransitionType::MoveOut),
        ("SLIDE_OUT_TO_", TransitionType::SlideOut),
    ]
    .into_iter()
    .find_map(|(prefix, ty)| token.strip_prefix(prefix).map(|side| (ty, side)))?;
    let dir = match side {
        "LEFT" => TransitionDirection::Left,
        "RIGHT" => TransitionDirection::Right,
        "TOP" => TransitionDirection::Top,
        "BOTTOM" => TransitionDirection::Bottom,
        _ => return None,
    };
    Some((ty, dir))
}

impl Mapper<'_> {
    /// The vector paths of a shape node: its decoded fill geometry (rectangle radii, ellipse arcs,
    /// stars, polygons, vectors are already resolved in those blobs), else the vector network
    /// (VECTOR) or the segment from its size (LINE). Records the paths and returns their `SVPT`
    /// ids; empty when nothing could be emitted.
    fn geometry(&mut self, n: &FigNode, abs: &Affine, layer_id: &str) -> Vec<String> {
        let mut built: Vec<OwnedPath> = Vec::new();
        for g in &n.fill_geometry {
            let Some(cmds) = &g.commands else { continue };
            let Some(rule) = fill_rule(g.winding_rule.as_deref()) else {
                self.lose("native.geometry", "winding_rule_unknown");
                continue;
            };
            match path_from_commands(cmds, abs, rule) {
                Some(p) => built.push(p),
                None => self.lose("native.geometry", "geometry_rejected_by_nib"),
            }
        }
        if built.is_empty() && n.fill_geometry.is_empty() {
            match &n.payload {
                NodePayload::Vector {
                    network: Some(net),
                    normalized_size,
                } => {
                    let scale = match (n.size, normalized_size) {
                        (Some(s), Some(z)) if z.x > 0.0 && z.y > 0.0 => {
                            (wide(s.x) / wide(z.x), wide(s.y) / wide(z.y))
                        }
                        _ => (1.0, 1.0),
                    };
                    built.extend(network_paths(net, scale, abs));
                }
                _ if n.type_name == "LINE" => {
                    built.extend(line_path(abs, n.size.map_or(0.0, |s| wide(s.x))));
                }
                _ => {}
            }
        }
        let mut ids = Vec::new();
        for geometry in built {
            let path_id = self.mint.mint("SVPT");
            ids.push(path_id.clone());
            self.paths.push(FigVectorPath {
                path_id,
                layer_id: layer_id.to_owned(),
                geometry,
            });
        }
        ids
    }

    /// A mapping loss. Inside an instance expansion the path is prefixed `expansion:`: the same
    /// definition data is mapped (and its losses counted) once on the definition itself, so the
    /// expansion copies stay distinguishable from it.
    fn lose(&mut self, path: impl Into<String>, code: &'static str) {
        let path = if self.exp.is_empty() {
            path.into()
        } else {
            format!("expansion:{}", path.into())
        };
        self.lose_plain(path, code);
    }

    /// A loss under its own path (expansion-specific losses).
    fn lose_plain(&mut self, path: impl Into<String>, code: &'static str) {
        let first = self.current;
        let slot = self.loss.entry((path.into(), code)).or_insert((0, first));
        slot.0 = slot.0.saturating_add(1);
    }

    fn check(&self) -> Result<()> {
        self.cancel.check().map_err(|_| FigError::Canceled)
    }

    fn appearance(&mut self, n: &FigNode, group: bool) -> Option<LayerAppearance> {
        let opacity = n.opacity.unwrap_or(1.0);
        let default = if group {
            BLEND_PASS_THROUGH
        } else {
            BLEND_NORMAL
        };
        let mut mode = match n.blend_mode.as_deref() {
            None | Some("UNSET") => default,
            Some(name) => match blend(name) {
                Some(m) => m,
                None => {
                    self.lose(
                        format!("native.blend_mode={name}"),
                        "blend_mode_no_folio_home",
                    );
                    default
                }
            },
        };
        if mode == BLEND_PASS_THROUGH && !group {
            self.lose(
                "native.blend_mode=PASS_THROUGH",
                "pass_through_on_leaf_mapped_normal",
            );
            mode = BLEND_NORMAL;
        }
        if (opacity - 1.0).abs() < f32::EPSILON && mode == default {
            return None;
        }
        Some(LayerAppearance {
            opacity: percent(opacity),
            fill_opacity: 100.0,
            blend_mode: mode,
            clipped: false,
            isolation: if group && mode == BLEND_PASS_THROUGH {
                Isolation::PassThrough
            } else {
                Isolation::Isolated
            },
            mask: None,
        })
    }

    fn paint(
        &mut self,
        p: &Paint,
        size: Option<crate::nodes::Vec2>,
        what: &str,
        abs: &Affine,
    ) -> Option<(StudioPaint, f64)> {
        let base = p.opacity.unwrap_or(1.0);
        match &p.kind {
            PaintKind::Solid { color: Some(c) } => Some((
                StudioPaint::Solid { colour: colour(c) },
                percent(base * c.a),
            )),
            PaintKind::Solid { color: None } => {
                self.lose(format!("native.{what}.solid"), "paint_colour_missing");
                None
            }
            PaintKind::Gradient {
                gradient,
                stops,
                transform,
            } => {
                let geometry = match gradient.as_str() {
                    "GRADIENT_LINEAR" => StudioGradientGeometry::Linear {
                        x1: 0.0,
                        y1: 0.5,
                        x2: 1.0,
                        y2: 0.5,
                    },
                    "GRADIENT_RADIAL" => StudioGradientGeometry::Radial {
                        cx: 0.5,
                        cy: 0.5,
                        r: 0.5,
                        fx: 0.5,
                        fy: 0.5,
                        fr: 0.0,
                    },
                    other => {
                        self.lose(
                            format!("native.{what}.{other}"),
                            "gradient_kind_no_folio_home",
                        );
                        return None;
                    }
                };
                // Figma's paint transform maps the node's unit box to gradient space; Folio's maps
                // gradient space to the layer's geometry, which is baked into document px: the
                // node's absolute transform x node size x inverse(paint transform). Checked against
                // the fixture PNG for the identity paint transform (fig_paints).
                let identity = crate::nodes::Matrix {
                    m00: 1.0,
                    m01: 0.0,
                    m02: 0.0,
                    m10: 0.0,
                    m11: 1.0,
                    m12: 0.0,
                };
                let (Some(inv), Some(size)) =
                    (invert(transform.as_ref().unwrap_or(&identity)), size)
                else {
                    self.lose(
                        format!("native.{what}.gradient_transform"),
                        "gradient_transform_unmappable",
                    );
                    return None;
                };
                let (w, h) = (wide(size.x), wide(size.y));
                let local = [
                    inv[0] * w,
                    inv[1] * h,
                    inv[2] * w,
                    inv[3] * h,
                    inv[4] * w,
                    inv[5] * h,
                ];
                // abs = [m00 m01 m02; m10 m11 m12] after the Folio matrix [a c e; b d f].
                let [a, b, c, d, e, f] = local;
                let t = [
                    abs[0] * a + abs[1] * b,
                    abs[3] * a + abs[4] * b,
                    abs[0] * c + abs[1] * d,
                    abs[3] * c + abs[4] * d,
                    abs[0] * e + abs[1] * f + abs[2],
                    abs[3] * e + abs[4] * f + abs[5],
                ]
                .map(tidy);
                if w <= 0.0 || h <= 0.0 {
                    self.lose(
                        format!("native.{what}.gradient_transform"),
                        "gradient_transform_unmappable",
                    );
                    return None;
                }
                let stops = stops
                    .iter()
                    .map(|s| StudioGradientStop {
                        position: percent(s.position),
                        midpoint: None,
                        opacity: percent(s.color.a),
                        colour: colour(&s.color),
                    })
                    .collect();
                Some((
                    StudioPaint::Gradient {
                        gradient: StudioGradient {
                            geometry,
                            transform: t,
                            spread: GradientSpread::Pad,
                            interpolation: GradientInterpolation::Classic,
                            dither: false,
                            stops,
                        },
                    },
                    percent(base),
                ))
            }
            PaintKind::Image {
                part,
                scale_mode,
                transform,
                ..
            } => {
                // A stand-in grey row carries the paint; the image binds to that row and reads
                // its bytes from the `images/<hash>` part of the import-original entry.
                let digest = |d: &[u8; 32]| ContentDigest {
                    algorithm: "sha256".to_owned(),
                    digest: hex(d),
                };
                let mode = match scale_mode.as_deref() {
                    None | Some("FILL") => Some(StudioImageScale::Fill),
                    Some("FIT") => Some(StudioImageScale::Fit),
                    Some("CROP") | Some("STRETCH") => Some(StudioImageScale::Crop),
                    Some("TILE") => Some(StudioImageScale::Tile),
                    Some(_) => None,
                };
                let part_digest = part
                    .as_ref()
                    .and_then(|p| self.doc.image_parts.get(p).map(|d| (p.clone(), *d)));
                let (Some(mode), Some((part, part_sha)), Some(source_sha)) =
                    (mode, part_digest, self.doc.source_sha256)
                else {
                    self.lose(
                        format!("native.{what}.IMAGE"),
                        if mode.is_none() {
                            "image_scale_mode_unknown"
                        } else {
                            "image_part_unavailable"
                        },
                    );
                    return None;
                };
                // Figma's image transform maps the node's unit box to image space; Folio's maps
                // the unit image square to the unit square of the object: the inverse. UNVERIFIED.
                let folio_transform = match transform {
                    None => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    Some(m) => match invert(m) {
                        Some(i) => i.map(tidy),
                        None => {
                            self.lose(
                                format!("native.{what}.IMAGE"),
                                "image_transform_not_invertible",
                            );
                            return None;
                        }
                    },
                };
                self.pending_image = Some(StudioImagePaint {
                    style_id: String::new(),
                    row: 0,
                    image: StudioImageSource::ImportOriginal {
                        entry_digest: digest(&source_sha),
                        part,
                        content_digest: digest(&part_sha),
                    },
                    scale_mode: mode,
                    transform: folio_transform,
                    scaling_factor: 1.0,
                    rotation: 0.0,
                    opacity: percent(base),
                });
                Some((
                    StudioPaint::Solid {
                        colour: StudioColor {
                            r: 0.5,
                            g: 0.5,
                            b: 0.5,
                            colour_profile_id: ProfileBinding::Unassigned(()),
                        },
                    },
                    percent(base),
                ))
            }
            PaintKind::Unsupported { type_name } => {
                self.lose(
                    format!("native.{what}.{type_name}"),
                    "paint_type_unsupported",
                );
                None
            }
        }
    }

    fn paint_blend(&mut self, p: &Paint, what: &str) -> u16 {
        match p.blend_mode.as_deref() {
            None | Some("NORMAL") | Some("UNSET") | Some("PASS_THROUGH") => BLEND_NORMAL,
            Some(name) => match blend(name) {
                Some(m) => m,
                None => {
                    self.lose(
                        format!("native.{what}.blend_mode={name}"),
                        "blend_mode_no_folio_home",
                    );
                    BLEND_NORMAL
                }
            },
        }
    }

    /// One PAINT style holding the node's whole appearance stack (fills bottom to top, then
    /// strokes), bound through `fill_style_id`. `None` when the node has no mappable paint.
    fn style(&mut self, n: &FigNode, node: NodeRef, abs: &Affine) -> Result<()> {
        self.style_rows(n, node, true, true, abs)
    }

    /// [`Self::style`] restricted to the fill rows and/or the stroke rows of the node.
    fn style_rows(
        &mut self,
        n: &FigNode,
        node: NodeRef,
        fills: bool,
        strokes: bool,
        abs: &Affine,
    ) -> Result<()> {
        let mut rows = Vec::new();
        let mut images: Vec<StudioImagePaint> = Vec::new();
        let none: &[Paint] = &[];
        for p in if fills { &n.fills[..] } else { none } {
            self.pending_image = None;
            if let Some((paint, opacity)) = self.paint(p, n.size, "fills", abs) {
                if let Some(mut img) = self.pending_image.take() {
                    img.row = u32::try_from(rows.len()).unwrap_or(u32::MAX);
                    images.push(img);
                }
                let blend_mode = self.paint_blend(p, "fills");
                rows.push(StudioAppearanceRow::Fill {
                    paint,
                    opacity,
                    blend_mode,
                    visible: p.visible.unwrap_or(true),
                });
            }
        }
        if strokes && !n.strokes.is_empty() {
            let join = match n.stroke_join.as_deref() {
                Some("ROUND") => StrokeJoin::Round,
                Some("BEVEL") => StrokeJoin::Bevel,
                _ => StrokeJoin::Miter,
            };
            let align = match n.stroke_align.as_deref() {
                Some("INSIDE") => StrokeAlign::Inside,
                Some("OUTSIDE") => StrokeAlign::Outside,
                _ => StrokeAlign::Center,
            };
            let weight = wide(n.stroke_weight.unwrap_or(1.0)).max(0.0);
            // Figma caps: NONE = butt, ROUND, SQUARE = projecting; marker caps (arrows, ERD,
            // washi tape) have no Folio stroke cap: butt with a typed loss.
            let cap = match n.stroke_cap.as_deref() {
                None | Some("NONE") => StrokeCap::Butt,
                Some("ROUND") => StrokeCap::Round,
                Some("SQUARE") => StrokeCap::Projecting,
                Some(other) => {
                    self.lose(
                        format!("native.strokes.cap={other}"),
                        "stroke_cap_marker_no_folio_home",
                    );
                    StrokeCap::Butt
                }
            };
            // Miter limit: Figma's stored ratio (default 4), Folio bounds 1..=500.
            let miter_limit = match n.miter_limit.map(wide) {
                None => 4.0,
                Some(m) if (1.0..=500.0).contains(&m) => m,
                Some(_) => {
                    self.lose("native.strokes.miter_limit", "miter_limit_out_of_range");
                    4.0
                }
            };
            // Dashes: Figma repeats an odd-length pattern; Folio needs an even count, so an
            // odd pattern is written twice (the same repetition). Negative lengths are refused.
            let mut dash: Vec<Length> = n.dash_pattern.iter().map(|d| px(wide(*d))).collect();
            if dash.iter().any(|d| d.value < 0.0) || (!dash.is_empty() && dash.iter().all(|d| d.value == 0.0)) {
                self.lose("native.strokes.dash", "dash_pattern_invalid_solid_used");
                dash.clear();
            } else if dash.len() % 2 == 1 {
                dash.extend(dash.clone());
            }
            for p in &n.strokes {
                self.pending_image = None;
                if let Some((paint, opacity)) = self.paint(p, n.size, "strokes", abs) {
                    if let Some(mut img) = self.pending_image.take() {
                        img.row = u32::try_from(rows.len()).unwrap_or(u32::MAX);
                        images.push(img);
                    }
                    let blend_mode = self.paint_blend(p, "strokes");
                    rows.push(StudioAppearanceRow::Stroke {
                        paint,
                        stroke: StudioStrokeStyle {
                            weight: px(weight),
                            cap,
                            join,
                            align,
                            miter_limit,
                            dash: dash.clone(),
                            dash_offset: px(0.0),
                        },
                        opacity,
                        blend_mode,
                        visible: p.visible.unwrap_or(true),
                    });
                }
            }
        }
        if rows.is_empty() {
            return Ok(());
        }
        let style_id = self.mint.mint("SSTY");
        let ordinal = self.styles.len();
        self.styles.push(StudioPaintStyle {
            schema_id: StyleRegistrySchema::V1,
            style_id: style_id.clone(),
            kind: StyleKind::Paint,
            name: format!("fig/{ordinal}"),
            publishing: StudioPublishing {
                description: String::new(),
                description_markdown: String::new(),
                documentation_links: Vec::new(),
                publish_key: format!("fig-import/{ordinal}"),
                is_remote: false,
            },
            rows,
        });
        for mut img in images {
            img.style_id = style_id.clone();
            self.image_paint_records.push(img);
        }
        self.design_for(&node)
            .style_refs
            .insert("fill_style_id".into(), style_id);
        Ok(())
    }

    /// Inline story of a TEXT node: paragraphs end after each `\n`, one character range per style
    /// run (byte offsets). Run fonts, sizes, line height, letter spacing and alignment have no
    /// Folio record (type styles are host-resolved `STYS` references): typed losses.
    fn story(&mut self, t: &crate::nodes::TextPayload, story_id: String, node_fills: &[Paint]) {
        let text = &t.characters;
        let len = text.len() as u64;
        let mut paragraphs = Vec::new();
        let mut start = 0u64;
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                let end = (i as u64).saturating_add(1);
                paragraphs.push((start, end));
                start = end;
            }
        }
        if start < len {
            paragraphs.push((start, len));
        }
        // Run boundaries are Unicode scalar indices; convert them to byte offsets.
        let offsets: Vec<u64> = text
            .char_indices()
            .map(|(i, _)| i as u64)
            .chain(std::iter::once(len))
            .collect();
        let at = |k: u32| offsets.get(k as usize).copied().unwrap_or(len);
        // Character styles: one `STYS` binding per distinct (family, style, size, fill) of a run;
        // a story without runs binds the node's base values.
        let base_fills = node_fills;
        let mut ranges: Vec<(u64, u64, Option<String>)> = Vec::new();
        for r in &t.runs {
            let (a, b) = (at(r.start), at(r.end));
            if a < b {
                let fills = if r.fills.is_empty() {
                    base_fills
                } else {
                    &r.fills
                };
                let style = self.character_style(&r.font_family, &r.font_style, r.font_size, fills);
                ranges.push((a, b, style));
            }
        }
        if ranges.is_empty() && len > 0 {
            let style =
                self.character_style(&t.font_family, &t.font_style, t.font_size, base_fills);
            ranges.push((0, len, style));
        }
        // Paragraph style: line height, letter spacing, alignment and auto-resize of the node.
        // Vertical alignment and auto-resize have no type style member: typed losses per story.
        if t.align_vertical.is_some() {
            self.lose(
                "native.text.align_vertical",
                "text_layout_no_type_style_home",
            );
        }
        if t.auto_resize.is_some() {
            self.lose("native.text.auto_resize", "text_layout_no_type_style_home");
        }
        let paragraph_style = if t.line_height.is_some()
            || t.letter_spacing.is_some()
            || t.align_horizontal.is_some()
        {
            let pair = |p: &Option<(f32, String)>| p.as_ref().map(|(v, u)| (wide(*v), u.clone()));
            Some(self.text_style(FigTextStyle {
                style_ref: String::new(),
                kind: "paragraph",
                font_family: None,
                font_style: None,
                font_size: None,
                fill: None,
                line_height: pair(&t.line_height),
                letter_spacing: pair(&t.letter_spacing),
                align_horizontal: t.align_horizontal.clone(),
            }))
        } else {
            None
        };
        self.stories.push(StudioTextStory {
            schema_id: TextStorySchema::V1,
            story_id,
            text_kind: StoryTextKind::Point,
            story_direction: StoryDirection::LeftToRight,
            content: StoryContent::Inline {
                text_utf8: text.clone(),
            },
            paragraphs: paragraphs
                .into_iter()
                .map(|(start, end)| StoryParagraph {
                    start,
                    end,
                    style_ref: paragraph_style.clone(),
                    paragraph_direction: None,
                })
                .collect(),
            character_ranges: ranges
                .into_iter()
                .map(|(start, end, style_ref)| StoryRange {
                    start,
                    end,
                    style_ref,
                    applied_language: None,
                })
                .collect(),
        });
    }

    /// The `STYS` id of a style value set (one id per distinct set within the document).
    fn text_style(&mut self, mut st: FigTextStyle) -> String {
        let key = st.key();
        if let Some(id) = self.text_style_ids.get(&key) {
            return id.clone();
        }
        let id = self.mint.mint("STYS");
        st.style_ref = id.clone();
        self.text_style_ids.insert(key, id.clone());
        self.text_styles.push(st);
        id
    }

    /// Character style of one run; solid fills only (any other fill kind is a typed loss).
    fn character_style(
        &mut self,
        family: &Option<String>,
        style: &Option<String>,
        size: Option<f32>,
        fills: &[Paint],
    ) -> Option<String> {
        let mut fill = None;
        for p in fills {
            match &p.kind {
                PaintKind::Solid { color: Some(c) } if p.visible != Some(false) => {
                    if fill.is_none() {
                        let a = wide(c.a) * wide(p.opacity.unwrap_or(1.0));
                        fill = Some([wide(c.r), wide(c.g), wide(c.b), tidy(a)]);
                    }
                }
                PaintKind::Solid { .. } => {}
                _ => self.lose("native.text.run_fills", "text_fill_kind_not_carried"),
            }
        }
        if family.is_none() && style.is_none() && size.is_none() && fill.is_none() {
            return None;
        }
        Some(self.text_style(FigTextStyle {
            style_ref: String::new(),
            kind: "character",
            font_family: family.clone(),
            font_style: style.clone(),
            font_size: size.map(wide),
            fill,
            line_height: None,
            letter_spacing: None,
            align_horizontal: None,
        }))
    }

    fn publishing(key: Option<&str>, fallback: String) -> StudioPublishing {
        StudioPublishing {
            description: String::new(),
            description_markdown: String::new(),
            documentation_links: Vec::new(),
            publish_key: key.map_or(fallback, str::to_owned),
            is_remote: false,
        }
    }

    /// Component records after the walk: SYMBOL -> `StudioComponent` (its group layer), state-group
    /// FRAME -> `StudioComponentSet` (variant values in member order, first member the default),
    /// INSTANCE -> `StudioComponentInstance` (main component, overrides of characters, visibility
    /// and opacity on definition layers; fills listed without a value).
    fn components(&mut self) -> Result<()> {
        let doc = self.doc;
        let layer_of: HashMap<SourceId, String> = self
            .id_map
            .iter()
            .filter(|e| e.node.node_kind == NodeKind::Layer)
            .map(|e| (e.source, e.node.node_id.clone()))
            .collect();
        let mut component_of: HashMap<SourceId, String> = HashMap::new();
        let symbols = std::mem::take(&mut self.symbols);
        let sets = std::mem::take(&mut self.sets);
        let instances = std::mem::take(&mut self.instances);
        for (idx, layer_id) in &symbols {
            let n = &doc.nodes[*idx];
            self.current = Some(n.id);
            let component_id = self.mint.mint("SCMP");
            component_of.insert(n.id, component_id.clone());
            let key = match &n.payload {
                NodePayload::Symbol { component_key, .. } => component_key.as_deref(),
                _ => None,
            };
            self.components.push(StudioComponent {
                schema_id: hsk_studio_folio::ComponentSchema::V1,
                component_id: component_id.clone(),
                layer_id: layer_id.clone(),
                publishing: Self::publishing(key, format!("fig-import/{}", n.id)),
                variant_properties: None,
                component_property_definitions: Vec::new(),
            });
        }
        for (idx, layer_id) in &sets {
            let set = &doc.nodes[*idx];
            self.current = Some(set.id);
            let NodePayload::Frame(f) = &set.payload else {
                continue;
            };
            let names: HashMap<SourceId, String> = f
                .component_props
                .iter()
                .filter(|d| d.kind.as_deref() == Some("VARIANT"))
                .filter_map(|d| Some((d.id?, d.name.clone()?)))
                .collect();
            let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut default_variant = None;
            for &c in &set.children {
                let member = &doc.nodes[c];
                let NodePayload::Symbol { variant, .. } = &member.payload else {
                    continue;
                };
                let Some(cid) = component_of.get(&member.id).cloned() else {
                    continue;
                };
                let mut assigned = BTreeMap::new();
                for v in variant {
                    match (v.prop_def.and_then(|p| names.get(&p)), &v.value) {
                        (Some(name), Some(value)) => {
                            let values = groups.entry(name.clone()).or_default();
                            if !values.contains(value) {
                                values.push(value.clone());
                            }
                            assigned.insert(name.clone(), value.clone());
                        }
                        _ => {
                            self.current = Some(member.id);
                            self.lose("native.variant_property", "variant_property_unresolved");
                        }
                    }
                }
                if let Some(k) = self.components.iter_mut().find(|k| k.component_id == cid) {
                    k.variant_properties = (!assigned.is_empty()).then_some(assigned);
                }
                default_variant.get_or_insert(cid);
            }
            self.current = Some(set.id);
            let Some(default_variant_id) = default_variant else {
                self.lose("native.component_set", "component_set_without_members");
                continue;
            };
            if groups.is_empty() {
                self.lose(
                    "native.component_set",
                    "component_set_without_variant_properties",
                );
                continue;
            }
            let component_id = self.mint.mint("SCMP");
            self.component_sets.push(StudioComponentSet {
                schema_id: hsk_studio_folio::ComponentSchema::V1,
                component_id,
                layer_id: layer_id.clone(),
                publishing: Self::publishing(None, format!("fig-import/{}", set.id)),
                default_variant_id,
                variant_group_properties: groups,
                component_property_definitions: Vec::new(),
            });
        }
        // A component that names variant values but sits in no mapped set loses them.
        let members: std::collections::BTreeSet<String> = self
            .component_sets
            .iter()
            .flat_map(|s| {
                let set_layer = s.layer_id.clone();
                self.layers
                    .iter()
                    .filter(move |l| matches!(&l.parent, Parent::Node(p) if p.node_id == set_layer))
                    .map(|l| l.layer_id.clone())
            })
            .collect();
        let mut stray = 0u32;
        for k in &mut self.components {
            if k.variant_properties.is_some() && !members.contains(&k.layer_id) {
                k.variant_properties = None;
                stray = stray.saturating_add(1);
            }
        }
        for _ in 0..stray {
            self.lose("native.variant_property", "variant_outside_component_set");
        }
        for (idx, instance_id) in instances {
            let n = &doc.nodes[idx];
            self.current = Some(n.id);
            let NodePayload::Instance { instance, .. } = &n.payload else {
                continue;
            };
            let main = instance
                .symbol_id
                .and_then(|s| component_of.get(&s).cloned());
            if main.is_none() {
                self.lose(
                    "native.instance.symbol_id",
                    "instance_main_component_not_in_file",
                );
            }
            let mut by_node: BTreeMap<String, StudioOverrideRecord> = BTreeMap::new();
            for ov in &instance.overrides {
                let target = match ov.guid_path.as_slice() {
                    [one] => layer_of.get(one).cloned(),
                    _ => None,
                };
                let Some(target) = target else {
                    self.lose(
                        "native.instance.override_path",
                        "override_target_not_mapped",
                    );
                    continue;
                };
                let mut fields: Vec<(&'static str, Option<StudioValue>)> = Vec::new();
                if let Some(c) = &ov.characters {
                    fields.push(("characters", Some(StudioValue::String { value: c.clone() })));
                }
                if let Some(v) = ov.visible {
                    fields.push(("visible", Some(StudioValue::Boolean { value: v })));
                }
                if let Some(o) = ov.opacity {
                    fields.push(("opacity", Some(StudioValue::Float { value: wide(o) })));
                }
                if ov.fills.is_some() {
                    self.lose(
                        "native.instance.override_fills",
                        "override_fill_value_no_folio_home",
                    );
                    fields.push(("fills", None));
                }
                if ov.name.is_some() {
                    self.lose(
                        "native.instance.override_name",
                        "override_name_no_folio_home",
                    );
                }
                if ov.size.is_some() {
                    self.lose(
                        "native.instance.override_size",
                        "override_size_no_folio_home",
                    );
                }
                if fields.is_empty() {
                    continue;
                }
                let rec = by_node
                    .entry(target.clone())
                    .or_insert_with(|| StudioOverrideRecord {
                        overridden_node_id: target,
                        overridden_fields: Vec::new(),
                        values: BTreeMap::new(),
                    });
                for (field, value) in fields {
                    if !rec.overridden_fields.iter().any(|f| f == field) {
                        rec.overridden_fields.push(field.to_owned());
                    }
                    if let Some(value) = value {
                        rec.values.insert(field.to_owned(), value);
                    }
                }
            }
            let scale = instance.uniform_scale.map_or(1.0, wide);
            let scale_factor = if (0.01..=100.0).contains(&scale) {
                scale
            } else {
                self.lose("native.instance.scale", "instance_scale_out_of_range");
                1.0
            };
            self.component_instances.push(StudioComponentInstance {
                schema_id: hsk_studio_folio::ComponentInstanceSchema::V1,
                instance_id,
                main_component_id: main,
                component_properties: BTreeMap::new(),
                overrides: by_node.into_values().collect(),
                scale_factor,
                is_exposed_instance: false,
            });
        }
        Ok(())
    }

    /// The node design entry of `node` (one per node; Folio refuses duplicates).
    fn design_for(&mut self, node: &NodeRef) -> &mut StudioNodeDesign {
        let at = match self.designs.iter().position(|d| &d.node == node) {
            Some(i) => i,
            None => {
                self.designs.push(design(node.clone()));
                self.designs.len().saturating_sub(1)
            }
        };
        &mut self.designs[at]
    }

    /// Figma stack fields -> `StudioAutoLayout` on a container (group layer or artboard). Padding
    /// keeps the stored names: stackHorizontalPadding = left, stackVerticalPadding = top (Figma
    /// plugin naming; UNVERIFIED for files without right/bottom, which then reuse them). Sizing,
    /// alignment, wrap and border inclusion are not decoded: defaults with a typed loss.
    fn auto_layout(&mut self, f: &crate::nodes::FramePayload, node: &NodeRef, container: bool) {
        let Some(a) = &f.auto_layout else {
            return;
        };
        if !container {
            self.lose(
                "native.auto_layout",
                "auto_layout_on_non_container_no_folio_home",
            );
            return;
        }
        let mode = match a.mode.as_str() {
            "HORIZONTAL" => LayoutMode::Horizontal,
            "VERTICAL" => LayoutMode::Vertical,
            "GRID" => LayoutMode::Grid,
            "NONE" => return,
            _ => {
                self.lose(
                    format!("native.auto_layout.mode={}", a.mode),
                    "auto_layout_mode_unknown",
                );
                return;
            }
        };
        let spacing = a.spacing.map_or(0.0, wide);
        let left = a.horizontal_padding.map_or(0.0, wide);
        let top = a.vertical_padding.map_or(0.0, wide);
        let right = a.padding_right.map_or(left, wide);
        let bottom = a.padding_bottom.map_or(top, wide);
        let pad_ok = |v: f64| (0.0..=PADDING_MAX).contains(&v);
        if !(SPACING_MIN..=SPACING_MAX).contains(&spacing)
            || ![left, top, right, bottom].iter().all(|v| pad_ok(*v))
        {
            self.lose("native.auto_layout", "auto_layout_value_out_of_range");
            return;
        }
        self.lose(
            "native.auto_layout.sizing_alignment",
            "auto_layout_defaults_for_undecoded_fields",
        );
        self.design_for(node).auto_layout = Some(StudioAutoLayout {
            schema_id: AutoLayoutSchema::V1,
            layout_mode: mode,
            layout_wrap: LayoutWrap::NoWrap,
            primary_axis_sizing_mode: AxisSizingMode::Fixed,
            counter_axis_sizing_mode: AxisSizingMode::Fixed,
            primary_axis_align_items: PrimaryAxisAlign::Min,
            counter_axis_align_items: CounterAxisAlign::Min,
            counter_axis_align_content: CounterAxisAlignContent::Auto,
            item_spacing: spacing,
            counter_axis_spacing: None,
            padding_top: top,
            padding_right: right,
            padding_bottom: bottom,
            padding_left: left,
            strokes_included_in_layout: false,
            item_reverse_z_index: false,
        });
    }

    /// Prototype interactions -> ordered `StudioReaction`s on the node's design entry. Destinations
    /// resolve through the id map (after the walk). Easing: only LINEAR has a verified curve; other
    /// easings keep type and duration with a linear curve and a typed loss.
    fn reactions(&mut self) {
        let doc = self.doc;
        let nodes: HashMap<SourceId, NodeRef> = self
            .id_map
            .iter()
            .map(|e| (e.source, e.node.clone()))
            .collect();
        let entries: Vec<(SourceId, NodeRef)> = self
            .id_map
            .iter()
            .map(|e| (e.source, e.node.clone()))
            .collect();
        for (source, node_ref) in entries {
            let Some(n) = doc.node(source) else {
                continue;
            };
            if n.interactions.is_empty() {
                continue;
            }
            self.current = Some(source);
            let mut reactions = Vec::new();
            for it in &n.interactions {
                if it.deleted == Some(true) {
                    self.lose("native.interactions", "interaction_deleted_skipped");
                    continue;
                }
                let trigger = match it.trigger.as_deref() {
                    Some("ON_CLICK") => StudioTrigger::OnClick {},
                    Some("ON_HOVER") => StudioTrigger::OnHover {},
                    Some("ON_PRESS") => StudioTrigger::OnPress {},
                    Some("MOUSE_ENTER") => StudioTrigger::MouseEnter {},
                    Some("MOUSE_LEAVE") => StudioTrigger::MouseLeave {},
                    Some("MOUSE_DOWN") => StudioTrigger::MouseDown {},
                    Some("MOUSE_UP") => StudioTrigger::MouseUp {},
                    Some("AFTER_TIMEOUT") => {
                        let ms = it.timeout.map_or(0.0, |s| (wide(s) * 1000.0).round());
                        if !(0.0..=f64::from(hsk_studio_folio::MAX_TIMEOUT_MS)).contains(&ms) {
                            self.lose("native.interactions.timeout", "timeout_out_of_range");
                            continue;
                        }
                        StudioTrigger::AfterTimeout { timeout: ms as u32 }
                    }
                    other => {
                        self.lose(
                            format!("native.interactions.trigger={}", other.unwrap_or("?")),
                            "trigger_not_mapped",
                        );
                        continue;
                    }
                };
                let mut actions = Vec::new();
                for a in &it.actions {
                    match a.connection.as_deref() {
                        Some("BACK") => actions.push(StudioAction::Back {}),
                        Some("CLOSE") => actions.push(StudioAction::Close {}),
                        Some("INTERNAL_NODE") | None => {
                            let navigation = match a.navigation.as_deref() {
                                Some("NAVIGATE") | None => Navigation::Navigate,
                                Some("OVERLAY") => Navigation::Overlay,
                                Some("SWAP") => Navigation::Swap,
                                Some("SCROLL_TO") => Navigation::ScrollTo,
                                Some("CHANGE_TO") => Navigation::ChangeTo,
                                Some(other) => {
                                    self.lose(
                                        format!("native.interactions.navigation={other}"),
                                        "navigation_not_mapped",
                                    );
                                    continue;
                                }
                            };
                            let destination_id = match a.destination {
                                Some(d) => match nodes.get(&d) {
                                    Some(r) => Some(r.node_id.clone()),
                                    None => {
                                        self.lose(
                                            "native.interactions.destination",
                                            "destination_not_mapped",
                                        );
                                        continue;
                                    }
                                },
                                None => None,
                            };
                            let transition = match a.transition.as_deref() {
                                None | Some("INSTANT_TRANSITION") => None,
                                Some(t) => {
                                    // Directional tokens (`MOVE_FROM_LEFT`, `SLIDE_OUT_TO_TOP`, ...)
                                    // -> STU-PRO-118 type + direction (lane SMB-FIG-SMART-ANIMATE-2;
                                    // the direction names the edge entered from / left toward, a
                                    // Studio reading, Figma parity UNVERIFIED).
                                    let (transition_type, direction) = match t {
                                        "DISSOLVE" => (TransitionType::Dissolve, None),
                                        "FADE" => {
                                            self.lose("native.interactions.transition=FADE", "fade_mapped_as_dissolve");
                                            (TransitionType::Dissolve, None)
                                        }
                                        "SMART_ANIMATE" => (TransitionType::SmartAnimate, None),
                                        "SCROLL_ANIMATE" => (TransitionType::ScrollAnimate, None),
                                        other => match directional_transition(other) {
                                            Some((ty, dir)) => (ty, Some(dir)),
                                            None => {
                                                self.lose(
                                                    format!("native.interactions.transition={other}"),
                                                    "transition_not_mapped",
                                                );
                                                continue;
                                            }
                                        },
                                    };
                                    let match_layers = direction.map(|_| a.smart_animate.unwrap_or(false));
                                    let ms = a.duration.map_or(0.0, |d| wide(d) * 1000.0).round();
                                    if !(0.0..=f64::from(MAX_DURATION_MS)).contains(&ms) {
                                        self.lose(
                                            "native.interactions.duration",
                                            "transition_duration_out_of_range",
                                        );
                                        continue;
                                    }
                                    // Figma easing names -> the Studio-declared preset tuples
                                    // (STU-PRO-120, folio::EASING_PRESETS); a custom cubic keeps
                                    // its stored numbers; springs have no published tuple.
                                    let preset = match a.easing.as_deref() {
                                        None | Some("LINEAR") => easing_preset("LINEAR"),
                                        Some("IN_CUBIC") | Some("EASE_IN") => easing_preset("EASE_IN"),
                                        Some("OUT_CUBIC") => easing_preset("EASE_OUT"),
                                        Some("INOUT_CUBIC") => easing_preset("EASE_IN_AND_OUT"),
                                        Some("IN_BACK_CUBIC") => easing_preset("EASE_IN_BACK"),
                                        Some("OUT_BACK_CUBIC") => easing_preset("EASE_OUT_BACK"),
                                        Some("INOUT_BACK_CUBIC") => easing_preset("EASE_IN_AND_OUT_BACK"),
                                        Some("CUSTOM_CUBIC") => match a.easing_function.as_slice() {
                                            [x1, y1, x2, y2] => Some(CubicBezier {
                                                x1: wide(*x1),
                                                y1: wide(*y1),
                                                x2: wide(*x2),
                                                y2: wide(*y2),
                                            }),
                                            _ => None,
                                        },
                                        _ => None,
                                    };
                                    // CUSTOM_SPRING: `easingFunction` read in the plugin-API order
                                    // {mass, stiffness, damping, initialVelocity} (the .fig order
                                    // is UNVERIFIED: typed note); out-of-range sets fall back.
                                    let spring = match (a.easing.as_deref(), a.easing_function.as_slice()) {
                                        (Some("CUSTOM_SPRING"), [m, k, c, rest @ ..]) if rest.len() <= 1 => {
                                            let sp = hsk_studio_folio::SpringEasing {
                                                mass: wide(*m),
                                                stiffness: wide(*k),
                                                damping: wide(*c),
                                                initial_velocity: rest.first().map_or(0.0, |v| wide(*v)),
                                            };
                                            let ok = sp.mass > 0.0
                                                && sp.mass <= 100.0
                                                && sp.stiffness > 0.0
                                                && sp.stiffness <= 10_000.0
                                                && (0.0..=1000.0).contains(&sp.damping)
                                                && (-1000.0..=1000.0).contains(&sp.initial_velocity);
                                            if ok {
                                                self.lose("native.interactions.easing=CUSTOM_SPRING", "spring_parameter_order_unverified");
                                                Some(sp)
                                            } else {
                                                None
                                            }
                                        }
                                        _ => None,
                                    };
                                    let bezier = match preset {
                                        _ if spring.is_some() => (0.0, 0.0, 1.0, 1.0),
                                        Some(b) => (b.x1, b.y1, b.x2, b.y2),
                                        None => {
                                            self.lose(
                                                format!(
                                                    "native.interactions.easing={}",
                                                    a.easing.as_deref().unwrap_or("?")
                                                ),
                                                "easing_without_studio_tuple_linear_used",
                                            );
                                            (0.0, 0.0, 1.0, 1.0)
                                        }
                                    };
                                    Some(StudioTransition {
                                        transition_type,
                                        direction,
                                        match_layers,
                                        easing: StudioEasing {
                                            easing_function_cubic_bezier: spring.is_none().then_some(CubicBezier {
                                                x1: bezier.0,
                                                y1: bezier.1,
                                                x2: bezier.2,
                                                y2: bezier.3,
                                            }),
                                            easing_function_spring: spring,
                                        },
                                        duration: ms as u32,
                                    })
                                }
                            };
                            let overlay_relative_position = self.overlay_position(a, navigation);
                            actions.push(StudioAction::Node {
                                destination_id,
                                navigation,
                                transition,
                                preserve_scroll_position: false,
                                overlay_relative_position,
                                reset_video_position: false,
                                reset_scroll_position: false,
                                reset_interactive_components: false,
                            });
                        }
                        Some(other) => {
                            self.lose(
                                format!("native.interactions.connection={other}"),
                                "action_not_mapped",
                            );
                        }
                    }
                }
                reactions.push(StudioReaction {
                    trigger: Some(trigger),
                    actions,
                });
            }
            if !reactions.is_empty() {
                self.design_for(&node_ref).reactions = reactions;
            }
        }
    }

    /// STU-PRO-122..124: a frame's `overlayPositionType`, `overlayBackgroundAppearance` and
    /// `overlayBackgroundInteraction` -> one `StudioFrameOverlay` (only when the frame stores any of
    /// them). Unknown tokens are typed losses and fall back to the spec's unset / `NONE` states.
    fn frame_overlay(&mut self, f: &crate::nodes::FramePayload, node_ref: &NodeRef) {
        if f.overlay_position_type.is_none() && f.overlay_background.is_none() && f.overlay_background_interaction.is_none() {
            return;
        }
        let overlay_position_type = match f.overlay_position_type.as_deref() {
            None => None,
            Some("CENTER") => Some(StudioOverlayPositionType::Center),
            Some("TOP_LEFT") => Some(StudioOverlayPositionType::TopLeft),
            Some("TOP_CENTER") => Some(StudioOverlayPositionType::TopCenter),
            Some("TOP_RIGHT") => Some(StudioOverlayPositionType::TopRight),
            Some("BOTTOM_LEFT") => Some(StudioOverlayPositionType::BottomLeft),
            Some("BOTTOM_CENTER") => Some(StudioOverlayPositionType::BottomCenter),
            Some("BOTTOM_RIGHT") => Some(StudioOverlayPositionType::BottomRight),
            Some("MANUAL") => Some(StudioOverlayPositionType::Manual),
            Some(other) => {
                self.lose(format!("native.overlay_position_type={other}"), "overlay_position_unknown_center_used");
                None
            }
        };
        let (bg_type, bg_colour) = match &f.overlay_background {
            Some((t, c)) => (t.as_deref(), *c),
            None => (None, None),
        };
        let overlay_background = match (bg_type, bg_colour) {
            (Some("SOLID_COLOR"), Some(c)) => StudioOverlayBackground::SolidColor {
                color: StudioRgba {
                    r: wide(c.r).clamp(0.0, 1.0),
                    g: wide(c.g).clamp(0.0, 1.0),
                    b: wide(c.b).clamp(0.0, 1.0),
                    a: wide(c.a).clamp(0.0, 1.0),
                    colour_profile_id: ProfileBinding::Unassigned(()),
                },
            },
            (Some("SOLID_COLOR"), None) => {
                self.lose("native.overlay_background", "overlay_background_colour_missing_none_used");
                StudioOverlayBackground::None {}
            }
            (None | Some("NONE"), _) => StudioOverlayBackground::None {},
            (Some(other), _) => {
                self.lose(format!("native.overlay_background={other}"), "overlay_background_unknown_none_used");
                StudioOverlayBackground::None {}
            }
        };
        let overlay_background_interaction = match f.overlay_background_interaction.as_deref() {
            None | Some("NONE") => StudioOverlayBackgroundInteraction::None,
            Some("CLOSE_ON_CLICK_OUTSIDE") => StudioOverlayBackgroundInteraction::CloseOnClickOutside,
            Some(other) => {
                self.lose(format!("native.overlay_background_interaction={other}"), "overlay_interaction_unknown_none_used");
                StudioOverlayBackgroundInteraction::None
            }
        };
        self.frame_overlay.push(StudioFrameOverlay {
            node: node_ref.clone(),
            overlay_position_type,
            overlay_background,
            overlay_background_interaction,
        });
    }

    /// The `overlay_relative_position` of a mapped `NODE` action (STU-PRO-112/123, document px,
    /// precision 2): the action's stored `overlayRelativePosition` on an `OVERLAY` navigation, else
    /// the destination frame's node-level one when that frame is `MANUAL` (typed note), else the
    /// origin `{0, 0}` when that frame is `MANUAL` (typed loss: Folio refuses a `MANUAL` overlay
    /// without a position, STU-PRO-154). A position on any other navigation is dropped (STU-PRO-155,
    /// typed loss).
    fn overlay_position(&mut self, a: &crate::nodes::InteractionAction, navigation: Navigation) -> Option<OverlayPosition> {
        let at = |v: crate::nodes::Vec2| {
            let p = OverlayPosition { x: (wide(v.x) * 100.0).round() / 100.0, y: (wide(v.y) * 100.0).round() / 100.0 };
            (p.x.is_finite() && p.y.is_finite()).then_some(p)
        };
        if navigation != Navigation::Overlay {
            if a.overlay_relative_position.is_some() {
                self.lose("native.interactions.overlay_relative_position", "overlay_position_on_non_overlay_navigation");
            }
            return None;
        }
        let doc = self.doc;
        let dest = a.destination.and_then(|d| doc.node(d)).and_then(|n| match &n.payload {
            NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } => Some(f),
            _ => None,
        });
        let manual = dest.is_some_and(|f| f.overlay_position_type.as_deref() == Some("MANUAL"));
        if let Some(p) = a.overlay_relative_position.and_then(at) {
            return Some(p);
        }
        if !manual {
            return None;
        }
        if let Some(p) = dest.and_then(|f| f.overlay_relative_position).and_then(at) {
            self.lose("native.interactions.overlay_relative_position", "overlay_position_from_destination_node");
            return Some(p);
        }
        self.lose("native.interactions.overlay_relative_position", "overlay_manual_position_missing_origin_used");
        Some(OverlayPosition { x: 0.0, y: 0.0 })
    }

    /// VARIABLE_SET -> `StudioVariableCollection` (Figma mode ids kept as `session:local`, first mode
    /// the default), VARIABLE -> `StudioVariable` with a value per mode (colour, float, boolean,
    /// string, alias). Paint bindings to variables have no Folio field: typed loss.
    fn variables(&mut self) {
        let doc = self.doc;
        let sets: Vec<usize> = doc
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.type_name == "VARIABLE_SET")
            .map(|(i, _)| i)
            .collect();
        let vars: Vec<usize> = doc
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.type_name == "VARIABLE")
            .map(|(i, _)| i)
            .collect();
        let mut collection_of: HashMap<SourceId, (String, Vec<String>)> = HashMap::new();
        for &i in &sets {
            let n = &doc.nodes[i];
            self.current = Some(n.id);
            let NodePayload::VariableSet { modes } = &n.payload else {
                continue;
            };
            if modes.is_empty() {
                self.lose("native.variable_set", "variable_set_without_modes");
                continue;
            }
            let id = self.mint.mint("SVCL");
            let mode_ids: Vec<String> = modes.iter().map(|m| m.id.to_string()).collect();
            collection_of.insert(n.id, (id.clone(), mode_ids));
            self.record_map.push(RecordMapEntry {
                source: n.id,
                record_id: id.clone(),
            });
            self.collections.push(StudioVariableCollection {
                schema_id: VariableCollectionSchema::V1,
                collection_id: id,
                name: n.name.clone().unwrap_or_default(),
                hidden_from_publishing: false,
                publishing: Self::publishing(None, format!("fig-import/{}", n.id)),
                is_extension: false,
                modes: modes
                    .iter()
                    .map(|m| StudioMode {
                        mode_id: m.id.to_string(),
                        name: m.name.clone().unwrap_or_else(|| m.id.to_string()),
                    })
                    .collect(),
                default_mode_id: modes[0].id.to_string(),
                variable_ids: Vec::new(),
            });
        }
        // Ids first: aliases may point forward.
        let mut var_ids: HashMap<SourceId, (String, VariableType)> = HashMap::new();
        for &i in &vars {
            let n = &doc.nodes[i];
            let NodePayload::Variable(v) = &n.payload else {
                continue;
            };
            let ty = match v.resolved_type.as_deref() {
                Some("COLOR") => VariableType::Color,
                Some("FLOAT") => VariableType::Float,
                Some("BOOLEAN") => VariableType::Boolean,
                Some("STRING") => VariableType::String,
                other => {
                    self.current = Some(n.id);
                    self.lose(
                        format!("native.variable.type={}", other.unwrap_or("?")),
                        "variable_type_not_mapped",
                    );
                    continue;
                }
            };
            var_ids.insert(n.id, (self.mint.mint("SVAR"), ty));
        }
        for &i in &vars {
            let n = &doc.nodes[i];
            self.current = Some(n.id);
            let NodePayload::Variable(v) = &n.payload else {
                continue;
            };
            let Some((variable_id, ty)) = var_ids.get(&n.id).cloned() else {
                continue;
            };
            let Some((collection_id, modes)) =
                v.set_id.and_then(|s| collection_of.get(&s)).cloned()
            else {
                self.lose("native.variable.set", "variable_collection_not_mapped");
                continue;
            };
            let mut values = BTreeMap::new();
            for (mode, value) in &v.values {
                let mapped = match value {
                    VariableValue::Color(c) => Some(StudioValue::Color {
                        value: StudioRgba {
                            r: wide(c.r).clamp(0.0, 1.0),
                            g: wide(c.g).clamp(0.0, 1.0),
                            b: wide(c.b).clamp(0.0, 1.0),
                            a: wide(c.a).clamp(0.0, 1.0),
                            colour_profile_id: ProfileBinding::Unassigned(()),
                        },
                    }),
                    VariableValue::Float(f) => Some(StudioValue::Float { value: wide(*f) }),
                    VariableValue::Bool(b) => Some(StudioValue::Boolean { value: *b }),
                    VariableValue::Text(t) => Some(StudioValue::String { value: t.clone() }),
                    VariableValue::Alias(target) => {
                        var_ids
                            .get(target)
                            .map(|(id, t)| StudioValue::VariableAlias {
                                id: id.clone(),
                                resolved_type: *t,
                            })
                    }
                    VariableValue::Unsupported(_) => None,
                };
                match mapped {
                    Some(m) => {
                        values.insert(mode.to_string(), m);
                    }
                    None => self.lose("native.variable.value", "variable_value_not_mapped"),
                }
            }
            if modes.iter().any(|m| !values.contains_key(m)) || values.len() != modes.len() {
                self.lose(
                    "native.variable.values_by_mode",
                    "variable_mode_coverage_incomplete",
                );
                continue;
            }
            let scopes: Vec<String> = v
                .scopes
                .iter()
                .filter(|s| VARIABLE_SCOPES.contains(&s.as_str()))
                .cloned()
                .collect();
            if scopes.len() != v.scopes.len() {
                self.lose("native.variable.scopes", "variable_scope_not_in_folio_set");
            }
            if let Some(k) = self
                .collections
                .iter_mut()
                .find(|k| k.collection_id == collection_id)
            {
                k.variable_ids.push(variable_id.clone());
            }
            self.record_map.push(RecordMapEntry {
                source: n.id,
                record_id: variable_id.clone(),
            });
            self.variables.push(StudioVariable {
                schema_id: VariableSchema::V1,
                variable_id,
                name: n.name.clone().unwrap_or_default(),
                hidden_from_publishing: false,
                publishing: Self::publishing(None, format!("fig-import/{}", n.id)),
                variable_collection_id: collection_id,
                resolved_type: ty,
                values_by_mode: values,
                scopes,
                code_syntax: BTreeMap::new(),
            });
        }
        // Paints bound to variables (colorVar): no Folio field binds a paint row to a variable.
        let bound = doc
            .nodes
            .iter()
            .flat_map(|n| n.fills.iter().chain(&n.strokes).map(move |p| (n.id, p)))
            .filter(|(_, p)| p.color_variable.is_some())
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        for id in bound {
            self.current = Some(id);
            self.lose(
                "native.paint.color_variable",
                "paint_variable_binding_no_folio_home",
            );
        }
    }

    /// Shadow effects of a layer node -> one `StudioEffectStack` in the raster records; every
    /// effect without a Folio kind (blurs) or on an artboard is a typed loss.
    fn effects(&mut self, n: &FigNode, node: &NodeRef) {
        let doc = self.doc;
        // An effect style (`styleIdForEffect`) is a local style node carrying the effects; it
        // supplies the node's effects (Figma keeps the style's effects authoritative).
        let effects: &[crate::nodes::Effect] = match n.effect_style {
            None => &n.effects,
            Some(style) => match doc.node(style) {
                Some(st) if !st.effects.is_empty() => {
                    if !n.effects.is_empty() && n.effects != st.effects {
                        self.lose("native.effects.style", "effect_style_replaces_inline_effects");
                    }
                    &st.effects
                }
                _ => {
                    self.lose("native.effects.style", "effect_style_not_in_file");
                    &n.effects
                }
            },
        };
        if n.fill_style.is_some() || n.stroke_style.is_some() {
            // The paints are the style's synced values; the style link itself has no Folio field.
            self.lose("native.paint.style_ref", "paint_style_reference_not_mapped");
        }
        let mut list = Vec::new();
        for (i, e) in effects.iter().enumerate() {
            let path = format!("native.effects.{}", e.kind);
            if node.node_kind != NodeKind::Layer {
                self.lose(path, "effect_on_artboard_no_folio_home");
                continue;
            }
            match effect(e, i) {
                Ok((fx, notes)) => {
                    list.push(fx);
                    for note in notes {
                        self.lose(path.clone(), note);
                    }
                }
                Err(code) => self.lose(path, code),
            }
        }
        if !list.is_empty() {
            let stack = StudioEffectStack {
                effect_stack_id: self.mint.mint("SEFX"),
                enabled: true,
                scale: 100.0,
                effects: list,
            };
            let mut rec = StudioRasterLayerRecord::new(node.node_id.clone());
            rec.effects = Some(stack);
            self.fx.push(rec);
            // A group with effects composites as one isolated Normal unit (a pass-through group
            // has no own result to shadow; composite refuses effects on pass-through).
            if let Some(l) = self
                .layers
                .iter_mut()
                .find(|l| l.layer_id == node.node_id && l.kind == "group")
            {
                let a = l.appearance.get_or_insert(LayerAppearance {
                    opacity: 100.0,
                    fill_opacity: 100.0,
                    blend_mode: BLEND_PASS_THROUGH,
                    clipped: false,
                    isolation: Isolation::PassThrough,
                    mask: None,
                });
                if a.blend_mode == BLEND_PASS_THROUGH {
                    a.blend_mode = BLEND_NORMAL;
                    a.isolation = Isolation::Isolated;
                    self.lose_plain("native.effects.group", "group_with_effects_isolated");
                }
            }
        }
    }

    /// Losses for decoded node data this slice does not map.
    /// `baked`: the node's geometry was emitted as vector paths with its transform applied.
    fn unmapped(&mut self, n: &FigNode, artboard: bool, baked: bool) {
        let ty = n.type_name.as_str();
        if !artboard && ty != "CANVAS" && !baked {
            // A container's transform is composed into the geometry of its descendants.
            if n.transform.is_some() && !CONTAINER_KINDS.contains(&ty) && ty != "DOCUMENT" {
                self.lose("native.transform", "layer_transform_no_folio_home");
            }
            if n.size.is_some() && ty != "TEXT" {
                self.lose("native.size", "layer_size_no_folio_home");
            }
        }
        if !n.stroke_geometry.is_empty() {
            // The stroke outline is derived from the path plus the stroke row; not stored.
            self.lose("native.stroke_geometry", "stroke_outline_not_stored");
        }
        if !baked && !n.fill_geometry.is_empty() {
            self.lose(
                "native.geometry",
                if CONTAINER_KINDS.contains(&ty) || artboard {
                    "container_geometry_no_folio_home"
                } else {
                    "vector_geometry_not_emitted"
                },
            );
        }
        match &n.payload {
            NodePayload::Rectangle { .. } if !baked => {
                self.lose("native.corner_radii", "shape_geometry_not_emitted")
            }
            NodePayload::Ellipse { .. } if !baked => {
                self.lose("native.arc", "shape_geometry_not_emitted")
            }
            NodePayload::Star { .. } if !baked => {
                self.lose("native.star", "shape_geometry_not_emitted")
            }
            NodePayload::Vector { .. } if !baked => {
                self.lose("native.vector_network", "vector_geometry_not_emitted")
            }
            // Mapped to a `StudioBooleanGroup` (or lost) by `walk`.
            NodePayload::Boolean { .. } => {}
            NodePayload::Frame(f)
            | NodePayload::Symbol { frame: f, .. }
            | NodePayload::Instance { frame: f, .. } => {
                // With a background layer the rounded frame outline is its geometry; the clip
                // is mapped (or lost) by `frame_background`.
                if !baked && (f.corner_radius.is_some() || f.corner_radii.is_some()) {
                    self.lose("native.corner_radii", "frame_corner_radii_no_folio_home");
                }
                for d in &f.component_props {
                    if d.kind.as_deref() != Some("VARIANT") {
                        self.lose(
                            format!(
                                "native.component_property={}",
                                d.kind.as_deref().unwrap_or("?")
                            ),
                            "component_property_default_not_decoded",
                        );
                    }
                }
            }
            NodePayload::Canvas {
                background: Some(_),
                ..
            } => self.lose(
                "native.canvas_background",
                "canvas_background_no_folio_home",
            ),
            _ => {}
        }
    }

    /// Records an emitted node: paint order, plus the id map (document nodes) or the expansion
    /// map (nodes created by instance expansion; their definition keeps its id-map entry).
    fn record(&mut self, source: SourceId, node: &NodeRef) {
        self.order.push(node.clone());
        match self.exp.last() {
            None => self.id_map.push(IdMapEntry {
                source,
                node: node.clone(),
            }),
            Some(e) => {
                let mut path = e.prefix.clone();
                path.push(source);
                self.expanded.push(ExpandedNode {
                    instance: e.root,
                    path,
                    node: node.clone(),
                });
            }
        }
    }

    /// The node with the active expansion's overrides and derived values applied (`None`: the
    /// node is used as decoded).
    fn patched(&mut self, n: &FigNode) -> Option<FigNode> {
        let e = self.exp.last_mut()?;
        let key = [n.id];
        let mut hits = Vec::new();
        for (path, ov, matched) in &mut e.overrides {
            if path.as_slice() == key {
                *matched = true;
                hits.push(ov.clone());
            }
        }
        let mut derived = None;
        for (path, d, matched) in &mut e.derived {
            if path.as_slice() == key {
                *matched = true;
                derived = Some(d.clone());
            }
        }
        if hits.is_empty() && derived.is_none() {
            return None;
        }
        let mut out = n.clone();
        for ov in &hits {
            self.apply_override(&mut out, ov, false, derived.as_ref());
        }
        if let Some(d) = derived {
            self.apply_derived(&mut out, &d);
        }
        Some(out)
    }

    /// One override onto an expanded node. `root`: the override addresses the instance itself
    /// (its size is the instance node's own stored size, already in effect).
    fn apply_override(
        &mut self,
        n: &mut FigNode,
        ov: &InstanceOverride,
        root: bool,
        derived: Option<&DerivedSymbolNode>,
    ) {
        if let Some(name) = &ov.name {
            n.name = Some(name.clone());
        }
        if let Some(v) = ov.visible {
            n.visible = Some(v);
        }
        if let Some(o) = ov.opacity {
            n.opacity = Some(o);
        }
        if let Some(f) = &ov.fills {
            // Text runs without fills of their own carry the node fills (decoder copy): they
            // follow the override; runs with their own fills keep them.
            let old = std::mem::replace(&mut n.fills, f.clone());
            if let NodePayload::Text(t) = &mut n.payload {
                for r in t.runs.iter_mut().filter(|r| r.fills == old) {
                    r.fills = f.clone();
                }
            }
        }
        if ov.size.is_some() && !root && derived.and_then(|d| d.size).is_none() {
            // A resized sublayer needs Figma's laid-out geometry; without a derived entry for it
            // the definition's geometry is kept.
            self.lose_plain(
                "native.instance.expansion.override.size",
                "override_size_without_derived_layout",
            );
        }
        if let Some(chars) = &ov.characters {
            match &mut n.payload {
                NodePayload::Text(t) => {
                    let distinct = t
                        .runs
                        .iter()
                        .map(|r| r.style_id)
                        .collect::<std::collections::BTreeSet<_>>();
                    if distinct.len() > 1 {
                        self.lose_plain(
                            "native.instance.expansion.override.characters",
                            "override_text_runs_reset_to_first_style",
                        );
                    }
                    let first = t.runs.first().cloned();
                    t.characters = chars.clone();
                    let len = u32::try_from(chars.chars().count()).unwrap_or(u32::MAX);
                    t.runs = match first {
                        Some(r) if len > 0 => vec![TextRun {
                            start: 0,
                            end: len,
                            ..r
                        }],
                        _ => Vec::new(),
                    };
                }
                _ => self.lose_plain(
                    "native.instance.expansion.override.characters",
                    "override_characters_on_non_text",
                ),
            }
        }
        for field in &ov.unsupported {
            self.lose_plain(
                format!("native.instance.expansion.override.{field}"),
                "override_field_unsupported",
            );
        }
    }

    /// Figma's laid-out values of an expanded sublayer (`derivedSymbolData`).
    fn apply_derived(&mut self, n: &mut FigNode, d: &DerivedSymbolNode) {
        if let Some(t) = d.transform {
            n.transform = Some(t);
        }
        if let Some(s) = d.size {
            let changed = n.size.is_none_or(|old| old != s);
            if changed && !n.fill_geometry.is_empty() && n.type_name != "TEXT" {
                self.lose_plain(
                    "native.instance.expansion.derived.size",
                    "derived_size_geometry_not_rederived",
                );
            }
            n.size = Some(s);
        }
        if let (Some(text), NodePayload::Text(t)) = (&d.text, &mut n.payload) {
            t.derived = Some(text.clone());
        }
    }

    /// Nodes the expansion of symbol `sym_idx` creates (nested expansions included, two per node
    /// as an upper bound for a background or clip layer), or the refusal code: a symbol already
    /// being expanded (cycle), nesting past [`MAX_INSTANCE_DEPTH`], or more than `left` nodes.
    /// Walks node indices only; nothing is allocated beyond the bounded symbol chain and stack.
    fn expansion_cost(&self, sym_idx: usize, left: u64) -> std::result::Result<u64, &'static str> {
        let mut chain: Vec<SourceId> = Vec::new();
        let mut total = 0u64;
        self.cost_of(sym_idx, &mut chain, &mut total, left)?;
        Ok(total)
    }

    fn cost_of(
        &self,
        sym_idx: usize,
        chain: &mut Vec<SourceId>,
        total: &mut u64,
        left: u64,
    ) -> std::result::Result<(), &'static str> {
        let doc = self.doc;
        let sym = &doc.nodes[sym_idx];
        if chain.contains(&sym.id) {
            return Err("instance_cycle_refused");
        }
        if chain.len() >= MAX_INSTANCE_DEPTH {
            return Err("instance_nesting_over_limit");
        }
        chain.push(sym.id);
        let mut stack: Vec<usize> = vec![sym_idx];
        while let Some(i) = stack.pop() {
            self.cancel
                .check()
                .map_err(|_| "instance_expansion_canceled")?;
            *total = total.saturating_add(2);
            if *total > left {
                return Err("instance_expansion_over_budget");
            }
            let c = &doc.nodes[i];
            match &c.payload {
                NodePayload::Instance { instance, .. } if i != sym_idx => {
                    if let Some(&s) = instance
                        .symbol_id
                        .and_then(|s| self.symbol_index.get(&s))
                    {
                        self.cost_of(s, chain, total, left)?;
                    }
                }
                _ => stack.extend(c.children.iter().copied()),
            }
        }
        chain.pop();
        Ok(())
    }

    /// Draws an INSTANCE through an expansion group (`group` layer): the instance's own fill as
    /// background, its clip, then the definition's children with the overrides mapped onto them
    /// (the instance's own, then those of enclosing instances, which win). `binding`: the
    /// `component_instance` layer of a document instance (nested instances have none). Refusals
    /// (typed losses, nothing created): symbol not in the file, cycle, nesting or node budget.
    fn expand_instance(
        &mut self,
        n: &FigNode,
        parent: Parent,
        depth: usize,
        abs: &Affine,
        binding: Option<String>,
    ) -> Result<Option<NodeRef>> {
        let doc = self.doc;
        self.current = Some(n.id);
        let NodePayload::Instance { frame, instance } = &n.payload else {
            return Ok(None);
        };
        let Some(sym_idx) = instance
            .symbol_id
            .and_then(|s| self.symbol_index.get(&s).copied())
        else {
            self.lose_plain("native.instance.expansion", "instance_symbol_not_in_file");
            return Ok(None);
        };
        let symbol = &doc.nodes[sym_idx];
        let mut chain = self.exp.last().map(|e| e.chain.clone()).unwrap_or_default();
        if chain.contains(&symbol.id) {
            self.lose_plain("native.instance.expansion", "instance_cycle_refused");
            return Ok(None);
        }
        if chain.len() >= MAX_INSTANCE_DEPTH {
            self.lose_plain("native.instance.expansion", "instance_nesting_over_limit");
            return Ok(None);
        }
        if self.exp.is_empty() {
            // The whole expansion, nested instances included, is costed before anything exists.
            let left = self.expansion_budget.saturating_sub(self.expanded_total);
            match self.expansion_cost(sym_idx, left) {
                Ok(cost) => self.expanded_total = self.expanded_total.saturating_add(cost),
                Err(code) => {
                    self.lose_plain("native.instance.expansion", code);
                    return Ok(None);
                }
            }
        }
        chain.push(symbol.id);
        let mut overrides = Vec::new();
        for ov in &instance.overrides {
            match ov.guid_path.len() {
                0 => self.lose_plain(
                    "native.instance.expansion.override_path",
                    "override_path_empty",
                ),
                k if k > MAX_OVERRIDE_PATH => self.lose_plain(
                    "native.instance.expansion.override_path",
                    "override_path_over_limit",
                ),
                _ => overrides.push((ov.guid_path.clone(), ov.clone(), false)),
            }
        }
        let mut derived = Vec::new();
        for d in &instance.derived {
            if d.guid_path.is_empty() || d.guid_path.len() > MAX_OVERRIDE_PATH {
                self.lose_plain(
                    "native.instance.expansion.derived_path",
                    "derived_path_out_of_bounds",
                );
            } else {
                derived.push((d.guid_path.clone(), d.clone(), false));
            }
        }
        if let Some(outer) = self.exp.last_mut() {
            for (path, ov, matched) in &mut outer.overrides {
                if path.len() > 1 && path[0] == n.id {
                    *matched = true;
                    overrides.push((path[1..].to_vec(), ov.clone(), false));
                }
            }
            for (path, d, matched) in &mut outer.derived {
                if path.len() > 1 && path[0] == n.id {
                    *matched = true;
                    derived.push((path[1..].to_vec(), d.clone(), false));
                }
            }
        }
        if instance
            .uniform_scale
            .is_some_and(|s| (wide(s) - 1.0).abs() > 1e-9)
        {
            self.lose_plain("native.instance.expansion.scale", "instance_scale_not_applied");
        }
        // Overrides of the instance itself (path = the definition root).
        let mut root = n.clone();
        let mut root_hits = Vec::new();
        for (path, ov, matched) in &mut overrides {
            if path.as_slice() == [symbol.id] {
                *matched = true;
                root_hits.push(ov.clone());
            }
        }
        for ov in &root_hits {
            self.apply_override(&mut root, ov, true, None);
        }
        let mut prefix = self.exp.last().map(|e| e.prefix.clone()).unwrap_or_default();
        prefix.push(n.id);
        let root_id = self.exp.last().map_or(n.id, |e| e.root);
        let nested = binding.is_none();
        let group_id = self.mint.mint("SLYR");
        let appearance = self.appearance(&root, true);
        self.layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: group_id.clone(),
            name: root.name.clone().unwrap_or_default(),
            parent,
            kind: "group".into(),
            payload: Payload::Group,
            visible: root.visible.unwrap_or(true),
            appearance,
            text_frame: None,
        });
        let group = NodeRef {
            node_kind: NodeKind::Layer,
            node_id: group_id.clone(),
        };
        self.order.push(group.clone());
        if nested {
            self.expanded.push(ExpandedNode {
                instance: root_id,
                path: prefix.clone(),
                node: group.clone(),
            });
        }
        self.expansions.push(InstanceExpansion {
            instance: root_id,
            path: prefix.clone(),
            binding_layer_id: binding,
            group_layer_id: group_id,
        });
        self.exp.push(Expansion {
            root: root_id,
            prefix,
            symbol: symbol.id,
            chain,
            overrides,
            derived,
        });
        let body = self.expansion_body(&root, frame, symbol, &group, depth, abs, nested);
        let done = self.exp.pop();
        self.current = Some(n.id);
        if let Some(e) = done {
            let unmatched = e.overrides.iter().filter(|o| !o.2).count();
            for _ in 0..unmatched {
                self.lose_plain(
                    "native.instance.expansion.override_path",
                    "override_target_not_in_expansion",
                );
            }
            let _ = e.symbol;
        }
        body?;
        Ok(Some(group))
    }

    #[allow(clippy::too_many_arguments)]
    fn expansion_body(
        &mut self,
        root: &FigNode,
        frame: &FramePayload,
        symbol: &FigNode,
        group: &NodeRef,
        depth: usize,
        abs: &Affine,
        nested: bool,
    ) -> Result<()> {
        let background =
            self.frame_background(root, frame, abs, group, !symbol.children.is_empty())?;
        // The expansion group is what draws the instance: it carries the instance's effects
        // (also kept on the binding layer's records for a document instance).
        let _ = nested;
        self.effects(root, group);
        let clip = background.and_then(|b| b.1);
        let children = symbol.children.clone();
        self.walk_children(
            &children,
            Parent::Node(group.clone()),
            false,
            depth,
            abs,
            clip,
        )?;
        Ok(())
    }

    /// Frame fill as a background `vector` layer (first child of the frame: bottom-most) and, when
    /// the frame clips its content, a clip whose region is that layer's geometry (a hidden
    /// clip-source layer when the frame has no fill). Geometry: the frame's decoded fill geometry
    /// (corner radii resolved), else its size rectangle. Returns the layer and the clip index.
    fn frame_background(
        &mut self,
        n: &FigNode,
        f: &FramePayload,
        abs: &Affine,
        frame: &NodeRef,
        has_children: bool,
    ) -> Result<Option<(String, Option<usize>)>> {
        let painted = !n.fills.is_empty();
        let clip = f.mask_disabled == Some(false) && has_children;
        if !painted && !clip {
            return Ok(None);
        }
        let layer_id = self.mint.mint("SLYR");
        let mut path_ids = self.geometry(n, abs, &layer_id);
        if path_ids.is_empty()
            && let Some(p) = n.size.and_then(|s| rect_path(abs, wide(s.x), wide(s.y)))
        {
            let path_id = self.mint.mint("SVPT");
            path_ids.push(path_id.clone());
            self.paths.push(FigVectorPath {
                path_id,
                layer_id: layer_id.clone(),
                geometry: p,
            });
        }
        if path_ids.is_empty() {
            self.lose("native.frame_background", "frame_geometry_missing");
            if clip {
                self.lose("native.clips_content", "frame_clip_geometry_missing");
            }
            return Ok(None);
        }
        self.layers.push(StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: layer_id.clone(),
            name: n.name.clone().unwrap_or_default(),
            parent: Parent::Node(frame.clone()),
            kind: "vector".into(),
            payload: Payload::Vector { path_ids },
            visible: painted,
            appearance: None,
            text_frame: None,
        });
        let node = NodeRef {
            node_kind: NodeKind::Layer,
            node_id: layer_id.clone(),
        };
        self.order.push(node.clone());
        if painted {
            self.style_rows(n, node, true, false, abs)?;
        }
        let clip_index = if clip {
            let id = format!("fig-clip-{}", self.clips.len());
            self.clips.push((id, layer_id.clone(), Vec::new()));
            Some(self.clips.len().saturating_sub(1))
        } else {
            None
        };
        self.backgrounds.push(FrameBackground {
            source: n.id,
            frame: frame.clone(),
            layer_id: layer_id.clone(),
            painted,
            clip_id: clip_index.and_then(|i| self.clips.get(i)).map(|c| c.0.clone()),
        });
        Ok(Some((layer_id, clip_index)))
    }

    /// Lines and runs of a text layer from Figma's stored layout (baselines in document px),
    /// recorded for drawing. Text whose transform is not a pure translation, or without a stored
    /// layout that matches its characters, gets no lines (typed loss).
    fn text_layout(
        &mut self,
        n: &FigNode,
        t: &TextPayload,
        abs: &Affine,
        layer_id: &str,
        story_id: &str,
    ) {
        let text = &t.characters;
        let len = text.len() as u64;
        let offsets: Vec<u64> = text
            .char_indices()
            .map(|(i, _)| i as u64)
            .chain(std::iter::once(len))
            .collect();
        let chars = offsets.len().saturating_sub(1);
        let at = |k: u32| offsets.get(k as usize).copied().unwrap_or(len);
        let translation =
            abs[0] == 1.0 && abs[1] == 0.0 && abs[3] == 0.0 && abs[4] == 1.0;
        let mut lines = Vec::new();
        let place = |x: f32, y: f32| [tidy(abs[2] + wide(x)), tidy(abs[5] + wide(y))];
        if !translation {
            self.lose("native.text.layout", "text_transform_not_translation");
        } else {
            match &t.derived {
                Some(d) if !d.lines.is_empty() => {
                    let covered = d.lines.last().map_or(0, |l| l.end) as usize;
                    let ordered = d.lines.windows(2).all(|w| w[0].end <= w[1].first);
                    if covered == chars && ordered {
                        for l in &d.lines {
                            lines.push(FigTextLine {
                                start: at(l.first),
                                end: at(l.end),
                                baseline_px: place(l.x, l.y),
                            });
                        }
                    } else if d.lines.len() == 1
                        && !text.contains('\n')
                        && matches!(t.align_horizontal.as_deref(), None | Some("LEFT"))
                    {
                        // An overridden one-line, left-aligned text keeps its line origin.
                        let l = d.lines[0];
                        self.lose("native.text.layout", "text_layout_reused_for_new_characters");
                        lines.push(FigTextLine {
                            start: 0,
                            end: len,
                            baseline_px: place(l.x, l.y),
                        });
                    } else {
                        self.lose("native.text.layout", "text_layout_mismatch");
                    }
                }
                _ => self.lose("native.text.layout", "text_layout_not_derived"),
            }
        }
        let glyph_size = |s: u32, e: u32| {
            t.derived.as_ref().and_then(|d| {
                d.glyphs
                    .iter()
                    .find(|g| g.first >= s && g.first < e)
                    .map(|g| g.font_size)
            })
        };
        let fill_of = |fills: &[Paint]| {
            fills.iter().find_map(|p| match &p.kind {
                PaintKind::Solid { color: Some(c) } if p.visible != Some(false) => Some([
                    wide(c.r),
                    wide(c.g),
                    wide(c.b),
                    tidy(wide(c.a) * wide(p.opacity.unwrap_or(1.0))),
                ]),
                _ => None,
            })
        };
        let mut runs = Vec::new();
        let base;
        let spans: &[TextRun] = if t.runs.is_empty() && !text.is_empty() {
            base = [TextRun {
                start: 0,
                end: u32::try_from(chars).unwrap_or(u32::MAX),
                style_id: 0,
                font_family: t.font_family.clone(),
                font_style: t.font_style.clone(),
                font_size: t.font_size,
                fills: n.fills.clone(),
            }];
            &base
        } else {
            &t.runs
        };
        for r in spans {
            let derived = glyph_size(r.start, r.end);
            if let (Some(d), Some(s)) = (derived, r.font_size)
                && (d - s).abs() > f32::EPSILON
            {
                self.lose("native.text.font_size", "text_size_from_derived_layout");
            }
            let fills = if r.fills.is_empty() { &n.fills } else { &r.fills };
            runs.push(FigTextRunSpec {
                start: at(r.start),
                end: at(r.end),
                family: r.font_family.clone(),
                style: r.font_style.clone(),
                size_px: derived.or(r.font_size).map(wide),
                fill_rgba: fill_of(fills),
            });
        }
        // Glyph outlines: em-unit commands scaled by the glyph's font size at its pen position in
        // node space (Figma's glyph space is y-up: GLYPH_Y), then the node transform. Only when the
        // stored glyphs belong to these characters (same count as the stored lines cover).
        let mut glyphs = Vec::new();
        if let (Some(d), true) = (&t.derived, translation && !lines.is_empty()) {
            let covered = d.lines.last().map_or(0, |l| l.end) as usize;
            if covered == chars {
                for g in &d.glyphs {
                    let start = at(g.first);
                    let path = g.commands.as_ref().filter(|c| !c.is_empty()).and_then(|raw| {
                        // Figma glyph blobs open with a Close (no current point yet): a Close
                        // outside a subpath carries no geometry and is dropped.
                        let mut open = false;
                        let cmds: Vec<crate::nodes::PathCommand> = raw
                            .iter()
                            .filter(|c| match c {
                                crate::nodes::PathCommand::MoveTo { .. } => {
                                    open = true;
                                    true
                                }
                                crate::nodes::PathCommand::Close => std::mem::replace(&mut open, false),
                                _ => open,
                            })
                            .copied()
                            .collect();
                        if cmds.is_empty() {
                            return None;
                        }
                        let fs = wide(g.font_size);
                        let m = [fs, 0.0, abs[2] + wide(g.x), 0.0, GLYPH_Y * fs, abs[5] + wide(g.y)];
                        path_from_commands(&cmds, &m, hsk_studio_nib::FillRule::NonZero)
                    });
                    glyphs.push(FigGlyphOutline { start, path });
                }
            } else {
                self.lose("native.text.glyphs", "glyph_outlines_not_for_these_characters");
            }
        }
        self.text_layouts.push(FigTextLayout {
            layer_id: layer_id.to_owned(),
            story_id: story_id.to_owned(),
            source: n.id,
            lines,
            runs,
            glyphs,
        });
    }

    /// Walks children in order; masks, clips and boolean operands follow each child's
    /// representative node. Returns the child layer ids (boolean operands).
    fn walk_children(
        &mut self,
        children: &[usize],
        parent: Parent,
        canvas: bool,
        depth: usize,
        abs: &Affine,
        clip: Option<usize>,
    ) -> Result<Vec<String>> {
        let doc = self.doc;
        // A mask node masks every later sibling in the same parent (Figma stacking order:
        // ascending position = bottom to top).
        let mut open_mask: Option<usize> = None;
        let mut operands: Vec<String> = Vec::new();
        for &c in children {
            let Some(r) = self.walk(c, parent.clone(), canvas, depth.saturating_add(1), abs)?
            else {
                continue;
            };
            self.current = Some(doc.nodes[c].id);
            if r.node_kind == NodeKind::Layer {
                operands.push(r.node_id.clone());
                if let Some(entry) = clip.and_then(|k| self.clips.get_mut(k)) {
                    entry.2.push(r.node_id.clone());
                }
            }
            if doc.nodes[c].mask == Some(true) {
                if r.node_kind == NodeKind::Layer {
                    open_mask = Some(self.mask_plans.len());
                    self.mask_plans.push((r.node_id, Vec::new()));
                } else {
                    self.lose("native.mask", "mask_on_artboard_no_folio_home");
                }
            } else if let Some(m) = open_mask {
                if r.node_kind == NodeKind::Layer {
                    if let Some(plan) = self.mask_plans.get_mut(m) {
                        plan.1.push(r.node_id);
                    }
                } else {
                    self.lose("native.mask", "masked_artboard_no_folio_home");
                }
            }
        }
        Ok(operands)
    }

    /// Maps one node and its subtree. Returns the node standing for it among its parent's
    /// children (`None`: nothing emitted).
    fn walk(
        &mut self,
        idx: usize,
        parent: Parent,
        parent_is_canvas: bool,
        depth: usize,
        parent_abs: &Affine,
    ) -> Result<Option<NodeRef>> {
        self.check()?;
        if depth > 64 {
            self.lose("native.depth", "nesting_too_deep");
            return Ok(None);
        }
        let doc = self.doc;
        let source = &doc.nodes[idx];
        self.current = Some(source.id);
        let patched = self.patched(source);
        let n: &FigNode = patched.as_ref().unwrap_or(source);
        let in_expansion = !self.exp.is_empty();
        let abs = compose(parent_abs, n.transform.as_ref());
        let mut baked = false;
        let mut bool_op: Option<StudioBooleanOperation> = None;
        let mut bool_outline: Vec<String> = Vec::new();
        self.current = Some(n.id);
        let ty = n.type_name.as_str();
        let name = n.name.clone().unwrap_or_default();
        if NON_LAYER_KINDS.contains(&ty) {
            // Variable records, built after the walk (`variables`).
            return Ok(None);
        }
        if ty == "INSTANCE" && in_expansion {
            // An instance inside an expansion is drawn by its own expansion group only.
            return self.expand_instance(n, parent, depth, &abs, None);
        }
        let is_artboard = parent_is_canvas
            && matches!(ty, "FRAME" | "SECTION")
            && !matches!(&n.payload, NodePayload::Frame(f) if f.is_state_group == Some(true));
        let node_ref;
        let mut binding: Option<String> = None;
        if ty == "CANVAS" {
            let layer_id = self.mint.mint("SLYR");
            let internal = matches!(
                n.payload,
                NodePayload::Canvas {
                    internal_only: Some(true),
                    ..
                }
            );
            self.layers.push(StudioLayer {
                schema_id: LayerSchema::V1,
                layer_id: layer_id.clone(),
                name,
                parent: Parent::Root(()),
                kind: "group".into(),
                payload: Payload::Group,
                visible: n.visible.unwrap_or(true) && !internal,
                appearance: None,
                text_frame: None,
            });
            node_ref = NodeRef {
                node_kind: NodeKind::Layer,
                node_id: layer_id,
            };
        } else if is_artboard {
            let artboard_id = self.mint.mint("SART");
            let (x, y) = match n.transform {
                Some(t) if t.m00 == 1.0 && t.m01 == 0.0 && t.m10 == 0.0 && t.m11 == 1.0 => {
                    (wide(t.m02), wide(t.m12))
                }
                Some(t) => {
                    self.lose(
                        "native.artboard_transform",
                        "artboard_transform_not_axis_aligned",
                    );
                    (wide(t.m02), wide(t.m12))
                }
                None => (0.0, 0.0),
            };
            let size = n
                .size
                .map_or((0.0, 0.0), |s| (wide(s.x).max(0.0), wide(s.y).max(0.0)));
            if n.visible == Some(false) {
                self.lose(
                    "native.artboard_visible",
                    "artboard_visibility_no_folio_home",
                );
            }
            if n.opacity.is_some_and(|o| (o - 1.0).abs() >= f32::EPSILON)
                || n.blend_mode.as_deref().is_some_and(|b| b != "PASS_THROUGH")
            {
                self.lose(
                    "native.artboard_appearance",
                    "artboard_appearance_no_folio_home",
                );
            }
            self.artboards.push(StudioArtboard {
                schema_id: ArtboardSchema::V1,
                artboard_id: artboard_id.clone(),
                name,
                parent: parent.clone(),
                bounds: Bounds {
                    x: px(x),
                    y: px(y),
                    width: px(size.0),
                    height: px(size.1),
                },
            });
            node_ref = NodeRef {
                node_kind: NodeKind::Artboard,
                node_id: artboard_id,
            };
        } else {
            let layer_id = self.mint.mint("SLYR");
            if ty == "INSTANCE" {
                let instance_id = self.mint.mint("SCIN");
                let mut l = instance_layer(&layer_id, &name, parent.clone(), &instance_id)
                    .map_err(|_| FigError::NativeRejected(hsk_studio_folio::Code::InvalidId))?;
                l.visible = n.visible.unwrap_or(true);
                l.appearance = self.appearance(n, false);
                self.layers.push(l);
                self.instances.push((idx, instance_id));
                binding = Some(layer_id.clone());
            } else {
                let (kind, payload, group) = if CONTAINER_KINDS.contains(&ty) || ty == "DOCUMENT" {
                    if let NodePayload::Boolean { operation } = &n.payload {
                        bool_op = match operation.as_deref() {
                            Some("UNION") => Some(StudioBooleanOperation::Union),
                            Some("INTERSECT") => Some(StudioBooleanOperation::Intersect),
                            Some("SUBTRACT") => Some(StudioBooleanOperation::Subtract),
                            Some("EXCLUDE") => Some(StudioBooleanOperation::Exclude),
                            _ => {
                                self.lose("native.boolean_operation", "boolean_operation_unknown");
                                None
                            }
                        };
                        if bool_op.is_some() {
                            // Cached outline: the decoded result geometry of the node.
                            bool_outline = self.geometry(n, &abs, &layer_id);
                            baked = !bool_outline.is_empty();
                        }
                    }
                    ("group".to_owned(), Payload::Group, true)
                } else if LEAF_KINDS.contains(&ty) {
                    let path_ids = self.geometry(n, &abs, &layer_id);
                    baked = !path_ids.is_empty();
                    ("vector".to_owned(), Payload::Vector { path_ids }, false)
                } else if let NodePayload::Text(text) = &n.payload {
                    let story_id = self.mint.mint("STXT");
                    self.story(text, story_id.clone(), &n.fills);
                    self.text_layout(n, text, &abs, &layer_id, &story_id);
                    ("text".to_owned(), Payload::Text { story_id }, false)
                } else {
                    // Every kind without a Folio layer kind.
                    self.lose(
                        format!("native.node={ty}"),
                        "layer_kind_preserved_unsupported",
                    );
                    let kind = format!("fig_{}", ty.to_ascii_lowercase());
                    (
                        kind.clone(),
                        Payload::Unsupported {
                            kind,
                            schema_id: "fig-kiwi.NodeChange".into(),
                            encoded_bytes: Vec::new(),
                            reason: "no Folio layer kind for this Figma node kind in this slice"
                                .into(),
                        },
                        false,
                    )
                };
                let mut appearance = self.appearance(n, group);
                if bool_op.is_some() {
                    // A boolean result is one shape (composite draws it as an isolated leaf), so
                    // it composites Normal, not Pass Through.
                    let a = appearance.get_or_insert(LayerAppearance {
                        opacity: 100.0,
                        fill_opacity: 100.0,
                        blend_mode: BLEND_NORMAL,
                        clipped: false,
                        isolation: Isolation::Isolated,
                        mask: None,
                    });
                    if a.blend_mode == BLEND_PASS_THROUGH {
                        a.blend_mode = BLEND_NORMAL;
                        a.isolation = Isolation::Isolated;
                    }
                }
                self.layers.push(StudioLayer {
                    schema_id: LayerSchema::V1,
                    layer_id: layer_id.clone(),
                    name,
                    parent: parent.clone(),
                    kind,
                    payload,
                    visible: n.visible.unwrap_or(true),
                    appearance,
                    text_frame: None,
                });
                if ty == "SYMBOL" && !in_expansion {
                    self.symbols.push((idx, layer_id.clone()));
                }
                if !in_expansion
                    && matches!(&n.payload, NodePayload::Frame(f) if f.is_state_group == Some(true))
                {
                    self.sets.push((idx, layer_id.clone()));
                }
            }
            node_ref = NodeRef {
                node_kind: NodeKind::Layer,
                node_id: layer_id,
            };
        }
        self.record(n.id, &node_ref);
        // STU-PRO-128 flow starting points (on the page) and STU-PRO-138 overflow (on the frame).
        if let Some(name) = &n.flow_start {
            match (&self.page, in_expansion) {
                (Some(page), false) => {
                    let page = page.clone();
                    self.flow_starts.push((page, node_ref.node_id.clone(), name.clone(), n.flow_position.clone()));
                }
                _ => self.lose("native.flow_start", "flow_start_outside_page"),
            }
        }
        if let NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } =
            &n.payload
            && let Some(dir) = f.scroll_direction.as_deref()
        {
            let overflow_direction = match dir {
                "NONE" => Some(StudioOverflowDirection::None),
                "HORIZONTAL" => Some(StudioOverflowDirection::Horizontal),
                "VERTICAL" => Some(StudioOverflowDirection::Vertical),
                "BOTH" => Some(StudioOverflowDirection::Both),
                other => {
                    self.lose(format!("native.scroll_direction={other}"), "scroll_direction_unknown");
                    None
                }
            };
            if let Some(overflow_direction) = overflow_direction {
                self.frame_overflow.push(StudioFrameOverflow { node: node_ref.clone(), overflow_direction });
            }
        }
        if let NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } =
            &n.payload
        {
            self.frame_overlay(f, &node_ref);
        }
        let is_container = ty == "CANVAS" || is_artboard || CONTAINER_KINDS.contains(&ty);
        // Frame fill as a background layer and frame clip (artboards, frames, components).
        let mut background = None;
        if is_container
            && let NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } = &n.payload
        {
            background = self.frame_background(n, f, &abs, &node_ref, !n.children.is_empty())?;
            baked |= background.is_some();
        }
        self.unmapped(n, is_artboard, baked);
        self.effects(n, &node_ref);
        if ty != "CANVAS" {
            if background.is_some() {
                // The fill rows live on the background layer; strokes stay on the frame.
                self.style_rows(n, node_ref.clone(), false, true, &abs)?;
            } else {
                self.style(n, node_ref.clone(), &abs)?;
            }
        }
        if let NodePayload::Frame(f)
        | NodePayload::Symbol { frame: f, .. }
        | NodePayload::Instance { frame: f, .. } = &n.payload
        {
            let container = is_artboard || CONTAINER_KINDS.contains(&ty);
            self.auto_layout(f, &node_ref, container);
        }
        let children = n.children.clone();
        if !is_container && !children.is_empty() {
            for &c in &children {
                self.current = Some(doc.nodes[c].id);
                self.lose("native.children_of_leaf", "child_of_non_container_dropped");
            }
        }
        if binding.is_some() {
            // A document instance: the binding layer carries the component records; the
            // expansion group (following sibling) draws it and stands for it among siblings.
            let drawn = self.expand_instance(n, parent, depth, &abs, binding)?;
            return Ok(Some(drawn.unwrap_or(node_ref)));
        }
        if !is_container {
            return Ok(Some(node_ref));
        }
        let group_layer = parent_layer_id(&Parent::Node(node_ref.clone()));
        let clip = background.and_then(|b| b.1);
        let outer_page = if ty == "CANVAS" { self.page.replace(node_ref.clone()) } else { None };
        let operands = self.walk_children(
            &children,
            Parent::Node(node_ref.clone()),
            ty == "CANVAS",
            depth,
            &abs,
            clip,
        );
        if ty == "CANVAS" {
            self.page = outer_page;
        }
        let operands = operands?;
        if let (Some(operation), Some(layer_id)) = (bool_op, group_layer) {
            self.current = Some(n.id);
            if operands.is_empty() {
                self.paths.retain(|p| p.layer_id != layer_id);
                self.lose("native.boolean_operation", "boolean_without_operands");
            } else {
                self.boolean_groups.push(StudioBooleanGroup {
                    layer_id,
                    operation,
                    operands,
                    fill_geometry_path_ids: bool_outline,
                });
            }
        }
        Ok(Some(node_ref))
    }
}

fn design(node: NodeRef) -> StudioNodeDesign {
    StudioNodeDesign {
        node,
        auto_layout: None,
        child_layout: None,
        size_clamps: None,
        constraints: None,
        reactions: Vec::new(),
        component_property_references: BTreeMap::new(),
        explicit_variable_modes: BTreeMap::new(),
        style_refs: BTreeMap::new(),
    }
}

fn folio_error(d: hsk_studio_folio::Diagnostic) -> FigError {
    FigError::NativeRejected(d.code)
}

fn aggregate(loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>) -> Vec<NodeLoss> {
    loss.into_iter()
        .map(|((path, code), (count, first))| NodeLoss {
            path,
            code,
            count,
            first,
        })
        .collect()
}

/// Maps a decoded document into a Folio document plus design records. The DOCUMENT root becomes
/// the Folio document itself; each CANVAS a root `group` layer; each FRAME/SECTION directly on a
/// canvas an artboard; other containers `group` layers; shapes `vector` layers (paths not mapped);
/// paints one PAINT style per node. Both outputs are validated with the caller's budget and
/// resolver before they are returned.
pub fn to_native(
    doc: &FigDocument,
    cfg: &NativeConfig,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<NativeProposal> {
    cancel.check().map_err(|_| FigError::Canceled)?;
    let document_id = mint.mint("SDOC");
    let mut m = Mapper {
        doc,
        mint,
        cancel,
        layers: Vec::new(),
        stories: Vec::new(),
        paths: Vec::new(),
        fx: Vec::new(),
        image_paint_records: Vec::new(),
        boolean_groups: Vec::new(),
        pending_image: None,
        text_styles: Vec::new(),
        text_style_ids: HashMap::new(),
        mask_plans: Vec::new(),
        artboards: Vec::new(),
        styles: Vec::new(),
        designs: Vec::new(),
        id_map: Vec::new(),
        loss: BTreeMap::new(),
        current: None,
        symbols: Vec::new(),
        sets: Vec::new(),
        instances: Vec::new(),
        components: Vec::new(),
        component_sets: Vec::new(),
        component_instances: Vec::new(),
        collections: Vec::new(),
        variables: Vec::new(),
        record_map: Vec::new(),
        order: Vec::new(),
        symbol_index: doc
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.type_name == "SYMBOL")
            .map(|(i, n)| (n.id, i))
            .collect(),
        exp: Vec::new(),
        expansion_budget: MAX_EXPANDED_NODES.min(
            u64::try_from(cfg.budget.nodes)
                .unwrap_or(u64::MAX)
                .saturating_sub(u64::try_from(doc.nodes.len()).unwrap_or(u64::MAX).saturating_mul(2)),
        ),
        expanded_total: 0,
        expanded: Vec::new(),
        expansions: Vec::new(),
        backgrounds: Vec::new(),
        clips: Vec::new(),
        text_layouts: Vec::new(),
        page: None,
        flow_starts: Vec::new(),
        frame_overflow: Vec::new(),
        frame_overlay: Vec::new(),
    };
    for &root in &doc.roots {
        let r = &doc.nodes[root];
        m.current = Some(r.id);
        if r.type_name == "DOCUMENT" {
            for &c in &r.children {
                m.walk(c, Parent::Root(()), false, 0, &IDENTITY)?;
            }
        } else {
            m.walk(root, Parent::Root(()), false, 0, &IDENTITY)?;
        }
    }
    m.components()?;
    m.variables();
    m.reactions();
    let Mapper {
        layers,
        stories,
        paths,
        fx,
        image_paint_records,
        boolean_groups,
        text_styles,
        mask_plans,
        components,
        component_sets,
        component_instances,
        collections,
        variables,
        record_map,
        artboards,
        styles,
        designs,
        id_map,
        loss,
        order,
        expanded,
        expansions,
        backgrounds,
        clips,
        text_layouts,
        flow_starts,
        frame_overflow,
        frame_overlay,
        ..
    } = m;
    let mut layers = layers;
    let mut loss = loss;
    // Mask sources are not drawn themselves (Figma masks hide their own shape): hidden layers,
    // like the source layers of `hsk-studio-interop-vector` masks.
    for (source, targets) in &mask_plans {
        if targets.is_empty() {
            let slot = loss
                .entry(("native.mask".to_owned(), "mask_without_targets"))
                .or_insert((0, None));
            slot.0 = slot.0.saturating_add(1);
        } else if let Some(l) = layers.iter_mut().find(|l| &l.layer_id == source) {
            l.visible = false;
        }
    }
    let booleans: std::collections::HashSet<String> =
        boolean_groups.iter().map(|g| g.layer_id.clone()).collect();
    // A document instance is drawn by its expansion group; its binding layer has no graph form.
    let drawn_by_expansion: std::collections::HashSet<String> = expansions
        .iter()
        .filter_map(|e| e.binding_layer_id.clone())
        .collect();
    let built = crate::graph::build(&layers, &artboards, &order, &booleans, &drawn_by_expansion);
    for ((path, code), count) in built.loss {
        let slot = loss.entry((path.to_owned(), code)).or_insert((0, None));
        slot.0 = slot.0.saturating_add(count);
    }
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: document_id.clone(),
        revision: 1,
        geometry_unit: GeometryUnit::Px,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards,
        page_spreads: Vec::new(),
        layers,
        graph: built.graph,
        sequences: Vec::new(),
    };
    let snapshot =
        validate_document(document, cfg.budget, cancel, cfg.resolver).map_err(folio_error)?;
    let mut design_records = StudioDesignRecords::new(document_id);
    design_records.paint_styles = styles;
    design_records.nodes = designs;
    design_records.components = components;
    design_records.component_sets = component_sets;
    design_records.component_instances = component_instances;
    design_records.variable_collections = collections;
    design_records.variables = variables;
    // Flow starting points per page in their stored order (fractional index, byte order).
    let mut flow_starts = flow_starts;
    flow_starts.sort_by(|a, b| a.0.node_id.cmp(&b.0.node_id).then(a.3.cmp(&b.3)));
    for (page, node_id, name, _) in flow_starts {
        match design_records.prototype_pages.iter_mut().find(|p| p.page == page) {
            Some(p) => p.flow_starting_points.push(StudioFlowStart { node_id, name }),
            None => design_records.prototype_pages.push(StudioPrototypePage {
                page,
                flow_starting_points: vec![StudioFlowStart { node_id, name }],
                prototype_start_node_id: None,
            }),
        }
    }
    design_records.frame_overflow = frame_overflow;
    design_records.frame_overlay = frame_overlay;
    // Type styles: one `StudioTypeStyle` per STYS id; stories resolve against the set.
    let mut type_styles = StudioTypeStyleRecords::new(snapshot.document().document_id.clone());
    let mut unit_lost = false;
    for (i, st) in text_styles.iter().enumerate() {
        let (style, lost) = st.to_folio(i);
        unit_lost |= lost;
        type_styles.styles.push(style);
    }
    if unit_lost {
        let slot = loss
            .entry((
                "native.text.units".to_owned(),
                "text_unit_no_type_style_home",
            ))
            .or_insert((0, None));
        slot.0 = slot.0.saturating_add(1);
    }
    validate_type_style_records(
        &type_styles,
        snapshot.document(),
        cfg.budget,
        cancel,
        cfg.resolver,
    )
    .map_err(folio_error)?;
    let style_resolver = TypeStyleResolver {
        styles: &type_styles,
        inner: cfg.resolver,
    };
    let mut unresolved_text_styles = Vec::new();
    for story in &stories {
        let issues =
            validate_text_story(story, cfg.budget, cancel, &style_resolver).map_err(folio_error)?;
        unresolved_text_styles.extend(issues.into_iter().map(|i| i.target));
        check_story_style_scopes(story, &type_styles).map_err(folio_error)?;
    }
    validate_design_records(
        &design_records,
        snapshot.document(),
        cfg.budget,
        cancel,
        cfg.resolver,
    )
    .map_err(folio_error)?;
    let doc_id = snapshot.document().document_id.clone();
    let mut vector_records = StudioVectorRecords::new(doc_id.clone());
    // A clip or mask on a pass-through group applies to each of its children (the group has no
    // own result; composite refuses a mask on a pass-through layer): the attachment is pushed down
    // to the children, which composite onto the same backdrop, so the coverage is the same.
    let doc_layers = &snapshot.document().layers;
    let pass_through = |id: &str| {
        doc_layers.iter().any(|l| {
            l.layer_id == id
                && l.kind == "group"
                && l.appearance
                    .as_ref()
                    .is_none_or(|a| a.blend_mode == BLEND_PASS_THROUGH)
        })
    };
    let push_down = |target: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(target.to_owned(), 0usize)];
        while let Some((t, depth)) = stack.pop() {
            if depth < 64 && pass_through(&t) {
                let kids: Vec<(String, usize)> = doc_layers
                    .iter()
                    .filter(|l| matches!(&l.parent, Parent::Node(p) if p.node_id == t))
                    .map(|l| (l.layer_id.clone(), depth.saturating_add(1)))
                    .collect();
                stack.extend(kids.into_iter().rev());
            } else {
                out.push(t);
            }
        }
        out
    };
    let mut attached: BTreeMap<String, ()> = BTreeMap::new();
    for (i, (source, targets)) in mask_plans.iter().enumerate() {
        if targets.is_empty() {
            continue;
        }
        let targets: Vec<String> = targets.iter().flat_map(|t| push_down(t)).collect();
        let mask_id = format!("fig-mask-{i}");
        vector_records.masks.push(StudioVectorMask {
            mask_id: mask_id.clone(),
            source_layer_id: source.clone(),
            // Figma's maskType is not decoded: alpha is its default reading (UNVERIFIED).
            mode: StudioMaskMode::Alpha,
            mask_clip: false,
            invert_mask: false,
            region: None,
        });
        for t in targets {
            if attached.insert(t.clone(), ()).is_some() {
                let slot = loss
                    .entry(("native.mask".to_owned(), "second_mask_on_layer_dropped"))
                    .or_insert((0, None));
                slot.0 = slot.0.saturating_add(1);
                continue;
            }
            vector_records.attachments.push(StudioMaskAttachment {
                layer_id: t.clone(),
                clip_id: None,
                mask_id: Some(mask_id.clone()),
            });
        }
    }
    // Frame clips: the region is the frame's background (or hidden clip-source) geometry; every
    // child layer of the frame carries the clip (beside a mask it may already have).
    let mut backgrounds = backgrounds;
    // Deepest frames first: a frame's clip is recorded before the walk reaches its children, so
    // reverse order visits inner frames before outer ones.
    for (clip_id, source, targets) in clips.iter().rev() {
        if targets.is_empty() {
            let slot = loss
                .entry(("native.clips_content".to_owned(), "frame_clip_without_layers"))
                .or_insert((0, None));
            slot.0 = slot.0.saturating_add(1);
            for b in backgrounds.iter_mut().filter(|b| b.clip_id.as_ref() == Some(clip_id)) {
                b.clip_id = None;
            }
            continue;
        }
        vector_records.clips.push(StudioVectorClip {
            clip_id: clip_id.clone(),
            source_layer_id: source.clone(),
            clip_rule: StudioClipRule::NonZero,
        });
        // Inner clips are attached first (deepest frames first), so a pushed-down outer clip that
        // meets a layer already clipped keeps the inner one (typed loss, see below).
        let targets: Vec<String> = targets.iter().flat_map(|t| push_down(t)).collect();
        for t in &targets {
            match vector_records.attachments.iter_mut().find(|a| &a.layer_id == t) {
                Some(a) if a.clip_id.is_none() => a.clip_id = Some(clip_id.clone()),
                Some(_) => {
                    let slot = loss
                        .entry(("native.clips_content".to_owned(), "outer_clip_dropped_inner_kept"))
                        .or_insert((0, None));
                    slot.0 = slot.0.saturating_add(1);
                }
                None => vector_records.attachments.push(StudioMaskAttachment {
                    layer_id: t.clone(),
                    clip_id: Some(clip_id.clone()),
                    mask_id: None,
                }),
            }
        }
    }
    vector_records.image_paints = image_paint_records;
    vector_records.boolean_groups = boolean_groups;
    validate_vector_records(
        &vector_records,
        snapshot.document(),
        Some(&design_records),
        cfg.budget,
        cancel,
    )
    .map_err(folio_error)?;
    let mut raster_records = StudioRasterRecords::new(doc_id);
    raster_records.layers = fx;
    validate_raster_records(
        &raster_records,
        snapshot.document(),
        cfg.budget,
        cancel,
        cfg.resolver,
    )
    .map_err(folio_error)?;
    Ok(NativeProposal {
        snapshot,
        design: design_records,
        stories,
        paths,
        type_styles,
        unresolved_text_styles,
        raster_records,
        vector_records,
        id_map,
        record_map,
        mapping_loss: aggregate(loss),
        decode_loss: doc.loss.clone(),
        editable: EditableStatus::NotProven,
        frame_backgrounds: backgrounds,
        expansions,
        expanded,
        text_layouts,
    })
}

fn node_kind_token(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::Layer => "layer",
        NodeKind::Artboard => "artboard",
        NodeKind::PageSpread => "page_spread",
    }
}

fn loss_json(list: &[NodeLoss]) -> Value {
    Value::Array(
        list.iter()
            .map(|l| {
                json!({"path": l.path, "code": l.code, "count": l.count,
                       "first": l.first.map(|s| s.to_string())})
            })
            .collect(),
    )
}

/// What every receipt of this mapping states it does not prove.
pub const NATIVE_NOT_PROVEN: &[&str] = &[
    "editable_import (names, ids, mapped records, vector geometry, effects, masks, image paints, boolean groups and type styles survive a save and reopen; nothing renders them yet and no Figma render parity is shown)",
    "effect_conventions (shadow size = radius px, offset as distance + Photoshop light angle, colour alpha as opacity percent, inner-shadow spread as choke percent; not compared with Figma rendering)",
    "mask_semantics (a mask node masks its later siblings, written as ALPHA masks with the source layer hidden; Figma maskType is not decoded)",
    "text_styles (STYS type styles carry family, style, size, solid fill, line height, letter spacing and horizontal alignment; vertical alignment and auto-resize are losses; no font file is resolved)",
    "blur_radius (the Figma blur radius is written as the Folio radius unchanged; whether it is the Gaussian sigma or twice it is unverified)",
    "image_transform (Folio transform = inverse of the Figma image transform, identity when absent; scaling factor 1, rotation 0; scale modes FILL FIT CROP/STRETCH TILE; row stand-in is a grey solid; unverified against Figma rendering)",
    "boolean_groups (operands = all child layers bottom first; cached outline = the decoded result geometry, in canvas px)",
    "geometry_space (paths are baked into canvas px with every node transform composed; artboard-local coordinates are path minus artboard bounds)",
    "figma_render_parity (prototype-pages page 1 renders within measured tolerances of the Operator PNGs with Inter Regular granted; basic-shapes and the components page are not at parity: strokes, gradients, image paints, effect styles and Inter Bold are not drawn)",
    "instance_expansion (instances draw through an expansion group of the definition's children with overrides mapped; sublayer sizes come from derivedSymbolData only, resized shapes keep the definition geometry; uniform scale other than 1 is not applied)",
    "frame_background (frame fills are a bottom-most vector layer of the frame geometry; frame strokes stay on the frame and are not drawn; Figma's order of frame strokes over children is unverified)",
    "frame_clip (clip content = a NONZERO vector clip of the frame geometry on every child layer, pushed down through pass-through groups; a layer inside nested clipping frames keeps only the inner clip, typed loss)",
    "strokes (caps NONE/ROUND/SQUARE = butt/round/projecting, marker caps lost; an odd dash pattern is written twice; miter limit as stored; only fixture strokes without caps or dashes are compared with Figma rendering)",
    "effect_styles (styleIdForEffect resolves to a local style node whose effects replace the node's; a group with effects is made isolated Normal; library styles are losses)",
    "text_layout (lines and baselines come from Figma's stored derivedTextData; glyph sizes from its laid-out glyphs; fonts must be granted by the caller per family and style)",
    "gradient_transform (Figma paint transform inverted, scaled by the node size and placed by the node's absolute transform into document px; rendered within 1.5/255 of the fixture PNG for its identity paint transform only; angular and diamond gradients have no Folio geometry)",
    "stacking_order (children in ascending position = bottom to top, read from one fixture PNG)",
    "text_direction (stories are written left-to-right; Figma stores AUTO directionality)",
    "auto_layout_fields (stackHorizontalPadding/stackVerticalPadding read as left/top; sizing, alignment, wrap and border inclusion are defaults)",
    "transition_easing (Figma easing names are written as the Studio-declared STU-PRO-120 preset tuples of folio::EASING_PRESETS, CUSTOM_CUBIC keeps its stored numbers; whether Figma's presets use the same numbers is unverified; springs are written linear with a loss entry)",
    "prototype_flow (flow starting points are ordered by the stored fractional position per page; overflow direction from scrollDirection; AFTER_TIMEOUT delay = transitionTimeout seconds x 1000)",
];

impl NativeProposal {
    /// Records, receipt and original import entries for a native package (`hsk-studio-package`
    /// `ImportEntry`): records = `{design_records, id_map}`, receipt = provenance + every loss.
    pub fn import_entries(&self, import: &FigImport, original: &[u8]) -> Result<Vec<ImportEntry>> {
        let enc = |_| FigError::NativeRejected(hsk_studio_folio::Code::InvalidJson);
        let records = json!({
            "records": "fig_native",
            "records_version": 1,
            "design_records": serde_json::to_value(&self.design).map_err(enc)?,
            "text_stories": serde_json::to_value(&self.stories).map_err(enc)?,
            // Geometry as Nib's canonical JSON text (shortest round-trip digits, no f64 drift).
            "vector_paths": self.paths.iter().map(|p| json!({
                "path_id": p.path_id,
                "layer_id": p.layer_id,
                "geometry": p.geometry.to_canonical_json(),
            })).collect::<Vec<_>>(),
            "type_style_records": serde_json::to_value(&self.type_styles).map_err(enc)?,
            "raster_records": serde_json::to_value(&self.raster_records).map_err(enc)?,
            "vector_records": serde_json::to_value(&self.vector_records).map_err(enc)?,
            "id_map": self.id_map.iter().map(|e| json!({
                "source": e.source.to_string(),
                "node_kind": node_kind_token(&e.node.node_kind),
                "node_id": e.node.node_id,
            })).collect::<Vec<_>>(),
            "record_map": self.record_map.iter().map(|e| json!({
                "source": e.source.to_string(),
                "record_id": e.record_id,
            })).collect::<Vec<_>>(),
        });
        let original_entry = ImportEntry::new(ImportRole::Original, original.to_vec());
        let receipt = json!({
            "receipt": "import",
            "receipt_version": 1,
            "source_format": "fig",
            "importer": "hsk-studio-interop-fig",
            "source": {"sha256": hex(&import.source_sha256), "byte_len": original.len(),
                       "entry": original_entry.name(), "fig_kind": import.kind.label(),
                       "fig_version": import.version},
            "layers": self.snapshot.document().layers.len(),
            "artboards": self.snapshot.document().artboards.len(),
            "paint_styles": self.design.paint_styles.len(),
            "text_stories": self.stories.len(),
            "vector_paths": self.paths.len(),
            "type_styles": self.type_styles.styles.len(),
            "unresolved_text_styles": self.unresolved_text_styles.len(),
            "effect_stacks": self.raster_records.layers.len(),
            "masks": self.vector_records.masks.len(),
            "boolean_groups": self.vector_records.boolean_groups.len(),
            // Image bytes stay byte-identical in the original entry; identity of each part here.
            "image_paints": self.vector_records.image_paints.iter().map(|p| {
                let StudioImageSource::ImportOriginal { part, content_digest, .. } = &p.image
                else {
                    return json!({"style_id": p.style_id, "row": p.row});
                };
                let held = import.container.part(part);
                json!({"style_id": p.style_id, "row": p.row, "part": part,
                       "part_bytes": held.map(|x| x.bytes.len()),
                       "part_sha256": held.map(|x| hex(&x.sha256)),
                       "recorded_sha256": content_digest.digest})
            }).collect::<Vec<_>>(),
            "mapped_nodes": self.id_map.len(),
            "mapping_loss": loss_json(&self.mapping_loss),
            "decode_loss": loss_json(&self.decode_loss),
            "editable": self.editable.label(),
            "not_proven": NATIVE_NOT_PROVEN,
        });
        Ok(vec![
            ImportEntry::new(
                ImportRole::Records,
                serde_json::to_vec(&records).map_err(enc)?,
            ),
            ImportEntry::new(
                ImportRole::Receipt,
                serde_json::to_vec(&receipt).map_err(enc)?,
            ),
            original_entry,
        ])
    }
}

/// Records read back from a package's records entry.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeRecords {
    pub design: StudioDesignRecords,
    pub stories: Vec<StudioTextStory>,
    pub paths: Vec<FigVectorPath>,
    pub type_styles: StudioTypeStyleRecords,
    pub raster_records: StudioRasterRecords,
    pub vector_records: StudioVectorRecords,
    pub id_map: Vec<IdMapEntry>,
    pub record_map: Vec<RecordMapEntry>,
}

/// Parses a records import entry written by [`NativeProposal::import_entries`].
pub fn read_native_records(bytes: &[u8]) -> Result<NativeRecords> {
    let bad = FigError::NativeRejected(hsk_studio_folio::Code::InvalidJson);
    let v: Value = serde_json::from_slice(bytes).map_err(|_| bad.clone())?;
    if v["records"] != "fig_native" || v["records_version"] != 1 {
        return Err(bad);
    }
    let design: StudioDesignRecords =
        serde_json::from_value(v["design_records"].clone()).map_err(|_| bad.clone())?;
    let stories: Vec<StudioTextStory> =
        serde_json::from_value(v["text_stories"].clone()).map_err(|_| bad.clone())?;
    let mut paths = Vec::new();
    for e in v["vector_paths"].as_array().ok_or_else(|| bad.clone())? {
        let (Some(path_id), Some(layer_id), Some(text)) = (
            e["path_id"].as_str(),
            e["layer_id"].as_str(),
            e["geometry"].as_str(),
        ) else {
            return Err(bad);
        };
        let geometry = OwnedPath::from_canonical_json(text, &PathLimits::default())
            .map_err(|_| bad.clone())?;
        paths.push(FigVectorPath {
            path_id: path_id.to_owned(),
            layer_id: layer_id.to_owned(),
            geometry,
        });
    }
    let type_styles: StudioTypeStyleRecords =
        serde_json::from_value(v["type_style_records"].clone()).map_err(|_| bad.clone())?;
    let raster_records: StudioRasterRecords =
        serde_json::from_value(v["raster_records"].clone()).map_err(|_| bad.clone())?;
    let vector_records: StudioVectorRecords =
        serde_json::from_value(v["vector_records"].clone()).map_err(|_| bad.clone())?;
    let mut id_map = Vec::new();
    for e in v["id_map"].as_array().ok_or_else(|| bad.clone())? {
        let (Some(source), Some(kind), Some(node_id)) = (
            e["source"].as_str(),
            e["node_kind"].as_str(),
            e["node_id"].as_str(),
        ) else {
            return Err(bad);
        };
        let (s, l) = source.split_once(':').ok_or_else(|| bad.clone())?;
        let source = SourceId {
            session: s.parse().map_err(|_| bad.clone())?,
            local: l.parse().map_err(|_| bad.clone())?,
        };
        let node_kind = match kind {
            "layer" => NodeKind::Layer,
            "artboard" => NodeKind::Artboard,
            "page_spread" => NodeKind::PageSpread,
            _ => return Err(bad),
        };
        id_map.push(IdMapEntry {
            source,
            node: NodeRef {
                node_kind,
                node_id: node_id.to_owned(),
            },
        });
    }
    let mut record_map = Vec::new();
    for e in v["record_map"].as_array().ok_or_else(|| bad.clone())? {
        let (Some(source), Some(record_id)) = (e["source"].as_str(), e["record_id"].as_str())
        else {
            return Err(bad);
        };
        let (s, l) = source.split_once(':').ok_or_else(|| bad.clone())?;
        record_map.push(RecordMapEntry {
            source: SourceId {
                session: s.parse().map_err(|_| bad.clone())?,
                local: l.parse().map_err(|_| bad.clone())?,
            },
            record_id: record_id.to_owned(),
        });
    }
    Ok(NativeRecords {
        design,
        stories,
        paths,
        type_styles,
        raster_records,
        vector_records,
        id_map,
        record_map,
    })
}

/// Index of a proposal's id map by source id.
pub fn id_index(map: &[IdMapEntry]) -> HashMap<SourceId, &NodeRef> {
    map.iter().map(|e| (e.source, &e.node)).collect()
}

fn parent_layer_id(p: &Parent) -> Option<String> {
    match p {
        Parent::Node(r) if r.node_kind == NodeKind::Layer => Some(r.node_id.clone()),
        _ => None,
    }
}
