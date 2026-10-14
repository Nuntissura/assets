//! Shared bounded non-canonical ZIP reader (D16): STORE/DEFLATE round trip, hostile bounds,
//! structural refusals, ZIP64 opt-in and the bounded raw-DEFLATE helper. Archives are built here
//! from raw bytes so every field can be tampered with.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_package::{
    InflateFail, ZipDirectory, ZipLimits, ZipReadError, crc32_of, inflate_exact, read_zip,
};
use miniz_oxide::deflate::compress_to_vec;

fn limits() -> ZipLimits {
    ZipLimits {
        max_bytes: 1 << 20,
        max_entries: 16,
        max_entry_bytes: 1 << 16,
        max_inflated_bytes: 1 << 17,
        max_ratio: 100,
        allow_zip64: false,
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn err(bytes: &[u8], limits: &ZipLimits) -> ZipReadError {
    read_zip(bytes, limits, &token()).expect_err("must be refused")
}

fn p16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn p32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn p64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}

struct Part<'a> {
    name: &'a str,
    data: &'a [u8],
    deflate: bool,
}

/// `zip64` writes sentinel sizes and offsets plus ZIP64 extras and a ZIP64 end record.
fn build(parts: &[Part], zip64: bool, comment: &[u8]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for part in parts {
        let body = if part.deflate {
            compress_to_vec(part.data, 6)
        } else {
            part.data.to_vec()
        };
        let method = if part.deflate { 8u16 } else { 0 };
        let (crc, comp, uncomp, local) = (
            crc32_of(part.data),
            body.len() as u32,
            part.data.len() as u32,
            out.len() as u32,
        );
        let extra = |sizes: bool, offset: bool| {
            let mut e = Vec::new();
            if sizes {
                p64(&mut e, u64::from(uncomp));
                p64(&mut e, u64::from(comp));
            }
            if offset {
                p64(&mut e, u64::from(local));
            }
            let mut full = Vec::new();
            p16(&mut full, 1);
            p16(&mut full, e.len() as u16);
            full.extend_from_slice(&e);
            full
        };
        p32(&mut out, 0x0403_4b50);
        for v in [20u16, 0, method, 0, 0x21] {
            p16(&mut out, v);
        }
        let local_extra = if zip64 { extra(true, false) } else { vec![] };
        p32(&mut out, crc);
        p32(&mut out, if zip64 { u32::MAX } else { comp });
        p32(&mut out, if zip64 { u32::MAX } else { uncomp });
        p16(&mut out, part.name.len() as u16);
        p16(&mut out, local_extra.len() as u16);
        out.extend_from_slice(part.name.as_bytes());
        out.extend_from_slice(&local_extra);
        out.extend_from_slice(&body);

        let cd_extra = if zip64 { extra(true, true) } else { vec![] };
        p32(&mut central, 0x0201_4b50);
        for v in [20u16, 20, 0, method, 0, 0x21] {
            p16(&mut central, v);
        }
        p32(&mut central, crc);
        p32(&mut central, if zip64 { u32::MAX } else { comp });
        p32(&mut central, if zip64 { u32::MAX } else { uncomp });
        p16(&mut central, part.name.len() as u16);
        p16(&mut central, cd_extra.len() as u16);
        for v in [0u16, 0, 0] {
            p16(&mut central, v);
        }
        p32(&mut central, 0);
        p32(&mut central, if zip64 { u32::MAX } else { local });
        central.extend_from_slice(part.name.as_bytes());
        central.extend_from_slice(&cd_extra);
    }
    let (cd_off, cd_size, n) = (out.len(), central.len(), parts.len());
    out.extend_from_slice(&central);
    if zip64 {
        let eocd64 = out.len() as u64;
        p32(&mut out, 0x0606_4b50);
        p64(&mut out, 44);
        p16(&mut out, 45);
        p16(&mut out, 45);
        p32(&mut out, 0);
        p32(&mut out, 0);
        for v in [n as u64, n as u64, cd_size as u64, cd_off as u64] {
            p64(&mut out, v);
        }
        p32(&mut out, 0x0706_4b50);
        p32(&mut out, 0);
        p64(&mut out, eocd64);
        p32(&mut out, 1);
    }
    p32(&mut out, 0x0605_4b50);
    p16(&mut out, 0);
    p16(&mut out, 0);
    if zip64 {
        p16(&mut out, u16::MAX);
        p16(&mut out, u16::MAX);
        p32(&mut out, u32::MAX);
        p32(&mut out, u32::MAX);
    } else {
        p16(&mut out, n as u16);
        p16(&mut out, n as u16);
        p32(&mut out, cd_size as u32);
        p32(&mut out, cd_off as u32);
    }
    p16(&mut out, comment.len() as u16);
    out.extend_from_slice(comment);
    out
}

