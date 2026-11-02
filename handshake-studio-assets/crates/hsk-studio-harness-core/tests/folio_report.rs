//! Lane SMB-R53-FOLLOWUPS-1, STUDIO-R53 F-R53-1 (HOST INTEGRATION rule 5): the host adapter emits
//! every terminal outcome of Folio's swatch-group and colour-book entries through observe as codes,
//! counters and the opaque correlation only. Oracle: observe's version-2 detail wire (`detail_wire`
//! in observe's DESCRIPTOR) decoded byte by byte, and the results the same Folio calls return without
//! an adapter. Every swatch, group and book name here is invented (no branded book data).
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_folio as folio;
use hsk_studio_harness_core::folio_report::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Error as ObserveError, Observe, SinkPort};
use serde_json::{Value, json};

const CORRELATION: u64 = 41;
const REVISION: u64 = 7;
// observe detail_wire enumerations.
const OUTCOME_SUCCESS: u8 = 2;
const OUTCOME_FAILURE: u8 = 3;
const OUTCOME_CANCELED: u8 = 4;
const FAIL_NONE: u8 = 0;
const FAIL_VALIDATION: u8 = 1;
const FAIL_UNAVAILABLE: u8 = 2;
const FAIL_UNSUPPORTED: u8 = 4;
const DISP_CHANGED: u8 = 1;
const DISP_NO_CHANGE: u8 = 2;
const DISP_REJECTED: u8 = 3;
const DC_NONE: u8 = 0;
const DC_INVALID_TARGET: u8 = 7;
const DC_REVISION_CONFLICT: u8 = 12;
const DC_CANCELED: u8 = 16;
const DC_VALIDATION_REJECTED: u8 = 17;

// Invented names; none may reach a frame.
const SWATCH_NAMES: [&str; 4] = ["Quillfrond Amber", "Quillfrond Slate", "Quillfrond Moss", "Quillfrond Ash"];
const GROUP_NAMES: [&str; 3] = ["Mossgrove Root", "Mossgrove Warm", "Mossgrove Cool"];
const GROUP_IDS: [&str; 3] = ["glenroot", "glenwarm", "glencool"];
const BOOK_ID: &str = "zibbleshade-inks";
const BOOK_TITLE: &str = "Zibbleshade Test Inks";
const ENTRY_NAMES: [&str; 2] = ["Zibble Dusk", "Zibble Fern"];
const ENTRY_CODES: [&str; 2] = ["ZD-77", "ZF-88"];

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}
fn ctx(command: &str) -> CommandContext {
    CommandContext::new(actor(), ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), REVISION, CorrelationId::new(CORRELATION), None)
}
fn swatch(n: usize, group: Option<&str>) -> Value {
    let mut v = json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",n as u32 + 1),"name":SWATCH_NAMES[n],"swatch_kind":"process","color_model":"RGB","components":[0.125,0.375,0.625]});
    if let Some(g) = group {
        v["group_ref"] = json!(g);
    }
    v
}
/// Root group, two named groups; swatch 0 in the root, 1 and 2 in the warm group, 3 in the cool group.
fn records() -> folio::StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "swatches":[swatch(0,None),swatch(1,Some(GROUP_IDS[1])),swatch(2,Some(GROUP_IDS[1])),swatch(3,Some(GROUP_IDS[2]))],
        "swatch_groups":[{"group_id":GROUP_IDS[0],"name":GROUP_NAMES[0],"root":true},{"group_id":GROUP_IDS[1],"name":GROUP_NAMES[1]},{"group_id":GROUP_IDS[2],"name":GROUP_NAMES[2]}]}))
    .unwrap()
}
fn book_bytes(state: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_id":"hsk.studio.colour_book@1","book_id":BOOK_ID,"title":BOOK_TITLE,"name_prefix":"","name_postfix":"",
        "description":"synthetic test book","declared_count":2,"color_space":"lab","components_per_color":3,"ink_type":"spot",
        "page_size":2,"page_selector_offset":0,"license_state":state,"license_note":"invented for tests",
        "entries":[{"name":ENTRY_NAMES[0],"code":ENTRY_CODES[0],"components":[41.5,12.25,-33.75]},{"name":ENTRY_NAMES[1],"code":ENTRY_CODES[1],"components":[62.0,-21.5,18.0]}]}))
    .unwrap()
}

