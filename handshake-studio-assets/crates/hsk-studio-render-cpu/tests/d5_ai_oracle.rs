//! D5 Illustrator oracle comparison (lane SMB-D5-AI-ID-AE-COMPARE): every case of
//! `oracle-session/hsk_oracle_illustrator.jsx` (AI-01..05) rebuilt with Studio's engines (nib stroke
//! outline, coverage rasteriser and paint; pigment blend, group and knockout maths) and compared
//! pixel by pixel with the TIFFs Illustrator 30.8.2 exported to `<oracle-corpus>/adobe-session/illustrator/`.
//!
//! Oracle facts measured while building this test (they define the comparison, not the verdicts):
//! * 72 ppi, 1 pt = 1 px; the export covers the artwork bounds, with the pixel grid anchored at the
//!   top of those bounds: a miter spike reaching page y = 136.154 makes the image 137 px tall and
//!   row r spans page y 136.154 - r - 1 .. 136.154 - r (sub-pixel shift; integer bottom alignment
//!   misplaces every edge by 0.154 px). Studio maps page y to pixel y as `top - y`, `top` = the
//!   largest page y of the artboard (128) and of Studio's own outline.
//! * `ARTOPTIMIZED` anti-aliasing is 4 x 4 point sampling at sub-pixel centres with 17 grey levels
//!   (grey = round(255 * (16 - samples) / 16): 255, 239, .., 143, 128, 112, .., 16, 0), and it counts a sample as painted when it
//!   lies inside the shape OR within a small distance of its outline: 0.17 px for fills (fitted on
//!   AI-02 fill_nonzero: 99.7 % of pixels exact versus 97.2 % without), 0.04 px for strokes (fitted
//!   on AI-01 cap_round_join_round_miter1: 99.7 % exact versus 98.1 %). Studio's own coverage is the
//!   exact pixel area (no bias), so its ink is 0.5 % (strokes) to 3.5 % (even-odd star) lighter.
//! * Geometry verdict (AI-01, AI-02): Studio's outline (nib stroke/dash outline, or the filled path
//!   under its fill rule) is sampled with the oracle's sampler above; MATCH when every pixel is within
//!   2 sample levels (32 grey), at least 99 % of pixels are identical and the summed ink agrees to
//!   0.2 %. The 31 variants not used for fitting are the independent check. The detail line also
//!   reports Studio's exact-coverage raster against the oracle (`native ...`).
//! * Colour cases (AI-03..05) are smooth or flat: tolerance 2 levels (two 8-bit roundings).
//!   Illustrator blends in higher precision than its 8-bit output: the AI-04 backdrop is the
//!   unquantised ramp value (ColorBurn amplifies an 8-bit backdrop step 12x).
//!
//! Verdicts are printed one per variant (`D5_VERDICT:`); a MISMATCH fails unless it is listed with
//! a reason in `tests/d5_ai_expected_mismatch.txt`.
mod d5_support;

