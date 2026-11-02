//! Host adapter for Folio's swatch-group and colour-book reports (lane SMB-R53-FOLLOWUPS-1,
//! STUDIO-R53 F-R53-1, HOST INTEGRATION rule 5). Folio may depend only on Accord, so its mutating
//! entries `apply_swatch_group_command`, `undo_swatch_group_command`, `add_colour_book_swatches`,
//! `StudioColourBookLibrary::load` and `StudioColourBookLibrary::mark_absent` return code-and-counter
//! reports (`StudioSwatchEditReport`, `StudioBookReport`) with the caller's opaque correlation. Each
//! `*_observed` function here calls the Folio entry with the caller's `CommandContext` (attribution is
//! the caller's: this module mints no actor, command or correlation) and emits exactly one Observe
//! terminal for its result, in the form other producers use (press-write `ObserveRoute`): observe's
//! version-2 detail with the closed outcome, disposition, diagnostic code and five capped counters.
//!
//! The frame carries the context's correlation and base revision and nothing else from the call:
//! never a swatch, group, book or entry name, id, colour or document content (test
//! `tests/folio_report.rs` `frames_carry_no_name_id_or_colour`). The returned result is Folio's own,
//! whatever the emission did: a refused or failed delivery is reported in [`FolioEmission::receipt`]
//! and never changes the result (test `emission_never_changes_the_result`).
//!
//! Terminal codes ([`FolioEmission::code`], stable tokens):
//! - an applied group command or undo: the command id (`swatch.group.*`), success; disposition
//!   `changed`, or `no_change` when the resulting palette equals the input; counters
//!   `[groups, swatches moved, groups, swatches moved, swatches moved]` (groups of the result, the
//!   implicit root counting one; zero moved on `no_change`);
//! - a library change or population: the operation (`book.load`, `book.mark_absent`,
//!   `swatch.book.add`), success, disposition `changed`, counters
//!   `[books, entries, books, entries, entries]`;
//! - a refusal: the Folio error code (`StudioSwatchGroupError::code`, `StudioColourBookError::code`),
//!   a rejected failure with zero counters (the diagnostic code per error below); a canceled book
//!   operation is the canceled outcome with diagnostic `Canceled`.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio as folio;
use hsk_studio_observe::{
    DiagnosticCode, Disposition, Error as ObserveError, FailureCode, MAX_DETAIL_COUNT, Observe, Outcome,
    Receipt, SinkPort,
};

/// Where one operation's terminal goes: the cancellation token, the one [`Observe`] emitter of the
/// operation (built by the host with the context's correlation, base revision, resource and actor)
/// and the sink it delivers to. An emitter carries one terminal, so a route serves one operation.
pub struct FolioReportRoute<'a> {
    pub cancel: &'a CancellationToken,
    pub observe: &'a mut Observe,
    pub sink: &'a mut dyn SinkPort,
}

/// The terminal emission of one operation: its typed code (a stable token, never document text)
/// and the delivery receipt or the observe refusal.
#[derive(Debug, PartialEq)]
pub struct FolioEmission {
    pub code: &'static str,
    pub receipt: Result<Receipt, ObserveError>,
}

/// A count capped at observe's detail bound.
fn capped(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX).min(MAX_DETAIL_COUNT)
}

/// One terminal of an operation attributed to `ctx`.
pub(crate) struct Terminal {
    code: &'static str,
    outcome: Outcome,
    disposition: Disposition,
    diagnostic: Option<DiagnosticCode>,
    counts: [u32; 5],
}

pub(crate) fn emit(ctx: &CommandContext, t: Terminal, route: FolioReportRoute<'_>) -> FolioEmission {
    // RED stub (SMB-R53-FOLLOWUPS-1 red commit): nothing is emitted yet.
    let _ = (ctx, t.outcome, t.disposition, t.diagnostic, t.counts, route.cancel, route.observe, route.sink);
    FolioEmission { code: "", receipt: Err(ObserveError::Closed) }
}

/// A rejected terminal with zero counters; `failure` `None` is the canceled outcome.
pub(crate) fn rejected(code: &'static str, failure: Option<FailureCode>, diagnostic: DiagnosticCode) -> Terminal {
    let outcome = failure.map_or(Outcome::Canceled, Outcome::Failure);
    Terminal { code, outcome, disposition: Disposition::Rejected, diagnostic: Some(diagnostic), counts: [0; 5] }
}

fn swatch_refusal(e: folio::StudioSwatchGroupError) -> Terminal {
    use folio::StudioSwatchGroupError as E;
    let (failure, diagnostic) = match e {
        E::CommandMissing | E::CommandMismatch => (FailureCode::Validation, DiagnosticCode::InvalidCommand),
        E::StaleRevision { .. } => (FailureCode::Unavailable, DiagnosticCode::RevisionConflict),
        E::ReadOnlyScope => (FailureCode::Unsupported, DiagnosticCode::InvalidTarget),
        E::SecondRoot | E::DuplicateGroup => (FailureCode::Validation, DiagnosticCode::DuplicateAddress),
        E::RootMissing | E::RootNotDeletable | E::RootNotMovable | E::GroupMissing | E::SwatchMissing | E::InvalidReplacement => {
            (FailureCode::Validation, DiagnosticCode::InvalidTarget)
        }
        E::InvalidGroupId | E::InvalidName | E::IndexOutOfRange => (FailureCode::Validation, DiagnosticCode::InvalidInput),
        E::BudgetExceeded => (FailureCode::Validation, DiagnosticCode::BudgetExceeded),
        E::InvalidPalette(_) => (FailureCode::Validation, DiagnosticCode::ValidationRejected),
    };
    rejected(e.code(), Some(failure), diagnostic)
}

