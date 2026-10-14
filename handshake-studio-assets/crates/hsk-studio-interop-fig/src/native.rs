//! Mapping of a decoded [`FigDocument`] into the Studio native document (Folio): pages, artboards,
//! layer tree, layer appearance, paint styles (fills and strokes), inline text stories and the node
//! design records that bind them. Only Folio's own types are produced (CX-CAP-001); anything without a Folio home is a
//! typed loss entry, never dropped silently (test: fig_native::real_native_losses_accounted).
//!
//! Identity: every Folio id comes from the caller's [`IdMint`]; the Figma source id of every mapped
//! node is kept in [`NativeProposal::id_map`] and in the records import entry, so a saved and
//! reopened package still names the source node of every layer (test:
//! fig_native::real_native_package_round_trip).
//!
//! Not mapped here (typed losses): vector path geometry (Folio layers list `hsk.studio.vector_path@1`
//! ids whose geometry is owned by `hsk-studio-nib`; interop-fig has no edge to nib), layer
//! transforms of non-artboard nodes (Folio layers carry no transform), image paints, effects,
//! masks. Editable import stays NOT_PROVEN (`FigImport::editable`).
#![deny(clippy::arithmetic_side_effects)]
use crate::container::hex;
use crate::import::{FigImport, SourceId};
use crate::nodes::{FigDocument, FigNode, NodeLoss, NodePayload, Paint, PaintKind};
use crate::semantic_decode::EditableStatus;
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
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
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioTextStory,
    TextStorySchema, validate_text_story,
};
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
    pub id_map: Vec<IdMapEntry>,
    /// Mapping losses only (`native.*` paths), aggregated like [`NodeLoss`].
    pub mapping_loss: Vec<NodeLoss>,
    /// The decode losses of the source [`FigDocument`].
    pub decode_loss: Vec<NodeLoss>,
    pub editable: EditableStatus,
}