use d5_support::{Ledger, Verdict, corpus_root, read_pinned};
use hsk_studio_nib::raster::{
    CoveragePlane, Gradient, GradientGeometry, GradientStop, IDENTITY, Interpolation, Paint, PixelFrame,
    RasterOptions, Spread, paint_coverage, rasterize,
};
use hsk_studio_nib::stroke::{DashPattern, LineCap, LineJoin, StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::blend_math::{
    BlendContext, BlendSpace, PixelSite, Rgba, StackNode, StudioBlendMode, composite_over, evaluate_stack,
};
use hsk_studio_pigment::knockout_math::knockout_over;
use std::path::Path;

const DIR: &str = "adobe-session/illustrator";
const EXPECTED: &str = include_str!("d5_ai_expected_mismatch.txt");
const EDGE_MAX: i32 = 32;
const EXACT_MIN: f64 = 0.99;
const INK_REL: f64 = 0.002;
const COLOUR_MAX: i32 = 2;
/// Oracle sampler bias (px), see module docs.
const BIAS_FILL: f64 = 0.17;
const BIAS_STROKE: f64 = 0.04;

/// SHA-256 of every Illustrator export read (session 2026-10-16, Illustrator 30.8.2).
const PINS: &[(&str, &str)] = &[
    ("AI-01__cap_butt_join_bevel_miter1.tif", "e763f18a79d053e789d32e589f87cd3207524e853283f3294d0e66e9eba27ad2"),
    ("AI-01__cap_butt_join_bevel_miter10.tif", "b773f4388bd3f731b9935d9485d88bb3843ed0848458f105e553ce0a2172a657"),
    ("AI-01__cap_butt_join_bevel_miter4.tif", "005b1eed383ec475f0f6a5074b0f389d85d4c207e4923a13f2cd348c6a107145"),
    ("AI-01__cap_butt_join_miter_miter1.tif", "d8e295dfca7559cbd3128d566b91b1997596a4b1313079a6d7ca6915c76763d1"),
    ("AI-01__cap_butt_join_miter_miter10.tif", "a72e9f3610dd786d3f5713488b3b7a2e765dd0852ef3a63c44d9319d5934a0e8"),
    ("AI-01__cap_butt_join_miter_miter1_aa_none.tif", "f9a93b853fa30a08fd6e7b2aa3f4e5ae5969b7b384a799f0e13dc45418111390"),
    ("AI-01__cap_butt_join_miter_miter4.tif", "f4b0a5d1c36733194ec2c6b2727f1fd2f363a79811c8788efd490789db976827"),
    ("AI-01__cap_butt_join_round_miter1.tif", "77f43738160768837723f425ea9530c6063f06918881ecde96dfd03f6e974d4a"),
    ("AI-01__cap_butt_join_round_miter10.tif", "7deb9351a97622626eb4b918533b809b7971485955896ca597f528778c9ff86d"),
    ("AI-01__cap_butt_join_round_miter4.tif", "a25c4ea7f35bf49412a40765e6e3f93d78acd1f11ae50c326a3feab5161f2557"),
    ("AI-01__cap_projecting_join_bevel_miter1.tif", "3e8e49a84955ef1bb9822fdc8e63a7e08b0019b78a21f4869b7c46ae3f201696"),
    ("AI-01__cap_projecting_join_bevel_miter10.tif", "b2ad682617b31ea34432871a735e82034ecc53044f1e1609cd2b378e90b17a28"),
    ("AI-01__cap_projecting_join_bevel_miter4.tif", "ef7a885d15ce7a3265c1db1747c3ac44058d19af174c1eec360df5fac9dbe92e"),
    ("AI-01__cap_projecting_join_miter_miter1.tif", "346fcbf8097518cf3c3b55e95506f01846fb5c67fd6d07744231a939be7dc838"),
    ("AI-01__cap_projecting_join_miter_miter10.tif", "0bc786f46cc74bf1141ed59a964231781a5ab46e94c93d4effaea33eb94702b9"),
    ("AI-01__cap_projecting_join_miter_miter4.tif", "69f71485824559fe5774f3ad6c00a69016a979f8c7556c9b5c7259a39cc1c235"),
    ("AI-01__cap_projecting_join_round_miter1.tif", "fba2230c769536cbff8c772384c49beaa45e4d6b3b6f1bdfab2cd1537f4da6d5"),
    ("AI-01__cap_projecting_join_round_miter10.tif", "384b2afff88e44ab6bc1028f1460eb0795edbce679f78610b2a7d70bc4b274f6"),
    ("AI-01__cap_projecting_join_round_miter4.tif", "78714e8b3a1d18e8819d59c0abaa8f825f6c697acf3179512be995bdc6c62a1a"),
    ("AI-01__cap_round_join_bevel_miter1.tif", "0e7531f4db8724da5255f42b451a9f51d31e2fe12b561836cad08c861acc8cd3"),
    ("AI-01__cap_round_join_bevel_miter10.tif", "695c64592365f59e84ea876b84beb4b41fce8017efb732349a9aafa7e5893624"),
    ("AI-01__cap_round_join_bevel_miter4.tif", "948c1cd8204ec4617115da593cf1a94e2fde327037c4d147468477376034bafb"),
    ("AI-01__cap_round_join_miter_miter1.tif", "46c6e3c24607a449e5a673e94cf4b2dc6765da2619bd2982c6725dc8589f9c0b"),
    ("AI-01__cap_round_join_miter_miter10.tif", "c7df28ef3b4ff67d6c88a501507a583371e4b8f3328adf5709cbc921cc2851f3"),
    ("AI-01__cap_round_join_miter_miter4.tif", "50a2eeb8044ec6fcd63d054f13a10faf531e523de413d19004cf3c2ba49b9ead"),
    ("AI-01__cap_round_join_round_miter1.tif", "a751fbb43f832e3b60eb15a0d981ae2e2055f4f956e1cb8b8c5316c9ec7b9836"),
    ("AI-01__cap_round_join_round_miter10.tif", "9f87a152c7d1652e1eb991050501a7422c818ecd4b6e598506a0e39686d1c92f"),
    ("AI-01__cap_round_join_round_miter4.tif", "600747ef9ea6791a50d4e2e049d258360427ca74c719d9b6d49ffa40b4cd5403"),
    ("AI-02__dash_d0_10_round.tif", "427dcec510c35fe9973ac83fa3c41db26a65019d625032d1bc9c3b907a28ae71"),
    ("AI-02__dash_d12_6.tif", "19ffe02791bb1c0fb99beddd706dc277b7719a7b8ab4a68785f45d2e4f3482db"),
    ("AI-02__dash_d12_6_off5.tif", "0e953fc634e78272d0ae631298bfaa1b5c4039c53cef85c01923f5d573503ac3"),
    ("AI-02__dash_d6_6_6_12.tif", "2aae0192bf942c753b40e6c52146c8f2dfe9d3fbaa853896be8ed01a70ee1adf"),
    ("AI-02__fill_evenodd.tif", "3be60a61060f8e6fe1bcde81b4947b6f59edb43f9dbf9e4f6aec6adc2d0b82b0"),
    ("AI-02__fill_nonzero.tif", "363eb4ad92e0c2b4acf6e339c9658360ed7fd4161a0d7b623ceced91b5a251ad"),
    ("AI-03__mid25.tif", "797eb0652a11bbbee7b059434fead779517ebea54e648b3d9094b037521c6e96"),
    ("AI-03__mid50.tif", "6c377ea5b35d65e3b932b205cab73a3ae385bcde00f8383acf60eac0b7ec59eb"),
    ("AI-03__mid75.tif", "92be11063ce4c1fb9deb6be443f086661b7c1a49d81dc881153870620aa18e2f"),
    ("AI-03__radial_black_white.tif", "b9082bd1954e244ea700432b3a31c4b9e97c09c9d40bc7f6e0d9f4187aa100d9"),
    ("AI-03__stop_opacity_50_over_red.tif", "e7ebea18555cdd0f70fa4ad5f0c45281be14d9ba59fe2f33d1c42c9f934eaa6e"),
    ("AI-04__COLORBLEND_o100.tif", "5a948c16452905c751b58c34292d06a12786df1b7150d0a7477dd2aaf694bbff"),
    ("AI-04__COLORBLEND_o50.tif", "a7243aa9217bdeb50370d467d3e34e41077b31446759da53d36352843e940ce1"),
    ("AI-04__COLORBURN_o100.tif", "5a8765c37581f4d5e30a442f1bfbc9670c587824310ffc17c81385a96223e939"),
    ("AI-04__COLORBURN_o50.tif", "a5ace29c11f18a774f06208bb78685cffecee5a5aafe917a3823c36c076d0afe"),
    ("AI-04__COLORDODGE_o100.tif", "0c747343742f4129d875ceaa3f340c6690fa32336f6ad73be8e7642e2b322140"),
    ("AI-04__COLORDODGE_o50.tif", "0c9a73a85c3700e4969e2072f77e7f906c05c0ac85940fb85699cdbd53dd5d97"),
    ("AI-04__DARKEN_o100.tif", "ff5ebea3d5c2535a675090d7261341cf76cad2411b2ffea6ef0504f311a06ced"),
    ("AI-04__DARKEN_o50.tif", "b3391109299335b86a9c5f1724cb74a48e4f85e551dee49b4920b7bce60e0e52"),
    ("AI-04__DIFFERENCE_o100.tif", "84011a633280a20d6b153746b9d41dbbdca2a05bfae027240839c76acfeeb7fc"),
    ("AI-04__DIFFERENCE_o50.tif", "d03e1a538fc484433f804cc5a66a940bb4aed753ea23c7c13ec9a54253baa9a0"),
    ("AI-04__EXCLUSION_o100.tif", "baacb546a0125e789882eeef8b8650d0b6c102cbcd659c931ac44ffc35f739e7"),
    ("AI-04__EXCLUSION_o50.tif", "c78219763f6449b38ad5d93abda29e54ebc644ba02b53d9e30417b1cd0c85b84"),
    ("AI-04__HARDLIGHT_o100.tif", "5e37731bdf78461a7232cfaa844fc684b516997e7b43917dff834863816cf1de"),
    ("AI-04__HARDLIGHT_o50.tif", "be67331cd01f77926478ce922af808dbc8f5d04585373fccb0fc9a472b0c829b"),
    ("AI-04__HUE_o100.tif", "fade506d12a5c9440dc099abca6fc2773218439e658cac337f53bc1d4862e118"),
    ("AI-04__HUE_o50.tif", "9dd50f84164bb5a82356601376691abce5d204c8a7a5efa07b6c4d000740fd1f"),
    ("AI-04__LIGHTEN_o100.tif", "58b82afd81d0e55a722b3bd1f77fd30b241852414cdfbfbcdb59bf85a2212d96"),
    ("AI-04__LIGHTEN_o50.tif", "f55d1b81c6e1e83ababd47fe235e44adbcc252e63662fa8ae5492add555129d5"),
    ("AI-04__LUMINOSITY_o100.tif", "e41fd53dd01e324ad9de645cd1960c83f7db50723185092f076f9aa1998df769"),
    ("AI-04__LUMINOSITY_o50.tif", "124ee6d63a3ffe7c9ad07079ac09b1f1688268ee805074f254addd56f687f52f"),
    ("AI-04__MULTIPLY_o100.tif", "65f03baf6197caf43472b1ba265e730b9f0e6de4f824906f6ccf391d8e5fecc8"),
    ("AI-04__MULTIPLY_o50.tif", "79665571d7a3b4a06fb98c9d416d91ce63b747d9fbd2676c04ec4b42b16b567b"),
    ("AI-04__NORMAL_o100.tif", "e5c3136e6ba56428b60c1effc5a805ca63e00035b641635838c67910a9065608"),
    ("AI-04__NORMAL_o50.tif", "b567df416f457c4539b8192dcb14fd68873d5cb1f3eafe4712d775acce0286d2"),
    ("AI-04__OVERLAY_o100.tif", "d2058c4f3501e8b0de88ac5519c65da02222442143189fb4caca5ebc3c97d9df"),
    ("AI-04__OVERLAY_o50.tif", "821e86c0ada6fd343884bd93153cb2e3f97669ea434bdedb46096098d5d0b1d4"),
    ("AI-04__SATURATIONBLEND_o100.tif", "683cda9598aac0974f241ba0e96d5a8aea31d85f27d23a39e90b5c9240117ebf"),
    ("AI-04__SATURATIONBLEND_o50.tif", "2e288295a74380de0ca21880fbd33e6616269bd7cb9ac9b243b2e89c24d977df"),
    ("AI-04__SCREEN_o100.tif", "61426766b7777c5fe4fa84d9244318843b5bf1316b1c486c5a1e432525622838"),
    ("AI-04__SCREEN_o50.tif", "41f970a7ae067158c2f306b9ad3c065d820df6c9228f8f06e290bae56b4dbcbd"),
    ("AI-04__SOFTLIGHT_o100.tif", "6f7f74012c5c5f02b91dc3852285a2bf8a4c611d7f2475639872b87cfd85ccb0"),
    ("AI-04__SOFTLIGHT_o50.tif", "5674fb8c838770d0d1903f7230ddaf59e2e058e74a58005b8b1cc037d68afb0a"),
    ("AI-05__iso_off_ko_disabled.tif", "3ce1ed99e24382df24e5d6677678691899b680180d1d4d1ea4f9c76b17f6ea70"),
    ("AI-05__iso_off_ko_enabled.tif", "cf05765e7ab34a31bbea0b2ab25eb1ff21e74b64080c55151fe4b72156514c30"),
    ("AI-05__iso_on_ko_disabled.tif", "264c75614b553fbe49eb6bc2d1f85153c88644ecd831032520b858b0d6fc1a7a"),
    ("AI-05__iso_on_ko_enabled.tif", "ae1c2b042507371b34e0a16435a6b2b2e6c89d4776b2ded6631492b96770cdac"),
];

// ---------------------------------------------------------------------------------------------
// Minimal TIFF reader for the oracle exports: little-endian baseline TIFF, 8-bit RGB, chunky,
// uncompressed strips. Anything else is refused (bounded, checked arithmetic).
struct Image {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn rd16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at.checked_add(1)?)?]))
}
fn rd32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_tiff(b: &[u8]) -> Result<Image, String> {
    if b.get(0..4) != Some(b"II*\0".as_slice()) {
        return Err("not a little-endian TIFF".into());
    }
    let ifd = rd32(b, 4).ok_or("ifd offset")? as usize;
    let n = rd16(b, ifd).ok_or("ifd count")? as usize;
    if n > 512 {
        return Err("ifd too long".into());
    }
    let (mut w, mut h, mut spp, mut comp, mut planar) = (0usize, 0usize, 1usize, 1u32, 1u32);
    let mut bits = Vec::new();
    let (mut offsets, mut counts) = (Vec::new(), Vec::new());
    for i in 0..n {
        let e = ifd.checked_add(2).and_then(|x| x.checked_add(i.checked_mul(12)?)).ok_or("entry offset")?;
        let tag = rd16(b, e).ok_or("tag")?;
        let typ = rd16(b, e + 2).ok_or("type")?;
        let cnt = rd32(b, e + 4).ok_or("count")? as usize;
        if cnt > 1 << 20 {
            return Err("tag count".into());
        }
        let size = match typ {
            3 => 2usize,
            4 => 4,
            _ => 0,
        };
        let values = |b: &[u8]| -> Option<Vec<u32>> {
            if size == 0 {
                return Some(Vec::new());
            }
            let total = size.checked_mul(cnt)?;
            let base = if total <= 4 { e + 8 } else { rd32(b, e + 8)? as usize };
            (0..cnt)
                .map(|k| {
                    let at = base.checked_add(k.checked_mul(size)?)?;
                    if size == 2 { rd16(b, at).map(u32::from) } else { rd32(b, at) }
                })
                .collect()
        };
        let v = values(b).ok_or("tag values")?;
        let first = v.first().copied().unwrap_or(0);
        match tag {
            256 => w = first as usize,
            257 => h = first as usize,
            258 => bits = v,
            259 => comp = first,
            273 => offsets = v,
            277 => spp = first as usize,
            279 => counts = v,
            284 => planar = first,
            _ => {}
        }
    }
    if comp != 1 || planar != 1 || spp != 3 || bits.iter().any(|x| *x != 8) || w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("unsupported tiff w={w} h={h} spp={spp} comp={comp} planar={planar} bits={bits:?}"));
    }
    let need = w.checked_mul(h).and_then(|x| x.checked_mul(3)).ok_or("size")?;
    let mut rgb = Vec::with_capacity(need);
    for (o, c) in offsets.iter().zip(&counts) {
        let (o, c) = (*o as usize, *c as usize);
        rgb.extend_from_slice(b.get(o..o.checked_add(c).ok_or("strip")?).ok_or("strip out of file")?);
    }
    if rgb.len() < need {
        return Err("short pixel data".into());
    }
    rgb.truncate(need);
    Ok(Image { width: w, height: h, rgb })
}

