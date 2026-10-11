//! SVG/AI vector import/export with fallible loss-aware proposals; PDF decoding/editing exposed
//! through Press provider ports, not unconditional all-PDF engine.
#![forbid(unsafe_code)]

mod ai_adapter;
mod error;
mod ids;
mod model;
mod pdf_port;
mod svg_adapter;
mod svg_export;

pub use ai_adapter::{AiKind, identify_ai, import_ai};
pub use error::{PreservedBlob, Result, VectorError};
pub use ids::{IdMint, SequentialIdMint};
pub use model::*;
pub use pdf_port::{NoPdfProvider, PdfVectorProvider};
pub use svg_adapter::import_svg;
pub use svg_export::{export_proposal, export_svg};

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
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_VECTOR","version":1,"operation":"import_svg(bytes, ImportPolicy, IdMint, CancellationToken) -> VectorProposal {Folio snapshot, VectorPath anchors, StyleProposal, LossEntry list, Provenance}; export_svg(paths, styles, ExportOptions) -> deterministic SVG text; identify_ai(bytes) -> AiKind; import_ai -> NativeEditabilityBlocked with the original bytes preserved; PdfVectorProvider port with NoPdfProvider","operator":{"intent":"Bring SVG vector artwork into a Studio document as editable paths without silent loss","result":"A proposal listing every path as anchors with relative handles in points and every unsupported or approximated feature as a typed loss; nothing is committed","recovery":"entity_denied: remove the DOCTYPE internal subset; budget_exceeded: split the file or raise the named limit; invalid_geometry: coordinates must be finite and within 1e9; native_editability_blocked: keep the original .ai file, editable import needs a native decoder; pdf_unavailable: no PDF provider is selected; canceled: nothing was produced"},"model":{"units":"SVG user units are CSS px; px to pt factor is 72 / pixels_per_inch (0.75 at 96) recorded once in provenance; viewBox and preserveAspectRatio are applied; coordinates and handles in the proposal are points","paths":"one VectorPath per subpath (compound SVG paths share layer_id); handles are vectors relative to their anchor; winding NONZERO or EVENODD, NONE when the fill is none; a closing curve whose end equals the first anchor is merged into it","supported":"svg viewBox/width/height/preserveAspectRatio, g, a, path (M L H V C S Q T A Z, arcs become cubics), rect (rx ry), circle, ellipse, line, polyline, polygon, transform (baked), fill, stroke, stroke-width, fill-rule, fill-opacity, stroke-opacity, opacity, color, display, visibility as attributes or inline style","loss":"approximated (arc_to_cubic, ellipse_cubics, rounded_rect_cubics, viewport_defaulted, slice_viewport_not_clipped, stroke_width_nonuniform_scale), transform_baked, unsupported (defs, use, symbol, image, text, text on path, gradients, patterns, filters, masks, clip paths, markers, css stylesheets, scripts, animation, dash and cap and join, blend modes), external_denied (href not #local or data:, url() paints, DOCTYPE external id), ignored (metadata, foreign namespaces, degenerate subpaths)","ids":"this crate mints no ids: IdMint is caller supplied; SequentialIdMint is deterministic and uuid_v7-shaped","styles":"fill and stroke colour with combined opacity and stroke width in points are proposed as plain data; Folio layer opacity carries element opacity; no style primitive is created"},"limits":{"max_bytes":8388608,"max_nodes":20000,"max_depth":32,"max_segments":500000,"coordinate":1000000000,"case_seconds":30},"safety":"DOCTYPE with an internal subset is refused, external DTD ids are never resolved, no href or url() is ever followed, scripts and animation are never executed; no network, file or process access exists in this crate","cancel":"CancellationToken is checked at entry and per element and every 256 path segments; a canceled call returns canceled and no partial proposal","undo":"imports are proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"VectorProposal paths, styles and losses, Folio snapshot","action":"the same import and export functions with the same cancellation token","capture":"caller captures the source bytes, proposal and Observe frame under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, element text or paths are serialized by this crate","native_gap":"AI files are detected by header only; the private-data marker names are UNVERIFIED candidates; import_ai always returns NativeEditabilityBlocked (STU-IO-186); EPS and PostScript are not decoded; PDF decoding is a Press provider","next":"gradients and appearance stacks, SVGZ, text, brushes and artboards"}"#;
