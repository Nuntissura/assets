//! Layer effects (oracle cases PS-C12..PS-C17) and effects with Blend-If / groups (PS-C24..PS-C27).
//! Scene (`effDoc`): 128 x 128 transparent document, optional solid backdrop, one white shape layer
//! (square 32..96; circle r 32 at 64,64 drawn from a 96-gon selection; soft = the square with a
//! 4 px selection feather) carrying pigment `EffectOp`s through the envelope.
//!
//! The circle and soft shapes are taken from the oracle's own PS-C17 outputs, whose inner glow
//! (light yellow in Screen mode over white content) leaves the shape unchanged; that makes the
//! shape geometry identical to Photoshop's and isolates the effect maths. For the same reason the
//! PS-C17 inner-glow variants carry no inner-glow evidence (a match there only proves Screen over
//! white is white); they are reported with no matrix row.
use super::support::*;
use hsk_studio_pigment::coverage_math::*;
use hsk_studio_pigment::effect_math::*;
use hsk_studio_pigment::render_plan::*;

const N: StudioBlendMode = StudioBlendMode::Normal;
const ROWS: &[&str] = &["PS-09"];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const GLOW: [f32; 3] = [1.0, 1.0, 190.0 / 255.0];

/// Numeric tokens of a variant name: `s10`, `sp50`, `ck100`, `a-60`, `d2.5`, `fill50`, `range25`.
struct Tok<'a>(Vec<&'a str>);
impl<'a> Tok<'a> {
    fn new(v: &'a str) -> Self {
        Self(v.split('_').collect())
    }
    fn get(&self, prefix: &str) -> Option<f32> {
        self.0.iter().find_map(|t| {
            let rest = t.strip_prefix(prefix)?;
            if rest.is_empty() || !(rest.starts_with('-') || rest.starts_with(|c: char| c.is_ascii_digit())) {
                return None;
            }
            rest.parse().ok()
        })
    }
    /// `s<n>` (size) only: the `sp`/`softer`/`square` tokens never parse as a number after `s`.
    fn size(&self) -> Option<f32> {
        self.get("s")
    }
    fn has(&self, t: &str) -> bool {
        self.0.contains(&t)
    }
}

fn shape_tile(corpus: &Corpus, shape: &str) -> Result<OwnedTile, String> {
    match shape {
        "square" => Ok(OwnedTile::new(128, 128, |x, y| if (32..96).contains(&x) && (32..96).contains(&y) { [1.0; 4] } else { [0.0; 4] })),
        "thin" => Ok(OwnedTile::new(128, 128, |x, y| if (54..74).contains(&x) && (32..96).contains(&y) { [1.0; 4] } else { [0.0; 4] })),
        "circle" | "soft" => {
            let file = if shape == "circle" { "PS-C17__circle_edge_s2_ck0_softer.tif" } else { "PS-C17__soft_edge_s10_ck0.tif" };
            let img = corpus.output(file)?;
            Ok(OwnedTile::new(128, 128, |x, y| {
                let a = img.at(x, y, 3) as f32;
                if a > 0.0 { [1.0, 1.0, 1.0, a] } else { [0.0; 4] }
            }))
        }
        other => Err(format!("shape {other}")),
    }
}

