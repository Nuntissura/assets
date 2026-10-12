//! PSD/PSB conversion with provenance and structured unsupported/loss report; preserve
//! originals/unknown bytes.
//!
//! This crate carries its own parser written from the public Adobe Photoshop File Format
//! Specification. It copies no code from other PSD readers and uses no Adobe runtime. Output
//! of the importer is a proposal, not a second authority.
#![forbid(unsafe_code)]

mod adjust;
mod blend;
mod compression;
mod convert;
mod descriptor;
mod document;
mod effects;
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
pub use convert::{
    AbsoluteSummary, BpcSummary, ClipMethodKind, ClipSummary, EngineSummary, IccConverter, IccRefusal, PrismConverter, PrismRoute,
    ProfileDevice, ProfileFacts, RenderingIntent, WIDE_UNAVAILABLE, WideConvertRequest, WideConvertSource, WideConverted,
    refusal_code,
};
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
    DisplayConversion, GAMUT_CLAMP_TOLERANCE, IdMint, ImportProposal, LayerExtras, LayerMapEntry, MaskTileBlob,
    ProposalConfig, Provenance, SourceIdentity, TILE_EDGE, TileBlob, to_proposal,
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
    "\"observe_terminal_frame\",\"export_edited_folio_layer_properties_preserving_original_bytes\",",
    "\"cmyk_embedded_icc_display_conversion_8_16_bit\",\"lab_pcs_native_display_conversion_8_16_bit\",\"gray_ktrc_icc_display_conversion_8_16_bit\",",
    "\"raster_records_adjustments_levels_curves_hue_saturation_brightness_contrast_exposure_invert_threshold_posterize\",",
    "\"raster_records_blend_if_mask_density_feather_knockout_blend_toggles\",\"raster_records_lfx2_effect_stack_not_rendered\",",
    "\"adjustment_layers_as_graph_sources_with_input_ordinal\",\"layer_clipped_to_adjustment_unclipped_with_typed_loss\",",
    "\"converter_absolute_bpc_engine_report_in_display_receipt\",\"absolute_media_white_divergence_as_transformed_loss_entry\",",
    "\"blend_if_on_clip_base_or_clipped_layer_unsupported_loss_entry\",\"blend_clipped_layers_as_group_off_unsupported_loss_entry\",",
    "\"effects_with_blend_if_on_one_layer_unsupported_loss_entry\",\"mask_on_pass_through_group_unsupported_loss_entry\",",
    "\"effects_over_layer_limit_unsupported_loss_entry_effects_over_budget\",\"effects_over_document_cap_unsupported_loss_entries_import_continues\"],",
    "\"raster_records\":\"ImportProposal::raster_records (folio StudioRasterRecords) validated by validate_raster_records; records only, nothing applied or rendered; unreadable or out-of-range record keeps the layer a typed unsupported layer; IdMint::effect_stack_id defaults to None (effects preserved and reported)\",",
    "\"effect_limits\":\"counted_instances_are_the_enabled_instances_of_enabled_stacks_on_visible_layers_in_visible_groups_as_composite_convert_effects_and_pigment_plan_extensions_validate_count_them;limits_owned_by_pigment_effect_math_64_per_layer_4096_per_document;a_stack_over_64_counted_instances_or_over_folios_per_stack_ceiling_is_not_mapped_loss_effects_over_budget;the_stack_that_takes_the_counted_total_past_4096_and_every_later_stack_with_counted_instances_are_not_mapped_loss_effects_over_document_budget;kept_instances_no_plan_counts_hidden_master_off_or_switched_off_are_bounded_by_4096;the_stack_with_no_counted_instances_that_takes_them_past_4096_and_every_later_such_stack_are_not_mapped_loss_effects_unrendered_over_document_budget;a_stack_with_counted_instances_is_never_refused_for_that_bound_past_it_it_is_mapped_with_its_counted_instances_only_loss_transformed_effects_unrendered_dropped_over_document_budget;each_bound_cuts_only_its_own_category;an_effect_count_never_fails_the_import;record_validation_nodes_are_max_of_caller_nodes_and_records_plus_kept_instances_at_most_max_layers_plus_8192;the_importer_counts_by_visibility_alone_so_it_over_counts_a_visible_layer_with_no_pixels_and_a_visible_clipped_layer_whose_base_is_hidden_or_empty_which_no_plan_counts_a_counted_stack_can_be_lost_although_the_plan_count_fits_both_caps_apply_the_document_cap_past_4096_apparent_counted_instances_and_the_per_layer_cap_past_64_on_one_such_layer_test_the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases;tests_effects_budget\",",
    "\"display_conversion\":\"projection_only_original_channels_byte_preserved;wide_prism_path_for_rgb_gray_cmyk_lab;intent_perceptual_assumed_unless_configured;bpc_on_assumed_unless_configured_and_sent_to_prism_for_every_source_kind;legacy_rgb_converter_records_intent_and_bpc_not_applied;unclamped_output_clamped_with_counts_and_loss_entry;cmyk_without_embedded_profile_is_typed_loss_cmyk_profile_missing_no_default_profile\",",
    "\"blend\":\"psd_key_table_to_pigment_StudioBlendMode;unknown_key_typed_loss_never_normal;pass_is_group_flag\",",
    "\"unknown_bytes\":\"preserved_byte_exact\",",
    "\"oracle\":\"merged_image_only_when_hasRealMergedData\",",
    "\"pending\":[\"photoshop_authored_oracle_fixtures\",\"photoshop_clip_to_adjustment_layer_behaviour_unverified\",",
    "\"effect_gradient_pattern_contents_and_bevel_texture\",\"adjustment_record_export_after_edit\",\"cmyk_inverted_storage_and_lab_16_bit_encoding_verification\",\"rgb_lut_profiles_via_wide_path_untested\",",
    "\"vector_mask_and_layer_effect_rendering\",\"text_smart_object_editing\",\"mask_density_feather\",",
    "\"sixteen_bit_scale_and_float32_transfer_verification\"]}"
);
