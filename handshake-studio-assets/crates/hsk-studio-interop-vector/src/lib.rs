//! SVG/AI vector import/export with fallible loss-aware proposals; PDF decoding/editing exposed
//! through Press provider ports, not unconditional all-PDF engine.
#![forbid(unsafe_code)]

mod ai_adapter;
mod error;
mod gzip;
mod ids;
mod model;
mod pdf_port;
mod svg_adapter;
mod svg_export;

pub use ai_adapter::{AiKind, identify_ai, import_ai};
pub use error::{PreservedBlob, Result, VectorError};
pub use gzip::gzip_svg;
pub use ids::{IdMint, SequentialIdMint};
pub use model::*;
pub use pdf_port::{NoPdfProvider, PdfVectorProvider};
pub use svg_adapter::import_svg;
pub use svg_export::{
    SvgExport, export_proposal, export_proposal_with_loss, export_svg, export_svg_with_loss,
};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps an import/export result to exactly one Observe terminal outcome (no ids, no element text).
/// A proposal that carries losses is still `Success`; the loss list is the detail. Blocked native
/// editability and a missing PDF provider are `Failure(Unavailable)`, unsupported formats
/// `Failure(Unsupported)`, budget/invalid input `Failure(Validation)`, cancellation `Canceled`.
/// `correlation_id` and `revision` must equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