struct Doc {
    scene: Scene,
    root: u32,
    shape_index: u32,
}
/// `effDoc(shape, backdrop)`: [backdrop solid], shape layer with `fill`.
fn eff_doc(corpus: &Corpus, shape: &str, backdrop: Option<[u8; 3]>, fill: f32, below: Option<OwnedTile>) -> Result<Doc, String> {
    let mut scene = Scene::new(128, 128);
    let mut layers = Vec::new();
    if let Some(c) = backdrop {
        let c = c.map(|v| f32::from(v) / 255.0);
        let b = scene.node(PlanNode::Solid { rect: rect(0, 0, 128, 128), rgba: [c[0], c[1], c[2], 1.0] });
        layers.push(layer(b, N, 1.0, 1.0));
    }
    if let Some(t) = below {
        // Measured on the oracle (PS-C14 multiply_shadow_over_gradient): the 256 x 64 ramp pasted
        // into the 128 x 128 document sits at x 0 (left-aligned, not centred) and y 32 (centred).
        let (_, y) = paste_origin(128, 128, t.width, t.height);
        let b = scene.tile(t, 0, y);
        layers.push(layer(b, N, 1.0, 1.0));
    }
    let s = scene.tile(shape_tile(corpus, shape)?, 0, 0);
    let shape_index = layers.len() as u32;
    layers.push(layer(s, N, 1.0, fill));
    let root = scene.node(group(true, layers));
    scene.root(root);
    Ok(Doc { scene, root, shape_index })
}
fn finish(mut d: Doc, effects: Vec<EffectOp>, alpha: bool, rows: &'static [&'static str]) -> Built {
    d.scene.extend(LayerExtension::new(PlanNodeId(d.root), d.shape_index).with_effects(effects));
    Built::Render { scene: d.scene, store: if alpha { Store::Premultiplied } else { Store::FlattenWhite }, rows }
}
fn shadow(size: f32, spread: f32, angle: f32, distance: f32) -> DropShadowOp {
    DropShadowOp::new(BLACK, N, 1.0).with_offset(angle, distance).with_spread(spread).with_size(size).with_layer_knocks_out(true)
}
fn inner_shadow(size: f32, choke: f32, angle: f32, distance: f32) -> InnerShadowOp {
    InnerShadowOp::new(BLACK, N, 1.0).with_offset(angle, distance).with_choke(choke).with_size(size)
}
fn shape_of(v: &str) -> &str {
    v.split('_').next().unwrap_or("square")
}

pub fn build(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let v = e.variant.as_str();
    let t = Tok::new(v);
    match e.case_id.as_str() {
        "PS-C12" => {
            if let Some(rest) = v.strip_prefix("soft_fill") {
                let fill: f32 = rest.split('_').next().unwrap_or("100").parse().map_err(|_| "fill")?;
                let d = eff_doc(corpus, "soft", None, fill / 100.0, None)?;
                let op = shadow(10.0, 0.0, 120.0, 8.0).with_layer_knocks_out(v.ends_with("_knocks"));
                return Ok(finish(d, vec![EffectOp::DropShadow(op)], true, ROWS));
            }
            let (size, spread) = (t.size().ok_or("size")?, t.get("sp").unwrap_or(0.0));
            let op = shadow(size, spread / 100.0, t.get("a").ok_or("angle")?, t.get("d").ok_or("distance")?);
            Ok(finish(eff_doc(corpus, shape_of(v), None, 1.0, None)?, vec![EffectOp::DropShadow(op)], true, ROWS))
        }
        "PS-C13" => {
            if v == "square_overlay_blue_stroke_red_o5_opacity50" {
                let fx = vec![
                    EffectOp::ColourOverlay(ColourOverlayOp::new(BLUE, N, 1.0)),
                    EffectOp::Stroke(StrokeOp::new(RED, N, 0.5, 5.0, StrokePosition::Outside)),
                ];
                return Ok(finish(eff_doc(corpus, "square", None, 1.0, None)?, fx, true, ROWS));
            }
            let pos = if t.has("outside") {
                StrokePosition::Outside
            } else if t.has("inside") {
                StrokePosition::Inside
            } else {
                StrokePosition::Centre
            };
            let op = StrokeOp::new(RED, N, 1.0, t.size().ok_or("size")?, pos);
            Ok(finish(eff_doc(corpus, shape_of(v), None, 1.0, None)?, vec![EffectOp::Stroke(op)], true, ROWS))
        }
        "PS-C14" => c14(corpus, e),
        "PS-C15" => {
            let fill = if v.ends_with("_fill0") { 0.0 } else { 1.0 };
            let angle = t.get("a").unwrap_or(120.0);
            let op = inner_shadow(t.size().ok_or("size")?, t.get("ck").unwrap_or(0.0) / 100.0, angle, t.get("d").ok_or("distance")?);
            Ok(finish(eff_doc(corpus, shape_of(v), None, fill, None)?, vec![EffectOp::InnerShadow(op)], true, ROWS))
        }
        "PS-C16" => {
            let fill = if v.starts_with("soft_fill0") { 0.0 } else { 1.0 };
            let mut op = OuterGlowOp::new(GLOW, StudioBlendMode::Screen, 1.0)
                .with_size(t.size().ok_or("size")?)
                .with_spread(t.get("sp").unwrap_or(0.0) / 100.0)
                .with_technique(if t.has("precise") { GlowTechnique::Precise } else { GlowTechnique::Softer });
            if let Some(r) = t.get("range") {
                op = op.with_range(r / 100.0);
            }
            if let Some(j) = t.get("jitter") {
                op = op.with_jitter(j / 100.0);
            }
            let shape = if v.starts_with("soft") { "soft" } else { shape_of(v) };
            Ok(finish(eff_doc(corpus, shape, None, fill, None)?, vec![EffectOp::OuterGlow(op)], true, ROWS))
        }
        "PS-C17" => {
            let shape = if v.starts_with("thin") { "thin" } else { shape_of(v) };
            let op = InnerGlowOp::new(GLOW, StudioBlendMode::Screen, 1.0)
                .with_size(t.size().ok_or("size")?)
                .with_choke(t.get("ck").unwrap_or(0.0) / 100.0)
                .with_source(if t.has("center") { GlowSource::Centre } else { GlowSource::Edge })
                .with_technique(if t.has("precise") { GlowTechnique::Precise } else { GlowTechnique::Softer });
            Ok(finish(eff_doc(corpus, shape, None, 1.0, None)?, vec![EffectOp::InnerGlow(op)], true, &[]))
        }
        "PS-C24" | "PS-C25" | "PS-C26" => fx_blend_if(corpus, e),
        "PS-C27" => c27(corpus, e),
        other => Err(format!("case {other} not in this family")),
    }
}

