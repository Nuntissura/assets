//! Lane SMB-R53-FOLLOWUPS-1, MT-31251 (STU-COL-188 through STU-COL-126) with the MT-31248 and
//! MT-31249 rows the join proves: an ICC named-colour profile parsed by Prism becomes a Folio colour
//! book under the operator's per-book licensing posture (STU-COL-186), and the swatches it populates
//! carry exactly Prism's measured values for the same entries. Prism is reached only with an import
//! feature that enables it (round unit: harness-core import-tiff, see the lane notes). Every profile
//! is built here from scratch with invented names (no branded book data).
#![cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, DomainId};
use hsk_studio_folio as folio;
use hsk_studio_harness_core::folio_report::FolioReportRoute;
use hsk_studio_harness_core::named_colour_book::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, Observe, SinkPort};
use hsk_studio_prism as prism;
use serde_json::json;

const PROFILE_ID: &str = "SCPF-019abcde-0000-7000-8000-0000000000c1";
const BOOK_ID: &str = "gribblet-test-inks";
const BOOK_TITLE: &str = "Gribblet Test Inks";

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn name32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..s.len()].copy_from_slice(s.as_bytes());
    out
}
/// The `ncl2` tag: prefix, suffix, then per colour a root name, legacy Lab words and CMYK device words.
fn ncl2(prefix: &str, suffix: &str, colours: &[(&str, [u16; 3])]) -> Vec<u8> {
    let mut t = b"ncl2\0\0\0\0".to_vec();
    t.extend_from_slice(&be32(0));
    t.extend_from_slice(&be32(colours.len() as u32));
    t.extend_from_slice(&be32(4));
    t.extend_from_slice(&name32(prefix));
    t.extend_from_slice(&name32(suffix));
    for (name, lab) in colours {
        t.extend_from_slice(&name32(name));
        for w in lab {
            t.extend_from_slice(&w.to_be_bytes());
        }
        for w in [65535u16, 0, 32768, 0] {
            t.extend_from_slice(&w.to_be_bytes());
        }
    }
    t
}
/// A v4 `nmcl` profile (CMYK data, Lab PCS) holding one `ncl2` tag.
fn profile(tag: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 128];
    b[8] = 4;
    b[12..16].copy_from_slice(b"nmcl");
    b[16..20].copy_from_slice(b"CMYK");
    b[20..24].copy_from_slice(b"Lab ");
    b[36..40].copy_from_slice(b"acsp");
    b.extend_from_slice(&be32(1));
    b.extend_from_slice(b"ncl2");
    b.extend_from_slice(&be32(144));
    b.extend_from_slice(&be32(tag.len() as u32));
    b.extend_from_slice(tag);
    let size = b.len() as u32;
    b[0..4].copy_from_slice(&be32(size));
    b
}
/// Two invented colours: L 50 / a +16 / b -32 and L 75 / a -8 / b +4.5 (legacy 16-bit Lab words).
fn colours() -> Vec<(&'static str, [u16; 3])> {
    vec![("Gribblet 01", [0x7F80, 0x9000, 0x6000]), ("Gribblet 02", [0xBF40, 0x7800, 0x8480])]
}
fn bytes_of(colours: &[(&str, [u16; 3])]) -> Vec<u8> {
    profile(&ncl2("GT ", " X", colours))
}
fn id() -> DomainId {
    DomainId::parse(PROFILE_ID).unwrap()
}
fn input<'a>(id: &'a DomainId, bytes: &'a [u8]) -> prism::ProfileInput<'a> {
    prism::ProfileInput { profile_id: id, bytes, expected_sha256: prism::profile_hash(bytes) }
}
fn spec(state: folio::StudioBookLicenseState) -> NamedColourBookSpec<'static> {
    NamedColourBookSpec { book_id: BOOK_ID, title: BOOK_TITLE, license_state: state, license_note: "synthetic test profile" }
}
fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}
fn ctx(command: &str) -> CommandContext {
    CommandContext::new(actor(), ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), 7, CorrelationId::new(43), None)
}
fn token() -> CancellationToken {
    CancellationToken::default()
}
fn sid(n: u32) -> String {
    format!("SSWT-019abcde-0000-7000-8000-{n:012x}")
}
fn document() -> folio::StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":7,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .unwrap()
}
fn empty_records() -> folio::StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001"})).unwrap()
}
use folio::StudioBookLicenseState as State;

