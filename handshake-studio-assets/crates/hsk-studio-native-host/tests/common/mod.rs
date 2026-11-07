//! Shared GUI fixtures: a layered PSD assembled byte by byte (independent of the importer; pattern of
//! hsk-studio-harness tests/common), opened with `Viewer::open`, saved with `Viewer::save` as .handshake. The repo's
//! demo.handshake stays the edit fixture of the harness tests and is never used here.
#![allow(dead_code)]
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use std::path::{Path, PathBuf};

/// Scratch directory under TMP (studio-env.sh puts TMP on D:), removed on drop.
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-native-host-gui-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One opaque PSD layer: rect [top, left, bottom, right] in canvas px.
#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub rect: [i32; 4],
    pub rgb: [u8; 3],
    pub hidden: bool,
}

pub const W: u32 = 160;
pub const H: u32 = 120;

/// Bottom first: Blue, Green, Red (opaque, distinct colours at distinct rects) and the hidden Sun on top.
pub fn layers() -> Vec<Layer> {
    let l = |name: &str, rect: [i32; 4], rgb: [u8; 3], hidden: bool| Layer { name: name.into(), rect, rgb, hidden };
    vec![
        l("Blue", [60, 20, 110, 90], [40, 80, 220], false),
        l("Green", [30, 60, 90, 120], [60, 200, 60], false),
        l("Red", [10, 10, 60, 70], [220, 40, 40], false),
        l("Sun", [0, 100, 40, 160], [250, 220, 0], true),
    ]
}

/// A doc pixel only Red covers (x, y).
pub const RED_ONLY: (u32, u32) = (15, 20);

/// RGB 8-bit PSD with raw-channel opaque layers (bottom first); hidden layers carry flag bit 1.
pub fn psd(width: u32, height: u32, layers: &[Layer]) -> Vec<u8> {
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
    let px = |l: &Layer| ((l.rect[2] - l.rect[0]) * (l.rect[3] - l.rect[1])) as usize;
    for l in layers {
        for r in l.rect {
            info.extend(r.to_be_bytes());
        }
        info.extend(3u16.to_be_bytes());
        for id in 0i16..3 {
            info.extend(id.to_be_bytes());
            info.extend(((2 + px(l)) as u32).to_be_bytes());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend([255u8, 0, if l.hidden { 0x0A } else { 0x08 }, 0]);
        let mut extra = 0u32.to_be_bytes().to_vec();
        extra.extend(0u32.to_be_bytes());
        let name = &l.name.as_bytes()[..l.name.len().min(255)];
        let mut pascal = vec![name.len() as u8];
        pascal.extend_from_slice(name);
        while pascal.len() % 4 != 0 {
            pascal.push(0);
        }
        extra.extend(pascal);
        info.extend((extra.len() as u32).to_be_bytes());
        info.extend(extra);
    }
    for l in layers {
        for c in l.rgb {
            info.extend(0u16.to_be_bytes());
            info.extend(std::iter::repeat_n(c, px(l)));
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
    v.extend(0u16.to_be_bytes());
    v.extend(std::iter::repeat_n(10u8, (width * height * 3) as usize));
    v
}

/// Writes the layered PSD, opens it with `Viewer::open`, saves it as `layered.handshake` with `Viewer::save`.
pub fn fixture(dir: &Path) -> PathBuf {
    let source = dir.join("layered.psd");
    std::fs::write(&source, psd(W, H, &layers())).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("open psd: {e}"));
    let out = dir.join("layered.handshake");
    v.save(&out, false).unwrap_or_else(|e| panic!("save: {e}"));
    out
}
