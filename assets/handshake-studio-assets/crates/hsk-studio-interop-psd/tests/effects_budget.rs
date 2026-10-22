//! SMB-INTEROP-PSD-BUDGET (PLR-053, R9 remediation): an effects-heavy but structurally valid PSD never fails the
//! import because of its effect count, under any code. Pigment owns the effect limits (`MAX_EFFECTS_PER_LAYER` = 64,
//! `MAX_EFFECTS_PER_PLAN` = 4096). A render plan counts only the ENABLED instances of an ENABLED stack on a layer
//! that is VISIBLE with all its groups (composite `convert_effects` and `lower_inner`, pigment
//! `PlanExtensions::validate`; the evidence is at the top of `src/effects.rs`), and the importer counts by the same
//! visibility rule, which over-counts a visible layer with no pixels and a visible clipped layer whose base is hidden
//! or empty (test `the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases`; nested
//! groups: `nested_hidden_groups_hide_every_descendant_until_the_outermost_closes`). A stack that does not fit is not mapped (its `lfx2` block stays byte-exact) and the layer reports a typed
//! loss: `effects_over_budget` (per layer, or a stack with no counted instances past Folio's own per-stack ceiling),
//! `effects_over_document_budget` (counted instances past the plan limit: the crossing stack and every later
//! stack that has counted instances) or `effects_unrendered_over_document_budget` (the kept instances no plan counts
//! are bounded too: the crossing stack and every later stack that has NO counted instances). Each bound cuts only
//! its own category (R10, PLR-063): a stack with counted instances is never refused for the unrendered bound (past it the
//! stack keeps its counted instances and loses its switched-off ones, `effects_unrendered_dropped_over_document_budget`),
//! nor for Folio's per-stack ceiling (same record, `effects_unrendered_dropped_over_stack_ceiling`, test
//! `folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances`),
//! and a stack without counted instances is never refused for the counted bound. Folio's
//! record-validation node budget (records + kept instances, `validate_raster_records`) is sized from the input
//! inside those bounds. The two `Transformed` dropped-instances losses name the count of the MAPPED switched-off
//! instances the record does not hold and the dropped instances add no note (PLR-089, SMB-INTEROP-PSD-LOSS-DETAIL); the
//! descriptor reader's 512
//! entries per list is a parse bound that cannot know a category (PLR-086), and so are its 8192 items per descriptor,
//! measured per effect kind (PLR-095). All inputs are synthetic byte fixtures built here, independently of the crate's
//! encoder.

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
const DROPPED_STACK: &str = "effects_unrendered_dropped_over_stack_ceiling";

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
    /// Mapped to a record with this many instances, all of them enabled: its counted and switched-off instances did not
    /// fit Folio's per-stack ceiling together and the single typed loss entry (kind `Transformed`,
    /// `effects_unrendered_dropped_over_stack_ceiling`) names the layer.
    CountedOnlyStack(usize),
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
/// How a loss path names the effect stack of layer `id`: `None` when it does not end in `lfx2(layer_id=<id>...)`;
/// `Some(None)` for `lfx2(layer_id=<id>)` (a stack that was not mapped); `Some(Some(count))` for
/// `lfx2(layer_id=<id>,dropped=<count>)` (a stack mapped without `count` MAPPED switched-off instances).
fn lfx2_loss_tail(path: &str, id: &str) -> Option<Option<usize>> {
    let tail = path.rsplit('/').next()?;
    let rest = tail.strip_prefix("lfx2(layer_id=")?.strip_prefix(id)?;
    if rest == ")" {
        return Some(None);
    }
    let count = rest.strip_prefix(",dropped=")?.strip_suffix(')')?;
    Some(Some(count.parse().ok()?))
}
/// The `dropped=<count>` of the `Transformed` entry of layer `name` (`None` when it has no such entry).
fn dropped_count(p: &ImportProposal, name: &str) -> Option<usize> {
    let id = layer_id_by_name(p, name);
    p.loss
        .entries
        .iter()
        .find_map(|e| match lfx2_loss_tail(&e.path, &id) {
            Some(Some(count)) => Some(count),
            _ => None,
        })
}
fn fate(p: &ImportProposal, name: &str) -> Fate {
    let id = layer_id_by_name(p, name);
    let entries: Vec<&LossEntry> = p
        .loss
        .entries
        .iter()
        .filter(|e| lfx2_loss_tail(&e.path, &id).is_some())
        .collect();
    match p.raster_records.layer(&id).and_then(|r| r.effects.as_ref()) {
        Some(stack) => match entries.as_slice() {
            [] => Fate::Kept(stack.effects.len()),
            [entry] => {
                assert_eq!(entry.kind, LossKind::Transformed, "{name}");
                assert!(entry.path.starts_with("layer["), "{name}: {}", entry.path);
                assert!(
                    matches!(lfx2_loss_tail(&entry.path, &id), Some(Some(count)) if count > 0),
                    "{name}: a dropped-instances entry names how many instances the record does not hold: {}",
                    entry.path
                );
                assert!(
                    stack.effects.iter().all(|effect| effect.enabled),
                    "{name}: only the counted instances are recorded"
                );
                match entry.code {
                    DROPPED_UNRENDERED => Fate::CountedOnly(stack.effects.len()),
                    DROPPED_STACK => Fate::CountedOnlyStack(stack.effects.len()),
                    other => panic!("{name}: unexpected Transformed code {other}"),
                }
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
            assert_eq!(
                lfx2_loss_tail(&entry.path, &id),
                Some(None),
                "{name}: a stack that was not mapped drops no instance from a record: {}",
                entry.path
            );
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

/// Folio refuses a stack of more than `MAX_EFFECTS` (1024) instances: that would be a rejected import
/// (`raster_records_rejected`). The importer does not map a stack with NO counted instances past it (switched-off
/// instances only, a hidden layer, a master-off stack) and reports the layer; a stack of exactly 1024 switched-off
/// instances is kept. A stack WITH counted instances is not lost to the ceiling, see
/// `folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances`.
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

/// PLR-063: Folio's 1024 instances per record hold the counted and the switched-off instances of a stack together, and
/// the counted category is bounded far below it (64 per layer), so the ceiling can only ever cut the switched-off
/// category. A stack with counted instances keeps ALL of them and drops its switched-off ones when the two do not fit
/// together (`Transformed` `effects_unrendered_dropped_over_stack_ceiling`, the same record as the unrendered bound
/// produces); a stack with NO counted instances (switched-off instances only, a hidden layer, a master-off stack) is
/// all switched-off instances and is not mapped past the ceiling (`effects_over_budget`). The reported shape is one
/// live instance and 1024 switched-off ones (1025 in all): before the fix the whole visible stack was lost. The
/// switched-off instances dropped for the ceiling were never kept, so they cost the document's bound on the
/// instances no plan counts nothing: five such stacks (5 x 1024 dropped) leave the whole 4096 to hidden stacks.
#[test]
fn folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances()
{
    bounded_case(
        "folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances",
        || {
            let room = MAX_EFFECTS - MAX_EFFECTS_PER_LAYER;
            let panel = layers_of([
                Fx::mixed(1, MAX_EFFECTS),
                Fx::mixed(1, MAX_EFFECTS - 1),
                Fx::mixed(MAX_EFFECTS_PER_LAYER, room),
                Fx::mixed(MAX_EFFECTS_PER_LAYER, room + 1),
                Fx::mixed(1, MAX_EFFECTS + 100),
                Fx::disabled(MAX_EFFECTS + 1),
                Fx::mixed(1, MAX_EFFECTS).hidden(),
                Fx::mixed(1, MAX_EFFECTS).master_off(),
            ]);
            let bytes = build(&panel);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            assert_fates(
                &p,
                &[
                    // 1 live + 1024 off: the reported shape, 1025 in all
                    (0, Fate::CountedOnlyStack(1)),
                    // 1 live + 1023 off: exactly 1024, whole
                    (1, Fate::Kept(MAX_EFFECTS)),
                    // 64 live + 960 off: exactly 1024, whole
                    (2, Fate::Kept(MAX_EFFECTS)),
                    // 64 live + 961 off: one past
                    (3, Fate::CountedOnlyStack(MAX_EFFECTS_PER_LAYER)),
                    // far past
                    (4, Fate::CountedOnlyStack(1)),
                    // no counted instance: switched-off instances only, hidden, master-off
                    (5, Fate::Lost(OVER_LAYER)),
                    (6, Fate::Lost(OVER_LAYER)),
                    (7, Fate::Lost(OVER_LAYER)),
                ],
            );
            assert_eq!(kept_stacks(&p), 5);
            assert_eq!(minted, 5, "a refused stack consumes no SEFX id");
            assert_eq!(
                kept_total(&p),
                1 + 2 * MAX_EFFECTS + MAX_EFFECTS_PER_LAYER + 1
            );
            assert_eq!(
                write_psd(&doc, &Limits::default()).unwrap(),
                bytes,
                "nothing is dropped from the file: the dropped instances are still in the lfx2 blocks"
            );
            assert_record_need_is_exact(&p);

            // the dropped instances cost the bound on the instances no plan counts nothing
            let mut panel = layers_of(repeated(5, Fx::mixed(1, MAX_EFFECTS)));
            panel.extend(layers_of(repeated(
                MAX_EFFECTS_PLAN_LAYERS,
                Fx::live(MAX_EFFECTS_PER_LAYER).hidden(),
            )));
            panel.extend(layers_of([Fx::live(1).hidden()]));
            let (_, p) = import(&build(&panel)).expect("imports");
            let mut want: Vec<(usize, Fate)> =
                (0..5).map(|n| (n, Fate::CountedOnlyStack(1))).collect();
            want.extend(
                (5..5 + MAX_EFFECTS_PLAN_LAYERS).map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER))),
            );
            want.push((5 + MAX_EFFECTS_PLAN_LAYERS, Fate::Lost(OVER_UNRENDERED)));
            assert_fates(&p, &want);
            assert_eq!(kept_total(&p), 5 + MAX_EFFECTS_PER_PLAN);
            assert_record_need_is_exact(&p);
        },
    );
}

/// The ceiling cuts the switched-off category wherever the counted instances sit in the stack: the stack is scanned to
/// its end after the switched-off instances stop fitting, so a counted instance that comes after more than 1024
/// switched-off ones is kept, and every kept instance keeps its source ordinal (its position in the descriptor's
/// stack, dropped instances included).
#[test]
fn a_counted_instance_after_more_switched_off_ones_than_the_ceiling_is_still_kept() {
    bounded_case(
        "a_counted_instance_after_more_switched_off_ones_than_the_ceiling_is_still_kept",
        || {
            let mut interleaved = vec![true];
            interleaved.extend(vec![false; MAX_EFFECTS + 76]);
            interleaved.push(true);
            interleaved.extend(vec![false; 10]);
            interleaved.push(true);
            let mut trailing = vec![false; MAX_EFFECTS + 5];
            trailing.extend([true, true]);
            let fx = |enabled: &Vec<bool>| Fx {
                enabled: enabled.clone(),
                master: true,
                hidden: false,
                clipped: false,
            };
            let ordinals = |enabled: &Vec<bool>| -> Vec<usize> {
                enabled
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(|(n, _)| n)
                    .collect()
            };
            let panel = layers_of([fx(&interleaved), fx(&trailing)]);
            let bytes = build(&panel);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            assert_fates(
                &p,
                &[
                    (0, Fate::CountedOnlyStack(3)),
                    (1, Fate::CountedOnlyStack(2)),
                ],
            );
            assert_eq!(
                ordinals(&interleaved),
                vec![0, MAX_EFFECTS + 77, MAX_EFFECTS + 88]
            );
            assert_eq!(kept_ordinals(&p, "L0"), ordinals(&interleaved));
            assert_eq!(ordinals(&trailing), vec![MAX_EFFECTS + 5, MAX_EFFECTS + 6]);
            assert_eq!(kept_ordinals(&p, "L1"), ordinals(&trailing));
            assert_eq!(minted, 2);
            assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);
        },
    );
}