struct Collect {
    frames: Vec<(DeliveryClass, Vec<u8>)>,
    fail: Option<DeliveryError>,
}
impl SinkPort for Collect {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if let Some(e) = self.fail {
            return Err(e);
        }
        self.frames.push((class, bytes.to_vec()));
        Ok(())
    }
}
fn emitter() -> Observe {
    Observe::new(CORRELATION, REVISION, DomainId::parse(&identity("SDOC", 1)).unwrap(), actor(), Budget::new(0, 0).unwrap())
}

/// One emit_detail frame (version 2).
#[derive(Debug, PartialEq)]
struct Wire {
    version: u8,
    outcome: u8,
    failure: u8,
    correlation: u64,
    revision: u64,
    disposition: u8,
    code: u8,
    counts: [u32; 5],
}
fn decode(bytes: &[u8]) -> Wire {
    assert_eq!(&bytes[..4], b"HSKO");
    let (version, outcome, failure) = (bytes[4], bytes[5], bytes[6]);
    let correlation = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let revision = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let mut p = 8 + 5 * 8;
    for _ in 0..7 {
        let n = usize::from(u16::from_be_bytes([bytes[p], bytes[p + 1]]));
        p += 2 + n;
    }
    let (disposition, code, address) = (bytes[p], bytes[p + 1], bytes[p + 2]);
    assert_eq!(address, 0, "no address on a folio report");
    p += 3;
    let mut counts = [0u32; 5];
    for c in &mut counts {
        *c = u32::from_be_bytes(bytes[p..p + 4].try_into().unwrap());
        p += 4;
    }
    assert_eq!(p, bytes.len(), "no trailing bytes");
    Wire { version, outcome, failure, correlation, revision, disposition, code, counts }
}
fn wire(outcome: u8, failure: u8, disposition: u8, code: u8, counts: [u32; 5]) -> Wire {
    Wire { version: 2, outcome, failure, correlation: CORRELATION, revision: REVISION, disposition, code, counts }
}

