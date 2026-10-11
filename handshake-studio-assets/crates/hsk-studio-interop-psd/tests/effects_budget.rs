//! SMB-INTEROP-PSD-BUDGET (PLR-053): an effects-heavy but structurally valid PSD must not fail the whole
//! import under a mapping-defect code. Pigment owns the effect limits (`MAX_EFFECTS_PER_LAYER` = 64,
//! `MAX_EFFECTS_PER_PLAN` = 4096): a layer over its limit keeps the document and loses only its stack
//! (typed loss `effects_over_budget`, the `lfx2` block stays byte-exact); more mapped effects than the plan
//! limit is the typed input error `psd_effect_limit`; Folio's record-validation node budget (records +
//! effect instances, `validate_raster_records`) is sized from the input inside those limits.
//! All inputs are synthetic byte fixtures built here, independently of the crate's encoder.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Code, Resolution, Resolver, TileRef, validate_raster_records};
use hsk_studio_interop_psd::*;
use hsk_studio_pigment::effect_math::{MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN};

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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

/// The caller budget of every other test in this crate: Folio's default node budget (4096).
fn budget() -> Budget {
    Budget {
        input_bytes: 8 << 20,
        payload_bytes: 4 << 20,
        ..Budget::default()
    }
}

// ---- Photoshop descriptor builders (independent of the crate's reader) ----------------------

fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 {
        be32(0)
    } else {
        be32(k.len() as u32)
    };
    v.extend_from_slice(k.as_bytes());
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0); // empty Unicode class name
    v.extend(ds_key(class));
    v.extend(be32(items.len() as u32));
    for (key, tagged) in items {
        v.extend(ds_key(key));
        v.extend(tagged);
    }
    v
}
fn tagged(tag: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.extend(payload);
    v
}
fn v_objc(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    tagged(b"Objc", ds_object(class, items))
}
fn v_list(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut v = be32(items.len() as u32);
    for item in items {
        v.extend(item);
    }
    tagged(b"VlLs", v)
}
fn v_enum(type_id: &str, value: &str) -> Vec<u8> {
    let mut v = ds_key(type_id);
    v.extend(ds_key(value));
    tagged(b"enum", v)
}

/// The smallest mappable effect: its blend mode is the only required item (everything else defaults).
fn minimal_effect() -> Vec<u8> {
    v_objc("null", vec![("Md  ", v_enum("BlnM", "Mltp"))])
}
/// An `lfx2` block of exactly `count` effects. They are spread over three multi lists so that no list passes
/// the descriptor reader's 512 items per container (the reader's own bound, unchanged by this lane).
fn lfx2_with(count: usize) -> Vec<u8> {
    let keys = ["dropShadowMulti", "innerShadowMulti", "solidFillMulti"];
    let per_list = count.div_ceil(keys.len());
    let mut left = count;
    let mut items = Vec::new();
    for key in keys {
        let n = left.min(per_list);
        left -= n;
        if n > 0 {
            items.push((key, v_list((0..n).map(|_| minimal_effect()).collect())));
        }
    }
    assert_eq!(left, 0);
    let mut v = be32(0);
    v.extend(be32(16));
    v.extend(ds_object("null", items));
    v
}

/// A 1x1 RGB 8-bit document whose raster layers carry `lfx2` blocks of the given effect counts, in file order.
fn doc_with_effect_counts(counts: &[usize]) -> Vec<u8> {
    let layers: Vec<TLayer> = counts
        .iter()
        .map(|&count| {
            let mut l = TLayer::new("fx", [0, 0, 1, 1]);
            l.channels = vec![(0, 0, vec![10]), (1, 0, vec![20]), (2, 0, vec![30])];
            l.blocks = vec![layer_block(b"lfx2", &lfx2_with(count), false)];
            l
        })
        .collect();
    let info = layer_info(&layers, false, false);
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

fn import(bytes: &[u8]) -> Result<(PsdDocument, ImportProposal)> {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let proposal = to_proposal(
        &doc,
        &SourceIdentity::of(bytes),
        &cfg,
        &mut Mint(0),
        None,
        &CancellationToken::default(),
    )?;
    Ok((doc, proposal))
}
fn layer_id_of(p: &ImportProposal, psd_index: usize) -> String {
    p.layer_map
        .iter()
        .find(|e| e.psd_index == psd_index)
        .map(|e| e.layer_id.clone())
        .expect("layer is mapped")
}
fn effect_count_of(p: &ImportProposal, psd_index: usize) -> Option<usize> {
    let id = layer_id_of(p, psd_index);
    p.raster_records
        .layer(&id)
        .and_then(|r| r.effects.as_ref())
        .map(|s| s.effects.len())
}

/// The reviewer's case (R8 review B, PLR-053): several layers whose stacks are far past any limit used to
/// fail the WHOLE import as `raster_records_rejected` (Folio's node budget ran out), a code the crate itself
/// documents as a mapping defect. Now each such layer keeps the document and loses only its stack, with a
/// typed loss naming the layer; the layers at and under the limit keep theirs. The six degraded layers carry
/// 4200 effects together, more than the plan limit, and still add nothing to the plan total (only mapped
/// instances count).
#[test]
fn effects_heavy_layers_lose_their_stack_with_a_typed_loss_and_the_document_imports() {
    bounded_case(
        "effects_heavy_layers_lose_their_stack_with_a_typed_loss_and_the_document_imports",
        || {
            assert_eq!(
                (MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN),
                (64, 4096),
                "the limits this test is written against"
            );
            // file order: six layers x 700 (4200 effects, more than the default node budget), one at the layer limit,
            // one over it by a single effect, one ordinary layer
            let counts = [
                700,
                700,
                700,
                700,
                700,
                700,
                MAX_EFFECTS_PER_LAYER,
                MAX_EFFECTS_PER_LAYER + 1,
                1,
            ];
            let bytes = doc_with_effect_counts(&counts);
            let (doc, p) = import(&bytes).expect("an effects-heavy but valid document must import");
            assert_eq!(
                p.snapshot.document().layers.len(),
                counts.len(),
                "every layer is in the document"
            );

            for (index, &count) in counts.iter().enumerate() {
                let id = layer_id_of(&p, index);
                let degraded: Vec<_> = p
                    .loss
                    .entries
                    .iter()
                    .filter(|e| e.code == "effects_over_budget")
                    .filter(|e| e.path == format!("layer[{index}]/tagged/lfx2(layer_id={id})"))
                    .collect();
                if count > MAX_EFFECTS_PER_LAYER {
                    assert_eq!(
                        degraded.len(),
                        1,
                        "layer {index} ({count} effects) is reported once, naming the layer"
                    );
                    assert_eq!(degraded[0].kind, LossKind::Unsupported);
                    assert_eq!(
                        effect_count_of(&p, index),
                        None,
                        "layer {index} keeps no stack"
                    );
                    assert!(
                        p.raster_records.layer(&id).is_none(),
                        "an effects-only layer has no other record"
                    );
                } else {
                    assert!(
                        degraded.is_empty(),
                        "layer {index} ({count} effects) is within the limit"
                    );
                    assert_eq!(
                        effect_count_of(&p, index),
                        Some(count),
                        "layer {index} keeps its whole stack"
                    );
                }
            }
            assert_eq!(
                p.loss
                    .entries
                    .iter()
                    .filter(|e| e.code == "effects_over_budget")
                    .count(),
                7
            );

            // nothing is dropped from the file: the unmapped stacks are still in the document bytes
            assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);
            // and what was mapped validates against the caller's own (default) budget
            validate_raster_records(
                &p.raster_records,
                p.snapshot.document(),
                budget(),
                &CancellationToken::default(),
                &Host,
            )
            .expect("the kept records validate");
        },
    );
}

