//! SMB-PRISM-ENG-5, oracle tier T1: WideEngine against LittleCMS 2 reference values.
//!
//! The reference numbers in `oracle/t1_lcms2.json` were produced OFFLINE by `prism-oracle/gen/t1_generate.py`
//! (lcms2 as bundled in the `imagecodecs` wheel, version recorded in the fixture). Nothing here links or
//! runs LittleCMS: the repository holds numbers, profile SHA-256 values and provenance only (STU-COL-140,
//! STU-COL-325). Installed Windows/Adobe profiles are read at test time from disk, checked against the
//! recorded SHA-256, and a pair whose profile is absent or different SKIPS with a typed reason. The three
//! synthetic profiles are committed (built from scratch, no licence question).
//!
//! Three families of tests, so a failure can be attributed:
//!   * `src_leg_*`  source profile -> PCS XYZ (relative, no BPC) against lcms2, unclamped;
//!   * `dst_leg_*`  PCS Lab -> destination device values for every intent (no BPC), clamped to 0..1;
//!   * `<pair>`     whole conversions, 4 intents x BPC on/off.
//! Metric: CIEDE2000 in destination Lab (D50). Device outputs are clamped to 0..1 and evaluated through the
//! SAME destination-to-XYZ leg for engine and lcms2 (the destination profile read as a source by WideEngine,
//! identity-colorant XYZ target), so the number measures the difference between two device vectors. The
//! stored lcms2 Lab of its own output is compared as well (`ref_consistency`, `de_vs_lcms_lab`) to expose
//! evaluation differences in that leg. Thresholds (fixture `thresholds`): matrix pairs max <= 0.1; LUT/CMYK
//! pairs mean <= 0.5 and max <= 1.5. They are never tuned to the engine.
mod oracle_support;
mod wide_support;
use hsk_studio_prism::*;
use oracle_support::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use wide_support::*;

const FIXTURE: &str = include_str!("oracle/t1_lcms2.json");
const SYNTH_RGB_V2_TABLE: &[u8] = include_bytes!("oracle/profiles/hsk-synth-rgb-v2-table.icc");
const SYNTH_RGB_V4_PARA4: &[u8] = include_bytes!("oracle/profiles/hsk-synth-rgb-v4-para4.icc");
const SYNTH_CMYK_V2: &[u8] = include_bytes!("oracle/profiles/hsk-synth-cmyk-v2.icc");

