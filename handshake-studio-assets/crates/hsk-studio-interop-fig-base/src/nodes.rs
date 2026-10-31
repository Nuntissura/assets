//! Typed node tree over the generic Kiwi value tree of a `.fig` message: identity, parent/order,
//! common properties, per-kind payloads, paints, effects, path and vector-network blobs, text runs,
//! components/instances, variables and prototype interactions. Field names come from the file's
//! own schema (observed in three Figma desktop 126.10.7 files, `tests/fixtures`).
//!
//! Nothing is dropped silently: every field that is present but not interpreted, every value of an
//! unexpected type, unknown enum value, unresolvable blob, image or variable alias becomes a typed
//! [`NodeLoss`] entry (test: fig_nodes::real_fields_decoded_or_listed_as_loss). Decoding the tree
//! is not an editable import: mapping into the Studio native document is a later lane
//! (`FigImport::editable` stays NOT_PROVEN).
//!
//! Blob formats (path commands and vector networks): MIT community decoders (sketch-hq/fig2sketch
//! `vector_network.py`; open-pencil and OpenFig-org/openfig-core path commands 0 close, 1 move,
//! 2 line, 4 cubic); command 3 = quadratic (4 floats) is an inference proven only by exact blob
//! consumption on the fixture glyph blobs. All floats are f32 little-endian.
//!
//! GRID auto layout (lane SMB-FIG-GRID-2): a frame's `gridRows` / `gridColumns` (track ids in
//! fractional-index order), `gridRowsSizing` / `gridColumnsSizing` (min/max sizing per track id),
//! `gridRowGap` / `gridColumnGap`, `gridAutoTracks` and `gridReflowEnabled` decode into
//! [`AutoLayout::grid`]; a child's `gridRowAnchor` / `gridColumnAnchor` (track ids), spans and
//! alignments into a [`FigGridChild`] attached to its parent's [`FigGrid::children`] (the anchors name
//! the parent's tracks; a placement under a parent without grid fields is a loss). The names are the
//! ones the embedded schema of the three Operator fixtures declares (test fig_grid_decode); the schema
//! has no count fields (the counts are the track list lengths) and no items-positioning field.
#![deny(clippy::arithmetic_side_effects)]
use crate::container::{FigContainer, IMAGES_PREFIX, Limits};
use crate::import::{FigImport, SourceId};
use crate::kiwi::schema_and_message;
use crate::kiwi_value::{KiwiField, KiwiValue, decode_tree};
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

/// 2x3 affine matrix as stored (`m02`/`m12` are the translation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub m00: f32,
    pub m01: f32,
    pub m02: f32,
    pub m10: f32,
    pub m11: f32,
    pub m12: f32,
}

/// RGBA, components as stored (0..1 floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    MoveTo {
        x: f32,
        y: f32,
    },
    LineTo {
        x: f32,
        y: f32,
    },
    QuadTo {
        x1: f32,
        y1: f32,
        x: f32,
        y: f32,
    },
    CubicTo {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x: f32,
        y: f32,
    },
    Close,
}

/// One `fillGeometry`/`strokeGeometry` entry; `commands` is `None` when its blob did not decode
/// (a loss entry names why).
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub winding_rule: Option<String>,
    pub blob: Option<u32>,
    pub commands: Option<Vec<PathCommand>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetVertex {
    pub style: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetSegment {
    pub style: u32,
    pub start: u32,
    pub tangent_start: Vec2,
    pub end: u32,
    pub tangent_end: Vec2,
}

/// A filled region: `loops` are lists of segment indices; `nonzero` is bit 0 of the stored flags
/// (fig2sketch reading: set = NONZERO, clear = EVENODD); `style` is the remaining bits.
#[derive(Clone, Debug, PartialEq)]
pub struct NetRegion {
    pub style: u32,
    pub nonzero: bool,
    pub loops: Vec<Vec<u32>>,
}

/// Decoded `vectorNetworkBlob`: coordinates are in the node's `normalizedSize` space.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorNetwork {
    pub vertices: Vec<NetVertex>,
    pub segments: Vec<NetSegment>,
    pub regions: Vec<NetRegion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradientStop {
    pub position: f32,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaintKind {
    Solid {
        color: Option<Color>,
    },
    /// `gradient` is the stored paint type name (`GRADIENT_LINEAR`, `GRADIENT_RADIAL`, ...).
    Gradient {
        gradient: String,
        stops: Vec<GradientStop>,
        transform: Option<Matrix>,
    },
    /// `part` is the container entry `images/<hex(hash)>` when it exists; `data_blob` an inline
    /// blob index when the paint carries one.
    Image {
        hash: Vec<u8>,
        part: Option<String>,
        data_blob: Option<u32>,
        scale_mode: Option<String>,
        transform: Option<Matrix>,
        original_size: Option<(u32, u32)>,
    },
    /// A paint type this decoder does not interpret (loss `paint_type_unsupported`).
    Unsupported {
        type_name: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    pub kind: PaintKind,
    pub opacity: Option<f32>,
    pub visible: Option<bool>,
    pub blend_mode: Option<String>,
    /// `colorVar` alias: the VARIABLE node bound to this paint's colour.
    pub color_variable: Option<SourceId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    /// Stored effect type name (`DROP_SHADOW`, `INNER_SHADOW`, `FOREGROUND_BLUR`, ...).
    pub kind: String,
    pub offset: Option<Vec2>,
    pub radius: Option<f32>,
    pub spread: Option<f32>,
    pub color: Option<Color>,
    pub visible: Option<bool>,
    pub blend_mode: Option<String>,
    pub show_shadow_behind_node: Option<bool>,
}

/// Auto-layout fields under their stored names (no left/top reinterpretation).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoLayout {
    pub mode: String,
    pub spacing: Option<f32>,
    pub horizontal_padding: Option<f32>,
    pub vertical_padding: Option<f32>,
    pub padding_right: Option<f32>,
    pub padding_bottom: Option<f32>,
    /// The grid container fields (lane SMB-FIG-GRID-2), present when the node stores any of them
    /// (whatever `mode` is; mapping uses them only under `GRID`).
    pub grid: Option<FigGrid>,
}

/// One `GridTrackSizingFunction` (`type` as stored: `FLEX`, `FIXED`, `HUG`; `value` as stored).
#[derive(Clone, Debug, PartialEq)]
pub struct FigTrackSizing {
    pub kind: String,
    pub value: Option<f32>,
}

/// One `GridTrackSize`: the stored `minSizing` and `maxSizing` functions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FigTrackSize {
    pub min: Option<FigTrackSizing>,
    pub max: Option<FigTrackSizing>,
}

/// One grid track: a `gridRows` / `gridColumns` (`GUIDPositionMap`) entry and the size its
/// `gridRowsSizing` / `gridColumnsSizing` (`GUIDGridTrackSizeMap`) entry gives it (`None`: no entry).
#[derive(Clone, Debug, PartialEq)]
pub struct FigGridTrack {
    pub id: SourceId,
    pub position: String,
    pub size: Option<FigTrackSize>,
}

/// The grid placement a child stores (`gridRowAnchor` / `gridColumnAnchor` are track ids of the
/// parent's `gridRows` / `gridColumns`; spans and alignments as stored).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FigGridChild {
    pub row_anchor: Option<SourceId>,
    pub column_anchor: Option<SourceId>,
    pub row_span: Option<u32>,
    pub column_span: Option<u32>,
    pub vertical_align: Option<String>,
    pub horizontal_align: Option<String>,
}

/// The grid container fields of a frame. Tracks are in stored order (fractional-index `position`
/// byte order, then id); the schema has no count fields, the counts are the track list lengths.
/// `children` holds the placements of the node's children that store grid fields, in child order
/// (a child's grid fields are attached here, to the parent whose track ids they name).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FigGrid {
    pub rows: Vec<FigGridTrack>,
    pub columns: Vec<FigGridTrack>,
    pub row_gap: Option<f32>,
    pub column_gap: Option<f32>,
    /// `gridAutoTracks` as stored (`NONE`, `ROWS`).
    pub auto_tracks: Option<String>,
    /// `gridReflowEnabled` as stored (its meaning is UNVERIFIED; not mapped).
    pub reflow_enabled: Option<bool>,
    pub children: Vec<(SourceId, FigGridChild)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComponentPropDef {
    pub id: Option<SourceId>,
    pub name: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantProp {
    pub prop_def: Option<SourceId>,
    pub value: Option<String>,
}

/// Payload shared by FRAME, SECTION, SYMBOL and INSTANCE.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePayload {
    pub corner_radius: Option<f32>,
    /// top-left, top-right, bottom-right, bottom-left.
    pub corner_radii: Option<[f32; 4]>,
    /// `frameMaskDisabled` as stored (true = content not clipped).
    pub mask_disabled: Option<bool>,
    pub auto_layout: Option<AutoLayout>,
    pub scroll_direction: Option<String>,
    pub component_props: Vec<ComponentPropDef>,
    pub is_state_group: Option<bool>,
    /// `overlayPositionType` as stored (`CENTER`, ..., `MANUAL`; lane SMB-FIG-SMART-ANIMATE).
    pub overlay_position_type: Option<String>,
    /// `overlayBackgroundInteraction` as stored (`NONE`, `CLOSE_ON_CLICK_OUTSIDE`).
    pub overlay_background_interaction: Option<String>,
    /// `overlayBackgroundAppearance`: (`backgroundType` as stored, `backgroundColor`).
    pub overlay_background: Option<(Option<String>, Option<Color>)>,
    /// Node-level `overlayRelativePosition` (the action-level one is on [`InteractionAction`]).
    pub overlay_relative_position: Option<Vec2>,
}

/// One style run of a TEXT node, in Unicode scalar indices `[start, end)`; font and fills are the
/// run's override where it has one, else the node's base values.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub start: u32,
    pub end: u32,
    pub style_id: u32,
    pub font_family: Option<String>,
    pub font_style: Option<String>,
    pub font_size: Option<f32>,
    pub fills: Vec<Paint>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextPayload {
    pub characters: String,
    pub font_family: Option<String>,
    pub font_style: Option<String>,
    pub font_size: Option<f32>,
    /// (value, units name).
    pub line_height: Option<(f32, String)>,
    pub letter_spacing: Option<(f32, String)>,
    pub align_horizontal: Option<String>,
    pub align_vertical: Option<String>,
    pub auto_resize: Option<String>,
    pub runs: Vec<TextRun>,
    /// `autoRename`: the layer name follows the text content (implicit name; smart animate then
    /// matches the layer by text style).
    pub auto_rename: Option<bool>,
    /// Figma's own laid-out text (`derivedTextData`), when the file carries it.
    pub derived: Option<DerivedText>,
}

