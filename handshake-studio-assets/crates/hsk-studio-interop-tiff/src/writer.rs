//! Generic classic-TIFF container writer for engines that author their own tags and payloads (DNG:
//! IFD0 + SubIFDs + Exif IFD). The caller supplies typed entries and opaque strip payloads; this module
//! owns layout (header, IFD tables, out-of-line values, strip data), entry order, offsets and the
//! synthetic pointer entries (StripOffsets 273, StripByteCounts 279 and the per-child pointer tags such as
//! SubIFDs 330 / Exif IFD 34665). Little-endian, classic TIFF (offsets are 32-bit: the output is bounded
//! by `max_bytes` and by `u32::MAX`). No pixel codec, no compression, no timestamps: the output is a pure
//! function of the request (deterministic).
//!
//! Hostile sizes (V4-01/02): the whole layout is computed with checked arithmetic and compared with the
//! byte budget BEFORE the output buffer is allocated; entry, strip, child and nesting counts are capped.
use crate::error::{cadd, cmul, to_usize, Result, TiffError};

/// Largest number of entries in one IFD (matches the decoder's `Limits::max_ifd_entries`).
pub const MAX_WRITE_ENTRIES: usize = 4096;
/// Largest number of strips in one IFD.
pub const MAX_WRITE_STRIPS: usize = 1 << 20;
/// Largest number of IFDs in one container.
pub const MAX_WRITE_IFDS: usize = 64;
/// Deepest child nesting below the root IFD.
pub const MAX_WRITE_DEPTH: usize = 3;

const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;

/// One typed IFD value. Strings are ASCII and get their terminating NUL appended.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Byte(Vec<u8>),
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    /// Unsigned rational (numerator, denominator); a zero denominator is refused.
    Rational(Vec<(u32, u32)>),
    SByte(Vec<i8>),
    Undefined(Vec<u8>),
    SShort(Vec<i16>),
    SLong(Vec<i32>),
    /// Signed rational (numerator, denominator); a zero denominator is refused.
    SRational(Vec<(i32, i32)>),
    Float(Vec<f32>),
    Double(Vec<f64>),
}

/// Pointer entries: `tag` holds one LONG offset per child IFD (SubIFDs 330 when more than one child,
/// Exif IFD 34665 with one).
#[derive(Clone, Debug, PartialEq)]
pub struct ChildIfds {
    pub tag: u16,
    pub ifds: Vec<IfdSpec>,
}

/// One IFD to write. `entries` need not be sorted; tags 273 and 279 are generated from `strips`, and every
/// `children[].tag` is generated from the children, so none of those may appear in `entries`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IfdSpec {
    pub entries: Vec<(u16, FieldValue)>,
    /// Opaque strip payloads, in order (empty: no strip entries are written).
    pub strips: Vec<Vec<u8>>,
    pub children: Vec<ChildIfds>,
}

struct Ent {
    tag: u16,
    ty: u16,
    count: u32,
    data: Vec<u8>,
}

struct Node<'a> {
    spec: &'a IfdSpec,
    /// Per `children` pointer tag: the node indexes of its IFDs.
    kids: Vec<Vec<usize>>,
}

fn usage(code: &'static str) -> TiffError {
    TiffError::Usage(code)
}

fn align(v: u64, to: u64) -> Result<u64> {
    let rem = v.checked_rem(to).ok_or(TiffError::DimsOverflow)?;
    if rem == 0 {
        Ok(v)
    } else {
        cadd(v, to.checked_sub(rem).ok_or(TiffError::DimsOverflow)?)
    }
}

fn count_u32(len: usize) -> Result<u32> {
    u32::try_from(len).map_err(|_| TiffError::DimsOverflow)
}

