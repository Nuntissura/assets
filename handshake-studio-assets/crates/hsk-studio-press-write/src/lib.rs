//! The write half of STUDIO-MODULE-PRESS (SMB-CRATE-SEAM-SPLITS, slice 1): Folio/vector -> PDF
//! export, the PDF/X-4 check and the STU-LAY-160 PDF/X standards enumeration, font subsetting (CFF, sfnt), export verdicts, the
//! ink manager contract (STU-LAY-160), the separations preview with its typed readouts and the total ink coverage contract (STU-LAY-250..254, `coverage`), built on the read half's PDF syntax, object writer, painter and vector reading. `hsk-studio-press` is the public face.
#![forbid(unsafe_code)]

// The read half's modules under the names `export` has always used (`crate::syntax`, ...).
#[allow(unused_imports)]
use hsk_studio_press_read::{canon, cmap, content, error, filters, flatten, paint, pdf_edit, reader, syntax, writer};
// The read half's vector-page types under the crate-root names `separations` has always used.
#[allow(unused_imports)]
use hsk_studio_press_read::{VectorColour, VectorEl, VectorItem, VectorMode, VectorPage, VectorPaint, VectorSpace, read_vector_document};

mod coverage;
mod export;
mod inks;
mod separations;

pub use coverage::{
    CoverageError, CoverageInputStage, CoverageMetric, CoverageMetrics, CoveragePage, CoverageParameter, CoverageParameterSpec, CoverageRequest,
    CoverageUnit, DocRegion, FindingPageRef, IndeterminateCause, InkCoverageFinding, InkCoverageReport, InkLimitCheckState, InkLimitRule,
    InkLimitScope, InkLimitSites, InkLimitSource, LimitEvaluation, MAX_COVERAGE_OBJECT_REFS, MAX_COVERAGE_PAGES, MAX_COVERAGE_SAMPLES,
    MAX_INDETERMINATE_CAUSES, MAX_OBJECT_REF_BYTES, MAX_PAGE_REF_BYTES, MAX_RULE_ID_BYTES, ObjectRef, PUBLISHED_INK_LIMIT_REFERENCE, PageReference,
    ParamPrecision, ParamValue, PublishedInkLimit, ResolvedInkLimits, ResolvedLimit, evaluate_ink_coverage,
    Emission, ObserveRoute, evaluate_ink_coverage_observed,
};

pub use export::{
    BlendMode, Colour, DrawState, Gradient, GradientGeometry, GradientSpread, GradientStop, GroupStyle, MaskImage, MAX_GRADIENT_CYCLES, TextClipRun, PDFX4_VERSION, PDFA2B_CONFORMANCE, PDFA2B_PART, CONFORMANCE_PDFA2B_VERAPDF, VERAPDF_PDFA2B_PROFILE, MAX_REPORT_BYTES, PdfX4, PdfXReport, PdfXRule, PdfXStandard, SoftMask, check_pdfx4, EXPORT_CONFORMANCE, ExportError, ExportOptions, ExportReceipt, FillRule, FontHandle, IccHandle, ImageColour, ImageSpec, LineCap,
    LineJoin, MAX_EXPORT_COORD, MAX_EXPORT_TEXT, OutputIntent, OutputIntentSubtype, PageHandle, PageSpec, PathCommand, PdfExport, RenderedPage, StrokeStyle,
    font_advance_widths, normalize_verapdf_report, render_page, MAX_OUTLINE_DEPTH, MAX_OUTLINE_ENTRIES, OutlineEntry, PageLabelRange,
    PageLabelStyle,
};
pub use inks::{
    Ink, InkEdit, InkEditOutcome, InkEditReceipt, InkError, InkManager, InkParameter, InkParameterSpec, InkTarget, InkType, InkUnit, MAX_INKS,
    MAX_MIXED_GROUP_SWATCHES, MAX_MIXED_INKS, MAX_MIXED_SPOT_INKS, MixedInk, MixedInkGroup, MixedInkGroupRow, ProcessInk,
};
pub use separations::{
    InkLimitFinding, InkLimitReport, InkLimitViewReadout, InkLimits, InkReading, LimitKind, MAX_SEP_CLIP_DEPTH, MAX_SEP_PLATES, Plate,
    PlateCoverage, PlateReadout, PlateToggle, PlateView, ProcessConverter, SepDraw, SepError, SepLimits, SepNote, SepPaint, SepViewError,
    SeparationPreview, SeparationsReadout, SeparationsViewState, Separator, Severity as InkLimitSeverity, separate_vector_page,
};