fn pin(file: &str) -> Option<&'static str> {
    PINS.iter().find(|(f, _)| *f == file).map(|(_, h)| *h)
}

/// Loads one export, or records NOT_PROVEN for the variant and returns `None`.
fn load(root: &Path, ledger: &mut Ledger, case: &str, variant: &str) -> Option<Image> {
    let file = format!("{case}__{variant}.tif");
    let Some(hash) = pin(&file) else {
        ledger.record(case, variant, Verdict::NotProven, format!("{file} has no pin"));
        return None;
    };
    match read_pinned(root, DIR, &file, hash).and_then(|b| read_tiff(&b)) {
        Ok(img) => Some(img),
        Err(reason) => {
            ledger.record(case, variant, Verdict::NotProven, reason);
            None
        }
    }
}

fn root_or_not_proven(test: &str) -> Option<std::path::PathBuf> {
    let root = corpus_root();
    if root.is_none() {
        println!("NOT_PROVEN: {test} reason=oracle-corpus root not found via .config/oracle-corpus.toml");
    }
    root
}

// ---------------------------------------------------------------------------------------------
// Geometry helpers (page space: origin bottom-left, y up; converted to pixel space y down).
fn poly(points: &[[f64; 2]], height: f64, closed: bool, rule: FillRule) -> OwnedPath {
    let mut path = OwnedPath::new(rule);
    for (i, p) in points.iter().enumerate() {
        let q = PathPoint::new(p[0], height - p[1]);
        path.elements.push(if i == 0 { PathElement::MoveTo(q) } else { PathElement::LineTo(q) });
    }
    if closed {
        path.elements.push(PathElement::Close);
    }
    path
}

