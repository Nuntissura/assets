//! Shared machinery of the D5 Photoshop oracle comparison (lane SMB-D5-PS-COMPARE): corpus root and
//! sha256 pins, TIFF loading through the Studio TIFF owner (`hsk-studio-interop-tiff`), owned plan
//! sources, render through `hsk-studio-render-cpu`, per-variant comparison, the checked-in
//! expectations and the per-variant report.
//!
//! Corpus: `<oracle corpus root>/adobe-session/photoshop` (root from `.config/oracle-corpus.toml`).
//! Every file read is pinned by sha256 in `corpus_sha256.txt`; an absent root, file or hash prints
//! `NOT_PROVEN:` (V4-03) and is never counted as a match.
//!
//! Verdicts per manifest variant:
//! - `MATCH`: every compared sample within the depth tolerance ([`Tolerance`]).
//! - `MISMATCH`: at least one sample outside it (max / mean delta, worst pixel recorded).
//! - `NOT_SUPPORTED`: Studio refuses the scene (typed refusal key) or the scene cannot be built
//!   from Studio's plan vocabulary (builder reason).
//! - `ORACLE_ABSENT`: the oracle run has no output for the variant (manifest status not `ok`).
//! - `ORACLE_INVALID`: the oracle output provably lacks the setting the case asked for.
//! - `NOT_PROVEN`: the corpus file is absent or differs from its pin.
//!
//! A `MISMATCH` or `NOT_SUPPORTED` fails its test unless `expectations.json` lists it with the same
//! verdict and a reason; an exact-name expectation whose variant now matches is printed as
//! `STALE_EXPECTATION` (a glob entry covers a family whose members may match).
#![allow(dead_code)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_prism::{hash_hex, profile_hash};
use hsk_studio_render_cpu::Renderer;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CORPUS_SUBDIR: &str = "adobe-session/photoshop";
const PINS: &str = include_str!("corpus_sha256.txt");
const EXPECTATIONS: &str = include_str!("expectations.json");

// ---- corpus -------------------------------------------------------------------------------------

/// The one corpus-root convention (`.config/oracle-corpus.toml`): walk up from the workspace root.
pub fn corpus_root() -> Result<PathBuf, String> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(ws.join(".config/oracle-corpus.toml"))
        .map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let value = |key: &str| {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let search = value("search").ok_or("corpus_convention_without_search")?;
    let max: usize = value("max-ancestors")
        .and_then(|v| v.parse().ok())
        .ok_or("corpus_convention_without_max_ancestors")?;
    let mut dir = ws
        .canonicalize()
        .map_err(|e| format!("workspace_unresolved error={e}"))?;
    for _ in 0..=max {
        let candidate = dir.join(&search);
        if candidate.is_dir() {
            return Ok(candidate);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    Err(format!("corpus_root_absent search={search}"))
}

/// The pinned sha256 of every corpus file the suite reads (`sha256  relative/path`).
pub fn pins() -> BTreeMap<String, String> {
    PINS.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (h, p) = l.split_once("  ")?;
            Some((p.trim().to_string(), h.trim().to_string()))
        })
        .collect()
}

/// Reads one corpus file and checks its pin; `Err` is the NOT_PROVEN reason.
pub fn read_pinned(dir: &Path, rel: &str, pins: &BTreeMap<String, String>) -> Result<Vec<u8>, String> {
    let want = pins.get(rel).ok_or_else(|| format!("unpinned file={rel}"))?;
    let bytes = std::fs::read(dir.join(rel)).map_err(|e| format!("file_absent file={rel} error={e}"))?;
    let got = hash_hex(&profile_hash(&bytes));
    if &got != want {
        return Err(format!("hash_mismatch file={rel} sha256={got} pinned={want}"));
    }
    Ok(bytes)
}

/// One decoded oracle or fixture image: samples normalised to `[0, 1]` (floats as stored).
#[derive(Clone, Debug)]
pub struct Img {
    pub w: u32,
    pub h: u32,
    pub ch: usize,
    pub bits: u16,
    pub float: bool,
    /// File photometric: 1 gray, 2 RGB, 5 CMYK, 8 CIELab.
    pub photometric: u16,
    pub px: Vec<f64>,
}
impl Img {
    pub fn at(&self, x: u32, y: u32, c: usize) -> f64 {
        let i = (y as usize * self.w as usize + x as usize) * self.ch + c;
        self.px[i]
    }
    /// The colour of `(x, y)` as straight RGB (first three channels).
    pub fn rgb(&self, x: u32, y: u32) -> [f32; 3] {
        [self.at(x, y, 0) as f32, self.at(x, y, 1) as f32, self.at(x, y, 2) as f32]
    }
}