/// Runs `f` with a fresh emitter and collecting sink; returns its value, the emission and the frames.
fn observed<R>(cancel: &CancellationToken, fail: Option<DeliveryError>, f: impl FnOnce(FolioReportRoute<'_>) -> (R, FolioEmission)) -> (R, FolioEmission, Vec<(DeliveryClass, Vec<u8>)>) {
    let (mut observe, mut sink) = (emitter(), Collect { frames: Vec::new(), fail });
    let (r, e) = f(FolioReportRoute { cancel, observe: &mut observe, sink: &mut sink });
    (r, e, sink.frames)
}
/// Exactly one terminal frame, decoded.
fn one_terminal(frames: &[(DeliveryClass, Vec<u8>)]) -> Wire {
    assert_eq!(frames.len(), 1, "exactly one terminal event");
    assert_eq!(frames[0].0, DeliveryClass::Terminal);
    decode(&frames[0].1)
}

/// Group commands and undo: one terminal each, the command id as code, the counters of the report.
#[test]
fn swatch_group_commands_emit_one_terminal_with_code_and_counters() {
    let cancel = CancellationToken::default();
    let r = records();
    let run = |cmd: folio::StudioSwatchGroupCommand, palette: &folio::StudioVectorRecords| {
        observed(&cancel, None, |route| apply_swatch_group_command_observed(palette, &ctx(cmd.command_id()), REVISION, &cmd, route))
    };
    // Create: a new named group, nothing moved.
    let create = folio::StudioSwatchGroupCommand::Create { group_id: "glenspare".into(), name: "Mossgrove Spare".into(), index: None };
    let (res, em, frames) = run(create, &r);
    let created = res.expect("create applies");
    assert_eq!(em.code, "swatch.group.create");
    assert!(em.receipt.is_ok(), "{:?}", em.receipt);
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [4, 0, 4, 0, 0]));
    assert_eq!(created.report.groups, 4);

    // Delete the warm group into the root: two swatches move.
    let delete = folio::StudioSwatchGroupCommand::Delete { group_id: GROUP_IDS[1].into(), replace_with: GROUP_IDS[0].into() };
    let (res, em, frames) = run(delete, &r);
    let deleted = res.expect("delete applies");
    assert_eq!((em.code, deleted.report.swatches_moved), ("swatch.group.delete", 2));
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [2, 2, 2, 2, 2]));

    // A rename to the same name changes nothing: no_change, zero changed.
    let same = folio::StudioSwatchGroupCommand::Rename { group_id: GROUP_IDS[2].into(), name: GROUP_NAMES[2].into() };
    let (res, em, frames) = run(same, &r);
    assert_eq!(res.expect("rename applies").palette, r);
    assert_eq!(em.code, "swatch.group.rename");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_NO_CHANGE, DC_NONE, [3, 0, 3, 0, 0]));

    // Undo of the delete: one terminal with the undo command id.
    let (res, em, frames) = observed(&cancel, None, |route| undo_swatch_group_command_observed(&deleted.palette, &ctx("swatch.group.undo"), REVISION, &deleted.undo, route));
    assert_eq!(res.expect("undo applies").palette, r);
    assert_eq!(em.code, "swatch.group.undo");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [3, 2, 3, 2, 2]));
}

/// Refusals: one rejected terminal with the Folio code's class and zero counters.
#[test]
fn swatch_group_refusals_emit_one_rejected_terminal() {
    let cancel = CancellationToken::default();
    let r = records();
    let rename = folio::StudioSwatchGroupCommand::Rename { group_id: GROUP_IDS[1].into(), name: "Mossgrove Renamed".into() };
    // Stale revision: the caller's revision differs from the context's base.
    let (res, em, frames) = observed(&cancel, None, |route| apply_swatch_group_command_observed(&r, &ctx(rename.command_id()), REVISION + 1, &rename, route));
    assert!(matches!(res, Err(folio::StudioSwatchGroupError::StaleRevision { .. })));
    assert_eq!(em.code, "stale_revision");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_FAILURE, FAIL_UNAVAILABLE, DISP_REJECTED, DC_REVISION_CONFLICT, [0; 5]));
    // The read-only system palette.
    let mut system = folio::StudioSwatchPalette::new("glensystem", "Mossgrove System", folio::StudioPaletteScope::System);
    system.swatches = r.swatches.clone();
    system.swatch_groups = r.swatch_groups.clone();
    let (res, em, frames) = observed(&cancel, None, |route| apply_swatch_group_command_observed(&system, &ctx(rename.command_id()), REVISION, &rename, route));
    assert_eq!(res.unwrap_err(), folio::StudioSwatchGroupError::ReadOnlyScope);
    assert_eq!(em.code, "read_only_scope");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_FAILURE, FAIL_UNSUPPORTED, DISP_REJECTED, DC_INVALID_TARGET, [0; 5]));
}

