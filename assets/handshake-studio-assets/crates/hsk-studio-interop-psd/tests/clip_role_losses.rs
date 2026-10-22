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
    fn effect_stack_id(&mut self, _layer_id: &str) -> Option<String> {
        Some(self.next("SEFX"))
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

// ---- SMB-FX-OPS-ADOPT: effects with Blend-If on one layer, and a mask on a Pass Through group --------------

// Photoshop descriptor builders, independent of the crate's reader (the same shapes as adjustment_records.rs).
fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 { be32(0) } else { be32(k.len() as u32) };
    v.extend_from_slice(k.as_bytes());
    v
}
fn tagged(tag: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend(payload);
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0); // empty Unicode class name
    v.extend(ds_key(class));
    v.extend(be32(items.len() as u32));
    for (key, value) in items {
        v.extend(ds_key(key));
        v.extend(value);
    }
    v
}
fn v_objc(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    tagged(b"Objc", ds_object(class, items))
}
fn v_untf(unit: &[u8; 4], value: f64) -> Vec<u8> {
    let mut v = unit.to_vec();
    v.extend(value.to_be_bytes());
    tagged(b"UntF", v)
}
fn v_doub(value: f64) -> Vec<u8> {
    tagged(b"doub", value.to_be_bytes().to_vec())
}
fn v_bool(value: bool) -> Vec<u8> {
    tagged(b"bool", vec![u8::from(value)])
}
fn v_enum(type_id: &str, value: &str) -> Vec<u8> {
    let mut v = ds_key(type_id);
    v.extend(ds_key(value));
    tagged(b"enum", v)
}
/// An `lfx2` block with one enabled drop shadow; `master` is the stack switch (`masterFXSwitch`).
fn lfx2_shadow(master: bool) -> Vec<u8> {
    let shadow = v_objc(
        "DrSh",
        vec![
            ("enab", v_bool(true)),
            ("Md  ", v_enum("BlnM", "Mltp")),
            ("Opct", v_untf(b"#Prc", 75.0)),
            ("Clr ", v_objc("RGBC", vec![("Rd  ", v_doub(255.0)), ("Grn ", v_doub(0.0)), ("Bl  ", v_doub(51.0))])),
            ("uglg", v_bool(true)),
            ("lagl", v_untf(b"#Ang", 120.0)),
            ("Dstn", v_untf(b"#Pxl", 2.0)),
            ("Ckmt", v_untf(b"#Prc", 10.0)),
            ("blur", v_untf(b"#Pxl", 4.0)),
            ("Nose", v_untf(b"#Prc", 0.0)),
            ("AntA", v_bool(false)),
        ],
    );
    let mut v = be32(0); // object-effects version
    v.extend(be32(16)); // descriptor version
    v.extend(ds_object("null", vec![("masterFXSwitch", v_bool(master)), ("DrSh", shadow)]));
    v
}
fn with_effects(mut layer: TLayer, master: bool) -> TLayer {
    layer.blocks.push(layer_block(b"lfx2", &lfx2_shadow(master), false));
    layer
}
/// A layer mask over the 1x1 document: rectangle, default colour 255, `flags` (bit 1 = disabled), padding.
fn with_user_mask(mut layer: TLayer, flags: u8) -> TLayer {
    let mut mask: Vec<u8> = [0i32, 0, 1, 1].iter().flat_map(|v| v.to_be_bytes()).collect();
    mask.extend([255, flags, 0, 0]);
    layer.mask = mask;
    layer.channels.push((-2, 0, vec![255]));
    layer
}
/// An open folder with the `pass` blend key: a Pass Through group.
fn pass_through_folder(name: &'static str) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.key = *b"pass";
    l.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"pass");
    l.blocks = vec![layer_block(b"lsct", &lsct, false)];
    l
}
fn effect_stack_enabled(p: &ImportProposal, name: &str) -> Option<bool> {
    p.raster_records.layer(&id_of(p, name)).and_then(|r| r.effects.as_ref()).map(|s| s.enabled)
}

