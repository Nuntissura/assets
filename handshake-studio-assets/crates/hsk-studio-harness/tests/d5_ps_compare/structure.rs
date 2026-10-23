//! Layer structure cases: Blend-If (PS-C09), mask density and feather (PS-C10), knockout (PS-C11),
//! clipping with Blend-If (PS-C18..PS-C23) and pass-through groups (PS-C28, PS-C29). Scenes follow
//! the oracle script rigs (`clipRig`, `ptRig`); a Normal-mode Photoshop group is an isolated group.
use super::support::*;
use hsk_studio_pigment::adjust_math::*;
use hsk_studio_pigment::coverage_math::*;
use hsk_studio_pigment::render_plan::*;

const N: StudioBlendMode = StudioBlendMode::Normal;
const W: u32 = 256;
const H: u32 = 64;

fn solid(scene: &mut Scene, rgb: [u8; 3], x: i64, y: i64, w: u32, h: u32) -> u32 {
    let c = rgb.map(|v| f32::from(v) / 255.0);
    scene.node(PlanNode::Solid { rect: rect(x, y, w, h), rgba: [c[0], c[1], c[2], 1.0] })
}
fn fixture_tile(corpus: &Corpus, scene: &mut Scene, name: &str) -> Result<u32, String> {
    let img = corpus.fixture(name)?;
    let e = scene.plan.target.extent;
    let (x, y) = paste_origin(e.width, e.height, img.w, img.h);
    Ok(scene.tile(OwnedTile::from_img(&img), x, y))
}
/// Oracle `blendIf(side, chan, [50, 100], [200, 240])` (split sliders, levels / 255).
fn range(bk: [f32; 2], wh: [f32; 2]) -> SplitRange {
    [bk[0] / 255.0, bk[1] / 255.0, wh[0] / 255.0, wh[1] / 255.0]
}
fn blend_if(this: bool, channel: BlendChannel, bk: [f32; 2], wh: [f32; 2]) -> BlendRangeEntry {
    let mut e = BlendRangeEntry::identity(channel);
    if this {
        e.source = range(bk, wh);
    } else {
        e.destination = range(bk, wh);
    }
    e
}
const STD: ([f32; 2], [f32; 2]) = ([50.0, 100.0], [200.0, 240.0]);

pub fn build(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    match e.case_id.as_str() {
        "PS-C09" => c09(corpus, e),
        "PS-C10" => c10(e),
        "PS-C11" => c11(corpus, e),
        "PS-C18" | "PS-C19" | "PS-C21" | "PS-C22" => clip_ramp_rig(corpus, e),
        "PS-C20" => c20(corpus, e),
        "PS-C23" => c23(corpus, e),
        "PS-C28" | "PS-C29" => pass_through(corpus, e),
        other => Err(format!("case {other} not in this family")),
    }
}

/// PS-C09: RGBCMY bands over the gray ramp with Blend-If split sliders on the bands layer.
fn c09(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let mut scene = Scene::new(e.w, e.h);
    let b = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let t = fixture_tile(corpus, &mut scene, "F_RGBCMY_BANDS_256x48")?;
    let root = scene.node(group(true, vec![layer(b, N, 1.0, 1.0), layer(t, N, 1.0, 1.0)]));
    scene.root(root);
    let v = e.variant.as_str();
    if v != "none" {
        let this = v.starts_with("this_");
        let channel = if v.contains("_red_") { BlendChannel::Red } else { BlendChannel::Gray };
        let (bk, wh) = if v.contains("hard_100_100__200_200") { ([100.0, 100.0], [200.0, 200.0]) } else { STD };
        scene.extend(LayerExtension::new(PlanNodeId(root), 1).with_blend_if(vec![blend_if(this, channel, bk, wh)]));
    }
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-11"] })
}

/// PS-C10: a white layer over black with a mask revealing the left half, at mask density and
/// feather. The oracle shows the script's `userMaskFeather` setting had no effect (the feather
/// variants are byte-identical to feather 0), so those variants carry no feather evidence.
fn c10(e: &Entry) -> Result<Built, String> {
    let density = e.params["density_percent"].as_f64().ok_or("params.density_percent")? as f32 / 100.0;
    let feather = e.params["feather_px"].as_f64().ok_or("params.feather_px")? as f32;
    if feather > 0.0 {
        return Ok(Built::OracleInvalid {
            reason: "oracle output byte-identical to the feather-0 variant of the same density: the script's Action Manager key userMaskFeather was not applied by Photoshop 27.11 (feather unproven, re-script)".into(),
            rows: &["PS-04"],
        });
    }
    let mut scene = Scene::new(e.w, e.h);
    let bg = solid(&mut scene, [0, 0, 0], 0, 0, W, H);
    let fg = solid(&mut scene, [255, 255, 255], 0, 0, W, H);
    let mask = scene.mask(OwnedMask::u8(rect(0, 0, W, H), |x, _| if x < 128 { 255 } else { 0 }), false, density);
    let mut l = layer(fg, N, 1.0, 1.0);
    l.mask = Some(mask);
    let root = scene.node(group(true, vec![layer(bg, N, 1.0, 1.0), l]));
    scene.root(root);
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-04"] })
}