/// MT-31251: the joined book is the profile's entries with Prism's measured Lab values unchanged and
/// the profile reference Prism verified; the swatches it populates (MT-31248) carry those values and
/// the displayed name Prism reports, and validate as document swatches.
#[test]
fn named_colour_profile_joins_into_a_bound_lab_spot_book() {
    let (id, bytes) = (id(), bytes_of(&colours()));
    // The profile engine's own output for the same profile.
    let table = prism::parse_named_colour_profile(input(&id, &bytes), &token()).unwrap();
    let book = colour_book_from_named_colour_profile(input(&id, &bytes), &spec(State::OperatorSupplied), &token()).unwrap();
    let profile_ref = book.named_colour_profile.clone().expect("bound to the profile");
    assert_eq!((profile_ref.profile_id.as_str(), profile_ref.content_digest.algorithm.as_str()), (PROFILE_ID, "sha256"));
    assert_eq!(profile_ref.content_digest.digest, prism::hash_hex(&prism::profile_hash(&bytes)));
    assert_eq!((book.color_space, book.ink_type, book.declared_count, book.page_count()), (folio::StudioBookColorSpace::Lab, folio::StudioBookInkType::Spot, 2, 1));
    assert_eq!((book.name_prefix.as_str(), book.name_postfix.as_str(), book.license_state), ("GT ", " X", State::OperatorSupplied));
    assert_eq!(book.entries.len(), table.entries.len());
    for (i, (entry, source)) in book.entries.iter().zip(&table.entries).enumerate() {
        assert_eq!((entry.name.as_str(), entry.code.as_str()), (source.root_name.as_str(), source.root_name.as_str()));
        assert_eq!(entry.components, source.pcs_lab.to_vec(), "measured Lab unchanged");
        assert_eq!(Some(book.display_name(entry)), table.display_name(i), "displayed name = Prism's prefix + root + suffix");
    }
    assert_eq!(book.entries[1].components, vec![75.0, -8.0, 4.5]);

    // Loaded with the operator origin: look-up works; populated spot swatches carry Prism's values.
    let mut library = folio::StudioColourBookLibrary::new();
    let report = load_named_colour_book(&mut library, &ctx("swatch.book.load"), input(&id, &bytes), &spec(State::OperatorSupplied), folio::StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    assert_eq!((report.operation, report.entries, report.books), ("book.load", 2, 1));
    assert_eq!(library.lookup(BOOK_ID, "Gribblet 02").unwrap(), &book.entries[1]);
    let loaded = library.book(BOOK_ID).unwrap();
    let picks = [("Gribblet 01", sid(1)), ("Gribblet 02", sid(2))];
    let picks: Vec<(&str, &str)> = picks.iter().map(|(c, s)| (*c, s.as_str())).collect();
    let out = folio::add_colour_book_swatches(&empty_records(), &ctx("swatch.book.add"), 7, loaded, &picks, &token()).unwrap();
    for (s, (i, source)) in out.records.swatches.iter().zip(table.entries.iter().enumerate()) {
        let ink = s.spot.as_ref().expect("a spot swatch");
        assert_eq!(s.swatch_kind, folio::StudioSwatchKind::Spot);
        assert_eq!(ink.measured_lab, Some(source.pcs_lab), "the swatch's measured Lab is the profile engine's entry output");
        assert_eq!(Some(s.name.clone()), table.display_name(i));
        assert_eq!(Some(ink.ink_name.clone()), table.display_name(i));
    }
    assert_eq!(out.records.colour_book_refs, [folio::StudioColourBookRef { book_id: BOOK_ID.into(), title: BOOK_TITLE.into(), license_state: State::OperatorSupplied }]);
    folio::validate_vector_records(&out.records, &document(), None, folio::Budget::default(), &token()).expect("populated swatches validate");
}

/// STU-COL-186 (MT-31249): bundled books may ship, operator-supplied ones load at run time and never
/// ship, a posture/origin mismatch is refused without change, and an absent book is only referenced:
/// the profile is not read, look-up is lost and stored swatches stay byte-identical.
#[test]
fn licensing_posture_decides_bundling_lookup_or_reference_only() {
    let (id, bytes) = (id(), bytes_of(&colours()));
    let load = |library: &mut folio::StudioColourBookLibrary, state: State, origin: folio::StudioBookOrigin, bytes: &[u8]| {
        load_named_colour_book(library, &ctx("swatch.book.load"), input(&id, bytes), &spec(state), origin, &token())
    };
    // Bundled posture, bundled origin: among the books a build may ship.
    let mut library = folio::StudioColourBookLibrary::new();
    load(&mut library, State::Bundled, folio::StudioBookOrigin::Bundled, &bytes).unwrap();
    assert_eq!(library.bundled_books().map(|b| b.book_id.as_str()).collect::<Vec<_>>(), [BOOK_ID]);

    // Operator-supplied posture: loaded from the operator's profile, look-up works, never bundled.
    let mut library = folio::StudioColourBookLibrary::new();
    load(&mut library, State::OperatorSupplied, folio::StudioBookOrigin::OperatorSupplied, &bytes).unwrap();
    assert_eq!(library.state(BOOK_ID), Some(State::OperatorSupplied));
    assert!(library.lookup(BOOK_ID, "Gribblet 01").is_ok());
    assert_eq!(library.bundled_books().count(), 0, "a book whose posture forbids bundling never ships");

    // Posture and origin must agree; a refusal leaves the library (and its last-good copy) unchanged.
    let before = library.clone();
    for (state, origin) in [(State::OperatorSupplied, folio::StudioBookOrigin::Bundled), (State::Bundled, folio::StudioBookOrigin::OperatorSupplied)] {
        assert_eq!(load(&mut library, state, origin, &bytes).unwrap_err(), NamedColourBookError::Book(folio::StudioColourBookError::LicenseStateMismatch), "{state:?}/{origin:?}");
        assert_eq!(library, before);
    }

    // Swatches populated while the book was available.
    let picks = [("Gribblet 02", sid(5))];
    let picks: Vec<(&str, &str)> = picks.iter().map(|(c, s)| (*c, s.as_str())).collect();
    let populated = folio::add_colour_book_swatches(&empty_records(), &ctx("swatch.book.add"), 7, library.book(BOOK_ID).unwrap(), &picks, &token()).unwrap().records;
    let stored = serde_json::to_vec(&populated).unwrap();

    // Absent posture: the profile is not read (these bytes are not even a profile); only the reference stays.
    let report = load(&mut library, State::Absent, folio::StudioBookOrigin::OperatorSupplied, b"not a profile").unwrap();
    assert_eq!((report.operation, report.entries), ("book.mark_absent", 0));
    assert_eq!(library.state(BOOK_ID), Some(State::Absent));
    assert_eq!(library.lookup(BOOK_ID, "Gribblet 02"), Err(folio::StudioBookLookupError::BookAbsent));
    assert_eq!(serde_json::to_vec(&populated).unwrap(), stored, "losing look-up changes no stored colour");
    folio::validate_vector_records(&populated, &document(), None, folio::Budget::default(), &token()).expect("references and stored swatches stay valid");
    assert_eq!(
        colour_book_from_named_colour_profile(input(&id, &bytes), &spec(State::Absent), &token()).unwrap_err(),
        NamedColourBookError::Book(folio::StudioColourBookError::LicenseStateMismatch),
        "an absent book is recorded, never built"
    );
}

/// V4-01/02 through the join: Prism's bounds (dims u32::MAX, truncation, identity) and Folio's book
/// rules (duplicate codes, Lab range, empty names) refuse typed; the largest profile Prism admits
/// fits Folio's book and resource budgets; a full library refuses a new book (aggregate) unchanged.
#[test]
fn hostile_profiles_and_full_libraries_are_refused_typed_within_budgets() {
    let id = id();
    let build = |bytes: &[u8]| colour_book_from_named_colour_profile(input(&id, bytes), &spec(State::OperatorSupplied), &token());
    let good = ncl2("", "", &colours());
    let mut count_max = good.clone();
    count_max[12..16].copy_from_slice(&be32(u32::MAX));
    assert_eq!(build(&profile(&count_max)).unwrap_err(), NamedColourBookError::Profile(prism::NamedColourError::CountLimit), "count u32::MAX");
    assert_eq!(build(&profile(&good[..good.len() - 3])).unwrap_err(), NamedColourBookError::Profile(prism::NamedColourError::Truncated));
    let bytes = bytes_of(&colours());
    let forged = prism::ProfileInput { profile_id: &id, bytes: &bytes, expected_sha256: [7; 32] };
    assert!(matches!(colour_book_from_named_colour_profile(forged, &spec(State::OperatorSupplied), &token()), Err(NamedColourBookError::Profile(prism::NamedColourError::Wide(_)))));
    // Folio's book rules on what Prism decoded.
    let dup = [("Gribblet 01", [0x7F80, 0x8000, 0x8000]), ("Gribblet 01", [0x7F80, 0x8000, 0x8000])];
    assert_eq!(build(&bytes_of(&dup)).unwrap_err(), NamedColourBookError::Book(folio::StudioColourBookError::DuplicateEntry));
    let bright = [("Gribblet 09", [0xFFFF, 0x8000, 0x8000])];
    assert_eq!(build(&bytes_of(&bright)).unwrap_err(), NamedColourBookError::Book(folio::StudioColourBookError::InvalidComponents), "L* 100.39 is outside the Studio Lab range");
    let unnamed = [("", [0x7F80, 0x8000, 0x8000])];
    assert_eq!(build(&bytes_of(&unnamed)).unwrap_err(), NamedColourBookError::Book(folio::StudioColourBookError::InvalidField));

    // The largest profile Prism admits (16384 colours, 31-character names) joins within the 4 MiB book resource.
    let names: Vec<String> = (0..prism::MAX_NAMED_COLOURS).map(|i| format!("Gribblet-long-test-name-{i:06}")).collect();
    assert!(names.iter().all(|n| n.len() == 30));
    let many: Vec<(&str, [u16; 3])> = names.iter().map(|n| (n.as_str(), [0x7F80, 0x8000, 0x8000])).collect();
    let big = bytes_of(&many);
    let mut library = folio::StudioColourBookLibrary::new();
    let report = load_named_colour_book(&mut library, &ctx("swatch.book.load"), input(&id, &big), &spec(State::OperatorSupplied), folio::StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    assert_eq!(report.entries, folio::BOOK_MAX_ENTRIES);

    // Aggregate: a library holding its 256 books refuses one more, unchanged.
    let mut full = folio::StudioColourBookLibrary::new();
    for i in 0..folio::BOOK_LIBRARY_MAX_BOOKS {
        full.mark_absent(&ctx("swatch.book.absent"), &format!("gribblet-{i}")).unwrap();
    }
    let before = full.clone();
    let err = load_named_colour_book(&mut full, &ctx("swatch.book.load"), input(&id, &bytes), &spec(State::OperatorSupplied), folio::StudioBookOrigin::OperatorSupplied, &token()).unwrap_err();
    assert_eq!(err, NamedColourBookError::Book(folio::StudioColourBookError::BudgetExceeded));
    assert_eq!(full, before);
}

struct Collect(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for Collect {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

/// The observed join: one terminal per outcome with its code, no book or colour name in a frame.
#[test]
fn joined_book_outcomes_emit_one_terminal_each() {
    let (id, bytes) = (id(), bytes_of(&colours()));
    let truncated = profile(&ncl2("", "", &colours())[..60]);
    let mut library = folio::StudioColourBookLibrary::new();
    let cases: [(State, &[u8], &str, u8); 3] =
        [(State::OperatorSupplied, &bytes, "book.load", 2), (State::OperatorSupplied, &truncated, "named_colour_truncated", 3), (State::Absent, b"", "book.mark_absent", 2)];
    for (state, profile_bytes, code, outcome) in cases {
        let cancel = token();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let mut observe = Observe::new(43, 7, resource, actor(), Budget::new(0, 0).unwrap());
        let mut sink = Collect(Vec::new());
        let route = FolioReportRoute { cancel: &cancel, observe: &mut observe, sink: &mut sink };
        let (result, emission) =
            load_named_colour_book_observed(&mut library, &ctx("swatch.book.load"), input(&id, profile_bytes), &spec(state), folio::StudioBookOrigin::OperatorSupplied, &cancel, route);
        assert_eq!(emission.code, code);
        assert_eq!(result.is_ok(), outcome == 2, "{code}");
        assert!(emission.receipt.is_ok(), "{:?}", emission.receipt);
        assert_eq!(sink.0.len(), 1, "exactly one terminal for {code}");
        let frame = &sink.0[0].1;
        assert_eq!((sink.0[0].0, frame[5]), (DeliveryClass::Terminal, outcome), "{code}");
        for needle in ["Gribblet", BOOK_ID, BOOK_TITLE, "GT ", PROFILE_ID] {
            assert!(!frame.windows(needle.len()).any(|w| w == needle.as_bytes()), "{code}: frame carries {needle:?}");
        }
    }
}
