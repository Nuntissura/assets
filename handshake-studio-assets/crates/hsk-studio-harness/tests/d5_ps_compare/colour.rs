//! Colour cases. PS-L01: the 512 Lab patches (8-bit Lab promoted to 16 bits, input
//! `inputs/F_LAB_OFFNODE_512x1_8bit_src.tif`) converted to U.S. Web Coated (SWOP) v2 with relative
//! colorimetric or perceptual intent and black point compensation (the oracle script's
//! `convertProfile(profile, intent, true, false)`), through Prism's wide engine with the profile
//! Photoshop embedded in its own output; compared as ink fractions. PS-L02: the Photoshop-authored
//! 16-bit Lab PSD (`inputs/PS-L02__lab16_neutral_ramp.psd`) opened through the harness convert flow
//! (interop-psd + Prism into the linear-light sRGB working space); the reference is the oracle's own
//! Lab TIFF of the same document taken through CIE Lab (D50) -> XYZ -> Bradford D65 -> sRGB.
use super::support::*;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio as folio;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::{Intent, ProfileInput, WideEngine, WideSource, WideTransformRequest, profile_hash};

struct Null;
impl SinkPort for Null {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

/// The CIELab samples of a decoded TIFF (interop-tiff hands CIELab out as stored: L unsigned,
/// a/b two's-complement in the unsigned type, 256 steps per unit at 16 bits).
fn lab_of(img: &Img, x: u32, y: u32) -> [f32; 3] {
    let raw = |c: usize| (img.at(x, y, c) * 65535.0).round() as u16;
    [
        (f64::from(raw(0)) / 65535.0 * 100.0) as f32,
        (f64::from(raw(1) as i16) / 256.0) as f32,
        (f64::from(raw(2) as i16) / 256.0) as f32,
    ]
}

pub fn build(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    match e.case_id.as_str() {
        "PS-L01" => l01(corpus, e),
        "PS-L02" => l02(corpus, e),
        other => Err(format!("case {other} not in this family")),
    }
}

fn l01(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let input = corpus.fixture("F_LAB_OFFNODE_512x1_8bit_src")?;
    if input.photometric != 8 || input.bits != 16 {
        return Err(format!("lab input photometric {} bits {}", input.photometric, input.bits));
    }
    let file = e.file.clone().unwrap_or_default();
    let bytes = read_pinned(&corpus.dir, &file, &corpus.pins)?;
    let parsed = hsk_studio_interop_tiff::decode(&bytes, &hsk_studio_interop_tiff::Limits::default(), &CancellationToken::default())
        .map_err(|e| format!("oracle tiff {e:?}"))?;
    let icc = parsed.pages.first().and_then(|p| p.icc.clone()).ok_or("oracle output without its embedded SWOP profile")?;
    let mut pixels = Vec::with_capacity(input.w as usize * 3);
    for x in 0..input.w {
        pixels.extend_from_slice(&lab_of(&input, x, 0));
    }
    let actor = ActorContext::new("d5", "d5-ps-compare", "d5", "d5-ps-compare", "d5", "d5").map_err(|e| format!("actor {e:?}"))?;
    let dst_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-0000000000d5").map_err(|e| format!("id {e:?}"))?;
    let doc_id = DomainId::parse("SDOC-019abcde-0000-7000-8000-0000000000d5").map_err(|e| format!("id {e:?}"))?;
    let intent = if e.variant.ends_with("perceptual") { Intent::Perceptual } else { Intent::RelativeColorimetric };
    let request = WideTransformRequest {
        source: WideSource::Lab,
        destination: ProfileInput { profile_id: &dst_id, bytes: &icc, expected_sha256: profile_hash(&icc) },
        pixels: &pixels,
        intent,
        black_point_compensation: true,
        revision: 1,
        expected_revision: 1,
        correlation_id: 5,
        resource_id: &doc_id,
        actor: &actor,
    };
    let mut engine = WideEngine::new(actor.clone(), 2).map_err(|e| format!("engine {e:?}"))?;
    let out = match engine.transform(&request, &CancellationToken::default(), &mut Null) {
        Ok(out) => out,
        Err(f) => return Ok(Built::NotSupported { reason: format!("studio_refusal prism {:?}", f.error), rows: &["PS-27", "PS-26"] }),
    };
    let px: Vec<[f32; 4]> = out.pixels.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Ok(Built::Computed { px, store: Store::Ink, rows: &["PS-27", "PS-26"], engine: "prism WideEngine Lab->SWOP (BPC on)", reference: None })
}

/// CIE Lab (D50) -> linear sRGB (Bradford D50 -> D65), the reference of PS-L02.
fn lab_to_linear_srgb(l: f64, a: f64, b: f64) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| if t * t * t > 216.0 / 24389.0 { t * t * t } else { (116.0 * t - 16.0) / (24389.0 / 27.0) };
    let xyz = [inv(fx) * 0.9642, if l > 8.0 { fy * fy * fy } else { l / (24389.0 / 27.0) }, inv(fz) * 0.8249];
    // Bradford D50 -> D65 (Lindbloom) then XYZ -> linear sRGB (IEC 61966-2-1).
    let m = [[0.9555766, -0.0230393, 0.0631636], [-0.0282895, 1.0099416, 0.0210077], [0.0122982, -0.0204830, 1.3299098]];
    let d65 = [0, 1, 2].map(|i| m[i][0] * xyz[0] + m[i][1] * xyz[1] + m[i][2] * xyz[2]);
    let r = [[3.2404542, -1.5371385, -0.4985314], [-0.9692660, 1.8760108, 0.0415560], [0.0556434, -0.2040259, 1.0572252]];
    [0, 1, 2].map(|i| r[i][0] * d65[0] + r[i][1] * d65[1] + r[i][2] * d65[2])
}
fn encode_srgb(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

struct TrustAll;
impl folio::Resolver for TrustAll {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}

fn l02(corpus: &Corpus, e: &Entry) -> Result<Built, String> {
    let rows: &'static [&'static str] = &["PS-26", "PS-35"];
    let psd = read_pinned(&corpus.dir, "inputs/PS-L02__lab16_neutral_ramp.psd", &corpus.pins)?;
    let cancel = CancellationToken::default();
    let (package, _) = match hsk_studio_harness::import::convert_to_native(&psd, &TrustAll, &cancel) {
        Ok(p) => p,
        Err(err) => return Ok(Built::NotSupported { reason: format!("studio_refusal convert_to_native {}", err.code()), rows }),
    };
    let opened = hsk_studio_package::read_package(&package, &hsk_studio_package::Limits::default(), folio::Budget::default(), &TrustAll, &cancel)
        .map_err(|e| format!("package {e:?}"))?;
    let tile = opened.assets.values().next().ok_or("converted package without a tile")?;
    let floats: Vec<f32> = tile.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let n = e.w as usize;
    if floats.len() < n * 3 {
        return Err(format!("tile holds {} floats, want {}", floats.len(), n * 3));
    }
    // Studio: linear working RGB -> sRGB-encoded; reference: the oracle Lab TIFF through CIE maths.
    let px: Vec<[f32; 4]> = (0..n).map(|i| {
        let c = [0, 1, 2].map(|k| encode_srgb(f64::from(floats[i * 3 + k])) as f32);
        [c[0], c[1], c[2], 1.0]
    }).collect();
    let oracle = corpus.output(e.file.as_deref().unwrap_or_default())?;
    let mut reference = Img { w: oracle.w, h: 1, ch: 3, bits: 8, float: false, photometric: 2, px: Vec::with_capacity(n * 3) };
    for x in 0..oracle.w {
        let [l, a, b] = lab_of(&oracle, x, 0);
        let rgb = lab_to_linear_srgb(f64::from(l), f64::from(a), f64::from(b));
        for v in rgb {
            reference.px.push((encode_srgb(v) * 255.0).round() / 255.0);
        }
    }
    Ok(Built::Computed { px, store: Store::Opaque, rows, engine: "harness convert_to_native (interop-psd Lab16 + Prism) vs CIE reference of the oracle Lab TIFF", reference: Some(reference) })
}