/// PS-C11: ramp / outer group (red, x 0..160) / inner group (yellow x 64..192, fill 50) with the
/// knockout setting on the yellow layer.
fn c11(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let knockout = match e.variant.as_str() {
        "none" => Knockout::None,
        "shallow" | "deep" => {
            return Ok(Built::OracleInvalid {
                reason: "oracle outputs for none, shallow and deep are byte-identical: the script's Action Manager key `knockout` was not applied (knockout unproven, re-script)".into(),
                rows: &["PS-12"],
            });
        }
        v => return Err(format!("knockout variant {v}")),
    };
    let mut scene = Scene::new(e.w, e.h);
    let base = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let red = solid(&mut scene, [255, 0, 0], 0, 0, 160, H);
    let yellow = solid(&mut scene, [255, 255, 0], 64, 0, 128, H);
    let inner = scene.node(group(true, vec![layer(yellow, N, 1.0, 0.5)]));
    let outer = scene.node(group(true, vec![layer(red, N, 1.0, 1.0), layer(inner, N, 1.0, 1.0)]));
    let root = scene.node(group(true, vec![layer(base, N, 1.0, 1.0), layer(outer, N, 1.0, 1.0)]));
    scene.root(root);
    scene.extend(LayerExtension::new(PlanNodeId(inner), 0).with_knockout(knockout));
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-12", "PS-01"] })
}

/// PS-C18/C19/C21/C22: `clipRig`: backdrop ramp, [reversed ramp below the base], base ramp, and a
/// clipped layer (solid 200/160/120 Multiply, the ramp, or an Invert adjustment) with Blend-If on
/// the base or on the clipped layer and the "Blend Clipped Layers as Group" switch.
fn clip_ramp_rig(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let v = e.variant.as_str();
    let mut scene = Scene::new(e.w, e.h);
    let back = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let mut layers = vec![layer(back, N, 1.0, 1.0)];
    let bright = matches!(e.case_id.as_str(), "PS-C19") || v == "under_gray_bright_below";
    if bright {
        let b = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_REV_256x64")?;
        layers.push(layer(b, N, 1.0, 1.0));
    }
    let base = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let base_index = layers.len() as u32;
    layers.push(layer(base, N, 1.0, 1.0));
    let mut clipped = match e.case_id.as_str() {
        "PS-C18" | "PS-C19" => layer(solid(&mut scene, [200, 160, 120], 0, 0, W, H), StudioBlendMode::Multiply, 1.0, 1.0),
        "PS-C21" => layer(fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?, N, 1.0, 1.0),
        _ => layer(scene.placeholder(), N, 1.0, 1.0),
    };
    clipped.mask = Some(MaskInput::clip_to_base());
    layers.push(clipped);
    let root = scene.node(group(true, layers));
    scene.root(root);
    let g = PlanNodeId(root);
    let clipped_index = base_index + 1;
    let mut clipped_ext = LayerExtension::new(g, clipped_index);
    if e.case_id == "PS-C22" {
        clipped_ext = clipped_ext.with_adjustment(AdjustmentOp::Invert);
    }
    match e.case_id.as_str() {
        "PS-C18" | "PS-C19" => {
            if v.starts_with("blendif") {
                let this = e.case_id == "PS-C18";
                scene.extend(LayerExtension::new(g, base_index).with_blend_if(vec![blend_if(this, BlendChannel::Gray, STD.0, STD.1)]));
            }
        }
        "PS-C21" => clipped_ext = clipped_ext.with_blend_if(vec![blend_if(false, BlendChannel::Gray, STD.0, STD.1)]),
        "PS-C22" if v == "clipped_invert_this_gray" => clipped_ext = clipped_ext.with_blend_if(vec![blend_if(true, BlendChannel::Gray, STD.0, STD.1)]),
        _ => {}
    }
    if clipped_ext != LayerExtension::new(g, clipped_index) {
        scene.extend(clipped_ext);
    }
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-06", "PS-11"] })
}