fn stroked(path: &OwnedPath, style: &StrokeStyle) -> OwnedPath {
    outline_stroke(path, style, &StrokeOptions::default()).expect("nib stroke outline").path
}

fn union_of(parts: &[OwnedPath]) -> OwnedPath {
    let mut out = OwnedPath::new(FillRule::NonZero);
    for p in parts {
        out.elements.extend(p.elements.iter().cloned());
    }
    out
}

fn coverage(path: &OwnedPath, w: usize, h: usize) -> CoveragePlane {
    let frame = PixelFrame { x: 0, y: 0, width: w as u32, height: h as u32 };
    rasterize(path, frame, &RasterOptions::default()).expect("nib rasterize").0
}

/// Polygon edges `[x0, y0, x1, y1]` of a path made of MoveTo / LineTo / Close (nib outlines are
/// polygonal); every subpath is closed implicitly, as a fill closes it.
fn edges(path: &OwnedPath) -> Vec<[f64; 4]> {
    let mut out = Vec::new();
    let (mut first, mut last) = (None::<PathPoint>, None::<PathPoint>);
    let close = |out: &mut Vec<[f64; 4]>, a: Option<PathPoint>, b: Option<PathPoint>| {
        if let (Some(a), Some(b)) = (a, b) {
            out.push([a.x, a.y, b.x, b.y]);
        }
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(p) => {
                close(&mut out, last, first);
                first = Some(p);
                last = Some(p);
            }
            PathElement::LineTo(p) => {
                if let Some(a) = last {
                    out.push([a.x, a.y, p.x, p.y]);
                }
                last = Some(p);
            }
            PathElement::Close => {
                close(&mut out, last, first);
                last = None;
                first = None;
            }
            _ => panic!("polygonal outline expected"),
        }
    }
    close(&mut out, last, first);
    out.retain(|e| e[0] != e[2] || e[1] != e[3]);
    out
}

