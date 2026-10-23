//! Shared viewer test fixtures (SMB-HARNESS-UI tests): scratch directory and the synthetic
//! two-layer PSD (independent byte assembly).
#![allow(dead_code)]
use std::path::PathBuf;

/// Test scratch directory under TMP, removed on drop. TMP must be on D: (studio-env.sh sets it).
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-viewer-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 2x2 RGB 8-bit PSD, raw channels: `base` fills the canvas with (10, 20, 30); `top` covers the
/// right column with (200, 100, 0) at alpha 128 (row 0) and (201, 101, 1) at alpha 255 (row 1).
pub fn two_layer_psd() -> Vec<u8> {
    layered_psd(
        2,
        2,
        &[
            L { name: "base", rect: [0, 0, 2, 2], channels: vec![(0, vec![10; 4]), (1, vec![20; 4]), (2, vec![30; 4])] },
            L { name: "top", rect: [0, 1, 2, 2], channels: vec![(-1, vec![128, 255]), (0, vec![200, 201]), (1, vec![100, 101]), (2, vec![0, 1])] },
        ],
    )
}

/// One PSD layer: name, rect [top, left, bottom, right], raw channels (id, bytes).
pub struct L {
    pub name: &'static str,
    pub rect: [i32; 4],
    pub channels: Vec<(i16, Vec<u8>)>,
}

/// One opaque `width x height` RGB layer named `name` filled with `rgb`.
pub fn solid_psd(width: u32, height: u32, name: &'static str, rgb: [u8; 3]) -> Vec<u8> {
    let n = (width * height) as usize;
    layered_psd(width, height, &[L { name, rect: [0, 0, height as i32, width as i32], channels: vec![(0, vec![rgb[0]; n]), (1, vec![rgb[1]; n]), (2, vec![rgb[2]; n])] }])
}

/// RGB 8-bit PSD of `width x height` with raw-channel layers (bottom first).
pub fn layered_psd(width: u32, height: u32, layers: &[L]) -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(height.to_be_bytes());
    v.extend(width.to_be_bytes());
    v.extend(8u16.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let mut info = (layers.len() as i16).to_be_bytes().to_vec();
    for l in layers {
        for r in l.rect {
            info.extend(r.to_be_bytes());
        }
        info.extend((l.channels.len() as u16).to_be_bytes());
        for (id, data) in &l.channels {
            info.extend(id.to_be_bytes());
            info.extend(((2 + data.len()) as u32).to_be_bytes());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend([255u8, 0, 0x08, 0]);
        let mut extra = 0u32.to_be_bytes().to_vec();
        extra.extend(0u32.to_be_bytes());
        let mut pascal = vec![l.name.len() as u8];
        pascal.extend_from_slice(l.name.as_bytes());
        while pascal.len() % 4 != 0 {
            pascal.push(0);
        }
        extra.extend(pascal);
        info.extend((extra.len() as u32).to_be_bytes());
        info.extend(extra);
    }
    for l in layers {
        for (_, data) in &l.channels {
            info.extend(0u16.to_be_bytes());
            info.extend_from_slice(data);
        }
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = (info.len() as u32).to_be_bytes().to_vec();
    section.extend(info);
    section.extend(0u32.to_be_bytes());
    v.extend((section.len() as u32).to_be_bytes());
    v.extend(section);
    // Raw composite image data: 3 planes of width x height (content irrelevant to the importers'
    // layer path; filled with 10).
    v.extend(0u16.to_be_bytes());
    v.extend(std::iter::repeat_n(10u8, (width * height * 3) as usize));
    v
}

// ---- synthetic DNG (little-endian TIFF, raw in IFD0, one uncompressed 16-bit strip) -------------

/// sRGB -> XYZ D50 colorants (ICC sRGB / Lindbloom D50), literal.
const SRGB_D50: [f64; 9] = [0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045, 0.7141733];

fn entry(out: &mut Vec<u8>, extra: &mut Vec<u8>, extra_at: usize, tag: u16, typ: u16, count: u32, data: Vec<u8>) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&typ.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    if data.len() <= 4 {
        let mut field = data;
        field.resize(4, 0);
        out.extend_from_slice(&field);
    } else {
        out.extend_from_slice(&((extra_at + extra.len()) as u32).to_le_bytes());
        extra.extend_from_slice(&data);
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
    }
}

fn srational(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| [((x * 1e7).round() as i32).to_le_bytes(), 10_000_000i32.to_le_bytes()].concat()).collect()
}

