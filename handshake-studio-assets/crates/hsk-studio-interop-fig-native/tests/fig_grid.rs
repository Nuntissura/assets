//! Lane SMB-FIG-GRID-2 slice 2: a `.fig` GRID frame maps to Folio `StudioAutoLayout::grid` and its
//! children's `StudioChildLayout::grid_child` (STU-DS-146/147/150, bounds of lane SMB-FIG-GRID-1);
//! malformed or out-of-range grid fields are typed `auto_layout_grid_*` losses and the frame falls
//! back to no auto layout (NONE); the components solver runs on imported trees and its child rects
//! are compared with the geometry Figma stored; non-grid files import exactly as before.
//!
//! The Operator fixtures (`hsk-studio-interop-fig/tests/fixtures`, Figma desktop 126.10.7) hold no
//! GRID frame, so GRID frames are fixture frames with their message fields patched in the decoded
//! value tree, every name, field id and enum value taken from the fixture's embedded kiwi schema
//! (the builder panics when the schema does not declare one). Figma's grid behaviour is UNVERIFIED
//! (D5): only flow frames are compared with Figma's own stored output.
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{Design, LayoutLimits, LocalContent, NodeLocal, RectPt, solve_auto_layout};
use hsk_studio_folio::{
    Budget, ChildAlign, ChildPositioning, ChildSizing, ConstraintSchema, ConstraintType, GridAutoTracks, GridItemsPositioning, LayoutMode, Resolution, Resolver, StudioChildLayout, StudioConstraint, StudioGridChild, StudioGridLayout, StudioGridTrackSize, StudioNodeDesign, TileRef,
    validate_design_records,
};
use hsk_studio_interop_fig_base::{
    DefinitionSummary, FigDocument, FigImport, FigOutcome, KiwiField, KiwiValue, Limits, SchemaSummary, SourceId, VersionPolicy, decode_document,
    decode_message_tree, document_from_tree, import_fig,
};
use hsk_studio_interop_fig_native::{NativeConfig, NativeProposal, SequentialIdMint, to_native};
use std::collections::BTreeMap;
use std::sync::Arc;

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

fn fixture(name: &str) -> FigImport {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-interop-fig/tests/fixtures").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &CancellationToken::default()).unwrap() else {
        panic!("fixture imports")
    };
    *import
}

fn map(doc: &FigDocument) -> NativeProposal {
    let cfg = NativeConfig { budget: Budget::default(), resolver: &Host };
    to_native(doc, &cfg, &mut SequentialIdMint::new(0x6121), &CancellationToken::default()).unwrap_or_else(|e| panic!("to_native: {e:?}"))
}

/// Kiwi values with the names, ids and enum values of the fixture's own schema.
struct Kiwi<'a>(&'a SchemaSummary);

