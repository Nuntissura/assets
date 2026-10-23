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
mod image_source;
mod layers;
mod limits;
mod loss;
mod pixels;
mod patterns;
mod placed;
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
pub use image_source::{ImageSourceCanvas, image_source_document};
pub use layers::{BlendRanges, GroupRole, LayerMask, PsdChannel, PsdLayer, Rect, TaggedBlock};
pub use limits::Limits;
pub use loss::{LossEntry, LossKind, LossReport, key_text};
pub use patterns::{MAX_PATTERNS, MAX_PATTERN_SIDE};
pub use pixels::Plane;
pub use proposal::{
    DisplayConversion, GAMUT_CLAMP_TOLERANCE, IdMint, ImportProposal, LayerExtras, LayerMapEntry, MaskTileBlob,
    PatternTileBlob, ProposalConfig, Provenance, RasterSource, SourceIdentity, TILE_EDGE, TileBlob, raster_proposal, to_proposal,
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
    "\"api\":\"read_psd / image_source_document (TIFF tag 37724 layer records, 8BIM/8B64 and byte-swapped MIB8/46B8, 8-bit little-endian, import-only) / write_psd / to_proposal / raster_proposal / export_psd / apply_edits / report; PsdDocument::{tree,loss_report,decode_merged,merged_oracle_rgba8}\",",
    "\"capabilities\":[\"layer_records_groups_resources\",\"byte_exact_rewrite\",\"typed_loss_report\",",
    "\"folio_proposal_hierarchy_appearance_masks\",\"layerless_merged_image_as_one_raster_layer\",\"raster_source_proposal_shared_tile_mapping\",\"pigment_tiles_8_16_32_bit_full_precision\",\"embedded_icc_rgb_via_converter_when_accepted\",",
    "\"observe_terminal_frame\",\"export_edited_folio_layer_properties_preserving_original_bytes\",",
    "\"cmyk_embedded_icc_display_conversion_8_16_bit\",\"lab_pcs_native_display_conversion_8_16_bit\",\"gray_ktrc_icc_display_conversion_8_16_bit\",",
    "\"raster_records_adjustments_levels_curves_hue_saturation_brightness_contrast_exposure_invert_threshold_posterize\",",
    "\"raster_records_blend_if_mask_density_feather_knockout_blend_toggles\",\"raster_records_lfx2_effect_stack_not_rendered\",",
    "\"adjustment_layers_as_graph_sources_with_input_ordinal\",\"layer_clipped_to_adjustment_unclipped_with_typed_loss\",",
    "\"converter_absolute_bpc_engine_report_in_display_receipt\",\"absolute_media_white_divergence_as_transformed_loss_entry\",",
    "\"blend_if_on_clip_base_or_clipped_layer_unsupported_loss_entry\",\"blend_clipped_layers_as_group_off_unsupported_loss_entry\",",
    "\"effects_with_blend_if_on_one_layer_unsupported_loss_entry\",\"mask_on_pass_through_group_unsupported_loss_entry\",",
    "\"effects_over_layer_limit_unsupported_loss_entry_effects_over_budget\",\"patt_pat2_pat3_patterns_rgb_gray_indexed_8_16_bit_to_pattern_overlay_pattern_tile_when_idmint_pattern_id_mints\",\"effects_over_document_cap_unsupported_loss_entries_import_continues\",\"effects_over_folio_stack_ceiling_drops_switched_off_instances_transformed_loss_entry_effects_unrendered_dropped_over_stack_ceiling\"],",
    "\"raster_records\":\"ImportProposal::raster_records (folio StudioRasterRecords) validated by validate_raster_records; records only, nothing applied or rendered; unreadable or out-of-range record keeps the layer a typed unsupported layer; IdMint::effect_stack_id defaults to None (effects preserved and reported)\",",
    "\"effect_limits\":\"counted_instances_are_the_enabled_instances_of_enabled_stacks_on_visible_layers_in_visible_groups_as_composite_convert_effects_and_pigment_plan_extensions_validate_count_them;limits_owned_by_pigment_effect_math_64_per_layer_4096_per_document;a_stack_over_64_counted_instances_or_with_no_counted_instances_and_over_folios_per_stack_ceiling_of_1024_is_not_mapped_loss_effects_over_budget;the_stack_that_takes_the_counted_total_past_4096_and_every_later_stack_with_counted_instances_are_not_mapped_loss_effects_over_document_budget;kept_instances_no_plan_counts_hidden_master_off_or_switched_off_are_bounded_by_4096;the_stack_with_no_counted_instances_that_takes_them_past_4096_and_every_later_such_stack_are_not_mapped_loss_effects_unrendered_over_document_budget;a_stack_with_counted_instances_is_never_refused_for_that_bound_past_it_it_is_mapped_with_its_counted_instances_only_loss_transformed_effects_unrendered_dropped_over_document_budget;folios_per_stack_ceiling_of_1024_holds_counted_and_switched_off_instances_together_and_cuts_only_the_switched_off_ones_a_stack_with_counted_instances_keeps_all_of_them_and_drops_its_switched_off_ones_loss_transformed_effects_unrendered_dropped_over_stack_ceiling_test_folios_per_stack_ceiling_cuts_only_the_switched_off_instances_of_a_stack_with_counted_instances;the_two_transformed_dropped_losses_effects_unrendered_dropped_over_document_budget_and_effects_unrendered_dropped_over_stack_ceiling_are_one_entry_per_stack_with_the_path_ending_layer_id_dropped_naming_the_count_of_the_mapped_switched_off_instances_the_record_does_not_hold_and_a_switched_off_instance_that_was_mapped_and_left_out_of_the_record_adds_no_loss_entry_of_its_own_tests_the_ceiling_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them_and_the_document_budget_loss_names_the_count_of_the_instances_it_dropped_and_adds_no_note_for_them;the_descriptor_readers_cap_of_512_entries_per_list_is_a_category_blind_parse_bound_checked_before_any_entry_is_read_a_list_over_512_entries_refuses_the_descriptor_loss_unsupported_descriptor_item_limit_and_that_stack_is_lost_whether_its_entries_are_counted_or_switched_off_the_lfx2_block_stays_byte_exact_test_a_list_over_the_descriptor_readers_512_entry_cap_loses_the_stack_whatever_the_categories_of_its_entries;the_descriptor_readers_cap_of_8192_items_per_descriptor_counts_every_object_item_and_every_list_entry_at_every_nesting_depth_category_blind_and_checked_as_each_item_is_read;a_descriptor_over_8192_items_refuses_the_descriptor_loss_unsupported_descriptor_item_limit_and_that_stack_is_lost_before_any_instance_is_mapped_so_every_cap_and_loss_code_stated_above_applies_to_a_stack_whose_descriptor_fits_8192_items;measured_items_per_effect_instance_with_its_list_entry_or_slot_key_and_enab_and_only_the_items_the_importer_requires_drop_shadow_3_inner_shadow_3_outer_glow_4_inner_glow_5_bevel_16_colour_overlay_3_satin_3_stroke_5;measured_items_per_effect_instance_with_every_key_the_importer_reads_and_a_2_point_contour_drop_shadow_28_inner_shadow_27_outer_glow_29_inner_glow_30_bevel_46_colour_overlay_24_satin_27_stroke_28;a_contour_point_costs_4_items_and_a_contour_holds_up_to_256_points;most_instances_one_descriptor_holds_in_lists_of_512_with_the_required_items_only_drop_shadow_2728_inner_shadow_2728_colour_overlay_2728_stroke_1637_with_every_key_drop_shadow_292_inner_shadow_303_colour_overlay_341_stroke_292_and_with_a_256_point_contour_drop_shadow_7;1025_instances_do_not_fit_8192_items_from_8_items_per_instance_a_full_list_of_512_entries_from_16_items_per_instance_and_65_counted_instances_from_127_items_per_instance;for_instances_of_at_least_those_costs_the_stack_is_refused_whole_with_descriptor_item_limit_before_it_reaches_folios_ceiling_a_full_512_entry_list_or_the_64_per_layer_limit_so_the_ceiling_cut_loss_and_effects_over_budget_for_the_per_layer_limit_are_not_reported_for_it_test_the_descriptor_readers_8192_item_cap_is_measured_per_effect_kind_and_stated_in_the_descriptor;each_bound_cuts_only_its_own_category;an_effect_count_never_fails_the_import;record_validation_nodes_are_max_of_caller_nodes_and_records_plus_kept_instances_at_most_max_layers_plus_8192;the_importer_counts_by_visibility_alone_so_it_over_counts_a_visible_layer_with_no_pixels_and_a_visible_clipped_layer_whose_base_is_hidden_or_empty_which_no_plan_counts_a_counted_stack_can_be_lost_although_the_plan_count_fits_both_caps_apply_the_document_cap_past_4096_apparent_counted_instances_and_the_per_layer_cap_past_64_on_one_such_layer_test_the_importer_counts_layers_without_pixels_and_clipped_layers_on_hidden_or_empty_bases;tests_effects_budget\",",
    "\"display_conversion\":\"projection_only_original_channels_byte_preserved;wide_prism_path_for_rgb_gray_cmyk_lab;intent_perceptual_assumed_unless_configured;bpc_on_assumed_unless_configured_and_sent_to_prism_for_every_source_kind;legacy_rgb_converter_records_intent_and_bpc_not_applied;unclamped_output_clamped_with_counts_and_loss_entry;cmyk_without_embedded_profile_is_typed_loss_cmyk_profile_missing_no_default_profile\",",
    "\"blend\":\"psd_key_table_to_pigment_StudioBlendMode;unknown_key_typed_loss_never_normal;pass_is_group_flag\",",
    "\"unknown_bytes\":\"preserved_byte_exact\",",
    "\"oracle\":\"merged_image_only_when_hasRealMergedData\",",
    "\"pending\":[\"photoshop_authored_oracle_fixtures\",\"photoshop_clip_to_adjustment_layer_behaviour_unverified\",",
    "\"effect_gradient_contents_and_bevel_texture\",\"adjustment_record_export_after_edit\",\"cmyk_inverted_storage_and_lab_16_bit_encoding_verification\",\"rgb_lut_profiles_via_wide_path_untested\",",
    "\"vector_mask_and_layer_effect_rendering\",\"text_smart_object_editing\",\"mask_density_feather\",",
    "\"sixteen_bit_scale_and_float32_transfer_verification\"]}"
);
