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
//! Not mapped (typed losses): geometry of containers and artboards (no path home on groups), the
//! boolean result outline, stroke outlines (derived from the path plus the stroke row), transforms
//! and sizes of nodes without emitted geometry, image paints, effects, masks, text fonts and run
//! styles, paint-to-variable bindings, flow starts, scroll behaviour. Editable import stays
//! NOT_PROVEN (`FigImport::editable`).
#![deny(clippy::arithmetic_side_effects)]
use crate::container::hex;
use crate::effects::effect;
use crate::geometry::{
    Affine, IDENTITY, compose, fill_rule, line_path, network_paths, path_from_commands,
};
use crate::import::{FigImport, SourceId};
use crate::nodes::VariableValue;
use crate::nodes::{FigDocument, FigNode, NodeLoss, NodePayload, Paint, PaintKind};
use crate::semantic_decode::EditableStatus;
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::instance_layer;
use hsk_studio_folio::{
    ArtboardSchema, BLEND_NORMAL, BLEND_PASS_THROUGH, Bounds, Budget, DocumentSchema, GeometryUnit,
    GradientInterpolation, GradientSpread, GraphSchema, Isolation, LayerAppearance, LayerSchema,
    Length, NodeKind, NodeRef, Parent, Payload, ProfileBinding, Resolver, Snapshot, StrokeAlign,
    StrokeCap, StrokeJoin, StudioAppearanceRow, StudioArtboard, StudioColor, StudioDesignRecords,
    StudioDocument, StudioGradient, StudioGradientGeometry, StudioGradientStop, StudioLayer,
    StudioLayerGraph, StudioNodeDesign, StudioPaint, StudioPaintStyle, StudioPublishing,
    StudioStrokeStyle, StyleKind, StyleRegistrySchema, validate_design_records, validate_document,
};
use hsk_studio_folio::{
    AutoLayoutSchema, AxisSizingMode, CounterAxisAlign, CounterAxisAlignContent, CubicBezier,
    LayoutMode, LayoutWrap, MAX_DURATION_MS, Navigation, PADDING_MAX, PrimaryAxisAlign,
    SPACING_MAX, SPACING_MIN, StudioAction, StudioAutoLayout, StudioEasing, StudioMode,
    StudioReaction, StudioRgba, StudioTransition, StudioTrigger, StudioVariable,
    StudioVariableCollection, TransitionType, VARIABLE_SCOPES, VariableCollectionSchema,
    VariableSchema, VariableType,
};
use hsk_studio_folio::{
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioTextStory,
    TextStorySchema, validate_text_story,
};
use hsk_studio_folio::{
    StudioComponent, StudioComponentInstance, StudioComponentSet, StudioOverrideRecord, StudioValue,
};
use hsk_studio_folio::{
    StudioEffectStack, StudioMaskAttachment, StudioMaskMode, StudioRasterLayerRecord,
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

/// An IMAGE paint of a node. Folio's paint model has no image kind (declared gap in `paint.rs`),
/// so no appearance row is written; this record binds the node to the image part, whose bytes stay
/// byte-identical inside the package's original entry (receipt: `image_paints`).
#[derive(Clone, Debug, PartialEq)]
pub struct FigImagePaint {
    pub source: SourceId,
    pub node: NodeRef,
    /// `fills` or `strokes`.
    pub what: &'static str,
    /// Index in the node's paint list.
    pub ordinal: usize,
    /// Hex image hash as stored by Figma.
    pub hash: String,
    /// Container entry `images/<hash>` when present.
    pub part: Option<String>,
    pub scale_mode: Option<String>,
    pub original_size: Option<(u32, u32)>,
    pub opacity: f64,
    pub visible: bool,
}

/// The font and layout values bound to one `STYS` id of a text story (`kind` is `character` for a
/// range binding, `paragraph` for a paragraph binding). Folio stores only the `STYS` reference of
/// a story (the style record is host-resolved, `SCHEMA_STUDIO_TYPE_STYLE`), so the values live in
/// the records entry; the host creates the style records from them. Font references are by
/// Figma family and style name; no font file is resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct FigTextStyle {
    pub style_ref: String,
    pub kind: &'static str,
    pub font_family: Option<String>,
    pub font_style: Option<String>,
    pub font_size: Option<f64>,
    /// First visible solid fill (r, g, b, a in 0..=1).
    pub fill: Option<[f64; 4]>,
    pub line_height: Option<(f64, String)>,
    pub letter_spacing: Option<(f64, String)>,
    pub align_horizontal: Option<String>,
    pub align_vertical: Option<String>,
    pub auto_resize: Option<String>,
}