/// Same descriptor for models, human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_VECTOR","version":1,"operation":"import_svg(bytes, ImportPolicy, IdMint, CancellationToken) -> VectorProposal {Folio snapshot, VectorPath anchors, Folio design records (paint styles and node style_refs), gradient provenance, LossEntry list, Provenance}; export_svg(paths, design, opts) and export_proposal (groups, names, visibility, opacity; *_with_loss variants report export losses) read paint, stroke and appearance rows back from the Folio design records -> deterministic SVG text; gzip_svg(text) -> SVGZ; import_svg also reads SVGZ under a decompression cap; identify_ai(bytes) -> AiKind; import_ai -> NativeEditabilityBlocked with the original bytes preserved; PdfVectorProvider port with NoPdfProvider","operator":{"intent":"Bring SVG vector artwork into a Studio document as editable paths without silent loss","result":"A proposal listing every path as anchors with relative handles in points and every unsupported or approximated feature as a typed loss; nothing is committed","recovery":"entity_denied: remove the DOCTYPE internal subset; budget_exceeded: split the file or raise the named limit; invalid_geometry: coordinates must be finite and within 1e9; native_editability_blocked: keep the original .ai file, editable import needs a native decoder; pdf_unavailable: no PDF provider is selected; canceled: nothing was produced"},"model":{"units":"SVG user units are CSS px; px to pt factor is 72 / pixels_per_inch (0.75 at 96) recorded once in provenance; viewBox and preserveAspectRatio are applied; coordinates and handles in the proposal are points","paths":"one VectorPath per subpath (compound SVG paths share layer_id); handles are vectors relative to their anchor; winding NONZERO or EVENODD, NONE when the fill is none; a closing curve whose end equals the first anchor is merged into it","supported":"svg viewBox/width/height/preserveAspectRatio, g, a, path (M L H V C S Q T A Z, arcs become cubics), rect (rx ry), circle, ellipse, line, polyline, polygon, transform (baked), fill, stroke (solid or linearGradient/radialGradient with gradientUnits, gradientTransform, spreadMethod, href inheritance and stop-opacity), stroke-width, stroke-linecap, stroke-linejoin, stroke-miterlimit, stroke-dasharray, stroke-dashoffset, paint-order (stroke row below or above the fill row), fill-rule, fill-opacity, stroke-opacity, opacity, color, display, visibility as attributes or inline style (the style text is tokenised as CSS: comments, strings, escapes, case and nesting); pattern paint servers, clip-path and mask references resolved per SVG 1.1, SVG 2 and CSS Masking 1 to an exact result or a typed loss (masking); SVGZ (one gzip member)","loss":"approximated (arc_to_cubic, ellipse_cubics, rounded_rect_cubics, viewport_defaulted, slice_viewport_not_clipped, stroke_width_nonuniform_scale), transform_baked, unsupported (use outside a clipPath, symbol, image, text, text on path, filters, markers, css stylesheets, scripts, animation, blend modes, percent dash lengths, unresolved paint servers, degenerate gradients, pattern_paint_not_representable, clip_path_not_representable, clip_rule_evenodd, mask_not_representable with mask_type_alpha or mask_type_luminance, clip_path_reference_cycle, clip_path_depth_exceeded, pattern_href_cycle, pattern_href_chain_too_long, gradient_href_chain_too_long), external_denied (href not #local or data:, url() paints, DOCTYPE external id), ignored (metadata, foreign namespaces, degenerate subpaths, pattern_definition_not_stored, clip_path_definition_not_stored, mask_definition_not_stored, clip_path_noop_dropped, mask_noop_dropped, clip_path_reference_invalid, pattern_zero_size_no_paint, pattern_empty_no_paint, pattern_transform_singular_no_paint); approximated adds clip_path_empty_hides_element, mask_empty_hides_element, mask_reference_invalid_hidden (the layer is hidden) and pattern_fallback_color_used; export adds colour_profile_not_converted (colour with an assigned profile is written as plain sRGB; the export holds no profile bytes) with the Folio resolution issue colour_profile_unresolved, plus quantization and dropped row, blend, gradient members, working_space_not_converted (an assigned RGB working space), and the Folio layer appearance SVG cannot hold: layer_mask_not_exported, layer_clipped_to_base_not_exported, layer_blend_mode_not_exported, layer_fill_opacity_not_exported, group_pass_through_isolated","masking":"Folio has no pattern paint kind, no vector clip and no vector mask (its StudioMask is raster coverage tiles; spec STU-VEC-017, STU-VEC-053, STU-VEC-138), so a pattern, clipPath or mask definition is never carried over and each use is resolved to one result: a clip that contains everything the element paints (stroke reach included, rectangles only, clipPathUnits, transform, use of a rect, nested clip-path with loop and depth guards) or a mask that is the identity over it (maskUnits region, maskContentUnits, topmost opaque rectangle, white for luminance and any colour for alpha) is dropped; an empty clipPath, an empty or zero-region mask or a dangling mask reference hides the layer; an invalid clip-path reference clips nothing; a pattern with a zero or missing tile, a singular patternTransform or no content paints nothing; otherwise the element stays as drawn with a typed loss and a pattern stands in with its fallback colour when one is given; pattern href follows href over xlink:href with a visited set and depth 8; proofs: clip_path_exact_or_typed_loss, mask_exact_or_typed_loss, pattern_paint_exact_or_typed_loss, hostile_references_are_bounded, css_values_are_tokenised_not_substring_matched","ids":"this crate mints no ids: IdMint is caller supplied; SequentialIdMint is deterministic and uuid_v7-shaped","design":"paint, stroke and appearance data is Folio's: VectorProposal.design is a StudioDesignRecords set validated by folio::validate_design_records; each distinct appearance stack is a PAINT StudioPaintStyle (rows bottom to top: fill or stroke row = solid or gradient paint, row opacity percent, blend normal, visible; stroke = weight, cap, join, miter limit, dash, dash offset in pt, align CENTER; gradient = linear or radial geometry in gradient space, one gradient-to-point transform with the bounding-box mapping folded in, spread, stops in percent with opacity) and each vector layer names its style through StudioNodeDesign.style_refs.fill_style_id; colours are opaque 0..1 RGB with the document working space (null profile), alpha lives in row or stop opacity; SVG gradientUnits is kept as provenance only (gradient_provenance, gradient_units); export validates each referenced style with folio::validate_paint_style; stacks longer than one fill and one stroke export as duplicated path elements and are reported; paints Folio does not store (pattern, image, CMYK, spot) stay typed losses; Folio layer opacity carries element opacity; no style registry is created"},"limits":{"max_bytes":8388608,"max_nodes":20000,"max_depth":32,"max_segments":500000,"max_decompressed_bytes":33554432,"design_records_per_node":16,"reference_depth":8,"clip_children":64,"reference_work_per_segment_and_node":32,"coordinate":1000000000,"case_seconds":30},"safety":"SVGZ is inflated under max_decompressed_bytes (a bomb costs at most the cap) with CRC-32 and size verified and is never unwrapped twice; DTD processing is never enabled: the DOCTYPE is classified in the XML prolog only (a DOCTYPE mention in a comment, processing instruction or CDATA is not one), one with an internal subset is refused, a legacy one with an external id is blanked before parsing, external DTD ids are never resolved, no href or url() is ever followed, scripts and animation are never executed; no network, file or process access exists in this crate","cancel":"CancellationToken is checked at entry and per element and every 256 path segments; a canceled call returns canceled and no partial proposal","undo":"imports are proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"VectorProposal paths, design records and losses, Folio snapshot","action":"the same import and export functions with the same cancellation token","capture":"caller captures the source bytes, proposal and Observe frame under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, element text or paths are serialized by this crate","native_gap":"AI files are detected by header only; the private-data marker names are UNVERIFIED candidates; import_ai always returns NativeEditabilityBlocked (STU-IO-186); EPS and PostScript are not decoded; PDF decoding is a Press provider","next":"text, brushes, artboards; pattern paint, vector clip and vector mask records in Folio so that patterns, clips and masks can be carried instead of reported"}"#;
