//! SMB-PARSER-HOSTILE-1 item 8: the `interop-fig-native export.rs:384` candidate (`children_of`, `layer`
//! recurse with no guard). The depth is bounded before they run: `to_native`'s walk stops at
//! `depth > 64` with loss `native.depth` (`native.rs` `walk`), depth rises once per level and passes
//! through instance expansion unchanged, so the layer tree a file can reach is at most 65 levels.
//! Pinned here on hand-built documents (the `tests/fig_instances_frames.rs` synthetic shape): a chain
//! of 70 nested frames, and a chain with an instance along it, map with `native.depth` and a tree of at
//! most 65 levels; `to_native` and `export_frame_pdf` on the deepest chain finish typed on a 2 MiB
//! thread. `fig_depth_stack_probe` (ignored, by hand) reports the smallest of 256 KiB, 512 KiB and
//! 1 MiB that passes for each call.
mod common;
use common::Host;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Parent};
use hsk_studio_interop_fig::*;
use std::collections::BTreeMap;

fn sid(session: u32, local: u32) -> SourceId {
    SourceId { session, local }
}

fn node(id: SourceId, parent: Option<SourceId>, ty: &str, payload: NodePayload) -> FigNode {
    FigNode {
        id,
        parent,
        position: None,
        children: Vec::new(),
        type_name: ty.into(),
        name: Some(format!("{ty}-{id}")),
        visible: Some(true),
        opacity: Some(1.0),
        blend_mode: None,
        transform: None,
        size: Some(Vec2 { x: 10.0, y: 10.0 }),
        mask: None,
        fills: Vec::new(),
        strokes: Vec::new(),
        stroke_weight: None,
        stroke_align: None,
        stroke_join: None,
        stroke_cap: None,
        dash_pattern: Vec::new(),
        miter_limit: None,
        effect_style: None,
        fill_style: None,
        stroke_style: None,
        export_settings: Vec::new(),
        effects: Vec::new(),
        fill_geometry: Vec::new(),
        stroke_geometry: Vec::new(),
        interactions: Vec::new(),
        flow_start: None,
        flow_position: None,
        payload,
    }
}

fn frame(id: SourceId, parent: SourceId) -> FigNode {
    node(id, Some(parent), "FRAME", NodePayload::Frame(FramePayload::default()))
}

fn rect(id: SourceId, parent: SourceId) -> FigNode {
    node(id, Some(parent), "RECTANGLE", NodePayload::Rectangle { corner_radius: None, corner_radii: None })
}

/// DOCUMENT 0:0 > CANVAS 0:1 > the given nodes (parents by id); children linked in list order.
fn synthetic(mut nodes: Vec<FigNode>) -> FigDocument {
    let mut all = vec![
        node(sid(0, 0), None, "DOCUMENT", NodePayload::None),
        node(sid(0, 1), Some(sid(0, 0)), "CANVAS", NodePayload::Canvas { background: None, background_enabled: None, internal_only: None }),
    ];
    all.append(&mut nodes);
    let index: BTreeMap<SourceId, usize> = all.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    for i in 0..all.len() {
        if let Some(p) = all[i].parent {
            let pi = index[&p];
            all[pi].children.push(i);
        }
    }
    FigDocument { nodes: all, roots: vec![0], blob_count: 0, loss: Vec::new(), decoded: BTreeMap::new(), source_sha256: None, image_parts: BTreeMap::new() }
}

/// `n` frames nested under the canvas (1:1 outermost), a rectangle in the innermost.
fn chain(n: u32) -> FigDocument {
    let mut nodes = Vec::new();
    let mut parent = sid(0, 1);
    for k in 1..=n {
        nodes.push(frame(sid(1, k), parent));
        parent = sid(1, k);
    }
    nodes.push(rect(sid(1, 10_000), parent));
    synthetic(nodes)
}

/// 30 nested frames whose innermost holds an instance of a symbol that is itself a chain of 40 frames.
fn chain_with_instance() -> FigDocument {
    let canvas = sid(0, 1);
    let mut nodes = vec![node(sid(2, 0), Some(canvas), "SYMBOL", NodePayload::Symbol { frame: FramePayload::default(), variant: Vec::new(), component_key: None })];
    let mut parent = sid(2, 0);
    for k in 1..=40 {
        nodes.push(frame(sid(2, k), parent));
        parent = sid(2, k);
    }
    nodes.push(rect(sid(2, 10_000), parent));
    let mut parent = canvas;
    for k in 1..=30 {
        nodes.push(frame(sid(1, k), parent));
        parent = sid(1, k);
    }
    nodes.push(node(
        sid(3, 1),
        Some(parent),
        "INSTANCE",
        NodePayload::Instance { frame: FramePayload::default(), instance: InstancePayload { symbol_id: Some(sid(2, 0)), overrides: Vec::new(), uniform_scale: Some(1.0), derived: Vec::new() } },
    ));
    synthetic(nodes)
}

fn big() -> Budget {
    Budget { nodes: 1 << 20, edges: 1 << 20, input_bytes: 1 << 28, payload_bytes: 1 << 26, depth: 1024 }
}