impl Kiwi<'_> {
    fn def(&self, name: &str) -> &DefinitionSummary {
        self.0.definitions.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("the fixture schema declares no {name}"))
    }
    fn field(&self, def: &str, name: &str, value: KiwiValue) -> KiwiField {
        let f = self.def(def).fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("the fixture schema declares no {def}.{name}"));
        KiwiField { name: Arc::from(name), id: f.id, value }
    }
    fn member(&self, def: &str, name: &str, member: &str) -> KiwiValue {
        let ty = self.def(def).fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no {def}.{name}")).type_name.clone();
        let m = self.def(&ty).fields.iter().find(|f| f.name == member).unwrap_or_else(|| panic!("the fixture schema declares no {ty}.{member}"));
        KiwiValue::Enum { value: m.id, name: Some(Arc::from(member)) }
    }
    fn guid(&self, local: u32) -> KiwiValue {
        KiwiValue::Struct(vec![self.field("GUID", "sessionID", KiwiValue::Uint(4)), self.field("GUID", "localID", KiwiValue::Uint(local))])
    }
    fn tracks(&self, ids: &[u32]) -> KiwiValue {
        let list = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let pos = char::from(b'!' + u8::try_from(i % 90).unwrap()).to_string() + &"~".repeat(i / 90);
                KiwiValue::Message(vec![self.field("GUIDPositionMapEntry", "id", self.guid(*id)), self.field("GUIDPositionMapEntry", "position", KiwiValue::String(pos))])
            })
            .collect();
        KiwiValue::Message(vec![self.field("GUIDPositionMap", "entries", KiwiValue::Array(list))])
    }
    /// A `GUIDGridTrackSizeMap` with min = max = (kind, value) per track.
    fn sizes(&self, entries: &[(u32, &str, Option<f32>)]) -> KiwiValue {
        let sizing = |kind: &str, value: Option<f32>| {
            let mut f = vec![self.field("GridTrackSizingFunction", "type", self.member("GridTrackSizingFunction", "type", kind))];
            if let Some(v) = value {
                f.push(self.field("GridTrackSizingFunction", "value", KiwiValue::Float(v)));
            }
            KiwiValue::Message(f)
        };
        let list = entries
            .iter()
            .map(|(id, kind, value)| {
                let size = KiwiValue::Message(vec![self.field("GridTrackSize", "minSizing", sizing(kind, *value)), self.field("GridTrackSize", "maxSizing", sizing(kind, *value))]);
                KiwiValue::Message(vec![self.field("GUIDGridTrackSizeMapEntry", "id", self.guid(*id)), self.field("GUIDGridTrackSizeMapEntry", "trackSize", size)])
            })
            .collect();
        KiwiValue::Message(vec![self.field("GUIDGridTrackSizeMap", "entries", KiwiValue::Array(list))])
    }
    /// Turns node `name` into a GRID frame with these rows and columns (all tracks min = max).
    fn grid(&self, tree: &mut KiwiValue, name: &str, rows: &[(u32, &str, Option<f32>)], columns: &[(u32, &str, Option<f32>)], gaps: (f32, f32)) {
        let frame = node_fields(tree, name);
        set(frame, self.field("NodeChange", "stackMode", self.member("NodeChange", "stackMode", "GRID")));
        set(frame, self.field("NodeChange", "gridRows", self.tracks(&rows.iter().map(|r| r.0).collect::<Vec<_>>())));
        set(frame, self.field("NodeChange", "gridColumns", self.tracks(&columns.iter().map(|r| r.0).collect::<Vec<_>>())));
        set(frame, self.field("NodeChange", "gridRowsSizing", self.sizes(rows)));
        set(frame, self.field("NodeChange", "gridColumnsSizing", self.sizes(columns)));
        set(frame, self.field("NodeChange", "gridRowGap", KiwiValue::Float(gaps.0)));
        set(frame, self.field("NodeChange", "gridColumnGap", KiwiValue::Float(gaps.1)));
    }
    /// Anchors node `name` at (row track, column track) with spans.
    fn anchor(&self, tree: &mut KiwiValue, name: &str, row: u32, column: u32, spans: (u32, u32)) {
        let n = node_fields(tree, name);
        set(n, self.field("NodeChange", "gridRowAnchor", self.guid(row)));
        set(n, self.field("NodeChange", "gridColumnAnchor", self.guid(column)));
        set(n, self.field("NodeChange", "gridRowSpan", KiwiValue::Uint(spans.0)));
        set(n, self.field("NodeChange", "gridColumnSpan", KiwiValue::Uint(spans.1)));
    }
}