/// PLR-063: the counted (live) bound still cuts what it bounds. A visible stack with more than 64 live instances is
/// not mapped (`effects_over_budget`) whatever its switched-off instances are and however many (1025 live, 1024
/// live, 65 live next to 1024 switched-off ones); 64 live instances next to 1024 switched-off ones are within the
/// live bound and keep the 64. A hidden stack of 1025 enabled instances has none counted, and the ceiling cuts it
/// whole. A refused stack consumes no `SEFX` id and its `lfx2` block stays in the file.
#[test]
fn the_live_bound_still_cuts_a_stack_whatever_its_switched_off_instances() {
    bounded_case(
        "the_live_bound_still_cuts_a_stack_whatever_its_switched_off_instances",
        || {
            let panel = layers_of([
                Fx::live(MAX_EFFECTS + 1),
                Fx::live(MAX_EFFECTS),
                Fx::mixed(MAX_EFFECTS_PER_LAYER + 1, MAX_EFFECTS),
                Fx::mixed(MAX_EFFECTS_PER_LAYER, MAX_EFFECTS),
                Fx::live(MAX_EFFECTS + 1).hidden(),
            ]);
            let bytes = build(&panel);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            assert_fates(
                &p,
                &[
                    (0, Fate::Lost(OVER_LAYER)),
                    (1, Fate::Lost(OVER_LAYER)),
                    (2, Fate::Lost(OVER_LAYER)),
                    (3, Fate::CountedOnlyStack(MAX_EFFECTS_PER_LAYER)),
                    (4, Fate::Lost(OVER_LAYER)),
                ],
            );
            assert_eq!(minted, 1, "a refused stack consumes no SEFX id");
            assert_eq!(kept_total(&p), MAX_EFFECTS_PER_LAYER);
            assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);
            assert_record_need_is_exact(&p);
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
        "both_caps_apply_the_document_cap_past_4096_apparent_counted_instances_and_the_per_layer_cap_past_64_on_one_such_layer",
        cited,
    ] {
        assert!(effect_limits.contains(needle), "effect_limits does not state {needle}");
    }
    assert!(
        include_str!("effects_budget.rs").contains(&format!("fn {cited}()")),
        "the cited test does not exist"
    );
}