fn native(doc: &FigDocument) -> Result<NativeProposal> {
    to_native(doc, &NativeConfig { budget: big(), resolver: &Host }, &mut SequentialIdMint::new(1), &CancellationToken::default())
}

fn depth_losses(p: &NativeProposal) -> u32 {
    p.mapping_loss.iter().filter(|l| (l.path == "native.depth" || l.path == "expansion:native.depth") && l.code == "nesting_too_deep").map(|l| l.count).sum()
}

/// Deepest layer of the proposal, counted along `parent` links between layers (a root layer = 1).
fn tree_depth(p: &NativeProposal) -> usize {
    let layers = &p.snapshot.document().layers;
    let parent: BTreeMap<&str, &str> = layers
        .iter()
        .filter_map(|l| match &l.parent {
            Parent::Node(n) => Some((l.layer_id.as_str(), n.node_id.as_str())),
            Parent::Root(()) => None,
        })
        .collect();
    layers
        .iter()
        .map(|l| {
            let (mut d, mut at) = (1usize, l.layer_id.as_str());
            while let Some(up) = parent.get(at) {
                d += 1;
                at = up;
                assert!(d <= layers.len() + 1, "parent cycle");
            }
            d
        })
        .max()
        .unwrap_or(0)
}

fn on_stack<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(stack).spawn(f).expect("thread").join().expect("thread panicked")
}

/// `to_native` then the PDF export of the outermost frame (1:1).
fn map_and_export(doc: FigDocument) -> std::result::Result<(usize, u32), String> {
    let p = native(&doc).map_err(|e| format!("{e:?}"))?;
    let (plans, _) = plan_frame_exports(&doc, &p, sid(1, 1));
    let plan = plans.first().ok_or("no plan")?;
    let pdf = export_frame_pdf(&p, plan, None, &CancellationToken::default()).map_err(|e| format!("{e:?}"))?;
    Ok((tree_depth(&p), pdf.receipt.pages))
}

#[test]
fn seventy_nested_frames_stop_at_the_walk_depth() {
    let p = native(&chain(70)).expect("maps");
    let depth = tree_depth(&p);
    println!("FIG_DEPTH chain=70 tree_depth={depth} native_depth_losses={}", depth_losses(&p));
    assert!(depth_losses(&p) >= 1, "native.depth reported");
    assert!(depth <= 65, "tree {depth} levels");
}

#[test]
fn an_instance_along_the_chain_does_not_deepen_it() {
    let p = native(&chain_with_instance()).expect("maps");
    let depth = tree_depth(&p);
    let codes: Vec<(&str, &str, u32)> = p.mapping_loss.iter().map(|l| (l.path.as_str(), l.code, l.count)).collect();
    println!("FIG_DEPTH chain=30+instance+40 tree_depth={depth} native_depth_losses={} expansions={} loss={codes:?}", depth_losses(&p), p.expansions.len());
    // 30 frames + the instance + 40 symbol frames would be 71 levels: the walk cuts inside the
    // expansion (loss `expansion:native.depth`) and the tree stays within 65 (measured 64).
    assert!(depth_losses(&p) >= 1, "native.depth reported inside the expansion");
    assert!(depth <= 65, "tree {depth} levels");
}

#[test]
fn the_deepest_chain_maps_and_exports_on_2_mib() {
    for doc in [chain(70), chain_with_instance()] {
        let out = on_stack(2 << 20, move || map_and_export(doc));
        println!("FIG_DEPTH 2MiB answer={out:?}");
        assert!(matches!(out, Ok((d, 1)) if d <= 65), "{out:?}");
    }
}

/// Measurement probe (item 8): the smallest of 256 KiB, 512 KiB and 1 MiB on which `to_native` and
/// `to_native` + `export_frame_pdf` finish on the 70-frame chain. Re-runs this binary's
/// `fig_depth_stack_child` (an overflow ends only the child). By hand:
/// `--run-ignored only -E 'test(fig_depth_stack_probe)'`.
#[test]
#[ignore = "measurement probe, run by hand"]
fn fig_depth_stack_probe() {
    let exe = std::env::current_exe().expect("test binary");
    for call in ["to_native", "export"] {
        let smallest = [256usize << 10, 512 << 10, 1 << 20].into_iter().find(|stack| {
            std::process::Command::new(&exe)
                .args(["--exact", "fig_depth_stack_child", "--ignored", "--nocapture"])
                .env("HOSTILE_PROBE_STACK", stack.to_string())
                .env("HOSTILE_PROBE_CALL", call)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        println!("PROBE fig call={call} smallest_passing_stack={smallest:?}");
    }
}

#[test]
#[ignore = "child of fig_depth_stack_probe"]
fn fig_depth_stack_child() {
    let stack = std::env::var("HOSTILE_PROBE_STACK").ok().and_then(|v| v.parse::<usize>().ok());
    let call = std::env::var("HOSTILE_PROBE_CALL").unwrap_or_default();
    let Some(stack) = stack else {
        println!("NOT_PROVEN: probe child run without HOSTILE_PROBE_STACK");
        return;
    };
    let ok = on_stack(stack, move || if call == "export" { map_and_export(chain(70)).is_ok() } else { native(&chain(70)).is_ok() });
    assert!(ok);
}