fn seg_dist(e: &[f64; 4], x: f64, y: f64) -> f64 {
    let (vx, vy) = (e[2] - e[0], e[3] - e[1]);
    let len2 = vx * vx + vy * vy;
    let t = (((x - e[0]) * vx + (y - e[1]) * vy) / len2).clamp(0.0, 1.0);
    ((x - e[0] - t * vx).powi(2) + (y - e[1] - t * vy).powi(2)).sqrt()
}

/// The oracle's sampler (module docs) applied to Studio's geometry: grey per pixel.
fn oracle_sampler(path: &OwnedPath, rule: FillRule, w: usize, h: usize, bias: f64) -> Vec<u8> {
    let es = edges(path);
    let mut out = vec![255u8; w * h];
    for row in 0..h {
        let mut counts = vec![0u8; w];
        for j in 0..4 {
            let y = row as f64 + (f64::from(j) + 0.5) / 4.0;
            // Crossings of the scanline with their winding direction, sorted by x.
            let mut cross: Vec<(f64, i32)> = es
                .iter()
                .filter(|e| (e[1] <= y) != (e[3] <= y))
                .map(|e| (e[0] + (y - e[1]) / (e[3] - e[1]) * (e[2] - e[0]), if e[3] > e[1] { 1 } else { -1 }))
                .collect();
            cross.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Edges within `bias` of this scanline, bucketed by the columns their part inside the
            // band y +- bias can reach (keeps the distance test local).
            let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); w];
            for (k, e) in es.iter().enumerate() {
                let (ylo, yhi) = (y - bias, y + bias);
                if e[1].max(e[3]) < ylo || e[1].min(e[3]) > yhi {
                    continue;
                }
                let (mut t0, mut t1) = (0.0f64, 1.0f64);
                let dy = e[3] - e[1];
                if dy != 0.0 {
                    let (a, b) = ((ylo - e[1]) / dy, (yhi - e[1]) / dy);
                    t0 = t0.max(a.min(b));
                    t1 = t1.min(a.max(b));
                }
                let (xa, xb) = (e[0] + t0 * (e[2] - e[0]), e[0] + t1 * (e[2] - e[0]));
                let lo = (xa.min(xb) - bias).floor().max(0.0) as usize;
                let hi = ((xa.max(xb) + bias).floor().max(0.0) as usize).min(w.saturating_sub(1));
                for bucket in buckets.iter_mut().take(hi + 1).skip(lo) {
                    bucket.push(k);
                }
            }
            for (col, count) in counts.iter_mut().enumerate() {
                for i in 0..4 {
                    let x = col as f64 + (f64::from(i) + 0.5) / 4.0;
                    let wind: i32 = cross.iter().take_while(|c| c.0 < x).map(|c| c.1).sum();
                    let inside = match rule {
                        FillRule::EvenOdd => wind % 2 != 0,
                        _ => wind != 0,
                    };
                    let hit = inside || buckets[col].iter().any(|k| seg_dist(&es[*k], x, y) < bias);
                    if hit {
                        *count += 1;
                    }
                }
            }
        }
        for (col, c) in counts.iter().enumerate() {
            // Measured levels: 255, 239, 223, .., 143, 128, 112, .., 16, 0 = round(255 * (16 - c) / 16).
            out[row * w + col] = (255.0 * f64::from(16 - *c) / 16.0).round() as u8;
        }
    }
    out
}

/// Grey predictions against the oracle's red channel (the export is neutral grey: R = G = B
/// checked): (max abs, fraction identical, ink rel, mean abs).
fn grey_stats(pred: &[u8], img: &Image) -> (i32, f64, f64, f64) {
    let n = img.width * img.height;
    let (mut max, mut same, mut sum, mut ink_s, mut ink_o) = (0i32, 0usize, 0i64, 0i64, 0i64);
    for (i, p) in pred.iter().enumerate().take(n) {
        let px = &img.rgb[i * 3..i * 3 + 3];
        assert!(px[0] == px[1] && px[1] == px[2], "grey export expected");
        let (o, s) = (i32::from(px[0]), i32::from(*p));
        let d = (s - o).abs();
        max = max.max(d);
        same += usize::from(d == 0);
        sum += i64::from(d);
        ink_s += i64::from(255 - s);
        ink_o += i64::from(255 - o);
    }
    (max, same as f64 / n as f64, (ink_s - ink_o) as f64 / ink_o.max(1) as f64, sum as f64 / n as f64)
}

