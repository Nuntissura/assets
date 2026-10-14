//! Photoshop ImageSourceData (tag 37724) envelope: signature and size-width key only. The tagged blocks
//! that follow (layer records) are parsed by interop-psd.
use crate::error::{Result, TiffError};
use crate::model::{ByteOrder, RawBlock};

const SIGNATURE: &[u8] = b"Adobe Photoshop Document Data Block\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSourceData {
    /// Byte order of the tagged blocks (from the key; independent of the TIFF byte order).
    pub byte_order: ByteOrder,
    /// Block length fields are 64-bit (`8B64`/`46B8`) instead of 32-bit (`8BIM`/`MIB8`).
    pub wide_sizes: bool,
    /// Everything after the signature and key.
    pub blocks: Vec<u8>,
}

/// Validates the signature and the 4-byte key (`8BIM` BE32, `MIB8` LE32, `8B64` BE64, `46B8` LE64).
pub fn image_source_data(block: &RawBlock) -> Result<ImageSourceData> {
    let b = &block.bytes;
    if b.get(..SIGNATURE.len()) != Some(SIGNATURE) {
        return Err(TiffError::Corrupt("image_source_data_signature"));
    }
    let key_end = SIGNATURE.len().checked_add(4).ok_or(TiffError::Truncated)?;
    let key = b.get(SIGNATURE.len()..key_end).ok_or(TiffError::Truncated)?;
    let (byte_order, wide_sizes) = match key {
        b"8BIM" => (ByteOrder::Big, false),
        b"MIB8" => (ByteOrder::Little, false),
        b"8B64" => (ByteOrder::Big, true),
        b"46B8" => (ByteOrder::Little, true),
        _ => return Err(TiffError::unsupported("image_source_data_key")),
    };
    Ok(ImageSourceData { byte_order, wide_sizes, blocks: b.get(key_end..).unwrap_or(&[]).to_vec() })
}
