//! SMB-INTEROP-PSD-ADJ-2: mapped adjustment layers are composition-graph sources (Folio ENG-6,
//! STU-CMP-092: the `input_ordinal` is the stack position) and a layer clipped to an adjustment layer
//! is imported unclipped with a typed loss, because Folio refuses that arrangement
//! (`Code::AdjustmentClipBase`). All inputs are small synthetic byte fixtures built here, independently
//! of the crate's encoder. Photoshop's own behaviour for a layer clipped to an adjustment layer is
//! UNVERIFIED (D5 oracle); these tests check the importer's mapping and Folio's validation, not
//! Photoshop's rendering.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, Cardinality, Code, Diagnostic, OperationKind, Port, PortAddress, Resolution, Resolver,
    Semantic, StudioDocument, TileRef, ValueType, validate_document, validate_raster_records,
};
use hsk_studio_interop_psd::*;

struct Mint(u64);
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.0)
    }
}
impl IdMint for Mint {
    fn document_id(&mut self) -> String {
        self.next("SDOC")
    }
    fn artboard_id(&mut self) -> String {
        self.next("SART")
    }
    fn layer_id(&mut self) -> String {
        self.next("SLYR")
    }
    fn linear_working_profile_id(&mut self) -> String {
        self.next("SCPF")
    }
    fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
        format!("manifest/{layer_id}/{object_key}")
    }
}
/// A host resolver that binds the adjustment primitive.
struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == ADJUSTMENT_SCHEMA
    }
}

fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal(bytes: &[u8]) -> (PsdDocument, ImportProposal) {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default())
        .expect("proposal");
    (doc, p)
}

/// A 1x1 RGB 8-bit document holding `layers` (file order: bottom to top).
fn doc_with(layers: &[TLayer]) -> Vec<u8> {
    let info = layer_info(layers, false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 1,
        width: 1,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, vec![1, 2, 3])),
    }
    .bytes()
}
fn pixel(name: &'static str, clipping: u8) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 1, 1]);
    l.channels = vec![(0, 0, vec![10]), (1, 0, vec![20]), (2, 0, vec![30])];
    l.clipping = clipping;
    l
}
/// An `nvrt` (invert) adjustment layer: no data, no pixels.
fn invert(name: &'static str, clipping: u8) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"nvrt", &[], false)];
    l.clipping = clipping;
    l
}
fn divider() -> TLayer {
    let mut l = TLayer::new("</Layer group>", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"lsct", &be32(3), false)];
    l
}
fn folder(name: &'static str) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.key = *b"pass";
    l.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"pass");
    l.blocks = vec![layer_block(b"lsct", &lsct, false)];
    l
}

fn id_of(p: &ImportProposal, name: &str) -> String {
    p.snapshot.document().layers.iter().find(|l| l.name == name).expect(name).layer_id.clone()
}
fn layer_of<'a>(p: &'a ImportProposal, name: &str) -> &'a hsk_studio_folio::StudioLayer {
    p.snapshot.document().layers.iter().find(|l| l.name == name).expect(name)
}
fn clipped(p: &ImportProposal, name: &str) -> bool {
    layer_of(p, name).appearance.as_ref().expect("importer writes appearance").clipped
}
/// Layer ids of the stack of group `group`, bottom first, by `input_ordinal`.
fn stack(p: &ImportProposal, group: &str) -> Vec<String> {
    let address = PortAddress { layer_id: id_of(p, group), operation_key: "mix".into(), port_key: "inputs".into() };
    p.snapshot.ordered_inputs(&address).into_iter().map(|a| a.layer_id.clone()).collect()
}
fn ids(p: &ImportProposal, names: &[&str]) -> Vec<String> {
    names.iter().map(|n| id_of(p, n)).collect()
}
/// Independent validation of the proposal's document (the proposal was already built through Folio's
/// own validator; this repeats it on a copy so the claim does not rest on the importer's call).
fn revalidate(document: StudioDocument) -> std::result::Result<(), Diagnostic> {
    validate_document(document, budget(), &CancellationToken::default(), &Host).map(|_| ())
}
fn losses<'a>(p: &'a ImportProposal, code: &str) -> Vec<&'a LossEntry> {
    p.loss.entries.iter().filter(|e| e.code == code).collect()
}