fn two() -> Vec<u8> {
    let big = vec![b'a'; 4096];
    build(
        &[
            Part {
                name: "mimetype",
                data: b"application/x-test",
                deflate: false,
            },
            Part {
                name: "dir/body.xml",
                data: &big,
                deflate: true,
            },
        ],
        false,
        b"archive comment",
    )
}

fn find(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("needle present")
}

#[test]
fn store_and_deflate_roundtrip_with_comment_and_one_crc() {
    assert_eq!(crc32_of(b"123456789"), 0xCBF4_3926);
    let zip = two();
    let dir = ZipDirectory::parse(&zip, &limits(), &token()).unwrap();
    let names: Vec<_> = dir.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["mimetype", "dir/body.xml"]);
    assert_eq!(dir.entries()[0].local_offset(), 0);
    assert_eq!((dir.entries()[1].method, dir.entries()[1].size), (8, 4096));
    let all = read_zip(&zip, &limits(), &token()).unwrap();
    assert_eq!(all[0].1, b"application/x-test");
    assert_eq!(all[1].1, vec![b'a'; 4096]);
}

#[test]
fn hostile_bounds_and_aggregate_over_budget_are_typed() {
    let zip = two();
    let mut l = limits();
    l.max_bytes = zip.len() - 1;
    assert_eq!(err(&zip, &l), ZipReadError::BudgetExceeded("bytes"));
    let mut l = limits();
    l.max_entries = 1;
    assert_eq!(err(&zip, &l), ZipReadError::BudgetExceeded("entries"));
    let mut l = limits();
    l.max_entry_bytes = 4095;
    assert_eq!(err(&zip, &l), ZipReadError::BudgetExceeded("entry_bytes"));
    // Aggregate: each entry fits, the running total does not (dims_u32_max-style claim below).
    let mut l = limits();
    l.max_inflated_bytes = 4096;
    assert_eq!(err(&zip, &l), ZipReadError::BudgetExceeded("inflated_bytes"));
    // A central-directory claim of u32::MAX - 1 bytes is a claim, refused before any allocation.
    let mut claim = zip.clone();
    let cd = find(&claim, &[0x50, 0x4b, 0x01, 0x02]);
    let second = cd + 46 + "mimetype".len();
    claim[second + 24..second + 28].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    assert_eq!(err(&claim, &limits()), ZipReadError::BudgetExceeded("entry_bytes"));
    // Ratio: 2 MiB of zeros in a few hundred bytes, beyond the 1 MiB floor.
    let zeros = vec![0u8; 2 << 20];
    let bomb = build(
        &[Part {
            name: "z.bin",
            data: &zeros,
            deflate: true,
        }],
        false,
        b"",
    );
    let mut l = limits();
    l.max_entry_bytes = 4 << 20;
    l.max_inflated_bytes = 4 << 20;
    l.max_bytes = 4 << 20;
    assert_eq!(err(&bomb, &l), ZipReadError::BudgetExceeded("ratio"));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(
        read_zip(&zip, &limits(), &canceled).unwrap_err(),
        ZipReadError::Canceled
    );
}