/// PLR-063 (V4-07): the shipped DESCRIPTOR (`effect_limits` and the capabilities) states how Folio's per-stack ceiling
/// is counted (the two categories together, cutting only the switched-off one), names the loss code the importer
/// reports for it, and cites the test that pins it; the test it cites exists in this file.
#[test]
fn descriptor_states_the_folio_ceiling_rule() {
    let effect_limits = DESCRIPTOR
        .split("\"effect_limits\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the DESCRIPTOR has an effect_limits entry");
    let cited = "folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances";
    for needle in [
        "with_no_counted_instances_and_over_folios_per_stack_ceiling_of_1024_is_not_mapped_loss_effects_over_budget",
        "folios_per_stack_ceiling_of_1024_holds_counted_and_switched_off_instances_together_and_cuts_only_the_switched_off_ones",
        "a_stack_with_counted_instances_keeps_all_of_them_and_drops_its_switched_off_ones",
        "loss_transformed_effects_unrendered_dropped_over_stack_ceiling",
        "each_bound_cuts_only_its_own_category",
        cited,
    ] {
        assert!(
            effect_limits.contains(needle),
            "effect_limits does not state {needle}"
        );
    }
    assert!(
        DESCRIPTOR.contains(DROPPED_STACK),
        "the capabilities name the loss code the importer reports"
    );
    assert!(
        include_str!("effects_budget.rs").contains(&format!("fn {cited}()")),
        "the cited test does not exist"
    );
}

// ---- PLR-089: the dropped-instances losses are exact; PLR-086: the descriptor reader's list cap ----------------------

/// One instance of a hand-built stack.
#[derive(Clone, Copy)]
enum Inst {
    /// Enabled; maps cleanly.
    Live,
    /// Enabled; maps, and adds one `effect_contour_unmapped` note (an optional contour the importer cannot read).
    LiveNoisy,
    /// Switched off; maps cleanly.
    Off,
    /// Switched off; maps, and adds one `effect_contour_unmapped` note.
    OffNoisy,
    /// Switched off and not mappable (a plain number where a percent is required): the importer reports it itself
    /// (`effect_unit_unexpected`). It is not an instance the record dropped.
    OffBroken,
}
use Inst::{Live, LiveNoisy, Off, OffBroken, OffNoisy};
const CONTOUR_NOTE: &str = "effect_contour_unmapped";
const BROKEN_NOTE: &str = "effect_unit_unexpected";
const DESCRIPTOR_LIMIT: &str = "descriptor_item_limit";

fn inst_effect(instance: Inst) -> Vec<u8> {
    let enabled = matches!(instance, Live | LiveNoisy);
    let mut items = vec![("Md  ", v_enum("BlnM", "Mltp")), ("enab", v_bool(enabled))];
    match instance {
        LiveNoisy | OffNoisy => items.push(("TrnS", v_objc("ShpC", Vec::new()))),
        OffBroken => items.push(("Opct", tagged(b"doub", 50f64.to_be_bytes().to_vec()))),
        Live | Off => {}
    }
    v_objc("null", items)
}
/// An `lfx2` block of these instances in stack order, spread over three multi lists (no list passes the descriptor
/// reader's 512 entries).
fn stack_of(instances: &[Inst]) -> Vec<u8> {
    let keys = ["dropShadowMulti", "innerShadowMulti", "solidFillMulti"];
    let per_list = instances.len().div_ceil(keys.len()).max(1);
    let items = keys
        .iter()
        .zip(instances.chunks(per_list))
        .map(|(key, chunk)| {
            (
                *key,
                v_list(chunk.iter().map(|i| inst_effect(*i)).collect()),
            )
        })
        .collect();
    lfx2_block(items)
}
/// An `lfx2` block of these instances in ONE multi list.
fn one_list_of(instances: &[Inst]) -> Vec<u8> {
    lfx2_block(vec![(
        "dropShadowMulti",
        v_list(instances.iter().map(|i| inst_effect(*i)).collect()),
    )])
}
fn repeat(instance: Inst, n: usize) -> Vec<Inst> {
    vec![instance; n]
}
/// A document of stack layers named `L0`, `L1`, ... in FILE order (bottom to top: the import order is the reverse);
/// the layers `hidden` accepts are hidden.
fn doc_of_stacks(stacks: &[Vec<u8>], hidden: impl Fn(usize) -> bool) -> Vec<u8> {
    let layers: Vec<TLayer> = stacks
        .iter()
        .enumerate()
        .map(|(n, lfx2)| effects_layer(leaked(name_of(n)), hidden(n), lfx2.clone()))
        .collect();
    doc_of(&layers)
}
/// Loss entries with `code` on the effect stack path of the layer at `psd_index` (the notes of its instances).
fn stack_notes(p: &ImportProposal, psd_index: usize, code: &str) -> usize {
    let path = format!("layer[{psd_index}]/tagged/lfx2");
    p.loss
        .entries
        .iter()
        .filter(|e| e.code == code && e.path == path)
        .count()
}