/// A `width` x `height` RGGB DNG 1.4 (black 64, white 4095, as-shot neutral, D50 forward matrix)
/// with a smooth per-channel gradient, so every slider visibly changes pixels. The red and blue
/// levels reach past white after the as-shot gains: the developed values exceed 1 and the viewer
/// shows them through the renderers' declared display transform Clip (lane SMB-RENDER-SCENE-DISPLAY).
pub fn synthetic_dng(width: u32, height: u32) -> Vec<u8> {
    let mut samples = Vec::with_capacity((width * height * 2) as usize);
    for y in 0..height {
        for x in 0..width {
            let base = match (x % 2, y % 2) {
                (0, 0) => 300 + (x * 9) % 2600,          // R
                (1, 1) => 200 + (y * 11) % 2400,         // B
                _ => 400 + ((x + y) * 5) % 3000,         // G
            };
            samples.extend_from_slice(&(base as u16).to_le_bytes());
        }
    }
    let inverse = invert3(&SRGB_D50);
    let tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (254, 4, 1, 0u32.to_le_bytes().to_vec()),
        (256, 4, 1, width.to_le_bytes().to_vec()),
        (257, 4, 1, height.to_le_bytes().to_vec()),
        (258, 3, 1, 16u16.to_le_bytes().to_vec()),
        (259, 3, 1, 1u16.to_le_bytes().to_vec()),
        (262, 3, 1, 32803u16.to_le_bytes().to_vec()),
        (273, 4, 1, vec![0; 4]), // strip offset, patched below
        (277, 3, 1, 1u16.to_le_bytes().to_vec()),
        (278, 4, 1, height.to_le_bytes().to_vec()),
        (279, 4, 1, (samples.len() as u32).to_le_bytes().to_vec()),
        (33421, 3, 2, [2u16.to_le_bytes(), 2u16.to_le_bytes()].concat()),
        (33422, 1, 4, vec![0, 1, 1, 2]),
        (50706, 1, 4, vec![1, 4, 0, 0]),
        (50707, 1, 4, vec![1, 1, 0, 0]),
        (50708, 2, 12, b"Studio Test\0".to_vec()),
        (50714, 3, 1, 64u16.to_le_bytes().to_vec()),
        (50717, 4, 1, 4095u32.to_le_bytes().to_vec()),
        (50721, 10, 9, srational(&inverse)),
        (50728, 5, 3, [0.62f64, 1.0, 0.81].iter().flat_map(|v| [((v * 1e6).round() as u32).to_le_bytes(), 1_000_000u32.to_le_bytes()].concat()).collect()),
        (50778, 3, 1, 23u16.to_le_bytes().to_vec()),
        (50964, 10, 9, srational(&SRGB_D50)),
    ];
    let ifd_at = 8usize;
    let extra_at = ifd_at + 2 + 12 * tags.len() + 4;
    let mut ifd = (tags.len() as u16).to_le_bytes().to_vec();
    let mut extra = Vec::new();
    let mut strip_entry = 0usize;
    for (tag, typ, count, data) in tags {
        if tag == 273 {
            strip_entry = ifd.len() + 8;
        }
        entry(&mut ifd, &mut extra, extra_at, tag, typ, count, data);
    }
    ifd.extend_from_slice(&0u32.to_le_bytes());
    let strip_at = (extra_at + extra.len()) as u32;
    ifd[strip_entry..strip_entry + 4].copy_from_slice(&strip_at.to_le_bytes());
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&(ifd_at as u32).to_le_bytes());
    out.extend_from_slice(&ifd);
    out.extend_from_slice(&extra);
    out.extend_from_slice(&samples);
    out
}

/// Inverse of a row-major 3x3 matrix (cofactors / determinant).
fn invert3(m: &[f64; 9]) -> [f64; 9] {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    let c = [
        m[4] * m[8] - m[5] * m[7], m[2] * m[7] - m[1] * m[8], m[1] * m[5] - m[2] * m[4],
        m[5] * m[6] - m[3] * m[8], m[0] * m[8] - m[2] * m[6], m[2] * m[3] - m[0] * m[5],
        m[3] * m[7] - m[4] * m[6], m[1] * m[6] - m[0] * m[7], m[0] * m[4] - m[1] * m[3],
    ];
    c.map(|v| v / det)
}
