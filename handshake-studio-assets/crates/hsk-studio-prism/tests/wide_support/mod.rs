//! Synthetic ICC profile writer for the wide-path tests: no external or installed profile bytes.
#![allow(dead_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_prism::*;

pub const SRGB: &[u8] = include_bytes!("../profiles/sRGB-v4.icc");
pub const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

pub fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
pub fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}
pub fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        t.extend_from_slice(&s15(c));
    }
    t
}
/// `curv` with a gamma exponent (u8Fixed8) or, for `None`, the identity (count 0).
pub fn curv(gamma: Option<f64>) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    match gamma {
        None => t.extend_from_slice(&be32(0)),
        Some(g) => {
            t.extend_from_slice(&be32(1));
            t.extend_from_slice(&((g * 256.0).round() as u16).to_be_bytes());
            t.extend_from_slice(&[0, 0]);
        }
    }
    t
}
/// Assemble a profile; `tags` reference blobs by index so several tags may share one table.
pub fn assemble(
    version: u32,
    class: &[u8; 4],
    space: &[u8; 4],
    pcs: &[u8; 4],
    blobs: &[Vec<u8>],
    tags: &[(&[u8; 4], usize)],
) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[8..12].copy_from_slice(&version.to_be_bytes());
    out[12..16].copy_from_slice(class);
    out[16..20].copy_from_slice(space);
    out[20..24].copy_from_slice(pcs);
    out[36..40].copy_from_slice(b"acsp");
    for (i, v) in D50.into_iter().enumerate() {
        out[68 + i * 4..72 + i * 4].copy_from_slice(&s15(v));
    }
    out.extend_from_slice(&be32(tags.len() as u32));
    let mut offsets = Vec::new();
    let mut at = 132 + tags.len() * 12;
    for blob in blobs {
        offsets.push(at);
        at += blob.len().div_ceil(4) * 4;
    }
    for (sig, blob) in tags {
        out.extend_from_slice(*sig);
        out.extend_from_slice(&be32(offsets[*blob] as u32));
        out.extend_from_slice(&be32(blobs[*blob].len() as u32));
    }
    for blob in blobs {
        out.extend_from_slice(blob);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    let len = out.len() as u32;
    out[..4].copy_from_slice(&be32(len));
    out
}
/// Legacy (v2) 16-bit Lab encoding: L 0xFF00 = 100, a/b 0x8000 = 0 at 1/256 per unit.
pub fn lab16(l: f64, a: f64, b: f64) -> [u16; 3] {
    let q = |v: f64| v.round().clamp(0.0, 65535.0) as u16;
    [q(l / 100.0 * 65280.0), q((a + 128.0) * 256.0), q((b + 128.0) * 256.0)]
}
/// `mft2` with identity 2-entry input and output tables and an identity matrix.
pub fn mft2(in_ch: u8, out_ch: u8, grid: u8, clut: &[u16]) -> Vec<u8> {
    assert_eq!(
        clut.len(),
        (grid as usize).pow(in_ch as u32) * out_ch as usize
    );
    let mut t = b"mft2\0\0\0\0".to_vec();
    t.extend_from_slice(&[in_ch, out_ch, grid, 0]);
    for v in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        t.extend_from_slice(&s15(v));
    }
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    for _ in 0..in_ch {
        t.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
    }
    for v in clut {
        t.extend_from_slice(&v.to_be_bytes());
    }
    for _ in 0..out_ch {
        t.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
    }
    t
}
/// CMYK -> Lab table of `grid`^4 nodes; `lab(c, m, y, k)` takes 0..1 inks.
pub fn cmyk_a2b(grid: u8, lab: impl Fn(f64, f64, f64, f64) -> (f64, f64, f64)) -> Vec<u8> {
    let g = grid as usize;
    let mut clut = Vec::with_capacity(g.pow(4) * 3);
    let f = |i: usize| i as f64 / (g - 1) as f64;
    for c in 0..g {
        for m in 0..g {
            for y in 0..g {
                for k in 0..g {
                    let (l, a, b) = lab(f(c), f(m), f(y), f(k));
                    clut.extend_from_slice(&lab16(l, a, b));
                }
            }
        }
    }
    mft2(4, 3, grid, &clut)
}
/// Lab -> CMYK table of `grid`^3 nodes; `ink(l, a, b)` takes the legacy-encoded node's Lab.
pub fn cmyk_b2a(grid: u8, ink: impl Fn(f64, f64, f64) -> [f64; 4]) -> Vec<u8> {
    let g = grid as usize;
    let mut clut = Vec::with_capacity(g.pow(3) * 4);
    for li in 0..g {
        for ai in 0..g {
            for bi in 0..g {
                let x = |i: usize| i as f64 / (g - 1) as f64;
                // Normalised table coordinate -> legacy Lab16 value -> Lab.
                let l = x(li) * 65535.0 / 65280.0 * 100.0;
                let a = x(ai) * 65535.0 / 256.0 - 128.0;
                let b = x(bi) * 65535.0 / 256.0 - 128.0;
                for v in ink(l, a, b) {
                    clut.push((v.clamp(0.0, 1.0) * 65535.0).round() as u16);
                }
            }
        }
    }
    mft2(3, 4, grid, &clut)
}
/// ICC v2 CMYK printer profile with Lab PCS from the given tables (one table serves every intent).
pub fn cmyk_profile(a2b: Vec<u8>, b2a: Option<Vec<u8>>, intents: usize, white: [f64; 3]) -> Vec<u8> {
    let mut blobs = vec![xyz_tag(white), a2b];
    let mut tags: Vec<(&[u8; 4], usize)> = vec![(b"wtpt", 0)];
    let a_sigs: [&[u8; 4]; 3] = [b"A2B0", b"A2B1", b"A2B2"];
    for sig in a_sigs.iter().take(intents) {
        tags.push((sig, 1));
    }
    if let Some(b2a) = b2a {
        blobs.push(b2a);
        let b_sigs: [&[u8; 4]; 3] = [b"B2A0", b"B2A1", b"B2A2"];
        for sig in b_sigs.iter().take(intents) {
            tags.push((sig, 2));
        }
    }
    assemble(0x0210_0000, b"prtr", b"CMYK", b"Lab ", &blobs, &tags)
}
/// v2.1 RGB matrix profile with `curv` gamma curves (D50 colorants of sRGB).
pub fn matrix_v2(gamma: f64) -> Vec<u8> {
    let blobs = vec![
        xyz_tag(D50),
        xyz_tag([0.4360, 0.2225, 0.0139]),
        xyz_tag([0.3851, 0.7169, 0.0971]),
        xyz_tag([0.1431, 0.0606, 0.7141]),
        curv(Some(gamma)),
    ];
    assemble(
        0x0210_0000,
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &blobs,
        &[
            (b"wtpt", 0),
            (b"rXYZ", 1),
            (b"gXYZ", 2),
            (b"bXYZ", 3),
            (b"rTRC", 4),
            (b"gTRC", 4),
            (b"bTRC", 4),
        ],
    )
}
/// v4 RGB `mAB ` profile (A2B only, identity curves, no matrix) whose `grid`^3 CLUT maps device RGB
/// to PCS XYZ by `xyz(r, g, b)`; XYZ is stored u1Fixed15.
pub fn mab_rgb_profile(grid: u8, xyz: impl Fn(f64, f64, f64) -> [f64; 3]) -> Vec<u8> {
    let g = grid as usize;
    let identity = curv(None);
    let mut t = b"mAB \0\0\0\0".to_vec();
    t.extend_from_slice(&[3, 3, 0, 0]);
    let (b_off, clut_off) = (32u32, 32 + 3 * identity.len() as u32);
    let mut clut = vec![g as u8, g as u8, g as u8];
    clut.resize(16, 0);
    clut.extend_from_slice(&[2, 0, 0, 0]);
    let f = |i: usize| i as f64 / (g - 1) as f64;
    for r in 0..g {
        for gg in 0..g {
            for b in 0..g {
                for v in xyz(f(r), f(gg), f(b)) {
                    clut.extend_from_slice(&((v * 32768.0).round().clamp(0.0, 65535.0) as u16).to_be_bytes());
                }
            }
        }
    }
    let a_off = clut_off + clut.len() as u32;
    for off in [b_off, 0, 0, clut_off, a_off] {
        t.extend_from_slice(&be32(off));
    }
    for _ in 0..3 {
        t.extend_from_slice(&identity);
    }
    t.extend_from_slice(&clut);
    for _ in 0..3 {
        t.extend_from_slice(&identity);
    }
    let blobs = vec![xyz_tag(D50), t];
    assemble(
        0x0420_0000,
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &blobs,
        &[(b"wtpt", 0), (b"A2B0", 1), (b"A2B1", 1)],
    )
}