#[test]
fn structural_refusals_never_return_partial_data() {
    let zip = two();
    let mut bad = zip.clone();
    let body = find(&bad, b"application/x-test");
    bad[body] ^= 1;
    assert_eq!(err(&bad, &limits()), ZipReadError::Malformed("crc_mismatch"));

    let mut bad = zip.clone();
    bad[8] = 9; // local method of the first entry
    assert_eq!(err(&bad, &limits()), ZipReadError::Malformed("local_method_mismatch"));

    let mut bad = zip.clone();
    bad[14] ^= 1; // local CRC of the first entry
    assert_eq!(err(&bad, &limits()), ZipReadError::Malformed("local_header_mismatch"));

    let mut bad = zip.clone();
    bad[6] |= 1; // local encryption flag
    assert_eq!(err(&bad, &limits()), ZipReadError::Malformed("encrypted"));

    let mut bad = zip.clone();
    let cd = find(&bad, &[0x50, 0x4b, 0x01, 0x02]);
    bad[cd + 10] = 12; // central method bzip2
    assert_eq!(err(&bad, &limits()), ZipReadError::Malformed("method_unsupported"));

    let dup = build(
        &[
            Part { name: "a.xml", data: b"1", deflate: false },
            Part { name: "A.XML", data: b"2", deflate: false },
        ],
        false,
        b"",
    );
    assert_eq!(err(&dup, &limits()), ZipReadError::DuplicateName);
    for name in ["../x", "/abs", "a\\b", "c:x", "a//b", "./a"] {
        let z = build(&[Part { name, data: b"1", deflate: false }], false, b"");
        assert_eq!(err(&z, &limits()), ZipReadError::UnsafeName, "{name}");
    }
    let long = "n".repeat(256);
    let z = build(&[Part { name: &long, data: b"1", deflate: false }], false, b"");
    assert_eq!(err(&z, &limits()), ZipReadError::UnsafeName);

    // Bytes after the final DEFLATE block but inside the declared compressed size.
    let data = b"hello hello hello hello";
    let mut padded = compress_to_vec(data, 6);
    padded.push(0);
    assert_eq!(inflate_exact(&padded, 64), Err(InflateFail::Trailing));

    // Truncation anywhere is a typed refusal.
    for cut in [0, 10, 21, zip.len() / 2, zip.len() - 1] {
        assert!(read_zip(&zip[..cut], &limits(), &token()).is_err(), "cut {cut}");
    }
}

#[test]
fn zip64_is_opt_in_and_checked() {
    let parts = [Part {
        name: "canvas.fig",
        data: b"zip64 payload",
        deflate: true,
    }];
    let z64 = build(&parts, true, b"");
    assert_eq!(err(&z64, &limits()), ZipReadError::Malformed("zip64_unsupported"));
    let mut allow = limits();
    allow.allow_zip64 = true;
    let all = read_zip(&z64, &allow, &token()).unwrap();
    assert_eq!(all[0].1, b"zip64 payload");
    // The same archive without ZIP64 structures still reads under the opt-in.
    let plain = build(&parts, false, b"");
    assert_eq!(read_zip(&plain, &allow, &token()).unwrap()[0].1, b"zip64 payload");
    // EOCD64 disagreeing with a non-sentinel 32-bit field is refused.
    let mut bad = z64.clone();
    let eocd = bad.len() - 22;
    bad[eocd + 10..eocd + 12].copy_from_slice(&7u16.to_le_bytes());
    assert_eq!(err(&bad, &allow), ZipReadError::Malformed("eocd_zip64_disagree"));
    // EOCD64 entry count above the bound is a budget refusal, not a huge loop.
    let mut huge = z64.clone();
    let eocd64 = find(&huge, &[0x50, 0x4b, 0x06, 0x06]);
    huge[eocd64 + 32..eocd64 + 40].copy_from_slice(&u64::MAX.to_le_bytes());
    huge[eocd64 + 24..eocd64 + 32].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(err(&huge, &allow), ZipReadError::BudgetExceeded("entries"));
}

#[test]
fn inflate_exact_is_bounded_by_cap() {
    let data = vec![7u8; 10_000];
    let packed = compress_to_vec(&data, 6);
    assert_eq!(inflate_exact(&packed, 10_000).unwrap(), data);
    assert_eq!(inflate_exact(&packed, 9_999), Err(InflateFail::OutputLimit));
    assert_eq!(inflate_exact(&[0xff, 0xff, 0xff], 64), Err(InflateFail::Invalid));
    assert_eq!(inflate_exact(&packed[..packed.len() - 2], 10_000), Err(InflateFail::Invalid));
}
