//! The Prism -> Folio colour-book join (lane SMB-R53-FOLLOWUPS-1, MT-31251; STU-COL-188 through
//! STU-COL-126, licensing per STU-COL-186). Prism parses an ICC named-colour profile
//! (`parse_named_colour_profile`, the STU-COL-126 profile owner) and Folio holds the colour book
//! (`StudioColourBook::from_named_colours`, the book's adapter input); the two crates share no edge,
//! so this host module joins them. It adds no colour math: the book's entries are the profile's root
//! names (name and code) and Prism's measured PCS Lab values unchanged, its prefix and suffix are the
//! profile's, and the book records the profile as its `named_colour_profile` reference (the profile's
//! `SCPF` id and the sha256 Prism verified).
//!
//! Licensing is the operator's per-book posture ([`NamedColourBookSpec::license_state`]):
//! - `bundled`: the book is built and loaded with the bundled origin; it is among
//!   `StudioColourBookLibrary::bundled_books`, the only books a build may ship;
//! - `operator_supplied` (the rights holder does not permit redistribution): the book is built from
//!   the operator's profile at run time and loaded with the operator origin, so look-up works and it
//!   is never among the bundled books; loading it with the bundled origin is refused
//!   (`license_state_mismatch`, Folio's origin rule) and leaves the library unchanged;
//! - `absent`: the profile is not read; the library records the book as absent, so documents keep
//!   their references and stored swatches and only look-up is lost (`book_absent`).
//!
//! Budgets: Prism bounds the profile (2 MiB, 16384 colours, 15 device coordinates) before storing a
//! colour; Folio bounds the book (16384 entries, 4 MiB resource, name and code length, Lab ranges,
//! unique codes) and the library (256 books). The book enters the library through Folio's one
//! resource format (`StudioColourBookLibrary::load`), so a joined book passes the same checks as any
//! book file. Tests: `tests/named_colour_book.rs`.
#![deny(clippy::arithmetic_side_effects)]

use crate::folio_report::{FolioEmission, FolioReportRoute, book_terminal, emit, rejected};
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio as folio;
use hsk_studio_observe::{DiagnosticCode, FailureCode};
use hsk_studio_prism as prism;

/// The operator's description of one book read from a named-colour profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedColourBookSpec<'a> {
    /// Folio key id of the book.
    pub book_id: &'a str,
    pub title: &'a str,
    /// STU-COL-186: the operator's licensing posture for this book.
    pub license_state: folio::StudioBookLicenseState,
    pub license_note: &'a str,
}

/// Refusals of the join: Prism's profile refusal or Folio's book refusal, typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedColourBookError {
    Profile(prism::NamedColourError),
    Book(folio::StudioColourBookError),
}
impl NamedColourBookError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Profile(e) => e.code(),
            Self::Book(e) => e.code(),
        }
    }
}
impl std::fmt::Display for NamedColourBookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NamedColourBookError {}

/// Builds the Folio colour book of a named-colour profile (module docs). An `absent` posture builds
/// no book (`license_state_mismatch`): an absent book is recorded, never built.
pub fn colour_book_from_named_colour_profile(
    input: prism::ProfileInput<'_>,
    spec: &NamedColourBookSpec<'_>,
    token: &CancellationToken,
) -> Result<folio::StudioColourBook, NamedColourBookError> {
    // RED stub (SMB-R53-FOLLOWUPS-1 red commit): the join is not built yet.
    let _ = (input, spec, token);
    Err(NamedColourBookError::Book(folio::StudioColourBookError::Malformed))
}

/// Joins a named-colour profile into the host's colour-book library under the operator's posture
/// (module docs): `bundled` and `operator_supplied` books are built and loaded with `origin` (which
/// must match the posture), an `absent` book is recorded absent without reading the profile. On any
/// refusal the library is unchanged.
pub fn load_named_colour_book(
    library: &mut folio::StudioColourBookLibrary,
    ctx: &CommandContext,
    input: prism::ProfileInput<'_>,
    spec: &NamedColourBookSpec<'_>,
    origin: folio::StudioBookOrigin,
    token: &CancellationToken,
) -> Result<folio::StudioBookReport, NamedColourBookError> {
    // RED stub (SMB-R53-FOLLOWUPS-1 red commit): the join is not built yet.
    let _ = (library, ctx, origin);
    colour_book_from_named_colour_profile(input, spec, token).map(|_| unreachable!())
}

/// [`load_named_colour_book`] plus exactly one terminal through observe, in the form of
/// `folio_report` (codes, counters and the context's correlation only). A Prism refusal is a
/// rejected failure with its Prism code: `named_colour_count_limit` and
/// `named_colour_device_coord_limit` diagnostic `BudgetExceeded`, `unsupported_version`,
/// `not_named_colour_class` and `unsupported_pcs` failure unsupported, a canceled parse the
/// canceled outcome, every other refusal (identity, framing, tag, truncation, names) diagnostic
/// `InvalidInput`.
pub fn load_named_colour_book_observed(
    library: &mut folio::StudioColourBookLibrary,
    ctx: &CommandContext,
    input: prism::ProfileInput<'_>,
    spec: &NamedColourBookSpec<'_>,
    origin: folio::StudioBookOrigin,
    token: &CancellationToken,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioBookReport, NamedColourBookError>, FolioEmission) {
    let result = load_named_colour_book(library, ctx, input, spec, origin, token);
    let terminal = match &result {
        Ok(r) => book_terminal(Ok(r)),
        Err(NamedColourBookError::Book(e)) => book_terminal(Err(*e)),
        Err(NamedColourBookError::Profile(e)) => {
            use prism::NamedColourError as P;
            let (failure, diagnostic) = match e {
                P::Canceled => (None, DiagnosticCode::Canceled),
                P::CountLimit | P::DeviceCoordLimit => (Some(FailureCode::Validation), DiagnosticCode::BudgetExceeded),
                P::UnsupportedVersion | P::NotNamedColourClass | P::UnsupportedPcs => (Some(FailureCode::Unsupported), DiagnosticCode::InvalidInput),
                P::Wide(_) | P::MissingNamedColourTag | P::TagType | P::Truncated | P::InvalidName => (Some(FailureCode::Validation), DiagnosticCode::InvalidInput),
            };
            rejected(e.code(), failure, diagnostic)
        }
    };
    (result, emit(ctx, terminal, route))
}
