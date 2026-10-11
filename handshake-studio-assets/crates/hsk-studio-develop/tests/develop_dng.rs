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
}

struct Built {
    bytes: Vec<u8>,
    ifd0_at: usize,
    ifd0_entries: usize,
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
    let blocks: Vec<Vec<u8>> = match s.tiles {
        Some((tw, th)) => {
            let (tw, th) = (tw as usize, th as usize);
            let mut out = Vec::new();
            for ty in 0..h.div_ceil(th) {
                for tx in 0..w.div_ceil(tw) {
                    let mut b = Vec::new();
                    for y in 0..th {
                        for x in 0..tw {
                            let (sx, sy) = (tx * tw + x, ty * th + y);
                            let v = if sx < w && sy < h { s.samples[sy * w + sx] } else { 0 };
                            b.extend(sample_bytes(v));
                        }
                    }
                    out.push(b);
                }
            }
            out
        }
        None => (0..h / 2)
            .map(|strip| (0..2 * w).flat_map(|i| sample_bytes(s.samples[strip * 2 * w + i])).collect())
            .collect(),
    };
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
    Built { bytes, ifd0_at, ifd0_entries: t0.len() }
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

fn develop_pixels(raw: &DngRaw, window: PlaneWindow, tile: u32) -> Vec<DevelopedTile> {
    let (plane, _) = raw.cfa_plane(window, RangePolicy::Refuse, 7, Limits::SPEC_V1).unwrap();
    let (recipe, _) = raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile()).unwrap();
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
        // neutral (0.5, 1, 0.8): gains 2, 1, 1.25 are folded out of the matrix, so a camera pixel
        // equal to the neutral maps to the same white as neutral (1,1,1) does for (1,1,1).
        let warm = Spec { neutral: Some([0.5, 1.0, 0.8]), ..base() };
        let raw = decode_container(&build(&warm).bytes).unwrap();
        let (recipe, sol) = raw.recipe(&color::xyz_d50_to_linear_srgb().unwrap(), profile()).unwrap();
        assert_eq!(recipe.as_shot_neutral, [0.5, 1.0, 0.8]);
        let gain = hsk_studio_develop::recipe::as_shot_gain([0.5, 1.0, 0.8]).unwrap();
        let balanced = [0.5 * gain[0], 1.0 * gain[1], 0.8 * gain[2]];
        assert!(balanced.iter().all(|v| (v - balanced[0]).abs() < 1e-12));
        let rgb = color::mul_vec(&recipe.camera_to_profile_rgb, &balanced);
        let expect = color::mul_vec(&color::mul(&color::xyz_d50_to_linear_srgb().unwrap(), &sol.camera_to_xyz_d50), &[0.5, 1.0, 0.8]);
        for i in 0..3 {
            assert!((rgb[i] - expect[i]).abs() < 1e-9, "channel {i}");
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
    });
}

fn at_plane(plane: &CfaPlane, x: usize, y: usize) -> u16 {
    plane.sample(x, y)
}

#[test]
fn dng_refusals() {
    bounded_case("dng_refusals", || {
        let refuse = |bytes: &[u8], want: DevelopError| assert_eq!(decode_container(bytes).err(), Some(want));
        refuse(&build(&Spec { dng_version: None, ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { backward: [2, 0, 0, 0], ..base() }).bytes, DevelopError::UnsupportedContainer);
        refuse(&build(&Spec { compression: 7, ..base() }).bytes, DevelopError::UnsupportedCompression);
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
        refuse(b"RIFF....WEBP", DevelopError::DecoderUnavailable);
        refuse(b"II*\0\x08\0\0\0", DevelopError::InvalidContainer);
        // linearization: table maps raw values; WhiteLevel must then be explicit.
        let table: Vec<u16> = (0..=100u16).map(|i| i * 5).collect();
        let lin = Spec { samples: vec![20; 16], lin_table: Some(table.clone()), white: Some(500), ..base() };
        let raw = decode_container(&build(&lin).bytes).unwrap();
        assert_eq!((raw.sample(2, 3), raw.linearized), (100, true));
        refuse(&build(&Spec { lin_table: Some(table), white: None, ..lin }).bytes, DevelopError::InvalidContainer);
    });
}