pub struct Ctx {
    pub actor: ActorContext,
    pub source: DomainId,
    pub destination: DomainId,
    pub resource: DomainId,
}
impl Ctx {
    pub fn new() -> Self {
        Self {
            actor: ActorContext::new("account", "principal", "owner-account", "owner-principal", "space", "session")
                .unwrap(),
            source: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
            destination: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000002").unwrap(),
            resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000003").unwrap(),
        }
    }
    pub fn input<'a>(&'a self, id: &'a DomainId, bytes: &'a [u8]) -> ProfileInput<'a> {
        ProfileInput {
            profile_id: id,
            bytes,
            expected_sha256: profile_hash(bytes),
        }
    }
    pub fn request<'a>(
        &'a self,
        source: &'a [u8],
        destination: &'a [u8],
        pixels: &'a [f32],
        intent: Intent,
        bpc: bool,
    ) -> WideTransformRequest<'a> {
        WideTransformRequest {
            source: self.input(&self.source, source),
            destination: self.input(&self.destination, destination),
            pixels,
            intent,
            black_point_compensation: bpc,
            revision: 1,
            expected_revision: 1,
            correlation_id: 7,
            resource_id: &self.resource,
            actor: &self.actor,
        }
    }
}
pub struct Collect(pub Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
pub fn engine(ctx: &Ctx) -> WideEngine {
    WideEngine::new(ctx.actor.clone(), 4).unwrap()
}
pub fn run(
    engine: &mut WideEngine,
    ctx: &Ctx,
    source: &[u8],
    destination: &[u8],
    pixels: &[f32],
    intent: Intent,
    bpc: bool,
) -> Result<WideTransformResult, WideFailure> {
    engine.transform(
        &ctx.request(source, destination, pixels, intent, bpc),
        &CancellationToken::default(),
        &mut Collect(Vec::new()),
    )
}
/// sRGB encoding of a linear value (IEC 61966-2-1), f64.
pub fn srgb_encode(v: f64) -> f64 {
    if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}
/// Y of a neutral with CIELAB lightness `l`.
pub fn y_of_l(l: f64) -> f64 {
    if l > 8.0 { ((l + 16.0) / 116.0).powi(3) } else { l * ((8.0 + 16.0) / 116.0_f64).powi(3) / 8.0 }
}