/// Figma blend mode name -> Folio `StudioBlendMode` discriminant (`hsk-studio-pigment`
/// `blend_math::StudioBlendMode`, values read 2026-10-14). `None`: no counterpart.
fn blend(name: &str) -> Option<u16> {
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
const CONTAINER_KINDS: &[&str] = &[
    "FRAME",
    "SECTION",
    "GROUP",
    "BOOLEAN_OPERATION",
    "SYMBOL",
    "INSTANCE",
];
/// Folio has no layer for these: they become design records (variables) in a later slice.
const NON_LAYER_KINDS: &[&str] = &["VARIABLE", "VARIABLE_SET"];

struct Mapper<'a> {
    doc: &'a FigDocument,
    mint: &'a mut dyn IdMint,
    cancel: &'a CancellationToken,
    layers: Vec<StudioLayer>,
    stories: Vec<StudioTextStory>,
    artboards: Vec<StudioArtboard>,
    styles: Vec<StudioPaintStyle>,
    designs: Vec<StudioNodeDesign>,
    id_map: Vec<IdMapEntry>,
    loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>,
    current: Option<SourceId>,
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
fn wide(v: f32) -> f64 {
    v.to_string().parse().unwrap_or_else(|_| f64::from(v))
}

/// A computed f64 kept to 12 significant digits, for the same JSON round-trip reason as [`wide`].
fn tidy(v: f64) -> f64 {
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
                self.lose(format!("native.{what}.IMAGE"), "image_paint_no_folio_home");
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
        let mut d = design(node);
        d.style_refs.insert("fill_style_id".into(), style_id);
        self.designs.push(d);
        Ok(())
    }

    /// Inline story of a TEXT node: paragraphs end after each `\n`, one character range per style
    /// run (byte offsets). Run fonts, sizes, line height, letter spacing and alignment have no
    /// Folio record (type styles are host-resolved `STYS` references): typed losses.
    fn story(&mut self, t: &crate::nodes::TextPayload, story_id: String) {
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
        let mut ranges = Vec::new();
        for r in &t.runs {
            let (a, b) = (at(r.start), at(r.end));
            if a < b {
                ranges.push((a, b));
            }
        }
        if ranges.is_empty() && len > 0 {
            ranges.push((0, len));
        }
        let fonts: std::collections::BTreeSet<_> = t
            .runs
            .iter()
            .map(|r| {
                (
                    r.font_family.clone(),
                    r.font_style.clone(),
                    r.font_size.map(f32::to_bits),
                )
            })
            .collect();
        if t.font_family.is_some() || t.font_size.is_some() {
            self.lose("native.text.font", "text_font_no_folio_home");
        }
        if fonts.len() > 1 {
            self.lose("native.text.run_styles", "text_run_style_no_folio_home");
        }
        if t.runs.iter().any(|r| r.fills != t.runs[0].fills) {
            self.lose("native.text.run_fills", "text_run_fill_no_folio_home");
        }
        for (field, present) in [
            ("line_height", t.line_height.is_some()),
            ("letter_spacing", t.letter_spacing.is_some()),
            (
                "align",
                t.align_horizontal.is_some() || t.align_vertical.is_some(),
            ),
            ("auto_resize", t.auto_resize.is_some()),
        ] {
            if present {
                self.lose(format!("native.text.{field}"), "text_layout_no_folio_home");
            }
        }
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
                    style_ref: None,
                    paragraph_direction: None,
                })
                .collect(),
            character_ranges: ranges
                .into_iter()
                .map(|(start, end)| StoryRange {
                    start,
                    end,
                    style_ref: None,
                    applied_language: None,
                })
                .collect(),
        });
    }

    /// Losses for decoded node data this slice does not map.
    fn unmapped(&mut self, n: &FigNode, artboard: bool) {
        if !artboard && n.type_name != "CANVAS" {
            if n.transform.is_some() {
                self.lose("native.transform", "layer_transform_no_folio_home");
            }
            if n.size.is_some() && n.type_name != "TEXT" {
                self.lose("native.size", "layer_size_no_folio_home");
            }
        }
        if !n.effects.is_empty() {
            for e in &n.effects {
                self.lose(format!("native.effects.{}", e.kind), "effect_no_folio_home");
            }
        }
        if n.mask == Some(true) {
            self.lose("native.mask", "mask_not_mapped");
        }
        if !n.fill_geometry.is_empty() || !n.stroke_geometry.is_empty() {
            self.lose("native.geometry", "vector_geometry_needs_nib_edge");
        }
        if !n.interactions.is_empty() {
            self.lose("native.interactions", "reactions_not_mapped");
        }
        if n.flow_start.is_some() {
            self.lose("native.flow_start", "flow_start_not_mapped");
        }
        match &n.payload {
            NodePayload::Rectangle { .. } => {
                self.lose("native.corner_radii", "shape_parameters_need_nib_edge")
            }
            NodePayload::Ellipse { .. } => {
                self.lose("native.arc", "shape_parameters_need_nib_edge")
            }
            NodePayload::Star { .. } => self.lose("native.star", "shape_parameters_need_nib_edge"),
            NodePayload::Vector { .. } => {
                self.lose("native.vector_network", "vector_geometry_needs_nib_edge")
            }
            NodePayload::Boolean { .. } => {
                self.lose("native.boolean_operation", "boolean_operation_not_mapped")
            }
            NodePayload::Frame(f) => {
                if f.corner_radius.is_some() || f.corner_radii.is_some() {
                    self.lose("native.corner_radii", "frame_corner_radii_no_folio_home");
                }
                if f.mask_disabled == Some(false) {
                    self.lose("native.clips_content", "frame_clip_not_mapped");
                }
                if f.auto_layout.is_some() {
                    self.lose("native.auto_layout", "auto_layout_not_mapped");
                }
                if f.scroll_direction.is_some() {
                    self.lose("native.scroll_direction", "scroll_not_mapped");
                }
                if !f.component_props.is_empty() || f.is_state_group == Some(true) {
                    self.lose("native.component_set", "components_not_mapped");
                }
            }
            NodePayload::Symbol { .. } => self.lose("native.symbol", "components_not_mapped"),
            NodePayload::Instance { .. } => self.lose("native.instance", "components_not_mapped"),
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
    ) -> Result<()> {
        self.check()?;
        if depth > 64 {
            self.lose("native.depth", "nesting_too_deep");
            return Ok(());
        }
        let doc = self.doc;
        let n = &doc.nodes[idx];
        self.current = Some(n.id);
        let ty = n.type_name.as_str();
        let name = n.name.clone().unwrap_or_default();
        if NON_LAYER_KINDS.contains(&ty) {
            self.lose(format!("native.node={ty}"), "variables_not_mapped");
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
            let (kind, payload, group) = if CONTAINER_KINDS.contains(&ty) || ty == "DOCUMENT" {
                ("group".to_owned(), Payload::Group, true)
            } else if LEAF_KINDS.contains(&ty) {
                (
                    "vector".to_owned(),
                    Payload::Vector {
                        path_ids: Vec::new(),
                    },
                    false,
                )
            } else if let NodePayload::Text(text) = &n.payload {
                let story_id = self.mint.mint("STXT");
                self.story(text, story_id.clone());
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
                        reason: "no Folio layer kind for this Figma node kind in this slice".into(),
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
            node_ref = NodeRef {
                node_kind: NodeKind::Layer,
                node_id: layer_id,
            };
        }
        self.id_map.push(IdMapEntry {
            source: n.id,
            node: node_ref.clone(),
        });
        self.unmapped(n, is_artboard);
        if ty != "CANVAS" {
            self.style(n, node_ref.clone())?;
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
        for c in children {
            self.walk(c, parent.clone(), ty == "CANVAS", depth.saturating_add(1))?;
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
        artboards: Vec::new(),
        styles: Vec::new(),
        designs: Vec::new(),
        id_map: Vec::new(),
        loss: BTreeMap::new(),
        current: None,
    };
    for &root in &doc.roots {
        let r = &doc.nodes[root];
        m.current = Some(r.id);
        if r.type_name == "DOCUMENT" {
            for &c in &r.children {
                m.walk(c, Parent::Root(()), false, 0)?;
            }
        } else {
            m.walk(root, Parent::Root(()), false, 0)?;
        }
    }
    let Mapper {
        layers,
        stories,
        artboards,
        styles,
        designs,
        id_map,
        loss,
        ..
    } = m;
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
    for story in &stories {
        validate_text_story(story, cfg.budget, cancel, cfg.resolver).map_err(folio_error)?;
    }
    validate_design_records(
        &design_records,
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
        id_map,
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
    "editable_import (only names, ids and the mapped records survive a save and reopen; geometry, transforms, effects, images and masks are losses)",
    "figma_render_parity (no comparison against Figma's rendering)",
    "gradient_transform (Figma paint transform inverted and scaled by the node size; checked only for the identity transform of the fixtures)",
    "stacking_order (children in ascending position = bottom to top, read from one fixture PNG)",
    "text_direction (stories are written left-to-right; Figma stores AUTO directionality)",
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
            "id_map": self.id_map.iter().map(|e| json!({
                "source": e.source.to_string(),
                "node_kind": node_kind_token(&e.node.node_kind),
                "node_id": e.node.node_id,
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
    pub id_map: Vec<IdMapEntry>,
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
    Ok(NativeRecords {
        design,
        stories,
        id_map,
    })
}

/// Index of a proposal's id map by source id.
pub fn id_index(map: &[IdMapEntry]) -> HashMap<SourceId, &NodeRef> {
    map.iter().map(|e| (e.source, &e.node)).collect()
}