/// Geometry verdict of `path` (pixel space) against one export: the oracle sampler on Studio's
/// geometry decides; Studio's exact coverage is reported alongside.
fn geometry_verdict(path: &OwnedPath, rule: FillRule, bias: f64, img: &Image) -> (Verdict, String) {
    let sampled = oracle_sampler(path, rule, img.width, img.height, bias);
    let (max, exact, ink, mean) = grey_stats(&sampled, img);
    let native: Vec<u8> = coverage(path, img.width, img.height).data.iter().map(|c| 255 - c).collect();
    let (nmax, nexact, nink, nmean) = grey_stats(&native, img);
    let ok = max <= EDGE_MAX && exact >= EXACT_MIN && ink.abs() <= INK_REL;
    (
        if ok { Verdict::Match } else { Verdict::Mismatch },
        format!(
            "sampled max_abs={max} exact={exact:.4} ink_rel={ink:+.5} mean_abs={mean:.3}; native max_abs={nmax} exact={nexact:.4} ink_rel={nink:+.5} mean_abs={nmean:.3}; size={}x{}",
            img.width, img.height
        ),
    )
}

/// Largest page y of a path built with `top = 0` (`y_px = -y_page`).
fn max_page_y(path: &OwnedPath) -> f64 {
    path.elements
        .iter()
        .filter_map(|e| match e {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => Some(-p.y),
            _ => None,
        })
        .fold(f64::NEG_INFINITY, f64::max)
}

fn colour_verdict(pred: &[[u8; 3]], img: &Image) -> (Verdict, String) {
    let mut max = 0i32;
    let mut sum = 0i64;
    let mut worst = (0usize, [0u8; 3], [0u8; 3]);
    for (i, p) in pred.iter().enumerate() {
        let o = [img.rgb[i * 3], img.rgb[i * 3 + 1], img.rgb[i * 3 + 2]];
        for c in 0..3 {
            let d = (i32::from(p[c]) - i32::from(o[c])).abs();
            sum += i64::from(d);
            if d > max {
                max = d;
                worst = (i, *p, o);
            }
        }
    }
    let mean = sum as f64 / (pred.len() * 3) as f64;
    let (x, y) = (worst.0 % img.width, worst.0 / img.width);
    (
        if max <= COLOUR_MAX { Verdict::Match } else { Verdict::Mismatch },
        format!("max_abs={max} mean_abs={mean:.3} worst_px=({x},{y}) studio={:?} ai={:?}", worst.1, worst.2),
    )
}

fn q8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

// ---------------------------------------------------------------------------------------------
/// AI-01: caps x joins x miter limits (27) on an open zig-zag and a closed triangle, 14 pt, plus
/// the aliased (`antiAliasing None`) export of the first variant.
#[test]
fn ai01_caps_joins_miters() {
    const T: &str = "d5_ai_oracle::ai01_caps_joins_miters";
    let Some(root) = root_or_not_proven(T) else { return };
    let mut ledger = Ledger::new(T, EXPECTED);
    let caps = [("butt", LineCap::Butt), ("round", LineCap::Round), ("projecting", LineCap::Square)];
    let joins = [("miter", LineJoin::Miter), ("round", LineJoin::Round), ("bevel", LineJoin::Bevel)];
    for (cn, cap) in caps {
        for (jn, join) in joins {
            for ml in [1.0f64, 4.0, 10.0] {
                let variant = format!("cap_{cn}_join_{jn}_miter{ml}");
                let Some(img) = load(&root, &mut ledger, "AI-01", &variant) else { continue };
                let style = StrokeStyle { cap, join, miter_limit: ml, ..StrokeStyle::new(14.0) };
                let outline = |top: f64| {
                    let zig = poly(&[[20.0, 30.0], [70.0, 100.0], [120.0, 30.0], [170.0, 100.0]], top, false, FillRule::NonZero);
                    let tri = poly(&[[190.0, 20.0], [240.0, 20.0], [215.0, 110.0]], top, true, FillRule::NonZero);
                    union_of(&[stroked(&zig, &style), stroked(&tri, &style)])
                };
                let top = max_page_y(&outline(0.0)).max(128.0);
                let path = outline(top);
                let cov = coverage(&path, img.width, img.height);
                let (v, detail) = geometry_verdict(&path, FillRule::NonZero, BIAS_STROKE, &img);
                ledger.record("AI-01", &variant, v, format!("{detail} top={top:.3}"));
                if (cn, jn, ml) == ("butt", "miter", 1.0)
                    && let Some(alias) = load(&root, &mut ledger, "AI-01", &format!("{variant}_aa_none"))
                {
                    // Studio's rasteriser has no aliased mode: report how many pixels a 50 %
                    // coverage threshold gets right.
                    let n = alias.width * alias.height;
                    let agree = (0..n).filter(|i| (cov.data[*i] >= 128) == (alias.rgb[i * 3] < 128)).count();
                    ledger.record("AI-01", &format!("{variant}_aa_none"), Verdict::NotSupported,
                        format!("no aliased raster mode in nib; 50% threshold agrees on {agree}/{n} px"));
                }
            }
        }
    }
    ledger.finish();
}