fn encode_value(value: &FieldValue) -> Result<(u16, u32, Vec<u8>)> {
    fn each<T: Copy, const N: usize>(items: &[T], f: impl Fn(T) -> [u8; N]) -> Vec<u8> {
        items.iter().flat_map(|v| f(*v)).collect()
    }
    Ok(match value {
        FieldValue::Byte(v) => (1, count_u32(v.len())?, v.clone()),
        FieldValue::Ascii(s) => {
            if !s.is_ascii() || s.as_bytes().contains(&0) {
                return Err(usage("ascii_invalid"));
            }
            let mut data = s.as_bytes().to_vec();
            data.push(0);
            (2, count_u32(data.len())?, data)
        }
        FieldValue::Short(v) => (3, count_u32(v.len())?, each(v, u16::to_le_bytes)),
        FieldValue::Long(v) => (4, count_u32(v.len())?, each(v, u32::to_le_bytes)),
        FieldValue::Rational(v) => {
            if v.iter().any(|(_, d)| *d == 0) {
                return Err(usage("zero_denominator"));
            }
            let data = v.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect();
            (5, count_u32(v.len())?, data)
        }
        FieldValue::SByte(v) => (6, count_u32(v.len())?, each(v, i8::to_le_bytes)),
        FieldValue::Undefined(v) => (7, count_u32(v.len())?, v.clone()),
        FieldValue::SShort(v) => (8, count_u32(v.len())?, each(v, i16::to_le_bytes)),
        FieldValue::SLong(v) => (9, count_u32(v.len())?, each(v, i32::to_le_bytes)),
        FieldValue::SRational(v) => {
            if v.iter().any(|(_, d)| *d == 0) {
                return Err(usage("zero_denominator"));
            }
            let data = v.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect();
            (10, count_u32(v.len())?, data)
        }
        FieldValue::Float(v) => {
            if v.iter().any(|x| !x.is_finite()) {
                return Err(usage("non_finite"));
            }
            (11, count_u32(v.len())?, each(v, f32::to_le_bytes))
        }
        FieldValue::Double(v) => {
            if v.iter().any(|x| !x.is_finite()) {
                return Err(usage("non_finite"));
            }
            (12, count_u32(v.len())?, each(v, f64::to_le_bytes))
        }
    })
}

fn long_bytes(values: &[u64]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(to_usize(cmul(4, values.len() as u64)?)?);
    for v in values {
        out.extend_from_slice(&u32::try_from(*v).map_err(|_| TiffError::Limit("limit_container_bytes"))?.to_le_bytes());
    }
    Ok(out)
}

fn flatten<'a>(spec: &'a IfdSpec, depth: usize, nodes: &mut Vec<Node<'a>>) -> Result<usize> {
    if depth > MAX_WRITE_DEPTH {
        return Err(TiffError::Limit("limit_ifd_depth"));
    }
    if nodes.len() >= MAX_WRITE_IFDS {
        return Err(TiffError::Limit("limit_ifds"));
    }
    let me = nodes.len();
    nodes.push(Node { spec, kids: Vec::new() });
    let mut kids = Vec::with_capacity(spec.children.len());
    for group in &spec.children {
        let mut ids = Vec::with_capacity(group.ifds.len());
        for child in &group.ifds {
            ids.push(flatten(child, cadd(depth as u64, 1).map(|d| d as usize)?, nodes)?);
        }
        kids.push(ids);
    }
    if let Some(node) = nodes.get_mut(me) {
        node.kids = kids;
    }
    Ok(me)
}

