//! Lane SMB-FIG-GRID-2 slice 1: the GRID container and child fields of a `.fig` message decode into
//! `AutoLayout::grid` ([`FigGrid`]), with every field name, field id and enum value taken from the
//! kiwi schema embedded in the Operator fixture `fig-components-autolayout.fig` (Figma desktop
//! 126.10.7; `hsk-studio-interop-fig/tests/fixtures`). No fixture node uses GRID, so the GRID frame is
//! the fixture's own `horizontal-stack` frame with its message fields patched in the decoded value
//! tree (the builder panics when the fixture schema does not declare a name). Figma's own grid
//! behaviour is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_fig_base::nodes::{FigGrid, FigGridChild, FigTrackSize, FigTrackSizing};
use hsk_studio_interop_fig_base::{
    DefinitionSummary, FigDocument, FigImport, FigOutcome, KiwiField, KiwiValue, Limits, NodePayload, SchemaSummary, SourceId, VersionPolicy,
    decode_document, decode_message_tree, document_from_tree, import_fig,
};
use std::sync::Arc;

fn fixture(name: &str) -> FigImport {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-interop-fig/tests/fixtures").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &CancellationToken::default()).unwrap() else {
        panic!("fixture imports")
    };
    *import
}

/// One `GridTrackSizingFunction` as (type, value).
type Sizing<'a> = Option<(&'a str, Option<f32>)>;

/// Builds kiwi values with the names, ids and enum values of the fixture's own schema.
struct Kiwi<'a>(&'a SchemaSummary);

