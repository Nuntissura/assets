//! Container writer tests (lane SMB-DEVELOP-DNG-WRITER, slice 1). Every file is written by `write_container` and
//! read back through the read-only `Container` facade (independent code path: scan.rs). All bytes are built here.
use hsk_studio_interop_tiff::*;

fn read(bytes: &[u8]) -> (Container<'_>, Vec<Ifd>) {
    let c = Container::parse(bytes, &Limits::default()).expect("header");
    let ifds = c.walk().expect("walk");
    (c, ifds)
}

fn spec() -> IfdSpec {
    let exif = IfdSpec {
        entries: vec![(33434, FieldValue::Rational(vec![(1, 250)])), (34855, FieldValue::Short(vec![400]))],
        ..IfdSpec::default()
    };
    let raw = IfdSpec {
        entries: vec![
            (256, FieldValue::Long(vec![4])),
            (257, FieldValue::Long(vec![2])),
            (50714, FieldValue::SRational(vec![(-3, 2), (7, 1)])),
        ],
        strips: vec![vec![1, 2, 3, 4, 5, 6, 7, 8, 9], vec![10, 11, 12]],
        children: vec![],
    };
    IfdSpec {
        // Deliberately unsorted: the writer orders the table.
        entries: vec![
            (306, FieldValue::Ascii("2026:10:22 01:02:03".into())),
            (254, FieldValue::Long(vec![1])),
            (50706, FieldValue::Byte(vec![1, 4, 0, 0])),
            (700, FieldValue::Byte(b"<x:xmpmeta/>".to_vec())),
        ],
        strips: vec![vec![0xAA; 5]],
        children: vec![ChildIfds { tag: 330, ifds: vec![raw] }, ChildIfds { tag: 34665, ifds: vec![exif] }],
    }
}