/// PLR-089 (R21 pre-review S2, S3), Folio's ceiling: the `Transformed` loss `effects_unrendered_dropped_over_stack_ceiling`
/// is one entry per stack and names how many MAPPED switched-off instances the record does not hold (`dropped=<count>`),
/// whatever that count is (1024, 1500: the output stays one entry per stack); and a switched-off instance that was
/// mapped and left out of the
/// record, whether it was dropped as it was scanned or removed with its stack's other switched-off instances, adds no note
/// (`effect_contour_unmapped` here). An instance that cannot be mapped is not a dropped instance: it keeps the note of
/// its own and is not in the count, so kept + dropped + unmapped is every instance of the file. Kept instances keep their
/// notes (1 live + 1023 switched-off instances is a whole stack). The reported shape is the one of PLR-063.
#[test]
fn the_ceiling_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them() {
    bounded_case(
        "the_ceiling_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them",
        || {
            let stacks: Vec<Vec<Inst>> = vec![
                // 0: 1 live + 1024 switched off: the reported shape, 1025 in all
                [vec![LiveNoisy], repeat(OffNoisy, MAX_EFFECTS)].concat(),
                // 1: far past, no note anywhere
                [vec![Live], repeat(OffNoisy, 1500)].concat(),
                // 2: switched-off instances that map cleanly
                [vec![LiveNoisy], repeat(Off, 1100)].concat(),
                // 3: live instances between and after the switched-off ones, three that cannot be mapped
                [
                    vec![LiveNoisy],
                    repeat(OffNoisy, 1030),
                    repeat(OffBroken, 3),
                    vec![LiveNoisy],
                    repeat(OffNoisy, 10),
                    vec![Live],
                ]
                .concat(),
                // 4: 1 live + 1023 switched off: exactly the ceiling, whole, every instance keeps its note
                [vec![LiveNoisy], repeat(OffNoisy, MAX_EFFECTS - 1)].concat(),
                // 5: switched-off instances only, past the ceiling: not mapped (outcome unchanged, PLR-090)
                repeat(OffNoisy, MAX_EFFECTS + 1),
                // 6: the same switched-off instances with one live instance after them: mapped (PLR-090: the scan
                // cannot stop at the ceiling, a live instance may follow)
                [repeat(OffNoisy, MAX_EFFECTS + 1), vec![LiveNoisy]].concat(),
            ];
            let blocks: Vec<Vec<u8>> = stacks.iter().map(|s| stack_of(s)).collect();
            let bytes = doc_of_stacks(&blocks, |_| false);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            assert_fates(
                &p,
                &[
                    (0, Fate::CountedOnlyStack(1)),
                    (1, Fate::CountedOnlyStack(1)),
                    (2, Fate::CountedOnlyStack(1)),
                    (3, Fate::CountedOnlyStack(3)),
                    (4, Fate::Kept(MAX_EFFECTS)),
                    (5, Fate::Lost(OVER_LAYER)),
                    (6, Fate::CountedOnlyStack(1)),
                ],
            );
            // the count is exactly the MAPPED switched-off instances the record does not hold
            let dropped = [
                (0, Some(1024)),
                (1, Some(1500)),
                (2, Some(1100)),
                (3, Some(1040)),
                (4, None),
                (5, None),
                (6, Some(1025)),
            ];
            for (n, want) in dropped {
                assert_eq!(dropped_count(&p, &name_of(n)), want, "layer {}", name_of(n));
            }
            // kept + dropped + unmapped = every instance of the file (layer 3: 1046 in all)
            let unmapped = 3;
            assert_eq!(
                kept_ordinals(&p, &name_of(3)).len()
                    + dropped_count(&p, &name_of(3)).unwrap()
                    + unmapped,
                stacks[3].len()
            );
            // one entry per stack, not one per instance: five stacks, 5689 instances dropped
            assert_eq!(
                p.loss
                    .entries
                    .iter()
                    .filter(|e| e.code == DROPPED_STACK)
                    .count(),
                5
            );
            // notes: only the instances the record holds report anything
            for (index, notes) in [
                (0, 1),
                (1, 0),
                (2, 1),
                (3, 2),
                (4, MAX_EFFECTS),
                (5, 0),
                (6, 1),
            ] {
                assert_eq!(
                    stack_notes(&p, index, CONTOUR_NOTE),
                    notes,
                    "contour notes of layer {index}"
                );
            }
            assert_eq!(
                stack_notes(&p, 3, BROKEN_NOTE),
                3,
                "an unmapped instance reports itself"
            );
            assert_eq!(kept_total(&p), 1 + 1 + 1 + 3 + MAX_EFFECTS + 1);
            assert_eq!(minted, 6, "a refused stack consumes no SEFX id");
            assert_eq!(
                write_psd(&doc, &Limits::default()).unwrap(),
                bytes,
                "nothing is dropped from the file: the dropped instances are still in the lfx2 blocks"
            );
            assert_record_need_is_exact(&p);
        },
    );
}

/// PLR-089, the unrendered bound (the same class, the other cut): a stack with counted instances whose switched-off
/// instances do not fit the bound on the instances no plan counts keeps its counted instances, and the
/// `Transformed` loss `effects_unrendered_dropped_over_document_budget` names how many MAPPED switched-off instances the
/// record does not hold (`dropped=<count>`) and nothing else reports the dropped ones (no note). A stack that fits keeps
/// its notes. Import order
/// is the reverse of the file order: the control stack (4 instances) first, 64 hidden stacks that fill the bound
/// exactly (3 + 63 x 64 + 61 = 4096), then the two stacks that are cut.
#[test]
fn the_document_budget_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them()
{
    bounded_case(
        "the_document_budget_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them",
        || {
            let cut_last = [vec![LiveNoisy], repeat(OffNoisy, 7), repeat(Off, 3)].concat();
            let cut_first = [repeat(OffNoisy, 2), vec![Live], repeat(OffBroken, 1)].concat();
            let control = [vec![LiveNoisy], repeat(OffNoisy, 3)].concat();
            // file order: L0 cut_last, L1 cut_first, L2 (61 instances) and L3 .. L65 (64 each) hidden, L66 control
            let mut blocks = vec![
                stack_of(&cut_last),
                stack_of(&cut_first),
                stack_of(&repeat(Live, 61)),
            ];
            blocks.extend((0..63).map(|_| stack_of(&repeat(Live, MAX_EFFECTS_PER_LAYER))));
            blocks.push(stack_of(&control));
            let bytes = doc_of_stacks(&blocks, |n| (2..=65).contains(&n));
            let (doc, p, _) = import_counting(&bytes).expect("imports");
            let mut want = vec![
                (0, Fate::CountedOnly(1)),
                (1, Fate::CountedOnly(1)),
                (2, Fate::Kept(61)),
            ];
            want.extend((3..=65).map(|n| (n, Fate::Kept(MAX_EFFECTS_PER_LAYER))));
            want.push((66, Fate::Kept(4)));
            assert_fates(&p, &want);
            assert_eq!(dropped_count(&p, &name_of(0)), Some(10));
            assert_eq!(dropped_count(&p, &name_of(1)), Some(2));
            assert_eq!(dropped_count(&p, &name_of(66)), None);
            assert_eq!(
                p.loss
                    .entries
                    .iter()
                    .filter(|e| e.code == DROPPED_UNRENDERED)
                    .count(),
                2,
                "one entry per cut stack"
            );
            // notes: the cut stacks keep the note of their live instance only, the stack that fit keeps all of its own
            assert_eq!(stack_notes(&p, 0, CONTOUR_NOTE), 1);
            assert_eq!(stack_notes(&p, 1, CONTOUR_NOTE), 0);
            assert_eq!(stack_notes(&p, 66, CONTOUR_NOTE), 4);
            assert_eq!(
                stack_notes(&p, 1, BROKEN_NOTE),
                1,
                "an unmapped instance reports itself"
            );
            assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);
            assert_record_need_is_exact(&p);
        },
    );
}

