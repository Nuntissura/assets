//! Typed refusals and hostile-input limits. Messages are static strings and carry no document text.
use std::fmt;

/// Hostile-input limits. Every allocation the reader or writer makes is bounded by one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum input (and output) size in bytes.
    pub max_bytes: usize,
    /// Maximum number of in-use xref entries / loaded objects.
    pub max_objects: usize,
    /// Maximum nesting depth of arrays/dictionaries and of the page tree.
    pub max_depth: usize,
    /// Maximum decoded size of one stream.
    pub max_decoded_bytes: usize,
    /// Maximum decoded bytes summed over one operation (decompression-bomb bound).
    pub max_decoded_total: usize,
    /// Maximum number of cross-reference sections followed (`/Prev`, `/XRefStm`).
    pub max_xref_sections: usize,
    /// Maximum number of pages in the page tree.
    pub max_pages: usize,
    /// Maximum number of content-stream operations tokenised per page.
    pub max_content_ops: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 64 << 20,
            max_objects: 200_000,
            max_depth: 32,
            max_decoded_bytes: 64 << 20,
            max_decoded_total: 256 << 20,
            max_xref_sections: 64,
            max_pages: 10_000,
            max_content_ops: 2_000_000,
        }
    }
}

/// Content classes a region redaction cannot yet remove with proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RedactionClass {
    Image,
    Vector,
    FormXObject,
    Shading,
    InlineImage,
}

impl RedactionClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Vector => "vector",
            Self::FormXObject => "form_xobject",
            Self::Shading => "shading",
            Self::InlineImage => "inline_image",
        }
    }
}

/// A sink-side fault (disk full, handle lost, ...). Carries no detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PressError {
    Canceled,
    /// A named hostile-input limit was exceeded.
    InputLimit(&'static str),
    /// Structurally invalid PDF input.
    Malformed(&'static str),
    /// `/Encrypt` present; this slice cannot decrypt, so nothing is edited, saved or redacted.
    Encrypted,
    /// `/Encrypt` present and the modify permission bit is clear.
    PermissionDenied,
    /// A byte-range signature exists; a full rewrite would invalidate it.
    SignedDocument,
    NoSuchObject,
    /// Target is not a dictionary object (streams, arrays, scalars).
    UnsupportedObject,
    /// `expected_source_sha256` differs from the opened input.
    StaleSource,
    KeyNotAllowed,
    /// A stream filter this slice does not implement (name is the PDF filter name).
    UnsupportedFilter(String),
    /// Content in the redaction region that cannot be removed with proof (fail closed).
    UnsupportedRedactionClass(RedactionClass),
    /// A removed object is still reachable from the output (internal proof failed).
    RedactionIncomplete,
    NoSuchPage,
    Sink(SinkError),
}

impl PressError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InputLimit(_) => "input_limit",
            Self::Malformed(_) => "malformed",
            Self::Encrypted => "encrypted",
            Self::PermissionDenied => "permission_denied",
            Self::SignedDocument => "signed_document",
            Self::NoSuchObject => "no_such_object",
            Self::UnsupportedObject => "unsupported_object",
            Self::StaleSource => "stale_source",
            Self::KeyNotAllowed => "key_not_allowed",
            Self::UnsupportedFilter(_) => "unsupported_filter",
            Self::UnsupportedRedactionClass(_) => "unsupported_redaction_class",
            Self::RedactionIncomplete => "redaction_incomplete",
            Self::NoSuchPage => "no_such_page",
            Self::Sink(_) => "sink_fault",
        }
    }
}

impl fmt::Display for PressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputLimit(w) | Self::Malformed(w) => write!(f, "{}: {w}", self.code()),
            Self::UnsupportedFilter(n) => write!(f, "{}: {n}", self.code()),
            Self::UnsupportedRedactionClass(c) => write!(f, "{}: {}", self.code(), c.as_str()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for PressError {}

pub(crate) type Result<T> = std::result::Result<T, PressError>;
