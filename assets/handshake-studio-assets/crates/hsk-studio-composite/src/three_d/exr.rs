//! A minimal OpenEXR writer for named float planes (lane SMB-AE-3D-2; STU-CMP-050 "Extracts a named
//! auxiliary channel from a multi-channel render -- Z depth, object ID, ... surface normals" and
//! "Arbitrary-channel extract ... from a multi-part OpenEXR file by channel name": the 3D
//! renderer's auxiliary planes leave Studio in the field-standard container).
//!
//! Output: a single-part scanline file (magic `0x762f3101`, version 2, no flags), uncompressed
//! (`compression` 0), increasing Y, every channel 32-bit FLOAT with x/y sampling 1, the header
//! attributes the OpenEXR file layout requires (`channels`, `compression`, `dataWindow`,
//! `displayWindow`, `lineOrder`, `pixelAspectRatio`, `screenWindowCenter`, `screenWindowWidth`),
//! then one offset per scanline and one block per scanline (`y`, byte count, then each channel's
//! row in the channel list's order). The channel list is sorted by name, as the layout requires.
//!
//! Refused before any allocation (typed `&'static str` codes): an empty or oversized channel set,
//! a name that is empty, longer than 31 bytes (the short-name limit; no long-name flag is
//! written), not printable ASCII or repeated, a plane whose length is not `width x height`, a
//! zero or oversized dimension and a file above [`MAX_EXR_BYTES`]. Integer arithmetic is checked
//! (workflow V4-01).
#![deny(clippy::arithmetic_side_effects)]

/// Largest file the writer produces.
pub const MAX_EXR_BYTES: u64 = 1 << 32;
/// Most channels per file.
pub const MAX_EXR_CHANNELS: usize = 64;
/// Largest width or height (OpenEXR data windows are `i32`).
pub const MAX_EXR_SIDE: u32 = 1 << 20;
pub const EXR_DIMENSIONS: &str = "exr_dimensions";
pub const EXR_CHANNEL_SET: &str = "exr_channel_set";
pub const EXR_CHANNEL_NAME: &str = "exr_channel_name";
pub const EXR_CHANNEL_LENGTH: &str = "exr_channel_length";
pub const EXR_BUDGET: &str = "exr_budget";
/// A header string attribute the writer refuses (lane SMB-AE-3D-3): an empty name, a name longer
/// than 31 bytes or not printable ASCII, a name the writer itself writes, a repeated name, more
/// than [`MAX_EXR_ATTRIBUTES`] attributes or a value longer than [`MAX_EXR_ATTRIBUTE_BYTES`].
pub const EXR_ATTRIBUTE: &str = "exr_attribute";
/// Most extra string attributes per file.
pub const MAX_EXR_ATTRIBUTES: usize = 256;
/// Longest string attribute value (bytes).
pub const MAX_EXR_ATTRIBUTE_BYTES: usize = 1 << 24;
/// The attributes [`encode_exr_f32`] always writes.
const REQUIRED_ATTRIBUTES: [&str; 8] = ["channels", "compression", "dataWindow", "displayWindow", "lineOrder", "pixelAspectRatio", "screenWindowCenter", "screenWindowWidth"];

fn attr(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) -> Result<(), &'static str> {
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(kind.as_bytes());
    out.push(0);
    out.extend_from_slice(&i32::try_from(value.len()).map_err(|_| EXR_BUDGET)?.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

/// Encodes `channels` (name, row-major plane of `width x height` values) as an OpenEXR file.
pub fn encode_exr_f32(width: u32, height: u32, channels: &[(&str, &[f32])]) -> Result<Vec<u8>, &'static str> {
    encode_exr_f32_with_attributes(width, height, channels, &[])
}