fn book_refusal(e: folio::StudioColourBookError) -> Terminal {
    use folio::StudioColourBookError as E;
    let (failure, diagnostic) = match e {
        E::Canceled => return rejected(e.code(), None, DiagnosticCode::Canceled),
        E::BudgetExceeded => (FailureCode::Validation, DiagnosticCode::BudgetExceeded),
        E::Malformed | E::InvalidSwatchId => (FailureCode::Validation, DiagnosticCode::InvalidInput),
        E::InvalidField | E::ArityMismatch | E::CountMismatch | E::InvalidPageLayout | E::InvalidComponents | E::LicenseStateMismatch | E::AbsentWithEntries => {
            (FailureCode::Validation, DiagnosticCode::ValidationRejected)
        }
        E::DuplicateEntry => (FailureCode::Validation, DiagnosticCode::DuplicateAddress),
        E::EntryMissing => (FailureCode::Validation, DiagnosticCode::MissingRead),
        E::CommandMissing => (FailureCode::Validation, DiagnosticCode::InvalidCommand),
        E::StaleRevision { .. } => (FailureCode::Unavailable, DiagnosticCode::RevisionConflict),
    };
    rejected(e.code(), Some(failure), diagnostic)
}

fn swatch_terminal<T: folio::StudioSwatchContainer>(input: &T, result: &Result<folio::StudioSwatchGroupOutcome<T>, folio::StudioSwatchGroupError>) -> Terminal {
    match result {
        Ok(o) => {
            let unchanged = o.palette.swatch_list() == input.swatch_list() && o.palette.group_list() == input.group_list();
            let (groups, moved) = (capped(o.report.groups), if unchanged { 0 } else { capped(o.report.swatches_moved) });
            let disposition = if unchanged { Disposition::NoChange } else { Disposition::Changed };
            Terminal { code: o.report.command, outcome: Outcome::Success, disposition, diagnostic: None, counts: [groups, moved, groups, moved, moved] }
        }
        Err(e) => swatch_refusal(*e),
    }
}

pub(crate) fn book_terminal(result: Result<&folio::StudioBookReport, folio::StudioColourBookError>) -> Terminal {
    match result {
        Ok(r) => {
            let (books, entries) = (capped(r.books), capped(r.entries));
            Terminal { code: r.operation, outcome: Outcome::Success, disposition: Disposition::Changed, diagnostic: None, counts: [books, entries, books, entries, entries] }
        }
        Err(e) => book_refusal(e),
    }
}

/// [`folio::apply_swatch_group_command`] plus exactly one terminal through observe (module docs).
pub fn apply_swatch_group_command_observed<T: folio::StudioSwatchContainer>(
    palette: &T,
    ctx: &CommandContext,
    revision: u64,
    cmd: &folio::StudioSwatchGroupCommand,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioSwatchGroupOutcome<T>, folio::StudioSwatchGroupError>, FolioEmission) {
    let result = folio::apply_swatch_group_command(palette, ctx, revision, cmd);
    let emission = emit(ctx, swatch_terminal(palette, &result), route);
    (result, emission)
}

/// [`folio::undo_swatch_group_command`] plus exactly one terminal through observe (module docs).
pub fn undo_swatch_group_command_observed<T: folio::StudioSwatchContainer>(
    palette: &T,
    ctx: &CommandContext,
    revision: u64,
    state: &folio::StudioSwatchPaletteState,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioSwatchGroupOutcome<T>, folio::StudioSwatchGroupError>, FolioEmission) {
    let result = folio::undo_swatch_group_command(palette, ctx, revision, state);
    let emission = emit(ctx, swatch_terminal(palette, &result), route);
    (result, emission)
}

/// [`folio::add_colour_book_swatches`] plus exactly one terminal through observe (module docs).
pub fn add_colour_book_swatches_observed(
    records: &folio::StudioVectorRecords,
    ctx: &CommandContext,
    revision: u64,
    book: &folio::StudioColourBook,
    picks: &[(&str, &str)],
    token: &CancellationToken,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioBookSwatchOutcome, folio::StudioColourBookError>, FolioEmission) {
    let result = folio::add_colour_book_swatches(records, ctx, revision, book, picks, token);
    let emission = emit(ctx, book_terminal(result.as_ref().map(|o| &o.report).map_err(|e| *e)), route);
    (result, emission)
}

/// [`folio::StudioColourBookLibrary::load`] plus exactly one terminal through observe (module docs).
pub fn load_colour_book_observed(
    library: &mut folio::StudioColourBookLibrary,
    ctx: &CommandContext,
    bytes: &[u8],
    origin: folio::StudioBookOrigin,
    token: &CancellationToken,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioBookReport, folio::StudioColourBookError>, FolioEmission) {
    let result = library.load(ctx, bytes, origin, token);
    let emission = emit(ctx, book_terminal(result.as_ref().map_err(|e| *e)), route);
    (result, emission)
}

/// [`folio::StudioColourBookLibrary::mark_absent`] plus exactly one terminal through observe (module
/// docs).
pub fn mark_colour_book_absent_observed(
    library: &mut folio::StudioColourBookLibrary,
    ctx: &CommandContext,
    book_id: &str,
    route: FolioReportRoute<'_>,
) -> (Result<folio::StudioBookReport, folio::StudioColourBookError>, FolioEmission) {
    let result = library.mark_absent(ctx, book_id);
    let emission = emit(ctx, book_terminal(result.as_ref().map_err(|e| *e)), route);
    (result, emission)
}
