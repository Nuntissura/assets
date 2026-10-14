//! DNG container + colour tests. Inputs are synthesized in memory (no downloaded RAW files).
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::color::{self, Mat3};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

/// sRGB -> XYZ D50 colorants (ICC sRGB profile / Lindbloom D50 table), literal, independent of the crate.
const SRGB_D50: Mat3 = [
    0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045,
    0.7141733,
];

// ---- minimal TIFF/DNG writer -------------------------------------------------------------
#[derive(Clone)]
struct T {
    tag: u16,
    typ: u16,
    count: u32,
    data: Vec<u8>,
}

struct Enc {
    be: bool,
}

impl Enc {
    fn u16(&self, v: u16) -> [u8; 2] {
        if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
    }
    fn u32(&self, v: u32) -> [u8; 4] {
        if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
    }
    fn byte(&self, tag: u16, v: &[u8]) -> T {
        T { tag, typ: 1, count: v.len() as u32, data: v.to_vec() }
    }
    fn ascii(&self, tag: u16, s: &str) -> T {
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        T { tag, typ: 2, count: d.len() as u32, data: d }
    }
    fn short(&self, tag: u16, v: &[u16]) -> T {
        T { tag, typ: 3, count: v.len() as u32, data: v.iter().flat_map(|x| self.u16(*x)).collect() }
    }
    fn long(&self, tag: u16, v: &[u32]) -> T {
        T { tag, typ: 4, count: v.len() as u32, data: v.iter().flat_map(|x| self.u32(*x)).collect() }
    }
    fn rational(&self, tag: u16, v: &[f64], den: u32) -> T {
        let data = v
            .iter()
            .flat_map(|x| {
                let n = (x * f64::from(den)).round() as u32;
                [self.u32(n), self.u32(den)].concat()
            })
            .collect();
        T { tag, typ: 5, count: v.len() as u32, data }
    }
    fn srational(&self, tag: u16, v: &[f64], den: i32) -> T {
        let data = v
            .iter()
            .flat_map(|x| {
                let n = (x * f64::from(den)).round() as i32;
                [self.u32(n as u32), self.u32(den as u32)].concat()
            })
            .collect();
        T { tag, typ: 10, count: v.len() as u32, data }
    }
    fn double(&self, tag: u16, v: &[f64]) -> T {
        let data = v
            .iter()
            .flat_map(|x| if self.be { x.to_be_bytes() } else { x.to_le_bytes() })
            .collect();
        T { tag, typ: 12, count: v.len() as u32, data }
    }
    fn ifd_len(&self, tags: &[T]) -> usize {
        2 + 12 * tags.len() + 4 + tags.iter().map(|t| if t.data.len() > 4 { t.data.len() + (t.data.len() & 1) } else { 0 }).sum::<usize>()
    }
    fn ifd(&self, at: usize, tags: &[T], next: u32) -> Vec<u8> {
        let mut sorted = tags.to_vec();
        sorted.sort_by_key(|t| t.tag);
        let mut out = Vec::new();
        out.extend_from_slice(&self.u16(sorted.len() as u16));
        let mut extra: Vec<u8> = Vec::new();
        let extra_base = at + 2 + 12 * sorted.len() + 4;
        for t in &sorted {
            out.extend_from_slice(&self.u16(t.tag));
            out.extend_from_slice(&self.u16(t.typ));
            out.extend_from_slice(&self.u32(t.count));
            if t.data.len() <= 4 {
                let mut field = t.data.clone();
                field.resize(4, 0);
                out.extend_from_slice(&field);
            } else {
                out.extend_from_slice(&self.u32((extra_base + extra.len()) as u32));
                extra.extend_from_slice(&t.data);
                if t.data.len() & 1 == 1 {
                    extra.push(0);
                }
            }
        }
        out.extend_from_slice(&self.u32(next));
        out.extend_from_slice(&extra);
        out
    }
}

#[derive(Clone)]
struct Spec {
    be: bool,
    width: u32,
    height: u32,
    samples: Vec<u16>,
    black_dim: (u16, u16),
    black: Vec<f64>,
    white: Option<u32>,
    cfa: [u8; 4],
    neutral: Option<[f64; 3]>,
    forward: [Option<Mat3>; 2],
    color: [Option<Mat3>; 2],
    illuminant: [Option<u16>; 2],
    tiles: Option<(u32, u32)>,
    active: Option<[u32; 4]>,
    crop: Option<([u16; 2], [u16; 2])>,
    compression: u16,
    bits: u16,
    photometric: u16,
    dng_version: Option<[u8; 4]>,
    backward: [u8; 4],
    lin_table: Option<Vec<u16>>,
    extra: Vec<(u16, u32)>,
    baseline: Option<f64>,
    analog: Option<[f64; 3]>,
    calib: [Option<Mat3>; 2],
    delta_h: Option<Vec<f64>>,
    delta_v: Option<Vec<f64>>,
    /// Encode the BlackLevelDelta arrays as DOUBLE instead of SRATIONAL (hostile magnitudes).
    delta_double: bool,
    lj: (usize, u8, u8, u32, usize),
}

struct Built {
    bytes: Vec<u8>,
    ifd0_at: usize,
    ifd0_entries: usize,
    raw_at: usize,
}

fn inverse(m: &Mat3) -> Mat3 {
    color::invert(m).unwrap()
}

fn base() -> Spec {
    Spec {
        be: false,
        width: 4,
        height: 4,
        samples: matrix_samples(),
        black_dim: (1, 1),
        black: vec![0.0],
        white: Some(64),
        cfa: [0, 1, 1, 2],
        neutral: Some([1.0, 1.0, 1.0]),
        forward: [Some(SRGB_D50), None],
        color: [Some(inverse(&SRGB_D50)), None],
        illuminant: [Some(23), None],
        tiles: None,
        active: None,
        crop: None,
        compression: 1,
        bits: 16,
        photometric: 32803,
        dng_version: Some([1, 4, 0, 0]),
        backward: [1, 1, 0, 0],
        lin_table: None,
        extra: vec![],
        baseline: None,
        analog: None,
        calib: [None, None],
        delta_h: None,
        delta_v: None,
        delta_double: false,
        lj: (2, 1, 16, 0, 0),
    }
}

/// RGGB 4x4 (black 0, white 64) with hand-computed demosaic values (see develop_recipe.rs).
fn matrix_samples() -> Vec<u16> {
    vec![8, 4, 16, 12, 20, 40, 28, 48, 24, 36, 32, 44, 52, 56, 60, 2]
}