#[test]
fn written_container_reads_back_through_the_container_facade() {
    let bytes = write_container(&spec(), 1 << 20).unwrap();
    assert_eq!(&bytes[..4], b"II*\0");
    let (c, ifds) = read(&bytes);
    assert_eq!(ifds.len(), 2, "IFD0 and the SubIFD are walked; the Exif IFD hangs off 34665, which walk does not follow");
    let root = &ifds[0];
    let tags: Vec<u16> = root.entries.iter().map(|e| e.tag).collect();
    let mut sorted = tags.clone();
    sorted.sort_unstable();
    assert_eq!(tags, sorted, "entries are ordered by tag");
    assert!(tags.contains(&273) && tags.contains(&279) && tags.contains(&330) && tags.contains(&34665));
    assert_eq!(c.ascii(root.find(306).unwrap()).unwrap(), "2026:10:22 01:02:03");
    assert_eq!(c.bytes(root.find(700).unwrap()).unwrap(), b"<x:xmpmeta/>");
    // Strip 0 of IFD0 is read through StripOffsets/StripByteCounts.
    let off = c.uints(root.find(273).unwrap()).unwrap();
    let len = c.uints(root.find(279).unwrap()).unwrap();
    assert_eq!(c.region(off[0], len[0]).unwrap(), &[0xAA; 5]);
    // SubIFD strips and signed rationals.
    let raw = &ifds[1];
    let (off, len) = (c.uints(raw.find(273).unwrap()).unwrap(), c.uints(raw.find(279).unwrap()).unwrap());
    assert_eq!(len, vec![9, 3]);
    assert_eq!(c.region(off[0], len[0]).unwrap(), &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(c.region(off[1], len[1]).unwrap(), &[10, 11, 12]);
    assert_eq!(c.f64s(raw.find(50714).unwrap()).unwrap(), vec![-1.5, 7.0]);
    // The Exif IFD is reachable through its pointer.
    let exif_at = c.uints(root.find(34665).unwrap()).unwrap()[0];
    let exif = c.ifd(exif_at).unwrap();
    assert_eq!(c.f64s(exif.find(33434).unwrap()).unwrap(), vec![1.0 / 250.0]);
    assert_eq!(c.uints(exif.find(34855).unwrap()).unwrap(), vec![400]);
    // Offsets are word aligned and the file is deterministic.
    assert!(off.iter().all(|o| o % 2 == 0));
    assert_eq!(write_container(&spec(), 1 << 20).unwrap(), bytes);
}

#[test]
fn every_value_type_round_trips() {
    let all = IfdSpec {
        entries: vec![
            (1, FieldValue::Byte(vec![1, 2, 3, 4, 5, 6])),
            (2, FieldValue::Ascii("abc".into())),
            (3, FieldValue::Short(vec![1, 65535, 3])),
            (4, FieldValue::Long(vec![u32::MAX])),
            (5, FieldValue::Rational(vec![(1, 3), (u32::MAX, 1)])),
            (6, FieldValue::SByte(vec![-1, 5])),
            (7, FieldValue::Undefined(vec![9, 8, 7, 6, 5])),
            (8, FieldValue::SShort(vec![-2, 2])),
            (9, FieldValue::SLong(vec![i32::MIN])),
            (10, FieldValue::SRational(vec![(-1, 3)])),
            (11, FieldValue::Float(vec![1.5, -2.25])),
            (12, FieldValue::Double(vec![0.1])),
        ],
        ..IfdSpec::default()
    };
    let bytes = write_container(&all, 1 << 20).unwrap();
    let (c, ifds) = read(&bytes);
    let e = &ifds[0];
    assert_eq!(c.bytes(e.find(1).unwrap()).unwrap(), &[1, 2, 3, 4, 5, 6]);
    assert_eq!(c.ascii(e.find(2).unwrap()).unwrap(), "abc");
    assert_eq!(c.uints(e.find(3).unwrap()).unwrap(), vec![1, 65535, 3]);
    assert_eq!(c.uints(e.find(4).unwrap()).unwrap(), vec![u64::from(u32::MAX)]);
    assert_eq!(c.f64s(e.find(5).unwrap()).unwrap(), vec![1.0 / 3.0, f64::from(u32::MAX)]);
    assert_eq!(c.f64s(e.find(6).unwrap()).unwrap(), vec![-1.0, 5.0]);
    assert_eq!(c.bytes(e.find(7).unwrap()).unwrap(), &[9, 8, 7, 6, 5]);
    assert_eq!(c.f64s(e.find(8).unwrap()).unwrap(), vec![-2.0, 2.0]);
    assert_eq!(c.f64s(e.find(9).unwrap()).unwrap(), vec![f64::from(i32::MIN)]);
    assert_eq!(c.f64s(e.find(10).unwrap()).unwrap(), vec![-1.0 / 3.0]);
    assert_eq!(c.f64s(e.find(11).unwrap()).unwrap(), vec![1.5, -2.25]);
    assert_eq!(c.f64s(e.find(12).unwrap()).unwrap(), vec![0.1]);
}

#[test]
fn refusals_are_typed() {
    let code = |s: &IfdSpec, max: u64| write_container(s, max).unwrap_err().code();
    let one = |t: u16, v: FieldValue| IfdSpec { entries: vec![(t, v)], ..IfdSpec::default() };
    assert_eq!(code(&one(273, FieldValue::Long(vec![0])), 1 << 20), "reserved_tag");
    assert_eq!(code(&one(279, FieldValue::Long(vec![0])), 1 << 20), "reserved_tag");
    let mut with_child = spec();
    with_child.entries.push((330, FieldValue::Long(vec![0])));
    assert_eq!(code(&with_child, 1 << 20), "reserved_tag");
    let dup = IfdSpec { entries: vec![(256, FieldValue::Long(vec![1])), (256, FieldValue::Long(vec![2]))], ..IfdSpec::default() };
    assert_eq!(code(&dup, 1 << 20), "duplicate_tag");
    assert_eq!(code(&one(2, FieldValue::Ascii("a\0b".into())), 1 << 20), "ascii_invalid");
    assert_eq!(code(&one(2, FieldValue::Ascii("caf\u{e9}".into())), 1 << 20), "ascii_invalid");
    assert_eq!(code(&one(5, FieldValue::Rational(vec![(1, 0)])), 1 << 20), "zero_denominator");
    assert_eq!(code(&one(10, FieldValue::SRational(vec![(1, 0)])), 1 << 20), "zero_denominator");
    assert_eq!(code(&one(12, FieldValue::Double(vec![f64::NAN])), 1 << 20), "non_finite");
    assert_eq!(code(&one(11, FieldValue::Float(vec![f32::INFINITY])), 1 << 20), "non_finite");
    let empty_group = IfdSpec { children: vec![ChildIfds { tag: 330, ifds: vec![] }], ..IfdSpec::default() };
    assert_eq!(code(&empty_group, 1 << 20), "empty_child_group");
}

#[test]
fn hostile_sizes_are_refused_before_allocation() {
    let code = |s: &IfdSpec, max: u64| write_container(s, max).unwrap_err().code();
    // Byte budget: the strip alone exceeds it (the output buffer is never built).
    let big = IfdSpec { strips: vec![vec![0u8; 4096]], ..IfdSpec::default() };
    assert_eq!(code(&big, 1024), "limit_container_bytes");
    assert!(write_container(&big, 1 << 20).is_ok());
    // Entry count above the decoder's own cap.
    let many = IfdSpec { entries: (0..=4096u32).map(|t| (t as u16 + 1, FieldValue::Long(vec![0]))).collect(), ..IfdSpec::default() };
    assert_eq!(code(&many, u64::MAX), "ifd_entry_count");
    // Strip count cap.
    let strips = IfdSpec { strips: vec![Vec::new(); MAX_WRITE_STRIPS + 1], ..IfdSpec::default() };
    assert_eq!(code(&strips, u64::MAX), "limit_strips");
    // Nesting depth and IFD count.
    let mut deep = IfdSpec::default();
    for _ in 0..=MAX_WRITE_DEPTH {
        deep = IfdSpec { children: vec![ChildIfds { tag: 330, ifds: vec![deep] }], ..IfdSpec::default() };
    }
    assert_eq!(code(&deep, u64::MAX), "limit_ifd_depth");
    let wide = IfdSpec {
        children: vec![ChildIfds { tag: 330, ifds: vec![IfdSpec::default(); MAX_WRITE_IFDS] }],
        ..IfdSpec::default()
    };
    assert_eq!(code(&wide, u64::MAX), "limit_ifds");
}
