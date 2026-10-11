//! Typed errors and typed loss/unsupported reports. No source text is carried, only kinds and byte spans.

use hsk_studio_observe::{FailureCode, Outcome};
use std::fmt;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    Bytes,
    Nodes,
    Depth,
    Attributes,
    Edits,
    MediaBlocks,
}
impl LimitKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Bytes => "limit_bytes",
            Self::Nodes => "limit_nodes",
            Self::Depth => "limit_depth",
            Self::Attributes => "limit_attributes",
            Self::Edits => "limit_edits",
            Self::MediaBlocks => "limit_media_blocks",
        }
    }
}

/// Input whose structure cannot be determined without guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MalformedKind {
    UnterminatedTag,
    UnterminatedComment,
    UnterminatedValue,
    UnterminatedRawText,
    UnterminatedMarkup,
    UnterminatedCss,
    InvalidAttribute,
    InvalidEndTag,
    MisnestedEndTag,
    StrayEndTag,
    VoidEndTag,
    UnclosedElement,
    InvalidFragment,
    InvalidMediaQuery,
}
impl MalformedKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnterminatedTag => "unterminated_tag",
            Self::UnterminatedComment => "unterminated_comment",
            Self::UnterminatedValue => "unterminated_value",
            Self::UnterminatedRawText => "unterminated_raw_text",
            Self::UnterminatedMarkup => "unterminated_markup",
            Self::UnterminatedCss => "unterminated_css",
            Self::InvalidAttribute => "invalid_attribute",
            Self::InvalidEndTag => "invalid_end_tag",
            Self::MisnestedEndTag => "misnested_end_tag",
            Self::StrayEndTag => "stray_end_tag",
            Self::VoidEndTag => "void_end_tag",
            Self::UnclosedElement => "unclosed_element",
            Self::InvalidFragment => "invalid_fragment",
            Self::InvalidMediaQuery => "invalid_media_query",
        }
    }
}

/// Valid input outside the documented subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// Optional end tags (`<p>`, `<li>`, ...) need HTML5 tree construction.
    ImpliedEndTag,
    PlaintextElement,
    /// CDATA, processing instructions or bogus markup inside a fragment to be inserted.
    FragmentConstruct,
    MediaType,
    MediaFeature,
    MediaUnit,
    MediaSyntax,
}
impl UnsupportedKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ImpliedEndTag => "implied_end_tag",
            Self::PlaintextElement => "plaintext_element",
            Self::FragmentConstruct => "fragment_construct",
            Self::MediaType => "media_type",
            Self::MediaFeature => "media_feature",
            Self::MediaUnit => "media_unit",
            Self::MediaSyntax => "media_syntax",
        }
    }
}

