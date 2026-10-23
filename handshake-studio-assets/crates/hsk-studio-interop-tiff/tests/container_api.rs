//! Container facade tests (lane SMB-INTEROP-TIFF-CONTAINER-API). All bytes are built here (V4-03).
use hsk_studio_interop_tiff::*;

/// (tag, field type, count, value bytes in file byte order)
type Ent = (u16, u16, u32, Vec<u8>);

fn ifd_bytes(base: usize, entries: &[Ent], next: u32, le: bool) -> Vec<u8> {
    let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let table = 2 + entries.len() * 12 + 4;
    let mut out = w16(entries.len() as u16).to_vec();
    let mut tail: Vec<u8> = Vec::new();
    for (tag, ty, count, val) in entries {
        out.extend(w16(*tag));
        out.extend(w16(*ty));
        out.extend(w32(*count));
        if val.len() <= 4 {
            let mut f = val.clone();
            f.resize(4, 0);
            out.extend(f);
        } else {
            out.extend(w32((base + table + tail.len()) as u32));
            tail.extend(val);
        }
    }
    out.extend(w32(next));
    out.extend(tail);
    out
}

fn file(ifds: &[(&[Ent], u32)], le: bool) -> Vec<u8> {
    // IFDs are laid out back to back from offset 8; `next` is given as an absolute offset.
    let mut out = if le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
    out.extend(if le { 8u32.to_le_bytes() } else { 8u32.to_be_bytes() });
    for (e, next) in ifds {
        let b = ifd_bytes(out.len(), e, *next, le);
        out.extend(b);
    }
    out
}

fn u32le(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn parse(b: &[u8]) -> Container<'_> {
    Container::parse(b, &Limits::default()).unwrap()
}

#[test]
fn walk_follows_next_and_subifds_and_reads_typed_values() {
    // IFD0 at 8: Make (ASCII), SubIFDs -> 2 offsets, next -> IFD at C. Layout sizes are computed first.
    let sub_a_len = ifd_bytes(0, &[(256, 4, 1, u32le(&[7]))], 0, true).len();
    let rat = [1u32.to_le_bytes(), 4u32.to_le_bytes(), 3u32.to_le_bytes(), 2u32.to_le_bytes()].concat();
    let ifd0_probe = |sub: [u32; 2], next: u32| -> Vec<u8> {
        ifd_bytes(
            8,
            &[
                (271, 2, 6, b"SONY\0\0".to_vec()),
                (330, 4, 2, u32le(&sub)),
                (50717, 5, 2, rat.clone()),
                (258, 3, 1, 14u16.to_le_bytes().to_vec()),
            ],
            next,
            true,
        )
    };
    let ifd0_len = ifd0_probe([0, 0], 0).len();
    let a = 8 + ifd0_len;
    let b = a + sub_a_len;
    let c = b + sub_a_len;
    let mut bytes = b"II*\0".to_vec();
    bytes.extend(8u32.to_le_bytes());
    bytes.extend(ifd0_probe([a as u32, b as u32], c as u32));
    bytes.extend(ifd_bytes(a, &[(256, 4, 1, u32le(&[7]))], 0, true));
    bytes.extend(ifd_bytes(b, &[(256, 4, 1, u32le(&[8]))], 0, true));
    bytes.extend(ifd_bytes(c, &[(256, 4, 1, u32le(&[9]))], 0, true));
    let t = parse(&bytes);
    assert_eq!((t.byte_order(), t.is_big_tiff(), t.first_ifd_offset()), (ByteOrder::Little, false, 8));
    let ifds = t.walk().unwrap();
    let widths: Vec<u64> = ifds.iter().map(|i| i.find(256).map_or(0, |e| t.uints(e).unwrap()[0])).collect();
    assert_eq!(ifds.len(), 4, "IFD0, two SubIFDs, then the next-pointer IFD");
    assert_eq!(widths[1..], [7, 8, 9], "SubIFDs are walked before the next-pointer IFD");
    let root = &ifds[0];
    assert_eq!(t.ascii(root.find(271).unwrap()).unwrap(), "SONY");
    assert_eq!(t.uints(root.find(258).unwrap()).unwrap(), vec![14]);
    assert_eq!(t.f64s(root.find(50717).unwrap()).unwrap(), vec![0.25, 1.5]);
    assert_eq!(t.bytes(root.find(271).unwrap()).unwrap().len(), 6);
    // region: in range and a MakerNote-style body by offset
    let e = root.find(271).unwrap();
    assert_eq!(t.region(e.value_offset, e.value_len).unwrap(), b"SONY\0\0");
}