/// PLR-086 (lead decision DOC): the descriptor reader's cap of 512 entries per list is a parse bound checked before any
/// entry is read, so it cannot know a category: a list over 512 entries refuses the whole descriptor
/// (`descriptor_item_limit`, kind `Unsupported`, the layer named without a stack id) and that layer's stack is lost,
/// whether its entries are live or switched off, while the same entries spread over three lists (or 512 in one list) are
/// kept. The `lfx2` block stays byte-exact in the file.
#[test]
fn a_list_over_the_descriptor_readers_512_entry_cap_loses_the_stack_whatever_the_categories_of_its_entries()
 {
    bounded_case(
        "a_list_over_the_descriptor_readers_512_entry_cap_loses_the_stack_whatever_the_categories_of_its_entries",
        || {
            let blocks = vec![
                // 0: 512 entries in one list: kept
                one_list_of(&[vec![Live], repeat(Off, 511)].concat()),
                // 1: 513 entries: lost, 1 live + 512 switched off
                one_list_of(&[vec![Live], repeat(Off, 512)].concat()),
                // 2: 513 switched-off entries
                one_list_of(&repeat(Off, 513)),
                // 3: 513 live entries: the parse bound comes first, not the live bound
                one_list_of(&repeat(Live, 513)),
                // 4: the 513 entries of layer 1 spread over three lists: kept
                stack_of(&[vec![Live], repeat(Off, 512)].concat()),
            ];
            let bytes = doc_of_stacks(&blocks, |_| false);
            let (doc, p, minted) = import_counting(&bytes).expect("imports");
            assert_fates(&p, &[(0, Fate::Kept(512)), (4, Fate::Kept(513))]);
            for lost in 1..=3 {
                let id = layer_id_by_name(&p, &name_of(lost));
                assert!(
                    p.raster_records.layer(&id).is_none(),
                    "layer {lost} keeps no stack"
                );
                let entries: Vec<&LossEntry> = p
                    .loss
                    .entries
                    .iter()
                    .filter(|e| e.path.starts_with(&format!("layer[{lost}]/tagged/lfx2")))
                    .filter(|e| e.code != "layer_effects_not_rendered")
                    .collect();
                let [entry] = entries.as_slice() else {
                    panic!("layer {lost}: one loss entry for the stack expected, got {entries:?}");
                };
                assert_eq!(
                    (entry.kind, entry.code, entry.path.as_str()),
                    (
                        LossKind::Unsupported,
                        DESCRIPTOR_LIMIT,
                        format!("layer[{lost}]/tagged/lfx2").as_str()
                    ),
                    "layer {lost}"
                );
            }
            assert_eq!(minted, 2, "a refused stack consumes no SEFX id");
            assert_eq!(write_psd(&doc, &Limits::default()).unwrap(), bytes);
        },
    );
}

/// PLR-089 and PLR-086 (V4-07): the shipped DESCRIPTOR (`effect_limits`) states that the two dropped-instances losses are
/// one entry per stack naming the count of the MAPPED switched-off instances, that an instance left out of the record
/// adds no loss entry of its own, and
/// that the descriptor reader's 512-entry list cap is a category-blind parse bound; it cites the tests that pin each, and
/// the tests it cites exist in this file.
#[test]
fn descriptor_states_the_loss_detail_and_the_list_cap_rules() {
    let effect_limits = DESCRIPTOR
        .split("\"effect_limits\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the DESCRIPTOR has an effect_limits entry");
    let cited = [
        "the_ceiling_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them",
        "the_document_budget_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them",
        "a_list_over_the_descriptor_readers_512_entry_cap_loses_the_stack_whatever_the_categories_of_its_entries",
    ];
    for needle in [
        "are_one_entry_per_stack_with_the_path_ending_layer_id_dropped_naming_the_count_of_the_mapped_switched_off_instances_the_record_does_not_hold",
        "a_switched_off_instance_that_was_mapped_and_left_out_of_the_record_adds_no_loss_entry_of_its_own",
        "the_descriptor_readers_cap_of_512_entries_per_list_is_a_category_blind_parse_bound",
        "a_list_over_512_entries_refuses_the_descriptor_loss_unsupported_descriptor_item_limit_and_that_stack_is_lost",
        cited[0],
        cited[1],
        cited[2],
    ] {
        assert!(
            effect_limits.contains(needle),
            "effect_limits does not state {needle}"
        );
    }
    for test in cited {
        assert!(
            include_str!("effects_budget.rs").contains(&format!("fn {test}()")),
            "the cited test {test} does not exist"
        );
    }
}

// ---- PLR-095: the descriptor reader's 8192-item cap, measured for every mapped effect kind ----------------------------

/// The descriptor reader's two parse bounds (`descriptor.rs`, both crate-private): items in the whole descriptor and
/// entries in one list. The first is MEASURED through the importer below and asserted against this number.
const ITEM_CAP: usize = 8192;
const LIST_CAP: usize = 512;

fn v_double(value: f64) -> Vec<u8> {
    tagged(b"doub", value.to_be_bytes().to_vec())
}
fn v_unit(unit: &[u8; 4], value: f64) -> Vec<u8> {
    let mut payload = unit.to_vec();
    payload.extend(value.to_be_bytes());
    tagged(b"UntF", payload)
}
fn v_text(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut payload = be32(units.len() as u32);
    for unit in units {
        payload.extend(unit.to_be_bytes());
    }
    tagged(b"TEXT", payload)
}
/// An `RGBC` colour object: three items.
fn v_colour() -> Vec<u8> {
    v_objc(
        "RGBC",
        vec![
            ("Rd  ", v_double(10.0)),
            ("Grn ", v_double(20.0)),
            ("Bl  ", v_double(30.0)),
        ],
    )
}
/// A `TrnS` contour of `points` points, strictly ascending, which the importer maps.
fn v_contour(points: usize) -> Vec<u8> {
    let curve = (0..points)
        .map(|i| {
            v_objc(
                "CrPt",
                vec![
                    ("Hrzn", v_double(i as f64)),
                    ("Vrtc", v_double(i as f64)),
                    ("Cnty", v_bool(false)),
                ],
            )
        })
        .collect();
    v_objc(
        "ShpC",
        vec![("Nm  ", v_text("Linear")), ("Crv ", v_list(curve))],
    )
}

/// The effect kinds the importer maps (`Slot` of `src/effects.rs` without the gradient and pattern overlays, which it
/// reports and does not map).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    Bevel,
    ColourOverlay,
    Satin,
    Stroke,
}
const KINDS: [Kind; 8] = [
    Kind::DropShadow,
    Kind::InnerShadow,
    Kind::OuterGlow,
    Kind::InnerGlow,
    Kind::Bevel,
    Kind::ColourOverlay,
    Kind::Satin,
    Kind::Stroke,
];