/// The sorted entries of one node; `strip_offsets` and `child_offsets` are the generated values (zeros on
/// the sizing pass, which only needs their lengths).
fn entries_of(node: &Node<'_>, strip_offsets: &[u64], child_offsets: &[Vec<u64>]) -> Result<Vec<Ent>> {
    let spec = node.spec;
    if spec.entries.len() > MAX_WRITE_ENTRIES {
        return Err(TiffError::IfdEntryCount);
    }
    if spec.strips.len() > MAX_WRITE_STRIPS {
        return Err(TiffError::Limit("limit_strips"));
    }
    let mut out = Vec::with_capacity(spec.entries.len());
    for (tag, value) in &spec.entries {
        let reserved = *tag == TAG_STRIP_OFFSETS
            || *tag == TAG_STRIP_BYTE_COUNTS
            || spec.children.iter().any(|c| c.tag == *tag);
        if reserved {
            return Err(usage("reserved_tag"));
        }
        let (ty, count, data) = encode_value(value)?;
        out.push(Ent { tag: *tag, ty, count, data });
    }
    if !spec.strips.is_empty() {
        let counts: Vec<u64> = spec.strips.iter().map(|s| s.len() as u64).collect();
        out.push(Ent { tag: TAG_STRIP_OFFSETS, ty: 4, count: count_u32(strip_offsets.len())?, data: long_bytes(strip_offsets)? });
        out.push(Ent { tag: TAG_STRIP_BYTE_COUNTS, ty: 4, count: count_u32(counts.len())?, data: long_bytes(&counts)? });
    }
    for (group, offsets) in spec.children.iter().zip(child_offsets) {
        if group.ifds.is_empty() {
            return Err(usage("empty_child_group"));
        }
        out.push(Ent { tag: group.tag, ty: 4, count: count_u32(offsets.len())?, data: long_bytes(offsets)? });
    }
    out.sort_by_key(|e| e.tag);
    if out.windows(2).any(|w| w[0].tag == w[1].tag) {
        return Err(usage("duplicate_tag"));
    }
    if out.len() > MAX_WRITE_ENTRIES {
        return Err(TiffError::IfdEntryCount);
    }
    Ok(out)
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Write `root` (and its children) as a little-endian classic TIFF of at most `max_bytes`.
pub fn write_container(root: &IfdSpec, max_bytes: u64) -> Result<Vec<u8>> {
    let mut nodes = Vec::new();
    flatten(root, 0, &mut nodes)?;
    // Sizing pass: positions depend only on counts and lengths.
    struct Place {
        table: u64,
        values: u64,
        strips: Vec<u64>,
    }
    let mut places = Vec::with_capacity(nodes.len());
    let mut cursor = 8u64;
    for node in &nodes {
        let zero_strips = vec![0u64; node.spec.strips.len()];
        let zero_kids: Vec<Vec<u64>> = node.kids.iter().map(|k| vec![0u64; k.len()]).collect();
        let ents = entries_of(node, &zero_strips, &zero_kids)?;
        let table = cursor;
        let table_len = cadd(cadd(2, cmul(12, ents.len() as u64)?)?, 4)?;
        let values = cadd(table, table_len)?;
        let mut values_len = 0u64;
        for e in &ents {
            if e.data.len() > 4 {
                values_len = cadd(values_len, align(e.data.len() as u64, 4)?)?;
            }
        }
        cursor = cadd(values, values_len)?;
        let mut strips = Vec::with_capacity(node.spec.strips.len());
        for s in &node.spec.strips {
            strips.push(cursor);
            cursor = cadd(cursor, align(s.len() as u64, 2)?)?;
        }
        cursor = align(cursor, 4)?;
        places.push(Place { table, values, strips });
    }
    if cursor > max_bytes || cursor > u64::from(u32::MAX) {
        return Err(TiffError::Limit("limit_container_bytes"));
    }
    // Emit pass.
    let mut out = Vec::with_capacity(to_usize(cursor)?);
    out.extend_from_slice(b"II");
    put_u16(&mut out, 42);
    put_u32(&mut out, 8);
    for (node, place) in nodes.iter().zip(&places) {
        let kid_offsets: Vec<Vec<u64>> =
            node.kids.iter().map(|ids| ids.iter().map(|i| places.get(*i).map_or(0, |p| p.table)).collect()).collect();
        let ents = entries_of(node, &place.strips, &kid_offsets)?;
        if u64::try_from(out.len()).ok() != Some(place.table) {
            return Err(usage("layout_mismatch"));
        }
        put_u16(&mut out, u16::try_from(ents.len()).map_err(|_| TiffError::IfdEntryCount)?);
        let mut next_value = place.values;
        let mut deferred: Vec<&Ent> = Vec::new();
        for e in &ents {
            put_u16(&mut out, e.tag);
            put_u16(&mut out, e.ty);
            put_u32(&mut out, e.count);
            if e.data.len() <= 4 {
                out.extend_from_slice(&e.data);
                out.resize(out.len().saturating_add(4usize.saturating_sub(e.data.len())), 0);
            } else {
                put_u32(&mut out, u32::try_from(next_value).map_err(|_| TiffError::Limit("limit_container_bytes"))?);
                next_value = cadd(next_value, align(e.data.len() as u64, 4)?)?;
                deferred.push(e);
            }
        }
        put_u32(&mut out, 0); // next IFD: none (children hang off pointer tags)
        for e in deferred {
            out.extend_from_slice(&e.data);
            let padded = to_usize(align(e.data.len() as u64, 4)?)?;
            out.resize(out.len().saturating_add(padded.saturating_sub(e.data.len())), 0);
        }
        for s in &node.spec.strips {
            out.extend_from_slice(s);
            if s.len() % 2 == 1 {
                out.push(0);
            }
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    if u64::try_from(out.len()).ok() != Some(cursor) {
        return Err(usage("layout_mismatch"));
    }
    Ok(out)
}
