//! PS-B01: blend-mode grids. Backdrop `F_GRID_HRAMP_256` (gray = x), source `F_GRID_VRAMP_256`
//! (gray = y) pasted above it with the case's mode, opacity and fill, at 8 or 16 bits per channel
//! (oracle script `blendGrid`). Studio scene: one isolated root group [backdrop Normal, source
//! mode/opacity/fill], encoded blend space (Photoshop's default document blending).
use super::support::*;
use hsk_studio_pigment::render_plan::*;

const ROWS: &[&str] = &["PS-03", "PS-01"];

pub fn build(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let p = &e.params;
    let mode_name = p["mode"].as_str().ok_or("params.mode")?;
    let mode = ps_mode(mode_name).ok_or_else(|| format!("unknown mode {mode_name}"))?;
    let opacity = p["opacity"].as_f64().ok_or("params.opacity")? as f32 / 100.0;
    let fill = p["fill"].as_f64().ok_or("params.fill")? as f32 / 100.0;
    let back = corpus.fixture("F_GRID_HRAMP_256")?;
    let src = corpus.fixture("F_GRID_VRAMP_256")?;
    let mut scene = Scene::new(e.w, e.h);
    // A 16-bit document holds the pasted 8-bit fixtures on Photoshop's 15-bit scale.
    let tile = if e.bits.ends_with("SIXTEEN") { OwnedTile::from_img_15bit } else { OwnedTile::from_img };
    let b = scene.tile(tile(&back), 0, 0);
    let s = scene.tile(tile(&src), 0, 0);
    let root = scene.node(group(true, vec![layer(b, StudioBlendMode::Normal, 1.0, 1.0), layer(s, mode, opacity, fill)]));
    scene.root(root);
    if mode == StudioBlendMode::Dissolve && opacity * fill < 1.0 {
        let n = (e.w * e.h) as usize;
        let mut backdrop = Vec::with_capacity(n);
        let mut source = Vec::with_capacity(n);
        for y in 0..e.h {
            for x in 0..e.w {
                backdrop.push(back.rgb(x, y));
                source.push(src.rgb(x, y));
            }
        }
        return Ok(Built::Dissolve { scene, backdrop, source, rows: ROWS });
    }
    Ok(Built::Render { scene, store: Store::FlattenWhite, rows: ROWS })
}
