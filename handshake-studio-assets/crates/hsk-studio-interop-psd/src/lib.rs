//! PSD/PSB conversion with provenance and structured unsupported/loss report; preserve
//! originals/unknown bytes.
//!
//! This crate carries its own parser written from the public Adobe Photoshop File Format
//! Specification. It copies no code from other PSD readers and uses no Adobe runtime. Output
//! of the importer is a proposal, never a second authority.
#![forbid(unsafe_code)]

mod blend;
mod compression;
mod convert;
mod document;
mod error;
mod export;
mod header;
mod layers;
mod limits;
mod loss;
mod pixels;
mod proposal;
mod reader;
mod report;
mod samples;
mod resources;
mod writer;

pub use blend::{PSD_BLEND_KEYS, StudioBlendMode, psd_key_for, studio_blend_for_key};
pub use compression::{
    COMPRESSION_RAW, COMPRESSION_RLE, COMPRESSION_ZIP, COMPRESSION_ZIP_PREDICTION, PlaneGeometry, decode as decode_samples,
    encode_rle, pack_row, row_bytes,
};
pub use convert::{IccConverter, IccRefusal, PrismConverter, PrismRoute, refusal_code};
pub use document::{
    ImageData, LayerInfo, LayerInfoLocation, LayerSection, MergedImage, MergedStatus, PsdDocument, TreeNode, read_psd,
};
pub use error::{PsdError, Result};
pub use export::{ExportOptions, ExportReport, apply_edits, export_psd};
pub use header::{ColorMode, HEADER_BYTES, Header};
pub use layers::{BlendRanges, GroupRole, LayerMask, PsdChannel, PsdLayer, Rect, TaggedBlock};
pub use limits::Limits;
pub use loss::{LossEntry, LossKind, LossReport, key_text};
pub use pixels::Plane;
pub use proposal::{
    IdMint, ImportProposal, LayerExtras, LayerMapEntry, MaskTileBlob, ProposalConfig, Provenance, SourceIdentity, TILE_EDGE,
    TileBlob, to_proposal,
};
pub use report::{ReportContext, outcome_of, report};
pub use samples::{SixteenBitScale, linear_to_srgb, sample_to_unit, srgb_to_linear, unit_to_sample};
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
    "\"api\":\"read_psd / write_psd / to_proposal / export_psd / apply_edits / report; PsdDocument::{tree,loss_report,decode_merged,merged_oracle_rgba8}\",",
    "\"capabilities\":[\"layer_records_groups_resources\",\"byte_exact_rewrite\",\"typed_loss_report\",",
    "\"folio_proposal_hierarchy_appearance_masks\",\"pigment_tiles_8_16_32_bit_full_precision\",\"embedded_icc_rgb_via_converter_when_accepted\",",
    "\"observe_terminal_frame\",\"export_edited_folio_layer_properties_preserving_original_bytes\"],",
    "\"blend\":\"psd_key_table_to_pigment_StudioBlendMode;unknown_key_typed_loss_never_normal;pass_is_group_flag\",",
    "\"unknown_bytes\":\"preserved_byte_exact\",",
    "\"oracle\":\"merged_image_only_when_hasRealMergedData\",",
    "\"pending\":[\"photoshop_authored_oracle_fixtures\",\"cmyk_lab_gray_icc_display_conversion_dependency_SMB-PRISM-ENG-3\",",
    "\"vector_mask_and_layer_effect_rendering\",\"text_smart_object_editing\",\"mask_density_feather\",",
    "\"sixteen_bit_scale_and_float32_transfer_verification\"]}"
);
