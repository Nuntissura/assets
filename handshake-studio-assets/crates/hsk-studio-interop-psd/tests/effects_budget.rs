//! SMB-INTEROP-PSD-BUDGET (PLR-053, R9 remediation): an effects-heavy but structurally valid PSD never fails the
//! import because of its effect count, under any code. Pigment owns the effect limits (`MAX_EFFECTS_PER_LAYER` = 64,
//! `MAX_EFFECTS_PER_PLAN` = 4096). A render plan counts only the ENABLED instances of an ENABLED stack on a layer
//! that is VISIBLE with all its groups (composite `convert_effects` and `lower_inner`, pigment
//! `PlanExtensions::validate`; the evidence is at the top of `src/effects.rs`), and the importer counts by the same
//! visibility rule, which over-counts a visible layer with no pixels and a visible clipped layer whose base is hidden
//! or empty (test `the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases`; nested
//! groups: `nested_hidden_groups_hide_every_descendant_until_the_outermost_closes`). A stack that does not fit is not mapped (its `lfx2` block stays byte-exact) and the layer reports a typed
//! loss: `effects_over_budget` (per layer, or past Folio's own per-stack ceiling),
//! `effects_over_document_budget` (counted instances past the plan limit: the crossing stack and every later
//! stack that has counted instances) or `effects_unrendered_over_document_budget` (the kept instances no plan counts
//! are bounded too: the crossing stack and every later stack that has NO counted instances). Each bound cuts only
//! its own category (R10): a stack with counted instances is never refused for the unrendered bound (past it the
//! stack keeps its counted instances and loses its switched-off ones, `effects_unrendered_dropped_over_document_budget`),
//! and a stack without counted instances is never refused for the counted bound. Folio's
//! record-validation node budget (records + kept instances, `validate_raster_records`) is sized from the input
//! inside those bounds. All inputs are synthetic byte fixtures built here, independently of the crate's encoder.

mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Budget, Code, MAX_EFFECTS, Resolution, Resolver, TileRef, validate_raster_records,
};
use hsk_studio_interop_psd::*;
use hsk_studio_pigment::effect_math::{MAX_EFFECTS_PER_LAYER, MAX_EFFECTS_PER_PLAN};

const OVER_LAYER: &str = "effects_over_budget";
const OVER_DOCUMENT: &str = "effects_over_document_budget";
const OVER_UNRENDERED: &str = "effects_unrendered_over_document_budget";
const DROPPED_UNRENDERED: &str = "effects_unrendered_dropped_over_document_budget";

/// Host identity minting; `stacks` counts the `SEFX` ids it was asked for.
struct Mint {
    n: u64,
    stacks: usize,
}
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.n)
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
        self.stacks += 1;
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
fn v_bool(value: bool) -> Vec<u8> {
    tagged(b"bool", vec![u8::from(value)])
}

/// The smallest mappable effect: its blend mode is the only required item (everything else defaults).
fn minimal_effect() -> Vec<u8> {
    v_objc("null", vec![("Md  ", v_enum("BlnM", "Mltp"))])
}
/// The smallest mappable effect with its own enable switch.
fn switched_effect(enabled: bool) -> Vec<u8> {
    v_objc(
        "null",
        vec![("Md  ", v_enum("BlnM", "Mltp")), ("enab", v_bool(enabled))],
    )
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
    lfx2_block(items)
}
fn lfx2_block(items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0);
    v.extend(be32(16));
    v.extend(ds_object("null", items));
    v
}