/// PS-C14: drop shadow + red colour overlay + 3 px blue outside stroke over a 200-gray backdrop.
fn c14(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let stack = || {
        vec![
            EffectOp::DropShadow(shadow(6.0, 0.0, 120.0, 8.0)),
            EffectOp::ColourOverlay(ColourOverlayOp::new(RED, N, 1.0)),
            EffectOp::Stroke(StrokeOp::new(BLUE, N, 1.0, 3.0, StrokePosition::Outside)),
        ]
    };
    let rows: &'static [&'static str] = &["PS-09", "PS-01"];
    let gray = Some([200, 200, 200]);
    Ok(match e.variant.as_str() {
        "fill100" => finish(eff_doc(corpus, "square", gray, 1.0, None)?, stack(), false, rows),
        "fill50" | "blendInterior_off_fill50" => finish(eff_doc(corpus, "square", gray, 0.5, None)?, stack(), false, rows),
        "fill0" => finish(eff_doc(corpus, "square", gray, 0.0, None)?, stack(), false, rows),
        "opacity50" => {
            let mut d = eff_doc(corpus, "square", gray, 1.0, None)?;
            if let PlanNode::Group(g) = &mut d.scene.plan.nodes[d.root as usize] {
                g.layers[d.shape_index as usize].opacity = 0.5;
            }
            finish(d, stack(), false, rows)
        }
        "blendInterior_on" => Built::NotSupported { reason: "Blend Interior Effects as Group: no Studio envelope field (effects always composite separately over the content)".into(), rows },
        "mask_hides_effects_on" => Built::NotSupported { reason: "Layer Mask Hides Effects: no Studio envelope field (the effect matte is always the masked shape alpha)".into(), rows: &["PS-09", "PS-04"] },
        "mask_hides_effects_off" => {
            let mut d = eff_doc(corpus, "square", gray, 1.0, None)?;
            let m = d.scene.mask(OwnedMask::u8(rect(0, 0, 128, 128), |x, _| if x < 64 { 255 } else { 0 }), false, 1.0);
            if let PlanNode::Group(g) = &mut d.scene.plan.nodes[d.root as usize] {
                g.layers[d.shape_index as usize].mask = Some(m);
            }
            finish(d, stack(), false, &["PS-09", "PS-04"])
        }
        "multiply_shadow_over_gradient" => {
            let ramp = corpus.fixture("F_GRAY_RAMP_256x64")?;
            let d = eff_doc(corpus, "square", None, 1.0, Some(OwnedTile::from_img(&ramp)))?;
            let mut op = shadow(6.0, 0.0, 120.0, 8.0);
            op.mode = StudioBlendMode::Multiply;
            finish(d, vec![EffectOp::DropShadow(op)], false, rows)
        }
        "two_strokes_instance_order" => {
            // Photoshop's multi-instance list is top-first (measured: the first-listed red 8 px
            // stroke covers the blue 4 px one); Studio's envelope list is bottom-first.
            let fx = vec![
                EffectOp::Stroke(StrokeOp::new(BLUE, N, 1.0, 4.0, StrokePosition::Outside)),
                EffectOp::Stroke(StrokeOp::new(RED, N, 1.0, 8.0, StrokePosition::Outside)),
            ];
            finish(eff_doc(corpus, "square", None, 1.0, None)?, fx, true, ROWS)
        }
        other => return Err(format!("C14 variant {other}")),
    })
}

