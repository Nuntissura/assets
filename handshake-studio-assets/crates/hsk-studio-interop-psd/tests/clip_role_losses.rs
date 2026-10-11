//! SMB-INTEROP-PSD-CLIP-ROLES: pigment 0d28c03 refuses a non-identity Blend-If on a clip base
//! (`blend_if_clip_base`) and on a clipped layer (`blend_if_clipped_layer`) for every backend, because no
//! public source defines them, and its clip rule is the "Blend Clipped Layers as Group" ON reading. A PSD
//! that carries either must say so at import time: the ranges stay in the Folio record, and a typed loss
//! entry names the key and the layer id. Photoshop's own behaviour is UNVERIFIED (D5 cases 18-23); these
//! tests check the importer's reporting, not a rendering. All inputs are small synthetic byte fixtures.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ADJUSTMENT_SCHEMA, Budget, Resolution, Resolver, TileRef};
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

fn proposal(bytes: &[u8]) -> ImportProposal {
    let limits = Limits::default();
    let doc = read(bytes);
    let budget = Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() };
    let cfg = ProposalConfig::new(&limits, budget, &Host);
    to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default()).expect("proposal")
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
/// Composite source narrowed to 10..20 / 200..210, destination full: a non-identity Blend-If.
fn with_blend_if(mut layer: TLayer) -> TLayer {
    layer.ranges = vec![10, 20, 200, 210, 0, 0, 255, 255];
    layer
}
/// Source and destination full everywhere: the identity, which is not a Blend-If.
fn with_identity_ranges(mut layer: TLayer) -> TLayer {
    layer.ranges = vec![0, 0, 255, 255, 0, 0, 255, 255];
    layer
}
/// `clbl` (Blend Clipped Layers as Group): 1 = on (Photoshop's default), 0 = off.
fn with_clbl(mut layer: TLayer, on: u8) -> TLayer {
    layer.blocks.push(layer_block(b"clbl", &[on], false));
    layer
}
/// An `nvrt` (invert) adjustment layer.
fn invert(name: &'static str) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"nvrt", &[], false)];
    l
}
fn divider() -> TLayer {
    let mut l = TLayer::new("</Layer group>", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"lsct", &be32(3), false)];
    l
}
/// An open folder with the isolated `norm` blend key (a pass-through group cannot be clipped).
fn folder(name: &'static str, clipping: u8) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.key = *b"norm";
    l.clipping = clipping;
    l.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"norm");
    l.blocks = vec![layer_block(b"lsct", &lsct, false)];
    l
}

fn id_of(p: &ImportProposal, name: &str) -> String {
    p.snapshot.document().layers.iter().find(|l| l.name == name).expect(name).layer_id.clone()
}
fn losses<'a>(p: &'a ImportProposal, code: &str) -> Vec<&'a LossEntry> {
    p.loss.entries.iter().filter(|e| e.code == code).collect()
}
fn ranges_in_record(p: &ImportProposal, name: &str) -> usize {
    p.raster_records
        .layer(&id_of(p, name))
        .and_then(|r| r.advanced_blending.as_ref())
        .map_or(0, |a| a.blend_ranges.len())
}