fn build(s: &Spec) -> Built {
    let enc = Enc { be: s.be };
    let (w, h) = (s.width as usize, s.height as usize);
    let sample_bytes = |v: u16| -> Vec<u8> {
        if s.bits == 8 { vec![v as u8] } else { enc.u16(v).to_vec() }
    };
    // Per-block sample arrays (w, h, values) then their on-disk encoding.
    let raw_blocks: Vec<(usize, usize, Vec<u16>)> = match s.tiles {
        Some((tw, th)) => {
            let (tw, th) = (tw as usize, th as usize);
            let mut out = Vec::new();
            for ty in 0..h.div_ceil(th) {
                for tx in 0..w.div_ceil(tw) {
                    let mut b = Vec::new();
                    for y in 0..th {
                        for x in 0..tw {
                            let (sx, sy) = (tx * tw + x, ty * th + y);
                            b.push(if sx < w && sy < h { s.samples[sy * w + sx] } else { 0 });
                        }
                    }
                    out.push((tw, th, b));
                }
            }
            out
        }
        None => (0..h / 2).map(|strip| (w, 2, s.samples[strip * 2 * w..(strip + 1) * 2 * w].to_vec())).collect(),
    };
    let blocks: Vec<Vec<u8>> = raw_blocks
        .iter()
        .map(|(bw, bh, v)| {
            if s.compression == 7 {
                ljpeg_encode(v, *bw, *bh, s.lj)
            } else {
                v.iter().flat_map(|x| sample_bytes(*x)).collect()
            }
        })
        .collect();
    let make = |raw_at: u32, offsets: &[u32]| -> (Vec<T>, Vec<T>) {
        let mut t0 = vec![enc.short(254, &[1]), enc.ascii(50708, "Studio Test"), enc.long(330, &[raw_at]), enc.byte(50707, &s.backward)];
        if let Some(v) = s.dng_version {
            t0.push(enc.byte(50706, &v));
        }
        for (i, m) in s.color.iter().enumerate() {
            if let Some(m) = m {
                t0.push(enc.srational(50721 + i as u16, m, 10_000_000));
            }
        }
        for (i, m) in s.forward.iter().enumerate() {
            if let Some(m) = m {
                t0.push(enc.srational(50964 + i as u16, m, 10_000_000));
            }
        }
        for (i, c) in s.illuminant.iter().enumerate() {
            if let Some(c) = c {
                t0.push(enc.short(50778 + i as u16, &[*c]));
            }
        }
        if let Some(ab) = s.analog {
            t0.push(enc.rational(50727, &ab, 10_000_000));
        }
        for (i, m) in s.calib.iter().enumerate() {
            if let Some(m) = m {
                t0.push(enc.srational(50723 + i as u16, m, 10_000_000));
            }
        }
        if let Some(b) = s.baseline {
            t0.push(enc.srational(50730, &[b], 10_000));
        }
        if let Some(n) = s.neutral {
            t0.push(enc.rational(50728, &n, 10_000_000));
        }
        let counts: Vec<u32> = blocks.iter().map(|b| b.len() as u32).collect();
        let mut t1 = vec![
            enc.short(254, &[0]),
            enc.long(256, &[s.width]),
            enc.long(257, &[s.height]),
            enc.short(258, &[s.bits]),
            enc.short(259, &[s.compression]),
            enc.short(262, &[s.photometric]),
            enc.short(277, &[1]),
            enc.short(33421, &[2, 2]),
            enc.byte(33422, &s.cfa),
            enc.byte(50710, &[0, 1, 2]),
            enc.short(50711, &[1]),
            enc.short(50713, &[s.black_dim.0, s.black_dim.1]),
            enc.rational(50714, &s.black, 1),
        ];
        if let Some(wl) = s.white {
            t1.push(enc.long(50717, &[wl]));
        }
        match s.tiles {
            Some((tw, th)) => {
                t1.extend([enc.long(322, &[tw]), enc.long(323, &[th]), enc.long(324, offsets), enc.long(325, &counts)]);
            }
            None => {
                t1.extend([enc.long(273, offsets), enc.short(278, &[2]), enc.long(279, &counts)]);
            }
        }
        if let Some(a) = s.active {
            t1.push(enc.long(50829, &a));
        }
        if let Some((o, z)) = s.crop {
            t1.extend([enc.short(50719, &o), enc.short(50720, &z)]);
        }
        for (tag, v) in [(50715u16, &s.delta_h), (50716u16, &s.delta_v)] {
            if let Some(v) = v {
                t1.push(if s.delta_double { enc.double(tag, v) } else { enc.srational(tag, v, 1) });
            }
        }
        for (tag, value) in &s.extra {
            t1.push(enc.long(*tag, &[*value]));
        }
        if let Some(table) = &s.lin_table {
            t1.push(enc.short(50712, table));
        }
        (t0, t1)
    };
    let zeros = vec![0u32; blocks.len()];
    let (a, b) = make(0, &zeros);
    let ifd0_at = 8;
    let raw_at = ifd0_at + enc.ifd_len(&a);
    let data_at = raw_at + enc.ifd_len(&b);
    let mut offsets = Vec::new();
    let mut p = data_at;
    for blk in &blocks {
        offsets.push(p as u32);
        p += blk.len();
    }
    let (t0, t1) = make(raw_at as u32, &offsets);
    let mut bytes = if s.be { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
    bytes.extend_from_slice(&enc.u32(ifd0_at as u32));
    bytes.extend(enc.ifd(ifd0_at, &t0, 0));
    bytes.extend(enc.ifd(raw_at, &t1, 0));
    for blk in &blocks {
        bytes.extend_from_slice(blk);
    }
    Built { bytes, ifd0_at, ifd0_entries: t0.len(), raw_at }
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn develop_pixels(raw: &DngRaw, window: PlaneWindow, tile: u32) -> Vec<DevelopedTile> {
    let (plane, _) = raw.cfa_plane(window, RangePolicy::Refuse, 7, Limits::SPEC_V1).unwrap();
    let (recipe, _) = raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore).unwrap();
    let mut tiles = Vec::new();
    develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &recipe,
            recipe_revision: 1,
            expected_source_revision: 7,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: tile, tile_height: tile },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &CancellationToken::default(),
        &mut |t| {
            tiles.push(t);
            Ok(())
        },
    )
    .unwrap();
    tiles
}

fn px(tile: &DevelopedTile, tw: u32, x: usize, y: usize) -> [f32; 3] {
    let i = (y * tw as usize + x) * 12;
    let f = |o: usize| f32::from_le_bytes(tile.colour[i + o..i + o + 4].try_into().unwrap());
    [f(0), f(4), f(8)]
}

fn close(got: [f32; 3], want: [f32; 3], tol: f32) {
    for i in 0..3 {
        assert!((got[i] - want[i]).abs() <= tol, "channel {i}: {got:?} vs {want:?}");
    }
}