/// One laid-out line of `derivedTextData.baselines`: characters `[first, end)` (Unicode scalar
/// indices) on a baseline whose origin is (`x`, `y`) in the TEXT node's own space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLine {
    pub first: u32,
    pub end: u32,
    pub x: f32,
    pub y: f32,
    pub width: Option<f32>,
    pub line_y: Option<f32>,
    pub line_height: Option<f32>,
    pub line_ascent: Option<f32>,
}

/// One placed glyph of `derivedTextData.glyphs`: the character it starts at, its pen position in
/// node space, the font size Figma laid it out with, its advance and its outline (`commandsBlob`,
/// decoded like fill geometry; `None` when absent or undecodable, a loss entry names why).
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedGlyph {
    pub first: u32,
    pub x: f32,
    pub y: f32,
    pub font_size: f32,
    pub advance: Option<f32>,
    pub commands: Option<Vec<PathCommand>>,
}

/// `derivedTextData`: the layout Figma computed and stored (lines, glyph sizes, fonts used).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DerivedText {
    pub lines: Vec<TextLine>,
    pub glyphs: Vec<DerivedGlyph>,
    /// `fontMetaData[].key` (family, style) of the fonts the layout used.
    pub fonts: Vec<(Option<String>, Option<String>)>,
}

/// One `derivedSymbolData` entry of an INSTANCE: Figma's laid-out values for the instance's
/// sublayer at `guid_path` (after overrides and auto layout).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DerivedSymbolNode {
    pub guid_path: Vec<SourceId>,
    pub size: Option<Vec2>,
    pub transform: Option<Matrix>,
    pub text: Option<DerivedText>,
}

/// One `exportSettings` entry of a node, under the stored names. Fields not decoded here are named
/// in `unsupported` (and are decode losses), so an export can refuse the setting instead of
/// ignoring them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportSetting {
    pub suffix: Option<String>,
    /// `imageType` (`PNG`, `JPEG`, `SVG`, `PDF`, ...).
    pub image_type: Option<String>,
    /// `constraint`: (`CONTENT_SCALE` | `CONTENT_WIDTH` | `CONTENT_HEIGHT`, value).
    pub constraint: Option<(String, f32)>,
    pub contents_only: Option<bool>,
    pub use_absolute_bounds: Option<bool>,
    pub color_profile: Option<String>,
    pub quality: Option<f32>,
    pub unsupported: Vec<String>,
}