/// Defects already captured: (test id, intent, bpc, finding id). A listed combination must STILL exceed its
/// threshold (the test fails when it starts passing: remove the entry); everything else must pass.
const KNOWN_FINDINGS: &[(&str, &str, bool, &str)] = &[
    // DIV-ABS-V2-DISPLAY-WTPT-001: Absolute colorimetric with a v2 display profile (HP sRGB, Adobe RGB: wtpt D65): lcms2 treats the wtpt as D50, the engine uses the tag. Verified: with wtpt forced to D50 the engine agrees (matrix max <= 0.05, CMYK->RGB max <= 0.4).
    ("dst_leg_adobergb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("dst_leg_srgb_hp", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_fogra39_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_fogra39_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_gracol2006_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_gracol2006_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_rec2100_hlg203_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_rec2100_hlg203_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_swop_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_swop_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_synth_cmyk_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("l_synth_cmyk_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_acescg_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_acescg_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_p3_v4_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_p3_v4_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_prophoto_to_srgb", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_prophoto_to_srgb", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_srgb_to_p3_v4", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_srgb_to_p3_v4", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_srgb_to_prophoto", "absolute_colorimetric", false, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    ("m_srgb_to_prophoto", "absolute_colorimetric", true, "DIV-ABS-V2-DISPLAY-WTPT-001"),
    // DIV-CMYK-PERCEPTUAL-BPC-BLACK-001: Perceptual/saturation BPC from a v2 CMYK source: engine uses local black (SWOP L* 1.43), lcms2 the darkest colorant (400%, L* 0.0): different black point algorithm.
    ("l_fogra39_to_srgb", "perceptual", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    ("l_fogra39_to_srgb", "saturation", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    ("l_gracol2006_to_srgb", "perceptual", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    ("l_gracol2006_to_srgb", "saturation", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    ("l_swop_to_srgb", "perceptual", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    ("l_swop_to_srgb", "saturation", true, "DIV-CMYK-PERCEPTUAL-BPC-BLACK-001"),
    // DIV-V4-PERCEPTUAL-BLACK-001: lcms2 forces BPC for perceptual/saturation when a v4 profile with non-zero black is involved (its perceptual BPC-off result equals relative BPC-on); the engine keeps BPC an independent flag (STU-COL-136); also v4 reference-medium black.
    ("dst_leg_synth_rgb_v4_para4", "perceptual", false, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("dst_leg_synth_rgb_v4_para4", "saturation", false, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("l_rec2100_hlg203_to_srgb", "perceptual", true, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("l_synth_cmyk_to_synth_v4para4", "saturation", false, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("l_synth_cmyk_to_synth_v4para4", "saturation", true, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("m_synth_v2table_to_synth_v4para4", "perceptual", false, "DIV-V4-PERCEPTUAL-BLACK-001"),
    ("m_synth_v2table_to_synth_v4para4", "saturation", false, "DIV-V4-PERCEPTUAL-BLACK-001"),
    // PRISM-BPC-CMYK-BLACK-001: Relative BPC from a CMYK source: engine black L* 16.51 (SWOP) / 10.66 (FOGRA39) vs exact local black 16.59 / 10.68 (lcms2); the 0.08 L* gap is amplified near black (max 3-8 dE2000).
    ("l_fogra39_to_srgb", "relative_colorimetric", true, "PRISM-BPC-CMYK-BLACK-001"),
    ("l_gracol2006_to_srgb", "relative_colorimetric", true, "PRISM-BPC-CMYK-BLACK-001"),
    ("l_swop_to_srgb", "relative_colorimetric", true, "PRISM-BPC-CMYK-BLACK-001"),
    // PRISM-CURV-GAMMA-ZERO-001: ProPhoto RGB (curv gamma 1.8) as a source: primaries and secondaries convert to XYZ 0 (T0 srgb_curv18 reproduces it without any lcms2 data).
    ("src_leg_prophoto", "relative_colorimetric", false, "PRISM-CURV-GAMMA-ZERO-001"),
    // PRISM-HDR-SOURCE-CLAMP-001: Rec.2100 HLG203 (v4 mAB) as a source: PCS Y saturates at 1.0 from HLG signal 0.75 up, lcms2 reaches 4.92; HDR highlights are lost.
    ("src_leg_rec2100_hlg203", "relative_colorimetric", false, "PRISM-HDR-SOURCE-CLAMP-001"),
    // PRISM-LUT-DEST-ACCURACY-001: PCS -> CMYK table destinations: in-gamut Lab -> CMYK differs from exact table evaluation (mean 1.2-2.9, max 7-14 dE2000), worst in shadows; every RGB/CMYK -> CMYK pair misses the LUT gate.
    ("dst_leg_fogra39", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_fogra39", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_fogra39", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_fogra39", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_swop", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_swop", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_swop", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_swop", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_synth_cmyk_v2", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_synth_cmyk_v2", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_synth_cmyk_v2", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("dst_leg_synth_cmyk_v2", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "absolute_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "perceptual", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "relative_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_adobergb_to_swop", "saturation", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "absolute_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "perceptual", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "relative_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_fogra39", "saturation", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "absolute_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "perceptual", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "relative_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_swop", "saturation", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "absolute_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "perceptual", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "relative_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_srgb_to_synth_cmyk", "saturation", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "absolute_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "absolute_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "perceptual", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "perceptual", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "relative_colorimetric", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "relative_colorimetric", true, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "saturation", false, "PRISM-LUT-DEST-ACCURACY-001"),
    ("l_swop_to_fogra39", "saturation", true, "PRISM-LUT-DEST-ACCURACY-001"),
    // PRISM-LUT-SOURCE-REBAKE-001: Coarse-grid (7^4) synthetic CMYK source, saturation: dE2000 max 1.5-1.6 against the 1.5 gate (marginal).
    ("l_synth_cmyk_to_srgb", "saturation", false, "PRISM-LUT-SOURCE-REBAKE-001"),
    ("l_synth_cmyk_to_srgb", "saturation", true, "PRISM-LUT-SOURCE-REBAKE-001"),
    // PRISM-RGB-BPC-CURV-GAMMA-001: RGB profiles with curv gamma 1.8 (ProPhoto) and BPC on: garbage black point (L* -1.5e11), output destroyed.
    ("m_prophoto_to_srgb", "perceptual", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    ("m_prophoto_to_srgb", "relative_colorimetric", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    ("m_prophoto_to_srgb", "saturation", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    ("m_srgb_to_prophoto", "perceptual", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    ("m_srgb_to_prophoto", "relative_colorimetric", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    ("m_srgb_to_prophoto", "saturation", true, "PRISM-RGB-BPC-CURV-GAMMA-001"),
    // PRISM-SOURCE-XYZ-NEGATIVE-CLAMP-001: Source leg clamps negative PCS XYZ to 0 (P3 and ACEScg saturated red have negative Z after D50 adaptation).
    ("src_leg_acescg", "relative_colorimetric", false, "PRISM-SOURCE-XYZ-NEGATIVE-CLAMP-001"),
    ("src_leg_image_p3", "relative_colorimetric", false, "PRISM-SOURCE-XYZ-NEGATIVE-CLAMP-001"),
    ("src_leg_synth_rgb_v2_table", "relative_colorimetric", false, "PRISM-SOURCE-XYZ-NEGATIVE-CLAMP-001"),
    // PRISM-MISSING-INTENT-TABLE-REFUSAL-001: a v4 profile without an A2B2 table (HLG203 has A2B0/A2B1 only) makes the
    // engine refuse the saturation intent with a typed MissingIntentTable; lcms2 converts with a fallback table.
    ("l_rec2100_hlg203_to_srgb", "saturation", false, "PRISM-MISSING-INTENT-TABLE-REFUSAL-001"),
    ("l_rec2100_hlg203_to_srgb", "saturation", true, "PRISM-MISSING-INTENT-TABLE-REFUSAL-001"),
];

fn known_finding(id: &str, intent: &str, bpc: bool) -> Option<&'static str> {
    KNOWN_FINDINGS.iter().find(|(p, i, b, _)| *p == id && *i == intent && *b == bpc).map(|e| e.3)
}

fn load() -> Json {
    Json::parse(FIXTURE).expect("t1 fixture parses")
}

enum Resolved {
    Bytes(Vec<u8>),
    Skip(String),
}

fn resolve(doc: &Json, key: &str) -> Resolved {
    let spec = doc.get("profiles").get(key);
    if spec.get("origin").str() == "synthetic" {
        let bytes = match key {
            "synth_rgb_v2_table" => SYNTH_RGB_V2_TABLE,
            "synth_rgb_v4_para4" => SYNTH_RGB_V4_PARA4,
            "synth_cmyk_v2" => SYNTH_CMYK_V2,
            other => panic!("unknown synthetic profile {other}"),
        };
        assert_eq!(sha256_hex(bytes), spec.get("sha256").str(), "{key}: committed profile differs from the fixture record");
        return Resolved::Bytes(bytes.to_vec());
    }
    let file = spec.get("file").str();
    let mut dirs: Vec<PathBuf> = std::env::var_os(doc.get("search_env_var").str())
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    dirs.extend(spec.get("default_locations").arr().iter().map(|d| PathBuf::from(d.str())));
    for dir in dirs {
        if let Ok(bytes) = std::fs::read(dir.join(file)) {
            let got = sha256_hex(&bytes);
            return if got == spec.get("sha256").str() {
                Resolved::Bytes(bytes)
            } else {
                Resolved::Skip(format!("profile_sha256_mismatch file={file} found={got} recorded={}", spec.get("sha256").str()))
            };
        }
    }
    Resolved::Skip(format!("installed_profile_absent file={file}"))
}

/// Resolve every key or return the typed skip reason of the first unavailable profile.
fn resolve_all(doc: &Json, keys: &[&str]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    for key in keys {
        match resolve(doc, key) {
            Resolved::Bytes(b) => out.push(b),
            Resolved::Skip(reason) => return Err(format!("{key}: {reason}")),
        }
    }
    Ok(out)
}

/// Identity-colorant XYZ target: device values of this profile ARE PCS XYZ (D50 relative).
fn xyz_target() -> Vec<u8> {
    let s15 = |v: [i32; 3]| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for c in v {
            t.extend_from_slice(&c.to_be_bytes());
        }
        t
    };
    assemble(
        0x0420_0000,
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &[s15([63190, 65536, 54061]), s15([65536, 0, 0]), s15([0, 65536, 0]), s15([0, 0, 65536]), para_identity()],
        &[(b"wtpt", 0), (b"rXYZ", 1), (b"gXYZ", 2), (b"bXYZ", 3), (b"rTRC", 4), (b"gTRC", 4), (b"bTRC", 4)],
    )
}

/// Destination device values (clamped to 0..1) to Lab D50 through the engine's own device-to-PCS leg.
fn device_to_lab(eng: &mut WideEngine, ctx: &Ctx, dest: &[u8], target: &[u8], device: &[f32]) -> Vec<[f64; 3]> {
    let clamped: Vec<f32> = device.iter().map(|v| v.clamp(0.0, 1.0)).collect();
    let out = run(eng, ctx, dest, target, &clamped, Intent::RelativeColorimetric, false)
        .unwrap_or_else(|f| panic!("destination evaluation refused: {:?}", f.error));
    xyz_to_labs(&out.pixels)
}
fn xyz_to_labs(xyz: &[f32]) -> Vec<[f64; 3]> {
    xyz.chunks_exact(3).map(|p| xyz_to_lab([f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])).collect()
}

struct Stats {
    mean: f64,
    max: f64,
    /// Index of the worst patch in the original (unfiltered) patch list.
    worst: usize,
    des: Vec<f64>,
    /// Original patch index of every entry of `des`.
    idx: Vec<usize>,
}
/// Delta E 2000 statistics over `kept` patches (all when `None`): the in-gamut subset for RGB matrix
/// destinations, whose out-of-gamut device values are a clipping convention rather than a conversion.
fn stats(a: &[[f64; 3]], b: &[[f64; 3]], kept: Option<&[usize]>) -> Stats {
    assert_eq!(a.len(), b.len());
    let idx: Vec<usize> = kept.map_or_else(|| (0..a.len()).collect(), <[usize]>::to_vec);
    let des: Vec<f64> = idx.iter().map(|&i| delta_e_2000(a[i], b[i])).collect();
    let (mut max, mut worst) = (0.0f64, idx.first().copied().unwrap_or(0));
    for (k, d) in des.iter().enumerate() {
        if *d > max || d.is_nan() {
            (max, worst) = (if d.is_nan() { f64::INFINITY } else { *d }, idx[k]);
        }
    }
    Stats { mean: des.iter().sum::<f64>() / des.len().max(1) as f64, max, worst, des, idx }
}
fn kept_patches(v: &Json) -> Option<Vec<usize>> {
    match v {
        Json::Arr(items) => Some(items.iter().map(|i| i.i64() as usize).collect()),
        _ => None,
    }
}

/// Threshold verdict plus the bookkeeping shared by every T1 test.
struct Judge {
    id: &'static str,
    max_limit: f64,
    mean_limit: f64,
    failures: Vec<String>,
    expected_fails: Vec<String>,
}
impl Judge {
    fn new(doc: &Json, id: &'static str, class: &str) -> Self {
        let t = doc.get("thresholds");
        let (max_limit, mean_limit) = if class == "matrix" {
            (t.get("matrix").get("de2000_max").f64(), f64::INFINITY)
        } else {
            (t.get("lut").get("de2000_max").f64(), t.get("lut").get("de2000_mean").f64())
        };
        Self { id, max_limit, mean_limit, failures: Vec::new(), expected_fails: Vec::new() }
    }
    fn broken(&self, s: &Stats) -> bool {
        !(s.max <= self.max_limit && s.mean <= self.mean_limit)
    }
    fn record(&mut self, iname: &str, bpc: bool, s: &Stats) {
        let detail = format!(
            "{iname} bpc={bpc}: dE2000 mean {:.4} max {:.4} (worst patch {}; limits mean {} max {})",
            s.mean, s.max, s.worst, self.mean_limit, self.max_limit
        );
        match (self.broken(s), known_finding(self.id, iname, bpc)) {
            (true, Some(f)) => self.expected_fails.push(format!("{f}: {detail}")),
            (true, None) => self.failures.push(detail),
            (false, Some(f)) => self.failures.push(format!(
                "FINDING {f} no longer reproduces for {iname} bpc={bpc}: remove it from KNOWN_FINDINGS"
            )),
            (false, None) => {}
        }
    }
    fn finish(self) {
        for e in &self.expected_fails {
            eprintln!("ORACLE_T1_FINDING {}: {e}", self.id);
        }
        assert!(self.failures.is_empty(), "{}: {}", self.id, self.failures.join(" | "));
    }
}
fn worst3(s: &Stats, cin: usize, cout: usize, input: &[f32], prism: &[f32], reference: &[f32]) -> String {
    let mut order: Vec<usize> = (0..s.des.len()).collect();
    order.sort_by(|a, b| s.des[*b].total_cmp(&s.des[*a]));
    order
        .iter()
        .take(3)
        .map(|&k| {
            let i = s.idx[k];
            format!(
                "#{i} dE {:.3} in {:?} prism {:?} lcms2 {:?}",
                s.des[k],
                &input[i * cin..(i + 1) * cin],
                &prism[i * cout..(i + 1) * cout],
                &reference[i * cout..(i + 1) * cout]
            )
        })
        .collect::<Vec<_>>()
        .join(" || ")
}

// -------------------------------------------------------------------------------------------------
// Whole conversions
// -------------------------------------------------------------------------------------------------
fn run_pair(id: &'static str) {
    bounded(id, 600, move || {
        let doc = load();
        let pair = doc.get("pairs").arr().iter().find(|p| p.get("id").str() == id).unwrap_or_else(|| panic!("pair {id} missing"));
        let (src_key, dst_key) = (pair.get("source").str(), pair.get("destination").str());
        let bytes = match resolve_all(&doc, &[src_key, dst_key]) {
            Ok(b) => b,
            Err(reason) => {
                eprintln!("SKIP oracle_t1 pair={id} reason={reason}");
                return;
            }
        };
        let (src, dst) = (&bytes[0], &bytes[1]);
        let class = pair.get("class").str();
        let patches = to_f32(&doc.get("patch_sets").get(pair.get("patch_set").str()).get("values").f64s());
        let (cin, cout) = (pair.get("source_channels").i64() as usize, pair.get("destination_channels").i64() as usize);
        let n = patches.len() / cin;
        let ctx = Ctx::new();
        let mut eng = WideEngine::new(ctx.actor.clone(), 8).unwrap();
        let mut leg = WideEngine::new(ctx.actor.clone(), 2).unwrap();
        let target = xyz_target();
        let mut judge = Judge::new(&doc, id, class);
        let kept = kept_patches(pair.get("in_gamut_patches"));
        let combos = pair.get("combos").arr();
        let mut ref_cache: BTreeMap<usize, (Vec<f32>, Vec<[f64; 3]>)> = BTreeMap::new();
        for (ci, combo) in combos.iter().enumerate() {
            let (iname, bpc) = (combo.get("intent").str(), combo.get("bpc").bool());
            let source_idx = if combo.has("same_as") { combo.get("same_as").i64() as usize } else { ci };
            let entry = &combos[source_idx];
            assert!(entry.has("out"), "{id}: combo {ci} has no reference output (lcms_error: {:?})", entry.get("lcms_error"));
            ref_cache.entry(source_idx).or_insert_with(|| {
                let out = to_f32(&entry.get("out").f64s());
                assert_eq!(out.len(), n * cout, "{id}: reference length");
                let lab = entry.get("lab").f64s().chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                (out, lab)
            });
            let (ref_out, ref_lab_lcms) = &ref_cache[&source_idx];
            let out = match run(&mut eng, &ctx, src, dst, &patches, intent_of(iname), bpc) {
                Ok(o) => o,
                Err(f) => {
                    let detail = format!("{iname} bpc={bpc}: engine refused {:?}", f.error);
                    eprintln!("ORACLE_T1_REFUSED {id} {detail}");
                    match known_finding(id, iname, bpc) {
                        Some(fid) => judge.expected_fails.push(format!("{fid}: {detail}")),
                        None => judge.failures.push(detail),
                    }
                    continue;
                }
            };
            let prism_lab = device_to_lab(&mut leg, &ctx, dst, &target, &out.pixels);
            let ref_lab_same = device_to_lab(&mut leg, &ctx, dst, &target, ref_out);
            let main = stats(&prism_lab, &ref_lab_same, kept.as_deref());
            let vs_lcms_lab = stats(&prism_lab, ref_lab_lcms, kept.as_deref());
            let consistency = stats(&ref_lab_same, ref_lab_lcms, kept.as_deref());
            let clipped = out.pixels.iter().filter(|v| **v < -1e-4 || **v > 1.0001).count();
            // Absolute colorimetric attribution: lcms2 reads the wtpt of a v2 display profile as D50. Re-run the
            // engine with wtpt forced to D50 on such profiles; agreement then names the media-white convention.
            let abs_d50 = if iname == "absolute_colorimetric" && !bpc {
                let patch = |key: &str, b: &Vec<u8>| {
                    let v2_display = doc.get("profiles").get(key).get("icc_version").str().starts_with('2')
                        && doc.get("profiles").get(key).get("class").str() == "mntr";
                    if v2_display { with_d50_wtpt(b).unwrap_or_else(|| b.clone()) } else { b.clone() }
                };
                let (ps, pd) = (patch(src_key, src), patch(dst_key, dst));
                let mut diag = WideEngine::new(ctx.actor.clone(), 2).unwrap();
                run(&mut diag, &ctx, &ps, &pd, &patches, Intent::AbsoluteColorimetric, false).ok().map(|o| {
                    let lab = device_to_lab(&mut leg, &ctx, dst, &target, &o.pixels);
                    let st = stats(&lab, &ref_lab_same, kept.as_deref());
                    (st.mean, st.max)
                })
            } else {
                None
            };
            let r = out.receipt;
            eprintln!(
                "ORACLE_T1 {{\"pair\":\"{id}\",\"class\":\"{class}\",\"intent\":\"{iname}\",\"bpc\":{bpc},\"patches\":{n},\"scored_patches\":{},\"de2000_mean\":{:.4},\"de2000_max\":{:.4},\"worst_patch\":{},\"de_vs_lcms_lab_max\":{:.4},\"ref_consistency_max\":{:.4},\"out_of_range_values\":{clipped},\"bpc_rule\":\"{}\",\"bpc_src_lstar\":{:?},\"bpc_dst_lstar\":{:?},\"bpc_scale\":{:?},\"abs_scale\":{:?},\"abs_with_d50_wtpt\":{:?},\"verdict\":\"{}\"}}",
                main.idx.len(),
                main.mean,
                main.max,
                main.worst,
                vs_lcms_lab.max,
                consistency.max,
                r.bpc.rule,
                r.bpc.source_black_lstar,
                r.bpc.destination_black_lstar,
                r.bpc.scale,
                r.absolute.scale,
                abs_d50,
                if judge.broken(&main) { "EXCEEDS" } else { "ok" }
            );
            if judge.broken(&main) {
                eprintln!("ORACLE_T1_WORST {id} {iname} bpc={bpc}: {}", worst3(&main, cin, cout, &patches, &out.pixels, ref_out));
            }
            judge.record(iname, bpc, &main);
        }
        judge.finish();
    });
}

// -------------------------------------------------------------------------------------------------
// Source leg: profile -> PCS XYZ (relative colorimetric, no BPC), unclamped
// -------------------------------------------------------------------------------------------------
fn run_src_leg(id: &'static str, key: &'static str) {
    bounded(id, 300, move || {
        let doc = load();
        let leg = doc.get("legs").get("source").get(key);
        let bytes = match resolve_all(&doc, &[key]) {
            Ok(b) => b,
            Err(reason) => {
                eprintln!("SKIP oracle_t1 leg={id} reason={reason}");
                return;
            }
        };
        let patches = to_f32(&doc.get("patch_sets").get(leg.get("patch_set").str()).get("values").f64s());
        let cin = doc.get("patch_sets").get(leg.get("patch_set").str()).get("channels").i64() as usize;
        let reference = to_f32(&leg.get("xyz").f64s());
        let ctx = Ctx::new();
        let mut eng = WideEngine::new(ctx.actor.clone(), 2).unwrap();
        let mut judge = Judge::new(&doc, id, leg.get("class").str());
        let out = run(&mut eng, &ctx, &bytes[0], &xyz_target(), &patches, Intent::RelativeColorimetric, false)
            .unwrap_or_else(|f| panic!("{id}: engine refused {:?}", f.error));
        let s = stats(&xyz_to_labs(&out.pixels), &xyz_to_labs(&reference), None);
        let max_xyz = out.pixels.iter().zip(&reference).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        eprintln!(
            "ORACLE_T1 {{\"pair\":\"{id}\",\"class\":\"{}\",\"intent\":\"relative_colorimetric\",\"bpc\":false,\"patches\":{},\"de2000_mean\":{:.4},\"de2000_max\":{:.4},\"worst_patch\":{},\"max_abs_xyz_diff\":{max_xyz:.6},\"verdict\":\"{}\"}}",
            leg.get("class").str(),
            patches.len() / cin,
            s.mean,
            s.max,
            s.worst,
            if judge.broken(&s) { "EXCEEDS" } else { "ok" }
        );
        if judge.broken(&s) {
            eprintln!("ORACLE_T1_WORST {id} relative_colorimetric bpc=false: {}", worst3(&s, cin, 3, &patches, &out.pixels, &reference));
            if cin == 3 {
                // Neutral ramp (patches 0..=16 are k/16): PCS Y of the engine against lcms2.
                let ramp: Vec<String> = (0..17).map(|k| format!("{:.3}/{:.3}", out.pixels[k * 3 + 1], reference[k * 3 + 1])).collect();
                eprintln!("ORACLE_T1_RAMP {id} neutral Y prism/lcms2: {}", ramp.join(" "));
            }
        }
        judge.record("relative_colorimetric", false, &s);
        judge.finish();
    });
}

// -------------------------------------------------------------------------------------------------
// Destination leg: PCS Lab -> destination device values (every intent, no BPC), clamped to 0..1
// -------------------------------------------------------------------------------------------------
fn run_dst_leg(id: &'static str, key: &'static str) {
    bounded(id, 600, move || {
        let doc = load();
        let leg = doc.get("legs").get("destination").get(key);
        let bytes = match resolve_all(&doc, &[key]) {
            Ok(b) => b,
            Err(reason) => {
                eprintln!("SKIP oracle_t1 leg={id} reason={reason}");
                return;
            }
        };
        let lab = to_f32(&if leg.has("lab_patches") { leg.get("lab_patches") } else { doc.get("lab_patch_set").get("values") }.f64s());
        let (n, cout) = (lab.len() / 3, leg.get("channels").i64() as usize);
        let ctx = Ctx::new();
        let mut eng = WideEngine::new(ctx.actor.clone(), 8).unwrap();
        let mut evalr = WideEngine::new(ctx.actor.clone(), 2).unwrap();
        let target = xyz_target();
        let mut judge = Judge::new(&doc, id, leg.get("class").str());
        let combos = leg.get("combos").arr();
        let kept = kept_patches(leg.get("in_gamut_patches"));
        for (ci, combo) in combos.iter().enumerate() {
            let iname = combo.get("intent").str();
            let entry = if combo.has("same_as") { &combos[combo.get("same_as").i64() as usize] } else { &combos[ci] };
            let reference = to_f32(&entry.get("out").f64s());
            assert_eq!(reference.len(), n * cout, "{id}: reference length");
            let out = match run_lab(&mut eng, &ctx, &bytes[0], &lab, intent_of(iname), false) {
                Ok(o) => o,
                Err(f) => {
                    judge.failures.push(format!("{iname}: engine refused {:?}", f.error));
                    continue;
                }
            };
            let s = stats(
                &device_to_lab(&mut evalr, &ctx, &bytes[0], &target, &out.pixels),
                &device_to_lab(&mut evalr, &ctx, &bytes[0], &target, &reference),
                kept.as_deref(),
            );
            let clipped = out.pixels.iter().filter(|v| **v < -1e-4 || **v > 1.0001).count();
            eprintln!(
                "ORACLE_T1 {{\"pair\":\"{id}\",\"class\":\"{}\",\"intent\":\"{iname}\",\"bpc\":false,\"patches\":{n},\"scored_patches\":{},\"de2000_mean\":{:.4},\"de2000_max\":{:.4},\"worst_patch\":{},\"out_of_range_values\":{clipped},\"verdict\":\"{}\"}}",
                leg.get("class").str(),
                s.idx.len(),
                s.mean,
                s.max,
                s.worst,
                if judge.broken(&s) { "EXCEEDS" } else { "ok" }
            );
            if judge.broken(&s) {
                eprintln!("ORACLE_T1_WORST {id} {iname} bpc=false: {}", worst3(&s, 3, cout, &lab, &out.pixels, &reference));
            }
            judge.record(iname, false, &s);
        }
        judge.finish();
    });
}

macro_rules! t1_pairs {
    ($($name:ident),* $(,)?) => {
        const COVERED_PAIRS: &[&str] = &[$(stringify!($name)),*];
        $(#[test] fn $name() { run_pair(stringify!($name)); })*
    };
}
t1_pairs! {
    m_srgb_to_adobergb,
    m_adobergb_to_srgb,
    m_srgb_to_prophoto,
    m_prophoto_to_srgb,
    m_srgb_to_p3_v4,
    m_p3_v4_to_srgb,
    m_acescg_to_srgb,
    m_synth_v2table_to_synth_v4para4,
    m_synth_v4para4_to_synth_v2table,
    l_srgb_to_swop,
    l_swop_to_srgb,
    l_srgb_to_fogra39,
    l_fogra39_to_srgb,
    l_gracol2006_to_srgb,
    l_swop_to_fogra39,
    l_adobergb_to_swop,
    l_rec2100_hlg203_to_srgb,
    l_synth_cmyk_to_srgb,
    l_srgb_to_synth_cmyk,
    l_synth_cmyk_to_synth_v4para4,
}
macro_rules! t1_legs {
    (src: [$($sname:ident => $skey:literal),* $(,)?], dst: [$($dname:ident => $dkey:literal),* $(,)?]) => {
        const COVERED_SRC: &[&str] = &[$($skey),*];
        const COVERED_DST: &[&str] = &[$($dkey),*];
        $(#[test] fn $sname() { run_src_leg(stringify!($sname), $skey); })*
        $(#[test] fn $dname() { run_dst_leg(stringify!($dname), $dkey); })*
    };
}
t1_legs! {
    src: [
        src_leg_srgb_hp => "srgb_hp",
        src_leg_adobergb => "adobergb",
        src_leg_prophoto => "prophoto",
        src_leg_image_p3 => "image_p3",
        src_leg_acescg => "acescg",
        src_leg_rec2100_hlg203 => "rec2100_hlg203",
        src_leg_swop => "swop",
        src_leg_fogra39 => "fogra39",
        src_leg_gracol2006 => "gracol2006",
        src_leg_synth_rgb_v2_table => "synth_rgb_v2_table",
        src_leg_synth_rgb_v4_para4 => "synth_rgb_v4_para4",
        src_leg_synth_cmyk_v2 => "synth_cmyk_v2",
    ],
    dst: [
        dst_leg_srgb_hp => "srgb_hp",
        dst_leg_adobergb => "adobergb",
        dst_leg_prophoto => "prophoto",
        dst_leg_image_p3 => "image_p3",
        dst_leg_swop => "swop",
        dst_leg_fogra39 => "fogra39",
        dst_leg_synth_rgb_v2_table => "synth_rgb_v2_table",
        dst_leg_synth_rgb_v4_para4 => "synth_rgb_v4_para4",
        dst_leg_synth_cmyk_v2 => "synth_cmyk_v2",
    ]
}

#[test]
fn every_fixture_pair_and_leg_has_a_test_and_provenance_is_recorded() {
    let doc = load();
    let ids = |v: &Json| -> Vec<String> { v.arr().iter().map(|p| p.get("id").str().to_string()).collect() };
    let mut listed = ids(doc.get("pairs"));
    let mut covered: Vec<String> = COVERED_PAIRS.iter().map(|s| s.to_string()).collect();
    listed.sort();
    covered.sort();
    assert_eq!(listed, covered, "fixture pairs and test list differ");
    let mut src: Vec<&str> = doc.get("legs").get("source").keys();
    let mut dst: Vec<&str> = doc.get("legs").get("destination").keys();
    let (mut cs, mut cd) = (COVERED_SRC.to_vec(), COVERED_DST.to_vec());
    for v in [&mut src, &mut dst, &mut cs, &mut cd] {
        v.sort_unstable();
    }
    assert_eq!((src, dst), (cs, cd), "fixture legs and test list differ");
    let tool = doc.get("provenance").get("tool");
    assert!(tool.get("name").str().contains("lcms2"));
    assert!(!tool.get("version").str().is_empty(), "tool version must be recorded (STU-COL-325)");
    assert_eq!(doc.get("tier").str(), "T1");
    for key in doc.get("profiles").keys() {
        assert_eq!(doc.get("profiles").get(key).get("sha256").str().len(), 64, "{key}: sha256");
    }
    // No third-party profile bytes in the repository: only the three synthetic files are committed.
    let synthetic =
        doc.get("profiles").keys().into_iter().filter(|k| doc.get("profiles").get(k).get("origin").str() == "synthetic").count();
    assert_eq!(synthetic, 3);
}
