//! PS-C01..PS-C08: adjustment layers (oracle script cases 1-8). Each variant is the fixture as the
//! bottom layer of an isolated root group with one adjustment layer above it (pigment
//! `AdjustmentOp` on the canonical placeholder). The `dom_*` variants apply the same adjustment
//! destructively to the pixel layer (`ArtLayer.adjustLevels/threshold/posterize`); for an opaque
//! layer that is the same per-pixel function, so they use the same scene.
use super::support::*;
use hsk_studio_pigment::adjust_math::*;
use hsk_studio_pigment::render_plan::*;

const ROWS_ADJ: &[&str] = &["PS-07"];

fn num(s: &str, prefix: char) -> Option<f32> {
    s.strip_prefix(prefix)?.parse().ok()
}

/// One adjustment layer over `fixture` in an isolated root group.
fn over_fixture(corpus: &Corpus, e: &Entry, fixture: &str, op: AdjustmentOp, space: BlendSpace, input: Option<Img>) -> Result<Built, String> {
    let img = match input {
        Some(i) => i,
        None => (*corpus.fixture(fixture)?).clone(),
    };
    let mut scene = Scene::new(e.w, e.h);
    scene.plan.target.space.blend_space = space;
    let tile = if e.bits.ends_with("SIXTEEN") { OwnedTile::from_img_15bit(&img) } else { OwnedTile::from_img(&img) };
    let (x, y) = paste_origin(e.w, e.h, img.w, img.h);
    let t = scene.tile(tile, x, y);
    let ph = scene.placeholder();
    let root = scene.node(group(true, vec![layer(t, StudioBlendMode::Normal, 1.0, 1.0), layer(ph, StudioBlendMode::Normal, 1.0, 1.0)]));
    scene.root(root);
    scene.extend(LayerExtension::new(PlanNodeId(root), 1).with_adjustment(op));
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: ROWS_ADJ })
}

fn levels_record(spec: &str) -> Result<LevelsRecord, String> {
    // "g0.5" | "in20_230_g1.5_out10_240" | "in10_200_g0.8"
    let mut r = LevelsRecord::IDENTITY;
    let parts: Vec<&str> = spec.split('_').collect();
    let mut i = 0;
    while i < parts.len() {
        let p = parts[i];
        if let Some(v) = num(p, 'g') {
            r.gamma = v;
        } else if let Some(lo) = p.strip_prefix("in") {
            r.input_black = lo.parse::<f32>().map_err(|e| e.to_string())? / 255.0;
            i += 1;
            r.input_white = parts.get(i).ok_or("levels in")?.parse::<f32>().map_err(|e| e.to_string())? / 255.0;
        } else if let Some(lo) = p.strip_prefix("out") {
            r.output_black = lo.parse::<f32>().map_err(|e| e.to_string())? / 255.0;
            i += 1;
            r.output_white = parts.get(i).ok_or("levels out")?.parse::<f32>().map_err(|e| e.to_string())? / 255.0;
        } else {
            return Err(format!("levels token {p}"));
        }
        i += 1;
    }
    Ok(r)
}
fn levels(spec: &str) -> Result<Levels, String> {
    let mut l = Levels { composite: LevelsRecord::IDENTITY, red: LevelsRecord::IDENTITY, green: LevelsRecord::IDENTITY, blue: LevelsRecord::IDENTITY };
    match spec {
        "comp_and_red" => {
            l.composite = levels_record("in20_230_g1.5_out10_240")?;
            l.red = levels_record("in10_200_g0.8")?;
        }
        s if s.starts_with("comp_") => l.composite = levels_record(&s[5..])?,
        s if s.starts_with("red_") => l.red = levels_record(&s[4..])?,
        s => return Err(format!("levels spec {s}")),
    }
    Ok(l)
}

