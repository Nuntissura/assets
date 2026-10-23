//! SMB-AFFINITY-INTEROP-1 slice 2: KA container reader on synthetic containers built from the documented
//! layout (tests/common), hostile inputs, and the AFDesignLoad MIT V1 designs from the oracle corpus.
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_affinity::{
    AffinityError, BlobKind, Codec, DirectoryKind, EntryState, Generation, KaContainer, Limits, identify,
};
use std::path::{Path, PathBuf};

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 7) as u8)).collect()
}

#[test]
fn v1_v2_v3_containers_parse_decode_and_crc_check_at_the_head_revision() {
    let cancel = CancellationToken::default();
    let doc = pattern(5000, 3);
    // V1: #FAT, zlib (legacy compression codes), one folder.
    let mut v1 = Build::doc(9, b"#FAT", vec![Spec::new(1, "doc.dat", doc.clone(), ZLIB), Spec::new(2, "d/1", pattern(300, 9), STORED)]);
    v1.folders = vec![("d/", 1)];
    // V2: #FT4 zstd + a byte-delta predicted zstd stream.
    let mut tile = Spec::new(2, "d/7", pattern(4096, 1), ZSTD | 0x40);
    tile.byte_delta = true;
    let v2 = Build::doc(11, b"#FT4", vec![Spec::new(1, "doc.dat", doc.clone(), ZSTD), tile]);
    // V3: two revisions; revision 2 replaces doc.dat (unnamed state 1), deletes d/1 and adds d/2.
    let mut v3 = Build::doc(12, b"#FT4", vec![Spec::new(1, "doc.dat", pattern(64, 0), ZSTD), Spec::new(2, "d/1", pattern(10, 2), STORED)]);
    v3.flags = 4;
    let deleted = Spec { deleted: true, ..Spec::new(2, "d/1", Vec::new(), 0) };
    let updated = Spec { name: None, ..Spec::new(1, "doc.dat", doc.clone(), ZSTD) };
    v3.revisions.push(Revision { created: 1_760_000_100, streams: vec![updated, deleted, Spec::new(3, "d/2", pattern(70, 4), ZLIB)] });

    for (b, generation, kind) in [(&v1, Generation::V1, DirectoryKind::Fat), (&v2, Generation::V2, DirectoryKind::Ft4), (&v3, Generation::V3, DirectoryKind::Ft4)] {
        let bytes = build(b);
        assert_eq!(identify(&bytes), Some((b.version, *b"Prsn")));
        let c = KaContainer::parse(&bytes, &Limits::default()).unwrap();
        assert_eq!((c.header.generation(), c.header.is_document(), c.header.class_text()), (generation, true, "Prsn".to_string()));
        assert_eq!(c.directories[0].kind, kind);
        assert_eq!(c.read_stream("doc.dat", &cancel).unwrap(), doc, "{generation:?}");
    }
    let c1 = build(&v1);
    let c1 = KaContainer::parse(&c1, &Limits::default()).unwrap();
    assert_eq!(c1.stream("doc.dat").unwrap().entry.codec(), Codec::Zlib);
    assert_eq!(c1.directories[0].folders[0].name, "d/");
    let c2 = build(&v2);
    let c2 = KaContainer::parse(&c2, &Limits::default()).unwrap();
    let s = c2.stream("d/7").unwrap();
    assert_eq!((s.entry.codec(), s.entry.predictor()), (Codec::Zstd, 1));
    assert_eq!(c2.read_stream("d/7", &cancel).unwrap(), pattern(4096, 1), "byte-delta predictor inverted");
    let c3 = build(&v3);
    let c3 = KaContainer::parse(&c3, &Limits::default()).unwrap();
    assert_eq!(c3.directories.len(), 2);
    let names: Vec<_> = c3.streams().into_iter().map(|s| (s.name, s.entry.state, s.revision_unix)).collect();
    assert_eq!(names, vec![("d/2".into(), EntryState::Named, 1_760_000_100), ("doc.dat".into(), EntryState::Unnamed, 1_760_000_100)]);
    assert_eq!(c3.read_stream("d/1", &cancel), Err(AffinityError::StreamDeleted));
    assert_eq!(c3.read_stream("nope", &cancel), Err(AffinityError::StreamNotFound));
    assert_eq!(c3.read_stream("d/2", &cancel).unwrap(), pattern(70, 4));
}

