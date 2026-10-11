//! PSD/PSB conversion with provenance and structured unsupported/loss report; preserve
//! originals/unknown bytes.
//!
//! This crate carries its own parser written from the public Adobe Photoshop File Format
//! Specification. It copies no code from other PSD readers and uses no Adobe runtime. Output
//! of the importer is a proposal, never a second authority.
#![forbid(unsafe_code)]

mod blend;
mod compression;
mod document;
mod error;
mod header;
mod layers;
mod limits;
mod loss;
mod pixels;
mod reader;
mod resources;
mod writer;

pub use blend::BlendMode;
pub use compression::{
    COMPRESSION_RAW, COMPRESSION_RLE, COMPRESSION_ZIP, COMPRESSION_ZIP_PREDICTION, PlaneGeometry, decode as decode_samples,
    encode_rle, pack_row, row_bytes,
};
pub use document::{
    ImageData, LayerInfo, LayerInfoLocation, LayerSection, MergedImage, MergedStatus, PsdDocument, TreeNode, read_psd,
};
pub use error::{PsdError, Result};
pub use header::{ColorMode, HEADER_BYTES, Header};
pub use layers::{BlendRanges, GroupRole, LayerMask, PsdChannel, PsdLayer, Rect, TaggedBlock};
pub use limits::Limits;
pub use loss::{LossEntry, LossKind, LossReport, key_text};
pub use pixels::Plane;
pub use writer::write_psd;
pub use resources::{
    ImageResource, ImageResources, RESOURCE_ICC_PROFILE, RESOURCE_ICC_UNTAGGED, RESOURCE_VERSION_INFO, VersionInfo,
};

/// Machine-readable module descriptor (limits and honest capability statement).
pub const DESCRIPTOR: &str = concat!(
    "{\"module\":\"hsk-studio-interop-psd\",\"version\":\"0.1.0\",",
    "\"responsibility\":\"PSD/PSB conversion with provenance and structured unsupported/loss report; preserve originals/unknown bytes\",",
    "\"formats\":[\"psd\",\"psb\"],",
    "\"compressions\":{\"raw\":\"spec\",\"rle_packbits\":\"spec\",\"zip\":\"zlib,behaviour-reference\",\"zip_prediction\":\"depth8,behaviour-reference;depth16_32:unverified\"},",
    "\"limits\":{\"max_input_bytes\":536870912,\"max_layers\":8192,\"max_pixels\":268435456,\"max_decoded_bytes\":1073741824,",
    "\"max_tagged_block_bytes\":268435456,\"max_tagged_blocks\":16384,\"max_name_units\":1024,\"max_resources\":65536,\"max_group_depth\":128},",
    "\"unknown_blend_mode\":\"typed_loss_never_normal\",",
    "\"unknown_bytes\":\"preserved_byte_exact\",",
    "\"oracle\":\"merged_image_when_hasRealMergedData\",",
    "\"pending\":[\"proposal_mapping\",\"icc_conversion\",\"layer_mask_application\",\"text_smart_object_effects\",\"16_32_bit_prediction_oracle\"]}"
);