#[test]
fn dng_uncompressed_end_to_end() {
    bounded_case("dng_uncompressed_end_to_end", || {
        let built = build(&base());
        let raw = decode_container(&built.bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.bits_per_sample), (4, 4, 16));
        assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
        assert_eq!((raw.white_level, raw.black_level.clone()), (64, vec![0.0]));
        assert_eq!(raw.sample(1, 1), 40);
        assert_eq!(raw.unique_camera_model.as_deref(), Some("Studio Test"));
        assert_ne!(raw.container_sha256, [0; 32]);
        let window = raw.default_window();
        assert_eq!(window, PlaneWindow { x: 0, y: 0, width: 4, height: 4 });
        let (plane, report) = raw.cfa_plane(window, RangePolicy::Refuse, 7, Limits::SPEC_V1).unwrap();
        assert_eq!((report, plane.sensor_origin(), plane.black(), plane.white()), (WindowReport::default(), (0, 0), [0; 3], [64; 3]));
        // ForwardMatrix = sRGB colorants, neutral 1: camera is linear sRGB, so output == demosaic.
        let tiles = develop_pixels(&raw, window, 4);
        close(px(&tiles[0], 4, 1, 1), [0.3125, 0.34375, 0.625], 2e-5);
        close(px(&tiles[0], 4, 0, 0), [0.125, 0.1875, 0.625], 2e-5);
        close(px(&tiles[0], 4, 3, 3), [0.5, 0.8125, 0.03125], 2e-5);
        // big-endian container decodes to the same frame.
        let be = decode_container(&build(&Spec { be: true, ..base() }).bytes).unwrap();
        assert!((0..4).all(|y| (0..4).all(|x| be.sample(x, y) == raw.sample(x, y))));
        // contiguous image helper == tile output; nothing unapplied in the plain file.
        assert!(raw.unapplied.is_empty() && raw.require_complete().is_ok());
        let (plane, _) = raw.cfa_plane(window, RangePolicy::Refuse, 7, Limits::SPEC_V1).unwrap();
        let (recipe, _) = raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore).unwrap();
        let (image, receipt) = develop_to_image(&plane, &recipe, 1, &CancellationToken::default()).unwrap();
        assert_eq!((image.width, image.height, image.rgb.len(), receipt.tiles), (4, 4, 48, 1));
        let at = (4 + 1) * 3; // pixel (1,1)
        close([image.rgb[at], image.rgb[at + 1], image.rgb[at + 2]], [0.3125, 0.34375, 0.625], 2e-5);
        // BaselineExposure is carried only on request; unapplied features are reported, not dropped.
        let flagged = Spec { baseline: Some(0.5), extra: vec![(51009, 0), (274, 6)], ..base() };
        let flagged = decode_container(&build(&flagged).bytes).unwrap();
        assert_eq!(flagged.unapplied, vec!["opcode_list_2", "orientation"]);
        assert_eq!(flagged.require_complete().err(), Some(DevelopError::UnsupportedContribution("opcode_list_2")));
        let xyz2rgb = color::xyz_d50_to_linear_srgb().unwrap();
        assert_eq!(flagged.recipe(&xyz2rgb, profile(), BaselineExposure::Apply).unwrap().0.exposure_stops, 0.5);
        assert_eq!(flagged.recipe(&xyz2rgb, profile(), BaselineExposure::Ignore).unwrap().0.exposure_stops, 0.0);
    });
}

