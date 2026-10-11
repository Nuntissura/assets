//! Whole-file model and the bounded reader. The model keeps every section's raw bytes next to
//! the parsed views so an unmodified document re-emits identically (see `writer`).

use hsk_studio_accord::CancellationToken;

use crate::compression::{self, PlaneGeometry};
use crate::error::{PsdError, Result};
use crate::header::{ColorMode, HEADER_BYTES, Header};
use crate::layers::{self, GroupRole, PsdLayer, TaggedBlock};
use crate::limits::Limits;
use crate::pixels::Plane;
use crate::reader::Reader;
use crate::resources::{ImageResources, VersionInfo};
use crate::samples::{SixteenBitScale, srgb_to_linear};

const LAYER_INFO_KEYS: [[u8; 4]; 3] = [*b"Lr16", *b"Lr32", *b"Layr"];

/// Where the layer list was stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerInfoLocation {
    /// The primary layer-info area of the layer and mask section.
    Primary,
    /// Inside a global tagged block (`Lr16`, `Lr32` or `Layr`); the primary area is empty.
    Tagged { signature: [u8; 4], key: [u8; 4], insert_at: usize, pad: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerInfo {
    pub layers: Vec<PsdLayer>,
    /// Layer count was negative: the first alpha channel holds the merged transparency.
    pub merged_alpha: bool,
    /// Bytes inside the layer-info length after the last channel payload (alignment padding).
    pub trailing: Vec<u8>,
    pub location: LayerInfoLocation,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerSection {
    pub layer_info: Option<LayerInfo>,
    /// Global layer mask info payload (without its 4-byte length); `None` when the section ends
    /// after the layer info.
    pub global_mask: Option<Vec<u8>>,
    /// Global additional-layer-information blocks.
    pub blocks: Vec<TaggedBlock>,
    /// Section bytes after the parsed content.
    pub trailing: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    pub compression: u16,
    /// Everything after the compression field.
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PsdDocument {
    pub header: Header,
    pub color_mode_data: Vec<u8>,
    pub resources: ImageResources,
    pub layer_section: LayerSection,
    /// `None` when the file ends before an image-data section.
    pub image_data: Option<ImageData>,
    /// The 0 or 1 bytes after the layer section when too short to be an image-data section; kept
    /// so an unmodified document re-emits byte-identically.
    pub tail: Vec<u8>,
}

/// Whether the merged image can serve as an oracle for layer recomposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergedStatus {
    /// Resource 1057 says `hasRealMergedData`.
    Real,
    /// Resource 1057 says the merged image is a placeholder (no oracle).
    Placeholder,
    /// No resource 1057; cannot tell.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeNode {
    Layer { index: usize },
    Group { folder: usize, divider: usize, children: Vec<TreeNode> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedImage {
    pub color_mode: ColorMode,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub planes: Vec<Plane>,
    pub merged_alpha: bool,
}

impl MergedImage {
    /// 8-bit RGBA from an RGB or grayscale merged image. 16-bit samples are scaled by 255/65535
    /// (scale UNVERIFIED against Photoshop output). Alpha comes from the first extra plane only
    /// when the file declared merged transparency; otherwise it is opaque.
    pub fn to_rgba8(&self) -> Result<Vec<u8>> {
        let colour_planes = match self.color_mode {
            ColorMode::Rgb => 3,
            ColorMode::Grayscale => 1,
            _ => return Err(PsdError::UnsupportedMode("merged_color_mode")),
        };
        if self.planes.len() < colour_planes {
            return Err(PsdError::ChannelSize);
        }
        let eight = |plane: &Plane| -> Result<Vec<u8>> {
            match self.depth {
                8 => Ok(plane.bytes.clone()),
                16 => Ok(plane
                    .bytes
                    .chunks_exact(2)
                    .map(|p| ((u32::from(u16::from_be_bytes([p[0], p[1]])) * 255 + 32767) / 65535) as u8)
                    .collect()),
                _ => Err(PsdError::UnsupportedMode("merged_depth")),
            }
        };
        let colour = self.planes[..colour_planes].iter().map(eight).collect::<Result<Vec<_>>>()?;
        let alpha = if self.merged_alpha { self.planes.get(colour_planes).map(eight).transpose()? } else { None };
        let pixels = (self.width as usize).checked_mul(self.height as usize).ok_or(PsdError::Overflow)?;
        let mut out = Vec::with_capacity(pixels.checked_mul(4).ok_or(PsdError::Overflow)?);
        for i in 0..pixels {
            let (r, g, b) = match colour.as_slice() {
                [gray] => (gray[i], gray[i], gray[i]),
                [r, g, b] => (r[i], g[i], b[i]),
                _ => return Err(PsdError::UnsupportedMode("merged_color_mode")),
            };
            out.extend_from_slice(&[r, g, b, alpha.as_ref().map_or(255, |a| a[i])]);
        }
        Ok(out)
    }

    /// Linear-light RGBA as f32 for RGB or grayscale merged images at any supported depth:
    /// 8/16-bit samples through the sRGB decoding (assumed profile), 32-bit floats as stored.
    /// Alpha is the first extra plane only when the file declared merged transparency.
    pub fn to_linear_rgba_f32(&self, scale: SixteenBitScale) -> Result<Vec<f32>> {
        let colour_planes = match self.color_mode {
            ColorMode::Rgb => 3,
            ColorMode::Grayscale => 1,
            _ => return Err(PsdError::UnsupportedMode("merged_color_mode")),
        };
        if !matches!(self.depth, 8 | 16 | 32) {
            return Err(PsdError::UnsupportedMode("merged_depth"));
        }
        if self.planes.len() < colour_planes {
            return Err(PsdError::ChannelSize);
        }
        let alpha = if self.merged_alpha { self.planes.get(colour_planes) } else { None };
        let pixels = (self.width as usize).checked_mul(self.height as usize).ok_or(PsdError::Overflow)?;
        let mut out = Vec::with_capacity(pixels.checked_mul(4).ok_or(PsdError::Overflow)?);
        let unit = |plane: &Plane, i: usize| plane.unit_at(i, scale).ok_or(PsdError::ChannelSize);
        let linear = |v: f32| if self.depth == 32 { v } else { srgb_to_linear(v) };
        for i in 0..pixels {
            let rgb = if colour_planes == 1 {
                [linear(unit(&self.planes[0], i)?); 3]
            } else {
                [linear(unit(&self.planes[0], i)?), linear(unit(&self.planes[1], i)?), linear(unit(&self.planes[2], i)?)]
            };
            out.extend_from_slice(&rgb);
            out.push(match alpha {
                Some(plane) => unit(plane, i)?.clamp(0.0, 1.0),
                None => 1.0,
            });
        }
        Ok(out)
    }
}

impl PsdDocument {
    pub fn layers(&self) -> &[PsdLayer] {
        self.layer_section.layer_info.as_ref().map_or(&[], |i| i.layers.as_slice())
    }

    pub fn merged_status(&self, limits: &Limits) -> MergedStatus {
        match self.resources.version_info(limits.max_name_units) {
            Some(VersionInfo { has_real_merged_data: true, .. }) => MergedStatus::Real,
            Some(_) => MergedStatus::Placeholder,
            None => MergedStatus::Unknown,
        }
    }

    /// Rebuilds the group hierarchy in panel order (top-most first). The bounding divider (type
    /// 3) is stored below a group's children and its folder record (type 1/2) above them, so the
    /// file order is walked from the last record to the first.
    pub fn tree(&self, limits: &Limits) -> Result<Vec<TreeNode>> {
        let layers = self.layers();
        let mut root: Vec<TreeNode> = Vec::new();
        let mut stack: Vec<(usize, Vec<TreeNode>)> = Vec::new();
        for index in (0..layers.len()).rev() {
            match layers[index].group_role() {
                GroupRole::OpenFolder | GroupRole::ClosedFolder => {
                    if stack.len() >= limits.max_group_depth {
                        return Err(PsdError::GroupDepth);
                    }
                    stack.push((index, Vec::new()));
                }
                GroupRole::BoundingDivider => {
                    let (folder, children) = stack.pop().ok_or(PsdError::GroupUnbalanced)?;
                    let node = TreeNode::Group { folder, divider: index, children };
                    stack.last_mut().map_or(&mut root, |(_, list)| list).push(node);
                }
                GroupRole::None | GroupRole::Unknown(_) => {
                    stack.last_mut().map_or(&mut root, |(_, list)| list).push(TreeNode::Layer { index });
                }
            }
        }
        if stack.is_empty() { Ok(root) } else { Err(PsdError::GroupUnbalanced) }
    }

    /// RGBA8 `(width, height, pixels)` of the merged image for use as a recomposition oracle.
    /// `Ok(None)` unless resource 1057 says `hasRealMergedData` (a placeholder, or a file that does
    /// not say, is never served as an oracle; callers wanting the pixels anyway use
    /// [`PsdDocument::decode_merged`]), when there is none, or when the colour mode or depth has no
    /// 8-bit conversion here.
    pub fn merged_oracle_rgba8(
        &self,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Option<(u32, u32, Vec<u8>)>> {
        if self.merged_status(limits) != MergedStatus::Real {
            return Ok(None);
        }
        let Some(image) = self.decode_merged(limits, cancel)? else {
            return Ok(None);
        };
        match image.to_rgba8() {
            Ok(pixels) => Ok(Some((image.width, image.height, pixels))),
            Err(PsdError::UnsupportedMode(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Decodes the merged image (the Image Data section). `Ok(None)` when the file has none.
    pub fn decode_merged(&self, limits: &Limits, cancel: &CancellationToken) -> Result<Option<MergedImage>> {
        let Some(image) = &self.image_data else {
            return Ok(None);
        };
        let h = &self.header;
        let geometry = PlaneGeometry {
            width: h.width,
            height: h.height,
            depth: h.depth,
            planes: usize::from(h.channels),
            psb: h.is_psb(),
        };
        let raw = compression::decode(image.compression, &image.data, geometry, limits, cancel)?;
        let plane_len = geometry.plane_bytes()?;
        let planes = raw
            .chunks_exact(plane_len)
            .map(|c| Plane { width: h.width, height: h.height, depth: h.depth, bytes: c.to_vec() })
            .collect();
        let merged_alpha = self.layer_section.layer_info.as_ref().is_some_and(|i| i.merged_alpha);
        Ok(Some(MergedImage {
            color_mode: h.color_mode,
            width: h.width,
            height: h.height,
            depth: h.depth,
            planes,
            merged_alpha,
        }))
    }
}

/// Parses a PSD or PSB file. Never panics on hostile input; every length is checked against the
/// remaining bytes and the supplied [`Limits`] before it is used.
pub fn read_psd(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<PsdDocument> {
    cancel.check()?;
    if bytes.len() > limits.max_input_bytes {
        return Err(PsdError::InputTooLarge);
    }
    if bytes.len() < HEADER_BYTES {
        // Distinguish a wrong file type from a truncated one.
        return Err(if bytes.len() >= 4 && &bytes[..4] != b"8BPS" {
            PsdError::BadSignature
        } else {
            PsdError::Truncated("header")
        });
    }
    let mut r = Reader::new(bytes);
    let header = Header::read(&mut r, limits)?;
    let psb = header.is_psb();

    let color_mode_len = r.length(false, "color_mode_data")?;
    let color_mode_data = r.bytes(color_mode_len, "color_mode_data")?.to_vec();

    cancel.check()?;
    let resources_len = r.length(false, "image_resources")?;
    let resources = ImageResources::parse(r.bytes(resources_len, "image_resources")?, limits)?;

    cancel.check()?;
    let section_len = r.length(psb, "layer_and_mask")?;
    let layer_section = read_layer_section(r.bytes(section_len, "layer_and_mask")?, &header, limits, cancel)?;

    let (image_data, tail) = if r.remaining() >= 2 {
        let compression = r.u16("image_data")?;
        (Some(ImageData { compression, data: r.rest().to_vec() }), Vec::new())
    } else {
        (None, r.rest().to_vec())
    };
    Ok(PsdDocument { header, color_mode_data, resources, layer_section, image_data, tail })
}

fn read_layer_section(
    bytes: &[u8],
    header: &Header,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<LayerSection> {
    if bytes.is_empty() {
        return Ok(LayerSection::default());
    }
    let psb = header.is_psb();
    let mut r = Reader::new(bytes);
    let info_len = r.length(psb, "layer_info")?;
    let info_bytes = r.bytes(info_len, "layer_info")?;
    let mut layer_info = if info_len > 0 { Some(read_layer_info(info_bytes, header, limits, cancel)?) } else { None };

    let mut section = LayerSection::default();
    if r.remaining() >= 4 {
        let mask_len = r.length(false, "global_mask")?;
        section.global_mask = Some(r.bytes(mask_len, "global_mask")?.to_vec());
        let (blocks, trailing) = layers::parse_tagged_blocks(r.rest(), psb, true, limits)?;
        section.blocks = blocks;
        section.trailing = trailing;
    } else {
        section.trailing = r.rest().to_vec();
    }

    if layer_info.is_none()
        && let Some(at) = section.blocks.iter().position(|b| LAYER_INFO_KEYS.contains(&b.key))
    {
        let block = section.blocks.remove(at);
        let mut info = read_layer_info(&block.data, header, limits, cancel)?;
        info.location =
            LayerInfoLocation::Tagged { signature: block.signature, key: block.key, insert_at: at, pad: block.pad };
        layer_info = Some(info);
    }
    section.layer_info = layer_info;
    Ok(section)
}

fn read_layer_info(bytes: &[u8], header: &Header, limits: &Limits, cancel: &CancellationToken) -> Result<LayerInfo> {
    let mut r = Reader::new(bytes);
    let count = r.i16("layer_count")?;
    let merged_alpha = count < 0;
    let count = usize::from(count.unsigned_abs());
    if count > limits.max_layers {
        return Err(PsdError::LayerLimit);
    }
    let mut layer_list = Vec::with_capacity(count.min(1024));
    let mut lengths = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        cancel.check()?;
        let (layer, lens) = layers::read_record(&mut r, header, limits)?;
        layer_list.push(layer);
        lengths.push(lens);
    }
    layers::read_channel_data(&mut r, &mut layer_list, &lengths, cancel)?;
    Ok(LayerInfo { layers: layer_list, merged_alpha, trailing: r.rest().to_vec(), location: LayerInfoLocation::Primary })
}