#[test]
fn big_endian_values_and_signed_types() {
    let ents: Vec<Ent> = vec![
        (1, 3, 1, 0x0102u16.to_be_bytes().to_vec()),
        (2, 8, 1, (-2i16).to_be_bytes().to_vec()),
        (3, 10, 1, [(-3i32).to_be_bytes(), 2i32.to_be_bytes()].concat()),
        (4, 11, 1, 1.5f32.to_be_bytes().to_vec()),
    ];
    let bytes = file(&[(&ents, 0)], false);
    let t = parse(&bytes);
    assert_eq!(t.byte_order(), ByteOrder::Big);
    let ifd = &t.walk().unwrap()[0];
    assert_eq!(t.uints(ifd.find(1).unwrap()).unwrap(), vec![0x0102]);
    assert_eq!(t.f64s(ifd.find(2).unwrap()).unwrap(), vec![-2.0]);
    assert_eq!(t.f64s(ifd.find(3).unwrap()).unwrap(), vec![-1.5]);
    assert_eq!(t.f64s(ifd.find(4).unwrap()).unwrap(), vec![1.5]);
    assert_eq!(t.uints(ifd.find(2).unwrap()).unwrap_err().code(), "bad_field_type");
    assert_eq!(t.bytes(ifd.find(1).unwrap()).unwrap_err().code(), "bad_field_type");
}

#[test]
fn zero_denominator_and_non_finite_are_refused() {
    let ents: Vec<Ent> = vec![
        (1, 5, 1, [1u32.to_le_bytes(), 0u32.to_le_bytes()].concat()),
        (2, 11, 1, f32::NAN.to_le_bytes().to_vec()),
        (3, 12, 1, f64::INFINITY.to_le_bytes().to_vec()),
        (4, 10, 1, [1u32.to_le_bytes(), 0u32.to_le_bytes()].concat()),
    ];
    let bytes = file(&[(&ents, 0)], true);
    let t = parse(&bytes);
    let ifd = &t.walk().unwrap()[0];
    assert_eq!(t.f64s(ifd.find(1).unwrap()).unwrap_err().code(), "zero_denominator");
    assert_eq!(t.f64s(ifd.find(4).unwrap()).unwrap_err().code(), "zero_denominator");
    assert_eq!(t.f64s(ifd.find(2).unwrap()).unwrap_err().code(), "non_finite");
    assert_eq!(t.f64s(ifd.find(3).unwrap()).unwrap_err().code(), "non_finite");
}

#[test]
fn ifd_cycle_page_budget_and_entry_count_are_typed() {
    // IFD at 8 whose next pointer is itself.
    let ents: Vec<Ent> = vec![(256, 4, 1, u32le(&[1]))];
    let bytes = file(&[(&ents, 8)], true);
    assert_eq!(parse(&bytes).walk().unwrap_err(), TiffError::IfdCycle);
    // A SubIFD tag pointing back at IFD0 is a cycle too.
    let sub: Vec<Ent> = vec![(330, 4, 1, u32le(&[8]))];
    assert_eq!(parse(&file(&[(&sub, 0)], true)).walk().unwrap_err(), TiffError::IfdCycle);
    // Page budget: a chain of 3 IFDs under max_pages = 2.
    let one = ifd_bytes(0, &ents, 0, true).len() as u32;
    let chain = file(&[(&ents, 8 + one), (&ents, 8 + 2 * one), (&ents, 0)], true);
    let limits = Limits { max_pages: 2, ..Limits::default() };
    let c = Container::parse(&chain, &limits).unwrap();
    assert_eq!(c.walk().unwrap_err(), TiffError::Limit("limit_pages"));
    assert_eq!(parse(&chain).walk().unwrap().len(), 3);
    // Entry-count budget (declared 0xFFFF entries, tiny file, and a lowered caller limit).
    let mut huge = b"II*\0".to_vec();
    huge.extend(8u32.to_le_bytes());
    huge.extend(0xFFFFu16.to_le_bytes());
    assert_eq!(parse(&huge).walk().unwrap_err(), TiffError::IfdEntryCount);
    let small = Limits { max_ifd_entries: 1, ..Limits::default() };
    let two: Vec<Ent> = vec![(256, 4, 1, u32le(&[1])), (257, 4, 1, u32le(&[1]))];
    let f = file(&[(&two, 0)], true);
    assert_eq!(Container::parse(&f, &small).unwrap().walk().unwrap_err(), TiffError::IfdEntryCount);
}