/// A 1x1 RGB 8-bit document from layers in FILE order (bottom to top).
fn doc_of(layers: &[TLayer]) -> Vec<u8> {
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
/// A raster layer with an `lfx2` block. It has no pixels (an empty rectangle): every layer with pixels costs one
/// tile (about 1 MiB of f32 samples plus its SHA-256) in the proposal, which would make the 130-layer cases slow
/// without testing anything about effects.
fn effects_layer(name: &'static str, hidden: bool, lfx2: Vec<u8>) -> TLayer {
    let mut l = TLayer::new(name, [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"lfx2", &lfx2, false)];
    if hidden {
        l.flags |= 0x02;
    }
    l
}

/// A 1x1 RGB 8-bit document whose raster layers carry `lfx2` blocks of the given effect counts, in file order.
fn doc_with_effect_counts(counts: &[usize]) -> Vec<u8> {
    let layers: Vec<TLayer> = counts
        .iter()
        .map(|&count| effects_layer("fx", false, lfx2_with(count)))
        .collect();
    doc_of(&layers)
}

// ---- stacks with switches, layers and groups in PANEL order (top-most first) ------------------

/// One effect stack: the enable switch of each instance, the stack's master switch, the layer's visibility and whether
/// the layer is clipped to the layer below it.
#[derive(Clone)]
struct Fx {
    enabled: Vec<bool>,
    master: bool,
    hidden: bool,
    clipped: bool,
}
impl Fx {
    /// `n` enabled instances in an enabled stack on a visible layer.
    fn live(n: usize) -> Self {
        Self::mixed(n, 0)
    }
    /// `live` enabled and `off` switched-off instances.
    fn mixed(live: usize, off: usize) -> Self {
        let mut enabled = vec![true; live];
        enabled.extend(vec![false; off]);
        Self {
            enabled,
            master: true,
            hidden: false,
            clipped: false,
        }
    }
    /// `n` switched-off instances.
    fn disabled(n: usize) -> Self {
        Self::mixed(0, n)
    }
    fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
    fn master_off(mut self) -> Self {
        self.master = false;
        self
    }
    /// Clipped to the layer below it in the panel (its clip base).
    fn clipped(mut self) -> Self {
        self.clipped = true;
        self
    }
    fn lfx2(&self) -> Vec<u8> {
        let keys = ["dropShadowMulti", "innerShadowMulti", "solidFillMulti"];
        let per_list = self.enabled.len().div_ceil(keys.len()).max(1);
        let mut items = Vec::new();
        if !self.master {
            items.push(("masterFXSwitch", v_bool(false)));
        }
        for (key, chunk) in keys.iter().zip(self.enabled.chunks(per_list)) {
            items.push((
                *key,
                v_list(chunk.iter().map(|&on| switched_effect(on)).collect()),
            ));
        }
        lfx2_block(items)
    }
}
enum Node {
    Layer(Fx),
    Group {
        hidden: bool,
        children: Vec<Fx>,
    },
    /// A group whose children are nodes themselves, so groups nest (panel order, top-most first).
    Nested {
        hidden: bool,
        children: Vec<Node>,
    },
}
/// The name of the `n`th stack layer of a [`build`] document, counting in panel order.
fn name_of(n: usize) -> String {
    format!("L{n}")
}
fn leaked(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}
fn divider() -> TLayer {
    let mut l = TLayer::new("</Layer group>", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    l.blocks = vec![layer_block(b"lsct", &be32(3), false)];
    l
}
fn folder(hidden: bool) -> TLayer {
    let mut l = TLayer::new("G", [0, 0, 0, 0]);
    l.channels = empty_channels(&[0, 1, 2]);
    let mut lsct = be32(1);
    lsct.extend_from_slice(b"8BIM");
    lsct.extend_from_slice(b"norm");
    l.blocks = vec![layer_block(b"lsct", &lsct, false)];
    if hidden {
        l.flags |= 0x02;
    }
    l
}
/// The stack layer of `fx`, named after `next` (which it advances).
fn stack_layer(fx: &Fx, next: &mut usize) -> TLayer {
    let mut layer = effects_layer(leaked(name_of(*next)), fx.hidden, fx.lfx2());
    if fx.clipped {
        layer.clipping = 1;
    }
    *next += 1;
    layer
}
/// The layers of `nodes` (panel order, top-most first) in FILE order (bottom to top); groups nest.
fn file_layers(nodes: &[Node], next: &mut usize) -> Vec<TLayer> {
    let mut panel: Vec<Vec<TLayer>> = Vec::new();
    for node in nodes {
        panel.push(match node {
            Node::Layer(fx) => vec![stack_layer(fx, next)],
            Node::Group { hidden, children } => {
                let kids: Vec<TLayer> = children.iter().map(|fx| stack_layer(fx, next)).collect();
                // file order of one group: bounding divider, children bottom to top, folder
                let mut file = vec![divider()];
                file.extend(kids.into_iter().rev());
                file.push(folder(*hidden));
                file
            }
            Node::Nested { hidden, children } => {
                let mut file = vec![divider()];
                file.extend(file_layers(children, next));
                file.push(folder(*hidden));
                file
            }
        });
    }
    panel.into_iter().rev().flatten().collect()
}
/// The document of `nodes` given in panel order (top-most first); the stack layers are named `L0`, `L1`, ... in
/// that order, a group's children continuing the numbering.
fn build(nodes: &[Node]) -> Vec<u8> {
    doc_of(&file_layers(nodes, &mut 0))
}
fn layers_of(fxs: impl IntoIterator<Item = Fx>) -> Vec<Node> {
    fxs.into_iter().map(Node::Layer).collect()
}
fn repeated(n: usize, fx: Fx) -> impl Iterator<Item = Fx> {
    std::iter::repeat_n(fx, n)
}

// ---- import and inspection helpers ----------------------------------------------------------

fn import(bytes: &[u8]) -> Result<(PsdDocument, ImportProposal)> {
    import_counting(bytes).map(|(doc, proposal, _)| (doc, proposal))
}
/// [`import`], and how many `SEFX` ids the importer asked the host to mint.
fn import_counting(bytes: &[u8]) -> Result<(PsdDocument, ImportProposal, usize)> {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let mut mint = Mint { n: 0, stacks: 0 };
    let proposal = to_proposal(
        &doc,
        &SourceIdentity::of(bytes),
        &cfg,
        &mut mint,
        None,
        &CancellationToken::default(),
    )?;
    Ok((doc, proposal, mint.stacks))
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

/// What became of the effect stack of a layer.
#[derive(Debug, PartialEq, Eq)]
enum Fate {
    /// Mapped to a record with this many instances (and no budget loss entry).
    Kept(usize),
    /// Mapped to a record with this many instances, all of them enabled: the switched-off instances did not fit and
    /// the single typed loss entry (kind `Transformed`, `effects_unrendered_dropped_over_document_budget`) names the
    /// layer.
    CountedOnly(usize),
    /// Not mapped; the single typed loss entry names the layer and has this code.
    Lost(&'static str),
}
fn layer_id_by_name(p: &ImportProposal, name: &str) -> String {
    p.snapshot
        .document()
        .layers
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("layer {name} is in the document"))
        .layer_id
        .clone()
}
fn fate(p: &ImportProposal, name: &str) -> Fate {
    let id = layer_id_by_name(p, name);
    let suffix = format!("lfx2(layer_id={id})");
    let entries: Vec<&LossEntry> = p
        .loss
        .entries
        .iter()
        .filter(|e| e.path.ends_with(&suffix))
        .collect();
    match p.raster_records.layer(&id).and_then(|r| r.effects.as_ref()) {
        Some(stack) => match entries.as_slice() {
            [] => Fate::Kept(stack.effects.len()),
            [entry] => {
                assert_eq!(entry.kind, LossKind::Transformed, "{name}");
                assert_eq!(entry.code, DROPPED_UNRENDERED, "{name}");
                assert!(entry.path.starts_with("layer["), "{name}: {}", entry.path);
                assert!(
                    stack.effects.iter().all(|effect| effect.enabled),
                    "{name}: only the counted instances are recorded"
                );
                Fate::CountedOnly(stack.effects.len())
            }
            _ => panic!("{name}: at most one budget loss entry is expected, got {entries:?}"),
        },
        None => {
            let [entry] = entries.as_slice() else {
                panic!(
                    "{name}: exactly one loss entry naming the layer is expected, got {entries:?}"
                );
            };
            assert_eq!(entry.kind, LossKind::Unsupported, "{name}");
            assert!(entry.path.starts_with("layer["), "{name}: {}", entry.path);
            Fate::Lost(entry.code)
        }
    }
}
fn assert_fates(p: &ImportProposal, expected: &[(usize, Fate)]) {
    for (n, want) in expected {
        assert_eq!(&fate(p, &name_of(*n)), want, "layer {}", name_of(*n));
    }
}
/// Effect stacks held by the records (one `SEFX` id is minted for each).
fn kept_stacks(p: &ImportProposal) -> usize {
    p.raster_records
        .layers
        .iter()
        .filter(|r| r.effects.is_some())
        .count()
}
/// The source ordinals of the instances a kept stack holds (an instance id is `<key>-<ordinal>`, the ordinal is the
/// instance's position in the descriptor's stack, switched-off instances included).
fn kept_ordinals(p: &ImportProposal, name: &str) -> Vec<usize> {
    let id = layer_id_by_name(p, name);
    let stack = p
        .raster_records
        .layer(&id)
        .and_then(|r| r.effects.as_ref())
        .unwrap_or_else(|| panic!("{name} holds a stack"));
    stack
        .effects
        .iter()
        .map(|effect| {
            effect
                .instance_id
                .rsplit('-')
                .next()
                .and_then(|ordinal| ordinal.parse().ok())
                .unwrap_or_else(|| panic!("{name}: instance id {}", effect.instance_id))
        })
        .collect()
}
/// Effect instances held by the records.
fn kept_total(p: &ImportProposal) -> usize {
    p.raster_records
        .layers
        .iter()
        .map(|r| r.effects.as_ref().map_or(0, |s| s.effects.len()))
        .sum()
}
/// Folio's verdict on the records with `nodes` as the node budget: `Ok` or its error code.
fn validate_with_nodes(p: &ImportProposal, nodes: usize) -> std::result::Result<(), Code> {
    validate_raster_records(
        &p.raster_records,
        p.snapshot.document(),
        Budget { nodes, ..budget() },
        &CancellationToken::default(),
        &Host,
    )
    .map(|_| ())
    .map_err(|d| d.code)
}
/// Folio charges one node per record layer and one per effect instance: the need is exactly records + instances.
fn assert_record_need_is_exact(p: &ImportProposal) -> usize {
    let needed = p.raster_records.layers.len() + kept_total(p);
    assert_eq!(
        validate_with_nodes(p, needed),
        Ok(()),
        "records + kept instances is the whole node need"
    );
    assert_eq!(
        validate_with_nodes(p, needed - 1),
        Err(Code::BudgetExceeded),
        "one node less is Folio's budget error"
    );
    needed
}

/// The reviewer's case (R8 review B, PLR-053): several layers whose stacks are far past any limit used to
/// fail the WHOLE import as `raster_records_rejected` (Folio's node budget ran out), a code the crate itself
/// documents as a mapping defect. Now each such layer keeps the document and loses only its stack, with a
/// typed loss naming the layer; the layers at and under the limit keep theirs. The six degraded layers carry
/// 4200 effects together, more than the plan limit, and still add nothing to the plan total (only kept
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
                    .filter(|e| e.code == OVER_LAYER)
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
                    .filter(|e| e.code == OVER_LAYER)
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
            assert_eq!(kept_total(&p), MAX_EFFECTS_PER_PLAN);
            assert!(
                !p.loss.has_code(OVER_LAYER) && !p.loss.has_code(OVER_DOCUMENT),
                "nothing at the limit is degraded"
            );

            let needed = assert_record_need_is_exact(&p);
            assert_eq!(needed, layers + MAX_EFFECTS_PER_PLAN);
            assert!(
                needed > budget().nodes,
                "the caller's default budget alone could not have admitted this document"
            );
        },
    );
}