#[test]
fn dng_colour_solution_paths() {
    bounded_case("dng_colour_solution_paths", || {
        // Robertson CCT of standard illuminants (validates the isotemperature table).
        for (xy, kelvin) in [([0.31271, 0.32902], 6504.0), ([0.44757, 0.40745], 2856.0), ([0.34567, 0.35850], 5003.0)] {
            let got = color::cct_from_xy(xy).unwrap();
            assert!((got - kelvin).abs() < 40.0, "{xy:?}: {got} vs {kelvin}");
        }
        // XYZ D50 -> linear sRGB is the inverse of the published ICC colorants; the derived
        // primaries + Bradford route (used for other spaces) agrees with them within 1e-4.
        let product = color::mul(&color::xyz_d50_to_linear_srgb().unwrap(), &SRGB_D50);
        for (i, v) in product.iter().enumerate() {
            let want = if i % 4 == 0 { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-6, "entry {i}: {v}");
        }
        let derived = color::rgb_to_xyz_d50([[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]], [0.3127, 0.3290]).unwrap();
        for (i, (got, want)) in derived.iter().zip(SRGB_D50).enumerate() {
            assert!((got - want).abs() < 1e-4, "derived entry {i}: {got} vs {want}");
        }
        // ColorMatrix-only path: white = FM*(1,1,1) = D50 white, Bradford is ~identity => FM back.
        let no_fm = decode_container(&build(&Spec { forward: [None, None], ..base() }).bytes).unwrap();
        let s = no_fm.solve_colour().unwrap();
        assert!(!s.used_forward_matrix);
        assert!((s.white_xy[0] - 0.3457).abs() < 5e-4 && (s.white_xy[1] - 0.3585).abs() < 5e-4, "{:?}", s.white_xy);
        assert!((s.cct - 5003.0).abs() < 40.0);
        for (i, (got, want)) in s.camera_to_xyz_d50.iter().zip(SRGB_D50).enumerate() {
            assert!((got - want).abs() < 5e-5, "entry {i}");
        }
        // Two profiles A (2856 K) and D65 (6504 K): the weight toward the hotter profile is linear in
        // mired at the white's CCT (~5003 K => ~0.765), and listing the profiles in the other order
        // (illuminant 1 = D65) gives the same solution. FM_D65 has its blue column scaled by 0.9.
        let mut fm_d65 = SRGB_D50;
        for row in 0..3 {
            fm_d65[row * 3 + 2] *= 0.9;
        }
        let two = |first: (u16, Mat3), second: (u16, Mat3)| {
            let spec = Spec {
                forward: [Some(first.1), Some(second.1)],
                color: [Some(inverse(&SRGB_D50)), Some(inverse(&SRGB_D50))],
                illuminant: [Some(first.0), Some(second.0)],
                ..base()
            };
            decode_container(&build(&spec).bytes).unwrap().solve_colour().unwrap()
        };
        let a_first = two((17, SRGB_D50), (21, fm_d65));
        let d65_first = two((21, fm_d65), (17, SRGB_D50));
        assert!((a_first.weight - 0.765).abs() < 0.01, "{}", a_first.weight);
        assert!((a_first.weight - d65_first.weight).abs() < 1e-12);
        for i in 0..9 {
            assert!((a_first.camera_to_xyz_d50[i] - d65_first.camera_to_xyz_d50[i]).abs() < 1e-12);
        }
        // blue column = FM[2] * ((1 - w) + 0.9 w)
        let w = a_first.weight;
        let want = SRGB_D50[2] * ((1.0 - w) + 0.9 * w);
        assert!((a_first.camera_to_xyz_d50[2] - want).abs() < 1e-6);
        // Missing neutral and unknown illuminant refuse; AsShotNeutral drives the gain stage.
        let none = decode_container(&build(&Spec { neutral: None, ..base() }).bytes).unwrap();
        assert_eq!(none.solve_colour().err(), Some(DevelopError::MissingColourData));
        let odd = Spec {
            forward: [Some(SRGB_D50), Some(SRGB_D50)],
            illuminant: [Some(0), Some(21)],
            ..base()
        };
        let odd = decode_container(&build(&odd).bytes).unwrap();
        assert_eq!(odd.solve_colour().err(), Some(DevelopError::UnsupportedIlluminant));
        // Invariants worked out independently of the D / AnalogBalance derivation under test:
        // the ForwardMatrix path maps the camera neutral to FM * (1,1,1), i.e. the row sums of the
        // published sRGB D50 matrix = D50 white (0.96422, 1, 0.82521). Dropping the D factor
        // (diag(1/ReferenceNeutral)) breaks this for any neutral != (1,1,1).
        let d50 = [0.96422, 1.0, 0.82521];
        let neutral = [0.5, 1.0, 0.8];
        let warm = decode_container(&build(&Spec { neutral: Some(neutral), ..base() }).bytes).unwrap();
        let sol = warm.solve_colour().unwrap();
        assert!(sol.used_forward_matrix);
        let white = color::mul_vec(&sol.camera_to_xyz_d50, &neutral);
        for i in 0..3 {
            assert!((white[i] - d50[i]).abs() < 1e-5, "FM path white {white:?}");
        }
        // AnalogBalance (1.25, 1, 0.8), CameraCalibration absent (identity): ReferenceNeutral =
        // inv(AB)*n = (0.4, 1, 1), D = diag(2.5, 1, 1), inv(AB) = diag(0.8, 1, 1.25) => C2X = FM *
        // diag(2, 1, 1.25), worked out by hand.
        let ab = decode_container(&build(&Spec { neutral: Some(neutral), analog: Some([1.25, 1.0, 0.8]), ..base() }).bytes).unwrap();
        let sol_ab = ab.solve_colour().unwrap();
        for row in 0..3 {
            for (col, scale) in [2.0, 1.0, 1.25].into_iter().enumerate() {
                let want = SRGB_D50[row * 3 + col] * scale;
                let got = sol_ab.camera_to_xyz_d50[row * 3 + col];
                assert!((got - want).abs() < 1e-5, "AB case [{row}][{col}]: {got} vs {want}");
            }
        }
        // ColorMatrix-only path uses the same convention: neutral -> D50 white with Y = 1, whatever
        // the magnitude of ColorMatrix * neutral.
        let cm = decode_container(&build(&Spec { neutral: Some(neutral), forward: [None, None], ..base() }).bytes).unwrap();
        let sol_cm = cm.solve_colour().unwrap();
        assert!(!sol_cm.used_forward_matrix);
        let white = color::mul_vec(&sol_cm.camera_to_xyz_d50, &neutral);
        for i in 0..3 {
            assert!((white[i] - d50[i]).abs() < 1e-4, "CM path white {white:?}");
        }
        // Recipe level: gain-balanced neutral through the recipe matrix is linear-sRGB white.
        let (recipe, _) = warm.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore).unwrap();
        assert_eq!(recipe.as_shot_neutral, neutral);
        let gain = hsk_studio_develop::recipe::as_shot_gain(neutral).unwrap();
        let balanced = [neutral[0] * gain[0], neutral[1] * gain[1], neutral[2] * gain[2]];
        let rgb = color::mul_vec(&recipe.camera_to_profile_rgb, &balanced);
        for (i, v) in rgb.iter().enumerate() {
            assert!((v - 1.0).abs() < 1e-5, "channel {i}: {v}");
        }
    });
}

#[test]
fn dng_tiles_active_area_black_levels() {
    bounded_case("dng_tiles_active_area_black_levels", || {
        // 8x6 big-endian frame in 4x4 tiles (right tiles padded), active area rows 2..6 cols 2..8,
        // default crop origin (2,0) size (4,4) => raw window x=4,y=2. BlackLevel 2x2 = R 64, Gr 64,
        // Gb 66, B 64; sample(x,y) = 100 + x + 16*y.
        let samples: Vec<u16> = (0..48).map(|i| 100 + (i % 8) as u16 + 16 * (i / 8) as u16).collect();
        let spec = Spec {
            be: true,
            width: 8,
            height: 6,
            samples,
            tiles: Some((4, 4)),
            active: Some([2, 2, 6, 8]),
            crop: Some(([2, 0], [4, 4])),
            black_dim: (2, 2),
            black: vec![64.0, 64.0, 66.0, 64.0],
            white: Some(1023),
            ..base()
        };
        let raw = decode_container(&build(&spec).bytes).unwrap();
        assert_eq!(raw.sample(7, 5), 100 + 7 + 80);
        assert_eq!(raw.active_area, ActiveArea { top: 2, left: 2, bottom: 6, right: 8 });
        let window = raw.default_window();
        assert_eq!(window, PlaneWindow { x: 4, y: 2, width: 4, height: 4 });
        let (plane, report) = raw.cfa_plane(window, RangePolicy::Refuse, 3, Limits::SPEC_V1).unwrap();
        assert_eq!(plane.sensor_origin(), (4, 2));
        assert_eq!((plane.black(), plane.white()), ([64, 64, 64], [1023; 3]));
        assert!(report.black_site_adjusted && !report.black_rounded);
        // window (0,1) = raw (4,3) is a Gb site: 100+4+48 = 152, minus (66-64) => 150.
        let at = |x: usize, y: usize| u16::from_le_bytes([plane.bytes()[y * 8 + x * 2], plane.bytes()[y * 8 + x * 2 + 1]]);
        assert_eq!((at(0, 0), at(0, 1), at(1, 0)), (100 + 4 + 32, 150, 100 + 5 + 32));
        // CFA phase is anchored at the frame origin: raw (4,2) is R, raw (5,3) is B.
        assert_eq!((plane.channel(0, 0), plane.channel(1, 1), plane.channel(1, 0)), (0, 2, 1));
        // window outside the active area refuses; sample below black refuses or clamps (counted).
        let outside = PlaneWindow { x: 0, y: 0, width: 4, height: 4 };
        assert_eq!(raw.cfa_plane(outside, RangePolicy::Refuse, 3, Limits::SPEC_V1).err(), Some(DevelopError::InvalidDimensions));
        let mut low = spec.clone();
        low.samples[2 * 8 + 4] = 10; // raw (4,2), an R site inside the window
        let raw = decode_container(&build(&low).bytes).unwrap();
        assert_eq!(raw.cfa_plane(window, RangePolicy::Refuse, 3, Limits::SPEC_V1).err(), Some(DevelopError::SampleOutOfRange));
        let (plane, report) = raw.cfa_plane(window, RangePolicy::ClampReported, 3, Limits::SPEC_V1).unwrap();
        assert_eq!((report.clamped_below, report.clamped_above, at_plane(&plane, 0, 0)), (1, 0, 64));
        // BlackLevelDeltaH/V are applied per active-area column/row (review R2: they were ignored).
        // Black 64 + deltaH [0,1,2,3] + deltaV [0,10,0,0], every raw sample 200. Channel black is the
        // first site's effective black; every other site is shifted by its difference:
        // R first (0,0)=64; G first (1,0)=65; B first (1,1)=75. G (0,1) effective 64+0+10=74 -> 200-(74-65)=191;
        // B (1,3) effective 64+1+0=65 -> 200-(65-75)=210.
        let deltas = Spec {
            samples: vec![200; 16],
            white: Some(1023),
            delta_h: Some(vec![0.0, 1.0, 2.0, 3.0]),
            delta_v: Some(vec![0.0, 10.0, 0.0, 0.0]),
            black: vec![64.0],
            ..base()
        };
        let raw = decode_container(&build(&deltas).bytes).unwrap();
        assert_eq!((raw.black_delta_h.len(), raw.black_delta_v.len()), (4, 4));
        let (plane, report) = raw.cfa_plane(raw.default_window(), RangePolicy::Refuse, 1, Limits::SPEC_V1).unwrap();
        assert_eq!(plane.black(), [64, 65, 75]);
        assert!(report.black_site_adjusted);
        assert_eq!((at_plane(&plane, 0, 1), at_plane(&plane, 1, 3), at_plane(&plane, 2, 0)), (191, 210, 198));
    });
}