/// One `symbolOverrides` entry: the node path it targets and the overridden values decoded here.
/// Every other field the entry carries is named in `unsupported` (and is a decode loss entry), so
/// instance expansion can report it per override instead of dropping it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstanceOverride {
    pub guid_path: Vec<SourceId>,
    pub name: Option<String>,
    pub characters: Option<String>,
    pub size: Option<Vec2>,
    pub visible: Option<bool>,
    pub opacity: Option<f32>,
    pub fills: Option<Vec<Paint>>,
    /// Overridden fields not decoded here (`textData.lines`, `strokePaints`, ...).
    pub unsupported: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstancePayload {
    pub symbol_id: Option<SourceId>,
    pub overrides: Vec<InstanceOverride>,
    pub uniform_scale: Option<f32>,
    /// `derivedSymbolData` of the INSTANCE node.
    pub derived: Vec<DerivedSymbolNode>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariableValue {
    Color(Color),
    Float(f32),
    Bool(bool),
    Text(String),
    Alias(SourceId),
    /// A value kind not decoded here (its stored field name).
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariablePayload {
    pub set_id: Option<SourceId>,
    pub resolved_type: Option<String>,
    /// (mode id, value) as stored.
    pub values: Vec<(SourceId, VariableValue)>,
    pub scopes: Vec<String>,
    /// Value for the set's single mode with aliases followed; `None` (with a loss entry) when the
    /// set has more than one mode, the alias chain breaks or loops.
    pub resolved: Option<VariableValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableMode {
    pub id: SourceId,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InteractionAction {
    pub navigation: Option<String>,
    pub transition: Option<String>,
    pub duration: Option<f32>,
    pub easing: Option<String>,
    /// `easingFunction` (the four cubic-bezier numbers of a custom easing), as stored.
    pub easing_function: Vec<f32>,
    pub connection: Option<String>,
    pub destination: Option<SourceId>,
    /// `overlayRelativePosition` of an `OVERLAY` action (document px).
    pub overlay_relative_position: Option<Vec2>,
    /// `transitionShouldSmartAnimate` (directional transitions: smart-animate matching layers).
    pub smart_animate: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interaction {
    pub id: Option<SourceId>,
    /// Stored trigger name (`ON_CLICK`, ...).
    pub trigger: Option<String>,
    /// `event.transitionTimeout` (seconds) of an `AFTER_TIMEOUT` trigger.
    pub timeout: Option<f32>,
    pub deleted: Option<bool>,
    pub actions: Vec<InteractionAction>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodePayload {
    /// Kinds without a payload here (DOCUMENT, GROUP, LINE, and kinds not decoded; the latter carry
    /// a `node_kind_payload_not_decoded` loss).
    None,
    Canvas {
        background: Option<Color>,
        background_enabled: Option<bool>,
        internal_only: Option<bool>,
    },
    Frame(FramePayload),
    Rectangle {
        corner_radius: Option<f32>,
        corner_radii: Option<[f32; 4]>,
    },
    Ellipse {
        start_angle: Option<f32>,
        end_angle: Option<f32>,
        inner_radius: Option<f32>,
    },
    /// STAR and REGULAR_POLYGON (`inner_scale` only on stars).
    Star {
        count: Option<u32>,
        inner_scale: Option<f32>,
    },
    Vector {
        network: Option<VectorNetwork>,
        normalized_size: Option<Vec2>,
    },
    Boolean {
        operation: Option<String>,
    },
    Text(TextPayload),
    Symbol {
        frame: FramePayload,
        variant: Vec<VariantProp>,
        component_key: Option<String>,
    },
    Instance {
        frame: FramePayload,
        instance: InstancePayload,
    },
    Variable(VariablePayload),
    VariableSet {
        modes: Vec<VariableMode>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FigNode {
    pub id: SourceId,
    pub parent: Option<SourceId>,
    /// Fractional-index string ordering siblings (byte order).
    pub position: Option<String>,
    /// Indices into [`FigDocument::nodes`], ordered by `position` then id.
    pub children: Vec<usize>,
    pub type_name: String,
    pub name: Option<String>,
    pub visible: Option<bool>,
    pub opacity: Option<f32>,
    pub blend_mode: Option<String>,
    pub transform: Option<Matrix>,
    pub size: Option<Vec2>,
    pub mask: Option<bool>,
    pub fills: Vec<Paint>,
    pub strokes: Vec<Paint>,
    pub stroke_weight: Option<f32>,
    pub stroke_align: Option<String>,
    pub stroke_join: Option<String>,
    /// `strokeCap` as stored (`NONE`, `ROUND`, `SQUARE`, arrow/marker kinds).
    pub stroke_cap: Option<String>,
    /// `dashPattern` (dash, gap, ... in px); empty = solid.
    pub dash_pattern: Vec<f32>,
    pub miter_limit: Option<f32>,
    /// Local style nodes referenced by `styleIdForEffect` / `styleIdForFill` /
    /// `styleIdForStrokeFill` (`StyleId.guid`; a library `assetRef` is a loss).
    pub effect_style: Option<SourceId>,
    pub fill_style: Option<SourceId>,
    pub stroke_style: Option<SourceId>,
    /// `exportSettings` of the node (frame export).
    pub export_settings: Vec<ExportSetting>,
    pub effects: Vec<Effect>,
    pub fill_geometry: Vec<Geometry>,
    pub stroke_geometry: Vec<Geometry>,
    pub interactions: Vec<Interaction>,
    /// `prototypeStartingPoint.name` (a flow starts at this node).
    pub flow_start: Option<String>,
    /// `prototypeStartingPoint.position`: fractional index ordering the flows of a page.
    pub flow_position: Option<String>,
    pub payload: NodePayload,
}

/// An aggregated loss: `path` names the field (`NodeChange.fillPaints.authoredColor`) or item,
/// `count` how often it occurred, `first` the first node it occurred on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeLoss {
    pub path: String,
    pub code: &'static str,
    pub count: u32,
    pub first: Option<SourceId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FigDocument {
    pub nodes: Vec<FigNode>,
    /// Nodes without a parent field (the DOCUMENT in Figma-written files).
    pub roots: Vec<usize>,
    pub blob_count: usize,
    pub loss: Vec<NodeLoss>,
    /// Field paths that were read, with how often (`NodeChange.fillPaints.color` -> 12). Together
    /// with the `field_not_decoded` losses this accounts for every field present.
    pub decoded: BTreeMap<String, u32>,
    /// sha256 of the exact source bytes (`None` when decoded from a bare tree without container).
    pub source_sha256: Option<[u8; 32]>,
    /// `images/<hash>` container parts -> sha256 of the part bytes (image paint identity).
    pub image_parts: BTreeMap<String, [u8; 32]>,
}

impl FigDocument {
    pub fn node(&self, id: SourceId) -> Option<&FigNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a FigNode> + 'a {
        self.nodes
            .iter()
            .filter(move |n| n.name.as_deref() == Some(name))
    }
    /// Node count per stored type name.
    pub fn kind_histogram(&self) -> BTreeMap<String, u32> {
        let mut h = BTreeMap::new();
        for n in &self.nodes {
            let slot = h.entry(n.type_name.clone()).or_insert(0u32);
            *slot = slot.saturating_add(1);
        }
        h
    }
    pub fn loss_count(&self, path: &str, code: &str) -> u32 {
        self.loss
            .iter()
            .filter(|l| l.path == path && l.code == code)
            .map(|l| l.count)
            .sum()
    }
}

// ---------------------------------------------------------------------------------------------

struct Cx<'a> {
    loss: BTreeMap<(String, &'static str), (u32, Option<SourceId>)>,
    decoded: BTreeMap<String, u32>,
    current: Option<SourceId>,
    /// Grid placements read from children, attached to their parents after linking.
    grid_children: Vec<(SourceId, FigGridChild)>,
    /// `None`: blobs cannot be resolved (loss already recorded).
    blobs: Option<Vec<&'a [u8]>>,
    container: Option<&'a FigContainer>,
    geometry_budget: u64,
    cancel: &'a CancellationToken,
}

impl<'a> Cx<'a> {
    fn lose(&mut self, path: impl Into<String>, code: &'static str) {
        let first = self.current;
        let slot = self.loss.entry((path.into(), code)).or_insert((0, first));
        slot.0 = slot.0.saturating_add(1);
    }
    fn spend(&mut self, n: u64) -> Result<()> {
        self.geometry_budget = self
            .geometry_budget
            .checked_sub(n)
            .ok_or(FigError::BudgetExceeded("tree_values"))?;
        if self.geometry_budget & 4095 == 0 || n > 1 {
            self.cancel.check().map_err(|_| FigError::Canceled)?;
        }
        Ok(())
    }
    fn blob(&mut self, index: u32, path: &str) -> Option<&'a [u8]> {
        let Some(blobs) = &self.blobs else {
            self.lose(path, "blob_unresolvable");
            return None;
        };
        match blobs.get(index as usize).copied() {
            Some(b) => Some(b),
            None => {
                self.lose(path, "blob_index_out_of_range");
                None
            }
        }
    }
}

/// Field accessor that remembers which fields were read; `finish` reports every unread field.
struct Obj<'a> {
    fields: &'a [KiwiField],
    used: Vec<bool>,
    path: String,
}

impl<'a> Obj<'a> {
    fn of(v: &'a KiwiValue, path: impl Into<String>) -> Self {
        let fields = v.fields();
        Self {
            fields,
            used: vec![false; fields.len()],
            path: path.into(),
        }
    }
    fn sub(&self, name: &str) -> String {
        format!("{}.{}", self.path, name)
    }
    /// Names of the fields not read so far.
    fn unused(&self) -> Vec<String> {
        self.fields
            .iter()
            .zip(&self.used)
            .filter(|(_, used)| !**used)
            .map(|(f, _)| f.name.to_string())
            .collect()
    }
    fn take(&mut self, name: &str) -> Option<&'a KiwiValue> {
        let i = self.fields.iter().position(|f| &*f.name == name)?;
        self.used[i] = true;
        Some(&self.fields[i].value)
    }
    fn typed<T>(
        &mut self,
        cx: &mut Cx,
        name: &str,
        get: impl Fn(&'a KiwiValue) -> Option<T>,
    ) -> Option<T> {
        let v = self.take(name)?;
        let out = get(v);
        if out.is_none() {
            cx.lose(self.sub(name), "field_type_unexpected");
        }
        out
    }
    fn f32(&mut self, cx: &mut Cx, name: &str) -> Option<f32> {
        let v = self.typed(cx, name, KiwiValue::as_f32)?;
        if v.is_finite() {
            Some(v)
        } else {
            cx.lose(self.sub(name), "non_finite_value");
            None
        }
    }
    fn u32(&mut self, cx: &mut Cx, name: &str) -> Option<u32> {
        self.typed(cx, name, KiwiValue::as_u32)
    }
    fn bool(&mut self, cx: &mut Cx, name: &str) -> Option<bool> {
        self.typed(cx, name, KiwiValue::as_bool)
    }
    fn string(&mut self, cx: &mut Cx, name: &str) -> Option<String> {
        self.typed(cx, name, |v| v.as_str().map(str::to_owned))
    }
    fn enum_name(&mut self, cx: &mut Cx, name: &str) -> Option<String> {
        let v = self.take(name)?;
        match v {
            KiwiValue::Enum { name: Some(n), .. } => Some(n.to_string()),
            KiwiValue::Enum { name: None, .. } => {
                cx.lose(self.sub(name), "enum_value_unknown");
                None
            }
            _ => {
                cx.lose(self.sub(name), "field_type_unexpected");
                None
            }
        }
    }
    fn obj(&mut self, cx: &mut Cx, name: &str) -> Option<Obj<'a>> {
        let v = self.take(name)?;
        if matches!(v, KiwiValue::Struct(_) | KiwiValue::Message(_)) {
            Some(Obj::of(v, self.sub(name)))
        } else {
            cx.lose(self.sub(name), "field_type_unexpected");
            None
        }
    }
    fn array(&mut self, cx: &mut Cx, name: &str) -> &'a [KiwiValue] {
        match self.take(name) {
            None => &[],
            Some(KiwiValue::Array(a)) => a,
            Some(_) => {
                cx.lose(self.sub(name), "field_type_unexpected");
                &[]
            }
        }
    }
    fn guid(&mut self, cx: &mut Cx, name: &str) -> Option<SourceId> {
        let mut o = self.obj(cx, name)?;
        let id = guid_of(&mut o, cx);
        o.finish(cx);
        id
    }
    fn vec2(&mut self, cx: &mut Cx, name: &str) -> Option<Vec2> {
        let mut o = self.obj(cx, name)?;
        let v = match (o.f32(cx, "x"), o.f32(cx, "y")) {
            (Some(x), Some(y)) => Some(Vec2 { x, y }),
            _ => None,
        };
        o.finish(cx);
        v
    }
    fn color(&mut self, cx: &mut Cx, name: &str) -> Option<Color> {
        let mut o = self.obj(cx, name)?;
        let c = color_of(&mut o, cx);
        o.finish(cx);
        c
    }
    fn matrix(&mut self, cx: &mut Cx, name: &str) -> Option<Matrix> {
        let mut o = self.obj(cx, name)?;
        let m = match (
            o.f32(cx, "m00"),
            o.f32(cx, "m01"),
            o.f32(cx, "m02"),
            o.f32(cx, "m10"),
            o.f32(cx, "m11"),
            o.f32(cx, "m12"),
        ) {
            (Some(m00), Some(m01), Some(m02), Some(m10), Some(m11), Some(m12)) => Some(Matrix {
                m00,
                m01,
                m02,
                m10,
                m11,
                m12,
            }),
            _ => None,
        };
        o.finish(cx);
        m
    }
    fn finish(self, cx: &mut Cx) {
        for (f, used) in self.fields.iter().zip(&self.used) {
            let path = format!("{}.{}", self.path, f.name);
            if *used {
                let slot = cx.decoded.entry(path).or_insert(0);
                *slot = slot.saturating_add(1);
            } else {
                cx.lose(path, "field_not_decoded");
            }
        }
    }
}

/// `exportSettings`: suffix, image type, constraint and the options an export honours or refuses;
/// the rest are named per setting in `unsupported`.
fn export_settings(o: &mut Obj, cx: &mut Cx) -> Vec<ExportSetting> {
    let path = o.sub("exportSettings");
    let mut out = Vec::new();
    for v in o.array(cx, "exportSettings") {
        let mut so = Obj::of(v, path.clone());
        let mut s = ExportSetting {
            suffix: so.string(cx, "suffix"),
            image_type: so.enum_name(cx, "imageType"),
            contents_only: so.bool(cx, "contentsOnly"),
            use_absolute_bounds: so.bool(cx, "useAbsoluteBounds"),
            color_profile: so.enum_name(cx, "colorProfile"),
            quality: so.f32(cx, "quality"),
            ..ExportSetting::default()
        };
        if let Some(mut c) = so.obj(cx, "constraint") {
            s.constraint = c.enum_name(cx, "type").zip(c.f32(cx, "value"));
            c.finish(cx);
        }
        s.unsupported = so.unused();
        so.finish(cx);
        out.push(s);
    }
    out
}

/// A `StyleId` field: the local style node's guid; a library `assetRef` has no node in the file.
fn style_id(o: &mut Obj, cx: &mut Cx, name: &str) -> Option<SourceId> {
    let mut so = o.obj(cx, name)?;
    let id = so.guid(cx, "guid");
    if id.is_none() && so.fields.iter().any(|f| &*f.name == "assetRef") {
        cx.lose(so.sub("assetRef"), "style_asset_ref_not_in_file");
        let _ = so.take("assetRef");
    }
    so.finish(cx);
    id
}

fn guid_of(o: &mut Obj, cx: &mut Cx) -> Option<SourceId> {
    match (o.u32(cx, "sessionID"), o.u32(cx, "localID")) {
        (Some(session), Some(local)) => Some(SourceId { session, local }),
        _ => None,
    }
}

fn color_of(o: &mut Obj, cx: &mut Cx) -> Option<Color> {
    match (
        o.f32(cx, "r"),
        o.f32(cx, "g"),
        o.f32(cx, "b"),
        o.f32(cx, "a"),
    ) {
        (Some(r), Some(g), Some(b), Some(a)) => Some(Color { r, g, b, a }),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn read_f32(b: &[u8], at: usize) -> Option<f32> {
    let end = at.checked_add(4)?;
    let raw: [u8; 4] = b.get(at..end)?.try_into().ok()?;
    Some(f32::from_le_bytes(raw))
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let raw: [u8; 4] = b.get(at..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(raw))
}

/// Decodes a path commands blob; `Ok(Err(code))` is a malformed blob (soft), `Err` a budget hit.
fn decode_path(
    b: &[u8],
    cx: &mut Cx,
) -> Result<std::result::Result<Vec<PathCommand>, &'static str>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < b.len() {
        cx.spend(1)?;
        let cmd = b[at];
        at = at.saturating_add(1);
        let n = match cmd {
            0 => 0usize,
            1 | 2 => 2,
            3 => 4,
            4 => 6,
            _ => return Ok(Err("path_command_unknown")),
        };
        let mut f = [0f32; 6];
        for slot in f.iter_mut().take(n) {
            let Some(v) = read_f32(b, at) else {
                return Ok(Err("path_blob_truncated"));
            };
            *slot = v;
            at = at.saturating_add(4);
        }
        out.push(match cmd {
            0 => PathCommand::Close,
            1 => PathCommand::MoveTo { x: f[0], y: f[1] },
            2 => PathCommand::LineTo { x: f[0], y: f[1] },
            3 => PathCommand::QuadTo {
                x1: f[0],
                y1: f[1],
                x: f[2],
                y: f[3],
            },
            _ => PathCommand::CubicTo {
                x1: f[0],
                y1: f[1],
                x2: f[2],
                y2: f[3],
                x: f[4],
                y: f[5],
            },
        });
    }
    Ok(Ok(out))
}

/// Decodes a vector-network blob (`Ok(Err(code))` = malformed). Record counts are checked against
/// the remaining bytes before any allocation (V4-02).
fn decode_network(
    b: &[u8],
    cx: &mut Cx,
) -> Result<std::result::Result<(VectorNetwork, bool), &'static str>> {
    let bad = |c| Ok(Err(c));
    let (Some(nv), Some(ns), Some(nr)) = (read_u32(b, 0), read_u32(b, 4), read_u32(b, 8)) else {
        return bad("vector_network_truncated");
    };
    let fixed = (nv as u64)
        .checked_mul(12)
        .and_then(|v| (ns as u64).checked_mul(28).and_then(|s| v.checked_add(s)))
        .and_then(|t| t.checked_add(12));
    let Some(fixed) = fixed.filter(|t| *t <= b.len() as u64) else {
        return bad("vector_network_counts_exceed_blob");
    };
    if (nr as u64).saturating_mul(8) > (b.len() as u64).saturating_sub(fixed) {
        return bad("vector_network_counts_exceed_blob");
    }
    cx.spend(
        u64::from(nv)
            .saturating_add(u64::from(ns))
            .saturating_add(u64::from(nr)),
    )?;
    let mut at = 12usize;
    let mut vertices = Vec::with_capacity(nv as usize);
    for _ in 0..nv {
        let (Some(style), Some(x), Some(y)) = (
            read_u32(b, at),
            read_f32(b, at.saturating_add(4)),
            read_f32(b, at.saturating_add(8)),
        ) else {
            return bad("vector_network_truncated");
        };
        vertices.push(NetVertex { style, x, y });
        at = at.saturating_add(12);
    }
    let mut segments = Vec::with_capacity(ns as usize);
    for _ in 0..ns {
        let f = |k: usize| read_f32(b, at.saturating_add(k));
        let u = |k: usize| read_u32(b, at.saturating_add(k));
        let (Some(style), Some(start), Some(t1x), Some(t1y), Some(end), Some(t2x), Some(t2y)) =
            (u(0), u(4), f(8), f(12), u(16), f(20), f(24))
        else {
            return bad("vector_network_truncated");
        };
        if start >= nv || end >= nv {
            return bad("vector_network_vertex_index");
        }
        segments.push(NetSegment {
            style,
            start,
            tangent_start: Vec2 { x: t1x, y: t1y },
            end,
            tangent_end: Vec2 { x: t2x, y: t2y },
        });
        at = at.saturating_add(28);
    }
    let mut regions = Vec::with_capacity(nr as usize);
    for _ in 0..nr {
        let (Some(flags), Some(nloops)) = (read_u32(b, at), read_u32(b, at.saturating_add(4)))
        else {
            return bad("vector_network_truncated");
        };
        at = at.saturating_add(8);
        if (nloops as u64).saturating_mul(4) > (b.len().saturating_sub(at)) as u64 {
            return bad("vector_network_counts_exceed_blob");
        }
        let mut loops = Vec::with_capacity(nloops as usize);
        for _ in 0..nloops {
            let Some(count) = read_u32(b, at) else {
                return bad("vector_network_truncated");
            };
            at = at.saturating_add(4);
            if (count as u64).saturating_mul(4) > (b.len().saturating_sub(at)) as u64 {
                return bad("vector_network_counts_exceed_blob");
            }
            cx.spend(u64::from(count))?;
            let mut lp = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let Some(s) = read_u32(b, at) else {
                    return bad("vector_network_truncated");
                };
                if s >= ns {
                    return bad("vector_network_segment_index");
                }
                lp.push(s);
                at = at.saturating_add(4);
            }
            loops.push(lp);
        }
        regions.push(NetRegion {
            style: flags >> 1,
            nonzero: flags & 1 == 1,
            loops,
        });
    }
    Ok(Ok((
        VectorNetwork {
            vertices,
            segments,
            regions,
        },
        at != b.len(),
    )))
}

/// Decodes one path-commands blob (`fillGeometry`/`strokeGeometry`/glyph `commandsBlob`). At most
/// `limits.max_tree_values` commands; a malformed blob is `MessageInvalid` naming the cause.
pub fn decode_path_blob(blob: &[u8], limits: &Limits) -> Result<Vec<PathCommand>> {
    let cancel = CancellationToken::default();
    let mut cx = Cx {
        loss: BTreeMap::new(),
        decoded: BTreeMap::new(),
        current: None,
        grid_children: Vec::new(),
        blobs: None,
        container: None,
        geometry_budget: limits.max_tree_values,
        cancel: &cancel,
    };
    decode_path(blob, &mut cx)?.map_err(FigError::MessageInvalid)
}

fn geometry(o: &mut Obj, cx: &mut Cx, name: &str) -> Result<Vec<Geometry>> {
    let path = o.sub(name);
    let mut out = Vec::new();
    for g in o.array(cx, name) {
        let mut go = Obj::of(g, path.clone());
        let winding_rule = go.enum_name(cx, "windingRule");
        let blob = go.u32(cx, "commandsBlob");
        go.finish(cx);
        let mut commands = None;
        if let Some(i) = blob {
            let blob_path = format!("{path}.commandsBlob");
            if let Some(bytes) = cx.blob(i, &blob_path) {
                match decode_path(bytes, cx)? {
                    Ok(c) => commands = Some(c),
                    Err(code) => cx.lose(blob_path, code),
                }
            }
        }
        out.push(Geometry {
            winding_rule,
            blob,
            commands,
        });
    }
    Ok(out)
}

fn paints(o: &mut Obj, cx: &mut Cx, name: &str) -> Vec<Paint> {
    let path = o.sub(name);
    o.array(cx, name)
        .iter()
        .map(|p| paint(p, cx, &path))
        .collect()
}

fn paint(v: &KiwiValue, cx: &mut Cx, path: &str) -> Paint {
    let mut o = Obj::of(v, path);
    let type_name = o.enum_name(cx, "type").unwrap_or_default();
    let opacity = o.f32(cx, "opacity");
    let visible = o.bool(cx, "visible");
    let blend_mode = o.enum_name(cx, "blendMode");
    let color_variable = o.obj(cx, "colorVar").and_then(|mut cv| {
        let id = variable_alias(&mut cv, cx);
        cv.finish(cx);
        id
    });
    let kind = match type_name.as_str() {
        "SOLID" => PaintKind::Solid {
            color: o.color(cx, "color"),
        },
        t if t.starts_with("GRADIENT_") => {
            let stops_path = o.sub("stops");
            let stops = o
                .array(cx, "stops")
                .iter()
                .filter_map(|s| {
                    let mut so = Obj::of(s, stops_path.clone());
                    let st = match (so.f32(cx, "position"), so.color(cx, "color")) {
                        (Some(position), Some(color)) => Some(GradientStop { position, color }),
                        _ => None,
                    };
                    so.finish(cx);
                    st
                })
                .collect();
            PaintKind::Gradient {
                gradient: type_name.clone(),
                stops,
                transform: o.matrix(cx, "transform"),
            }
        }
        "IMAGE" => {
            let (mut hash, mut data_blob) = (Vec::new(), None);
            if let Some(mut io) = o.obj(cx, "image") {
                hash = io
                    .typed(cx, "hash", |v| v.as_bytes().map(<[u8]>::to_vec))
                    .unwrap_or_default();
                data_blob = io.u32(cx, "dataBlob");
                let _ = io.string(cx, "name");
                io.finish(cx);
            }
            let part = if hash.is_empty() {
                None
            } else {
                let name = format!("{IMAGES_PREFIX}{}", hex_lower(&hash));
                match cx.container {
                    Some(c) if c.part(&name).is_some() => Some(name),
                    _ => {
                        if data_blob.is_none() {
                            cx.lose(format!("{path}.image"), "image_part_missing");
                        }
                        None
                    }
                }
            };
            if let Some(i) = data_blob {
                let _ = cx.blob(i, &format!("{path}.image.dataBlob"));
            }
            let w = o.u32(cx, "originalImageWidth");
            let h = o.u32(cx, "originalImageHeight");
            PaintKind::Image {
                hash,
                part,
                data_blob,
                scale_mode: o.enum_name(cx, "imageScaleMode"),
                transform: o.matrix(cx, "transform"),
                original_size: w.zip(h),
            }
        }
        _ => {
            cx.lose(format!("{path}.type={type_name}"), "paint_type_unsupported");
            PaintKind::Unsupported { type_name }
        }
    };
    o.finish(cx);
    Paint {
        kind,
        opacity,
        visible,
        blend_mode,
        color_variable,
    }
}

/// `VariableData`-shaped value: `{value: {alias: {guid}} , dataType, resolvedDataType}` -> alias id.
fn variable_alias(o: &mut Obj, cx: &mut Cx) -> Option<SourceId> {
    let _ = o.enum_name(cx, "dataType");
    let _ = o.enum_name(cx, "resolvedDataType");
    match variable_value(o, cx)? {
        VariableValue::Alias(id) => Some(id),
        _ => None,
    }
}

/// Reads `value` of a `VariableData`-shaped object.
fn variable_value(o: &mut Obj, cx: &mut Cx) -> Option<VariableValue> {
    let mut vo = o.obj(cx, "value")?;
    let out = if vo.fields.iter().any(|f| &*f.name == "colorValue") {
        vo.color(cx, "colorValue").map(VariableValue::Color)
    } else if vo.fields.iter().any(|f| &*f.name == "floatValue") {
        vo.f32(cx, "floatValue").map(VariableValue::Float)
    } else if vo.fields.iter().any(|f| &*f.name == "boolValue") {
        vo.bool(cx, "boolValue").map(VariableValue::Bool)
    } else if vo.fields.iter().any(|f| &*f.name == "textValue") {
        vo.string(cx, "textValue").map(VariableValue::Text)
    } else if vo.fields.iter().any(|f| &*f.name == "alias") {
        vo.obj(cx, "alias").and_then(|mut ao| {
            let id = ao.guid(cx, "guid");
            ao.finish(cx);
            id.map(VariableValue::Alias)
        })
    } else {
        vo.fields
            .first()
            .map(|f| VariableValue::Unsupported(f.name.to_string()))
    };
    vo.finish(cx);
    out
}

fn effects(o: &mut Obj, cx: &mut Cx) -> Vec<Effect> {
    let path = o.sub("effects");
    o.array(cx, "effects")
        .iter()
        .map(|e| {
            let mut eo = Obj::of(e, path.clone());
            let eff = Effect {
                kind: eo.enum_name(cx, "type").unwrap_or_default(),
                offset: eo.vec2(cx, "offset"),
                radius: eo.f32(cx, "radius"),
                spread: eo.f32(cx, "spread"),
                color: eo.color(cx, "color"),
                visible: eo.bool(cx, "visible"),
                blend_mode: eo.enum_name(cx, "blendMode"),
                show_shadow_behind_node: eo.bool(cx, "showShadowBehindNode"),
            };
            eo.finish(cx);
            eff
        })
        .collect()
}

fn corners(o: &mut Obj, cx: &mut Cx) -> (Option<f32>, Option<[f32; 4]>) {
    let r = o.f32(cx, "cornerRadius");
    let radii = match (
        o.f32(cx, "rectangleTopLeftCornerRadius"),
        o.f32(cx, "rectangleTopRightCornerRadius"),
        o.f32(cx, "rectangleBottomRightCornerRadius"),
        o.f32(cx, "rectangleBottomLeftCornerRadius"),
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
        _ => None,
    };
    (r, radii)
}

fn frame(o: &mut Obj, cx: &mut Cx) -> Result<FramePayload> {
    let (corner_radius, corner_radii) = corners(o, cx);
    let auto_layout = match o.enum_name(cx, "stackMode") {
        Some(mode) => Some(AutoLayout {
            mode,
            spacing: o.f32(cx, "stackSpacing"),
            horizontal_padding: o.f32(cx, "stackHorizontalPadding"),
            vertical_padding: o.f32(cx, "stackVerticalPadding"),
            padding_right: o.f32(cx, "stackPaddingRight"),
            padding_bottom: o.f32(cx, "stackPaddingBottom"),
            grid: grid_container(o, cx)?,
        }),
        None => None,
    };
    let defs_path = o.sub("componentPropDefs");
    let component_props = o
        .array(cx, "componentPropDefs")
        .iter()
        .map(|d| {
            let mut dp = Obj::of(d, defs_path.clone());
            let def = ComponentPropDef {
                id: dp.guid(cx, "id"),
                name: dp.string(cx, "name"),
                kind: dp.enum_name(cx, "type"),
            };
            dp.finish(cx);
            def
        })
        .collect();
    Ok(FramePayload {
        corner_radius,
        corner_radii,
        mask_disabled: o.bool(cx, "frameMaskDisabled"),
        auto_layout,
        scroll_direction: o.enum_name(cx, "scrollDirection"),
        component_props,
        is_state_group: o.bool(cx, "isStateGroup"),
        overlay_position_type: o.enum_name(cx, "overlayPositionType"),
        overlay_background_interaction: o.enum_name(cx, "overlayBackgroundInteraction"),
        overlay_background: o.obj(cx, "overlayBackgroundAppearance").map(|mut bo| {
            let v = (bo.enum_name(cx, "backgroundType"), bo.color(cx, "backgroundColor"));
            bo.finish(cx);
            v
        }),
        overlay_relative_position: o.vec2(cx, "overlayRelativePosition"),
    })
}

/// The grid container fields a frame stores (kiwi names of the files' own schema, lane
/// SMB-FIG-GRID-2), `None` when it stores none of them.
const GRID_CONTAINER_FIELDS: [&str; 8] = [
    "gridRows",
    "gridColumns",
    "gridRowsSizing",
    "gridColumnsSizing",
    "gridRowGap",
    "gridColumnGap",
    "gridAutoTracks",
    "gridReflowEnabled",
];
/// The grid placement fields a child stores.
const GRID_CHILD_FIELDS: [&str; 6] = [
    "gridRowAnchor",
    "gridColumnAnchor",
    "gridRowSpan",
    "gridColumnSpan",
    "gridChildVerticalAlign",
    "gridChildHorizontalAlign",
];

fn stores_any(o: &Obj, names: &[&str]) -> bool {
    o.fields.iter().any(|f| names.contains(&&*f.name))
}

fn grid_container(o: &mut Obj, cx: &mut Cx) -> Result<Option<FigGrid>> {
    if !stores_any(o, &GRID_CONTAINER_FIELDS) {
        return Ok(None);
    }
    let mut rows = grid_tracks(o, cx, "gridRows")?;
    let mut columns = grid_tracks(o, cx, "gridColumns")?;
    grid_sizes(o, cx, "gridRowsSizing", &mut rows)?;
    grid_sizes(o, cx, "gridColumnsSizing", &mut columns)?;
    Ok(Some(FigGrid {
        rows,
        columns,
        row_gap: o.f32(cx, "gridRowGap"),
        column_gap: o.f32(cx, "gridColumnGap"),
        auto_tracks: o.enum_name(cx, "gridAutoTracks"),
        reflow_enabled: o.bool(cx, "gridReflowEnabled"),
        children: Vec::new(),
    }))
}

/// Most entries one GRID track list or sizing list may hold at decode: Folio's track bound
/// (`GRID_COUNT_MAX`, 1000). A longer list is refused whole as a typed loss before any per-entry
/// work (F-R45-1, lane SMB-R46-FOLLOWUPS-1: the kiwi arrays are bounded only by `Limits`, 16M).
const GRID_ENTRIES_MAX: usize = hsk_studio_folio::GRID_COUNT_MAX as usize;

/// A `GUIDPositionMap` of tracks, in stored order (position bytes, then id). An entry without id
/// or position, or repeating an id, is left out (loss); a list of more than [`GRID_ENTRIES_MAX`]
/// entries is left out whole (`grid_track_count_over_limit`). Cancellable per entry.
fn grid_tracks(o: &mut Obj, cx: &mut Cx, name: &str) -> Result<Vec<FigGridTrack>> {
    let Some(mut m) = o.obj(cx, name) else {
        return Ok(Vec::new());
    };
    let entries_path = m.sub("entries");
    let entries = m.array(cx, "entries");
    if entries.len() > GRID_ENTRIES_MAX {
        cx.lose(m.path.clone(), "grid_track_count_over_limit");
        m.finish(cx);
        return Ok(Vec::new());
    }
    let mut out: Vec<FigGridTrack> = Vec::with_capacity(entries.len());
    let mut seen: HashSet<SourceId> = HashSet::with_capacity(entries.len());
    for e in entries {
        cx.cancel.check().map_err(|_| FigError::Canceled)?;
        let mut eo = Obj::of(e, entries_path.clone());
        let id = eo.guid(cx, "id");
        let position = eo.string(cx, "position");
        eo.finish(cx);
        match (id, position) {
            (Some(id), _) if seen.contains(&id) => {
                cx.lose(m.path.clone(), "grid_track_duplicate_id");
            }
            (Some(id), Some(position)) => {
                seen.insert(id);
                out.push(FigGridTrack {
                    id,
                    position,
                    size: None,
                });
            }
            _ => cx.lose(m.path.clone(), "grid_track_incomplete"),
        }
    }
    m.finish(cx);
    out.sort_by(|a, b| {
        a.position
            .as_bytes()
            .cmp(b.position.as_bytes())
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

fn track_sizing(o: &mut Obj, cx: &mut Cx, name: &str) -> Option<FigTrackSizing> {
    let mut so = o.obj(cx, name)?;
    let kind = so.enum_name(cx, "type");
    let value = so.f32(cx, "value");
    so.finish(cx);
    kind.map(|kind| FigTrackSizing { kind, value })
}

/// A `GUIDGridTrackSizeMap`: each entry's size goes to the track with its id; an entry naming no
/// track of the axis, or a second entry for one track, is a loss; a list of more than
/// [`GRID_ENTRIES_MAX`] entries is left out whole (`grid_track_size_count_over_limit`). Tracks are
/// found through an id index; cancellable per entry.
fn grid_sizes(o: &mut Obj, cx: &mut Cx, name: &str, tracks: &mut [FigGridTrack]) -> Result<()> {
    let Some(mut m) = o.obj(cx, name) else {
        return Ok(());
    };
    let entries_path = m.sub("entries");
    let entries = m.array(cx, "entries");
    if entries.len() > GRID_ENTRIES_MAX {
        cx.lose(m.path.clone(), "grid_track_size_count_over_limit");
        m.finish(cx);
        return Ok(());
    }
    let at: HashMap<SourceId, usize> = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    for e in entries {
        cx.cancel.check().map_err(|_| FigError::Canceled)?;
        let mut eo = Obj::of(e, entries_path.clone());
        let id = eo.guid(cx, "id");
        let size = eo.obj(cx, "trackSize").map(|mut so| {
            let s = FigTrackSize {
                min: track_sizing(&mut so, cx, "minSizing"),
                max: track_sizing(&mut so, cx, "maxSizing"),
            };
            so.finish(cx);
            s
        });
        eo.finish(cx);
        match id.and_then(|id| at.get(&id)).and_then(|&i| tracks.get_mut(i)) {
            Some(t) if t.size.is_some() => cx.lose(m.path.clone(), "grid_track_size_duplicate"),
            Some(t) => match size {
                Some(s) => t.size = Some(s),
                None => cx.lose(m.path.clone(), "grid_track_size_incomplete"),
            },
            None => cx.lose(m.path.clone(), "grid_track_size_orphan"),
        }
    }
    m.finish(cx);
    Ok(())
}

/// The grid placement fields of any node (`None` when it stores none of them).
fn grid_child(o: &mut Obj, cx: &mut Cx) -> Option<FigGridChild> {
    if !stores_any(o, &GRID_CHILD_FIELDS) {
        return None;
    }
    Some(FigGridChild {
        row_anchor: o.guid(cx, "gridRowAnchor"),
        column_anchor: o.guid(cx, "gridColumnAnchor"),
        row_span: o.u32(cx, "gridRowSpan"),
        column_span: o.u32(cx, "gridColumnSpan"),
        vertical_align: o.enum_name(cx, "gridChildVerticalAlign"),
        horizontal_align: o.enum_name(cx, "gridChildHorizontalAlign"),
    })
}

/// The frame payload of a frame, component or instance node.
fn frame_payload_mut(p: &mut NodePayload) -> Option<&mut FramePayload> {
    match p {
        NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } => Some(f),
        _ => None,
    }
}

/// Attaches each child's grid placement to its parent's grid, in child order; a placement whose
/// parent stores no grid fields is a loss.
fn attach_grid_children(nodes: &mut [FigNode], index: &HashMap<SourceId, usize>, cx: &mut Cx) {
    let mut attach: Vec<(usize, usize, SourceId, FigGridChild)> = Vec::new();
    for (child, gc) in std::mem::take(&mut cx.grid_children) {
        let Some(&ci) = index.get(&child) else {
            continue;
        };
        let parent = nodes[ci].parent.and_then(|p| index.get(&p).copied());
        let grid_parent = parent.filter(|&pi| {
            frame_payload_mut(&mut nodes[pi].payload)
                .and_then(|f| f.auto_layout.as_ref())
                .is_some_and(|a| a.grid.is_some())
        });
        match grid_parent {
            Some(pi) => {
                let order = nodes[pi]
                    .children
                    .iter()
                    .position(|&k| k == ci)
                    .unwrap_or(usize::MAX);
                attach.push((pi, order, child, gc));
            }
            None => {
                cx.current = Some(child);
                cx.lose("NodeChange.grid", "grid_child_without_grid_parent");
            }
        }
    }
    attach.sort_by_key(|a| (a.0, a.1));
    for (pi, _, child, gc) in attach {
        if let Some(g) = frame_payload_mut(&mut nodes[pi].payload)
            .and_then(|f| f.auto_layout.as_mut())
            .and_then(|a| a.grid.as_mut())
        {
            g.children.push((child, gc));
        }
    }
}

fn font_name(o: &mut Obj, cx: &mut Cx) -> (Option<String>, Option<String>) {
    match o.obj(cx, "fontName") {
        Some(mut fo) => {
            let family = fo.string(cx, "family");
            let style = fo.string(cx, "style");
            let _ = fo.string(cx, "postscript");
            fo.finish(cx);
            (family, style)
        }
        None => (None, None),
    }
}

fn number_with_units(o: &mut Obj, cx: &mut Cx, name: &str) -> Option<(f32, String)> {
    let mut no = o.obj(cx, name)?;
    let v = no.f32(cx, "value").zip(no.enum_name(cx, "units"));
    no.finish(cx);
    v
}

fn text(o: &mut Obj, cx: &mut Cx, base_fills: &[Paint]) -> Result<TextPayload> {
    let (font_family, font_style) = font_name(o, cx);
    let font_size = o.f32(cx, "fontSize");
    let mut t = TextPayload {
        font_family,
        font_style,
        font_size,
        line_height: number_with_units(o, cx, "lineHeight"),
        letter_spacing: number_with_units(o, cx, "letterSpacing"),
        align_horizontal: o.enum_name(cx, "textAlignHorizontal"),
        align_vertical: o.enum_name(cx, "textAlignVertical"),
        auto_resize: o.enum_name(cx, "textAutoResize"),
        auto_rename: o.bool(cx, "autoRename"),
        derived: derived_text(o, cx)?,
        ..TextPayload::default()
    };
    let Some(mut td) = o.obj(cx, "textData") else {
        return Ok(t);
    };
    t.characters = td.string(cx, "characters").unwrap_or_default();
    let ids: Vec<u32> = td
        .array(cx, "characterStyleIDs")
        .iter()
        .map(|v| v.as_u32().unwrap_or(0))
        .collect();
    let table_path = td.sub("styleOverrideTable");
    let mut styles = HashMap::new();
    for s in td.array(cx, "styleOverrideTable") {
        let mut so = Obj::of(s, table_path.clone());
        let id = so.u32(cx, "styleID").unwrap_or(0);
        let (family, style) = font_name(&mut so, cx);
        let size = so.f32(cx, "fontSize");
        let fills_present = so.fields.iter().any(|f| &*f.name == "fillPaints");
        let fills = paints(&mut so, cx, "fillPaints");
        so.finish(cx);
        styles.insert(id, (family, style, size, fills_present.then_some(fills)));
    }
    td.finish(cx);
    // Runs over Unicode scalars; characters past the end of characterStyleIDs use style 0 (base).
    let len = u32::try_from(t.characters.chars().count()).unwrap_or(u32::MAX);
    let style_at = |i: u32| ids.get(i as usize).copied().unwrap_or(0);
    let mut start = 0u32;
    while start < len {
        let id = style_at(start);
        let mut end = start.saturating_add(1);
        while end < len && style_at(end) == id {
            end = end.saturating_add(1);
        }
        let ov = styles.get(&id);
        t.runs.push(TextRun {
            start,
            end,
            style_id: id,
            font_family: ov
                .and_then(|o| o.0.clone())
                .or_else(|| t.font_family.clone()),
            font_style: ov
                .and_then(|o| o.1.clone())
                .or_else(|| t.font_style.clone()),
            font_size: ov.and_then(|o| o.2).or(t.font_size),
            fills: ov
                .and_then(|o| o.3.clone())
                .unwrap_or_else(|| base_fills.to_vec()),
        });
        start = end;
    }
    Ok(t)
}

fn instance(o: &mut Obj, cx: &mut Cx) -> Result<InstancePayload> {
    let mut p = InstancePayload::default();
    let Some(mut sd) = o.obj(cx, "symbolData") else {
        p.derived = derived_symbol(o, cx)?;
        return Ok(p);
    };
    p.symbol_id = sd.guid(cx, "symbolID");
    p.uniform_scale = sd.f32(cx, "uniformScaleFactor");
    let ov_path = sd.sub("symbolOverrides");
    for ov in sd.array(cx, "symbolOverrides") {
        let mut oo = Obj::of(ov, ov_path.clone());
        let mut item = InstanceOverride::default();
        if let Some(mut gp) = oo.obj(cx, "guidPath") {
            let guids_path = gp.sub("guids");
            for g in gp.array(cx, "guids") {
                let mut go = Obj::of(g, guids_path.clone());
                if let Some(id) = guid_of(&mut go, cx) {
                    item.guid_path.push(id);
                }
                go.finish(cx);
            }
            gp.finish(cx);
        }
        item.name = oo.string(cx, "name");
        item.size = oo.vec2(cx, "size");
        item.visible = oo.bool(cx, "visible");
        item.opacity = oo.f32(cx, "opacity");
        if oo.fields.iter().any(|f| &*f.name == "fillPaints") {
            item.fills = Some(paints(&mut oo, cx, "fillPaints"));
        }
        let mut text_unused = Vec::new();
        if let Some(mut td) = oo.obj(cx, "textData") {
            item.characters = td.string(cx, "characters");
            text_unused = td.unused();
            td.finish(cx);
        }
        item.unsupported = oo.unused();
        item.unsupported
            .extend(text_unused.into_iter().map(|f| format!("textData.{f}")));
        oo.finish(cx);
        p.overrides.push(item);
    }
    sd.finish(cx);
    p.derived = derived_symbol(o, cx)?;
    Ok(p)
}

/// `derivedSymbolData` of an INSTANCE: per sublayer path, the laid-out size, transform and text.
fn derived_symbol(o: &mut Obj, cx: &mut Cx) -> Result<Vec<DerivedSymbolNode>> {
    let path = o.sub("derivedSymbolData");
    let mut out = Vec::new();
    for e in o.array(cx, "derivedSymbolData") {
        let mut eo = Obj::of(e, path.clone());
        let mut node = DerivedSymbolNode::default();
        if let Some(mut gp) = eo.obj(cx, "guidPath") {
            let guids_path = gp.sub("guids");
            for g in gp.array(cx, "guids") {
                let mut go = Obj::of(g, guids_path.clone());
                if let Some(id) = guid_of(&mut go, cx) {
                    node.guid_path.push(id);
                }
                go.finish(cx);
            }
            gp.finish(cx);
        }
        node.size = eo.vec2(cx, "size");
        node.transform = eo.matrix(cx, "transform");
        node.text = derived_text(&mut eo, cx)?;
        eo.finish(cx);
        out.push(node);
    }
    Ok(out)
}

/// `derivedTextData`: baselines (lines), glyph positions and sizes, font keys; every other
/// member stays a `field_not_decoded` loss.
fn derived_text(o: &mut Obj, cx: &mut Cx) -> Result<Option<DerivedText>> {
    let Some(mut d) = o.obj(cx, "derivedTextData") else {
        return Ok(None);
    };
    let mut out = DerivedText::default();
    let lines_path = d.sub("baselines");
    for b in d.array(cx, "baselines") {
        let mut bo = Obj::of(b, lines_path.clone());
        let pos = bo.vec2(cx, "position");
        let first = bo.u32(cx, "firstCharacter");
        let end = bo.u32(cx, "endCharacter");
        let line = TextLine {
            first: first.unwrap_or(0),
            end: end.unwrap_or(0),
            x: pos.map_or(0.0, |p| p.x),
            y: pos.map_or(0.0, |p| p.y),
            width: bo.f32(cx, "width"),
            line_y: bo.f32(cx, "lineY"),
            line_height: bo.f32(cx, "lineHeight"),
            line_ascent: bo.f32(cx, "lineAscent"),
        };
        bo.finish(cx);
        match (pos, first, end) {
            (Some(_), Some(f), Some(e)) if f <= e => out.lines.push(line),
            _ => cx.lose(lines_path.clone(), "derived_baseline_incomplete"),
        }
    }
    let glyphs_path = d.sub("glyphs");
    for g in d.array(cx, "glyphs") {
        let mut go = Obj::of(g, glyphs_path.clone());
        let first = go.u32(cx, "firstCharacter");
        let pos = go.vec2(cx, "position");
        let size = go.f32(cx, "fontSize");
        let advance = go.f32(cx, "advance");
        let blob = go.u32(cx, "commandsBlob");
        go.finish(cx);
        // The glyph outline Figma laid out (path commands; an empty blob = a space).
        let mut commands = None;
        if let Some(i) = blob {
            let blob_path = format!("{glyphs_path}.commandsBlob");
            if let Some(bytes) = cx.blob(i, &blob_path) {
                match decode_path(bytes, cx)? {
                    Ok(c) => commands = Some(c),
                    Err(code) => cx.lose(blob_path, code),
                }
            }
        }
        match (first, pos, size) {
            (Some(first), Some(p), Some(font_size)) => out.glyphs.push(DerivedGlyph {
                first,
                x: p.x,
                y: p.y,
                font_size,
                advance,
                commands,
            }),
            _ => cx.lose(glyphs_path.clone(), "derived_glyph_incomplete"),
        }
    }
    let fonts_path = d.sub("fontMetaData");
    for f in d.array(cx, "fontMetaData") {
        let mut fo = Obj::of(f, fonts_path.clone());
        let (family, style) = match fo.obj(cx, "key") {
            Some(mut k) => {
                let family = k.string(cx, "family");
                let style = k.string(cx, "style");
                let _ = k.string(cx, "postscript");
                k.finish(cx);
                (family, style)
            }
            None => (None, None),
        };
        fo.finish(cx);
        out.fonts.push((family, style));
    }
    d.finish(cx);
    Ok(Some(out))
}

fn variable(o: &mut Obj, cx: &mut Cx) -> VariablePayload {
    let set_id = o.obj(cx, "variableSetID").and_then(|mut so| {
        let id = so.guid(cx, "guid");
        so.finish(cx);
        id
    });
    let resolved_type = o.enum_name(cx, "variableResolvedType");
    let mut values = Vec::new();
    if let Some(mut dv) = o.obj(cx, "variableDataValues") {
        let entries_path = dv.sub("entries");
        for e in dv.array(cx, "entries") {
            let mut eo = Obj::of(e, entries_path.clone());
            let mode = eo.guid(cx, "modeID");
            let value = eo.obj(cx, "variableData").and_then(|mut vd| {
                let _ = vd.enum_name(cx, "dataType");
                let _ = vd.enum_name(cx, "resolvedDataType");
                let v = variable_value(&mut vd, cx);
                vd.finish(cx);
                v
            });
            eo.finish(cx);
            if let (Some(mode), Some(value)) = (mode, value) {
                values.push((mode, value));
            }
        }
        dv.finish(cx);
    }
    let scopes_path = o.sub("variableScopes");
    let scopes = o
        .array(cx, "variableScopes")
        .iter()
        .filter_map(|s| {
            let n = s.as_enum_name().map(str::to_owned);
            if n.is_none() {
                cx.lose(scopes_path.clone(), "enum_value_unknown");
            }
            n
        })
        .collect();
    VariablePayload {
        set_id,
        resolved_type,
        values,
        scopes,
        resolved: None,
    }
}

fn interactions(o: &mut Obj, cx: &mut Cx) -> Vec<Interaction> {
    let path = o.sub("prototypeInteractions");
    o.array(cx, "prototypeInteractions")
        .iter()
        .map(|i| {
            let mut io = Obj::of(i, path.clone());
            let mut it = Interaction {
                id: io.guid(cx, "id"),
                deleted: io.bool(cx, "isDeleted"),
                ..Interaction::default()
            };
            if let Some(mut ev) = io.obj(cx, "event") {
                it.trigger = ev.enum_name(cx, "interactionType");
                it.timeout = ev.f32(cx, "transitionTimeout");
                ev.finish(cx);
            }
            let actions_path = io.sub("actions");
            for a in io.array(cx, "actions") {
                let mut ao = Obj::of(a, actions_path.clone());
                it.actions.push(InteractionAction {
                    navigation: ao.enum_name(cx, "navigationType"),
                    transition: ao.enum_name(cx, "transitionType"),
                    duration: ao.f32(cx, "transitionDuration"),
                    easing: ao.enum_name(cx, "easingType"),
                    easing_function: ao
                        .array(cx, "easingFunction")
                        .iter()
                        .filter_map(KiwiValue::as_f32)
                        .collect(),
                    connection: ao.enum_name(cx, "connectionType"),
                    destination: ao.guid(cx, "transitionNodeID"),
                    overlay_relative_position: ao.vec2(cx, "overlayRelativePosition"),
                    smart_animate: ao.bool(cx, "transitionShouldSmartAnimate"),
                });
                ao.finish(cx);
            }
            io.finish(cx);
            it
        })
        .collect()
}

/// Fields of `NodeChange` interpreted for every node kind (per-kind fields are read only for their
/// kinds; on any other kind they are loss entries).
pub const COMMON_NODE_FIELDS: &[&str] = &[
    "guid",
    "phase",
    "parentIndex",
    "type",
    "name",
    "visible",
    "opacity",
    "blendMode",
    "size",
    "transform",
    "mask",
    "fillPaints",
    "strokePaints",
    "strokeWeight",
    "strokeAlign",
    "strokeJoin",
    "strokeCap",
    "dashPattern",
    "miterLimit",
    "styleIdForEffect",
    "styleIdForFill",
    "styleIdForStrokeFill",
    "exportSettings",
    "effects",
    "fillGeometry",
    "strokeGeometry",
    "prototypeInteractions",
    "prototypeStartingPoint",
    // Grid placement (lane SMB-FIG-GRID-2): read on every kind, attached to the parent's grid.
    "gridRowAnchor",
    "gridColumnAnchor",
    "gridRowSpan",
    "gridColumnSpan",
    "gridChildVerticalAlign",
    "gridChildHorizontalAlign",
];

fn node(v: &KiwiValue, cx: &mut Cx) -> Result<Option<FigNode>> {
    let mut o = Obj::of(v, "NodeChange");
    let Some(id) = o.guid(cx, "guid") else {
        cx.current = None;
        cx.lose("NodeChange.guid", "node_without_guid");
        o.finish(cx);
        return Ok(None);
    };
    cx.current = Some(id);
    if o.enum_name(cx, "phase").as_deref() == Some("REMOVED") {
        cx.lose("NodeChange.phase=REMOVED", "node_removed");
        return Ok(None);
    }
    let (mut parent, mut position) = (None, None);
    if let Some(mut pi) = o.obj(cx, "parentIndex") {
        parent = pi.guid(cx, "guid");
        position = pi.string(cx, "position");
        pi.finish(cx);
    }
    let type_name = o
        .enum_name(cx, "type")
        .unwrap_or_else(|| "(untyped)".to_owned());
    let fills = paints(&mut o, cx, "fillPaints");
    let dash_values = o.array(cx, "dashPattern");
    let dash_pattern: Vec<f32> = dash_values
        .iter()
        .filter_map(KiwiValue::as_f32)
        .filter(|v| v.is_finite())
        .collect();
    if dash_pattern.len() != dash_values.len() {
        cx.lose("NodeChange.dashPattern", "field_type_unexpected");
    }
    let mut n = FigNode {
        id,
        parent,
        position,
        children: Vec::new(),
        name: o.string(cx, "name"),
        visible: o.bool(cx, "visible"),
        opacity: o.f32(cx, "opacity"),
        blend_mode: o.enum_name(cx, "blendMode"),
        transform: o.matrix(cx, "transform"),
        size: o.vec2(cx, "size"),
        mask: o.bool(cx, "mask"),
        strokes: paints(&mut o, cx, "strokePaints"),
        stroke_weight: o.f32(cx, "strokeWeight"),
        stroke_align: o.enum_name(cx, "strokeAlign"),
        stroke_join: o.enum_name(cx, "strokeJoin"),
        stroke_cap: o.enum_name(cx, "strokeCap"),
        dash_pattern,
        miter_limit: o.f32(cx, "miterLimit"),
        effect_style: style_id(&mut o, cx, "styleIdForEffect"),
        fill_style: style_id(&mut o, cx, "styleIdForFill"),
        stroke_style: style_id(&mut o, cx, "styleIdForStrokeFill"),
        export_settings: export_settings(&mut o, cx),
        effects: effects(&mut o, cx),
        fill_geometry: geometry(&mut o, cx, "fillGeometry")?,
        stroke_geometry: geometry(&mut o, cx, "strokeGeometry")?,
        interactions: interactions(&mut o, cx),
        flow_start: None,
        flow_position: None,
        payload: NodePayload::None,
        type_name,
        fills,
    };
    if let Some(mut sp) = o.obj(cx, "prototypeStartingPoint") {
        n.flow_start = sp.string(cx, "name");
        n.flow_position = sp.string(cx, "position");
        sp.finish(cx);
    }
    if let Some(gc) = grid_child(&mut o, cx) {
        cx.grid_children.push((id, gc));
    }
    n.payload = match n.type_name.as_str() {
        "DOCUMENT" | "GROUP" | "LINE" => NodePayload::None,
        "CANVAS" => NodePayload::Canvas {
            background: o.color(cx, "backgroundColor"),
            background_enabled: o.bool(cx, "backgroundEnabled"),
            internal_only: o.bool(cx, "internalOnly"),
        },
        "FRAME" | "SECTION" => NodePayload::Frame(frame(&mut o, cx)?),
        "RECTANGLE" | "ROUNDED_RECTANGLE" => {
            let (corner_radius, corner_radii) = corners(&mut o, cx);
            NodePayload::Rectangle {
                corner_radius,
                corner_radii,
            }
        }
        "ELLIPSE" => {
            let (mut s, mut e, mut r) = (None, None, None);
            if let Some(mut ao) = o.obj(cx, "arcData") {
                s = ao.f32(cx, "startingAngle");
                e = ao.f32(cx, "endingAngle");
                r = ao.f32(cx, "innerRadius");
                ao.finish(cx);
            }
            NodePayload::Ellipse {
                start_angle: s,
                end_angle: e,
                inner_radius: r,
            }
        }
        "STAR" | "REGULAR_POLYGON" => NodePayload::Star {
            count: o.u32(cx, "count"),
            inner_scale: o.f32(cx, "starInnerScale"),
        },
        "VECTOR" => {
            let (mut network, mut normalized_size) = (None, None);
            if let Some(mut vd) = o.obj(cx, "vectorData") {
                normalized_size = vd.vec2(cx, "normalizedSize");
                if let Some(i) = vd.u32(cx, "vectorNetworkBlob") {
                    let path = vd.sub("vectorNetworkBlob");
                    if let Some(b) = cx.blob(i, &path) {
                        match decode_network(b, cx)? {
                            Ok((net, trailing)) => {
                                if trailing {
                                    cx.lose(path.clone(), "vector_network_trailing_bytes");
                                }
                                network = Some(net);
                            }
                            Err(code) => cx.lose(path, code),
                        }
                    }
                }
                vd.finish(cx);
            }
            NodePayload::Vector {
                network,
                normalized_size,
            }
        }
        "BOOLEAN_OPERATION" => NodePayload::Boolean {
            operation: o.enum_name(cx, "booleanOperation"),
        },
        "TEXT" => {
            let base = n.fills.clone();
            NodePayload::Text(text(&mut o, cx, &base)?)
        }
        "SYMBOL" => {
            let frame = frame(&mut o, cx)?;
            let specs_path = o.sub("variantPropSpecs");
            let variant = o
                .array(cx, "variantPropSpecs")
                .iter()
                .map(|s| {
                    let mut so = Obj::of(s, specs_path.clone());
                    let vp = VariantProp {
                        prop_def: so.guid(cx, "propDefId"),
                        value: so.string(cx, "value"),
                    };
                    so.finish(cx);
                    vp
                })
                .collect();
            NodePayload::Symbol {
                frame,
                variant,
                component_key: o.string(cx, "componentKey"),
            }
        }
        "INSTANCE" => NodePayload::Instance {
            frame: frame(&mut o, cx)?,
            instance: instance(&mut o, cx)?,
        },
        "VARIABLE" => NodePayload::Variable(variable(&mut o, cx)),
        "VARIABLE_SET" => {
            let modes_path = o.sub("variableSetModes");
            let modes = o
                .array(cx, "variableSetModes")
                .iter()
                .filter_map(|m| {
                    let mut mo = Obj::of(m, modes_path.clone());
                    let mode = mo.guid(cx, "id").map(|id| VariableMode {
                        id,
                        name: mo.string(cx, "name"),
                    });
                    mo.finish(cx);
                    mode
                })
                .collect();
            NodePayload::VariableSet { modes }
        }
        other => {
            cx.lose(
                format!("NodeChange.type={other}"),
                "node_kind_payload_not_decoded",
            );
            NodePayload::None
        }
    };
    o.finish(cx);
    Ok(Some(n))
}

fn resolve_variables(doc: &mut FigDocument, cx: &mut Cx) {
    let index: HashMap<SourceId, usize> = doc
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut resolved = Vec::new();
    for (i, n) in doc.nodes.iter().enumerate() {
        let NodePayload::Variable(var) = &n.payload else {
            continue;
        };
        cx.current = Some(n.id);
        let mut cur = var;
        let mut out = None;
        let mut done = false;
        for _ in 0..16 {
            let modes = cur
                .set_id
                .and_then(|s| index.get(&s))
                .map(|&si| &doc.nodes[si].payload);
            let Some(NodePayload::VariableSet { modes }) = modes else {
                cx.lose("VARIABLE.variableSetID", "variable_set_missing");
                done = true;
                break;
            };
            if modes.len() != 1 {
                cx.lose(
                    "VARIABLE.variableSetModes",
                    "variable_multi_mode_unresolved",
                );
                done = true;
                break;
            }
            match cur.values.iter().find(|(m, _)| *m == modes[0].id) {
                Some((_, VariableValue::Alias(target))) => {
                    match index.get(target).map(|&ti| &doc.nodes[ti].payload) {
                        Some(NodePayload::Variable(next)) => cur = next,
                        _ => {
                            cx.lose("VARIABLE.alias", "variable_alias_unresolved");
                            done = true;
                            break;
                        }
                    }
                }
                Some((_, v)) => {
                    out = Some(v.clone());
                    done = true;
                    break;
                }
                None => {
                    cx.lose("VARIABLE.variableDataValues", "variable_mode_value_missing");
                    done = true;
                    break;
                }
            }
        }
        if !done {
            // 16 alias hops without a value: a loop or an over-long chain.
            cx.lose("VARIABLE.alias", "variable_alias_chain_too_long");
        }
        resolved.push((i, out));
    }
    for (i, out) in resolved {
        if let NodePayload::Variable(var) = &mut doc.nodes[i].payload {
            var.resolved = out;
        }
    }
}

/// Builds the typed node tree from a decoded `Message` value tree. `container` (when given) lets
/// image paints be matched to their `images/<hex(hash)>` parts.
pub fn document_from_tree(
    message: &KiwiValue,
    container: Option<&FigContainer>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<FigDocument> {
    cancel.check().map_err(|_| FigError::Canceled)?;
    let mut cx = Cx {
        loss: BTreeMap::new(),
        decoded: BTreeMap::new(),
        current: None,
        grid_children: Vec::new(),
        blobs: None,
        container,
        geometry_budget: limits.max_tree_values,
        cancel,
    };
    let mut root = Obj::of(message, "Message");
    // Blobs first: nodes refer to them by index.
    let base = root.u32(&mut cx, "blobBaseIndex").unwrap_or(0);
    let blob_values = root.array(&mut cx, "blobs");
    let mut blobs = Vec::with_capacity(blob_values.len());
    for b in blob_values {
        let mut bo = Obj::of(b, "Message.blobs");
        blobs.push(
            bo.typed(&mut cx, "bytes", KiwiValue::as_bytes)
                .unwrap_or_default(),
        );
        bo.finish(&mut cx);
    }
    let blob_count = blobs.len();
    if base == 0 {
        cx.blobs = Some(blobs);
    } else {
        // The meaning of a non-zero base (split/incremental messages) is UNVERIFIED: no blob of
        // this message is resolved rather than guessing an offset.
        cx.lose("Message.blobBaseIndex", "blob_base_index_unsupported");
    }
    let changes = root.array(&mut cx, "nodeChanges");
    if changes.len() > limits.max_nodes as usize {
        return Err(FigError::BudgetExceeded("nodes"));
    }
    let mut nodes: Vec<FigNode> = Vec::with_capacity(changes.len());
    let mut index: HashMap<SourceId, usize> = HashMap::with_capacity(changes.len());
    for (k, c) in changes.iter().enumerate() {
        if k & 255 == 0 {
            cancel.check().map_err(|_| FigError::Canceled)?;
        }
        let placements = cx.grid_children.len();
        let Some(n) = node(c, &mut cx)? else {
            continue;
        };
        if index.contains_key(&n.id) {
            cx.lose("NodeChange.guid", "duplicate_guid");
            // The dropped node's grid placement goes with it.
            cx.grid_children.truncate(placements);
            continue;
        }
        index.insert(n.id, nodes.len());
        nodes.push(n);
    }
    cx.current = None;
    root.finish(&mut cx);

    // Parent links and sibling order.
    let mut roots = Vec::new();
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        match n.parent {
            None => roots.push(i),
            Some(p) => match index.get(&p) {
                Some(&pi) if pi != i => kids[pi].push(i),
                _ => {
                    cx.current = Some(n.id);
                    cx.lose("NodeChange.parentIndex", "parent_missing");
                }
            },
        }
    }
    for list in &mut kids {
        list.sort_by(|a, b| {
            nodes[*a]
                .position
                .as_deref()
                .unwrap_or("")
                .as_bytes()
                .cmp(nodes[*b].position.as_deref().unwrap_or("").as_bytes())
                .then(nodes[*a].id.cmp(&nodes[*b].id))
        });
    }
    // Reachability from the roots: nodes on a parent cycle are reported, never silently kept.
    let mut seen = vec![false; nodes.len()];
    let mut queue: VecDeque<usize> = roots.iter().copied().collect();
    while let Some(i) = queue.pop_front() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        queue.extend(kids[i].iter().copied());
    }
    for (i, list) in kids.into_iter().enumerate() {
        nodes[i].children = list;
        if !seen[i] {
            cx.current = Some(nodes[i].id);
            cx.lose("NodeChange.parentIndex", "unreachable_node");
        }
    }
    attach_grid_children(&mut nodes, &index, &mut cx);
    let mut doc = FigDocument {
        nodes,
        roots,
        blob_count,
        loss: Vec::new(),
        decoded: std::mem::take(&mut cx.decoded),
        source_sha256: container.map(|c| c.source_sha256),
        image_parts: container
            .map(|c| {
                c.parts
                    .iter()
                    .filter(|p| p.name.starts_with(IMAGES_PREFIX))
                    .map(|p| (p.name.clone(), p.sha256))
                    .collect()
            })
            .unwrap_or_default(),
    };
    resolve_variables(&mut doc, &mut cx);
    doc.loss = cx
        .loss
        .into_iter()
        .map(|((path, code), (count, first))| NodeLoss {
            path,
            code,
            count,
            first,
        })
        .collect();
    Ok(doc)
}

/// Typed node tree of an imported (version-admitted) file, decoded against the file's own schema.
/// Editable import into the Studio native document is NOT part of this (`FigImport::editable`
/// stays NOT_PROVEN).
pub fn decode_document(
    import: &FigImport,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<FigDocument> {
    let (schema, message) = schema_and_message(&import.container, limits, cancel)?;
    let tree = decode_tree(&schema, &message, "Message", limits, cancel)?;
    document_from_tree(&tree, Some(&import.container), limits, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn field(name: &str, value: KiwiValue) -> KiwiField {
        KiwiField { name: Arc::from(name), id: 0, value }
    }

    /// F-R45-1 (lane SMB-R46-FOLLOWUPS-1): the GRID track and sizing loops poll the caller's token
    /// per entry, so one hostile node no longer runs to its end after a cancel (document_from_tree
    /// polls only between nodes).
    #[test]
    fn grid_entry_loops_are_cancellable_inside_one_node() {
        let entry = KiwiValue::Message(vec![field("position", KiwiValue::String("!".into()))]);
        let map = KiwiValue::Message(vec![field("entries", KiwiValue::Array(vec![entry.clone(), entry]))]);
        let node = KiwiValue::Message(vec![field("gridRows", map.clone()), field("gridRowsSizing", map)]);
        let canceled = CancellationToken::default();
        canceled.cancel();
        let live = CancellationToken::default();
        let mut cx = Cx {
            loss: BTreeMap::new(),
            decoded: BTreeMap::new(),
            current: None,
            grid_children: Vec::new(),
            blobs: None,
            container: None,
            geometry_budget: 1 << 20,
            cancel: &canceled,
        };
        let mut o = Obj::of(&node, "NodeChange");
        assert!(matches!(grid_tracks(&mut o, &mut cx, "gridRows"), Err(FigError::Canceled)));
        assert!(matches!(grid_sizes(&mut o, &mut cx, "gridRowsSizing", &mut []), Err(FigError::Canceled)));
        cx.cancel = &live;
        let mut o = Obj::of(&node, "NodeChange");
        assert_eq!(grid_tracks(&mut o, &mut cx, "gridRows").map(|t| t.len()).ok(), Some(0));
        assert!(cx.loss.contains_key(&("NodeChange.gridRows".to_owned(), "grid_track_incomplete")));
    }
}