/// Effects together with non-identity Blend-If ranges on one layer: pigment refuses the combination for every
/// backend as `effects_with_blend_if` (no public source defines how the weight composes with the effect mattes),
/// so the importer says so. Both the ranges and the effect stack stay in the Folio records; the entry is
/// `Unsupported`, its code is pigment's key and its path names the layer id. Counterfactuals: effects alone,
/// ranges alone, effects with identity ranges, a switched-off stack, and a clip base (which keeps only its
/// clip-role entry) get no `effects_with_blend_if` entry.
#[test]
fn effects_with_blend_if_on_one_layer_is_a_typed_loss_and_both_stay_in_the_record() {
    bounded_case("effects_with_blend_if_on_one_layer_is_a_typed_loss_and_both_stay_in_the_record", || {
        let code = hsk_studio_pigment::render_plan::ExtOperator::EffectsWithBlendIf.key();
        assert_eq!(code, "effects_with_blend_if", "the pigment key, not a literal of this crate");

        // file order is bottom to top inside the folder "stack"
        let bytes = doc_with(&[
            divider(),
            with_effects(with_blend_if(pixel("both", 0)), true),
            with_effects(pixel("fx_only", 0), true),
            with_blend_if(pixel("ranges_only", 0)),
            with_effects(with_identity_ranges(pixel("identity", 0)), true),
            with_effects(with_blend_if(pixel("off", 0)), false),
            folder("stack", 0),
        ]);
        let p = proposal(&bytes);
        let both = id_of(&p, "both");
        let found = losses(&p, code);
        assert_eq!(found.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!((found[0].kind, found[0].path.as_str()), (LossKind::Unsupported, format!("layer[1]/blend_ranges(layer_id={both})").as_str()));
        assert!(p.loss.to_json().contains(code), "the key survives the JSON projection");
        // nothing is dropped: the ranges and the stack are in the record, the switched-off stack too
        assert_eq!(ranges_in_record(&p, "both"), 1);
        assert_eq!(effect_stack_enabled(&p, "both"), Some(true));
        assert_eq!(effect_stack_enabled(&p, "off"), Some(false));
        assert_eq!(ranges_in_record(&p, "off"), 1);
        assert_eq!(effect_stack_enabled(&p, "ranges_only"), None);

        // a clip base with effects and ranges has its clip-role entry only; the combination is not doubled
        let p = proposal(&doc_with(&[with_effects(with_blend_if(pixel("base", 0)), true), pixel("c1", 1)]));
        assert!(losses(&p, code).is_empty(), "{:?}", p.loss.to_json());
        assert_eq!(losses(&p, "blend_if_clip_base").len(), 1);

        // a group is a layer too
        let p = proposal(&doc_with(&[divider(), pixel("inner", 0), with_effects(with_blend_if(folder("grp", 0)), true)]));
        let found = losses(&p, code);
        assert_eq!(found.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!(found[0].path, format!("layer[2]/blend_ranges(layer_id={})", id_of(&p, "grp")));
    });
}

/// A layer mask on a Pass Through group: pigment defines no mask on a pass-through layer and refuses it for every
/// backend (`mask_on_pass_through`), so the importer reports it per group, as a typed `Unsupported` entry under
/// pigment's constant. The mask itself stays in the Folio document. Counterfactuals: the same mask on an isolated
/// group or on a raster layer, a Pass Through group without a mask and one with a disabled mask get no entry.
#[test]
fn mask_on_a_pass_through_group_is_a_typed_loss_and_the_mask_stays_in_the_document() {
    bounded_case("mask_on_a_pass_through_group_is_a_typed_loss_and_the_mask_stays_in_the_document", || {
        let code = hsk_studio_pigment::render_plan::PASS_THROUGH_MASK_OPERATOR;
        assert_eq!(code, "mask_on_pass_through");

        let p = proposal(&doc_with(&[divider(), pixel("inner", 0), with_user_mask(pass_through_folder("grp"), 0)]));
        let grp = id_of(&p, "grp");
        let found = losses(&p, code);
        assert_eq!(found.len(), 1, "{:?}", p.loss.to_json());
        assert_eq!((found[0].kind, found[0].path.as_str()), (LossKind::Unsupported, format!("layer[2]/mask(layer_id={grp})").as_str()));
        assert!(p.loss.to_json().contains(code), "the key survives the JSON projection");
        let layer = p.snapshot.document().layers.iter().find(|l| l.layer_id == grp).expect("group");
        let mask = layer.appearance.as_ref().and_then(|a| a.mask.as_ref()).expect("the mask stays in the document");
        assert!(mask.enabled && !mask.tiles.is_empty());

        for (what, layers) in [
            ("isolated group", vec![divider(), pixel("inner", 0), with_user_mask(folder("grp", 0), 0)]),
            ("raster layer", vec![with_user_mask(pixel("px", 0), 0)]),
            ("pass-through group without a mask", vec![divider(), pixel("inner", 0), pass_through_folder("grp")]),
            ("disabled mask (an authored no mask)", vec![divider(), pixel("inner", 0), with_user_mask(pass_through_folder("grp"), 2)]),
        ] {
            let p = proposal(&doc_with(&layers));
            assert!(losses(&p, code).is_empty(), "{what}: {:?}", p.loss.to_json());
        }
    });
}
