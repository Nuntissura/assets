//! One sequence-records change = one undo step (lane SMB-PR-SEQUENCE-SESSION-1; pattern of
//! `crate::multicam_step`).
//!
//! A [`SequenceRecordsStep`] carries zero or more [`SequenceEdit`]s (each quoting the live value) and
//! optional edits of the `sequence_settings`, `sequence_presets` and `track_display` entries (each
//! quoting the live records, `None` = no entry stored; an absent [`RecordEdit`] leaves that entry alone
//! and does not quote it). The session applies the whole step as ONE revision (`revision + 1`, also when only a
//! record changes, declared reading as in multicam), ONE receipt and ONE inverse
//! ([`crate::Applied::sequence_records_inverse`]); the undo's own inverse is the redo. Any refusal
//! leaves the document, the entries and the receipts untouched. A step that replaced records which did
//! not validate cannot put them back, so its inverse leaves that entry alone and the rest of the step
//! keeps its undo; [`crate::Applied::sequence_records_undo`] says which, and says when no undo exists
//! (R56 F-R56-7; tests/entry_references.rs).
//!
//! The three Folio commands (`hsk_studio_folio::SEQUENCE_SETTINGS_COMMAND_IDS`) reach the same step
//! through `Session::submit_sequence_command_as` and `_keyed_as`: Folio runs the command at proposal
//! time on the live document and entries under the caller's `CommandContext`, and its result becomes
//! the step. `save_from_sequence` and `apply` see the shipped library plus the stored presets (a
//! stored preset shadows a shipped one of the same id); only the stored presets are written. The six
//! track display commands (`hsk_studio_folio::TRACK_DISPLAY_COMMAND_IDS`) reach it the same way through
//! `Session::submit_track_display_command_as` and `_keyed_as`, and read only the `track_display` entry.
//!
//! The step shares the submit pipeline with provider submits. A commit authority receives the
//! successor DOCUMENT; a step's records travel in the binding hash, a command's binding covers the
//! command and its expected revision (declared reading: the entries are session-held state, stored
//! only when the session publishes the successor).
use crate::error::SessionError;
use crate::multicam_step::{self, MulticamStep, SequenceEdit};
use crate::sequence_records::{SEQUENCE_PRESETS_ENTRY, SEQUENCE_SETTINGS_ENTRY, TRACK_DISPLAY_ENTRY};
use crate::timeline::valid_command_id;
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio::{
    self as folio, Code, Resolver, Snapshot, StudioDocument, StudioMulticamRecords, StudioSequencePresetSet,
    StudioSequenceSettingsCommand, StudioSequenceSettingsError, StudioSequenceSettingsRecords,
    StudioSequenceSettingsReport, StudioTrackDisplayCommand, StudioTrackDisplayError, StudioTrackDisplayRecords,
    StudioTrackDisplayReport, apply_sequence_settings_command, apply_track_display_command, parse_sequence_presets,
    parse_sequence_settings_records, parse_track_display_records, shipped_sequence_presets, validate_multicam_records,
};

/// Provider name recorded in [`crate::Applied::provider`] and in the idempotency binding.
pub const SEQUENCE_RECORDS_STEP_PROVIDER: &str = "sequence.records";

/// One record entry's change: `expect` quotes the live record set (`None` = no entry), `write` is
/// the set after the step (`None` = the entry is removed).
#[derive(Clone, Debug, PartialEq)]
pub struct RecordEdit<T> {
    pub expect: Option<T>,
    pub write: Option<T>,
}

impl<T: Clone> RecordEdit<T> {
    fn swapped(&self) -> Self {
        Self { expect: self.write.clone(), write: self.expect.clone() }
    }
}

/// A sequence-records step (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceRecordsStep {
    pub command_id: String,
    pub correlation_id: u64,
    /// Applied in order; each sequence id appears in at most one edit.
    pub sequences: Vec<SequenceEdit>,
    /// `None` leaves the `sequence_settings` entry alone.
    pub settings: Option<RecordEdit<StudioSequenceSettingsRecords>>,
    /// `None` leaves the `sequence_presets` entry alone.
    pub presets: Option<RecordEdit<StudioSequencePresetSet>>,
    /// `None` leaves the `track_display` entry alone.
    pub track_display: Option<RecordEdit<StudioTrackDisplayRecords>>,
}