fn at_plane(plane: &CfaPlane, x: usize, y: usize) -> u16 {
    plane.sample(x, y)
}

/// Review R3 (SMB-DEVELOP): finite but absurd BlackLevelDelta values saturate the rounded site
/// black at an i64 limit; the per-site shift used to overflow (panic in debug, wrap in release).
#[test]
fn dng_black_level_delta_overflow_is_a_typed_refusal() {
    bounded_case("dng_black_level_delta_overflow", || {
        // 4x4 RGGB: R sites are columns 0 and 2. [-1e300, 0, 1e300, 0] puts one R site at i64::MIN
        // (the channel reference) and the next at i64::MAX: the shift overflows under either policy.
        // [1e300, 0, 0, 0] makes the reference i64::MAX: Refuse stops at the first site (below the
        // channel black); ClampReported clamps it and the other R site shifts by -i64::MAX, where
        // `sample - shift` would overflow.
        let cases = [
            (vec![-1e300, 0.0, 1e300, 0.0], RangePolicy::Refuse, DevelopError::InvalidLevels),
            (vec![-1e300, 0.0, 1e300, 0.0], RangePolicy::ClampReported, DevelopError::InvalidLevels),
            (vec![1e300, 0.0, 0.0, 0.0], RangePolicy::Refuse, DevelopError::SampleOutOfRange),
            (vec![1e300, 0.0, 0.0, 0.0], RangePolicy::ClampReported, DevelopError::InvalidLevels),
        ];
        for (deltas, policy, want) in cases {
            let spec = Spec { delta_h: Some(deltas.clone()), delta_double: true, ..base() };
            let raw = decode_container(&build(&spec).bytes).unwrap();
            let got = raw.cfa_plane(raw.default_window(), policy, 1, Limits::SPEC_V1).err();
            assert_eq!(got, Some(want), "{deltas:?} {policy:?}");
        }
        // A delta that is large but representable keeps its old behaviour: the site is out of range.
        let spec = Spec { delta_h: Some(vec![0.0, 0.0, 1e15, 0.0]), delta_double: true, ..base() };
        let raw = decode_container(&build(&spec).bytes).unwrap();
        assert_eq!(
            raw.cfa_plane(raw.default_window(), RangePolicy::Refuse, 1, Limits::SPEC_V1).err(),
            Some(DevelopError::SampleOutOfRange)
        );
    });
}

