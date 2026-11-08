//! SMB-PARSER-HOSTILE-1 item 6, PSD target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over byte-level fixtures built with
//! `tests/common` (pixel layers with raw and RLE channels, a merged RLE image) and files
//! `compose_psd` writes (lead ruling 2026-11-08: groups, Unicode `luni` names, `iOpa`, and a PSB
//! wider than 30000 px), read by `read_psd`, `decode_merged` and `to_proposal` with
//! `Limits::default()`. No outer CRC: plain bytes reach every section.
mod common;
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;

use common::*;
use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Resolution, Resolver, TileRef};
use hsk_studio_interop_psd::*;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;
/// Reach floor of the built-layer sweep: half the ok answers the first run measured (lane notes).
const PSD_BUILT_FLOOR: usize = 25;

struct Mint(u64);
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.0)
    }
}
impl IdMint for Mint {
    fn document_id(&mut self) -> String {
        self.next("SDOC")
    }
    fn artboard_id(&mut self) -> String {
        self.next("SART")
    }
    fn layer_id(&mut self) -> String {
        self.next("SLYR")
    }
    fn linear_working_profile_id(&mut self) -> String {
        self.next("SCPF")
    }
    fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
        format!("manifest/{layer_id}/{object_key}")
    }
}

struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn look(name: &str, fill_opacity: u8, blend: StudioBlendMode) -> ComposeLook {
    ComposeLook { name: name.to_owned(), opacity: 200, fill_opacity, blend, visible: true, clipped: false }
}

fn composed(width: u32, height: u32) -> Vec<u8> {
    let px = (width * height) as usize;
    let rgba: Vec<u8> = (0..px * 4).map(|i| (i * 37 % 256) as u8).collect();
    let req = ComposeRequest {
        width,
        height,
        merged: ComposeSamples::U8(rgba.clone()),
        merged_alpha: true,
        icc: None,
        records: vec![
            ComposeRecord::GroupEnd,
            ComposeRecord::Pixels { look: look("Ébauche ✓", 128, StudioBlendMode::Multiply), left: 0, top: 0, width, height, rgba: ComposeSamples::U8(rgba) },
            ComposeRecord::GroupOpen { look: look("Gruppe ü", 255, StudioBlendMode::PassThrough) },
        ],
    };
    compose_psd(&req, &Limits::default()).expect("seed composes")
}

fn built() -> Vec<u8> {
    let mut a = TLayer::new("raw", [0, 0, 2, 2]);
    a.channels = vec![(-1, 0, vec![255; 4]), (0, 0, vec![1, 2, 3, 4]), (1, 0, vec![5, 6, 7, 8]), (2, 0, vec![9, 10, 11, 12])];
    let mut b = TLayer::new("rle", [0, 0, 2, 2]);
    b.key = *b"scrn";
    let rows: [&[u8]; 2] = [&[7, 7], &[8, 9]];
    b.channels = vec![(0, 1, rle(&rows, false)), (1, 1, rle(&rows, false)), (2, 1, rle(&rows, false))];
    b.blocks = vec![layer_block(b"luni", &unicode("rle ü"), false)];
    let info = layer_info(&[a, b], false, true);
    // The merged image raw (planar, 3 channels of 2 x 2).
    let merged: Vec<u8> = (1..=12).collect();
    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: resource(1005, "", &[0; 16]),
        layer_section: layer_section(&info, &[], false),
        image: Some((0, merged)),
    }
    .bytes()
}

const PROFILE: Profile<'static> = Profile { dictionary: &[b"8BIM", b"8B64", b"Lr16", b"Lr32", b"Patt", b"lfx2", b"luni", b"lsct", b"iOpa", b"8BPS"], text: false };

fn answer(b: &[u8]) -> Answer {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let doc = read_psd(b, &limits, &cancel).map_err(|e| e.code())?;
    doc.decode_merged(&limits, &cancel).map_err(|e| e.code())?;
    let budget = Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() };
    let cfg = ProposalConfig::new(&limits, budget, &Host);
    to_proposal(&doc, &SourceIdentity::of(b), &cfg, &mut Mint(0), None, &cancel).map(|_| ()).map_err(|e| e.code())
}

#[test]
fn psd_mutation_sweep_built_layers() {
    let seeds = vec![built()];
    for s in &seeds {
        assert_eq!(answer(s), Ok(()), "seed reads");
    }
    let report = sweep("psd_built", 0x05D0_0000_0000_0001, &seeds, 600, PROFILE, CAP, answer);
    report.assert_reach(PSD_BUILT_FLOOR);
}

#[test]
fn psd_mutation_sweep_composed() {
    // A group with a Unicode-named pixel layer and fill opacity, and a PSB wider than 30000 px.
    let seeds = vec![composed(3, 2), composed(30_001, 1)];
    for s in &seeds {
        assert_eq!(answer(s), Ok(()), "seed reads");
    }
    let report = sweep("psd_composed", 0x05D0_0000_0000_0002, &seeds, 300, PROFILE, CAP, answer);
    report.assert_reach(41);
}
