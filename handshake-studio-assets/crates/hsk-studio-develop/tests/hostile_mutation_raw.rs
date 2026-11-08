//! SMB-PARSER-HOSTILE-1 item 6, camera RAW target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over synthetic DNG, NEF (plain and
//! Huffman), ARW, CR2, RAF, CR3, RW2 and ORF files (`support/hostile_raw_seeds.rs`, constructors
//! copied from this crate's format tests), read by `decode_container` and `exif_subset`. Plain
//! bytes: no outer checksum.
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;
#[path = "support/hostile_raw_seeds.rs"]
mod hostile_raw_seeds;

use hostile_mutator::*;
use hsk_studio_develop::{decode_container, exif_subset};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

const PROFILE: Profile<'static> = Profile {
    dictionary: &[b"II*\0", b"MM\0*", b"IIRS", b"IIU\0", b"FUJIFILMCCD-RAW ", b"ftypcrx ", b"\xFF\xD8", b"\xFF\xC3", b"\xFF\xC4", b"\x00\x01\x00\x00", b"\xFF\xFF\xFF\xFF"],
    text: false,
};

fn answer(b: &[u8]) -> Answer {
    let _ = std::hint::black_box(exif_subset(b));
    decode_container(b).map(|_| ()).map_err(|e| e.code())
}

#[test]
fn raw_seeds_decode() {
    for (name, bytes) in hostile_raw_seeds::seeds() {
        assert_eq!(answer(&bytes), Ok(()), "seed {name}");
    }
}

#[test]
fn raw_mutation_sweep_tiff_family() {
    let seeds: Vec<Vec<u8>> = hostile_raw_seeds::seeds().into_iter().filter(|(n, _)| !n.starts_with("raf") && !n.starts_with("cr3")).map(|(_, b)| b).collect();
    let report = sweep("raw_tiff_family", 0x4A30_0000_0000_0001, &seeds, 60, PROFILE, CAP, answer);
    report.assert_reach(54);
}

#[test]
fn raw_mutation_sweep_raf_cr3() {
    let seeds: Vec<Vec<u8>> = hostile_raw_seeds::seeds().into_iter().filter(|(n, _)| n.starts_with("raf") || n.starts_with("cr3")).map(|(_, b)| b).collect();
    let report = sweep("raw_raf_cr3", 0x4A30_0000_0000_0002, &seeds, 150, PROFILE, CAP, answer);
    report.assert_reach(17);
}