#[test]
fn dng_refusals() {
    bounded_case("dng_refusals", || {
        let refuse = |bytes: &[u8], want: DevelopError| assert_eq!(decode_container(bytes).err(), Some(want));
        refuse(&build(&Spec { dng_version: None, ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { backward: [2, 0, 0, 0], ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { compression: 8, ..base() }).bytes, DevelopError::UnsupportedCompression);
        refuse(&build(&Spec { photometric: 34892, ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { photometric: 2, ..base() }).bytes, DevelopError::InvalidContainer);
        refuse(&build(&Spec { cfa: [1, 0, 0, 1], ..base() }).bytes, DevelopError::UnsupportedPattern);
        refuse(&build(&Spec { active: Some([1, 0, 4, 4]), ..base() }).bytes, DevelopError::UnsupportedActiveArea);
        refuse(&build(&Spec { active: Some([0, 0, 5, 4]), ..base() }).bytes, DevelopError::InvalidDimensions);
        refuse(&build(&Spec { black: vec![0.0, 1.0], ..base() }).bytes, DevelopError::InvalidContainer); // 1x1 repeat, 2 values
        refuse(&build(&Spec { white: Some(70_000), ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { lin_table: Some(vec![0; 10]), white: Some(64), ..base() }).bytes, DevelopError::SampleOutOfRange);
        // 8-bit data decodes; packed 12-bit is not byte aligned and refuses.
        let eight = Spec { bits: 8, ..base() };
        assert_eq!(decode_container(&build(&eight).bytes).unwrap().sample(1, 1), 40);
        refuse(&build(&Spec { bits: 12, ..base() }).bytes, DevelopError::UnsupportedContainer);
        // container damage: truncated payload, BigTIFF header, self-referencing IFD chain, junk.
        let good = build(&base());
        refuse(&good.bytes[..good.bytes.len() - 4], DevelopError::InvalidContainer);
        let mut bigtiff = good.bytes.clone();
        bigtiff[2] = 43;
        refuse(&bigtiff, DevelopError::UnsupportedContainer);
        let mut cyclic = good.bytes.clone();
        let next_at = good.ifd0_at + 2 + 12 * good.ifd0_entries;
        cyclic[next_at..next_at + 4].copy_from_slice(&(good.ifd0_at as u32).to_le_bytes());
        refuse(&cyclic, DevelopError::InvalidContainer);
        refuse(b"RIFF....WEBP", DevelopError::DecoderUnavailable("unknown"));
        refuse(b"II*\0\x08\0\0\0", DevelopError::InvalidContainer);
        // Hostile tiled DNG (review R2): tile dimensions whose byte product overflows u64 must be a
        // typed refusal in every build profile, never a panic; and no frame is allocated for blocks
        // that do not fit in the file.
        let tiled = build(&Spec { tiles: Some((4, 4)), ..base() });
        let patch = |bytes: &[u8], tag: u16, value: u32| -> Vec<u8> {
            let mut out = bytes.to_vec();
            let n = u16::from_le_bytes([out[tiled.raw_at], out[tiled.raw_at + 1]]) as usize;
            let mut patched = false;
            for i in 0..n {
                let at = tiled.raw_at + 2 + 12 * i;
                if u16::from_le_bytes([out[at], out[at + 1]]) == tag {
                    out[at + 8..at + 12].copy_from_slice(&value.to_le_bytes());
                    patched = true;
                }
            }
            assert!(patched, "tag {tag} not found");
            out
        };
        let hostile = patch(&patch(&tiled.bytes, 322, (1 << 31) + 32_768), 323, u32::MAX - 65_534);
        refuse(&hostile, DevelopError::InvalidContainer);
        refuse(&patch(&patch(&tiled.bytes, 322, 65_535), 323, 65_535), DevelopError::InvalidContainer); // > pixel cap
        refuse(&patch(&patch(&tiled.bytes, 322, 16_384), 323, 16_384), DevelopError::InvalidContainer); // block not in file
        // Declared sample count far beyond the stream length (allocation bomb) refuses before decoding.
        // BlackLevelDelta arrays must match the active area exactly.
        refuse(&build(&Spec { delta_h: Some(vec![0.0; 3]), ..base() }).bytes, DevelopError::InvalidContainer);
        refuse(&build(&Spec { delta_v: Some(vec![0.0; 5]), ..base() }).bytes, DevelopError::InvalidContainer);
        // Limits that would reopen the reflect(.., 1) panic are refused at admission.
        let raw = decode_container(&build(&base()).bytes).unwrap();
        for bad in [
            Limits { min_bayer_side: 1, ..Limits::SPEC_V1 },
            Limits { min_xtrans_side: 5, ..Limits::SPEC_V1 },
            Limits { max_tiles: 0, ..Limits::SPEC_V1 },
        ] {
            assert_eq!(raw.cfa_plane(raw.default_window(), RangePolicy::Refuse, 1, bad).err(), Some(DevelopError::InvalidLimits));
        }
        assert_eq!(Limits { max_side: 128, ..Limits::SPEC_V1 }.name(), "custom");
        // linearization: table maps raw values; WhiteLevel must then be explicit.
        let table: Vec<u16> = (0..=100u16).map(|i| i * 5).collect();
        let lin = Spec { samples: vec![20; 16], lin_table: Some(table.clone()), white: Some(500), ..base() };
        let raw = decode_container(&build(&lin).bytes).unwrap();
        assert_eq!((raw.sample(2, 3), raw.linearized), (100, true));
        refuse(&build(&Spec { lin_table: Some(table), white: None, ..lin }).bytes, DevelopError::InvalidContainer);
    });
}

// ---- test-side lossless JPEG encoder (T.81 SOF3, one 5-bit-code Huffman table) -------------
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.flush_byte();
            }
        }
    }
    fn flush_byte(&mut self) {
        self.out.push(self.acc as u8);
        if self.acc as u8 == 0xFF {
            self.out.push(0);
        }
        self.acc = 0;
        self.n = 0;
    }
    fn pad(&mut self) {
        while self.n != 0 {
            self.put(1, 1);
        }
    }
}

/// `lj` = (components, predictor, precision, point transform, restart rows).
fn ljpeg_encode(samples: &[u16], bw: usize, bh: usize, lj: (usize, u8, u8, u32, usize)) -> Vec<u8> {
    let (comps, predictor, precision, pt, restart_rows) = lj;
    let x_per_line = bw / comps;
    let be16 = |v: usize| (v as u16).to_be_bytes();
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xC3];
    out.extend(be16(8 + 3 * comps));
    out.push(precision);
    out.extend(be16(bh));
    out.extend(be16(x_per_line));
    out.push(comps as u8);
    for c in 0..comps {
        out.extend([c as u8 + 1, 0x11, 0]);
    }
    out.extend([0xFF, 0xC4]);
    out.extend(be16(2 + 1 + 16 + 17));
    out.push(0);
    let mut counts = [0u8; 16];
    counts[4] = 17; // seventeen 5-bit codes: symbol k has code k
    out.extend(counts);
    out.extend(0..=16u8);
    if restart_rows > 0 {
        out.extend([0xFF, 0xDD, 0, 4]);
        out.extend(be16(restart_rows * x_per_line));
    }
    out.extend([0xFF, 0xDA]);
    out.extend(be16(6 + 2 * comps));
    out.push(comps as u8);
    for c in 0..comps {
        out.extend([c as u8 + 1, 0]);
    }
    out.extend([predictor, 0, pt as u8]);
    let mut w = BitWriter { out, acc: 0, n: 0 };
    let v = |i: usize| i64::from(samples[i]) >> pt;
    let mut rst = 0u8;
    for y in 0..bh {
        let first_row = y == 0 || (restart_rows > 0 && y % restart_rows == 0);
        if restart_rows > 0 && y > 0 && y % restart_rows == 0 {
            w.pad();
            w.out.extend([0xFF, 0xD0 + (rst & 7)]);
            rst += 1;
        }
        for x in 0..x_per_line {
            for c in 0..comps {
                let at = y * bw + x * comps + c;
                let prediction = if first_row {
                    if x == 0 { 1i64 << (precision as u32 - pt - 1) } else { v(at - comps) }
                } else if x == 0 {
                    v(at - bw)
                } else {
                    let (a, b, cc) = (v(at - comps), v(at - bw), v(at - bw - comps));
                    match predictor {
                        1 => a,
                        2 => b,
                        3 => cc,
                        4 => a + b - cc,
                        5 => a + ((b - cc) >> 1),
                        6 => b + ((a - cc) >> 1),
                        _ => (a + b) >> 1,
                    }
                };
                let mut diff = (v(at) - prediction).rem_euclid(65536);
                if diff > 32767 {
                    diff -= 65536;
                }
                if diff == -32768 {
                    w.put(16, 5); // category 16: no extra bits
                    continue;
                }
                let category = 64 - diff.unsigned_abs().leading_zeros();
                w.put(category, 5);
                if category > 0 {
                    let bits = if diff >= 0 { diff } else { diff - 1 };
                    w.put((bits as u32) & ((1u32 << category) - 1), category);
                }
            }
        }
    }
    w.pad();
    let mut out = w.out;
    out.extend([0xFF, 0xD9]);
    out
}