fn node_fields<'a>(tree: &'a mut KiwiValue, name: &str) -> &'a mut Vec<KiwiField> {
    let KiwiValue::Message(root) = tree else { panic!("message root") };
    let changes = root.iter_mut().find(|f| &*f.name == "nodeChanges").expect("nodeChanges");
    let KiwiValue::Array(nodes) = &mut changes.value else { panic!("nodeChanges array") };
    nodes
        .iter_mut()
        .find_map(|n| match n {
            KiwiValue::Message(f) if f.iter().any(|x| &*x.name == "name" && x.value.as_str() == Some(name)) => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node {name}"))
}

fn set(fields: &mut Vec<KiwiField>, f: KiwiField) {
    fields.retain(|x| x.name != f.name);
    fields.push(f);
}

/// `fig-components-autolayout.fig` with `patch` applied to its decoded message tree.
fn patched(patch: impl FnOnce(&Kiwi, &mut KiwiValue)) -> FigDocument {
    let import = fixture("fig-components-autolayout.fig");
    let cancel = CancellationToken::default();
    let mut tree = decode_message_tree(&import, &Limits::default(), &cancel).unwrap();
    patch(&Kiwi(import.schema_summary.as_ref().expect("fixture schema")), &mut tree);
    document_from_tree(&tree, Some(&import.container), &Limits::default(), &cancel).unwrap()
}

const ROW_A: u32 = 101;
const ROW_B: u32 = 100;
const COL_A: u32 = 110;
const COL_B: u32 = 111;
const COL_C: u32 = 112;

/// `horizontal-stack` (4:15, three instance children) as a 2 x 3 GRID.
fn two_by_three(k: &Kiwi, tree: &mut KiwiValue) {
    k.grid(tree, "horizontal-stack", &[(ROW_A, "FIXED", Some(40.0)), (ROW_B, "HUG", None)], &[(COL_A, "FLEX", Some(1.0)), (COL_B, "FIXED", Some(100.0)), (COL_C, "HUG", None)], (8.0, 12.0));
    let frame = node_fields(tree, "horizontal-stack");
    set(frame, k.field("NodeChange", "gridAutoTracks", k.member("NodeChange", "gridAutoTracks", "ROWS")));
    k.anchor(tree, "button-instance-1", ROW_A, COL_A, (1, 1));
    let one = node_fields(tree, "button-instance-1");
    set(one, k.field("NodeChange", "gridChildVerticalAlign", k.member("NodeChange", "gridChildVerticalAlign", "CENTER")));
    k.anchor(tree, "button-instance-2", ROW_B, COL_B, (1, 2));
}

fn layer_named(p: &NativeProposal, name: &str) -> String {
    p.snapshot.document().layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id.clone()
}

fn design<'a>(p: &'a NativeProposal, node: &str) -> Option<&'a StudioNodeDesign> {
    p.design.nodes.iter().find(|d| d.node.node_id == node)
}

/// The layer standing for document instance `local` among its parent's children: its expansion group.
fn expansion(p: &NativeProposal, local: u32) -> String {
    p.expansions.iter().find(|e| e.instance == SourceId { session: 4, local }).expect("expansion").group_layer_id.clone()
}

fn grid_losses(p: &NativeProposal) -> Vec<&'static str> {
    p.mapping_loss.iter().filter(|l| l.path.starts_with("native.auto_layout.grid")).map(|l| l.code).collect()
}

