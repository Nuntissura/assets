//! Typed failures of vector interchange. A failure never carries project text, paths or ids.
use hsk_studio_folio::Code as FolioCode;
use hsk_studio_observe::FailureCode;
use std::fmt;

/// Original bytes of a file Studio cannot edit natively, kept byte-exact so nothing is lost
/// (STU-IO-005 preservation blob; the host adds the content hash and receipt when it adopts it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreservedBlob {
    /// Short format label, for example `ai_pdf_compatible`.
    pub kind: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VectorError {
    /// The caller's cancellation token was set; no partial proposal exists.
    Canceled,
    /// A declared limit was hit before or while decoding (`what` names the limit).
    BudgetExceeded(&'static str),
    /// A coordinate is not finite or exceeds 1e9, or a path cannot be exported.
    InvalidGeometry,
    /// The bytes are not UTF-8 text or the root is not an SVG-namespace `svg` element.
    NotSvg,
    /// The SVGZ container is damaged or unsupported (`what` names the check).
    Gzip(&'static str),
    /// The XML is malformed (including undefined entity references).
    MalformedXml,
    /// A DOCTYPE with an internal subset (entity declarations) was refused; hostile by default.
    EntityDenied,
    /// An import or export option is not usable (`what` names it).
    InvalidPolicy(&'static str),
    /// Native Illustrator editability needs a decoder that does not exist (STU-IO-186). The
    /// original bytes are preserved in `preserved`; a PDF preview never proves native editability.
    NativeEditabilityBlocked {
        version: Option<String>,
        reason: &'static str,
        preserved: PreservedBlob,
    },
    /// No PDF provider is selected (PDF decoding is a Press provider, not an engine in this crate).
    PdfUnavailable,
    /// The Folio document built from the import was refused.
    Folio(FolioCode),
    /// The input uses a format feature this adapter does not implement at all.
    Unsupported(&'static str),
    /// A DWG drawing: no permissively licensed DWG reader exists, so Studio does not decode DWG.
    /// `route` names the supported path (export DXF from the CAD application, then import it);
    /// `version` is the six-byte DWG version string (for example `AC1032`) when it is printable.
    /// The original bytes are preserved in `preserved`.
    DwgUnavailable {
        version: Option<String>,
        route: &'static str,
        preserved: PreservedBlob,
    },
    /// The DXF text or binary stream is damaged (`what` names the check).
    MalformedDxf(&'static str),
    /// The EPS file is damaged: DOS binary header, DSC comments or preview (`what` names the check).
    MalformedEps(&'static str),
    /// EPS vector content needs the isolated PostScript worker, which this call did not use or
    /// this build does not have; `route` names how to obtain the vector content. The original bytes
    /// are preserved. A raster preview, when the file has one, is available from `import_eps_preview`.
    EpsVectorNotDecoded {
        route: &'static str,
        preserved: PreservedBlob,
    },
    /// No EPS-to-PDF converter is available (the worker program is missing or could not start).
    EpsConverterUnavailable,
    /// The isolated EPS-to-PDF conversion failed (`what`: deadline, memory_cap, crashed,
    /// postscript_error, no_pages, output_over_cap, protocol, canceled_by_worker).
    EpsConversionFailed(&'static str),
}

impl VectorError {
    /// Stable snake_case code, free of input-derived text.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::InvalidGeometry => "invalid_geometry",
            Self::NotSvg => "not_svg",
            Self::MalformedXml => "malformed_xml",
            Self::Gzip(_) => "gzip_invalid",
            Self::EntityDenied => "entity_denied",
            Self::InvalidPolicy(_) => "invalid_policy",
            Self::NativeEditabilityBlocked { .. } => "native_editability_blocked",
            Self::PdfUnavailable => "pdf_unavailable",
            Self::Folio(_) => "folio_refused",
            Self::Unsupported(_) => "unsupported",
            Self::DwgUnavailable { .. } => "dwg_unavailable",
            Self::MalformedDxf(_) => "malformed_dxf",
            Self::MalformedEps(_) => "malformed_eps",
            Self::EpsVectorNotDecoded { .. } => "eps_vector_not_decoded",
            Self::EpsConverterUnavailable => "eps_converter_unavailable",
            Self::EpsConversionFailed(_) => "eps_conversion_failed",
        }
    }

    /// Observe mapping: `None` is the `Canceled` outcome.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::NativeEditabilityBlocked { .. }
            | Self::PdfUnavailable
            | Self::DwgUnavailable { .. }
            | Self::EpsVectorNotDecoded { .. }
            | Self::EpsConverterUnavailable => Some(FailureCode::Unavailable),
            Self::Unsupported(_) => Some(FailureCode::Unsupported),
            _ => Some(FailureCode::Validation),
        }
    }
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for VectorError {}

pub type Result<T> = std::result::Result<T, VectorError>;