#[test]
fn hostile_containers_are_typed_refusals_without_panics() {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let good = build(&Build::doc(12, b"#FT4", vec![Spec::new(1, "doc.dat", pattern(2000, 5), ZSTD)]));
    // Every truncation is a typed error (never a panic, never a fake success past the directory).
    for n in 0..good.len() {
        if let Ok(c) = KaContainer::parse(&good[..n], &limits) {
            assert!(c.read_stream("doc.dat", &cancel).is_err(), "prefix {n} decoded");
        }
    }
    let parse = |b: &[u8]| KaContainer::parse(b, &limits).err();
    assert_eq!(parse(b"PK\x03\x04"), Some(AffinityError::NotAffinity));
    assert_eq!(parse(&good[..20]), Some(AffinityError::HeaderTruncated));
    let mut v = good.clone();
    v[4] = 13;
    assert_eq!(parse(&v), Some(AffinityError::VersionUnsupported(13)));
    let dir_at = u64::from_le_bytes(good[0x10..0x18].try_into().unwrap()) as usize;
    // Cyclic chain: the directory names itself as next.
    let mut cyc = good.clone();
    cyc[dir_at + 4..dir_at + 12].copy_from_slice(&(dir_at as u64).to_le_bytes());
    assert_eq!(parse(&cyc), Some(AffinityError::DirectoryCycle));
    let mut oob = good.clone();
    oob[0x10..0x18].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(parse(&oob), Some(AffinityError::DirectoryOutOfBounds));
    // A huge entry count is refused before any allocation.
    let mut many = good.clone();
    many[dir_at + 44..dir_at + 48].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(parse(&many), Some(AffinityError::EntryBudget));
    let mut many2 = good.clone();
    many2[dir_at + 44..dir_at + 48].copy_from_slice(&1000u32.to_le_bytes());
    assert_eq!(parse(&many2), Some(AffinityError::DirectoryTruncated));
    let c = KaContainer::parse(&good, &limits).unwrap();
    let entry = c.stream("doc.dat").unwrap().entry;
    // zstd bomb: 8 MiB of zeros compressed; declared size over the stream cap, the ratio cap, or
    // under the real size (inflation stops at the declared size).
    let bomb = vec![0u8; 8 << 20];
    let bomb_doc = build(&Build::doc(12, b"#FT4", vec![Spec::new(1, "doc.dat", bomb, ZSTD)]));
    let small = Limits { max_stream_bytes: 1 << 20, ..Limits::default() };
    assert_eq!(KaContainer::parse(&bomb_doc, &small).unwrap().read_stream("doc.dat", &cancel), Err(AffinityError::StreamTooLarge));
    let tight = Limits { max_ratio: 64, ..Limits::default() };
    assert_eq!(KaContainer::parse(&bomb_doc, &tight).unwrap().read_stream("doc.dat", &cancel), Err(AffinityError::StreamRatio));
    assert_eq!(KaContainer::parse(&bomb_doc, &Limits { max_total_bytes: 1 << 20, ..Limits::default() }).unwrap().resources(&cancel).err(), Some(AffinityError::TotalBudget));
    let roomy = Limits { max_ratio: u64::MAX, ..Limits::default() };
    let bc = KaContainer::parse(&bomb_doc, &roomy).unwrap();
    let mut lied = bc.stream("doc.dat").unwrap().entry;
    lied.size = 1 << 20;
    assert_eq!(bc.read_entry(&lied, &cancel), Err(AffinityError::InflatedTooLarge));
    // zlib past the declared size, CRC mismatch, bad #Fil tag, out-of-bounds payload, codec 3.
    let z = build(&Build::doc(9, b"#FAT", vec![Spec::new(1, "doc.dat", vec![7u8; 100_000], ZLIB)]));
    let zc = KaContainer::parse(&z, &limits).unwrap();
    let mut short = zc.stream("doc.dat").unwrap().entry;
    short.size = 1000;
    assert_eq!(zc.read_entry(&short, &cancel), Err(AffinityError::InflatedTooLarge));
    let mut e = entry.clone();
    e.crc32 ^= 1;
    assert_eq!(c.read_entry(&e, &cancel), Err(AffinityError::CrcMismatch));
    let mut e = entry.clone();
    e.offset += 1;
    assert_eq!(c.read_entry(&e, &cancel), Err(AffinityError::StreamTagInvalid));
    let mut e = entry.clone();
    e.stored_size = u64::MAX / 2;
    assert_eq!(c.read_entry(&e, &cancel), Err(AffinityError::StreamOutOfBounds));
    let mut e = entry.clone();
    e.compression = 3;
    assert_eq!(c.read_entry(&e, &cancel), Err(AffinityError::CompressionUnsupported(3)));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(c.read_stream("doc.dat", &canceled), Err(AffinityError::Canceled));
}