pub fn build(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let p = &e.params;
    match e.case_id.as_str() {
        "PS-C01" => c01(corpus, e),
        "PS-C02" => {
            let spec = if e.variant.starts_with("dom_") { "comp_in20_230_g1.5_out10_240".to_string() } else { p["levels"].as_str().ok_or("params.levels")?.to_string() };
            over_fixture(corpus, e, "F_GRAY_RAMP_256x64", AdjustmentOp::Levels(levels(&spec)?), BlendSpace::Encoded, None)
        }
        "PS-C03" => {
            let pts: Vec<[f32; 2]> = p["points_input_output"]
                .as_array()
                .ok_or("params.points_input_output")?
                .iter()
                .map(|q| [q[0].as_f64().unwrap_or(0.0) as f32 / 255.0, q[1].as_f64().unwrap_or(0.0) as f32 / 255.0])
                .collect();
            let op = AdjustmentOp::Curves(Curves { composite: pts, red: vec![], green: vec![], blue: vec![] });
            over_fixture(corpus, e, "F_GRAY_RAMP_256x64", op, BlendSpace::Encoded, None)
        }
        "PS-C04" => {
            let f = |k: &str| p[k].as_f64().unwrap_or(0.0) as f32;
            let op = AdjustmentOp::HueSaturation(HueSaturation { hue: f("hue"), saturation: f("saturation"), lightness: f("lightness"), colorize: p["colorize"].as_bool().unwrap_or(false) });
            over_fixture(corpus, e, "F_RGB_LATTICE17_289x17", op, BlendSpace::Encoded, None)
        }
        "PS-C05" => {
            let op = AdjustmentOp::BrightnessContrast(BrightnessContrast {
                brightness: p["brightness"].as_f64().ok_or("params.brightness")? as f32,
                contrast: p["contrast"].as_f64().ok_or("params.contrast")? as f32,
                use_legacy: p["useLegacy"].as_bool().ok_or("params.useLegacy")?,
            });
            over_fixture(corpus, e, "F_GRAY_RAMP_256x64", op, BlendSpace::Encoded, None)
        }
        "PS-C06" => c06(corpus, e),
        "PS-C07" => {
            let level = p["level"].as_f64().ok_or("params.level")? as f32 / 255.0;
            let fixture = if e.variant.contains("lattice") { "F_RGB_LATTICE17_289x17" } else { "F_GRAY_RAMP_256x64" };
            over_fixture(corpus, e, fixture, AdjustmentOp::Threshold { level }, BlendSpace::Encoded, None)
        }
        "PS-C08" => {
            let levels = p["levels"].as_u64().ok_or("params.levels")? as u16;
            over_fixture(corpus, e, "F_GRAY_RAMP_256x64", AdjustmentOp::Posterize { levels }, BlendSpace::Encoded, None)
        }
        other => Err(format!("case {other} not in this family")),
    }
}

/// PS-C01: ramp at layer opacity 50 inside a Normal group on a transparent document, with an Invert
/// adjustment layer (Normal or Multiply) above it in the group; exported with transparency.
fn c01(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let img = corpus.fixture("F_GRAY_RAMP_256x64")?;
    let mut scene = Scene::new(e.w, e.h);
    let t = scene.tile(OwnedTile::from_img(&img), 0, 0);
    let mode = match e.params["adj_mode"].as_str() {
        Some("null") | None => None,
        Some(m) => Some(ps_mode(m).ok_or_else(|| format!("adj_mode {m}"))?),
    };
    let mut layers = vec![layer(t, StudioBlendMode::Normal, 0.5, 1.0)];
    let ph = scene.placeholder();
    if let Some(m) = mode {
        layers.push(layer(ph, m, 1.0, 1.0));
    }
    let g = scene.node(group(true, layers));
    let root = scene.node(group(true, vec![layer(g, StudioBlendMode::Normal, 1.0, 1.0)]));
    scene.root(root);
    if mode.is_some() {
        scene.extend(LayerExtension::new(PlanNodeId(g), 1).with_adjustment(AdjustmentOp::Invert));
    }
    Ok(Built::Render { scene, store: Store::Premultiplied, rows: &["PS-07", "PS-01"] })
}

/// PS-C06: Exposure. 8-bit: the encoded ramp with pigment's encoded-document stand-in
/// (`linearize_gamma` 2.2, as composite sets it for an encoded document). 32-bit: Photoshop's
/// linear-light document; its own identity output (the pasted ramp converted to linear) is the
/// input state, the plan runs in linear light with the extended output stage (values above 1 kept).
fn c06(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let p = &e.params;
    let f = |k: &str| p[k].as_f64().unwrap_or(0.0) as f32;
    let deep = p["depth"].as_str() == Some("32");
    let op = AdjustmentOp::Exposure(Exposure { exposure: f("exposure_ev"), offset: f("offset"), gamma: f("gamma"), linearize_gamma: if deep { 1.0 } else { 2.2 } });
    if !deep {
        return over_fixture(corpus, e, "F_GRAY_RAMP_256x64", op, BlendSpace::Encoded, None);
    }
    let input = corpus.output("PS-C06__32bit_identity.tif")?;
    let built = over_fixture(corpus, e, "", op, BlendSpace::LinearLight, Some(input))?;
    let Built::Render { mut scene, rows, .. } = built else { return Err("unreachable".into()) };
    scene.ext = std::mem::take(&mut scene.ext).with_output(OutputStage::new(SampleRange::Extended, None));
    Ok(Built::Render { scene, store: Store::Opaque, rows })
}
