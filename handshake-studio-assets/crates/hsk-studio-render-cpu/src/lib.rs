//! CPU render backend/reference; opt-in professional reference cases require external independent
//! oracles.
//!
//! Implements the pigment-owned `RenderBackend` port (`hsk_studio_pigment::render_plan`) for
//! `Renderer`. The plan IR, `StudioBlendMode`, the receipt and the ONLY blend arithmetic
//! (`hsk_studio_pigment::blend_math`) live in pigment (CX-CAP-001); this crate adds bounded
//! chunked execution, groups, masks, vector coverage, budgets and cancellation.
#![forbid(unsafe_code)]
mod coverage;
pub mod plan_sink;
pub mod report;
pub mod vector;

pub use plan_sink::{
    DEFAULT_CHUNK_SIDE, MAX_ANCHORS, MAX_CHUNK_SIDE, RENDERER_NAME, RENDERER_VERSION, RenderError,
    Renderer, SourceFault, blend_supported,
};
pub use report::{outcome_of, report};
pub use vector::{VectorFill, VectorSource};

pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-RENDER_CPU","version":1,"renderer":"hsk-studio-render-cpu/0.1","operation":"execute_render_plan_to_tile_sink","api":"impl RenderBackend<V: VectorSource> for Renderer (pigment render_plan port); Renderer::execute_detailed keeps the internal error; report(result, correlation, revision, cancel, observe, sink) emits one terminal Observe frame","command":"render-cpu-consumer --descriptor | --selftest","plan":"hsk_studio_pigment::render_plan::RenderPlan (tile, solid, vector, group{isolated, layers bottom-first}, layer{mode, opacity, fill_opacity, mask}); admit() runs first","blend":{"owner":"hsk_studio_pigment::blend_math (single owner); render-cpu keeps no blend arithmetic","numerics":"binary32 per pigment; inputs outside [0,1] are refused, never clamped in","supported":"every layer-applicable mode with a recovered function; refused: pass_through as a blend, behind/clear (tool-only), average/negation/reflect/glow/erase (no function)","parity":"exactness classes come from pigment facts; Adobe output parity NOT_PROVEN"},"operators":{"exact":["tile","solid","group_isolated","group_pass_through","mask coverage_u8/coverage_u16le"],"approximate":["vector (supersampled coverage)","fill_opacity (folded into opacity as a product)"],"unsupported":["group_pass_through_opacity","group_non_isolated_blended","mask density other than 1","mask on a pass-through layer","premultiplied alpha","samples outside [0,1]"]},"formats":{"colour":"rgb_f32le","alpha":"f32le straight, separate plane","chunk":"tightly packed row-major, sink receives rect + both planes"},"limits":{"anchors_per_path":65536,"chunk_side_max":4096,"default_chunk_side":64,"scratch_bytes_per_chunk_pixel":32,"scratch_bytes_per_isolated_group_depth_pixel":16,"aa_samples_per_axis":"1..16","case_seconds":30},"vector":"nib anchors (handles relative to anchor), winding NonZero|EvenOdd, coverage = exact winding of the true curve at n*n pixel-centre samples","colour_management":"none: plan colour tag (profile hash + blend space) must equal every source tag, never converted (STU-COL-163)","failure":"cancel/deadline between chunks; stale revision, invalid plan, unsupported operator, unresolved or mismatching source (hash, stride, declared size/format/tag, mask gap) refuse before the first sink call; sink error aborts; scratch released on every exit; receipt only after the last chunk","privacy":"no pixel, profile or path bytes in telemetry","pending":["blend_math owns remaining modes; render-cpu follows its exactness facts","strokes, gradients, patterns, layer effects, clipping, knockout, blend-if","16/32-bit integer paths, SIMD, parallel chunks","external-oracle parity vs Adobe: NOT_PROVEN"]}"#;