pub fn decode_tiff(bytes: &[u8]) -> Result<Img, String> {
    let file = hsk_studio_interop_tiff::decode(bytes, &hsk_studio_interop_tiff::Limits::default(), &CancellationToken::default())
        .map_err(|e| format!("tiff_decode error={e:?}"))?;
    let page = file.pages.first().ok_or("tiff_without_page")?;
    let ch = usize::from(page.samples_per_pixel);
    let (px, float): (Vec<f64>, bool) = match page.pixels() {
        hsk_studio_interop_tiff::Samples::U8(v) => (v.iter().map(|s| f64::from(*s) / 255.0).collect(), false),
        hsk_studio_interop_tiff::Samples::U16(v) => (v.iter().map(|s| f64::from(*s) / 65535.0).collect(), false),
        hsk_studio_interop_tiff::Samples::F32(v) => (v.iter().map(|s| f64::from(*s)).collect(), true),
        other => return Err(format!("tiff_sample_type_unexpected bits={}", other.bits())),
    };
    Ok(Img {
        w: page.width,
        h: page.height,
        ch,
        bits: page.bits_per_sample,
        float,
        photometric: page.source_photometric,
        px,
    })
}

/// One manifest entry (`hsk.oracle.session.manifest@1`).
#[derive(Clone, Debug)]
pub struct Entry {
    pub case_id: String,
    pub variant: String,
    pub file: Option<String>,
    pub status: String,
    pub alpha: bool,
    pub w: u32,
    pub h: u32,
    pub bits: String,
    pub params: Value,
}

pub struct Corpus {
    pub dir: PathBuf,
    pub pins: BTreeMap<String, String>,
    pub entries: Vec<Entry>,
    fixtures: std::sync::Mutex<BTreeMap<String, std::sync::Arc<Img>>>,
}
impl Corpus {
    /// `Err` = the whole suite is NOT_PROVEN (reason).
    pub fn open() -> Result<Self, String> {
        let dir = corpus_root()?.join(CORPUS_SUBDIR);
        let pins = pins();
        let manifest = read_pinned(&dir, "manifest.json", &pins)?;
        let v: Value = serde_json::from_slice(&manifest).map_err(|e| format!("manifest_json error={e}"))?;
        let entries = v["entries"]
            .as_array()
            .ok_or("manifest_without_entries")?
            .iter()
            .map(|e| Entry {
                case_id: e["case_id"].as_str().unwrap_or_default().to_string(),
                variant: e["variant"].as_str().unwrap_or_default().to_string(),
                file: e["file"].as_str().map(str::to_string),
                status: e["status"].as_str().unwrap_or_default().to_string(),
                alpha: e["alpha"].as_bool().unwrap_or(false),
                w: e["w"].as_u64().unwrap_or(0) as u32,
                h: e["h"].as_u64().unwrap_or(0) as u32,
                bits: e["bits"].as_str().unwrap_or_default().to_string(),
                params: e["params"].clone(),
            })
            .collect();
        Ok(Self { dir, pins, entries, fixtures: Default::default() })
    }
    /// A fixture under `inputs/` (decoded once, shared).
    pub fn fixture(&self, name: &str) -> Result<std::sync::Arc<Img>, String> {
        if let Some(img) = self.fixtures.lock().unwrap().get(name) {
            return Ok(img.clone());
        }
        let bytes = read_pinned(&self.dir, &format!("inputs/{name}.tif"), &self.pins)?;
        let img = std::sync::Arc::new(decode_tiff(&bytes)?);
        self.fixtures.lock().unwrap().insert(name.to_string(), img.clone());
        Ok(img)
    }
    /// Another oracle output used as an input state (e.g. the 32-bit identity ramp).
    pub fn output(&self, file: &str) -> Result<Img, String> {
        decode_tiff(&read_pinned(&self.dir, file, &self.pins)?)
    }
}

// ---- owned plan sources and the render call ----------------------------------------------------