impl FigTextStyle {
    fn to_json(&self) -> Value {
        json!({
            "style_ref": self.style_ref, "kind": self.kind,
            "font_family": self.font_family, "font_style": self.font_style,
            "font_size": self.font_size, "fill": self.fill,
            "line_height": self.line_height.as_ref().map(|(v, u)| json!([v, u])),
            "letter_spacing": self.letter_spacing.as_ref().map(|(v, u)| json!([v, u])),
            "align_horizontal": self.align_horizontal,
            "align_vertical": self.align_vertical,
            "auto_resize": self.auto_resize,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        let pair = |k: &str| {
            let a = v[k].as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_str()?.to_owned()))
        };
        let fill = match v["fill"].as_array() {
            Some(a) => {
                let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                Some(<[f64; 4]>::try_from(n).ok()?)
            }
            None => None,
        };
        Some(Self {
            style_ref: s("style_ref")?,
            kind: match v["kind"].as_str()? {
                "character" => "character",
                "paragraph" => "paragraph",
                _ => return None,
            },
            font_family: s("font_family"),
            font_style: s("font_style"),
            font_size: v["font_size"].as_f64(),
            fill,
            line_height: pair("line_height"),
            letter_spacing: pair("letter_spacing"),
            align_horizontal: s("align_horizontal"),
            align_vertical: s("align_vertical"),
            auto_resize: s("auto_resize"),
        })
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
    /// Font and layout values behind the `STYS` bindings of the stories.
    pub text_styles: Vec<FigTextStyle>,
    /// `STYS` bindings the caller's resolver did not resolve (host-owned style records pending).
    pub unresolved_text_styles: Vec<String>,
    /// Shadow effects per layer (`hsk.studio.raster_records@1`), validated.
    pub raster_records: StudioRasterRecords,
    /// Masks and their attachments (`hsk.studio.vector_records@1`), validated.
    pub vector_records: StudioVectorRecords,
    /// IMAGE paints (no Folio paint kind; see [`FigImagePaint`]).
    pub image_paints: Vec<FigImagePaint>,
    pub id_map: Vec<IdMapEntry>,
    /// Figma nodes that became design records rather than document nodes (variables).
    pub record_map: Vec<RecordMapEntry>,
    /// Mapping losses only (`native.*` paths), aggregated like [`NodeLoss`].
    pub mapping_loss: Vec<NodeLoss>,
    /// The decode losses of the source [`FigDocument`].
    pub decode_loss: Vec<NodeLoss>,
    pub editable: EditableStatus,
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

struct Mapper<'a> {
    doc: &'a FigDocument,
    mint: &'a mut dyn IdMint,
    cancel: &'a CancellationToken,
    layers: Vec<StudioLayer>,
    stories: Vec<StudioTextStory>,
    paths: Vec<FigVectorPath>,
    fx: Vec<StudioRasterLayerRecord>,
    image_paints: Vec<FigImagePaint>,
    text_styles: Vec<FigTextStyle>,
    /// Canonical style JSON (without the id) -> minted `STYS` id.
    text_style_ids: HashMap<String, String>,
    /// Node index -> mapped node, for sibling mask resolution.
    node_refs: HashMap<usize, NodeRef>,
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

    fn lose(&mut self, path: impl Into<String>, code: &'static str) {
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
                // gradient space to the layer: node size x inverse(paint transform). UNVERIFIED
                // beyond the identity transform of the fixture.
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
                let t = [
                    inv[0] * w,
                    inv[1] * h,
                    inv[2] * w,
                    inv[3] * h,
                    inv[4] * w,
                    inv[5] * h,
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
            PaintKind::Image { .. } => {
                // Bound and carried (FigImagePaint, original entry) but not an appearance row.
                self.lose(
                    format!("native.{what}.IMAGE"),
                    "image_paint_not_a_folio_paint",
                );
                None
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
    fn style(&mut self, n: &FigNode, node: NodeRef) -> Result<()> {
        let mut rows = Vec::new();
        for p in &n.fills {
            if let Some((paint, opacity)) = self.paint(p, n.size, "fills") {
                let blend_mode = self.paint_blend(p, "fills");
                rows.push(StudioAppearanceRow::Fill {
                    paint,
                    opacity,
                    blend_mode,
                    visible: p.visible.unwrap_or(true),
                });
            }
        }
        if !n.strokes.is_empty() {
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
            for p in &n.strokes {
                if let Some((paint, opacity)) = self.paint(p, n.size, "strokes") {
                    let blend_mode = self.paint_blend(p, "strokes");
                    rows.push(StudioAppearanceRow::Stroke {
                        paint,
                        // Cap and miter limit: Figma defaults (no cap, limit 4) where the file has
                        // none; the decoder reports any stored strokeCap/miterLimit as a loss.
                        stroke: StudioStrokeStyle {
                            weight: px(weight),
                            cap: StrokeCap::Butt,
                            join,
                            align,
                            miter_limit: 4.0,
                            dash: Vec::new(),
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
        let paragraph_style = if t.line_height.is_some()
            || t.letter_spacing.is_some()
            || t.align_horizontal.is_some()
            || t.align_vertical.is_some()
            || t.auto_resize.is_some()
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
                align_vertical: t.align_vertical.clone(),
                auto_resize: t.auto_resize.clone(),
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
        let key = st.to_json().to_string();
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
            align_vertical: None,
            auto_resize: None,
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
                                    let transition_type = match t {
                                        "DISSOLVE" => TransitionType::Dissolve,
                                        "SMART_ANIMATE" => TransitionType::SmartAnimate,
                                        "SCROLL_ANIMATE" => TransitionType::ScrollAnimate,
                                        other => {
                                            self.lose(
                                                format!("native.interactions.transition={other}"),
                                                "transition_not_mapped",
                                            );
                                            continue;
                                        }
                                    };
                                    let ms = a.duration.map_or(0.0, |d| wide(d) * 1000.0).round();
                                    if !(0.0..=f64::from(MAX_DURATION_MS)).contains(&ms) {
                                        self.lose(
                                            "native.interactions.duration",
                                            "transition_duration_out_of_range",
                                        );
                                        continue;
                                    }
                                    let bezier = match a.easing.as_deref() {
                                        Some("LINEAR") => (0.0, 0.0, 1.0, 1.0),
                                        other => {
                                            self.lose(
                                                format!(
                                                    "native.interactions.easing={}",
                                                    other.unwrap_or("?")
                                                ),
                                                "easing_curve_unverified_linear_used",
                                            );
                                            (0.0, 0.0, 1.0, 1.0)
                                        }
                                    };
                                    Some(StudioTransition {
                                        transition_type,
                                        direction: None,
                                        match_layers: None,
                                        easing: StudioEasing {
                                            easing_function_cubic_bezier: Some(CubicBezier {
                                                x1: bezier.0,
                                                y1: bezier.1,
                                                x2: bezier.2,
                                                y2: bezier.3,
                                            }),
                                            easing_function_spring: None,
                                        },
                                        duration: ms as u32,
                                    })
                                }
                            };
                            actions.push(StudioAction::Node {
                                destination_id,
                                navigation,
                                transition,
                                preserve_scroll_position: false,
                                overlay_relative_position: None,
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
        let mut list = Vec::new();
        for (i, e) in n.effects.iter().enumerate() {
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
        }
    }

    /// Records the image fill/stroke paints of a node: the Folio paint model has no image kind
    /// (declared gap), so the binding lives in the records entry; the bytes stay in the original.
    fn image_paints(&mut self, n: &FigNode, node: &NodeRef) {
        for (what, list) in [("fills", &n.fills), ("strokes", &n.strokes)] {
            for (ordinal, p) in list.iter().enumerate() {
                let PaintKind::Image {
                    hash,
                    part,
                    scale_mode,
                    original_size,
                    ..
                } = &p.kind
                else {
                    continue;
                };
                self.image_paints.push(FigImagePaint {
                    source: n.id,
                    node: node.clone(),
                    what,
                    ordinal,
                    hash: hash.iter().map(|b| format!("{b:02x}")).collect(),
                    part: part.clone(),
                    scale_mode: scale_mode.clone(),
                    original_size: *original_size,
                    opacity: percent(p.opacity.unwrap_or(1.0)),
                    visible: p.visible.unwrap_or(true),
                });
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
        if n.flow_start.is_some() {
            self.lose("native.flow_start", "flow_start_not_mapped");
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
            NodePayload::Boolean { .. } => {
                self.lose("native.boolean_operation", "boolean_operation_not_mapped")
            }
            NodePayload::Frame(f)
            | NodePayload::Symbol { frame: f, .. }
            | NodePayload::Instance { frame: f, .. } => {
                if f.corner_radius.is_some() || f.corner_radii.is_some() {
                    self.lose("native.corner_radii", "frame_corner_radii_no_folio_home");
                }
                if f.mask_disabled == Some(false) {
                    self.lose("native.clips_content", "frame_clip_not_mapped");
                }
                if f.scroll_direction.is_some() {
                    self.lose("native.scroll_direction", "scroll_not_mapped");
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

    fn walk(
        &mut self,
        idx: usize,
        parent: Parent,
        parent_is_canvas: bool,
        depth: usize,
        parent_abs: &Affine,
    ) -> Result<()> {
        self.check()?;
        if depth > 64 {
            self.lose("native.depth", "nesting_too_deep");
            return Ok(());
        }
        let doc = self.doc;
        let n = &doc.nodes[idx];
        let abs = compose(parent_abs, n.transform.as_ref());
        let mut baked = false;
        self.current = Some(n.id);
        let ty = n.type_name.as_str();
        let name = n.name.clone().unwrap_or_default();
        if NON_LAYER_KINDS.contains(&ty) {
            // Variable records, built after the walk (`variables`).
            return Ok(());
        }
        let is_artboard = parent_is_canvas
            && matches!(ty, "FRAME" | "SECTION")
            && !matches!(&n.payload, NodePayload::Frame(f) if f.is_state_group == Some(true));
        let node_ref;
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
                parent,
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
                let mut l = instance_layer(&layer_id, &name, parent, &instance_id)
                    .map_err(|_| FigError::NativeRejected(hsk_studio_folio::Code::InvalidId))?;
                l.visible = n.visible.unwrap_or(true);
                l.appearance = self.appearance(n, false);
                self.layers.push(l);
                self.instances.push((idx, instance_id));
            } else {
                let (kind, payload, group) = if CONTAINER_KINDS.contains(&ty) || ty == "DOCUMENT" {
                    ("group".to_owned(), Payload::Group, true)
                } else if LEAF_KINDS.contains(&ty) {
                    let path_ids = self.geometry(n, &abs, &layer_id);
                    baked = !path_ids.is_empty();
                    ("vector".to_owned(), Payload::Vector { path_ids }, false)
                } else if let NodePayload::Text(text) = &n.payload {
                    let story_id = self.mint.mint("STXT");
                    self.story(text, story_id.clone(), &n.fills);
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
                let appearance = self.appearance(n, group);
                self.layers.push(StudioLayer {
                    schema_id: LayerSchema::V1,
                    layer_id: layer_id.clone(),
                    name,
                    parent,
                    kind,
                    payload,
                    visible: n.visible.unwrap_or(true),
                    appearance,
                    text_frame: None,
                });
                if ty == "SYMBOL" {
                    self.symbols.push((idx, layer_id.clone()));
                }
                if matches!(&n.payload, NodePayload::Frame(f) if f.is_state_group == Some(true)) {
                    self.sets.push((idx, layer_id.clone()));
                }
            }
            node_ref = NodeRef {
                node_kind: NodeKind::Layer,
                node_id: layer_id,
            };
        }
        self.id_map.push(IdMapEntry {
            source: n.id,
            node: node_ref.clone(),
        });
        self.unmapped(n, is_artboard, baked);
        self.node_refs.insert(idx, node_ref.clone());
        self.effects(n, &node_ref);
        self.image_paints(n, &node_ref);
        if ty != "CANVAS" {
            self.style(n, node_ref.clone())?;
        }
        if let NodePayload::Frame(f)
        | NodePayload::Symbol { frame: f, .. }
        | NodePayload::Instance { frame: f, .. } = &n.payload
        {
            let container = is_artboard || CONTAINER_KINDS.contains(&ty);
            self.auto_layout(f, &node_ref, container);
        }
        let is_container = ty == "CANVAS" || is_artboard || CONTAINER_KINDS.contains(&ty);
        let children = n.children.clone();
        if !is_container && !children.is_empty() {
            for &c in &children {
                self.current = Some(doc.nodes[c].id);
                self.lose("native.children_of_leaf", "child_of_non_container_dropped");
            }
            return Ok(());
        }
        let parent = Parent::Node(node_ref);
        // A mask node masks every later sibling in the same parent (Figma stacking order:
        // ascending position = bottom to top).
        let mut open_mask: Option<usize> = None;
        for c in children {
            self.walk(
                c,
                parent.clone(),
                ty == "CANVAS",
                depth.saturating_add(1),
                &abs,
            )?;
            let Some(r) = self.node_refs.get(&c).cloned() else {
                continue;
            };
            self.current = Some(doc.nodes[c].id);
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
        Ok(())
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
        image_paints: Vec::new(),
        text_styles: Vec::new(),
        text_style_ids: HashMap::new(),
        node_refs: HashMap::new(),
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
        image_paints,
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
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: Vec::new(),
            edges: Vec::new(),
            outputs: Vec::new(),
        },
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
    let mut unresolved_text_styles = Vec::new();
    for story in &stories {
        let issues =
            validate_text_story(story, cfg.budget, cancel, cfg.resolver).map_err(folio_error)?;
        unresolved_text_styles.extend(issues.into_iter().map(|i| i.target));
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
    let mut attached: BTreeMap<String, ()> = BTreeMap::new();
    for (i, (source, targets)) in mask_plans.iter().enumerate() {
        if targets.is_empty() {
            continue;
        }
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
        text_styles,
        unresolved_text_styles,
        raster_records,
        vector_records,
        image_paints,
        id_map,
        record_map,
        mapping_loss: aggregate(loss),
        decode_loss: doc.loss.clone(),
        editable: EditableStatus::NotProven,
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
    "editable_import (names, ids, mapped records, vector geometry, shadow effects and masks survive a save and reopen; image paints are bindings only, blurs and text fonts are losses)",
    "effect_conventions (shadow size = radius px, offset as distance + Photoshop light angle, colour alpha as opacity percent, inner-shadow spread as choke percent; not compared with Figma rendering)",
    "mask_semantics (a mask node masks its later siblings, written as ALPHA masks with the source layer hidden; Figma maskType is not decoded)",
    "text_styles (font family/style/size, solid fill, line height, letter spacing and alignment are carried as STYS-bound values in the records entry; Folio has no style record, so the host resolves the STYS ids, and no font file is resolved)",
    "geometry_space (paths are baked into canvas px with every node transform composed; artboard-local coordinates are path minus artboard bounds)",
    "figma_render_parity (no comparison against Figma's rendering)",
    "gradient_transform (Figma paint transform inverted and scaled by the node size; checked only for the identity transform of the fixtures)",
    "stacking_order (children in ascending position = bottom to top, read from one fixture PNG)",
    "text_direction (stories are written left-to-right; Figma stores AUTO directionality)",
    "auto_layout_fields (stackHorizontalPadding/stackVerticalPadding read as left/top; sizing, alignment, wrap and border inclusion are defaults)",
    "transition_easing (only LINEAR has a verified curve; other easings are written linear with a loss entry)",
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
            "text_styles": self.text_styles.iter().map(FigTextStyle::to_json).collect::<Vec<_>>(),
            "raster_records": serde_json::to_value(&self.raster_records).map_err(enc)?,
            "vector_records": serde_json::to_value(&self.vector_records).map_err(enc)?,
            "image_paints": self.image_paints.iter().map(|p| json!({
                "source": p.source.to_string(),
                "node_kind": node_kind_token(&p.node.node_kind),
                "node_id": p.node.node_id,
                "what": p.what,
                "ordinal": p.ordinal,
                "hash": p.hash,
                "part": p.part,
                "scale_mode": p.scale_mode,
                "original_size": p.original_size.map(|(w, h)| json!([w, h])),
                "opacity": p.opacity,
                "visible": p.visible,
            })).collect::<Vec<_>>(),
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
            "text_styles": self.text_styles.len(),
            "unresolved_text_styles": self.unresolved_text_styles.len(),
            "effect_stacks": self.raster_records.layers.len(),
            "masks": self.vector_records.masks.len(),
            // Image bytes stay byte-identical in the original entry; identity of each part here.
            "image_paints": self.image_paints.iter().map(|p| {
                let part = p.part.as_deref().and_then(|n| import.container.part(n));
                json!({"node_id": p.node.node_id, "hash": p.hash, "part": p.part,
                       "part_bytes": part.map(|x| x.bytes.len()),
                       "part_sha256": part.map(|x| hex(&x.sha256))})
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
    pub text_styles: Vec<FigTextStyle>,
    pub raster_records: StudioRasterRecords,
    pub vector_records: StudioVectorRecords,
    pub image_paints: Vec<FigImagePaint>,
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
    let mut text_styles = Vec::new();
    for e in v["text_styles"].as_array().ok_or_else(|| bad.clone())? {
        text_styles.push(FigTextStyle::from_json(e).ok_or_else(|| bad.clone())?);
    }
    let raster_records: StudioRasterRecords =
        serde_json::from_value(v["raster_records"].clone()).map_err(|_| bad.clone())?;
    let vector_records: StudioVectorRecords =
        serde_json::from_value(v["vector_records"].clone()).map_err(|_| bad.clone())?;
    let mut image_paints = Vec::new();
    for e in v["image_paints"].as_array().ok_or_else(|| bad.clone())? {
        let (Some(source), Some(kind), Some(node_id), Some(what), Some(hash)) = (
            e["source"].as_str(),
            e["node_kind"].as_str(),
            e["node_id"].as_str(),
            e["what"].as_str(),
            e["hash"].as_str(),
        ) else {
            return Err(bad);
        };
        let (s, l) = source.split_once(':').ok_or_else(|| bad.clone())?;
        let node_kind = match kind {
            "layer" => NodeKind::Layer,
            "artboard" => NodeKind::Artboard,
            _ => return Err(bad),
        };
        let size = |i: usize| {
            e["original_size"][i]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
        };
        image_paints.push(FigImagePaint {
            source: SourceId {
                session: s.parse().map_err(|_| bad.clone())?,
                local: l.parse().map_err(|_| bad.clone())?,
            },
            node: NodeRef {
                node_kind,
                node_id: node_id.to_owned(),
            },
            what: match what {
                "fills" => "fills",
                "strokes" => "strokes",
                _ => return Err(bad),
            },
            ordinal: e["ordinal"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| bad.clone())?,
            hash: hash.to_owned(),
            part: e["part"].as_str().map(str::to_owned),
            scale_mode: e["scale_mode"].as_str().map(str::to_owned),
            original_size: size(0).zip(size(1)),
            opacity: e["opacity"].as_f64().ok_or_else(|| bad.clone())?,
            visible: e["visible"].as_bool().ok_or_else(|| bad.clone())?,
        });
    }
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
        text_styles,
        raster_records,
        vector_records,
        image_paints,
        id_map,
        record_map,
    })
}

/// Index of a proposal's id map by source id.
pub fn id_index(map: &[IdMapEntry]) -> HashMap<SourceId, &NodeRef> {
    map.iter().map(|e| (e.source, &e.node)).collect()
}