#[test]
fn count_overflow_value_ranges_and_regions_out_of_bounds() {
    // LONG8-sized count that would overflow count*size on 32-bit-wide arithmetic: a classic entry whose
    // 0xFFFFFFFF x 8-byte RATIONAL points outside the file.
    let ents: Vec<Ent> = vec![(1, 5, u32::MAX, vec![0; 4])];
    let mut bytes = file(&[(&ents, 0)], true);
    // patch the value offset (entry field) to 0x10 so the range starts inside the file
    let field = 8 + 2 + 8;
    bytes[field..field + 4].copy_from_slice(&16u32.to_le_bytes());
    let t = parse(&bytes);
    let ifd = &t.walk().unwrap()[0];
    let e = ifd.find(1).unwrap();
    assert_eq!(e.value_len, u64::from(u32::MAX) * 8);
    // The caller's tag budget refuses it before the range check allocates anything.
    assert_eq!(t.f64s(e).unwrap_err(), TiffError::Limit("limit_tag_bytes"));
    let roomy = Limits { max_tag_bytes: u64::MAX, ..Limits::default() };
    let t2 = Container::parse(&bytes, &roomy).unwrap();
    assert_eq!(t2.f64s(e).unwrap_err(), TiffError::Truncated);
    // region bounds
    let len = bytes.len() as u64;
    assert!(t.region(0, len).is_ok());
    assert_eq!(t.region(len, 1).unwrap_err(), TiffError::Truncated);
    assert_eq!(t.region(1, len).unwrap_err(), TiffError::Truncated);
    assert_eq!(t.region(u64::MAX, 2).unwrap_err(), TiffError::Truncated);
}

#[test]
fn header_input_budget_and_bigtiff() {
    assert_eq!(Container::parse(b"XX*\0\x08\0\0\0", &Limits::default()).err(), Some(TiffError::NotTiff));
    assert_eq!(Container::parse(b"II", &Limits::default()).err(), Some(TiffError::NotTiff));
    let tiny = Limits { max_input_bytes: 4, ..Limits::default() };
    assert_eq!(Container::parse(b"II*\0\x08\0\0\0", &tiny).err(), Some(TiffError::Limit("limit_input_bytes")));
    // BigTIFF: header, one IFD with one LONG8 entry (inline), and its walk.
    let mut b = b"II+\0\x08\0\0\0".to_vec();
    b.extend(16u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    b.extend(256u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    b.extend(77u64.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    let t = parse(&b);
    assert!(t.is_big_tiff());
    let ifd = &t.walk().unwrap()[0];
    assert_eq!(t.uints(ifd.find(256).unwrap()).unwrap(), vec![77]);
}

#[test]
fn unknown_field_types_are_kept_but_refused() {
    let ents: Vec<Ent> = vec![(1, 99, 1, vec![0; 4]), (2, 4, 1, u32le(&[5]))];
    let bytes = file(&[(&ents, 0)], true);
    let t = parse(&bytes);
    let ifd = &t.walk().unwrap()[0];
    assert_eq!(ifd.entries.len(), 2);
    assert_eq!(t.uints(ifd.find(1).unwrap()).unwrap_err().code(), "bad_field_type");
    assert_eq!(t.uints(ifd.find(2).unwrap()).unwrap(), vec![5]);
}