pub fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}
/// Photoshop's default: document values blended as encoded (gamma) values, never converted here.
pub fn tag() -> ColourTag {
    ColourTag { profile_sha256: profile_hash(b"d5-oracle-document-space"), blend_space: BlendSpace::Encoded }
}
pub fn target(w: u32, h: u32) -> OutputTarget {
    OutputTarget { extent: rect(0, 0, w, h), format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: tag() }
}
pub fn layer(input: u32, mode: StudioBlendMode, opacity: f32, fill: f32) -> Layer {
    Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: fill, mask: None }
}
pub fn group(isolated: bool, layers: Vec<Layer>) -> PlanNode {
    PlanNode::Group(Group { isolated, layers })
}

pub struct OwnedTile {
    pub width: u32,
    pub height: u32,
    colour: Vec<u8>,
    alpha: Vec<u8>,
    /// The plan's colour tag (set by [`Scene::tile`]); sources must carry the plan's tag.
    space: ColourTag,
}
impl OwnedTile {
    pub fn new(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let n = width as usize * height as usize;
        let (mut colour, mut alpha) = (Vec::with_capacity(n * 12), Vec::with_capacity(n * 4));
        for y in 0..height {
            for x in 0..width {
                let p = pixel(x, y);
                for v in &p[..3] {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { width, height, colour, alpha, space: tag() }
    }
    /// An opaque tile from an RGB image (8/16-bit fixtures normalised exactly).
    pub fn from_img(img: &Img) -> Self {
        Self::new(img.w, img.h, |x, y| {
            let [r, g, b] = img.rgb(x, y);
            [r, g, b, if img.ch > 3 { img.at(x, y, 3) as f32 } else { 1.0 }]
        })
    }
    /// [`OwnedTile::from_img`] with every sample rounded to Photoshop's 15-bit scale: the state of
    /// an 8-bit fixture pasted into a 16-bit document (`round(v * 32768) / 32768`).
    pub fn from_img_15bit(img: &Img) -> Self {
        let q = |v: f32| ((f64::from(v) * 32768.0).round() / 32768.0) as f32;
        Self::new(img.w, img.h, |x, y| {
            let [r, g, b] = img.rgb(x, y);
            [q(r), q(g), q(b), if img.ch > 3 { img.at(x, y, 3) as f32 } else { 1.0 }]
        })
    }
    pub fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.width,
            height: self.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.width) * 4,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: self.space,
        }
    }
    pub fn input_at(&self, key: u32, x: i64, y: i64) -> TileInput {
        TileInput {
            key: SourceKey(key),
            placement: rect(x, y, self.width, self.height),
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: profile_hash(&self.colour),
            alpha_sha256: profile_hash(&self.alpha),
        }
    }
}

pub struct OwnedMask {
    pub bounds: Rect,
    bytes: Vec<u8>,
}
impl OwnedMask {
    /// An 8-bit coverage plane.
    pub fn u8(bounds: Rect, value: impl Fn(u32, u32) -> u8) -> Self {
        let mut bytes = Vec::with_capacity(bounds.width as usize * bounds.height as usize);
        for y in 0..bounds.height {
            for x in 0..bounds.width {
                bytes.push(value(x, y));
            }
        }
        Self { bounds, bytes }
    }
    pub fn input(&self, key: u32, inverted: bool, density: f32) -> MaskInput {
        MaskInput {
            key: SourceKey(key),
            placement: self.bounds,
            format: MaskFormat::CoverageU8,
            sha256: profile_hash(&self.bytes),
            inverted,
            density,
        }
    }
}

/// A scene: the plan, its envelope and the sources it reads.
pub struct Scene {
    pub plan: RenderPlan,
    pub ext: PlanExtensions,
    pub tiles: Vec<(u32, OwnedTile)>,
    pub masks: Vec<(u32, OwnedMask)>,
}
impl Scene {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            plan: RenderPlan { revision: 1, target: target(w, h), nodes: Vec::new(), root: PlanNodeId(0) },
            ext: PlanExtensions::default(),
            tiles: Vec::new(),
            masks: Vec::new(),
        }
    }
    /// Adds a node and returns its id.
    pub fn node(&mut self, n: PlanNode) -> u32 {
        self.plan.nodes.push(n);
        (self.plan.nodes.len() - 1) as u32
    }
    /// Adds a tile source placed at `(x, y)` and its node; returns the node id.
    pub fn tile(&mut self, mut t: OwnedTile, x: i64, y: i64) -> u32 {
        t.space = self.plan.target.space;
        let key = self.tiles.len() as u32 + 1;
        let input = t.input_at(key, x, y);
        self.tiles.push((key, t));
        self.node(PlanNode::Tile(input))
    }
    /// Adds a coverage-mask source; returns the mask input.
    pub fn mask(&mut self, m: OwnedMask, inverted: bool, density: f32) -> MaskInput {
        let key = 1000 + self.masks.len() as u32;
        let input = m.input(key, inverted, density);
        self.masks.push((key, m));
        input
    }
    /// The canonical adjustment placeholder over the whole document.
    pub fn placeholder(&mut self) -> u32 {
        let e = self.plan.target.extent;
        self.node(PlanNode::Solid { rect: e, rgba: [0.0; 4] })
    }
    pub fn root(&mut self, id: u32) {
        self.plan.root = PlanNodeId(id);
    }
    pub fn extend(&mut self, e: LayerExtension) {
        self.ext.layers.push(e);
    }
}