/// Book library changes and population: one terminal each; refusals and cancellation typed.
#[test]
fn colour_book_operations_emit_one_terminal_with_code_and_counters() {
    let cancel = CancellationToken::default();
    let mut library = folio::StudioColourBookLibrary::new();
    let bytes = book_bytes("operator_supplied");
    let (res, em, frames) = observed(&cancel, None, |route| {
        load_colour_book_observed(&mut library, &ctx("swatch.book.load"), &bytes, folio::StudioBookOrigin::OperatorSupplied, &cancel, route)
    });
    assert_eq!((res.expect("book loads").entries, em.code), (2, "book.load"));
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [1, 2, 1, 2, 2]));

    // Population: two swatches from the loaded book.
    let book = library.book(BOOK_ID).unwrap().clone();
    let picks = [(ENTRY_CODES[0], identity("SSWT", 20)), (ENTRY_CODES[1], identity("SSWT", 21))];
    let picks: Vec<(&str, &str)> = picks.iter().map(|(c, s)| (*c, s.as_str())).collect();
    let r = records();
    let (res, em, frames) = observed(&cancel, None, |route| add_colour_book_swatches_observed(&r, &ctx("swatch.book.add"), REVISION, &book, &picks, &cancel, route));
    assert_eq!(res.expect("swatches populate").records.swatches.len(), 6);
    assert_eq!(em.code, "swatch.book.add");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [1, 2, 1, 2, 2]));

    // Marked absent: look-up lost, no entries.
    let (res, em, frames) = observed(&cancel, None, |route| mark_colour_book_absent_observed(&mut library, &ctx("swatch.book.absent"), BOOK_ID, route));
    assert_eq!((res.expect("marked absent").books, em.code), (1, "book.mark_absent"));
    assert_eq!(one_terminal(&frames), wire(OUTCOME_SUCCESS, FAIL_NONE, DISP_CHANGED, DC_NONE, [1, 0, 1, 0, 0]));

    // An operator file declaring itself bundled is refused; the library keeps its state.
    let before = library.clone();
    let forged = book_bytes("bundled");
    let (res, em, frames) = observed(&cancel, None, |route| {
        load_colour_book_observed(&mut library, &ctx("swatch.book.load"), &forged, folio::StudioBookOrigin::OperatorSupplied, &cancel, route)
    });
    assert_eq!(res.unwrap_err(), folio::StudioColourBookError::LicenseStateMismatch);
    assert_eq!((em.code, &library), ("license_state_mismatch", &before));
    assert_eq!(one_terminal(&frames), wire(OUTCOME_FAILURE, FAIL_VALIDATION, DISP_REJECTED, DC_VALIDATION_REJECTED, [0; 5]));

    // A canceled load: the canceled outcome, still exactly one terminal.
    let canceled = CancellationToken::default();
    canceled.cancel();
    let (res, em, frames) = observed(&canceled, None, |route| {
        load_colour_book_observed(&mut library, &ctx("swatch.book.load"), &bytes, folio::StudioBookOrigin::OperatorSupplied, &canceled, route)
    });
    assert_eq!(res.unwrap_err(), folio::StudioColourBookError::Canceled);
    assert_eq!(em.code, "canceled");
    assert_eq!(one_terminal(&frames), wire(OUTCOME_CANCELED, FAIL_NONE, DISP_REJECTED, DC_CANCELED, [0; 5]));
}