/// Exactly pigment's per-plan limit (64 layers x 64 effects = 4096) imports fully. Folio charges one node per
/// record layer and one per effect instance (64 + 4096 = 4160), more than the caller's 4096 nodes, so the
/// importer sizes that one validation from the input: the need is asserted to be exactly records + instances by
/// validating the result directly at the boundary.
#[test]
fn effects_at_pigments_plan_limit_import_fully_with_the_record_budget_sized_from_the_input() {
    bounded_case(
        "effects_at_pigments_plan_limit_import_fully_with_the_record_budget_sized_from_the_input",
        || {
            let layers = MAX_EFFECTS_PLAN_LAYERS;
            let bytes = doc_with_effect_counts(&vec![MAX_EFFECTS_PER_LAYER; layers]);
            let (_, p) = import(&bytes).expect("a document exactly at the plan limit imports");
            assert_eq!(p.raster_records.layers.len(), layers);
            let total: usize = p
                .raster_records
                .layers
                .iter()
                .map(|r| r.effects.as_ref().map_or(0, |s| s.effects.len()))
                .sum();
            assert_eq!(total, MAX_EFFECTS_PER_PLAN);
            assert!(
                !p.loss.has_code("effects_over_budget"),
                "nothing at the limit is degraded"
            );

            let needed = layers + total;
            let check = |nodes: usize| {
                validate_raster_records(
                    &p.raster_records,
                    p.snapshot.document(),
                    Budget { nodes, ..budget() },
                    &CancellationToken::default(),
                    &Host,
                )
            };
            check(needed).expect("records + effect instances is the whole node need");
            assert_eq!(
                check(needed - 1).unwrap_err().code,
                Code::BudgetExceeded,
                "one node less is Folio's budget error"
            );
            assert!(
                needed > budget().nodes,
                "the caller's default budget alone could not have admitted this document"
            );
        },
    );
}

/// More mapped effects than pigment's per-plan limit is the document's own hard cap: a typed INPUT error
/// (`psd_effect_limit`, like `psd_layer_limit`), not `Proposal(..)`, which the crate reserves for mapping
/// defects. One effect over fails; the same document one effect under is the previous test.
#[test]
fn effects_over_the_plan_limit_are_a_typed_input_error_not_a_mapping_defect() {
    bounded_case(
        "effects_over_the_plan_limit_are_a_typed_input_error_not_a_mapping_defect",
        || {
            let over_by_a_layer = vec![MAX_EFFECTS_PER_LAYER; MAX_EFFECTS_PLAN_LAYERS + 1];
            let mut over_by_one = vec![MAX_EFFECTS_PER_LAYER; MAX_EFFECTS_PLAN_LAYERS];
            over_by_one.push(1);
            for (name, counts) in [
                ("a whole layer over", over_by_a_layer),
                ("one effect over", over_by_one),
            ] {
                let total: usize = counts.iter().sum();
                assert!(total > MAX_EFFECTS_PER_PLAN, "{name}: {total}");
                let err = import(&doc_with_effect_counts(&counts)).expect_err(name);
                assert_eq!(err.code(), "psd_effect_limit", "{name}: {err}");
                assert!(
                    !matches!(err, PsdError::Proposal(_)),
                    "{name}: an input limit is not a mapping defect"
                );
            }
        },
    );
}

/// Layers that exactly fill pigment's plan limit at the per-layer limit.
const MAX_EFFECTS_PLAN_LAYERS: usize = MAX_EFFECTS_PER_PLAN / MAX_EFFECTS_PER_LAYER;