struct Sources<'a>(&'a Scene);
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.0.tiles.iter().find(|(k, _)| *k == key.0).map(|(_, t)| t.planes())
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let (_, m) = self.0.masks.iter().find(|(k, _)| *k == key.0)?;
        Some(Mask {
            bounds: m.bounds,
            sample_format: MaskFormat::CoverageU8.key(),
            bytes: &m.bytes,
            stride_bytes: u64::from(m.bounds.width),
            sha256: profile_hash(&m.bytes),
        })
    }
}

/// Assembles the delivered chunks into a full-extent straight RGBA grid.
struct Grid {
    w: u32,
    px: Vec<[f32; 4]>,
}
fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let s = row * r.width as usize + col;
                let i = (r.y as usize + row) * self.w as usize + r.x as usize + col;
                self.px[i] = [f32_at(colour, s * 12), f32_at(colour, s * 12 + 4), f32_at(colour, s * 12 + 8), f32_at(alpha, s * 4)];
            }
        }
        Ok(())
    }
}

/// Renders a scene through render-cpu's envelope path; `Err` is the typed refusal key.
pub fn render(scene: &Scene) -> Result<Vec<[f32; 4]>, String> {
    let e = scene.plan.target.extent;
    let mut grid = Grid { w: e.width, px: vec![[f32::NAN; 4]; e.width as usize * e.height as usize] };
    let renderer = Renderer::new(64).map_err(|e| format!("renderer {e:?}"))?;
    let request = ExecuteRequest { expected_revision: 1, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 34 };
    renderer
        .execute_extended(&scene.plan, &scene.ext, &request, &Sources(scene), &CancellationToken::default(), &mut grid)
        .map_err(refusal_key)?;
    Ok(grid.px)
}
fn refusal_key(e: BackendError) -> String {
    match e {
        BackendError::Unsupported { operator, .. } => format!("unsupported:{operator}"),
        BackendError::InvalidPlan(p) => format!("invalid_plan:{}", p.code()),
        other => other.code().to_string(),
    }
}

// ---- comparison ---------------------------------------------------------------------------------

/// The MATCH tolerance per oracle depth, in the oracle's own terms:
/// 8-bit: integer code difference <= 1 after rounding Studio's value to 8 bits;
/// 16-bit: <= 4 steps of Photoshop's 15-bit internal scale (0..32768), Studio unquantised;
/// 32-bit float: absolute difference <= 1e-4.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance;
impl Tolerance {
    pub const CODES_8: f64 = 1.0;
    pub const STEPS_15: f64 = 4.0;
    pub const FLOAT: f64 = 1e-4;
    pub fn describe() -> &'static str {
        "8-bit: |studio rounded to 8 bits - oracle| <= 1 code; 16-bit: <= 4 steps of Photoshop's 15-bit scale (studio unquantised); 32-bit float: <= 1e-4"
    }
}

/// How Studio's straight RGBA grid is turned into the oracle's stored samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Store {
    /// Oracle has an associated (premultiplied) alpha channel: compare `c * a` and `a`.
    Premultiplied,
    /// Oracle saved without transparency: Photoshop flattens onto white (`c a + 1 - a`).
    FlattenWhite,
    /// Compare straight RGB only (opaque scenes whose alpha is checked separately).
    Opaque,
    /// Four device channels (CMYK ink fractions, ICC convention 1 = full ink) as stored.
    Ink,
}

#[derive(Clone, Debug, Default)]
pub struct Delta {
    /// In 8-bit code units (difference * 255).
    pub max: f64,
    pub mean: f64,
    pub over: usize,
    pub total: usize,
    pub worst: Option<(u32, u32, usize, f64, f64)>,
}