/// Anti-aliased coverage of the oracle's 96-gon circle selection (16 x 16 samples per pixel).
pub fn polygon_circle(w: u32, h: u32, cx: f64, cy: f64, r: f64) -> OwnedMask {
    let pts: Vec<(f64, f64)> = (0..96).map(|i| {
        let a = 2.0 * std::f64::consts::PI * f64::from(i) / 96.0;
        (cx + r * a.cos(), cy + r * a.sin())
    }).collect();
    let inside = |px: f64, py: f64| {
        let mut c = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    OwnedMask::u8(rect(0, 0, w, h), |x, y| {
        let mut n = 0u32;
        for sy in 0..16 {
            for sx in 0..16 {
                if inside(f64::from(x) + (f64::from(sx) + 0.5) / 16.0, f64::from(y) + (f64::from(sy) + 0.5) / 16.0) {
                    n += 1;
                }
            }
        }
        ((f64::from(n) * 255.0 / 256.0).round()) as u8
    })
}

/// PS-C20: a white circle (r 30 at 128,32) as clip base, the ramp clipped to it.
fn c20(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let mut scene = Scene::new(e.w, e.h);
    let back = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let white = solid(&mut scene, [255, 255, 255], 0, 0, W, H);
    let circle = scene.mask(polygon_circle(W, H, 128.0, 32.0, 30.0), false, 1.0);
    let mut base = layer(white, N, 1.0, 1.0);
    base.mask = Some(circle);
    let ramp = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let mut clipped = layer(ramp, N, 1.0, 1.0);
    clipped.mask = Some(MaskInput::clip_to_base());
    let root = scene.node(group(true, vec![layer(back, N, 1.0, 1.0), base, clipped]));
    scene.root(root);
    if e.variant == "clipped_this_gray" {
        scene.extend(LayerExtension::new(PlanNodeId(root), 2).with_blend_if(vec![blend_if(true, BlendChannel::Gray, STD.0, STD.1)]));
    }
    Ok(Built::Render { scene, store: Store::Premultiplied, rows: &["PS-06", "PS-11"] })
}

/// PS-C23: backdrop ramp, Normal group holding the reversed ramp with Underlying Blend-If, and
/// (variant 2) a solid clipped to the group.
fn c23(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let mut scene = Scene::new(e.w, e.h);
    let back = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let rev = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_REV_256x64")?;
    let g = scene.node(group(true, vec![layer(rev, N, 1.0, 1.0)]));
    let mut layers = vec![layer(back, N, 1.0, 1.0), layer(g, N, 1.0, 1.0)];
    if e.variant == "clipped_layer_to_group" {
        let s = solid(&mut scene, [200, 160, 120], 0, 0, W, H);
        let mut c = layer(s, N, 1.0, 1.0);
        c.mask = Some(MaskInput::clip_to_base());
        layers.push(c);
    }
    let root = scene.node(group(true, layers));
    scene.root(root);
    scene.extend(LayerExtension::new(PlanNodeId(g), 0).with_blend_if(vec![blend_if(false, BlendChannel::Gray, STD.0, STD.1)]));
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-11", "PS-06", "PS-01"] })
}

/// PS-C28/C29 `ptRig`: backdrop ramp; Pass Through group with a Multiply solid (200/120/40) and a
/// Levels adjustment (30, 220, 1.4) above it; group mask (left half hidden), opacity, density, fill.
fn pass_through(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let p = &e.params;
    let masked = p["mask"].as_bool().unwrap_or(false);
    let opacity = p["group_opacity"].as_f64().unwrap_or(100.0) as f32 / 100.0;
    let density = p["mask_density"].as_f64().unwrap_or(100.0) as f32 / 100.0;
    let fill = p["group_fill"].as_f64().unwrap_or(100.0) as f32 / 100.0;
    let mut scene = Scene::new(e.w, e.h);
    let back = fixture_tile(corpus, &mut scene, "F_GRAY_RAMP_256x64")?;
    let px = solid(&mut scene, [200, 120, 40], 0, 0, W, H);
    let ph = scene.placeholder();
    let pt = scene.node(group(false, vec![layer(px, StudioBlendMode::Multiply, 1.0, 1.0), layer(ph, N, 1.0, 1.0)]));
    let mut gl = layer(pt, StudioBlendMode::PassThrough, opacity, fill);
    if masked {
        gl.mask = Some(scene.mask(OwnedMask::u8(rect(0, 0, W, H), |x, _| if x < 128 { 0 } else { 255 }), false, density));
    }
    let root = scene.node(group(true, vec![layer(back, N, 1.0, 1.0), gl]));
    scene.root(root);
    let lv = LevelsRecord { input_black: 30.0 / 255.0, input_white: 220.0 / 255.0, gamma: 1.4, output_black: 0.0, output_white: 1.0 };
    let op = AdjustmentOp::Levels(Levels { composite: lv, red: LevelsRecord::IDENTITY, green: LevelsRecord::IDENTITY, blue: LevelsRecord::IDENTITY });
    scene.extend(LayerExtension::new(PlanNodeId(pt), 1).with_adjustment(op));
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: &["PS-01", "PS-04", "PS-07"] })
}