#[test]
fn grid_frame_imports_with_counts_sizes_gaps_and_anchors() {
    let p = map(&patched(two_by_three));
    let frame = layer_named(&p, "horizontal-stack");
    let a = design(&p, &frame).and_then(|d| d.auto_layout.as_ref()).expect("auto layout");
    assert_eq!(a.layout_mode, LayoutMode::Grid);
    assert_eq!(
        a.grid,
        Some(StudioGridLayout {
            grid_row_count: 2,
            grid_column_count: 3,
            grid_row_gap: 8.0,
            grid_column_gap: 12.0,
            grid_row_sizes: vec![StudioGridTrackSize::fixed(40.0), StudioGridTrackSize::hug()],
            grid_column_sizes: vec![StudioGridTrackSize::flex(1.0), StudioGridTrackSize::fixed(100.0), StudioGridTrackSize::hug()],
            grid_auto_tracks: GridAutoTracks::Rows,
            grid_items_positioning: GridItemsPositioning::RowAutoFlow,
        })
    );
    // Anchors resolve to track indices; the instance's anchor rides on its expansion group (the
    // layer standing for it among the frame's children); the unanchored child auto-flows.
    let child = |align, gc| StudioChildLayout {
        layout_align: align,
        layout_grow: 0,
        layout_positioning: ChildPositioning::Auto,
        layout_sizing_horizontal: ChildSizing::Fixed,
        layout_sizing_vertical: ChildSizing::Fixed,
        grid_child: Some(gc),
    };
    assert_eq!(
        design(&p, &expansion(&p, 16)).and_then(|d| d.child_layout.clone()),
        Some(child(ChildAlign::Center, StudioGridChild { grid_row_anchor_index: 0, grid_column_anchor_index: 0, grid_row_span: 1, grid_column_span: 1 }))
    );
    assert_eq!(
        design(&p, &expansion(&p, 18)).and_then(|d| d.child_layout.clone()),
        Some(child(ChildAlign::Inherit, StudioGridChild { grid_row_anchor_index: 1, grid_column_anchor_index: 1, grid_row_span: 1, grid_column_span: 2 }))
    );
    assert!(design(&p, &expansion(&p, 20)).and_then(|d| d.child_layout.as_ref()).is_none());
    // The frame's background layer (its fill) is not a grid item: absolute, stretched with the frame.
    for b in p.frame_backgrounds.iter().filter(|b| b.frame.node_id == frame) {
        let d = design(&p, &b.layer_id).expect("background design");
        assert_eq!(d.child_layout.as_ref().map(|k| (k.layout_positioning, k.grid_child)), Some((ChildPositioning::Absolute, None)));
        assert_eq!(d.constraints.as_ref().map(|c| (c.horizontal, c.vertical)), Some((ConstraintType::Stretch, ConstraintType::Stretch)));
    }
    validate_design_records(&p.design, p.snapshot.document(), Budget::default(), &CancellationToken::default(), &Host).expect("folio accepts the grid records");
    // Declared readings are typed losses: no items-positioning field in the schema.
    assert_eq!(grid_losses(&p), ["auto_layout_grid_items_positioning_default"]);
}

#[test]
fn malformed_or_hostile_grid_fields_fall_back_to_none_with_typed_losses() {
    let cases: Vec<(&str, Box<dyn Fn(&Kiwi, &mut KiwiValue)>)> = vec![
        // FIXED without a value.
        ("auto_layout_grid_track_value", Box::new(|k, t| k.grid(t, "horizontal-stack", &[(ROW_A, "HUG", None)], &[(COL_A, "FIXED", None)], (0.0, 0.0)))),
        // 1001 columns: refused by count before any track list is built (STU-DS-146 max 1000).
        (
            "auto_layout_grid_count",
            Box::new(|k, t| {
                let cols: Vec<(u32, &str, Option<f32>)> = (0..1001).map(|i| (1000 + i, "HUG", None)).collect();
                k.grid(t, "horizontal-stack", &[(ROW_A, "HUG", None)], &cols, (0.0, 0.0));
            }),
        ),
        // A span reaching u32::MAX: checked, never wrapping.
        (
            "auto_layout_grid_span",
            Box::new(|k, t| {
                k.grid(t, "horizontal-stack", &[(ROW_A, "HUG", None)], &[(COL_A, "HUG", None), (COL_B, "HUG", None)], (0.0, 0.0));
                k.anchor(t, "button-instance-1", ROW_A, COL_B, (1, u32::MAX));
            }),
        ),
        // An anchor naming no track of the frame.
        (
            "auto_layout_grid_anchor_unresolved",
            Box::new(|k, t| {
                k.grid(t, "horizontal-stack", &[(ROW_A, "HUG", None)], &[(COL_A, "HUG", None)], (0.0, 0.0));
                k.anchor(t, "button-instance-1", ROW_A, 999, (1, 1));
            }),
        ),
        // A negative gap.
        ("auto_layout_grid_gap", Box::new(|k, t| k.grid(t, "horizontal-stack", &[(ROW_A, "HUG", None)], &[(COL_A, "HUG", None)], (-1.0, 0.0)))),
    ];
    for (code, patch) in cases {
        let p = map(&patched(|k, t| patch(k, t)));
        assert!(grid_losses(&p).contains(&code), "{code}: {:?}", grid_losses(&p));
        let frame = layer_named(&p, "horizontal-stack");
        assert!(design(&p, &frame).and_then(|d| d.auto_layout.as_ref()).is_none(), "{code}: falls back to NONE");
        assert!(p.design.nodes.iter().all(|d| d.child_layout.as_ref().is_none_or(|k| k.grid_child.is_none())), "{code}: no grid child");
    }
}