fn pseudo(i: usize, precision: u8, pt: u32) -> u16 {
    let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40;
    ((h as u16) & (((1u32 << precision) - 1) as u16)) & !((1u16 << pt) - 1)
}

#[test]
fn dng_lossless_jpeg() {
    bounded_case("dng_lossless_jpeg", || {
        // Known answer worked out by hand from T.81 (P=8, one line of 3 samples, 5-bit codes):
        // 100 vs 128 -> diff -28 (cat 5, bits 00011); 102 vs 100 -> +2 (cat 2, bits 10); 99 vs 102 -> -3 (cat 2, bits 00).
        let mut vector = vec![0xFF, 0xD8, 0xFF, 0xC3, 0, 11, 8, 0, 1, 0, 3, 1, 1, 0x11, 0, 0xFF, 0xC4, 0, 36, 0];
        vector.extend([0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        vector.extend(0..=16u8);
        vector.extend([0xFF, 0xDA, 0, 8, 1, 1, 0, 1, 0, 0, 0x28, 0xC5, 0x08, 0xFF, 0xD9]);
        let image = ljpeg::decode(&vector, 1 << 10).unwrap();
        assert_eq!((image.width, image.height, image.components, image.precision), (3, 1, 1, 8));
        assert_eq!(image.samples, vec![100, 102, 99]);
        // A truncated stream, a subsampled component, a bad predictor and an oversize frame refuse.
        assert!(ljpeg::decode(&vector[..vector.len() - 4], 1 << 10).is_err());
        let mut subsampled = vector.clone();
        subsampled[13] = 0x22;
        assert_eq!(ljpeg::decode(&subsampled, 1 << 10).err(), Some(DevelopError::UnsupportedCompression));
        let mut bad = vector.clone();
        let ss = vector.len() - 6;
        bad[ss] = 9;
        assert_eq!(ljpeg::decode(&bad, 1 << 10).err(), Some(DevelopError::InvalidContainer));
        assert_eq!(ljpeg::decode(&vector, 2).err(), Some(DevelopError::InvalidDimensions));
        // A tiny stream declaring a 65535 x 65535 frame is refused before any allocation (>= 1 bit per sample).
        let mut bomb = vector.clone();
        bomb[7..11].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(ljpeg::decode(&bomb, usize::MAX).err(), Some(DevelopError::InvalidContainer));

        // Round trips through DNG strips and tiles: every predictor, 1/2/4 components, 12/14/16-bit,
        // point transform, restart per row; the decoded frame must equal the source exactly.
        let cases: [(usize, u8, u8, u32, usize); 9] = [
            (2, 1, 16, 0, 0),
            (1, 7, 14, 0, 0),
            (2, 4, 12, 2, 0),
            (1, 2, 16, 0, 0),
            (1, 3, 12, 0, 0),
            (2, 5, 14, 0, 0),
            (1, 6, 16, 0, 0),
            (4, 1, 16, 0, 0),
            (2, 1, 12, 1, 1),
        ];
        for (n, case) in cases.into_iter().enumerate() {
            let (_, predictor, precision, pt, _) = case;
            for tiles in [None, Some((4u32, 4u32))] {
                let (width, height) = (8u32, 6u32);
                let samples: Vec<u16> = (0..(width * height) as usize).map(|i| pseudo(i + n, precision, pt)).collect();
                let spec = Spec {
                    width,
                    height,
                    samples: samples.clone(),
                    tiles,
                    compression: 7,
                    lj: case,
                    white: Some(60_000),
                    forward: [None, None],
                    color: [None, None],
                    ..base()
                };
                let raw = decode_container(&build(&spec).bytes).unwrap_or_else(|e| panic!("case {n} {tiles:?} predictor {predictor}: {e}"));
                let got: Vec<u16> = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| raw.sample(x, y)).collect();
                assert_eq!(got, samples, "case {n} tiles {tiles:?} predictor {predictor}");
            }
        }
        // A block whose frame width does not match the block width refuses.
        let odd = Spec { samples: vec![0; 16], compression: 7, lj: (2, 1, 16, 0, 0), ..base() };
        let mut bytes = build(&odd).bytes;
        // (the frame now declares more samples than the block can hold, so admission refuses it)
        for i in 0..bytes.len() - 12 {
            if bytes[i..i + 4] == [0xFF, 0xC3, 0, 14] {
                bytes[i + 8] = 3; // SOF3 samples-per-line 2 -> 3
            }
        }
        assert_eq!(decode_container(&bytes).err(), Some(DevelopError::InvalidDimensions));
    });
}

// ---- allocation tracking for the hostile tiny-file case (per thread: tests run in parallel) ----
mod tracking {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
        static LARGEST: Cell<usize> = const { Cell::new(0) };
    }

    pub fn arm() {
        LARGEST.with(|largest| largest.set(0));
        ARMED.with(|armed| armed.set(true));
    }

    /// Stops tracking and returns the largest single allocation this thread requested since `arm`.
    pub fn disarm() -> usize {
        ARMED.with(|armed| armed.set(false));
        LARGEST.with(Cell::get)
    }

    fn note(size: usize) {
        if ARMED.with(Cell::get) {
            LARGEST.with(|largest| largest.set(largest.get().max(size)));
        }
    }

    struct Tracking;

    unsafe impl GlobalAlloc for Tracking {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note(new_size);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Tracking = Tracking;
}

/// Overwrite the inline value of a single-value LONG/SHORT entry in the raw IFD at `raw_at`.
fn patch_long(bytes: &[u8], raw_at: usize, tag: u16, value: u32) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let at = entry_at(&out, raw_at, tag);
    out[at + 8..at + 12].copy_from_slice(&value.to_le_bytes());
    out
}

fn entry_at(bytes: &[u8], raw_at: usize, tag: u16) -> usize {
    let n = u16::from_le_bytes([bytes[raw_at], bytes[raw_at + 1]]) as usize;
    (0..n)
        .map(|i| raw_at + 2 + 12 * i)
        .find(|at| u16::from_le_bytes([bytes[*at], bytes[*at + 1]]) == tag)
        .unwrap_or_else(|| panic!("tag {tag} not found"))
}