/// Blend-If on a clip base or on a clipped layer: the ranges stay in the record (the layer is not demoted or
/// stripped), and each affected layer gets an `Unsupported` entry whose code is pigment's refusal key and whose
/// path names the layer id (and the base id of a clipped layer). Counterfactuals: Blend-If on an unclipped
/// layer, identity ranges on a clip role, a clipped layer without ranges and a layer demoted because its base is
/// an adjustment layer (it is no longer clipped in Folio) get no entry.
#[test]
fn blend_if_on_a_clip_base_or_a_clipped_layer_keeps_the_ranges_and_reports_the_refusal() {
    bounded_case("blend_if_on_a_clip_base_or_a_clipped_layer_keeps_the_ranges_and_reports_the_refusal", || {
        // file order is bottom to top; inside the folder "stack": base (ranges), c1 (clipped, ranges),
        // c2 (clipped, none), c3 (clipped, identity ranges), top (unclipped, ranges: the counterfactual)
        let bytes = doc_with(&[
            divider(),
            with_blend_if(pixel("base", 0)),
            with_blend_if(pixel("c1", 1)),
            pixel("c2", 1),
            with_identity_ranges(pixel("c3", 1)),
            with_blend_if(pixel("top", 0)),
            folder("stack", 0),
        ]);
        let p = proposal(&bytes);
        let (base, c1) = (id_of(&p, "base"), id_of(&p, "c1"));
        for name in ["base", "c1", "top"] {
            assert_eq!(ranges_in_record(&p, name), 1, "{name}: the Blend-If range stays in the Folio record");
        }
        assert_eq!(ranges_in_record(&p, "c2") + ranges_in_record(&p, "c3"), 0);

        let on_base = losses(&p, "blend_if_clip_base");
        assert_eq!(on_base.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!((on_base[0].kind, on_base[0].path.as_str()), (LossKind::Unsupported, format!("layer[1]/blend_ranges(layer_id={base})").as_str()));
        let on_clipped = losses(&p, "blend_if_clipped_layer");
        assert_eq!(on_clipped.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!(on_clipped[0].kind, LossKind::Unsupported);
        assert_eq!(on_clipped[0].path, format!("layer[2]/blend_ranges(layer_id={c1},base_layer_id={base})"));
        assert_eq!(p.loss.entries.iter().filter(|e| e.path.contains("/blend_ranges(")).count(), 2, "nothing for c2, c3, top");
        assert!(p.loss.to_json().contains("blend_if_clip_base"), "the key survives the JSON projection");

        // a clipped GROUP with ranges is a clipped layer too (root stack: bottom, grp)
        let p = proposal(&doc_with(&[pixel("bottom", 0), divider(), pixel("inner", 0), with_blend_if(folder("grp", 1))]));
        assert_eq!(ranges_in_record(&p, "grp"), 1);
        let found = losses(&p, "blend_if_clipped_layer");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, format!("layer[3]/blend_ranges(layer_id={},base_layer_id={})", id_of(&p, "grp"), id_of(&p, "bottom")));
        assert!(losses(&p, "blend_if_clip_base").is_empty(), "bottom has no ranges");

        // the role is the clip flag written to Folio: a layer clipped to an adjustment layer is demoted
        // (clip_to_adjustment_not_supported), so neither it nor the adjustment layer has a clip role
        let p = proposal(&doc_with(&[pixel("under", 0), invert("adj"), with_blend_if(pixel("demoted", 1))]));
        assert_eq!(losses(&p, "clip_to_adjustment_not_supported").len(), 1);
        assert!(losses(&p, "blend_if_clipped_layer").is_empty() && losses(&p, "blend_if_clip_base").is_empty(), "{:?}", p.loss.to_json());
        assert_eq!(ranges_in_record(&p, "demoted"), 1);

        // a clipped layer over an adjustment-free base keeps both entries when the base also has ranges
        let p = proposal(&doc_with(&[with_blend_if(pixel("b", 0)), with_blend_if(pixel("x", 1))]));
        assert_eq!((losses(&p, "blend_if_clip_base").len(), losses(&p, "blend_if_clipped_layer").len()), (1, 1));
    });
}

/// "Blend Clipped Layers as Group" switched off is not modelled by the render-plan IR (its clip rule is the
/// switched-on reading), so it is a typed `Unsupported` entry on the clip base that carries it; the record keeps
/// the toggle. The same setting where it changes nothing (no clipped layer above, or on the clipped layer
/// itself) and the default (on) produce no entry.
#[test]
fn blend_clipped_layers_as_group_off_is_a_typed_loss_on_a_clip_base() {
    bounded_case("blend_clipped_layers_as_group_off_is_a_typed_loss_on_a_clip_base", || {
        let code = "blend_clipped_layers_as_group_off_not_modelled";

        let p = proposal(&doc_with(&[with_clbl(pixel("base", 0), 0), pixel("c1", 1)]));
        let found = losses(&p, code);
        assert_eq!(found.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!((found[0].kind, found[0].path.as_str()), (LossKind::Unsupported, format!("layer[0]/tagged/clbl(layer_id={})", id_of(&p, "base")).as_str()));
        let record = p.raster_records.layer(&id_of(&p, "base")).and_then(|r| r.advanced_blending.as_ref()).expect("record");
        assert!(!record.blend_clipped_effects, "the toggle stays in the Folio record");

        // a folder is a clip base like any layer
        let p = proposal(&doc_with(&[divider(), pixel("inner", 0), with_clbl(folder("grp", 0), 0), pixel("c1", 1)]));
        let found = losses(&p, code);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, format!("layer[2]/tagged/clbl(layer_id={})", id_of(&p, "grp")));

        // on (the default), absent, no clipped layer above, and set on the clipped layer: nothing to report
        for (what, layers) in [
            ("clbl on", vec![with_clbl(pixel("base", 0), 1), pixel("c1", 1)]),
            ("clbl absent", vec![pixel("base", 0), pixel("c1", 1)]),
            ("off, nothing clipped above", vec![with_clbl(pixel("base", 0), 0), pixel("top", 0)]),
            ("off on the clipped layer", vec![pixel("base", 0), with_clbl(pixel("c1", 1), 0)]),
        ] {
            let p = proposal(&doc_with(&layers));
            assert!(losses(&p, code).is_empty(), "{what}: {:?}", p.loss.to_json());
        }

        // both gaps on one base are two independent entries
        let p = proposal(&doc_with(&[with_clbl(with_blend_if(pixel("base", 0)), 0), pixel("c1", 1)]));
        assert_eq!((losses(&p, code).len(), losses(&p, "blend_if_clip_base").len()), (1, 1));
    });
}