impl Kind {
    /// The word the DESCRIPTOR uses for the kind.
    fn word(self) -> &'static str {
        match self {
            Kind::DropShadow => "drop_shadow",
            Kind::InnerShadow => "inner_shadow",
            Kind::OuterGlow => "outer_glow",
            Kind::InnerGlow => "inner_glow",
            Kind::Bevel => "bevel",
            Kind::ColourOverlay => "colour_overlay",
            Kind::Satin => "satin",
            Kind::Stroke => "stroke",
        }
    }
    /// The single-slot key of the kind and its multi-list key, when the kind has one (`slot_of` in `src/effects.rs`).
    fn keys(self) -> (&'static str, Option<&'static str>) {
        match self {
            Kind::DropShadow => ("DrSh", Some("dropShadowMulti")),
            Kind::InnerShadow => ("IrSh", Some("innerShadowMulti")),
            Kind::OuterGlow => ("OrGl", None),
            Kind::InnerGlow => ("IrGl", None),
            Kind::Bevel => ("ebbl", None),
            Kind::ColourOverlay => ("SoFi", Some("solidFillMulti")),
            Kind::Satin => ("ChFX", None),
            Kind::Stroke => ("FrFX", Some("frameFXMulti")),
        }
    }
    /// The items besides `Md  ` that `map_effect` requires for the kind (an absent one leaves the instance unmapped).
    fn required(self) -> Vec<(&'static str, Vec<u8>)> {
        let blend = || v_enum("BlnM", "Mltp");
        match self {
            Kind::OuterGlow => vec![("GlwT", v_enum("GlwT", "SfBL"))],
            Kind::InnerGlow => vec![
                ("GlwT", v_enum("GlwT", "SfBL")),
                ("glwS", v_enum("glwS", "SrcE")),
            ],
            Kind::Stroke => vec![
                ("PntT", v_enum("PntT", "SClr")),
                ("Styl", v_enum("Styl", "InsF")),
            ],
            Kind::Bevel => vec![
                ("bvlS", v_enum("bvlS", "InrB")),
                ("bvlT", v_enum("bvlT", "SfBL")),
                ("bvlD", v_enum("bvlD", "In  ")),
                ("hglM", blend()),
                ("hglC", v_colour()),
                ("sdwM", blend()),
                ("sdwC", v_colour()),
            ],
            Kind::DropShadow | Kind::InnerShadow | Kind::ColourOverlay | Kind::Satin => Vec::new(),
        }
    }
    /// The other kind-specific items `map_effect` reads (it defaults them when absent).
    fn optional(self) -> Vec<(&'static str, Vec<u8>)> {
        let (prc, pxl) = (|v| v_unit(b"#Prc", v), |v| v_unit(b"#Pxl", v));
        match self {
            Kind::DropShadow => vec![
                ("Dstn", pxl(4.0)),
                ("Ckmt", prc(10.0)),
                ("blur", pxl(5.0)),
                ("layerConceals", v_bool(true)),
            ],
            Kind::InnerShadow => vec![("Dstn", pxl(4.0)), ("Ckmt", prc(10.0)), ("blur", pxl(5.0))],
            Kind::OuterGlow | Kind::InnerGlow => vec![
                ("Ckmt", prc(10.0)),
                ("blur", pxl(5.0)),
                ("Inpr", prc(50.0)),
                ("ShdN", prc(0.0)),
            ],
            Kind::Satin => vec![
                ("Dstn", pxl(4.0)),
                ("blur", pxl(5.0)),
                ("Invr", v_bool(false)),
            ],
            Kind::ColourOverlay => Vec::new(),
            Kind::Stroke => vec![("Sz  ", pxl(3.0)), ("overprint", v_bool(false))],
            Kind::Bevel => vec![
                ("useTexture", v_bool(false)),
                ("Lald", v_unit(b"#Ang", 30.0)),
                ("srgR", prc(100.0)),
                ("blur", pxl(5.0)),
                ("Sftn", pxl(1.0)),
                ("hglO", prc(75.0)),
                ("sdwO", prc(75.0)),
                ("antialiasGloss", v_bool(false)),
                ("useShape", v_bool(false)),
            ],
        }
    }
}

/// How much of an effect instance a descriptor spells out.
#[derive(Clone, Copy)]
enum Shape {
    /// `Md  `, the items the importer requires for the kind and `enab`: the smallest instance that maps and is
    /// switched off (the kind of instance the Folio ceiling cuts).
    Required,
    /// Every item `map_effect` reads for the kind, with a contour of this many points.
    AllKeys(usize),
}

/// One effect instance object. `padding` adds that many unread `bool` items (an unknown key is just an item).
fn effect_object(kind: Kind, shape: Shape, enabled: bool, padding: usize) -> Vec<u8> {
    let mut items = vec![("Md  ", v_enum("BlnM", "Mltp"))];
    items.extend(kind.required());
    items.push(("enab", v_bool(enabled)));
    if let Shape::AllKeys(points) = shape {
        items.extend([
            ("showInDialog", v_bool(true)),
            ("Opct", v_unit(b"#Prc", 80.0)),
            ("Clr ", v_colour()),
            ("AntA", v_bool(true)),
            ("Nose", v_unit(b"#Prc", 5.0)),
            ("lagl", v_unit(b"#Ang", 30.0)),
            ("uglg", v_bool(true)),
            ("TrnS", v_contour(points)),
        ]);
        items.extend(kind.optional());
    }
    items.extend((0..padding).map(|_| ("Pad ", v_bool(true))));
    v_objc("null", items)
}

/// Root items holding `instances` in `key` lists of at most 512 entries, the key repeated once per list (the reader
/// and the importer accept a repeated key; it costs what distinct list keys cost).
fn lists_of(key: &'static str, instances: Vec<Vec<u8>>) -> Vec<(&'static str, Vec<u8>)> {
    instances
        .chunks(LIST_CAP)
        .map(|chunk| (key, v_list(chunk.to_vec())))
        .collect()
}
/// Root items worth exactly `n` descriptor items and nothing else: lists of `bool` entries (a list of `e` entries
/// costs `e + 1` items).
fn filler(n: usize) -> Vec<(&'static str, Vec<u8>)> {
    let mut left = n;
    let mut items = Vec::new();
    while left > 0 {
        let entries = (left - 1).min(LIST_CAP);
        items.push(("Fill", v_list(vec![v_bool(true); entries])));
        left -= entries + 1;
    }
    items
}