/// Compares Studio's straight RGBA grid with the oracle image.
#[allow(clippy::needless_range_loop)] // the channel index addresses the oracle sample and the grid together
pub fn compare(studio: &[[f32; 4]], oracle: &Img, store: Store) -> Delta {
    let mut d = Delta::default();
    let mut sum = 0.0;
    let chans = if matches!(store, Store::Premultiplied | Store::Ink) { 4 } else { 3 };
    for y in 0..oracle.h {
        for x in 0..oracle.w {
            let p = studio[(y * oracle.w + x) as usize];
            let a = f64::from(p[3]);
            for c in 0..chans.min(oracle.ch) {
                let s = match (store, c) {
                    (Store::Ink, _) => f64::from(p[c]),
                    (_, 3) => a,
                    (Store::Premultiplied, _) => f64::from(p[c]) * a,
                    (Store::FlattenWhite, _) => f64::from(p[c]) * a + 1.0 - a,
                    (Store::Opaque, _) => f64::from(p[c]),
                };
                let o = oracle.at(x, y, c);
                let (diff, over) = if !s.is_finite() {
                    // A non-finite Studio sample never matches (counted at full scale).
                    (255.0, true)
                } else if oracle.float {
                    let diff = (s - o).abs();
                    (diff * 255.0, diff > Tolerance::FLOAT)
                } else if oracle.bits == 16 {
                    let steps = (s * 32768.0 - o * 32768.0).abs();
                    (steps * 255.0 / 32768.0, steps > Tolerance::STEPS_15)
                } else {
                    let codes = ((s * 255.0).round() - (o * 255.0).round()).abs();
                    (codes, codes > Tolerance::CODES_8)
                };
                sum += diff;
                d.total += 1;
                if over {
                    d.over += 1;
                }
                if diff > d.max {
                    d.max = diff;
                    d.worst = Some((x, y, c, s, o));
                }
            }
        }
    }
    d.mean = if d.total > 0 { sum / d.total as f64 } else { 0.0 };
    d
}

// ---- verdicts, expectations, report ---------------------------------------------------------------

/// What a case builder hands back for one variant.
pub enum Built {
    /// Render this scene and compare it as stored.
    Render { scene: Scene, store: Store, rows: &'static [&'static str] },
    /// Compare an already computed straight RGBA grid (a Studio engine call outside the plan path).
    /// `reference`: compare against this image (derived from the oracle file) instead of the file.
    Computed { px: Vec<[f32; 4]>, store: Store, rows: &'static [&'static str], engine: &'static str, reference: Option<Img> },
    /// Dissolve at partial coverage: Photoshop's generator is proprietary, so the per-pixel values
    /// cannot match; compared statistically instead (see [`dissolve_verdict`]).
    Dissolve { scene: Scene, backdrop: Vec<[f32; 3]>, source: Vec<[f32; 3]>, rows: &'static [&'static str] },
    /// No Studio scene for the variant (reason).
    NotSupported { reason: String, rows: &'static [&'static str] },
    /// The oracle output does not show the setting the case asked for (a script key Photoshop
    /// ignored, proven by byte-identical outputs); no evidence either way.
    OracleInvalid { reason: String, rows: &'static [&'static str] },
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub case_id: String,
    pub variant: String,
    pub verdict: &'static str,
    pub detail: String,
    pub delta: Option<Delta>,
    pub rows: Vec<String>,
    pub expected: Option<Expectation>,
}