/// Authored rects (parent-local) of every layer under `root` from Figma's stored transforms and
/// sizes; a frame background (`frame_backgrounds`) is the frame's own box, made absolute and
/// stretched here (as the GRID mapping writes it) so it is not a flow item.
fn local_and_records(p: &NativeProposal, doc: &FigDocument) -> (LocalContent, hsk_studio_folio::StudioDesignRecords) {
    let mut local = LocalContent::default();
    let mut records = p.design.clone();
    let rect_of = |id: SourceId| {
        let n = doc.node(id)?;
        let (t, s) = (n.transform?, n.size?);
        Some(RectPt { x: f64::from(t.m02), y: f64::from(t.m12), w: f64::from(s.x), h: f64::from(s.y) })
    };
    for e in &p.id_map {
        if let Some(r) = rect_of(e.source) {
            local.nodes.insert(e.node.node_id.clone(), NodeLocal { rect: Some(r), ..NodeLocal::default() });
        }
    }
    for b in &p.frame_backgrounds {
        let Some(r) = rect_of(b.source) else { continue };
        local.nodes.insert(b.layer_id.clone(), NodeLocal { rect: Some(RectPt { x: 0.0, y: 0.0, ..r }), ..NodeLocal::default() });
        let k = StudioChildLayout {
            layout_align: ChildAlign::Inherit,
            layout_grow: 0,
            layout_positioning: ChildPositioning::Absolute,
            layout_sizing_horizontal: ChildSizing::Fixed,
            layout_sizing_vertical: ChildSizing::Fixed,
            grid_child: None,
        };
        let stretch = StudioConstraint { schema_id: ConstraintSchema::V1, horizontal: ConstraintType::Stretch, vertical: ConstraintType::Stretch };
        match records.nodes.iter_mut().find(|d| d.node.node_id == b.layer_id) {
            Some(d) => {
                d.child_layout.get_or_insert(k);
                d.constraints.get_or_insert(stretch);
            }
            None => {
                let mut d: StudioNodeDesign = serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": b.layer_id}})).unwrap();
                d.child_layout = Some(k);
                d.constraints = Some(stretch);
                records.nodes.push(d);
            }
        }
    }
    (local, records)
}

/// The Folio node of a top-level frame: an artboard, or a group layer (a component set on a page).
fn root_node(p: &NativeProposal, name: &str) -> String {
    let doc = p.snapshot.document();
    doc.artboards
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.artboard_id.clone())
        .or_else(|| doc.layers.iter().find(|l| l.name == name).map(|l| l.layer_id.clone()))
        .unwrap_or_else(|| panic!("root {name}"))
}

/// Solves the tree under the top-level frame `root` and returns each named layer's solved parent-local rect.
fn solve(p: &NativeProposal, doc: &FigDocument, root: &str, names: &[&str]) -> Vec<RectPt> {
    let (local, records) = local_and_records(p, doc);
    let design = Design::new(p.snapshot.document(), &records, &local).unwrap();
    let solved = solve_auto_layout(&design, &root_node(p, root), None, &LayoutLimits::default(), &CancellationToken::default()).unwrap_or_else(|e| panic!("solve {root}: {e:?}"));
    names.iter().map(|n| solved.get(&layer_named(p, n)).unwrap_or_else(|| panic!("box {n}")).rect).collect()
}

/// Tolerance of the solved-vs-stored comparison: 0.01 px (the stored geometry is f32 and whole here).
const TOL: f64 = 0.01;