/// Replace a single-value LONG entry by an array of `entries` copies of its value, appended to the
/// end of the file (the file grows by 4 bytes per entry).
fn patch_array(bytes: &[u8], raw_at: usize, tag: u16, entries: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let at = entry_at(&out, raw_at, tag);
    assert_eq!(u16::from_le_bytes([out[at + 2], out[at + 3]]), 4, "LONG entry expected");
    let value = [out[at + 8], out[at + 9], out[at + 10], out[at + 11]];
    let start = out.len();
    for _ in 0..entries {
        out.extend_from_slice(&value);
    }
    out[at + 4..at + 8].copy_from_slice(&(entries as u32).to_le_bytes());
    out[at + 8..at + 12].copy_from_slice(&(start as u32).to_le_bytes());
    out
}

/// Review R5 (CONFIRMED, group A): a tiny lossless-JPEG DNG whose tile and image dimensions declare
/// a 2^28-sample frame used to force the 512 MiB frame allocation (the block-size check only covered
/// uncompressed data). The declared frame must now be refused against the payload before allocating.
#[test]
fn dng_lossless_jpeg_declared_frame_cannot_outgrow_the_payload() {
    bounded_case("dng_lossless_jpeg_declared_frame_cannot_outgrow_the_payload", || {
        let tiled = build(&Spec { tiles: Some((4, 4)), compression: 7, ..base() });
        // ImageWidth, ImageLength, TileWidth, TileLength all 16384: one 16384 x 16384 block (the
        // pixel cap is exactly 2^28), whose single stream is the original 16-sample one.
        let mut hostile = tiled.bytes.clone();
        for tag in [256, 257, 322, 323] {
            hostile = patch_long(&hostile, tiled.raw_at, tag, 16_384);
        }
        assert!(hostile.len() < 4096, "the hostile file must stay tiny ({} bytes)", hostile.len());
        // The tracker itself works: a 2 MiB request is seen.
        tracking::arm();
        drop(Vec::<u8>::with_capacity(2 << 20));
        assert!(tracking::disarm() >= 2 << 20, "allocation tracker saw nothing");
        tracking::arm();
        let result = decode_container(&hostile);
        let largest = tracking::disarm();
        assert!(
            largest < 1 << 20,
            "decoding a {}-byte file requested a {largest}-byte allocation",
            hostile.len()
        );
        assert_eq!(result.err(), Some(DevelopError::InvalidDimensions));
        // The honest file with the same layout still decodes.
        assert!(decode_container(&tiled.bytes).is_ok());
    });
}

/// V4-02 `dims_u32_max`: every dimension field at u32::MAX (alone and together), in both
/// compressions, is a typed refusal with no large allocation.
#[test]
fn dng_dims_u32_max_is_a_typed_refusal_without_allocation() {
    bounded_case("dng_dims_u32_max", || {
        for compression in [1u16, 7] {
            let tiled = build(&Spec { tiles: Some((4, 4)), compression, ..base() });
            for (tags, want) in [
                (&[256u16][..], DevelopError::InvalidDimensions),
                (&[257], DevelopError::InvalidDimensions),
                (&[256, 257], DevelopError::InvalidDimensions),
                (&[322], DevelopError::InvalidContainer),
                (&[323], DevelopError::InvalidContainer),
                (&[322, 323], DevelopError::InvalidContainer),
                (&[256, 257, 322, 323], DevelopError::InvalidDimensions),
            ] {
                let mut hostile = tiled.bytes.clone();
                for tag in tags {
                    hostile = patch_long(&hostile, tiled.raw_at, *tag, u32::MAX);
                }
                tracking::arm();
                let result = decode_container(&hostile);
                let largest = tracking::disarm();
                assert_eq!(result.err(), Some(want), "compression {compression}, tags {tags:?}");
                assert!(largest < 1 << 20, "compression {compression}, tags {tags:?}: {largest}-byte allocation");
            }
        }
    });
}

/// V4-02 `aggregate_over_budget`: individually valid declarations whose aggregate the payload
/// cannot support, in both compressions, are refused before the frame allocation.
#[test]
fn dng_aggregate_over_budget_is_refused_before_allocating() {
    bounded_case("dng_aggregate_over_budget", || {
        // Over the pixel cap by one row of tiles: 16385 x 16384 > 2^28.
        let tiled = build(&Spec { tiles: Some((4, 4)), ..base() });
        let over_cap = patch_long(&patch_long(&tiled.bytes, tiled.raw_at, 256, 16_385), tiled.raw_at, 257, 16_384);
        assert_eq!(decode_container(&over_cap).err(), Some(DevelopError::InvalidDimensions));

        // Uncompressed: 4096 tile entries (256 x 256, 128 KiB each) all pointing at ONE real tile
        // declare a 16384 x 16384 frame (512 MiB) from a file of about 130 KiB. Every block is
        // inside the file, so only the aggregate check stops the allocation (the old code decoded it).
        let one = build(&Spec {
            width: 256,
            height: 256,
            samples: vec![0; 256 * 256],
            tiles: Some((256, 256)),
            ..base()
        });
        let mut bomb = patch_long(&patch_long(&one.bytes, one.raw_at, 256, 16_384), one.raw_at, 257, 16_384);
        bomb = patch_array(&bomb, one.raw_at, 324, 64 * 64);
        bomb = patch_array(&bomb, one.raw_at, 325, 64 * 64);
        assert!(bomb.len() < 200_000, "the file stays small ({} bytes)", bomb.len());
        tracking::arm();
        let result = decode_container(&bomb);
        let largest = tracking::disarm();
        assert_eq!(result.err(), Some(DevelopError::InvalidDimensions));
        assert!(largest < 4 << 20, "uncompressed aggregate bomb requested a {largest}-byte allocation");
        // The honest 256 x 256 file decodes.
        assert!(decode_container(&one.bytes).is_ok());

        // Lossless JPEG: 4096 tile entries (256 x 256) over one 4 x 4 stream: the summed byte
        // counts alone are far below one bit per declared sample.
        let lj = build(&Spec { tiles: Some((4, 4)), compression: 7, ..base() });
        let mut bomb = patch_long(&patch_long(&lj.bytes, lj.raw_at, 256, 16_384), lj.raw_at, 257, 16_384);
        bomb = patch_long(&patch_long(&bomb, lj.raw_at, 322, 256), lj.raw_at, 323, 256);
        bomb = patch_array(&bomb, lj.raw_at, 324, 64 * 64);
        bomb = patch_array(&bomb, lj.raw_at, 325, 64 * 64);
        tracking::arm();
        let result = decode_container(&bomb);
        let largest = tracking::disarm();
        assert_eq!(result.err(), Some(DevelopError::InvalidDimensions));
        assert!(largest < 4 << 20, "lossless JPEG aggregate bomb requested a {largest}-byte allocation");
    });
}