/// Imports a one-layer document whose `lfx2` has these root items. Returns the proposal and the loss entries on the
/// stack's path (kind, code), without the standing `layer_effects_not_rendered`.
fn import_stack(items: Vec<(&str, Vec<u8>)>) -> (ImportProposal, Vec<(LossKind, &'static str)>) {
    let bytes = doc_of_stacks(&[lfx2_block(items)], |_| false);
    let (_, proposal, _) = import_counting(&bytes).expect("imports");
    let losses = proposal
        .loss
        .entries
        .iter()
        .filter(|e| {
            e.path.starts_with("layer[0]/tagged/lfx2") && e.code != "layer_effects_not_rendered"
        })
        .map(|e| (e.kind, e.code))
        .collect();
    (proposal, losses)
}
/// Whether the reader refused the descriptor for a parse bound (`descriptor_item_limit`: a refusal of the whole
/// descriptor; with these fixtures only the 8192-item cap can be the bound, no list has more than 512 entries).
fn item_limit_hit(items: Vec<(&str, Vec<u8>)>) -> bool {
    import_stack(items)
        .1
        .contains(&(LossKind::Unsupported, DESCRIPTOR_LIMIT))
}
/// The largest `k < over` with `fits(k)`, by bisection; `fits` is monotone, true at 0 and false at `over`.
fn largest_fit(over: usize, fits: impl Fn(usize) -> bool) -> usize {
    assert!(fits(0), "nothing at all fits");
    assert!(!fits(over), "the upper bound {over} is not refused");
    let (mut fit, mut over) = (0, over);
    while over - fit > 1 {
        let mid = fit + (over - fit) / 2;
        if fits(mid) {
            fit = mid;
        } else {
            over = mid;
        }
    }
    fit
}
/// The descriptor items the reader charges for `items`, measured from outside: the least `cost` for which `items` plus
/// `cap - cost` filler items still parse (galloping on `cost`, then bisection).
fn item_cost(cap: usize, items: &[(&'static str, Vec<u8>)]) -> usize {
    let fits = |cost: usize| !item_limit_hit([items.to_vec(), filler(cap - cost)].concat());
    if fits(0) {
        return 0;
    }
    let (mut under, mut over) = (0, 1);
    while !fits(over) {
        (under, over) = (over, over * 2);
        assert!(over <= cap, "the items do not fit the cap at all");
    }
    while over - under > 1 {
        let mid = under + (over - under) / 2;
        if fits(mid) {
            over = mid;
        } else {
            under = mid;
        }
    }
    over
}
/// `fits` (monotone) holds at `n` and not at `n + 1`: `n` is where the reader's verdict changes.
fn assert_boundary(what: &str, n: usize, fits: impl Fn(usize) -> bool) {
    assert!(fits(n), "{what}: {n} fits");
    assert!(!fits(n + 1), "{what}: {} does not fit", n + 1);
}
/// A drop shadow that costs `3 + padding` items in a list (the entry, `Md  `, `enab`, the padding).
fn padded_stack(live: usize, off: usize, padding: usize) -> Vec<(&'static str, Vec<u8>)> {
    let instance = |enabled| effect_object(Kind::DropShadow, Shape::Required, enabled, padding);
    let mut instances = vec![instance(true); live];
    instances.extend(vec![instance(false); off]);
    lists_of("dropShadowMulti", instances)
}
/// `from` is the smallest per-instance cost (items, list entry included) at which `live` + `off` padded drop shadows no
/// longer fit the item cap: they fit at `from - 1` (padding `from - 4`) and do not fit at `from`.
fn assert_first_cost_over_cap(what: &str, live: usize, off: usize, from: usize) {
    assert_boundary(what, from - 4, |padding| {
        !item_limit_hit(padded_stack(live, off, padding))
    });
}
/// `fn names(values)`: `drop_shadow_3_inner_shadow_3_...`
fn figures(values: &[(Kind, usize)]) -> String {
    values
        .iter()
        .map(|(kind, n)| format!("{}_{n}", kind.word()))
        .collect::<Vec<_>>()
        .join("_")
}

/// PLR-095 (R22-S2, V4-07): the descriptor reader has a second category-blind parse bound besides the 512 entries per
/// list: 8192 items per descriptor, every object item and every list entry at every depth. This test MEASURES it through
/// the importer (no estimate): the cap itself; the items one instance of every mapped effect kind costs (the smallest
/// instance that maps, and an instance with every key the importer reads); the most instances of a kind one descriptor
/// holds; and the per-instance cost from which 8192 items no longer hold the smallest stack Folio's 1024 ceiling cuts
/// (1 live + 1024 switched off), a full 512-entry list, or the smallest stack over the 64 per-layer limit. It also pins
/// the loss code a stack gets on either side of those costs, and that the shipped DESCRIPTOR (`effect_limits`) states the
/// measured numbers. Nothing in the importer changes: the caps are the reader's and pigment's, unchanged.
#[test]
fn the_descriptor_readers_8192_item_cap_is_measured_per_effect_kind_and_stated_in_the_descriptor() {
    bounded_case(
        "the_descriptor_readers_8192_item_cap_is_measured_per_effect_kind_and_stated_in_the_descriptor",
        || {
            // The cap itself: the most items of bare filler that still parse.
            let cap = largest_fit(2 * ITEM_CAP, |k| !item_limit_hit(filler(k)));
            assert_eq!(cap, ITEM_CAP, "the reader's whole-descriptor item cap");
            assert_eq!(
                item_cost(cap, &[]),
                0,
                "the method charges nothing for nothing"
            );

            // Items per instance (slot key or list entry included), the smallest instance that maps and one with every
            // key the importer reads; each instance is checked to MAP, so the figures are those of mapped effects.
            let max_points = hsk_studio_folio::MAX_CURVE_POINTS;
            let (mut required, mut all_keys) = (Vec::new(), Vec::new());
            for kind in KINDS {
                let slot = kind.keys().0;
                for (shape, name) in [
                    (Shape::Required, "required"),
                    (Shape::AllKeys(2), "all keys"),
                ] {
                    let (proposal, losses) =
                        import_stack(vec![(slot, effect_object(kind, shape, true, 0))]);
                    assert_eq!(
                        effect_count_of(&proposal, 0),
                        Some(1),
                        "{kind:?} {name}: the instance maps"
                    );
                    assert!(
                        !losses
                            .iter()
                            .any(|(kind, _)| *kind == LossKind::Unsupported),
                        "{kind:?} {name}: reports {losses:?}"
                    );
                }
                let cost = |shape| item_cost(cap, &[(slot, effect_object(kind, shape, false, 0))]);
                required.push((kind, cost(Shape::Required)));
                all_keys.push((kind, cost(Shape::AllKeys(2))));
            }
            assert_eq!(
                required.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
                [3, 3, 4, 5, 16, 3, 3, 5],
                "items per instance, required items only: {required:?}"
            );
            assert_eq!(
                all_keys.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
                [28, 27, 29, 30, 46, 24, 27, 28],
                "items per instance, every key read: {all_keys:?}"
            );
            // A contour point is a list entry plus Hrzn, Vrtc, Cnty.
            let point = item_cost(
                cap,
                &[(
                    "DrSh",
                    effect_object(Kind::DropShadow, Shape::AllKeys(3), false, 0),
                )],
            ) - all_keys[0].1;
            assert_eq!(point, 4, "items per contour point");
            let widest = item_cost(
                cap,
                &[(
                    "DrSh",
                    effect_object(Kind::DropShadow, Shape::AllKeys(max_points), false, 0),
                )],
            );
            assert_eq!(
                widest,
                all_keys[0].1 + point * (max_points - 2),
                "a drop shadow with a 256-point contour"
            );

            // The most instances one descriptor holds, for the kinds that have a multi list (lists of 512): the
            // reader's verdict flips exactly there (fits at `n`, refused at `n + 1`).
            let holds = |kind: Kind, shape: Shape, n: usize| {
                let key = kind.keys().1.expect("a kind with a multi list");
                assert_boundary(&format!("{kind:?}"), n, |count| {
                    !item_limit_hit(lists_of(
                        key,
                        vec![effect_object(kind, shape, false, 0); count],
                    ))
                });
            };
            let multi = [
                Kind::DropShadow,
                Kind::InnerShadow,
                Kind::ColourOverlay,
                Kind::Stroke,
            ];
            let holds_required: Vec<(Kind, usize)> =
                multi.into_iter().zip([2728, 2728, 2728, 1637]).collect();
            let holds_all: Vec<(Kind, usize)> =
                multi.into_iter().zip([292, 303, 341, 292]).collect();
            let holds_widest = 7;
            for (kind, n) in &holds_required {
                holds(*kind, Shape::Required, *n);
            }
            for (kind, n) in &holds_all {
                holds(*kind, Shape::AllKeys(2), *n);
            }
            holds(Kind::DropShadow, Shape::AllKeys(max_points), holds_widest);

            // The per-instance cost from which 8192 items no longer hold the stack a cap would otherwise decide: 1 live
            // + 1024 switched off (the smallest stack Folio's ceiling cuts), a full list of 512 entries (the most the
            // 512-entry cap accepts), 65 live (the smallest stack over the 64 per-layer limit).
            let (from_ceiling, from_list, from_layer) = (8, 16, 127);
            assert_first_cost_over_cap("1 live + 1024 off", 1, MAX_EFFECTS, from_ceiling);
            assert_first_cost_over_cap("512 off", 0, LIST_CAP, from_list);
            assert_first_cost_over_cap("65 live", MAX_EFFECTS_PER_LAYER + 1, 0, from_layer);
            // The measured kinds sit on the sides these costs say: a kind that costs less than the cost from which a
            // stack no longer fits holds more instances than that stack has.
            for (kind, cost) in &required {
                if let Some((_, n)) = holds_required.iter().find(|(k, _)| k == kind) {
                    assert_eq!(
                        *n > MAX_EFFECTS,
                        *cost < from_ceiling,
                        "{kind:?} required: the ceiling stack"
                    );
                    assert_eq!(
                        *n >= LIST_CAP,
                        *cost < from_list,
                        "{kind:?} required: the full list"
                    );
                }
            }
            for (kind, cost) in &all_keys {
                if let Some((_, n)) = holds_all.iter().find(|(k, _)| k == kind) {
                    assert_eq!(
                        *n > MAX_EFFECTS,
                        *cost < from_ceiling,
                        "{kind:?} all keys: the ceiling stack"
                    );
                    assert_eq!(
                        *n >= LIST_CAP,
                        *cost < from_list,
                        "{kind:?} all keys: the full list"
                    );
                }
            }

            // The loss code on either side: the ceiling cut, and the per-layer loss, are reached only by a stack that
            // fits 8192 items; a stack that does not is refused whole, before any instance is mapped.
            let codes = |items| {
                import_stack(items)
                    .1
                    .into_iter()
                    .map(|(_, code)| code)
                    .collect::<Vec<_>>()
            };
            let fits_ceiling = codes(padded_stack(1, MAX_EFFECTS, from_ceiling - 1 - 3));
            assert!(
                fits_ceiling.contains(&DROPPED_STACK) && !fits_ceiling.contains(&DESCRIPTOR_LIMIT),
                "{fits_ceiling:?}"
            );
            let over_ceiling = codes(padded_stack(1, MAX_EFFECTS, from_ceiling - 3));
            assert!(
                over_ceiling.contains(&DESCRIPTOR_LIMIT) && !over_ceiling.contains(&DROPPED_STACK),
                "{over_ceiling:?}"
            );
            let fits_layer = codes(padded_stack(
                MAX_EFFECTS_PER_LAYER + 1,
                0,
                from_layer - 1 - 3,
            ));
            assert!(
                fits_layer.contains(&OVER_LAYER) && !fits_layer.contains(&DESCRIPTOR_LIMIT),
                "{fits_layer:?}"
            );
            let over_layer = codes(padded_stack(MAX_EFFECTS_PER_LAYER + 1, 0, from_layer - 3));
            assert!(
                over_layer.contains(&DESCRIPTOR_LIMIT) && !over_layer.contains(&OVER_LAYER),
                "{over_layer:?}"
            );

            // The shipped DESCRIPTOR states the measured numbers (V4-07: the claim cites this test).
            let effect_limits = DESCRIPTOR
                .split("\"effect_limits\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("the DESCRIPTOR has an effect_limits entry");
            let cited = "the_descriptor_readers_8192_item_cap_is_measured_per_effect_kind_and_stated_in_the_descriptor";
            for needle in [
                "the_descriptor_readers_cap_of_8192_items_per_descriptor_counts_every_object_item_and_every_list_entry_at_every_nesting_depth_category_blind_and_checked_as_each_item_is_read".to_owned(),
                "a_descriptor_over_8192_items_refuses_the_descriptor_loss_unsupported_descriptor_item_limit_and_that_stack_is_lost_before_any_instance_is_mapped_so_every_cap_and_loss_code_stated_above_applies_to_a_stack_whose_descriptor_fits_8192_items".to_owned(),
                format!("measured_items_per_effect_instance_with_its_list_entry_or_slot_key_and_enab_and_only_the_items_the_importer_requires_{}", figures(&required)),
                format!("measured_items_per_effect_instance_with_every_key_the_importer_reads_and_a_2_point_contour_{}", figures(&all_keys)),
                format!("a_contour_point_costs_{point}_items_and_a_contour_holds_up_to_{max_points}_points"),
                format!(
                    "most_instances_one_descriptor_holds_in_lists_of_512_with_the_required_items_only_{}_with_every_key_{}_and_with_a_{max_points}_point_contour_drop_shadow_{holds_widest}",
                    figures(&holds_required),
                    figures(&holds_all)
                ),
                format!(
                    "{}_instances_do_not_fit_8192_items_from_{from_ceiling}_items_per_instance_a_full_list_of_512_entries_from_{from_list}_items_per_instance_and_{}_counted_instances_from_{from_layer}_items_per_instance",
                    MAX_EFFECTS + 1,
                    MAX_EFFECTS_PER_LAYER + 1
                ),
                "for_instances_of_at_least_those_costs_the_stack_is_refused_whole_with_descriptor_item_limit_before_it_reaches_folios_ceiling_a_full_512_entry_list_or_the_64_per_layer_limit_so_the_ceiling_cut_loss_and_effects_over_budget_for_the_per_layer_limit_are_not_reported_for_it".to_owned(),
                format!("test_{cited}"),
            ] {
                assert!(effect_limits.contains(&needle), "effect_limits does not state {needle}");
            }
            assert!(
                include_str!("effects_budget.rs").contains(&format!("fn {cited}()")),
                "the cited test does not exist"
            );
        },
    );
}

/// Layers that exactly fill pigment's plan limit at the per-layer limit.
const MAX_EFFECTS_PLAN_LAYERS: usize = MAX_EFFECTS_PER_PLAN / MAX_EFFECTS_PER_LAYER;