/// Needle test: no swatch, group or book name, id, entry code or colour value reaches a frame.
#[test]
fn frames_carry_no_name_id_or_colour() {
    let cancel = CancellationToken::default();
    let r = records();
    let mut frames = Vec::new();
    for cmd in [
        folio::StudioSwatchGroupCommand::Rename { group_id: GROUP_IDS[1].into(), name: GROUP_NAMES[2].replace("Cool", "Dusk") },
        folio::StudioSwatchGroupCommand::MoveSwatch { swatch_id: identity("SSWT", 1), group_id: GROUP_IDS[2].into(), index: None },
        folio::StudioSwatchGroupCommand::Delete { group_id: GROUP_IDS[2].into(), replace_with: GROUP_IDS[1].into() },
        folio::StudioSwatchGroupCommand::Delete { group_id: GROUP_IDS[0].into(), replace_with: GROUP_IDS[1].into() },
    ] {
        let (_, _, f) = observed(&cancel, None, |route| apply_swatch_group_command_observed(&r, &ctx(cmd.command_id()), REVISION, &cmd, route));
        frames.extend(f);
    }
    let mut library = folio::StudioColourBookLibrary::new();
    let bytes = book_bytes("operator_supplied");
    let (_, _, f) = observed(&cancel, None, |route| load_colour_book_observed(&mut library, &ctx("swatch.book.load"), &bytes, folio::StudioBookOrigin::OperatorSupplied, &cancel, route));
    frames.extend(f);
    let book = library.book(BOOK_ID).unwrap().clone();
    let id = identity("SSWT", 30);
    let (_, _, f) = observed(&cancel, None, |route| add_colour_book_swatches_observed(&r, &ctx("swatch.book.add"), REVISION, &book, &[(ENTRY_CODES[0], id.as_str())], &cancel, route));
    frames.extend(f);
    let (_, _, f) = observed(&cancel, None, |route| mark_colour_book_absent_observed(&mut library, &ctx("swatch.book.absent"), BOOK_ID, route));
    frames.extend(f);
    assert_eq!(frames.len(), 7, "one terminal per operation");

    let mut needles: Vec<String> = Vec::new();
    needles.extend(SWATCH_NAMES.iter().map(|s| s.to_string()));
    needles.extend(GROUP_NAMES.iter().map(|s| s.to_string()));
    needles.extend(GROUP_IDS.iter().map(|s| s.to_string()));
    needles.extend(ENTRY_NAMES.iter().chain(ENTRY_CODES.iter()).map(|s| s.to_string()));
    needles.extend([BOOK_ID, BOOK_TITLE, "Quillfrond", "Mossgrove", "Zibble", "glen", "SSWT", "0.375", "41.5", "33.75"].map(String::from));
    for (_, bytes) in &frames {
        for n in &needles {
            assert!(!bytes.windows(n.len()).any(|w| w == n.as_bytes()), "frame carries {n:?}");
        }
        // The opaque correlation is the context's.
        assert_eq!(decode(bytes).correlation, CORRELATION);
    }
}

/// A rejected or failed delivery is reported in the emission and never changes Folio's result.
#[test]
fn emission_never_changes_the_result() {
    let cancel = CancellationToken::default();
    let r = records();
    let cmd = folio::StudioSwatchGroupCommand::Delete { group_id: GROUP_IDS[1].into(), replace_with: GROUP_IDS[2].into() };
    let direct = folio::apply_swatch_group_command(&r, &ctx(cmd.command_id()), REVISION, &cmd);
    for fail in [DeliveryError::Rejected, DeliveryError::Unavailable, DeliveryError::Indeterminate] {
        let (res, em, frames) = observed(&cancel, Some(fail), |route| apply_swatch_group_command_observed(&r, &ctx(cmd.command_id()), REVISION, &cmd, route));
        assert_eq!(res, direct, "{fail:?}");
        assert_eq!(em.code, "swatch.group.delete");
        assert_eq!(em.receipt, Err(ObserveError::Delivery(fail)));
        assert!(frames.is_empty());
    }
    // A refused book load: same refusal with or without a working sink.
    let mut a = folio::StudioColourBookLibrary::new();
    let mut b = folio::StudioColourBookLibrary::new();
    let forged = book_bytes("bundled");
    let direct = a.load(&ctx("swatch.book.load"), &forged, folio::StudioBookOrigin::OperatorSupplied, &cancel);
    let (res, em, _) = observed(&cancel, Some(DeliveryError::Rejected), |route| {
        load_colour_book_observed(&mut b, &ctx("swatch.book.load"), &forged, folio::StudioBookOrigin::OperatorSupplied, &cancel, route)
    });
    assert_eq!((res, &b), (direct, &a));
    assert_eq!(em.receipt, Err(ObserveError::Delivery(DeliveryError::Rejected)));
}