/// More counted instances than pigment's per-plan limit never fail the import: the stack that takes the total past
/// the limit and every stack after it are typed losses naming their layers (`effects_over_document_budget`), the
/// stacks before it are intact, and the `lfx2` blocks of the lost stacks stay in the document. Import order is panel
/// order, top-most first (a group before its children). R9: the first version of this lane failed the whole import
/// with `psd_effect_limit` here (`PsdError::EffectLimit`, now gone), counting hidden and disabled instances too.
#[test]
fn effects_over_the_document_cap_become_typed_losses_from_the_crossing_stack_on() {
    bounded_case(
        "effects_over_the_document_cap_become_typed_losses_from_the_crossing_stack_on",
        || {
            // 4097 counted instances over 65 layers: 64 x 64 fill the plan limit, the 65th stack is one over
            let mut panel = layers_of(repeated(
                MAX_EFFECTS_PLAN_LAYERS,
                Fx::live(MAX_EFFECTS_PER_LAYER),
            ));
            panel.push(Node::Layer(Fx::live(1)));
            let bytes = build(&panel);
            let (doc, p, minted) =
                import_counting(&bytes).expect("4097 counted effects: the import continues");
            assert_eq!(
                minted, MAX_EFFECTS_PLAN_LAYERS,
                "a refused stack consumes no SEFX id: one is minted per kept stack"
            );
            assert_eq!(
                p.snapshot.document().layers.len(),
                MAX_EFFECTS_PLAN_LAYERS + 1,
                "every layer is in the document"
            );
            let mut want: Vec<(usize, Fate)> = (0..MAX_EFFECTS_PLAN_LAYERS)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.push((MAX_EFFECTS_PLAN_LAYERS, Fate::Lost(OVER_DOCUMENT)));
            assert_fates(&p, &want);
            assert_eq!(
                kept_total(&p),
                MAX_EFFECTS_PER_PLAN,
                "the earlier stacks are intact"
            );
            assert_eq!(
                write_psd(&doc, &Limits::default()).unwrap(),
                bytes,
                "the lost stack's lfx2 block is still in the document"
            );
            assert_record_need_is_exact(&p);

            // The cut is a prefix of the import order: 63 x 64 + 60 = 4092 kept; the next stack (10) crosses; the
            // 2 after it WOULD fit (4094) and is lost with it; a later stack over the per-layer limit keeps its
            // own code; the last stack (1) is lost with the cut.
            let mut counts: Vec<usize> = vec![MAX_EFFECTS_PER_LAYER; 63];
            counts.extend([60, 10, 2, MAX_EFFECTS_PER_LAYER + 6, 1]);
            let (_, p) = import(&build(&layers_of(counts.iter().map(|&n| Fx::live(n)))))
                .expect("a prefix is kept and the import continues");
            let mut want: Vec<(usize, Fate)> = (0..63)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.extend([
                (63, Fate::Kept(60)),
                (64, Fate::Lost(OVER_DOCUMENT)),
                (65, Fate::Lost(OVER_DOCUMENT)),
                (66, Fate::Lost(OVER_LAYER)),
                (67, Fate::Lost(OVER_DOCUMENT)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), 63 * MAX_EFFECTS_PER_LAYER + 60);
            assert_record_need_is_exact(&p);
        },
    );
}

/// The count is the render plan's, not the number of mapped instances: with the counted total exactly at the plan
/// limit, a hidden layer, a layer whose stack master switch is off, a stack whose instances are all switched off and
/// a layer inside a hidden group are all kept (no plan lowers or counts them), while one more live instance on a
/// visible layer crosses the cap, and so does the same stack inside a VISIBLE group. Evidence for the rule:
/// `src/effects.rs` (composite `convert_effects` keeps enabled instances of an enabled stack; `lower_inner` lowers
/// only layers reachable through visible inputs; pigment `PlanExtensions::validate` counts what a plan carries).
#[test]
fn hidden_layers_and_disabled_effects_do_not_count_toward_the_document_cap() {
    bounded_case(
        "hidden_layers_and_disabled_effects_do_not_count_toward_the_document_cap",
        || {
            let full = || repeated(MAX_EFFECTS_PLAN_LAYERS, Fx::live(MAX_EFFECTS_PER_LAYER));
            let mut panel = layers_of(full());
            panel.extend(layers_of([
                Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
                Fx::live(MAX_EFFECTS_PER_LAYER).master_off(),
                Fx::disabled(MAX_EFFECTS_PER_LAYER),
            ]));
            panel.push(Node::Group {
                hidden: true,
                children: vec![Fx::live(MAX_EFFECTS_PER_LAYER)],
            });
            panel.push(Node::Layer(Fx::live(1)));
            let (_, p) =
                import(&build(&panel)).expect("hidden and disabled instances are not counted");
            let first = MAX_EFFECTS_PLAN_LAYERS;
            let mut want: Vec<(usize, Fate)> = (0..first)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.extend([
                (first, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 1, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 2, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 3, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 4, Fate::Lost(OVER_DOCUMENT)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(
                kept_total(&p),
                MAX_EFFECTS_PER_PLAN + 4 * MAX_EFFECTS_PER_LAYER,
                "4096 counted + 4 x 64 kept that no plan counts"
            );
            assert_record_need_is_exact(&p);

            // counterfactual for the group rule: the same child in a VISIBLE group counts and crosses the cap
            let mut panel = layers_of(full());
            panel.push(Node::Group {
                hidden: false,
                children: vec![Fx::live(MAX_EFFECTS_PER_LAYER)],
            });
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(&p, &[(first, Fate::Lost(OVER_DOCUMENT))]);
        },
    );
}

/// The per-layer limit counts the same way: 64 LIVE instances of a visible layer. A stack of 100 instances of
/// which 36 are switched off is within pigment's limit (composite `convert_effects` refuses a layer over 64 live
/// instances, not over 64 mapped ones) and is kept whole; 65 live is over; a hidden layer, or a stack with its
/// master switch off, counts nothing and keeps 100 live instances.
#[test]
fn the_per_layer_limit_counts_live_instances_of_visible_layers() {
    bounded_case(
        "the_per_layer_limit_counts_live_instances_of_visible_layers",
        || {
            let panel = layers_of([
                Fx::mixed(24, 76),
                Fx::mixed(MAX_EFFECTS_PER_LAYER + 1, 35),
                Fx::live(100).hidden(),
                Fx::live(100).master_off(),
                Fx::mixed(MAX_EFFECTS_PER_LAYER, 36),
                Fx::mixed(MAX_EFFECTS_PER_LAYER + 1, 0),
            ]);
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(
                &p,
                &[
                    (0, Fate::Kept(100)),
                    (1, Fate::Lost(OVER_LAYER)),
                    (2, Fate::Kept(100)),
                    (3, Fate::Kept(100)),
                    (4, Fate::Kept(100)),
                    (5, Fate::Lost(OVER_LAYER)),
                ],
            );
            assert_record_need_is_exact(&p);
        },
    );
}

/// Folio refuses a stack of more than `MAX_EFFECTS` (1024) instances, whether or not any is live: that would be
/// a rejected import (`raster_records_rejected`). The importer does not map such a stack, whatever its switches,
/// and reports the layer; a stack of exactly 1024 switched-off instances is kept.
#[test]
fn a_stack_past_folios_ceiling_is_a_typed_loss_not_a_rejected_import() {
    bounded_case(
        "a_stack_past_folios_ceiling_is_a_typed_loss_not_a_rejected_import",
        || {
            let panel = layers_of([
                Fx::disabled(MAX_EFFECTS),
                Fx::disabled(MAX_EFFECTS + 1),
                Fx::live(MAX_EFFECTS + 1).hidden(),
            ]);
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(
                &p,
                &[
                    (0, Fate::Kept(MAX_EFFECTS)),
                    (1, Fate::Lost(OVER_LAYER)),
                    (2, Fate::Lost(OVER_LAYER)),
                ],
            );
            validate_raster_records(
                &p.raster_records,
                p.snapshot.document(),
                budget(),
                &CancellationToken::default(),
                &Host,
            )
            .expect("what was kept validates under the caller's own budget");
        },
    );
}

/// The kept instances no plan counts are bounded too (the crate's own bound, the size of pigment's plan limit), so
/// the memory and the record-validation work of the instances nobody renders stay bounded: 64 hidden layers x 64
/// are kept, the next hidden stack crosses and is lost with `effects_unrendered_over_document_budget`. A VISIBLE
/// stack after the crossing is not touched by that bound (R10: it is governed by the counted bounds alone, see
/// `the_unrendered_bound_never_costs_a_stack_with_counted_instances_its_rendered_effects`). With the counted bound
/// filled as well, the whole document holds at most 2 x 4096 instances (a visible stack after that is lost to the
/// COUNTED bound, `effects_over_document_budget`), and the node budget the importer gives the record validation is
/// exactly records + those instances, far above the caller's 4096: the importer sizes it, the caller's `nodes` does
/// not bound it (documented on `ProposalConfig::budget`).
#[test]
fn instances_no_plan_counts_are_bounded_too_so_the_kept_total_and_the_record_validation_stay_bounded()
 {
    bounded_case(
        "instances_no_plan_counts_are_bounded_too_so_the_kept_total_and_the_record_validation_stay_bounded",
        || {
            let hidden = || {
                repeated(
                    MAX_EFFECTS_PLAN_LAYERS,
                    Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
                )
            };

            // the bound alone: 4096 hidden instances kept, then a hidden stack is lost; the visible stack after it
            // has counted instances and a counted total of 0, so it is kept (R10)
            let mut panel = layers_of(hidden());
            panel.extend(layers_of([Fx::live(1).hidden(), Fx::live(1)]));
            let (_, p) = import(&build(&panel)).expect("imports");
            let mut want: Vec<(usize, Fate)> = (0..MAX_EFFECTS_PLAN_LAYERS)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.extend([
                (MAX_EFFECTS_PLAN_LAYERS, Fate::Lost(OVER_UNRENDERED)),
                (MAX_EFFECTS_PLAN_LAYERS + 1, Fate::Kept(1)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), MAX_EFFECTS_PER_PLAN + 1);

            // both bounds full: 64 visible + 64 hidden stacks of 64, then one more of each kind
            let mut panel = layers_of(repeated(
                MAX_EFFECTS_PLAN_LAYERS,
                Fx::live(MAX_EFFECTS_PER_LAYER),
            ));
            panel.extend(layers_of(hidden()));
            panel.extend(layers_of([Fx::live(1).hidden(), Fx::live(1)]));
            let (_, p) = import(&build(&panel)).expect("imports");
            let layers = 2 * MAX_EFFECTS_PLAN_LAYERS;
            let mut want: Vec<(usize, Fate)> = (0..layers)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.extend([
                (layers, Fate::Lost(OVER_UNRENDERED)),
                (layers + 1, Fate::Lost(OVER_DOCUMENT)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), 2 * MAX_EFFECTS_PER_PLAN);
            let needed = assert_record_need_is_exact(&p);
            assert_eq!(
                needed,
                layers + 2 * MAX_EFFECTS_PER_PLAN,
                "records + at most 2 x the plan limit"
            );
            assert!(
                needed > budget().nodes,
                "the caller's default nodes did not bound it"
            );
        },
    );
}

/// R10 (review A, lead clarification): the 4096 bound on the instances no plan counts (hidden layers and groups,
/// master-off stacks, switched-off instances) bounds ONLY those. Once it is crossed in panel order, a later stack that
/// has counted instances keeps them: it is governed by the counted bounds (64 per layer, 4096 per document) alone. A
/// later stack with NO counted instances is lost with `effects_unrendered_over_document_budget`, and the cut is a
/// prefix (a later small uncounted stack that would still fit is lost with it). A stack with counted instances whose
/// own switched-off instances do not fit is mapped with its counted instances in their stack order and without the
/// switched-off ones, with a `Transformed` entry `effects_unrendered_dropped_over_document_budget`; that also closes
/// the unrendered side. So the memory bound (records + at most 2 x 4096 instances) holds without a rendered effect
/// ever being lost to the count of unrendered ones.
#[test]
fn the_unrendered_bound_never_costs_a_stack_with_counted_instances_its_rendered_effects() {
    bounded_case(
        "the_unrendered_bound_never_costs_a_stack_with_counted_instances_its_rendered_effects",
        || {
            let hidden = || {
                repeated(
                    MAX_EFFECTS_PLAN_LAYERS,
                    Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
                )
            };
            let kept_64 = |count: usize| -> Vec<(usize, Fate)> {
                (0..count)
                    .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                    .collect()
            };
            let first = MAX_EFFECTS_PLAN_LAYERS;

            // A: the bound is crossed early in panel order by a hidden stack (L64); every visible stack after it
            // with counted instances keeps its effects, every stack without counted instances after it is lost
            let mut panel = layers_of(hidden());
            panel.extend(layers_of([
                Fx::live(1).hidden(),
                Fx::live(5),
                Fx::live(1).hidden(),
                Fx::disabled(2),
                Fx::live(2).master_off(),
            ]));
            panel.push(Node::Group {
                hidden: true,
                children: vec![Fx::live(2)],
            });
            panel.push(Node::Group {
                hidden: false,
                children: vec![Fx::live(2)],
            });
            panel.push(Node::Layer(Fx::live(MAX_EFFECTS_PER_LAYER)));
            let bytes = build(&panel);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            let mut want = kept_64(first);
            want.extend([
                (first, Fate::Lost(OVER_UNRENDERED)),
                (first + 1, Fate::Kept(5)),
                (first + 2, Fate::Lost(OVER_UNRENDERED)),
                (first + 3, Fate::Lost(OVER_UNRENDERED)),
                (first + 4, Fate::Lost(OVER_UNRENDERED)),
                (first + 5, Fate::Lost(OVER_UNRENDERED)),
                (first + 6, Fate::Kept(2)),
                (first + 7, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), MAX_EFFECTS_PER_PLAN + 5 + 2 + 64);
            assert_eq!(minted, kept_stacks(&p), "one SEFX id per kept stack");
            assert_eq!(kept_stacks(&p), first + 3);
            assert_eq!(
                write_psd(&doc, &Limits::default()).unwrap(),
                bytes,
                "the lfx2 blocks of the lost stacks stay in the document"
            );
            assert_record_need_is_exact(&p);

            // B: the bound exactly full; a stack with counted instances whose switched-off instances do not fit
            // keeps its counted ones only, in their stack order; a stack with no switched-off instance is whole
            let interleaved = Fx {
                enabled: vec![false, true, false, true, true],
                master: true,
                hidden: false,
                clipped: false,
            };
            let mut panel = layers_of(hidden());
            panel.extend(layers_of([
                Fx::mixed(3, 5),
                interleaved,
                Fx::mixed(2, 0),
                Fx::mixed(4, 1),
                Fx::live(1).hidden(),
            ]));
            let (_, p, minted) = import_counting(&build(&panel)).expect("imports");
            let mut want = kept_64(first);
            want.extend([
                (first, Fate::CountedOnly(3)),
                (first + 1, Fate::CountedOnly(3)),
                (first + 2, Fate::Kept(2)),
                (first + 3, Fate::CountedOnly(4)),
                (first + 4, Fate::Lost(OVER_UNRENDERED)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(
                kept_ordinals(&p, &name_of(first + 1)),
                [1, 3, 4],
                "the enabled instances, in their source order"
            );
            assert_eq!(kept_total(&p), MAX_EFFECTS_PER_PLAN + 3 + 3 + 2 + 4);
            assert_eq!(minted, kept_stacks(&p));
            assert_record_need_is_exact(&p);

            // C: the cut is a prefix. 4090 unrendered instances are kept; the stack with counted instances whose
            // 10 switched-off instances would pass the bound (4100) keeps its counted one and closes the side; a
            // later hidden stack that would still fit (4091) is lost with it
            let mut panel = layers_of(repeated(
                first - 1,
                Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
            ));
            panel.extend(layers_of([
                Fx::live(58).hidden(),
                Fx::mixed(1, 10),
                Fx::live(1).hidden(),
                Fx::disabled(1),
                Fx::live(2),
                Fx::mixed(2, 3),
            ]));
            let (_, p, minted) = import_counting(&build(&panel)).expect("imports");
            let last = first - 1;
            let mut want = kept_64(last);
            want.extend([
                (last, Fate::Kept(58)),
                (last + 1, Fate::CountedOnly(1)),
                (last + 2, Fate::Lost(OVER_UNRENDERED)),
                (last + 3, Fate::Lost(OVER_UNRENDERED)),
                (last + 4, Fate::Kept(2)),
                (last + 5, Fate::CountedOnly(2)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), 4090 + 1 + 2 + 2);
            assert_eq!(minted, kept_stacks(&p));
            assert_record_need_is_exact(&p);
        },
    );
}

/// The counted bounds cut only stacks that have counted instances (R10, the same class from the other side): with the
/// counted total at the plan limit, a later stack with counted instances is lost with
/// `effects_over_document_budget` (whole: its switched-off instances go with it), while hidden layers and groups,
/// master-off stacks and stacks with every instance switched off keep their stacks up to the unrendered bound. The
/// refused stack with counted instances does not close the unrendered side.
#[test]
fn the_counted_bound_never_costs_a_stack_without_counted_instances_its_record() {
    bounded_case(
        "the_counted_bound_never_costs_a_stack_without_counted_instances_its_record",
        || {
            let first = MAX_EFFECTS_PLAN_LAYERS;
            let mut panel = layers_of(repeated(first, Fx::live(MAX_EFFECTS_PER_LAYER)));
            panel.extend(layers_of([
                Fx::live(1),
                Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
                Fx::disabled(5),
                Fx::mixed(1, 2),
                Fx::live(MAX_EFFECTS_PER_LAYER).master_off(),
            ]));
            panel.push(Node::Group {
                hidden: true,
                children: vec![Fx::live(2)],
            });
            panel.push(Node::Layer(Fx::live(2)));
            let bytes = build(&panel);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            let mut want: Vec<(usize, Fate)> = (0..first)
                .map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER)))
                .collect();
            want.extend([
                (first, Fate::Lost(OVER_DOCUMENT)),
                (first + 1, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 2, Fate::Kept(5)),
                (first + 3, Fate::Lost(OVER_DOCUMENT)),
                (first + 4, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                (first + 5, Fate::Kept(2)),
                (first + 6, Fate::Lost(OVER_DOCUMENT)),
            ]);
            assert_fates(&p, &want);
            assert_eq!(
                kept_total(&p),
                MAX_EFFECTS_PER_PLAN + 64 + 5 + 64 + 2,
                "4096 counted + the stacks no plan counts"
            );
            assert_eq!(minted, kept_stacks(&p), "one SEFX id per kept stack");
            assert_eq!(
                write_psd(&doc, &Limits::default()).unwrap(),
                bytes,
                "the lfx2 blocks of the lost stacks stay in the document"
            );
            assert_record_need_is_exact(&p);
        },
    );
}

/// PLR-065 (R11 pre-review): groups nest, and a layer is uncounted while ANY group around it is hidden. The importer
/// keeps a depth count of the hidden groups it is inside (a boolean would be cleared by the first group that closes).
/// With the counted total at the plan limit: everything inside a hidden group stays uncounted (so it is kept) after an
/// inner hidden group closes, and inside a visible group nested in a hidden one; the first layer after the outermost
/// hidden group closes counts again and crosses the cap. The second document is the other order: a hidden group inside
/// a visible one hides only its own children.
#[test]
fn nested_hidden_groups_hide_every_descendant_until_the_outermost_closes() {
    bounded_case(
        "nested_hidden_groups_hide_every_descendant_until_the_outermost_closes",
        || {
            let full = || layers_of(repeated(MAX_EFFECTS_PLAN_LAYERS, Fx::live(MAX_EFFECTS_PER_LAYER)));
            let first = MAX_EFFECTS_PLAN_LAYERS;
            let live = || Fx::live(MAX_EFFECTS_PER_LAYER);
            let kept = Fate::Kept(MAX_EFFECTS_PER_LAYER);

            // hidden outer { hidden inner { L }, L, visible mid { L } }, then a visible layer
            let mut panel = full();
            panel.push(Node::Nested {
                hidden: true,
                children: vec![
                    Node::Nested {
                        hidden: true,
                        children: vec![Node::Layer(live())],
                    },
                    Node::Layer(live()),
                    Node::Nested {
                        hidden: false,
                        children: vec![Node::Layer(live())],
                    },
                ],
            });
            panel.push(Node::Layer(Fx::live(1)));
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(
                &p,
                &[
                    (first - 1, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                    (first, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                    (first + 1, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                    (first + 2, Fate::Kept(MAX_EFFECTS_PER_LAYER)),
                    (first + 3, Fate::Lost(OVER_DOCUMENT)),
                ],
            );
            assert_eq!(
                kept_total(&p),
                MAX_EFFECTS_PER_PLAN + 3 * MAX_EFFECTS_PER_LAYER,
                "4096 counted + the three layers inside the hidden outer group"
            );
            assert_record_need_is_exact(&p);

            // visible outer { hidden inner { L }, L }: the layer after the inner group counts
            let mut panel = full();
            panel.push(Node::Nested {
                hidden: false,
                children: vec![
                    Node::Nested {
                        hidden: true,
                        children: vec![Node::Layer(live())],
                    },
                    Node::Layer(live()),
                ],
            });
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(&p, &[(first, kept), (first + 1, Fate::Lost(OVER_DOCUMENT))]);
            assert_eq!(
                kept_total(&p),
                MAX_EFFECTS_PER_PLAN + MAX_EFFECTS_PER_LAYER,
                "4096 counted + the layer inside the hidden inner group"
            );
        },
    );
}

/// PLR-064 (R11 pre-review): the importer counts by visibility alone, so it counts two kinds of layer that no render plan
/// counts: a visible layer with no pixels (composite builds no node for it) and a visible clipped layer whose clip base
/// is hidden or empty (composite drops it with its base). This is the documented over-count (`effects.rs`,
/// `ProposalConfig::budget`, DESCRIPTOR `effect_limits`). Every layer here has no tiles (asserted), so with the counted
/// total at the plan limit a plan would count 0; the importer's stacks are lost as for any visible layer, per document
/// and per layer, and the clipped layers on a hidden base and on an empty (visible, no-pixel) base are counted too.
#[test]
fn the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases() {
    bounded_case(
        "the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases",
        || {
            let first = MAX_EFFECTS_PLAN_LAYERS;
            let full = || layers_of(repeated(first, Fx::live(MAX_EFFECTS_PER_LAYER)));

            // (a) visible layers with no pixels: 64 x 64 count, the next layer crosses the cap, a 65-instance layer is
            // over the per-layer limit
            let mut panel = full();
            panel.extend(layers_of([Fx::live(1)]));
            let (_, p) = import(&build(&panel)).expect("imports");
            assert!(p.tiles.is_empty(), "no layer of this document has pixels");
            assert_fates(&p, &[(first - 1, Fate::Kept(MAX_EFFECTS_PER_LAYER)), (first, Fate::Lost(OVER_DOCUMENT))]);
            let (_, p) = import(&build(&layers_of([Fx::live(MAX_EFFECTS_PER_LAYER + 1)]))).expect("imports");
            assert!(p.tiles.is_empty());
            assert_fates(&p, &[(0, Fate::Lost(OVER_LAYER))]);

            // (b) a clipped layer above a hidden base, and above an empty visible base (switched-off instances only):
            // the clipped layer is counted although the plan drops it with its base
            for (what, base) in [("hidden base", Fx::disabled(1).hidden()), ("empty base", Fx::disabled(1))] {
                let mut panel = full();
                panel.extend(layers_of([Fx::live(1).clipped(), base.clone()]));
                let (_, p) = import(&build(&panel)).expect("imports");
                assert!(p.tiles.is_empty(), "{what}: no layer has pixels");
                assert_fates(&p, &[(first, Fate::Lost(OVER_DOCUMENT)), (first + 1, Fate::Kept(1))]);

                let panel = layers_of([Fx::live(MAX_EFFECTS_PER_LAYER + 1).clipped(), base]);
                let (_, p) = import(&build(&panel)).expect("imports");
                assert_fates(&p, &[(0, Fate::Lost(OVER_LAYER)), (1, Fate::Kept(1))]);
            }

            // the clip flag is what matters for the base rule: the same clipped layer with no base below it is still
            // a visible layer with counted instances
            let mut panel = full();
            panel.extend(layers_of([Fx::live(1).clipped()]));
            let (_, p) = import(&build(&panel)).expect("imports");
            assert_fates(&p, &[(first, Fate::Lost(OVER_DOCUMENT))]);
        },
    );
}

/// PLR-064 (V4-07): the shipped DESCRIPTOR (`effect_limits`) states both over-counts and cites the test that pins them,
/// and the test it cites exists in this file.
#[test]
fn descriptor_states_the_importers_over_counts() {
    let effect_limits = DESCRIPTOR
        .split("\"effect_limits\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the DESCRIPTOR has an effect_limits entry");
    let cited = "the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases";
    for needle in [
        "the_importer_counts_by_visibility_alone",
        "over_counts_a_visible_layer_with_no_pixels",
        "a_visible_clipped_layer_whose_base_is_hidden_or_empty",
        "which_no_plan_counts",
        cited,
    ] {
        assert!(effect_limits.contains(needle), "effect_limits does not state {needle}");
    }
    assert!(
        include_str!("effects_budget.rs").contains(&format!("fn {cited}()")),
        "the cited test does not exist"
    );
}

/// Layers that exactly fill pigment's plan limit at the per-layer limit.
const MAX_EFFECTS_PLAN_LAYERS: usize = MAX_EFFECTS_PER_PLAN / MAX_EFFECTS_PER_LAYER;
