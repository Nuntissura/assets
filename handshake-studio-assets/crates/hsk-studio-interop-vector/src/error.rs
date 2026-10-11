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
            Self::EntityDenied => "entity_denied",
            Self::InvalidPolicy(_) => "invalid_policy",
            Self::NativeEditabilityBlocked { .. } => "native_editability_blocked",
            Self::PdfUnavailable => "pdf_unavailable",
            Self::Folio(_) => "folio_refused",
            Self::Unsupported(_) => "unsupported",
        }
    }

    /// Observe mapping: `None` is the `Canceled` outcome.
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::NativeEditabilityBlocked { .. } | Self::PdfUnavailable => {
                Some(FailureCode::Unavailable)
            }
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