/// AI-02: dash arrays / offset on an 8 pt line, and even-odd vs non-zero on a pentagram.
#[test]
fn ai02_dashes_and_fill_rules() {
    const T: &str = "d5_ai_oracle::ai02_dashes_and_fill_rules";
    let Some(root) = root_or_not_proven(T) else { return };
    let mut ledger = Ledger::new(T, EXPECTED);
    let dashes: [(&str, &[f64], f64); 4] =
        [("d12_6", &[12.0, 6.0], 0.0), ("d12_6_off5", &[12.0, 6.0], 5.0), ("d6_6_6_12", &[6.0, 6.0, 6.0, 12.0], 0.0), ("d0_10_round", &[0.0, 10.0], 0.0)];
    for (name, lengths, phase) in dashes {
        let variant = format!("dash_{name}");
        let Some(img) = load(&root, &mut ledger, "AI-02", &variant) else { continue };
        let h = img.height as f64;
        let cap = if name.contains("round") { LineCap::Round } else { LineCap::Butt };
        let style = StrokeStyle { cap, dash: Some(DashPattern { lengths: lengths.to_vec(), phase }), ..StrokeStyle::new(8.0) };
        let line = poly(&[[10.0, 32.0], [246.0, 32.0]], h, false, FillRule::NonZero);
        let (v, detail) = geometry_verdict(&stroked(&line, &style), FillRule::NonZero, BIAS_STROKE, &img);
        ledger.record("AI-02", &variant, v, detail);
    }
    for (name, rule) in [("evenodd", FillRule::EvenOdd), ("nonzero", FillRule::NonZero)] {
        let variant = format!("fill_{name}");
        let Some(img) = load(&root, &mut ledger, "AI-02", &variant) else { continue };
        let h = img.height as f64;
        let pts: Vec<[f64; 2]> = (0..5)
            .map(|k| {
                let a = -std::f64::consts::FRAC_PI_2 + f64::from(k) * 4.0 * std::f64::consts::PI / 5.0;
                [64.0 + 56.0 * a.cos(), 64.0 - 56.0 * a.sin()]
            })
            .collect();
        let (v, detail) = geometry_verdict(&poly(&pts, h, true, rule), rule, BIAS_FILL, &img);
        ledger.record("AI-02", &variant, v, detail);
    }
    ledger.finish();
}

fn gradient(geometry: GradientGeometry, stops: Vec<GradientStop>) -> Paint {
    Paint::Gradient(Gradient { geometry, transform: IDENTITY, spread: Spread::Pad, interpolation: Interpolation::Classic, dither: false, stops })
}

/// Paints `paint` over a fully covered `w x h` plane and composites it over the opaque `under`.
fn paint_over(paint: &Paint, w: usize, h: usize, under: [f32; 3]) -> Vec<[u8; 3]> {
    let frame = PixelFrame { x: 0, y: 0, width: w as u32, height: h as u32 };
    let full = CoveragePlane { frame, data: vec![255; w * h] };
    let painted = paint_coverage(&full, paint).expect("nib paint");
    painted.data.iter().map(|p| [q8(p[0] + under[0] * (1.0 - p[3])), q8(p[1] + under[1] * (1.0 - p[3])), q8(p[2] + under[2] * (1.0 - p[3]))]).collect()
}

/// AI-03: linear black -> white with midpoint 25/50/75, a radial ramp, and a stop-opacity ramp
/// over red.
#[test]
fn ai03_gradients() {
    const T: &str = "d5_ai_oracle::ai03_gradients";
    let Some(root) = root_or_not_proven(T) else { return };
    let mut ledger = Ledger::new(T, EXPECTED);
    let black = [0.0, 0.0, 0.0, 1.0];
    let white = [1.0, 1.0, 1.0, 1.0];
    for mid in [25u32, 50, 75] {
        let variant = format!("mid{mid}");
        let Some(img) = load(&root, &mut ledger, "AI-03", &variant) else { continue };
        let stops = vec![
            GradientStop { offset: 0.0, colour: black, midpoint: Some(f64::from(mid) / 100.0) },
            GradientStop { offset: 1.0, colour: white, midpoint: None },
        ];
        let paint = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 256.0, y2: 0.0 }, stops);
        let (v, detail) = colour_verdict(&paint_over(&paint, img.width, img.height, [1.0; 3]), &img);
        ledger.record("AI-03", &variant, v, detail);
    }
    if let Some(img) = load(&root, &mut ledger, "AI-03", "radial_black_white") {
        let stops = vec![GradientStop { offset: 0.0, colour: black, midpoint: None }, GradientStop { offset: 1.0, colour: white, midpoint: None }];
        let paint = gradient(GradientGeometry::Radial { cx: 64.0, cy: 64.0, r: 64.0, fx: 64.0, fy: 64.0, fr: 0.0 }, stops);
        let (v, detail) = colour_verdict(&paint_over(&paint, img.width, img.height, [1.0; 3]), &img);
        ledger.record("AI-03", "radial_black_white", v, detail);
    }
    if let Some(img) = load(&root, &mut ledger, "AI-03", "stop_opacity_50_over_red") {
        let stops = vec![
            GradientStop { offset: 0.0, colour: [0.0, 0.0, 1.0, 1.0], midpoint: None },
            GradientStop { offset: 1.0, colour: [0.0, 0.0, 1.0, 0.0], midpoint: None },
        ];
        let paint = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 256.0, y2: 0.0 }, stops);
        let (v, detail) = colour_verdict(&paint_over(&paint, img.width, img.height, [1.0, 0.0, 0.0]), &img);
        ledger.record("AI-03", "stop_opacity_50_over_red", v, detail);
    }
    ledger.finish();
}

fn mode(name: &str) -> StudioBlendMode {
    use StudioBlendMode::*;
    match name {
        "NORMAL" => Normal,
        "MULTIPLY" => Multiply,
        "SCREEN" => Screen,
        "OVERLAY" => Overlay,
        "SOFTLIGHT" => SoftLight,
        "HARDLIGHT" => HardLight,
        "COLORDODGE" => ColourDodge,
        "COLORBURN" => ColourBurn,
        "DARKEN" => Darken,
        "LIGHTEN" => Lighten,
        "DIFFERENCE" => Difference,
        "EXCLUSION" => Exclusion,
        "HUE" => Hue,
        "SATURATIONBLEND" => Saturation,
        "COLORBLEND" => Colour,
        "LUMINOSITY" => Luminosity,
        other => panic!("mode {other}"),
    }
}