/// An adjustment layer between two raster layers is an image `source` operation with an edge into
/// the group, and its `input_ordinal` is its Photoshop stack position (bottom-most first). The same
/// holds for root layers, which are listed bottom-first in `graph.outputs`. The record set validates.
#[test]
fn adjustment_layer_between_raster_layers_keeps_its_stack_position() {
    bounded_case("adjustment_layer_between_raster_layers_keeps_its_stack_position", || {
        // file order is bottom to top: divider, bottom, adj, top, folder
        let bytes = doc_with(&[divider(), pixel("bottom", 0), invert("adj", 0), pixel("top", 0), folder("stack")]);
        let (_, p) = proposal(&bytes);
        let document = p.snapshot.document();
        let adj = id_of(&p, "adj");
        assert_eq!(layer_of(&p, "adj").kind, "adjustment");
        assert_eq!(stack(&p, "stack"), ids(&p, &["bottom", "adj", "top"]), "ordinal order is the PSD order");
        let ordinals: Vec<u64> = document.graph.edges.iter().filter(|e| e.source.layer_id == adj).map(|e| e.input_ordinal).collect();
        assert_eq!(ordinals, [1], "bottom 0, adjustment 1, top 2");
        let op = document.graph.operations.iter().find(|o| o.layer_id == adj).expect("adjustment source operation");
        assert_eq!(op.operation_kind, OperationKind::Source);
        assert_eq!(
            op.outputs,
            [Port { port_key: "out".into(), value_type: ValueType::Image, cardinality: Cardinality::One, semantic: Semantic::Source }]
        );
        revalidate(document.clone()).expect("Folio validates the document with the adjustment source");
        assert!(losses(&p, "clip_to_adjustment_not_supported").is_empty(), "nothing is clipped");
        let issues =
            validate_raster_records(&p.raster_records, document, budget(), &CancellationToken::default(), &Host).expect("records");
        assert!(issues.is_empty());
        assert!(p.raster_records.layer(&adj).is_some_and(|r| r.adjustment.is_some()), "the layer keeps its record");

        // root layers: no group, so the order is the order of graph.outputs (bottom-most first)
        let (_, root) = proposal(&doc_with(&[pixel("bottom", 0), invert("adj", 0), pixel("top", 0)]));
        let order: Vec<String> = root.snapshot.document().graph.outputs.iter().map(|a| a.layer_id.clone()).collect();
        assert_eq!(order, ids(&root, &["bottom", "adj", "top"]));
        assert!(root.snapshot.document().graph.operations.iter().any(|o| o.layer_id == id_of(&root, "adj")));
        revalidate(root.snapshot.document().clone()).expect("root-level adjustment validates");
    });
}

