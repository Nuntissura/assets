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
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_VECTOR","version":1,"operation":"import_svg(bytes, ImportPolicy, IdMint, CancellationToken) -> VectorProposal {Folio snapshot, VectorPath anchors, Folio design records (paint styles and node style_refs), gradient provenance, LossEntry list, Provenance}; export_svg(paths, design, opts) and export_proposal (groups, names, visibility, opacity; *_with_loss variants report export losses) read paint, stroke and appearance rows back from the Folio design records -> deterministic SVG text; gzip_svg(text) -> SVGZ; import_svg also reads SVGZ under a decompression cap; identify_ai(bytes) -> AiKind; import_ai -> NativeEditabilityBlocked with the original bytes preserved; PdfVectorProvider port with NoPdfProvider","operator":{"intent":"Bring SVG vector artwork into a Studio document as editable paths without silent loss","result":"A proposal listing every path as anchors with relative handles in points and every unsupported or approximated feature as a typed loss; nothing is committed","recovery":"entity_denied: remove the DOCTYPE internal subset; budget_exceeded: split the file or raise the named limit; invalid_geometry: coordinates must be finite and within 1e9; native_editability_blocked: keep the original .ai file, editable import needs a native decoder; pdf_unavailable: no PDF provider is selected; canceled: nothing was produced"},"model":{"units":"SVG user units are CSS px; px to pt factor is 72 / pixels_per_inch (0.75 at 96) recorded once in provenance; viewBox and preserveAspectRatio are applied; coordinates and handles in the proposal are points","paths":"one VectorPath per subpath (compound SVG paths share layer_id); handles are vectors relative to their anchor; winding NONZERO or EVENODD, NONE when the fill is none; a closing curve whose end equals the first anchor is merged into it","supported":"svg viewBox/width/height/preserveAspectRatio, g, a, path (M L H V C S Q T A Z, arcs become cubics), rect (rx ry), circle, ellipse, line, polyline, polygon, transform (baked), fill, stroke (solid or linearGradient/radialGradient with gradientUnits, gradientTransform, spreadMethod, href inheritance and stop-opacity), stroke-width, stroke-linecap, stroke-linejoin, stroke-miterlimit, stroke-dasharray, stroke-dashoffset, paint-order (stroke row below or above the fill row), fill-rule, fill-opacity, stroke-opacity, opacity, color, display, visibility as attributes or inline style; SVGZ (one gzip member)","loss":"approximated (arc_to_cubic, ellipse_cubics, rounded_rect_cubics, viewport_defaulted, slice_viewport_not_clipped, stroke_width_nonuniform_scale), transform_baked, unsupported (use, symbol, image, text, text on path, patterns, filters, masks, clip paths, markers, css stylesheets, scripts, animation, blend modes, markers, percent dash lengths, unresolved paint servers, degenerate gradients), external_denied (href not #local or data:, url() paints, DOCTYPE external id), ignored (metadata, foreign namespaces, degenerate subpaths)","ids":"this crate mints no ids: IdMint is caller supplied; SequentialIdMint is deterministic and uuid_v7-shaped","design":"paint, stroke and appearance data is Folio's: VectorProposal.design is a StudioDesignRecords set validated by folio::validate_design_records; each distinct appearance stack is a PAINT StudioPaintStyle (rows bottom to top: fill or stroke row = solid or gradient paint, row opacity percent, blend normal, visible; stroke = weight, cap, join, miter limit, dash, dash offset in pt, align CENTER; gradient = linear or radial geometry in gradient space, one gradient-to-point transform with the bounding-box mapping folded in, spread, stops in percent with opacity) and each vector layer names its style through StudioNodeDesign.style_refs.fill_style_id; colours are opaque 0..1 RGB with the document working space (null profile), alpha lives in row or stop opacity; SVG gradientUnits is kept as provenance only (gradient_provenance, gradient_units); export validates each referenced style with folio::validate_paint_style; stacks longer than one fill and one stroke export as duplicated path elements and are reported; paints Folio does not store (pattern, image, CMYK, spot) stay typed losses; Folio layer opacity carries element opacity; no style registry is created"},"limits":{"max_bytes":8388608,"max_nodes":20000,"max_depth":32,"max_segments":500000,"max_decompressed_bytes":33554432,"design_records_per_node":16,"coordinate":1000000000,"case_seconds":30},"safety":"SVGZ is inflated under max_decompressed_bytes (a bomb costs at most the cap) with CRC-32 and size verified and is never unwrapped twice; DOCTYPE with an internal subset is refused, external DTD ids are never resolved, no href or url() is ever followed, scripts and animation are never executed; no network, file or process access exists in this crate","cancel":"CancellationToken is checked at entry and per element and every 256 path segments; a canceled call returns canceled and no partial proposal","undo":"imports are proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"VectorProposal paths, design records and losses, Folio snapshot","action":"the same import and export functions with the same cancellation token","capture":"caller captures the source bytes, proposal and Observe frame under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, element text or paths are serialized by this crate","native_gap":"AI files are detected by header only; the private-data marker names are UNVERIFIED candidates; import_ai always returns NativeEditabilityBlocked (STU-IO-186); EPS and PostScript are not decoded; PDF decoding is a Press provider","next":"patterns, clip paths and masks, text, brushes, artboards"}"#;