/// What undoing a committed sequence-records step can restore ([`crate::Applied::sequence_records_undo`]).
/// A step that replaced records which did not validate cannot put them back: a commit never writes
/// invalid records. The tokens name the entries the inverse leaves as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SequenceRecordsUndo {
    /// The inverse restores everything the step changed.
    Complete,
    /// The inverse restores the sequences and every other entry, and leaves these entries alone.
    WithoutEntries(Vec<&'static str>),
    /// No undo exists (`Applied::sequence_records_inverse` is `None`): these entries cannot be put
    /// back, and nothing else the step changed can be restored without them.
    Unavailable(Vec<&'static str>),
}

impl SequenceRecordsStep {
    fn sequence_part(&self) -> MulticamStep {
        MulticamStep {
            command_id: self.command_id.clone(),
            correlation_id: self.correlation_id,
            sequences: self.sequences.clone(),
            records_expect: None,
            records_write: None,
        }
    }

    /// Deterministic bytes of the request for the idempotency binding and admission cost.
    pub(crate) fn binding_bytes(&self) -> Result<Vec<u8>, SessionError> {
        let invalid = |_| SessionError::Provider("invalid_input");
        let mut out = b"hsk-studio-sequence-records-step-1\0".to_vec();
        out.extend_from_slice(&self.sequence_part().binding_bytes()?);
        out.push(0);
        let settings = self.settings.as_ref().map(|e| (&e.expect, &e.write));
        out.extend_from_slice(&serde_json::to_vec(&settings).map_err(invalid)?);
        out.push(0);
        let presets = self.presets.as_ref().map(|e| (&e.expect, &e.write));
        out.extend_from_slice(&serde_json::to_vec(&presets).map_err(invalid)?);
        out.push(0);
        let track_display = self.track_display.as_ref().map(|e| (&e.expect, &e.write));
        out.extend_from_slice(&serde_json::to_vec(&track_display).map_err(invalid)?);
        Ok(out)
    }
}

/// Deterministic bytes of a Folio command under its context, for the idempotency binding.
pub(crate) fn command_binding_bytes(
    context: &CommandContext,
    command: &StudioSequenceSettingsCommand,
) -> Result<Vec<u8>, SessionError> {
    let invalid = |_| SessionError::Provider("invalid_input");
    let fields = match command {
        StudioSequenceSettingsCommand::SavePresetFromSequence { sequence_id, preset_id, name, category_path } => {
            serde_json::to_vec(&(sequence_id, preset_id, name, category_path))
        }
        StudioSequenceSettingsCommand::ApplyPreset { preset_id, sequence_id } => serde_json::to_vec(&(preset_id, sequence_id)),
        StudioSequenceSettingsCommand::SetEditingMode { sequence_id, mode_id } => serde_json::to_vec(&(sequence_id, mode_id)),
    }
    .map_err(invalid)?;
    let mut out = format!(
        "hsk-studio-sequence-command-1\0{}\0{}\0{}\0",
        command.command_id(),
        context.command().map_or("", |c| c.as_str()),
        context.correlation().value()
    )
    .into_bytes();
    out.extend_from_slice(&fields);
    Ok(out)
}

/// Deterministic bytes of a track display command under its context, for the idempotency binding.
pub(crate) fn track_display_binding_bytes(
    context: &CommandContext,
    command: &StudioTrackDisplayCommand,
) -> Result<Vec<u8>, SessionError> {
    use StudioTrackDisplayCommand as C;
    let fields = match command {
        C::SetSwitches { sequence_id, switches } => serde_json::to_vec(&(sequence_id, switches)),
        C::SetHeight { sequence_id, kind, height } => serde_json::to_vec(&(sequence_id, kind, height)),
        C::ExpandAll { sequence_id } | C::MinimizeAll { sequence_id } => serde_json::to_vec(sequence_id),
        C::SaveHeightPreset { sequence_id, preset_id, name } => serde_json::to_vec(&(sequence_id, preset_id, name)),
        C::RecallHeightPreset { sequence_id, preset_id } => serde_json::to_vec(&(sequence_id, preset_id)),
    }
    .map_err(|_| SessionError::Provider("invalid_input"))?;
    let mut out = format!(
        "hsk-studio-track-display-command-1\0{}\0{}\0{}\0",
        command.command_id(),
        context.command().map_or("", |c| c.as_str()),
        context.correlation().value()
    )
    .into_bytes();
    out.extend_from_slice(&fields);
    Ok(out)
}

/// The live entry bytes a step is checked against (`Err` = a repeated token, see
/// `crate::sequence_records::held_body`).
pub(crate) struct LiveEntries<'a> {
    pub settings: Result<Option<&'a [u8]>, folio::Diagnostic>,
    pub presets: Result<Option<&'a [u8]>, folio::Diagnostic>,
    pub track_display: Result<Option<&'a [u8]>, folio::Diagnostic>,
    pub multicam: Option<&'a [u8]>,
}