impl Kiwi<'_> {
    fn def(&self, name: &str) -> &DefinitionSummary {
        self.0.definitions.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("the fixture schema declares no {name}"))
    }
    fn field(&self, def: &str, name: &str, value: KiwiValue) -> KiwiField {
        let f = self.def(def).fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("the fixture schema declares no {def}.{name}"));
        KiwiField { name: Arc::from(name), id: f.id, value }
    }
    /// The enum member `member` of the type of field `def.name`.
    fn member(&self, def: &str, name: &str, member: &str) -> KiwiValue {
        let ty = self.def(def).fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no {def}.{name}")).type_name.clone();
        let e = self.def(&ty);
        assert_eq!(e.kind, "enum", "{ty}");
        let m = e.fields.iter().find(|f| f.name == member).unwrap_or_else(|| panic!("the fixture schema declares no {ty}.{member}"));
        KiwiValue::Enum { value: m.id, name: Some(Arc::from(member)) }
    }
    fn guid(&self, local: u32) -> KiwiValue {
        KiwiValue::Struct(vec![self.field("GUID", "sessionID", KiwiValue::Uint(4)), self.field("GUID", "localID", KiwiValue::Uint(local))])
    }
    /// A `GUIDPositionMap` of (track local id, position).
    fn tracks(&self, entries: &[(u32, &str)]) -> KiwiValue {
        let list = entries
            .iter()
            .map(|(id, pos)| KiwiValue::Message(vec![self.field("GUIDPositionMapEntry", "id", self.guid(*id)), self.field("GUIDPositionMapEntry", "position", KiwiValue::String((*pos).into()))]))
            .collect();
        KiwiValue::Message(vec![self.field("GUIDPositionMap", "entries", KiwiValue::Array(list))])
    }
    fn sizing(&self, kind: &str, value: Option<f32>) -> KiwiValue {
        let mut f = vec![self.field("GridTrackSizingFunction", "type", self.member("GridTrackSizingFunction", "type", kind))];
        if let Some(v) = value {
            f.push(self.field("GridTrackSizingFunction", "value", KiwiValue::Float(v)));
        }
        KiwiValue::Message(f)
    }
    /// A `GUIDGridTrackSizeMap` of (track local id, min, max).
    fn sizes(&self, entries: &[(u32, Sizing, Sizing)]) -> KiwiValue {
        let list = entries
            .iter()
            .map(|(id, min, max)| {
                let mut size = Vec::new();
                if let Some((k, v)) = min {
                    size.push(self.field("GridTrackSize", "minSizing", self.sizing(k, *v)));
                }
                if let Some((k, v)) = max {
                    size.push(self.field("GridTrackSize", "maxSizing", self.sizing(k, *v)));
                }
                KiwiValue::Message(vec![self.field("GUIDGridTrackSizeMapEntry", "id", self.guid(*id)), self.field("GUIDGridTrackSizeMapEntry", "trackSize", KiwiValue::Message(size))])
            })
            .collect();
        KiwiValue::Message(vec![self.field("GUIDGridTrackSizeMap", "entries", KiwiValue::Array(list))])
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

const ROW_A: u32 = 101;
const ROW_B: u32 = 100;
const COL_A: u32 = 110;
const COL_B: u32 = 111;
const COL_C: u32 = 112;

/// `horizontal-stack` (4:15) as a 2 x 3 GRID: rows stored out of id order (4:101 at "!", 4:100 at
/// "\""), row gap 8, column gap 12, auto tracks ROWS; children 4:16 anchored at (row 4:101, column
/// 4:110) centred vertically, 4:18 at (row 4:100, column 4:111) spanning two columns, 4:20 not
/// anchored. `patch` edits the tree further before decoding.
fn grid_document(patch: impl FnOnce(&Kiwi, &mut KiwiValue)) -> FigDocument {
    let import = fixture("fig-components-autolayout.fig");
    let cancel = CancellationToken::default();
    let mut tree = decode_message_tree(&import, &Limits::default(), &cancel).unwrap();
    let k = Kiwi(import.schema_summary.as_ref().expect("fixture schema"));
    let frame = node_fields(&mut tree, "horizontal-stack");
    set(frame, k.field("NodeChange", "stackMode", k.member("NodeChange", "stackMode", "GRID")));
    set(frame, k.field("NodeChange", "gridRows", k.tracks(&[(ROW_B, "\""), (ROW_A, "!")])));
    set(frame, k.field("NodeChange", "gridColumns", k.tracks(&[(COL_A, "!"), (COL_B, "\""), (COL_C, "#")])));
    set(frame, k.field("NodeChange", "gridRowsSizing", k.sizes(&[(ROW_A, Some(("FIXED", Some(40.0))), Some(("FIXED", Some(40.0)))), (ROW_B, None, Some(("HUG", None)))])));
    set(
        frame,
        k.field("NodeChange", "gridColumnsSizing", k.sizes(&[(COL_A, None, Some(("FLEX", Some(1.0)))), (COL_B, Some(("FIXED", Some(100.0))), Some(("FIXED", Some(100.0)))), (COL_C, None, Some(("HUG", None)))])),
    );
    set(frame, k.field("NodeChange", "gridRowGap", KiwiValue::Float(8.0)));
    set(frame, k.field("NodeChange", "gridColumnGap", KiwiValue::Float(12.0)));
    set(frame, k.field("NodeChange", "gridAutoTracks", k.member("NodeChange", "gridAutoTracks", "ROWS")));
    set(frame, k.field("NodeChange", "gridReflowEnabled", KiwiValue::Bool(true)));
    let one = node_fields(&mut tree, "button-instance-1");
    set(one, k.field("NodeChange", "gridRowAnchor", k.guid(ROW_A)));
    set(one, k.field("NodeChange", "gridColumnAnchor", k.guid(COL_A)));
    set(one, k.field("NodeChange", "gridRowSpan", KiwiValue::Uint(1)));
    set(one, k.field("NodeChange", "gridColumnSpan", KiwiValue::Uint(1)));
    set(one, k.field("NodeChange", "gridChildVerticalAlign", k.member("NodeChange", "gridChildVerticalAlign", "CENTER")));
    let two = node_fields(&mut tree, "button-instance-2");
    set(two, k.field("NodeChange", "gridRowAnchor", k.guid(ROW_B)));
    set(two, k.field("NodeChange", "gridColumnAnchor", k.guid(COL_B)));
    set(two, k.field("NodeChange", "gridColumnSpan", KiwiValue::Uint(2)));
    patch(&k, &mut tree);
    document_from_tree(&tree, Some(&import.container), &Limits::default(), &cancel).unwrap()
}

fn grid_of<'a>(doc: &'a FigDocument, name: &'a str) -> &'a FigGrid {
    let n = doc.named(name).next().unwrap_or_else(|| panic!("node {name}"));
    let NodePayload::Frame(f) = &n.payload else { panic!("{name} is a frame") };
    let a = f.auto_layout.as_ref().expect("auto layout");
    assert_eq!(a.mode, "GRID");
    a.grid.as_ref().expect("grid fields")
}

fn id(local: u32) -> SourceId {
    SourceId { session: 4, local }
}

fn sizing(kind: &str, value: Option<f32>) -> Option<FigTrackSizing> {
    Some(FigTrackSizing { kind: kind.into(), value })
}

#[test]
fn grid_container_and_child_fields_decode_under_the_fixture_schema_names() {
    let doc = grid_document(|_, _| {});
    let g = grid_of(&doc, "horizontal-stack");
    // Tracks in stored (position) order; the counts are the list lengths.
    assert_eq!(g.rows.iter().map(|t| (t.id, t.position.as_str())).collect::<Vec<_>>(), [(id(ROW_A), "!"), (id(ROW_B), "\"")]);
    assert_eq!(g.columns.iter().map(|t| t.id).collect::<Vec<_>>(), [id(COL_A), id(COL_B), id(COL_C)]);
    assert_eq!(g.rows[0].size, Some(FigTrackSize { min: sizing("FIXED", Some(40.0)), max: sizing("FIXED", Some(40.0)) }));
    assert_eq!(g.rows[1].size, Some(FigTrackSize { min: None, max: sizing("HUG", None) }));
    assert_eq!(g.columns[0].size, Some(FigTrackSize { min: None, max: sizing("FLEX", Some(1.0)) }));
    assert_eq!(g.columns[1].size, Some(FigTrackSize { min: sizing("FIXED", Some(100.0)), max: sizing("FIXED", Some(100.0)) }));
    assert_eq!(g.columns[2].size, Some(FigTrackSize { min: None, max: sizing("HUG", None) }));
    assert_eq!((g.row_gap, g.column_gap), (Some(8.0), Some(12.0)));
    assert_eq!(g.auto_tracks.as_deref(), Some("ROWS"));
    assert_eq!(g.reflow_enabled, Some(true));
    // Child placements, in child order; the unanchored child stores none.
    assert_eq!(
        g.children,
        [
            (id(16), FigGridChild { row_anchor: Some(id(ROW_A)), column_anchor: Some(id(COL_A)), row_span: Some(1), column_span: Some(1), vertical_align: Some("CENTER".into()), horizontal_align: None }),
            (id(18), FigGridChild { row_anchor: Some(id(ROW_B)), column_anchor: Some(id(COL_B)), row_span: None, column_span: Some(2), vertical_align: None, horizontal_align: None }),
        ]
    );
    // Every grid field was read: none is a field_not_decoded loss, and the decoded census names them.
    assert!(doc.loss.iter().all(|l| !l.path.contains(".grid")), "{:?}", doc.loss);
    for path in [
        "NodeChange.gridRows.entries.position",
        "NodeChange.gridColumnsSizing.entries.trackSize.maxSizing.type",
        "NodeChange.gridRowsSizing.entries.trackSize.minSizing.value",
        "NodeChange.gridRowAnchor.localID",
        "NodeChange.gridColumnSpan",
        "NodeChange.gridChildVerticalAlign",
        "NodeChange.gridAutoTracks",
        "NodeChange.gridReflowEnabled",
    ] {
        assert!(doc.decoded.contains_key(path), "{path} not decoded: {:?}", doc.decoded.keys().filter(|k| k.contains("grid")).collect::<Vec<_>>());
    }
}

#[test]
fn malformed_grid_fields_are_typed_losses() {
    let doc = grid_document(|k, tree| {
        let frame = node_fields(tree, "horizontal-stack");
        // A duplicate track id and a size entry for a track the frame does not have.
        set(frame, k.field("NodeChange", "gridRows", k.tracks(&[(ROW_B, "\""), (ROW_A, "!"), (ROW_A, "#")])));
        set(frame, k.field("NodeChange", "gridRowsSizing", k.sizes(&[(ROW_A, None, Some(("FIXED", Some(40.0)))), (999, None, Some(("HUG", None)))])));
        // A child with grid fields whose parent is not a grid container.
        let other = node_fields(tree, "state=default");
        set(other, k.field("NodeChange", "gridRowSpan", KiwiValue::Uint(2)));
        let text = node_fields(tree, "fixture-button-label");
        set(text, k.field("NodeChange", "gridColumnSpan", KiwiValue::Uint(3)));
    });
    let g = grid_of(&doc, "horizontal-stack");
    assert_eq!(g.rows.iter().map(|t| t.id).collect::<Vec<_>>(), [id(ROW_A), id(ROW_B)], "the duplicate is dropped");
    assert!(g.rows[1].size.is_none(), "track 4:100 has no size entry");
    assert_eq!(doc.loss_count("NodeChange.gridRows", "grid_track_duplicate_id"), 1);
    assert_eq!(doc.loss_count("NodeChange.gridRowsSizing", "grid_track_size_orphan"), 1);
    // state=default is a SYMBOL under the fixture-button FRAME (HORIZONTAL); fixture-button-label
    // sits on the internal canvas.
    assert_eq!(doc.loss_count("NodeChange.grid", "grid_child_without_grid_parent"), 2);
}

#[test]
fn fixtures_without_grid_fields_decode_without_grid() {
    let cancel = CancellationToken::default();
    for name in ["fig-basic-shapes.fig", "fig-components-autolayout.fig", "fig-prototype-pages.fig"] {
        let doc = decode_document(&fixture(name), &Limits::default(), &cancel).unwrap();
        for n in &doc.nodes {
            let frame = match &n.payload {
                NodePayload::Frame(f) | NodePayload::Symbol { frame: f, .. } | NodePayload::Instance { frame: f, .. } => f,
                _ => continue,
            };
            assert!(frame.auto_layout.as_ref().is_none_or(|a| a.grid.is_none()), "{name}: {:?}", n.name);
        }
        assert!(doc.loss.iter().all(|l| !l.path.contains("grid")), "{name}: {:?}", doc.loss);
    }
}

/// F-R45-1 (lane SMB-R46-FOLLOWUPS-1), hostile counts: a track list or sizing list longer than
/// Folio's 1000-track bound is refused whole as a typed loss before any per-entry work (200 001
/// distinct ids cost about 2 x 10^10 id comparisons in the old linear duplicate scan); exactly 1000
/// entries still decode, with each size found through the id index.
#[test]
fn grid_track_lists_over_the_count_bound_are_refused_before_entry_work() {
    const BOUND: u32 = 1000;
    let many = |n: u32| (0..n).map(|i| (1_000 + i, "!")).collect::<Vec<_>>();
    let doc = grid_document(|k, tree| {
        let frame = node_fields(tree, "horizontal-stack");
        set(frame, k.field("NodeChange", "gridRows", k.tracks(&many(200_001))));
        let sizes: Vec<(u32, Sizing, Sizing)> = (0..BOUND + 1).map(|i| (COL_A + i % 3, None, Some(("HUG", None)))).collect();
        set(frame, k.field("NodeChange", "gridColumnsSizing", k.sizes(&sizes)));
    });
    let g = grid_of(&doc, "horizontal-stack");
    assert!(g.rows.is_empty(), "an over-bound track list is left out whole");
    assert_eq!(doc.loss_count("NodeChange.gridRows", "grid_track_count_over_limit"), 1);
    assert_eq!(g.columns.len(), 3, "the columns themselves are within the bound");
    assert!(g.columns.iter().all(|c| c.size.is_none()), "an over-bound sizing list is left out whole");
    assert_eq!(doc.loss_count("NodeChange.gridColumnsSizing", "grid_track_size_count_over_limit"), 1);
    assert_eq!(doc.loss_count("NodeChange.gridColumnsSizing", "grid_track_size_duplicate"), 0, "no entry was visited");

    let doc = grid_document(|k, tree| {
        let frame = node_fields(tree, "horizontal-stack");
        set(frame, k.field("NodeChange", "gridRows", k.tracks(&many(BOUND))));
        let sizes: Vec<(u32, Sizing, Sizing)> = (0..BOUND).map(|i| (1_000 + i, None, Some(("FIXED", Some(i as f32))))).collect();
        set(frame, k.field("NodeChange", "gridRowsSizing", k.sizes(&sizes)));
    });
    let g = grid_of(&doc, "horizontal-stack");
    assert_eq!(g.rows.len(), BOUND as usize, "exactly the bound decodes");
    assert!(g.rows.iter().all(|t| t.size.as_ref().and_then(|s| s.max.as_ref()).and_then(|m| m.value) == Some((t.id.local - 1_000) as f32)));
    assert_eq!(doc.loss_count("NodeChange.gridRows", "grid_track_count_over_limit"), 0);
    assert_eq!(doc.loss_count("NodeChange.gridRowsSizing", "grid_track_size_count_over_limit"), 0);
}