/// Layers clipped to an adjustment layer would fail Folio validation with `AdjustmentClipBase`. They
/// are imported unclipped (the whole run sharing that base), each with a typed loss naming the layer
/// and its base; a layer clipped to a raster base stays clipped; stack order is unchanged; the PSD
/// bytes (clipping flags included) round-trip through an unedited export.
#[test]
fn clipped_to_adjustment_layer_is_unclipped_with_a_typed_loss_and_exports_unchanged() {
    bounded_case("clipped_to_adjustment_layer_is_unclipped_with_a_typed_loss_and_exports_unchanged", || {
        // file order: divider 0, base 1, adj 2, c1 3 (clipped), c2 4 (clipped), top 5, keep 6 (clipped), folder 7
        let bytes = doc_with(&[
            divider(),
            pixel("base", 0),
            invert("adj", 0),
            pixel("c1", 1),
            pixel("c2", 1),
            pixel("top", 0),
            pixel("keep", 1),
            folder("stack"),
        ]);
        let (doc, p) = proposal(&bytes);
        assert_eq!(stack(&p, "stack"), ids(&p, &["base", "adj", "c1", "c2", "top", "keep"]), "stack order is unchanged");
        let flags: Vec<bool> = ["base", "adj", "c1", "c2", "top", "keep"].iter().map(|n| clipped(&p, n)).collect();
        assert_eq!(flags, [false, false, false, false, false, true], "c1 and c2 are unclipped, keep (raster base) stays clipped");
        revalidate(p.snapshot.document().clone()).expect("the imported document validates");

        let (adj, c1, c2) = (id_of(&p, "adj"), id_of(&p, "c1"), id_of(&p, "c2"));
        let found = losses(&p, "clip_to_adjustment_not_supported");
        assert_eq!(found.len(), 2, "one entry per demoted layer, none for `keep`: {:?}", p.loss.to_json());
        assert!(found.iter().all(|e| e.kind == LossKind::Unsupported));
        assert_eq!(found[0].path, format!("layer[3]/clipping(layer_id={c1},base_layer_id={adj})"));
        assert_eq!(found[1].path, format!("layer[4]/clipping(layer_id={c2},base_layer_id={adj})"));

        // counterfactual: the same document with c1 clipped is refused by Folio, so the demotion is
        // what makes the import valid
        let mut value = serde_json::to_value(p.snapshot.document()).expect("document json");
        let layers = value["layers"].as_array_mut().expect("layers");
        let c1_json = layers.iter_mut().find(|l| l["layer_id"] == c1.as_str()).expect("c1");
        c1_json["appearance"]["clipped"] = serde_json::Value::Bool(true);
        let reclipped: StudioDocument = serde_json::from_value(value).expect("document shape");
        assert_eq!(revalidate(reclipped), Err(Diagnostic { code: Code::AdjustmentClipBase, target: c1.clone() }));

        // the file keeps its clipping bytes: an unedited export is byte-identical and applies nothing
        let limits = Limits::default();
        let (out, report) =
            export_psd(&doc, &p, &p.snapshot, &ExportOptions { limits: &limits, tiles: None }).expect("export");
        assert_eq!(out, bytes, "unedited export leaves the clipping bytes alone");
        assert!(report.applied.is_empty() && report.loss.entries.is_empty(), "{:?}", report.applied);
        assert_eq!(read(&out).layers()[3].clipping, 1);
    });
}

/// Root-level stacks follow the same rule, and a clipped adjustment layer is only a clipped layer:
/// clipped to a raster base it is accepted and is not a base for the layers clipped above it.
#[test]
fn root_stack_applies_the_clip_base_rule_and_a_clipped_adjustment_layer_is_no_base() {
    bounded_case("root_stack_applies_the_clip_base_rule_and_a_clipped_adjustment_layer_is_no_base", || {
        // file order: bottom 0, adj 1 (clipped), x 2 (clipped), adj2 3, y 4 (clipped)
        let bytes = doc_with(&[pixel("bottom", 0), invert("adj", 1), pixel("x", 1), invert("adj2", 0), pixel("y", 1)]);
        let (_, p) = proposal(&bytes);
        let order: Vec<String> = p.snapshot.document().graph.outputs.iter().map(|a| a.layer_id.clone()).collect();
        assert_eq!(order, ids(&p, &["bottom", "adj", "x", "adj2", "y"]));
        let flags: Vec<bool> = ["bottom", "adj", "x", "adj2", "y"].iter().map(|n| clipped(&p, n)).collect();
        assert_eq!(flags, [false, true, true, false, false], "adj and x share the raster base; only y has an adjustment base");
        let found = losses(&p, "clip_to_adjustment_not_supported");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, format!("layer[4]/clipping(layer_id={},base_layer_id={})", id_of(&p, "y"), id_of(&p, "adj2")));
        revalidate(p.snapshot.document().clone()).expect("valid");

        // a group clipped to an adjustment layer is a clipped layer too
        let bytes = doc_with(&[invert("adj", 0), divider(), pixel("inner", 0), {
            let mut g = folder("grp");
            g.clipping = 1;
            g
        }]);
        let (_, p) = proposal(&bytes);
        assert!(!clipped(&p, "grp"));
        assert_eq!(losses(&p, "clip_to_adjustment_not_supported").len(), 1);
        revalidate(p.snapshot.document().clone()).expect("valid");
    });
}