/// The checked outcome of a step: the successor document (`revision + 1`), the entry writes
/// (`None` = remove) and the inverse step.
pub(crate) struct Checked {
    pub successor: Snapshot,
    pub entries: Vec<(&'static str, Option<Vec<u8>>)>,
    pub inverse: Option<SequenceRecordsStep>,
    pub undo: SequenceRecordsUndo,
}

/// The inverse half of one record edit. Records that validate against the document the undo restores
/// (and `None`) can go back. Records that do not cannot: the half is dropped and `token` named, and
/// `blocked` is set when what the step wrote would not validate there either, because the inverse could
/// then never apply. Only a cancel is an error.
fn inverse_half<T: Clone + PartialEq>(
    edit: &RecordEdit<T>,
    token: &'static str,
    not_restored: &mut Vec<&'static str>,
    blocked: &mut bool,
    validates: impl Fn(&T) -> Result<(), folio::Diagnostic>,
) -> Result<Option<RecordEdit<T>>, SessionError> {
    let holds = |records: &T| match validates(records) {
        Ok(()) => Ok(true),
        Err(d) if d.code == Code::Canceled => Err(SessionError::Canceled),
        Err(_) => Ok(false),
    };
    let Some(expect) = edit.expect.as_ref().filter(|_| edit.expect != edit.write) else {
        return Ok(Some(edit.swapped()));
    };
    if holds(expect)? {
        return Ok(Some(edit.swapped()));
    }
    not_restored.push(token);
    if let Some(write) = &edit.write
        && !holds(write)?
    {
        *blocked = true;
    }
    Ok(None)
}

pub(crate) fn folio_code(d: folio::Diagnostic) -> SessionError {
    match d.code {
        Code::Canceled => SessionError::Canceled,
        Code::BudgetExceeded => SessionError::Provider("budget_exceeded"),
        _ => SessionError::Provider("folio_rejected"),
    }
}

/// `true` when the held bytes quote `expect`; bytes that do not parse and a repeated token never equal
/// an expectation, `None` included.
fn quote_matches<T: PartialEq>(
    held: &Result<Option<&[u8]>, folio::Diagnostic>,
    expect: Option<&T>,
    parse: impl Fn(&[u8]) -> Option<T>,
) -> bool {
    match held {
        Err(_) => false,
        Ok(None) => expect.is_none(),
        Ok(Some(bytes)) => parse(bytes).is_some_and(|live| Some(&live) == expect),
    }
}

/// Multicam's own promise holds on this path too: a removal may not orphan a multicam record set that
/// validates now. Corrupt or already stale records are not this step's to judge (they read as their
/// own diagnostic, `crate::multicam`).
pub(crate) fn keep_multicam_records_valid(
    multicam: Option<&[u8]>,
    document: &StudioDocument,
    successor: &StudioDocument,
    budget: folio::Budget,
    cancel: &CancellationToken,
) -> Result<(), SessionError> {
    let Some(records) = multicam.and_then(|bytes| serde_json::from_slice::<StudioMulticamRecords>(bytes).ok()) else {
        return Ok(());
    };
    if validate_multicam_records(&records, document, budget, cancel).is_err() {
        return Ok(());
    }
    validate_multicam_records(&records, successor, budget, cancel).map_err(folio_code)
}

/// Checks `step` against the live document and entries; `Ok(None)` when it changes nothing. Nothing
/// is published here. Every body it would store is parsed back through the check an open applies, so
/// a body over the Folio input budget is refused here (`budget_exceeded`) rather than at reopen.
pub(crate) fn successor(
    snapshot: &Snapshot,
    live: LiveEntries<'_>,
    step: &SequenceRecordsStep,
    budget: folio::Budget,
    resolver: &dyn Resolver,
    cancel: &CancellationToken,
) -> Result<Option<Checked>, SessionError> {
    if !valid_command_id(&step.command_id) {
        return Err(SessionError::Provider("invalid_command"));
    }
    let edited = multicam_step::successor(snapshot, None, &step.sequence_part(), budget, resolver, cancel)?;
    if let Some(edit) = &step.settings
        && !quote_matches(&live.settings, edit.expect.as_ref(), |b| serde_json::from_slice(b).ok())
    {
        return Err(SessionError::Provider("value_conflict"));
    }
    if let Some(edit) = &step.presets
        && !quote_matches(&live.presets, edit.expect.as_ref(), |b| serde_json::from_slice(b).ok())
    {
        return Err(SessionError::Provider("value_conflict"));
    }
    if let Some(edit) = &step.track_display
        && !quote_matches(&live.track_display, edit.expect.as_ref(), |b| serde_json::from_slice(b).ok())
    {
        return Err(SessionError::Provider("value_conflict"));
    }
    let settings_changed = step.settings.as_ref().is_some_and(|e| e.expect != e.write);
    let presets_changed = step.presets.as_ref().is_some_and(|e| e.expect != e.write);
    let track_display_changed = step.track_display.as_ref().is_some_and(|e| e.expect != e.write);
    if edited.is_none() && !settings_changed && !presets_changed && !track_display_changed {
        return Ok(None);
    }
    let document = snapshot.document();
    let (next, inverse_sequences) = match edited {
        Some(checked) => (checked.successor, checked.inverse.sequences),
        None => {
            let mut next = document.clone();
            next.revision = document.revision.checked_add(1).ok_or(SessionError::Provider("revision_overflow"))?;
            (folio::validate_document(next, budget, cancel, resolver).map_err(folio_code)?, Vec::new())
        }
    };
    if step.sequences.iter().any(|e| matches!(e, SequenceEdit::Remove { .. })) {
        keep_multicam_records_valid(live.multicam, document, next.document(), budget, cancel)?;
    }
    let invalid = |_| SessionError::Provider("invalid_input");
    // A record set the step keeps is checked like one it writes: quoting invalid records unchanged is
    // using them.
    let mut entries = Vec::new();
    if let Some(edit) = &step.settings {
        let body = match &edit.write {
            None => None,
            Some(records) => {
                let body = serde_json::to_vec(records).map_err(invalid)?;
                parse_sequence_settings_records(&body, next.document(), budget, cancel).map_err(folio_code)?;
                Some(body)
            }
        };
        if settings_changed {
            entries.push((SEQUENCE_SETTINGS_ENTRY, body));
        }
    }
    if let Some(edit) = &step.presets {
        let body = match &edit.write {
            None => None,
            Some(presets) => {
                let body = serde_json::to_vec(presets).map_err(invalid)?;
                parse_sequence_presets(&body, budget, cancel).map_err(folio_code)?;
                Some(body)
            }
        };
        if presets_changed {
            entries.push((SEQUENCE_PRESETS_ENTRY, body));
        }
    }
    if let Some(edit) = &step.track_display {
        let body = match &edit.write {
            None => None,
            Some(records) => {
                let body = serde_json::to_vec(records).map_err(invalid)?;
                parse_track_display_records(&body, next.document(), budget, cancel).map_err(folio_code)?;
                Some(body)
            }
        };
        if track_display_changed {
            entries.push((TRACK_DISPLAY_ENTRY, body));
        }
    }
    // What the undo restores is checked against the document it restores.
    let (mut not_restored, mut blocked) = (Vec::new(), false);
    let reencoded = |_: serde_json::Error| folio::Diagnostic { code: Code::InvalidJson, target: "sequence_records".to_owned() };
    let settings = match &step.settings {
        Some(edit) => inverse_half(edit, SEQUENCE_SETTINGS_ENTRY, &mut not_restored, &mut blocked, |records| {
            let body = serde_json::to_vec(records).map_err(reencoded)?;
            parse_sequence_settings_records(&body, document, budget, cancel).map(drop)
        })?,
        None => None,
    };
    let presets = match &step.presets {
        Some(edit) => inverse_half(edit, SEQUENCE_PRESETS_ENTRY, &mut not_restored, &mut blocked, |records| {
            let body = serde_json::to_vec(records).map_err(reencoded)?;
            parse_sequence_presets(&body, budget, cancel).map(drop)
        })?,
        None => None,
    };
    let track_display = match &step.track_display {
        Some(edit) => inverse_half(edit, TRACK_DISPLAY_ENTRY, &mut not_restored, &mut blocked, |records| {
            let body = serde_json::to_vec(records).map_err(reencoded)?;
            parse_track_display_records(&body, document, budget, cancel).map(drop)
        })?,
        None => None,
    };
    let restores_something = !inverse_sequences.is_empty()
        || settings.as_ref().is_some_and(|e| e.expect != e.write)
        || presets.as_ref().is_some_and(|e| e.expect != e.write)
        || track_display.as_ref().is_some_and(|e| e.expect != e.write);
    let (inverse, undo) = if not_restored.is_empty() {
        (true, SequenceRecordsUndo::Complete)
    } else if blocked || !restores_something {
        (false, SequenceRecordsUndo::Unavailable(not_restored))
    } else {
        (true, SequenceRecordsUndo::WithoutEntries(not_restored))
    };
    Ok(Some(Checked {
        successor: next,
        entries,
        inverse: inverse.then(|| SequenceRecordsStep {
            command_id: step.command_id.clone(),
            correlation_id: step.correlation_id,
            sequences: inverse_sequences,
            settings,
            presets,
            track_display,
        }),
        undo,
    }))
}

fn command_error(error: StudioSequenceSettingsError) -> SessionError {
    use StudioSequenceSettingsError as E;
    match error {
        E::StaleRevision { expected, current } => SessionError::StaleRevision { expected, current },
        E::Canceled => SessionError::Canceled,
        other => SessionError::Provider(other.code()),
    }
}

/// What `save_from_sequence` and `apply` look a preset up in: the stored presets, then every shipped
/// preset no stored preset shadows. `set` consults no preset, so it is given an empty set.
fn library(command: &StudioSequenceSettingsCommand, stored: Option<&StudioSequencePresetSet>) -> StudioSequencePresetSet {
    if matches!(command, StudioSequenceSettingsCommand::SetEditingMode { .. }) {
        return StudioSequencePresetSet::new();
    }
    let mut all = stored.cloned().unwrap_or_default();
    let shipped: Vec<_> = shipped_sequence_presets()
        .presets
        .into_iter()
        .filter(|p| stored.is_none_or(|s| s.preset(&p.preset_id).is_none()))
        .collect();
    all.presets.extend(shipped);
    all
}

/// Runs the Folio command on the live state under the caller's `context` and returns the step it
/// amounts to, with the Folio report (`presets` counts the stored presets the step leaves). An invalid
/// entry the command reads refuses it: all three read the settings, `save` and `apply` the presets.
pub(crate) fn command_step(
    document: &StudioDocument,
    live: LiveEntries<'_>,
    context: &CommandContext,
    revision: u64,
    command: &StudioSequenceSettingsCommand,
    budget: folio::Budget,
    cancel: &CancellationToken,
) -> Result<(SequenceRecordsStep, StudioSequenceSettingsReport), SessionError> {
    let held_settings = live
        .settings
        .and_then(|held| held.map(|bytes| parse_sequence_settings_records(bytes, document, budget, cancel)).transpose())
        .map_err(folio_code)?;
    let reads_presets = !matches!(command, StudioSequenceSettingsCommand::SetEditingMode { .. });
    let held_presets = match live.presets.and_then(|held| held.map(|bytes| parse_sequence_presets(bytes, budget, cancel)).transpose()) {
        Ok(presets) => presets,
        Err(d) if reads_presets || d.code == Code::Canceled => return Err(folio_code(d)),
        Err(_) => None,
    };
    let base = held_settings.clone().unwrap_or_else(|| StudioSequenceSettingsRecords::new(document.document_id.clone()));
    let outcome = apply_sequence_settings_command(
        document,
        &base,
        &library(command, held_presets.as_ref()),
        context,
        revision,
        command,
        budget,
        cancel,
    )
    .map_err(command_error)?;
    let sequences = document
        .sequences
        .iter()
        .zip(&outcome.document.sequences)
        .filter(|(before, after)| before != after)
        .map(|(before, after)| SequenceEdit::Replace { expect: before.clone(), write: after.clone() })
        .collect();
    let settings = (outcome.records != base).then_some(RecordEdit { expect: held_settings, write: Some(outcome.records) });
    // Only a save changes the set, by appending its new preset, and only the stored presets are kept.
    let added = matches!(command, StudioSequenceSettingsCommand::SavePresetFromSequence { .. })
        .then(|| outcome.presets.presets.into_iter().next_back())
        .flatten();
    let stored_count = held_presets.as_ref().map_or(0, |p| p.presets.len());
    let mut report = outcome.report;
    report.presets = stored_count.saturating_add(usize::from(added.is_some()));
    let presets = added.map(|preset| {
        let mut write = held_presets.clone().unwrap_or_default();
        write.presets.push(preset);
        RecordEdit { expect: held_presets, write: Some(write) }
    });
    Ok((
        SequenceRecordsStep {
            command_id: command.command_id().to_owned(),
            correlation_id: context.correlation().value(),
            sequences,
            settings,
            presets,
            track_display: None,
        },
        report,
    ))
}

fn track_display_error(error: StudioTrackDisplayError) -> SessionError {
    use StudioTrackDisplayError as E;
    match error {
        E::StaleRevision { expected, current } => SessionError::StaleRevision { expected, current },
        E::Canceled => SessionError::Canceled,
        other => SessionError::Provider(other.code()),
    }
}

/// Runs the Folio track display command on the live entry under the caller's `context` and returns the
/// step it amounts to, with the Folio report. An invalid entry refuses it.
pub(crate) fn track_display_step(
    document: &StudioDocument,
    held: Result<Option<&[u8]>, folio::Diagnostic>,
    context: &CommandContext,
    revision: u64,
    command: &StudioTrackDisplayCommand,
    budget: folio::Budget,
    cancel: &CancellationToken,
) -> Result<(SequenceRecordsStep, StudioTrackDisplayReport), SessionError> {
    let held = held
        .and_then(|held| held.map(|bytes| parse_track_display_records(bytes, document, budget, cancel)).transpose())
        .map_err(folio_code)?;
    let base = held.clone().unwrap_or_else(|| StudioTrackDisplayRecords::new(document.document_id.clone()));
    let outcome = apply_track_display_command(document, &base, context, revision, command, budget, cancel).map_err(track_display_error)?;
    let track_display = outcome.report.changed.then_some(RecordEdit { expect: held, write: Some(outcome.records) });
    Ok((
        SequenceRecordsStep {
            command_id: command.command_id().to_owned(),
            correlation_id: context.correlation().value(),
            sequences: Vec::new(),
            settings: None,
            presets: None,
            track_display,
        },
        outcome.report,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_observe::Outcome;

    /// F-R55-6: Folio's command errors keep their meaning at the session boundary. A cancelled command
    /// is the session's `Canceled` (Observe outcome `Canceled`, not a validation failure). Folio can
    /// only answer it when the token is set mid-command, after the session's own check, so the mapping
    /// is proven here.
    #[test]
    fn folio_command_errors_keep_their_meaning_at_the_session_boundary_r55_f6() {
        use StudioSequenceSettingsError as E;
        assert_eq!(command_error(E::Canceled), SessionError::Canceled);
        assert_eq!(command_error(E::Canceled).outcome(), Outcome::Canceled);
        assert_eq!(command_error(E::BudgetExceeded).code(), "budget_exceeded");
        assert_eq!(command_error(E::InvalidInput(Code::InvalidField)).code(), "invalid_input");
        assert_eq!(command_error(E::StaleRevision { expected: 1, current: 2 }), SessionError::StaleRevision { expected: 1, current: 2 });
        assert_eq!(command_error(E::PresetMissing).code(), "preset_missing");
    }

    /// The same for the track display commands, whose cancel can only be set mid-command too.
    #[test]
    fn track_display_errors_keep_their_meaning_at_the_session_boundary_r55_f6() {
        use StudioTrackDisplayError as E;
        assert_eq!(track_display_error(E::Canceled), SessionError::Canceled);
        assert_eq!(track_display_error(E::Canceled).outcome(), Outcome::Canceled);
        assert_eq!(track_display_error(E::BudgetExceeded).code(), "budget_exceeded");
        assert_eq!(track_display_error(E::InvalidInput(Code::InvalidField)).code(), "invalid_input");
        assert_eq!(track_display_error(E::StaleRevision { expected: 1, current: 2 }), SessionError::StaleRevision { expected: 1, current: 2 });
        assert_eq!(track_display_error(E::InvalidHeight).code(), "invalid_height");
    }
}