fn stored(doc: &FigDocument, name: &str) -> (f64, f64) {
    let t = doc.named(name).next().and_then(|n| n.transform).unwrap_or_else(|| panic!("{name} transform"));
    (f64::from(t.m02), f64::from(t.m12))
}

#[test]
fn solved_child_rects_match_the_geometry_figma_stored() {
    // Flow frames: Figma's own auto-layout output is the stored child geometry (FIXTURE_PROVEN).
    // fixture-button: HORIZONTAL frame (the component set), padding 24, spacing 24; its two
    // components are HORIZONTAL frames (padding 20 / 12) holding one TEXT each.
    let doc = decode_document(&fixture("fig-components-autolayout.fig"), &Limits::default(), &CancellationToken::default()).unwrap();
    let p = map(&doc);
    let solved = solve(&p, &doc, "fixture-button", &["state=default", "state=hover"]);
    for (rect, name) in solved.iter().zip(["state=default", "state=hover"]) {
        let (x, y) = stored(&doc, name);
        assert!((rect.x - x).abs() <= TOL && (rect.y - y).abs() <= TOL, "{name}: solved {rect:?} stored ({x}, {y})");
    }
    println!("FIXTURE_PROVEN: fig_grid solved_vs_stored fixture-button (HORIZONTAL) 2 children within {TOL} px");
}

#[test]
fn grid_frame_solves_from_its_imported_records() {
    // fixture-button as a one-row, two-column HUG grid with column gap 24 solves to the same cells
    // (Studio reading, hand-checked: 24 and 24 + 93 + 24 = 141). Figma's own GRID output cannot be
    // compared: no Operator fixture holds a GRID frame.
    let grid_doc = patched(|k, t| k.grid(t, "fixture-button", &[(ROW_A, "HUG", None)], &[(COL_A, "HUG", None), (COL_B, "HUG", None)], (0.0, 24.0)));
    let gp = map(&grid_doc);
    let cells = solve(&gp, &grid_doc, "fixture-button", &["state=default", "state=hover"]);
    assert_eq!(cells.iter().map(|r| (r.x, r.y)).collect::<Vec<_>>(), [(24.0, 24.0), (141.0, 24.0)]);
    println!("NOT_PROVEN: fig_grid solved_vs_stored GRID reason=no GRID frame in the Operator fixtures (fig-basic-shapes, fig-components-autolayout, fig-prototype-pages); Figma grid layout UNVERIFIED (D5)");
}

/// Digest of everything `to_native` produces for a file (document, design records, losses).
fn digest(p: &NativeProposal) -> String {
    let doc = serde_json::to_vec(p.snapshot.document()).unwrap();
    let design = serde_json::to_vec(&p.design).unwrap();
    let rest = format!("{:?}|{:?}|{:?}|{:?}", p.mapping_loss, p.decode_loss, p.id_map, p.frame_backgrounds);
    hsk_studio_package::sha256_hex_of(&[doc, design, rest.into_bytes()].concat())
}

#[test]
fn non_grid_files_import_byte_identically() {
    // Pinned at the lane base 91cb8889 (before the GRID decode and mapping).
    let pinned: BTreeMap<&str, &str> = BTreeMap::from([
        ("fig-basic-shapes.fig", "73725462aba9de4df8ee98e6d6fc90757edc17f09c48c22a4282ba467418fbb0"),
        ("fig-components-autolayout.fig", "9487631e88f2ce1c5de96155516c072780a902b64a0144ccab66b3c5b9a1b388"),
        ("fig-prototype-pages.fig", "f95f4c06e433bf6f8751201afeb1112792f9ce88764a996c0e8b24e5aa5967b6"),
    ]);
    let got: BTreeMap<&str, String> = pinned
        .keys()
        .map(|name| (*name, digest(&map(&decode_document(&fixture(name), &Limits::default(), &CancellationToken::default()).unwrap()))))
        .collect();
    let want: BTreeMap<&str, String> = pinned.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    assert_eq!(got, want);
}