#[derive(Clone, Debug)]
pub struct Expectation {
    pub case: String,
    pub variant: String,
    pub verdict: String,
    pub class: String,
    pub reason: String,
}
pub fn expectations() -> Vec<Expectation> {
    let v: Value = serde_json::from_str(EXPECTATIONS).expect("expectations.json parses");
    v["entries"]
        .as_array()
        .expect("expectations.json entries")
        .iter()
        .map(|e| Expectation {
            case: e["case"].as_str().unwrap_or_default().to_string(),
            variant: e["variant"].as_str().unwrap_or_default().to_string(),
            verdict: e["verdict"].as_str().unwrap_or_default().to_string(),
            class: e["class"].as_str().unwrap_or_default().to_string(),
            reason: e["reason"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}
/// `*` matches any run of characters, everything else literally.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Dissolve: every Photoshop and every Studio pixel must be either the backdrop or the full
/// source result (within 1 code), and the share of source pixels must agree within 0.02.
pub fn dissolve_verdict(studio: &[[f32; 4]], oracle: &Img, backdrop: &[[f32; 3]], source: &[[f32; 3]]) -> (bool, String) {
    let (mut ps_src, mut st_src, mut ps_bad, mut st_bad) = (0usize, 0usize, 0usize, 0usize);
    let n = (oracle.w * oracle.h) as usize;
    let close = |a: [f64; 3], b: [f32; 3]| (0..3).all(|c| ((a[c] * 255.0).round() - (f64::from(b[c]) * 255.0).round()).abs() <= 1.0);
    for i in 0..n {
        let (x, y) = (i as u32 % oracle.w, i as u32 / oracle.w);
        let o = [oracle.at(x, y, 0), oracle.at(x, y, 1), oracle.at(x, y, 2)];
        let s = [f64::from(studio[i][0]), f64::from(studio[i][1]), f64::from(studio[i][2])];
        let (ob, os) = (close(o, backdrop[i]), close(o, source[i]));
        let (sb, ss) = (close(s, backdrop[i]), close(s, source[i]));
        if os && !ob {
            ps_src += 1;
        }
        if ss && !sb {
            st_src += 1;
        }
        if !ob && !os {
            ps_bad += 1;
        }
        if !sb && !ss {
            st_bad += 1;
        }
    }
    let distinct = (0..n).filter(|i| !close([f64::from(source[*i][0]), f64::from(source[*i][1]), f64::from(source[*i][2])], backdrop[*i])).count().max(1);
    let (fp, fs) = (ps_src as f64 / distinct as f64, st_src as f64 / distinct as f64);
    let ok = ps_bad == 0 && st_bad == 0 && (fp - fs).abs() <= 0.02;
    (ok, format!("dissolve_stat source_share ps={fp:.4} studio={fs:.4} (of {distinct} distinguishable px) non_member ps={ps_bad} studio={st_bad} tolerance=0.02"))
}

/// Runs one family: every manifest entry whose case id is in `cases` goes through `build`.
/// Prints one line per variant, writes `<CARGO_TARGET_TMPDIR>/d5_ps_compare/<test>.json`, and
/// fails on any MISMATCH / NOT_SUPPORTED without a matching expectation.
pub fn run_family(test: &str, cases: &[&str], build: fn(&Corpus, &Entry) -> Result<Built, String>) {
    let corpus = match Corpus::open() {
        Ok(c) => c,
        Err(reason) => {
            println!("NOT_PROVEN: d5_ps_compare {test} reason={reason}");
            return;
        }
    };
    let entries: Vec<&Entry> = corpus.entries.iter().filter(|e| cases.contains(&e.case_id.as_str())).collect();
    if entries.is_empty() {
        println!("NOT_PROVEN: d5_ps_compare {test} reason=no_manifest_entries cases={cases:?}");
        return;
    }
    let exps = expectations();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(entry) = entries.get(i) else { break };
                let out = evaluate(&corpus, entry, build, &exps);
                results.lock().unwrap().push((i, out));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    let outcomes: Vec<Outcome> = results.into_iter().map(|(_, o)| o).collect();
    report(test, &outcomes, &exps, cases);
}

fn evaluate(corpus: &Corpus, entry: &Entry, build: fn(&Corpus, &Entry) -> Result<Built, String>, exps: &[Expectation]) -> Outcome {
    let mut out = Outcome {
        case_id: entry.case_id.clone(),
        variant: entry.variant.clone(),
        verdict: "NOT_PROVEN",
        detail: String::new(),
        delta: None,
        rows: Vec::new(),
        expected: None,
    };
    if entry.status != "ok" || entry.file.is_none() {
        out.verdict = "ORACLE_ABSENT";
        out.detail = format!("manifest status={}", entry.status);
        return out;
    }
    let file = entry.file.clone().unwrap_or_default();
    let oracle = match read_pinned(&corpus.dir, &file, &corpus.pins).and_then(|b| decode_tiff(&b)) {
        Ok(img) => img,
        Err(reason) => {
            out.detail = reason;
            return out;
        }
    };
    let built = match build(corpus, entry) {
        Ok(b) => b,
        Err(reason) => {
            out.detail = format!("builder_input {reason}");
            return out;
        }
    };
    match built {
        Built::NotSupported { reason, rows } => {
            out.rows = rows.iter().map(|r| r.to_string()).collect();
            out.verdict = "NOT_SUPPORTED";
            out.detail = reason;
        }
        Built::OracleInvalid { reason, rows } => {
            out.rows = rows.iter().map(|r| r.to_string()).collect();
            out.verdict = "ORACLE_INVALID";
            out.detail = reason;
        }
        Built::Render { scene, store, rows } => {
            out.rows = rows.iter().map(|r| r.to_string()).collect();
            match render(&at_document_depth(scene, entry)) {
                Err(key) => {
                    out.verdict = "NOT_SUPPORTED";
                    out.detail = format!("studio_refusal {key}");
                }
                Ok(px) => judge(&mut out, &px, &oracle, store, "render-cpu execute_extended"),
            }
        }
        Built::Computed { px, store, rows, engine, reference } => {
            out.rows = rows.iter().map(|r| r.to_string()).collect();
            judge(&mut out, &px, reference.as_ref().unwrap_or(&oracle), store, engine);
        }
        Built::Dissolve { scene, backdrop, source, rows } => {
            out.rows = rows.iter().map(|r| r.to_string()).collect();
            match render(&at_document_depth(scene, entry)) {
                Err(key) => {
                    out.verdict = "NOT_SUPPORTED";
                    out.detail = format!("studio_refusal {key}");
                }
                Ok(px) => {
                    let (ok, detail) = dissolve_verdict(&px, &oracle, &backdrop, &source);
                    out.verdict = if ok { "MATCH" } else { "MISMATCH" };
                    out.detail = detail;
                }
            }
        }
    }
    out.expected = exps
        .iter()
        .find(|x| x.case == entry.case_id && glob(&x.variant, &entry.variant))
        .cloned();
    out
}

/// The working depth of the oracle document (lane SMB-PS-ORACLE-PARITY-2): Photoshop's 8-bit
/// documents store every layer result in 8-bit levels and run Levels on whole levels, so the
/// variants of an 8-bit document render at pigment's `WorkingDepth::Eight` (16- and 32-bit ones
/// stay float).
fn at_document_depth(mut scene: Scene, entry: &Entry) -> Scene {
    if entry.bits.ends_with("EIGHT") {
        scene.ext.output = scene.ext.output.with_working_depth(WorkingDepth::Eight);
    }
    scene
}

/// Diagnostics only: with `HSK_D5_DUMP_DIR` set, Studio's straight RGBA grid of every compared
/// variant is written there as raw little-endian f32 (`<case>__<variant>__<w>x<h>.rgba32f`).
fn dump(out: &Outcome, px: &[[f32; 4]], oracle: &Img) {
    let Some(dir) = std::env::var_os("HSK_D5_DUMP_DIR") else { return };
    let mut bytes = Vec::with_capacity(px.len() * 16);
    for p in px {
        for v in p {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    let name = format!("{}__{}__{}x{}.rgba32f", out.case_id, out.variant, oracle.w, oracle.h);
    let _ = std::fs::write(PathBuf::from(dir).join(name), bytes);
}

fn judge(out: &mut Outcome, px: &[[f32; 4]], oracle: &Img, store: Store, engine: &str) {
    dump(out, px, oracle);
    if px.len() != (oracle.w * oracle.h) as usize {
        out.verdict = "MISMATCH";
        out.detail = format!("size studio={} oracle={}x{}", px.len(), oracle.w, oracle.h);
        return;
    }
    let d = compare(px, oracle, store);
    out.verdict = if d.over == 0 { "MATCH" } else { "MISMATCH" };
    out.detail = format!("engine={engine} store={store:?}");
    out.delta = Some(d);
}

fn report(test: &str, outcomes: &[Outcome], exps: &[Expectation], cases: &[&str]) {
    let mut failures = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut json_rows = Vec::new();
    for o in outcomes {
        *counts.entry(o.verdict).or_default() += 1;
        let d = o.delta.clone().unwrap_or_default();
        let worst = d.worst.map(|(x, y, c, s, ov)| format!(" worst=({x},{y},c{c}) studio={s:.5} oracle={ov:.5}")).unwrap_or_default();
        let exp = match (&o.expected, o.verdict) {
            (Some(e), v) if e.verdict == v => format!(" expected[{}]", e.class),
            // A glob entry describes a family; its members that match are evidence, not staleness.
            (Some(e), "MATCH") if e.variant.contains('*') => String::new(),
            (Some(e), "MATCH") => format!(" STALE_EXPECTATION[{} {}]", e.verdict, e.class),
            (Some(e), v) => {
                failures.push(format!("{}/{} {v} but expected {}", o.case_id, o.variant, e.verdict));
                format!(" UNEXPECTED(expected {})", e.verdict)
            }
            (None, "MISMATCH" | "NOT_SUPPORTED") => {
                failures.push(format!("{}/{} {} without expectation", o.case_id, o.variant, o.verdict));
                " UNEXPECTED".to_string()
            }
            _ => String::new(),
        };
        if o.verdict == "NOT_PROVEN" {
            println!("NOT_PROVEN: d5_ps_compare {test} {}/{} reason={}", o.case_id, o.variant, o.detail);
        }
        println!(
            "D5 {}/{} {} max={:.3} mean={:.4} over={}/{} rows={}{}{} | {}",
            o.case_id,
            o.variant,
            o.verdict,
            d.max,
            d.mean,
            d.over,
            d.total,
            o.rows.join(","),
            exp,
            worst,
            o.detail
        );
        json_rows.push(json!({
            "case": o.case_id, "variant": o.variant, "verdict": o.verdict,
            "max_delta_8bit_units": (d.max * 1000.0).round() / 1000.0,
            "mean_delta_8bit_units": (d.mean * 10000.0).round() / 10000.0,
            "samples_over_tolerance": d.over, "samples": d.total,
            "worst": d.worst.map(|(x, y, c, s, ov)| json!({"x": x, "y": y, "channel": c, "studio": s, "oracle": ov})),
            "rows": o.rows, "detail": o.detail,
            "expectation": o.expected.as_ref().map(|e| json!({"verdict": e.verdict, "class": e.class, "reason": e.reason})),
        }));
    }
    for e in exps.iter().filter(|e| cases.contains(&e.case.as_str())) {
        if !outcomes.iter().any(|o| o.case_id == e.case && glob(&e.variant, &o.variant)) {
            println!("STALE_EXPECTATION: {}/{} matches no manifest variant", e.case, e.variant);
        }
    }
    println!("D5 {test} totals {counts:?} tolerance: {}", Tolerance::describe());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("d5_ps_compare");
    let _ = std::fs::create_dir_all(&dir);
    let doc = json!({"schema": "hsk.studio.d5-ps-compare-results@1", "test": test, "tolerance": Tolerance::describe(), "counts": counts, "variants": json_rows});
    let _ = std::fs::write(dir.join(format!("{test}.json")), serde_json::to_vec_pretty(&doc).unwrap_or_default());
    assert!(failures.is_empty(), "{} unexpected outcome(s):\n{}", failures.len(), failures.join("\n"));
}

// ---- helpers shared by the case builders ---------------------------------------------------------

/// Photoshop blend-mode names (manifest `params.mode`, `BlendMode.*`) to Studio modes.
pub fn ps_mode(name: &str) -> Option<StudioBlendMode> {
    use StudioBlendMode::*;
    Some(match name.trim_start_matches("BlendMode.") {
        "NORMAL" => Normal,
        "DISSOLVE" => Dissolve,
        "DARKEN" => Darken,
        "MULTIPLY" => Multiply,
        "COLORBURN" => ColourBurn,
        "LINEARBURN" => LinearBurn,
        "DARKERCOLOR" => DarkerColour,
        "LIGHTEN" => Lighten,
        "SCREEN" => Screen,
        "COLORDODGE" => ColourDodge,
        "LINEARDODGE" => LinearDodgeAdd,
        "LIGHTERCOLOR" => LighterColour,
        "OVERLAY" => Overlay,
        "SOFTLIGHT" => SoftLight,
        "HARDLIGHT" => HardLight,
        "VIVIDLIGHT" => VividLight,
        "LINEARLIGHT" => LinearLight,
        "PINLIGHT" => PinLight,
        "HARDMIX" => HardMix,
        "DIFFERENCE" => Difference,
        "EXCLUSION" => Exclusion,
        "SUBTRACT" => Subtract,
        "DIVIDE" => Divide,
        "HUE" => Hue,
        "SATURATION" => Saturation,
        "COLORBLEND" => Colour,
        "LUMINOSITY" => Luminosity,
        _ => return None,
    })
}
/// Photoshop pastes a layer centred on the canvas: the top-left of a `w x h` paste.
pub fn paste_origin(doc_w: u32, doc_h: u32, w: u32, h: u32) -> (i64, i64) {
    ((i64::from(doc_w) - i64::from(w)) / 2, (i64::from(doc_h) - i64::from(h)) / 2)
}