/// Why an edit operation refuses a target node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditRefusal {
    DocumentRoot,
    NotAnElement,
    /// Void or self-closed element: no content to edit and no place to insert.
    Void,
    /// Text replacement would destroy element, comment or other markup children.
    HasMarkupChildren,
    /// The node is an opaque unsupported construct; it is kept byte-exact and never patched.
    Opaque,
    /// Raw text child nodes are edited through their owning element.
    RawChild,
    /// The replacement would terminate its raw-text element early.
    UnsafeRawText,
    IndexOutOfRange,
}
impl EditRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::DocumentRoot => "document_root",
            Self::NotAnElement => "not_an_element",
            Self::Void => "void",
            Self::HasMarkupChildren => "has_markup_children",
            Self::Opaque => "opaque",
            Self::RawChild => "raw_child",
            Self::UnsafeRawText => "unsafe_raw_text",
            Self::IndexOutOfRange => "index_out_of_range",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebError {
    Canceled,
    LimitExceeded { limit: LimitKind, span: Range<usize> },
    Malformed { kind: MalformedKind, span: Range<usize> },
    Unsupported { kind: UnsupportedKind, span: Range<usize> },
    NoSuchNode,
    NoSuchAttribute,
    /// Attribute or tag name outside the writable name grammar.
    InvalidName,
    InvalidSpan { span: Range<usize> },
    OverlappingEdits { first: Range<usize>, second: Range<usize> },
    NotEditable { reason: EditRefusal },
    /// Same revision number but different source bytes than the patch was made against.
    Conflict { span: Range<usize> },
    StaleRevision { expected: u64, found: u64 },
    RevisionOverflow,
}
impl WebError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::LimitExceeded { limit, .. } => limit.code(),
            Self::Malformed { kind, .. } => kind.code(),
            Self::Unsupported { kind, .. } => kind.code(),
            Self::NoSuchNode => "no_such_node",
            Self::NoSuchAttribute => "no_such_attribute",
            Self::InvalidName => "invalid_name",
            Self::InvalidSpan { .. } => "invalid_span",
            Self::OverlappingEdits { .. } => "overlapping_edits",
            Self::NotEditable { reason } => reason.code(),
            Self::Conflict { .. } => "conflict",
            Self::StaleRevision { .. } => "stale_revision",
            Self::RevisionOverflow => "revision_overflow",
        }
    }

    /// Byte span in the input that was being processed, when the error has one.
    pub fn span(&self) -> Option<&Range<usize>> {
        match self {
            Self::LimitExceeded { span, .. }
            | Self::Malformed { span, .. }
            | Self::Unsupported { span, .. }
            | Self::InvalidSpan { span }
            | Self::Conflict { span } => Some(span),
            _ => None,
        }
    }

    /// Observe outcome mapping: unsupported is `Unsupported`, cancellation is `Canceled`, the rest `Validation`.
    pub const fn outcome(&self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::Unsupported { .. } => Outcome::Failure(FailureCode::Unsupported),
            _ => Outcome::Failure(FailureCode::Validation),
        }
    }

    pub(crate) fn shift(mut self, by: usize) -> Self {
        match &mut self {
            Self::LimitExceeded { span, .. }
            | Self::Malformed { span, .. }
            | Self::Unsupported { span, .. }
            | Self::InvalidSpan { span }
            | Self::Conflict { span } => *span = span.start + by..span.end + by,
            _ => {}
        }
        self
    }
}
impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for WebError {}

impl From<hsk_studio_accord::ValidationError> for WebError {
    fn from(_: hsk_studio_accord::ValidationError) -> Self {
        // The only error accord's token produces is cancellation.
        Self::Canceled
    }
}

/// Non-fatal findings: the document parsed and round-trips, but something is kept opaque, was interpreted
/// per the HTML rules, or is outside the supported subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossKind {
    /// `<div/>`: HTML ignores the slash on non-void elements; the element stays open.
    SelfClosingIgnored,
    /// Second and later occurrences of an attribute name are ignored by HTML; the first wins.
    DuplicateAttribute,
    /// Kept as an opaque `Unsupported` node, never edited.
    Cdata,
    ProcessingInstruction,
    BogusMarkup,
    /// `svg`/`math` content is parsed with the HTML subset grammar only (self-closing allowed, no CDATA).
    ForeignContent,
}
impl LossKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SelfClosingIgnored => "self_closing_ignored",
            Self::DuplicateAttribute => "duplicate_attribute",
            Self::Cdata => "cdata",
            Self::ProcessingInstruction => "processing_instruction",
            Self::BogusMarkup => "bogus_markup",
            Self::ForeignContent => "foreign_content",
        }
    }
    /// True when the finding makes a node opaque (kept verbatim, never patched).
    pub const fn is_opaque(self) -> bool {
        matches!(self, Self::Cdata | Self::ProcessingInstruction | Self::BogusMarkup)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossEntry {
    pub kind: LossKind,
    pub span: Range<usize>,
}

pub const MAX_LOSS_ENTRIES: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LossReport {
    entries: Vec<LossEntry>,
    dropped: usize,
}
impl LossReport {
    pub fn entries(&self) -> &[LossEntry] {
        &self.entries
    }
    /// Findings beyond `MAX_LOSS_ENTRIES` are counted, not stored.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.dropped == 0
    }
    pub fn count(&self, kind: LossKind) -> usize {
        self.entries.iter().filter(|e| e.kind == kind).count()
    }
    pub(crate) fn push(&mut self, kind: LossKind, span: Range<usize>) {
        if self.entries.len() < MAX_LOSS_ENTRIES {
            self.entries.push(LossEntry { kind, span });
        } else {
            self.dropped += 1;
        }
    }
}