/// [`encode_exr_f32`] with extra header attributes of type `string` (lane SMB-AE-3D-3: the
/// Cryptomatte metadata keys), written after the required attributes in the given order; refused
/// [`EXR_ATTRIBUTE`] per its contract.
pub fn encode_exr_f32_with_attributes(width: u32, height: u32, channels: &[(&str, &[f32])], strings: &[(&str, &str)]) -> Result<Vec<u8>, &'static str> {
    if strings.len() > MAX_EXR_ATTRIBUTES {
        return Err(EXR_ATTRIBUTE);
    }
    let mut extra: u64 = 0;
    for (i, (name, value)) in strings.iter().enumerate() {
        let ok = !name.is_empty() && name.len() <= 31 && name.bytes().all(|b| (0x21..=0x7e).contains(&b));
        let repeated = strings.get(..i).is_some_and(|before| before.iter().any(|(n, _)| n == name));
        if !ok || repeated || REQUIRED_ATTRIBUTES.contains(name) || value.len() > MAX_EXR_ATTRIBUTE_BYTES {
            return Err(EXR_ATTRIBUTE);
        }
        extra = extra.checked_add(u64::try_from(value.len().checked_add(48).ok_or(EXR_BUDGET)?).map_err(|_| EXR_BUDGET)?).ok_or(EXR_BUDGET)?;
    }
    if width == 0 || height == 0 || width > MAX_EXR_SIDE || height > MAX_EXR_SIDE {
        return Err(EXR_DIMENSIONS);
    }
    if channels.is_empty() || channels.len() > MAX_EXR_CHANNELS {
        return Err(EXR_CHANNEL_SET);
    }
    let px = u64::from(width).checked_mul(u64::from(height)).ok_or(EXR_BUDGET)?;
    let mut sorted: Vec<(&str, &[f32])> = channels.to_vec();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (i, (name, plane)) in sorted.iter().enumerate() {
        let ok = !name.is_empty() && name.len() <= 31 && name.bytes().all(|b| (0x21..=0x7e).contains(&b));
        let repeated = i.checked_sub(1).and_then(|j| sorted.get(j)).is_some_and(|(prev, _)| prev == name);
        if !ok || repeated {
            return Err(EXR_CHANNEL_NAME);
        }
        if u64::try_from(plane.len()).map_err(|_| EXR_BUDGET)? != px {
            return Err(EXR_CHANNEL_LENGTH);
        }
    }
    let n = u64::try_from(sorted.len()).map_err(|_| EXR_BUDGET)?;
    let row_bytes = u64::from(width).checked_mul(4).and_then(|b| b.checked_mul(n)).ok_or(EXR_BUDGET)?;
    // Header (bounded: at most 64 names of 31 bytes plus fixed attributes) + offsets + blocks.
    let blocks = row_bytes.checked_add(8).and_then(|b| b.checked_mul(u64::from(height))).ok_or(EXR_BUDGET)?;
    let total = blocks.checked_add(u64::from(height).checked_mul(8).ok_or(EXR_BUDGET)?).and_then(|b| b.checked_add(4096)).and_then(|b| b.checked_add(extra)).ok_or(EXR_BUDGET)?;
    if total > MAX_EXR_BYTES {
        return Err(EXR_BUDGET);
    }
    let mut out = Vec::with_capacity(usize::try_from(total).map_err(|_| EXR_BUDGET)?);
    out.extend_from_slice(&20_000_630i32.to_le_bytes());
    out.extend_from_slice(&2i32.to_le_bytes());
    let mut chlist = Vec::new();
    for (name, _) in &sorted {
        chlist.extend_from_slice(name.as_bytes());
        chlist.push(0);
        chlist.extend_from_slice(&2i32.to_le_bytes()); // FLOAT
        chlist.extend_from_slice(&[0, 0, 0, 0]); // pLinear + reserved
        chlist.extend_from_slice(&1i32.to_le_bytes());
        chlist.extend_from_slice(&1i32.to_le_bytes());
    }
    chlist.push(0);
    attr(&mut out, "channels", "chlist", &chlist)?;
    attr(&mut out, "compression", "compression", &[0])?;
    let (xmax, ymax) = (i32::try_from(width.checked_sub(1).ok_or(EXR_DIMENSIONS)?).map_err(|_| EXR_DIMENSIONS)?, i32::try_from(height.checked_sub(1).ok_or(EXR_DIMENSIONS)?).map_err(|_| EXR_DIMENSIONS)?);
    let mut window = Vec::with_capacity(16);
    for v in [0, 0, xmax, ymax] {
        window.extend_from_slice(&v.to_le_bytes());
    }
    attr(&mut out, "dataWindow", "box2i", &window)?;
    attr(&mut out, "displayWindow", "box2i", &window)?;
    attr(&mut out, "lineOrder", "lineOrder", &[0])?;
    attr(&mut out, "pixelAspectRatio", "float", &1.0f32.to_le_bytes())?;
    let mut centre = Vec::with_capacity(8);
    centre.extend_from_slice(&0.0f32.to_le_bytes());
    centre.extend_from_slice(&0.0f32.to_le_bytes());
    attr(&mut out, "screenWindowCenter", "v2f", &centre)?;
    attr(&mut out, "screenWindowWidth", "float", &1.0f32.to_le_bytes())?;
    for (name, value) in strings {
        attr(&mut out, name, "string", value.as_bytes())?;
    }
    out.push(0);
    let table_at = u64::try_from(out.len()).map_err(|_| EXR_BUDGET)?;
    let first = table_at.checked_add(u64::from(height).checked_mul(8).ok_or(EXR_BUDGET)?).ok_or(EXR_BUDGET)?;
    let block = row_bytes.checked_add(8).ok_or(EXR_BUDGET)?;
    for y in 0..u64::from(height) {
        let at = y.checked_mul(block).and_then(|o| o.checked_add(first)).ok_or(EXR_BUDGET)?;
        out.extend_from_slice(&at.to_le_bytes());
    }
    let w = usize::try_from(width).map_err(|_| EXR_BUDGET)?;
    let row_i32 = i32::try_from(row_bytes).map_err(|_| EXR_BUDGET)?;
    for y in 0..height {
        out.extend_from_slice(&i32::try_from(y).map_err(|_| EXR_DIMENSIONS)?.to_le_bytes());
        out.extend_from_slice(&row_i32.to_le_bytes());
        let start = usize::try_from(y).ok().and_then(|y| y.checked_mul(w)).ok_or(EXR_BUDGET)?;
        let end = start.checked_add(w).ok_or(EXR_BUDGET)?;
        for (_, plane) in &sorted {
            for v in plane.get(start..end).ok_or(EXR_CHANNEL_LENGTH)? {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(out)
}