/// PS-C24..C26 `fxBi`: backdrop columns of gray 2x, white square with a drop shadow (6/0/120/8) or
/// an interior pair (inner shadow 6/0/120/4 + red overlay 50 %), Blend-If gray 50/100 200/240.
fn fx_blend_if(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let p = &e.params;
    if p["transparency_shapes_layer_off"].as_bool() == Some(true) {
        return Ok(Built::NotSupported { reason: "Transparency Shapes Layer off: no Studio envelope field".into(), rows: &["PS-11", "PS-09"] });
    }
    if p["blend_interior"].as_bool() == Some(true) {
        return Ok(Built::NotSupported { reason: "Blend Interior Effects as Group: no Studio envelope field".into(), rows: &["PS-11", "PS-09"] });
    }
    let fill = if p["fill0"].as_bool() == Some(true) { 0.0 } else { 1.0 };
    let mut scene = Scene::new(128, 128);
    let back = scene.tile(OwnedTile::new(128, 128, |x, _| { let g = (x * 2) as f32 / 255.0; [g, g, g, 1.0] }), 0, 0);
    let s = scene.tile(shape_tile(corpus, "square")?, 0, 0);
    let root = scene.node(group(true, vec![layer(back, N, 1.0, 1.0), layer(s, N, 1.0, fill)]));
    scene.root(root);
    let effects = if p["effect"].as_str() == Some("dropshadow") {
        vec![EffectOp::DropShadow(shadow(6.0, 0.0, 120.0, 8.0))]
    } else {
        // Photoshop's fixed style order puts the inner shadow above the colour overlay; Studio
        // renders the envelope list bottom-first, so the overlay is listed first.
        vec![EffectOp::ColourOverlay(ColourOverlayOp::new(RED, N, 0.5)), EffectOp::InnerShadow(inner_shadow(6.0, 0.0, 120.0, 4.0))]
    };
    let mut ext = LayerExtension::new(PlanNodeId(root), 1).with_effects(effects);
    match p["blend_if"].as_str() {
        Some("this") | Some("under") => {
            let mut b = BlendRangeEntry::identity(BlendChannel::Gray);
            let r = [50.0 / 255.0, 100.0 / 255.0, 200.0 / 255.0, 240.0 / 255.0];
            if p["blend_if"].as_str() == Some("this") { b.source = r } else { b.destination = r }
            ext = ext.with_blend_if(vec![b]);
        }
        _ => {}
    }
    scene.extend(ext);
    Ok(Built::Render { scene, store: Store::Premultiplied, rows: &["PS-11", "PS-09"] })
}

/// PS-C27: a Normal group holding the white square; drop shadow on the child (4/0/120/6, black),
/// on the group (10/0/120/14, red) or both.
fn c27(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let group_shadow = e.params["group_shadow"].as_bool().unwrap_or(false);
    let child_shadow = e.params["child_shadow"].as_bool().unwrap_or(false);
    let mut scene = Scene::new(128, 128);
    let s = scene.tile(shape_tile(corpus, "square")?, 0, 0);
    let g = scene.node(group(true, vec![layer(s, N, 1.0, 1.0)]));
    let root = scene.node(group(true, vec![layer(g, N, 1.0, 1.0)]));
    scene.root(root);
    if child_shadow {
        scene.extend(LayerExtension::new(PlanNodeId(g), 0).with_effects(vec![EffectOp::DropShadow(shadow(4.0, 0.0, 120.0, 6.0))]));
    }
    if group_shadow {
        let mut op = shadow(10.0, 0.0, 120.0, 14.0);
        op.colour = RED;
        scene.extend(LayerExtension::new(PlanNodeId(root), 0).with_effects(vec![EffectOp::DropShadow(op)]));
    }
    Ok(Built::Render { scene, store: Store::Premultiplied, rows: &["PS-09", "PS-01"] })
}
