//! Layer records, tagged blocks, masks and blending ranges. Raw sub-structures are retained
//! next to the parsed views so an unmodified record re-emits byte-identically.

use hsk_studio_accord::CancellationToken;

use crate::blend::BlendMode;
use crate::compression::{self, PlaneGeometry};
use crate::error::{PsdError, Result};
use crate::header::Header;
use crate::limits::Limits;
use crate::reader::Reader;
use crate::resources::read_unicode;

/// Keys whose payload length is 8 bytes in a PSB file (specification, Additional Layer
/// Information).
const WIDE_KEYS: [&[u8; 4]; 13] = [
    b"LMsk", b"Lr16", b"Lr32", b"Layr", b"Mt16", b"Mt32", b"Mtrn", b"Alph", b"FMsk", b"lnk2", b"FEid", b"FXid", b"PxSD",
];

pub const KEY_SECTION_DIVIDER: [u8; 4] = *b"lsct";
pub const KEY_SECTION_DIVIDER_NESTED: [u8; 4] = *b"lsdk";
pub const KEY_UNICODE_NAME: [u8; 4] = *b"luni";
pub const KEY_LAYER_ID: [u8; 4] = *b"lyid";

pub(crate) fn is_wide_key(psb: bool, key: [u8; 4]) -> bool {
    psb && WIDE_KEYS.contains(&&key)
}

