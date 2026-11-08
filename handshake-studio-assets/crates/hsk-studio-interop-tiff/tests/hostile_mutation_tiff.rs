//! SMB-PARSER-HOSTILE-1 item 6, TIFF target: a deterministic mutation sweep (`support/hostile_mutator.rs`)
//! over files this crate's encoder writes (None, LZW, Deflate, PackBits; RGB 8-bit and Gray 16-bit,
//! with and without the horizontal predictor), read by `decode` and by the container API
//! (`Container::parse` then `walk`), with `Limits::default()` (the production bounds). The older
//! byte sweep in `tiff_codec.rs` stays.
#[path = "support/hostile_mutator.rs"]
mod hostile_mutator;

use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::*;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

fn seeds() -> Vec<Vec<u8>> {
    let rgb = Samples::U8((0..16 * 16 * 3).map(|i| (i * 7 % 251) as u8).collect());
    let gray = Samples::U16((0..12 * 10).map(|i| (i * 523 % 65_521) as u16).collect());
    let mut out = Vec::new();
    for compression in [Compression::None, Compression::Lzw, Compression::Deflate, Compression::PackBits] {
        for (w, h, color, depth, samples, predictor) in [
            (16, 16, ColorModel::Rgb, 8, &rgb, Predictor::None),
            (12, 10, ColorModel::Gray, 16, &gray, Predictor::Horizontal),
        ] {
            let r = EncodeRequest {
                width: w,
                height: h,
                color,
                alpha: Alpha::None,
                bit_depth: depth,
                samples,
                icc: None,
                compression,
                predictor,
                big_tiff: false,
                resolution: None,
                orientation: None,
            };
            out.push(encode(&r).expect("seed encodes"));
        }
    }
    out
}

const PROFILE: Profile<'static> = Profile {
    dictionary: &[b"II*\0", b"MM\0*", b"II+\0", b"\x00\x01", b"\xFF\xFF\xFF\xFF", b"\x01\x01", b"\x11\x01", b"\x17\x01"],
    text: false,
};

fn answer(b: &[u8]) -> Answer {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let decoded = decode(b, &limits, &cancel).map(|_| ()).map_err(|e| e.code());
    let walked = Container::parse(b, &limits).and_then(|c| c.walk()).map(|_| ()).map_err(|e| e.code());
    decoded.and(walked)
}

#[test]
fn tiff_mutation_sweep() {
    let seeds = seeds();
    let report = sweep("tiff", 0x7111_FF00_0000_0001, &seeds, 250, PROFILE, CAP, answer);
    report.assert_reach(97);
}