#[test]
fn thumbnail_and_resource_blobs_are_content_addressed_assets() {
    let cancel = CancellationToken::default();
    let thumb = png_shell(512, 384);
    let font = [b"OTTO".as_slice(), &pattern(200, 8)].concat();
    let image = png_shell(16, 16);
    let mut b = Build::doc(12, b"#FT4", vec![
        Spec::new(1, "doc.dat", [&[0x00, 0xFF, 0x4B, 0x53][..], &pattern(300, 1)].concat(), ZSTD),
        Spec::new(2, "d/1", pattern(65_536, 2), ZSTD),
        Spec::new(3, "d/2", font.clone(), ZLIB),
        Spec::new(4, "d/3", image.clone(), STORED),
    ]);
    b.thumbnail = Some(thumb.clone());
    let bytes = build(&b);
    let c = KaContainer::parse(&bytes, &Limits::default()).unwrap();
    let t = c.thumbnail().unwrap();
    assert_eq!((t.kind, t.width, t.height, t.located_by), (BlobKind::Png, Some(512), Some(384), "structured"));
    assert_eq!(t.bytes, thumb);
    assert_eq!(t.sha256, hsk_studio_interop_affinity::sha256(&thumb));
    let r = c.resources(&cancel).unwrap();
    let assets: Vec<_> = r.assets.iter().map(|a| (a.stream.as_str(), a.kind, a.bytes.len())).collect();
    assert_eq!(assets, vec![("d/2", BlobKind::Font, font.len()), ("d/3", BlobKind::Png, image.len())]);
    assert_eq!(r.assets[1].sha256, hsk_studio_interop_affinity::sha256(&image));
    let carried: Vec<_> = r.carried.iter().map(|(n, k, _)| (n.as_str(), *k)).collect();
    assert_eq!(carried, vec![("d/1", BlobKind::Opaque), ("doc.dat", BlobKind::ObjectStream)]);
    // A block whose image-header field is wrong is found by the bounded scan; no block = absent;
    // a truncated PNG inside the block is refused.
    let mut odd = bytes.clone();
    let at = c.header.thumbnail_offset as usize;
    odd[at + 16..at + 20].copy_from_slice(&40u32.to_le_bytes());
    let t2 = KaContainer::parse(&odd, &Limits::default()).unwrap().thumbnail().unwrap();
    assert_eq!((t2.located_by, t2.bytes.len()), ("scan", thumb.len()));
    let mut none = build(&Build::doc(12, b"#FT4", vec![Spec::new(1, "doc.dat", vec![1], STORED)]));
    assert_eq!(KaContainer::parse(&none, &Limits::default()).unwrap().thumbnail(), Err(AffinityError::ThumbnailAbsent));
    none.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, b'T', b'h', b'm', b'b', 1, 0, 0, 0, 0xFF, 0xFF, 0, 0]);
    assert_eq!(KaContainer::parse(&none, &Limits::default()).unwrap().thumbnail(), Err(AffinityError::ThumbnailInvalid));
    let mut cut = bytes.clone();
    cut.truncate(cut.len() - 20);
    let len_at = at + 12;
    let block_len = u32::from_le_bytes(cut[len_at..len_at + 4].try_into().unwrap()) - 20;
    cut[len_at..len_at + 4].copy_from_slice(&block_len.to_le_bytes());
    assert_eq!(KaContainer::parse(&cut, &Limits::default()).unwrap().thumbnail(), Err(AffinityError::ThumbnailImageInvalid));
}

/// The one corpus-root convention (`.config/oracle-corpus.toml`): walk up from the workspace root.
fn corpus_root() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = std::fs::read_to_string(workspace.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let mut dir = workspace.canonicalize().ok()?;
    for _ in 0..=max {
        let candidate = dir.join(&search);
        if candidate.is_dir() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

#[test]
#[ignore = "needs oracle-corpus/affinity/afdesignload-mit (AFDesignLoad MIT test designs)"]
fn afdesignload_v1_designs_read_with_matching_crcs_and_thumbnails() {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: afdesignload_v1_designs corpus reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return;
    };
    let provenance = root.join("affinity/afdesignload-mit/provenance.json");
    let Ok(text) = std::fs::read_to_string(&provenance) else {
        println!("NOT_PROVEN: afdesignload_v1_designs provenance reason=missing {}", provenance.display());
        return;
    };
    let cancel = CancellationToken::default();
    let mut checked = 0;
    // provenance.json pins every file by sha256 ("file": "...", "sha256": "...").
    let pins: Vec<(String, String)> = text
        .split("\"file\": \"")
        .skip(1)
        .filter_map(|chunk| {
            let file = chunk.split('"').next()?.to_string();
            let sha = chunk.split("\"sha256\": \"").nth(1)?.split('"').next()?.to_string();
            Some((file, sha))
        })
        .collect();
    assert_eq!(pins.len(), 9, "nine pinned designs");
    for (file, sha) in pins {
        let Ok(bytes) = std::fs::read(root.join(&file)) else {
            println!("NOT_PROVEN: afdesignload_v1_designs {file} reason=missing");
            continue;
        };
        assert_eq!(hsk_studio_interop_affinity::hex(&hsk_studio_interop_affinity::sha256(&bytes)), sha, "{file} pin");
        let c = KaContainer::parse(&bytes, &Limits::default()).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!((c.header.version, c.header.generation(), c.header.is_document()), (9, Generation::V1, true), "{file}");
        assert_eq!(c.directories[0].kind, DirectoryKind::Fat, "{file}");
        let r = c.resources(&cancel).unwrap_or_else(|e| panic!("{file}: every stream decodes with a matching CRC: {e}"));
        assert!(r.carried.iter().any(|(n, _, _)| n == "doc.dat"), "{file}");
        let t = c.thumbnail().unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!((t.kind, t.width, t.height, t.located_by), (BlobKind::Png, Some(512), Some(512), "structured"), "{file}");
        checked += 1;
    }
    println!("afdesignload_v1_designs checked={checked}");
}