fn is_block_signature(bytes: Option<&[u8]>) -> bool {
    matches!(bytes, Some(b"8BIM" | b"8B64"))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

impl Rect {
    pub(crate) fn read(r: &mut Reader<'_>, what: &'static str) -> Result<Self> {
        Ok(Self { top: r.i32(what)?, left: r.i32(what)?, bottom: r.i32(what)?, right: r.i32(what)? })
    }

    pub fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)).clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)).clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn is_valid(&self) -> bool {
        self.bottom >= self.top && self.right >= self.left
    }

    pub fn area(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        for v in [self.top, self.left, self.bottom, self.right] {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaggedBlock {
    /// `8BIM` or `8B64`.
    pub signature: [u8; 4],
    pub key: [u8; 4],
    pub data: Vec<u8>,
    /// Alignment bytes after the payload that are not counted by the length field.
    pub pad: Vec<u8>,
}

/// Parses a run of additional-layer-information blocks. `global` selects the alignment policy
/// of the file-level list (payload followed by padding to a multiple of 4 that the length does
/// not count); layer-level lists carry padding inside the length. If the preferred policy does
/// not land on a block signature (or the end), the other observed policies are tried.
pub(crate) fn parse_tagged_blocks(
    bytes: &[u8],
    psb: bool,
    global: bool,
    limits: &Limits,
) -> Result<(Vec<TaggedBlock>, Vec<u8>)> {
    let mut r = Reader::new(bytes);
    let mut blocks = Vec::new();
    while r.remaining() >= 12 && is_block_signature(r.peek(4)) {
        if blocks.len() >= limits.max_tagged_blocks {
            return Err(PsdError::TaggedBlockLimit);
        }
        let signature = r.array4("tagged_block")?;
        let key = r.array4("tagged_block")?;
        let len = r.length(is_wide_key(psb, key), "tagged_block")?;
        if len > limits.max_tagged_block_bytes {
            return Err(PsdError::TaggedBlockLimit);
        }
        let data = r.bytes(len, "tagged_block")?.to_vec();
        let to4 = (4 - len % 4) % 4;
        let to2 = len % 2;
        let preferred = if global { to4 } else { 0 };
        let rest = r.rest();
        let pad = [preferred, 0, to2, to4]
            .into_iter()
            .find(|&p| p <= rest.len() && (rest.len() == p || is_block_signature(rest.get(p..p + 4))))
            .unwrap_or_else(|| preferred.min(rest.len()));
        let pad = r.bytes(pad, "tagged_block")?.to_vec();
        blocks.push(TaggedBlock { signature, key, data, pad });
    }
    Ok((blocks, r.rest().to_vec()))
}

pub(crate) fn encode_tagged_block(block: &TaggedBlock, psb: bool, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(&block.signature);
    out.extend_from_slice(&block.key);
    if is_wide_key(psb, block.key) {
        out.extend_from_slice(&(block.data.len() as u64).to_be_bytes());
    } else {
        let len = u32::try_from(block.data.len()).map_err(|_| PsdError::Overflow)?;
        out.extend_from_slice(&len.to_be_bytes());
    }
    out.extend_from_slice(&block.data);
    out.extend_from_slice(&block.pad);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PsdChannel {
    /// 0 red, 1 green, 2 blue (and so on per mode), -1 transparency, -2 user mask, -3 real user mask.
    pub id: i16,
    pub compression: u16,
    /// Compressed payload, i.e. the channel data after the 2-byte compression field.
    pub data: Vec<u8>,
}

impl PsdChannel {
    /// Decodes into raw big-endian samples for a `width` x `height` region.
    pub fn decode(
        &self,
        width: u32,
        height: u32,
        header: &Header,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>> {
        let geometry = PlaneGeometry { width, height, depth: header.depth, planes: 1, psb: header.is_psb() };
        compression::decode(self.compression, &self.data, geometry, limits, cancel)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupRole {
    /// Ordinary layer (no `lsct`, or type 0).
    None,
    OpenFolder,
    ClosedFolder,
    /// The hidden end marker of a group; stored below the group's children.
    BoundingDivider,
    /// An `lsct` type outside the specified 0..=3.
    Unknown(u32),
}

/// Parsed view of the layer mask data (the raw bytes stay authoritative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerMask {
    pub rect: Rect,
    pub default_color: u8,
    pub flags: u8,
    pub real_flags: Option<u8>,
    pub real_default_color: Option<u8>,
    pub real_rect: Option<Rect>,
}

impl LayerMask {
    pub fn disabled(&self) -> bool {
        self.flags & 0x02 != 0
    }

    fn parse(raw: &[u8]) -> Option<Self> {
        let mut r = Reader::new(raw);
        let rect = Rect::read(&mut r, "mask").ok()?;
        let default_color = r.u8("mask").ok()?;
        let flags = r.u8("mask").ok()?;
        // Real-mask fields follow only in the 36-byte form without a parameters byte.
        let (real_flags, real_default_color, real_rect) = if raw.len() >= 36 && flags & 0x10 == 0 {
            (r.u8("mask").ok(), r.u8("mask").ok(), Rect::read(&mut r, "mask").ok())
        } else {
            (None, None, None)
        };
        Some(Self { rect, default_color, flags, real_flags, real_default_color, real_rect })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlendRanges {
    pub composite_source: [u8; 4],
    pub composite_destination: [u8; 4],
    pub channels: Vec<([u8; 4], [u8; 4])>,
}

impl BlendRanges {
    fn parse(raw: &[u8]) -> Option<Self> {
        let mut r = Reader::new(raw);
        let composite_source = r.array4("blend_ranges").ok()?;
        let composite_destination = r.array4("blend_ranges").ok()?;
        let mut channels = Vec::new();
        while r.remaining() >= 8 {
            channels.push((r.array4("blend_ranges").ok()?, r.array4("blend_ranges").ok()?));
        }
        Some(Self { composite_source, composite_destination, channels })
    }

    /// Full-range source and destination everywhere: black 0..0, white 255..255.
    pub fn is_default(&self) -> bool {
        const FULL: [u8; 4] = [0, 0, 255, 255];
        self.composite_source == FULL
            && self.composite_destination == FULL
            && self.channels.iter().all(|(s, d)| *s == FULL && *d == FULL)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PsdLayer {
    pub rect: Rect,
    pub channels: Vec<PsdChannel>,
    pub blend_signature: [u8; 4],
    /// Blend key exactly as stored; see [`PsdLayer::blend_mode`].
    pub blend_key: [u8; 4],
    pub opacity: u8,
    /// 0 base, 1 non-base (clipped to the layer below).
    pub clipping: u8,
    /// Raw flags byte; see the accessors for bit meanings.
    pub flags: u8,
    pub filler: u8,
    /// Layer mask / adjustment data, raw, without its 4-byte size field.
    pub mask_raw: Vec<u8>,
    /// Layer blending ranges, raw, without their 4-byte length field.
    pub blending_ranges_raw: Vec<u8>,
    /// Display name: `luni` if present, else the Pascal name read as Latin-1. Setting a different
    /// value is how a rename reaches the writer.
    pub name: String,
    /// Pascal name content as stored (without length byte or padding).
    pub name_pascal: Vec<u8>,
    pub name_pad: Vec<u8>,
    pub blocks: Vec<TaggedBlock>,
    /// Bytes of the extra-data area after the last parsed block.
    pub extra_trailing: Vec<u8>,
}

impl PsdLayer {
    /// Bit 1 SET means hidden. The specification text calls bit 1 "visible"; two independent
    /// implementations (PhotoCraft psd crate, psd-tools/ag-psd behaviour) read it as hidden.
    /// UNVERIFIED against a Photoshop-authored fixture; keep [`PsdLayer::flags`] raw.
    pub fn hidden(&self) -> bool {
        self.flags & 0x02 != 0
    }

    pub fn visible(&self) -> bool {
        !self.hidden()
    }

    pub fn transparency_protected(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Bits 3 and 4: pixel data is irrelevant to the appearance of the document.
    pub fn pixel_data_irrelevant(&self) -> bool {
        self.flags & 0x08 != 0 && self.flags & 0x10 != 0
    }

    /// `None` when the key is not one of the 28 known keys (a typed loss, never Normal).
    pub fn blend_mode(&self) -> Option<BlendMode> {
        BlendMode::from_key(self.blend_key)
    }

    pub fn block(&self, key: &[u8; 4]) -> Option<&TaggedBlock> {
        self.blocks.iter().find(|b| &b.key == key)
    }

    pub fn group_role(&self) -> GroupRole {
        let block = self.block(&KEY_SECTION_DIVIDER).or_else(|| self.block(&KEY_SECTION_DIVIDER_NESTED));
        let Some(data) = block.and_then(|b| b.data.get(..4)) else {
            return GroupRole::None;
        };
        match u32::from_be_bytes([data[0], data[1], data[2], data[3]]) {
            0 => GroupRole::None,
            1 => GroupRole::OpenFolder,
            2 => GroupRole::ClosedFolder,
            3 => GroupRole::BoundingDivider,
            other => GroupRole::Unknown(other),
        }
    }

    /// Blend key carried by `lsct` (present when its payload is at least 12 bytes).
    pub fn group_blend_key(&self) -> Option<[u8; 4]> {
        let block = self.block(&KEY_SECTION_DIVIDER).or_else(|| self.block(&KEY_SECTION_DIVIDER_NESTED))?;
        let d = block.data.get(8..12)?;
        (block.data.get(4..8)? == b"8BIM").then_some([d[0], d[1], d[2], d[3]])
    }

    pub fn layer_id(&self) -> Option<u32> {
        let d = self.block(&KEY_LAYER_ID)?.data.get(..4)?;
        Some(u32::from_be_bytes([d[0], d[1], d[2], d[3]]))
    }

    fn flag_block(&self, key: &[u8; 4]) -> Option<bool> {
        self.block(key)?.data.first().map(|&v| v != 0)
    }

    /// `clbl`: blend clipped elements as a group.
    pub fn blend_clipped_elements(&self) -> Option<bool> {
        self.flag_block(b"clbl")
    }

    /// `infx`: blend interior elements as a group.
    pub fn blend_interior_elements(&self) -> Option<bool> {
        self.flag_block(b"infx")
    }

    pub fn knockout(&self) -> Option<bool> {
        self.flag_block(b"knko")
    }

    pub fn transparency_shapes_layer(&self) -> Option<bool> {
        self.flag_block(b"tsly")
    }

    pub fn mask(&self) -> Option<LayerMask> {
        if self.mask_raw.is_empty() { None } else { LayerMask::parse(&self.mask_raw) }
    }

    pub fn blend_ranges(&self) -> Option<BlendRanges> {
        if self.blending_ranges_raw.is_empty() { None } else { BlendRanges::parse(&self.blending_ranges_raw) }
    }

    /// Rectangle that the channel's samples cover: mask channels use the mask rectangles.
    pub fn channel_rect(&self, channel_id: i16) -> Rect {
        match (channel_id, self.mask()) {
            (-2, Some(m)) => m.rect,
            (-3, Some(m)) => m.real_rect.unwrap_or(m.rect),
            _ => self.rect,
        }
    }

    /// Name as the file itself states it (`luni`, else Pascal as Latin-1).
    pub fn stored_name(&self, limits: &Limits) -> String {
        if let Some(block) = self.block(&KEY_UNICODE_NAME) {
            let mut r = Reader::new(&block.data);
            if let Ok(name) = read_unicode(&mut r, limits.max_name_units) {
                return name;
            }
        }
        self.name_pascal.iter().map(|&b| char::from(b)).collect()
    }
}

/// Parses one record. Returns the layer (channel payloads still empty) and the per-channel data
/// lengths announced by the record.
pub(crate) fn read_record(
    r: &mut Reader<'_>,
    header: &Header,
    limits: &Limits,
) -> Result<(PsdLayer, Vec<usize>)> {
    let psb = header.is_psb();
    let rect = Rect::read(r, "layer_rect")?;
    if !rect.is_valid() {
        return Err(PsdError::LayerRect);
    }
    if rect.area() > limits.max_pixels {
        return Err(PsdError::PixelLimit);
    }
    let channel_count = usize::from(r.u16("layer_channels")?);
    if channel_count > 64 {
        return Err(PsdError::BadChannelCount);
    }
    let mut channels = Vec::with_capacity(channel_count);
    let mut lengths = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        let id = r.i16("channel_info")?;
        let len = r.length(psb, "channel_info")?;
        channels.push(PsdChannel { id, compression: 0, data: Vec::new() });
        lengths.push(len);
    }
    let blend_signature = r.array4("layer_blend")?;
    let blend_key = r.array4("layer_blend")?;
    let opacity = r.u8("layer_blend")?;
    let clipping = r.u8("layer_blend")?;
    let flags = r.u8("layer_blend")?;
    let filler = r.u8("layer_blend")?;
    let extra_len = r.length(false, "layer_extra")?;
    let mut e = Reader::new(r.bytes(extra_len, "layer_extra")?);
    let mask_len = e.length(false, "layer_mask")?;
    let mask_raw = e.bytes(mask_len, "layer_mask")?.to_vec();
    let ranges_len = e.length(false, "blending_ranges")?;
    let blending_ranges_raw = e.bytes(ranges_len, "blending_ranges")?.to_vec();
    let name_len = usize::from(e.u8("layer_name")?);
    let name_pascal = e.bytes(name_len, "layer_name")?.to_vec();
    let name_pad = e.skip_up_to((4 - (1 + name_len) % 4) % 4).to_vec();
    let (blocks, extra_trailing) = parse_tagged_blocks(e.rest(), psb, false, limits)?;
    let mut layer = PsdLayer {
        rect,
        channels,
        blend_signature,
        blend_key,
        opacity,
        clipping,
        flags,
        filler,
        mask_raw,
        blending_ranges_raw,
        name: String::new(),
        name_pascal,
        name_pad,
        blocks,
        extra_trailing,
    };
    layer.name = layer.stored_name(limits);
    Ok((layer, lengths))
}

/// Reads the per-channel payloads that follow all records, in record order.
pub(crate) fn read_channel_data(
    r: &mut Reader<'_>,
    layers: &mut [PsdLayer],
    lengths: &[Vec<usize>],
    cancel: &CancellationToken,
) -> Result<()> {
    for (layer, lens) in layers.iter_mut().zip(lengths) {
        cancel.check()?;
        for (channel, &len) in layer.channels.iter_mut().zip(lens) {
            if len < 2 {
                return Err(PsdError::ChannelLength);
            }
            let bytes = r.bytes(len, "channel_data")?;
            channel.compression = u16::from_be_bytes([bytes[0], bytes[1]]);
            channel.data = bytes[2..].to_vec();
        }
    }
    Ok(())
}

pub(crate) fn encode_record(layer: &PsdLayer, header: &Header, out: &mut Vec<u8>) -> Result<()> {
    let psb = header.is_psb();
    layer.rect.encode(out);
    out.extend_from_slice(&u16::try_from(layer.channels.len()).map_err(|_| PsdError::Overflow)?.to_be_bytes());
    for c in &layer.channels {
        out.extend_from_slice(&c.id.to_be_bytes());
        let len = c.data.len() + 2;
        if psb {
            out.extend_from_slice(&(len as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&u32::try_from(len).map_err(|_| PsdError::Overflow)?.to_be_bytes());
        }
    }
    out.extend_from_slice(&layer.blend_signature);
    out.extend_from_slice(&layer.blend_key);
    out.extend_from_slice(&[layer.opacity, layer.clipping, layer.flags, layer.filler]);
    let mut extra = Vec::new();
    extra.extend_from_slice(&u32::try_from(layer.mask_raw.len()).map_err(|_| PsdError::Overflow)?.to_be_bytes());
    extra.extend_from_slice(&layer.mask_raw);
    extra.extend_from_slice(
        &u32::try_from(layer.blending_ranges_raw.len()).map_err(|_| PsdError::Overflow)?.to_be_bytes(),
    );
    extra.extend_from_slice(&layer.blending_ranges_raw);
    extra.push(u8::try_from(layer.name_pascal.len()).map_err(|_| PsdError::Overflow)?);
    extra.extend_from_slice(&layer.name_pascal);
    extra.extend_from_slice(&layer.name_pad);
    for block in &layer.blocks {
        encode_tagged_block(block, psb, &mut extra)?;
    }
    extra.extend_from_slice(&layer.extra_trailing);
    out.extend_from_slice(&u32::try_from(extra.len()).map_err(|_| PsdError::Overflow)?.to_be_bytes());
    out.extend_from_slice(&extra);
    Ok(())
}