/// AI-04: 16 blend modes x opacity 100/50: four solid 16 pt bands over a black -> white backdrop.
#[test]
fn ai04_blend_modes() {
    const T: &str = "d5_ai_oracle::ai04_blend_modes";
    let Some(root) = root_or_not_proven(T) else { return };
    let mut ledger = Ledger::new(T, EXPECTED);
    let modes = [
        "NORMAL", "MULTIPLY", "SCREEN", "OVERLAY", "SOFTLIGHT", "HARDLIGHT", "COLORDODGE", "COLORBURN", "DARKEN", "LIGHTEN",
        "DIFFERENCE", "EXCLUSION", "HUE", "SATURATIONBLEND", "COLORBLEND", "LUMINOSITY",
    ];
    let bands: [[f32; 3]; 4] = [[200.0, 80.0, 40.0], [40.0, 160.0, 220.0], [128.0, 128.0, 128.0], [20.0, 200.0, 90.0]].map(|c| c.map(|v| v / 255.0));
    for op in [100u32, 50] {
        for m in modes {
            let variant = format!("{m}_o{op}");
            let Some(img) = load(&root, &mut ledger, "AI-04", &variant) else { continue };
            let stops = vec![
                GradientStop { offset: 0.0, colour: [0.0, 0.0, 0.0, 1.0], midpoint: None },
                GradientStop { offset: 1.0, colour: [1.0, 1.0, 1.0, 1.0], midpoint: None },
            ];
            let ramp = gradient(GradientGeometry::Linear { x1: 0.0, y1: 0.0, x2: 256.0, y2: 0.0 }, stops);
            let frame = PixelFrame { x: 0, y: 0, width: img.width as u32, height: 1 };
            let backdrop = paint_coverage(&CoveragePlane { frame, data: vec![255; img.width] }, &ramp).expect("nib paint").data;
            let mut pred = Vec::with_capacity(img.width * img.height);
            for row in 0..img.height {
                let band = bands[(row / 16).min(3)];
                for b in backdrop.iter().take(img.width) {
                    let cb = Rgba::new([b[0], b[1], b[2]], 1.0);
                    let out = composite_over(mode(m), cb, Rgba::new(band, 1.0), op as f32 / 100.0, PixelSite::default()).expect("blend");
                    pred.push(out.rgb.map(q8));
                }
            }
            let (v, detail) = colour_verdict(&pred, &img);
            ledger.record("AI-04", &variant, v, detail);
        }
    }
    ledger.finish();
}

/// AI-05: group isolation x knockout with two overlapping 50 % Multiply squares over yellow.
/// Illustrator's object opacity is PDF constant alpha, not shape: the knockout shape is the
/// object's coverage (the oracle's overlap shows the top square alone at 50 %).
#[test]
fn ai05_isolation_knockout() {
    const T: &str = "d5_ai_oracle::ai05_isolation_knockout";
    let Some(root) = root_or_not_proven(T) else { return };
    let mut ledger = Ledger::new(T, EXPECTED);
    let yellow = Rgba::new([230.0 / 255.0, 230.0 / 255.0, 60.0 / 255.0], 1.0);
    let red = Rgba::new([220.0 / 255.0, 30.0 / 255.0, 30.0 / 255.0], 1.0);
    let blue = Rgba::new([30.0 / 255.0, 30.0 / 255.0, 220.0 / 255.0], 1.0);
    let ctx = BlendContext { space: BlendSpace::Encoded, site: PixelSite::default() };
    for (variant, isolated, knockout) in
        [("iso_off_ko_disabled", false, false), ("iso_on_ko_disabled", true, false), ("iso_off_ko_enabled", false, true), ("iso_on_ko_enabled", true, true)]
    {
        let Some(img) = load(&root, &mut ledger, "AI-05", variant) else { continue };
        let (w, h) = (img.width, img.height);
        let hf = h as f64;
        let rect = |left: f64, top: f64| poly(&[[left, top - 60.0], [left + 60.0, top - 60.0], [left + 60.0, top], [left, top]], hf, true, FillRule::NonZero);
        let (cr, cbl) = (coverage(&rect(20.0, 100.0), w, h), coverage(&rect(50.0, 80.0), w, h));
        let mut pred = Vec::with_capacity(w * h);
        for i in 0..w * h {
            let (kr, kb) = (f32::from(cr.data[i]) / 255.0, f32::from(cbl.data[i]) / 255.0);
            let out = if knockout {
                let initial = if isolated { Rgba::TRANSPARENT } else { yellow };
                let r1 = knockout_over(StudioBlendMode::Multiply, initial, initial, red, 0.5 * kr, kr, PixelSite::default()).expect("ko red");
                let r2 = knockout_over(StudioBlendMode::Multiply, r1, initial, blue, 0.5 * kb, kb, PixelSite::default()).expect("ko blue");
                if isolated { composite_over(StudioBlendMode::Normal, yellow, r2, 1.0, PixelSite::default()).expect("group") } else { r2 }
            } else {
                let leaves = vec![
                    StackNode::Leaf { source: red, mode: StudioBlendMode::Multiply, opacity: 0.5 * kr },
                    StackNode::Leaf { source: blue, mode: StudioBlendMode::Multiply, opacity: 0.5 * kb },
                ];
                let group_mode = if isolated { StudioBlendMode::Normal } else { StudioBlendMode::PassThrough };
                evaluate_stack(&[StackNode::Group { children: leaves, mode: group_mode, opacity: 1.0 }], yellow, &ctx).expect("stack").0
            };
            pred.push(out.rgb.map(q8));
        }
        let (v, detail) = colour_verdict(&pred, &img);
        ledger.record("AI-05", variant, v, detail);
    }
    ledger.finish();
}
