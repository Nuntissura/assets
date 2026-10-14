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

