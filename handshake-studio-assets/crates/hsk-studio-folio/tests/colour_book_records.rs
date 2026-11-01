//! Lane SMB-COL-SWATCH-GROUPS-1, MT-31248..MT-31251 (STU-COL-185..188): the colour-book resource,
//! per-book licensing posture, captured coverage as planning data and the named-colour binding.
//! Every book here is synthetic with invented names (no branded book data is committed).
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn token() -> CancellationToken {
    CancellationToken::default()
}
fn document() -> StudioDocument {
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":5,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[],"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
/// A synthetic Lab spot book: seven invented entries on pages of three.
fn book(state: &str) -> Value {
    let entries: Vec<Value> = (1..=7).map(|i| json!({"name":format!("Quuxberry {i:02}"),"code":format!("QX{i:02}"),"components":[40.0 + f64::from(i), -10.0, 12.5]})).collect();
    json!({"schema_id":"hsk.studio.colour_book@1","book_id":"glimmerbrook-test","title":"Glimmerbrook Test Inks",
        "name_prefix":"GTI ","name_postfix":" fake","description":"(c) Nobody Real Test Fixture Co. Invented for tests.",
        "declared_count":7,"color_space":"lab","components_per_color":3,"ink_type":"spot","page_size":3,"page_selector_offset":1,
        "license_state":state,"license_note":"synthetic fixture","entries":entries})
}
fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}
fn parse(v: &Value, origin: StudioBookOrigin) -> Result<StudioColourBook, StudioColourBookError> {
    parse_colour_book(&bytes(v), origin, &token())
}
fn edit(f: &dyn Fn(&mut Value)) -> Result<StudioColourBook, StudioColourBookError> {
    let mut v = book("operator_supplied");
    f(&mut v);
    parse(&v, StudioBookOrigin::OperatorSupplied)
}
fn ctx(command: &str, revision: u64) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), revision, CorrelationId::new(77), None)
}
fn validate(r: &StudioVectorRecords) -> Result<(), Code> {
    validate_vector_records(r, &document(), None, Budget::default(), &token()).map_err(|d| d.code)
}

#[test]
fn book_resource_carries_every_field_and_pages_like_a_fan_deck() {
    let b = parse(&book("operator_supplied"), StudioBookOrigin::OperatorSupplied).unwrap();
    // Every STU-COL-185 field survives a round trip, the copyright text verbatim.
    assert_eq!(serde_json::to_value(&b).unwrap(), book("operator_supplied"));
    assert_eq!(b.description, "(c) Nobody Real Test Fixture Co. Invented for tests.");
    assert_eq!(b.display_name(&b.entries[0]), "GTI Quuxberry 01 fake");
    // Fan-deck pages: 7 entries on pages of 3 -> 3 pages; the page selector shows entry 1 of each.
    assert_eq!(b.page_count(), 3);
    assert_eq!(b.page(2).iter().map(|e| e.code.as_str()).collect::<Vec<_>>(), vec!["QX07"]);
    assert_eq!(b.page_selector_entry(0).map(|e| e.code.as_str()), Some("QX02"));
    assert_eq!(b.page_selector_entry(2), None, "the last page has no entry at offset 1");
    assert!(b.page(9).is_empty());
    // Empty prefix and postfix are legal.
    let plain = edit(&|v| { v["name_prefix"] = json!(""); v["name_postfix"] = json!(""); }).unwrap();
    assert_eq!(plain.display_name(&plain.entries[0]), "Quuxberry 01");
    // A CMYK process book populates process swatches.
    let cmyk = edit(&|v| {
        v["color_space"] = json!("cmyk");
        v["components_per_color"] = json!(4);
        v["ink_type"] = json!("process");
        for e in v["entries"].as_array_mut().unwrap() {
            e["components"] = json!([0.1, 0.2, 0.3, 0.0]);
        }
    })
    .unwrap();
    let s = cmyk.swatch_for(&cmyk.entries[0], &identity("SSWT", 1));
    assert_eq!((s.swatch_kind, s.color_model, s.components.clone()), (StudioSwatchKind::Process, Some(StudioColorModel::Cmyk), vec![0.1, 0.2, 0.3, 0.0]));
}

#[test]
fn book_resource_refuses_hostile_shapes_atomically() {
    use StudioColourBookError as E;
    assert_eq!(edit(&|v| v["declared_count"] = json!(8)).unwrap_err(), E::CountMismatch);
    assert_eq!(edit(&|v| v["components_per_color"] = json!(4)).unwrap_err(), E::ArityMismatch);
    assert_eq!(edit(&|v| v["entries"][3]["components"] = json!([50.0, 0.0])).unwrap_err(), E::ArityMismatch);
    assert_eq!(edit(&|v| v["entries"][3]["components"] = json!([150.0, 0.0, 0.0])).unwrap_err(), E::InvalidComponents);
    assert_eq!(edit(&|v| v["entries"][3]["code"] = json!("QX01")).unwrap_err(), E::DuplicateEntry);
    assert_eq!(edit(&|v| v["entries"][3]["name"] = json!("bad\u{7}name")).unwrap_err(), E::InvalidField);
    assert_eq!(edit(&|v| v["page_size"] = json!(0)).unwrap_err(), E::InvalidPageLayout);
    assert_eq!(edit(&|v| v["page_selector_offset"] = json!(3)).unwrap_err(), E::InvalidPageLayout);
    assert_eq!(edit(&|v| v["entries"][3] = json!({"name":"Truncated"})).unwrap_err(), E::Malformed, "truncated entry");
    assert_eq!(edit(&|v| v["vendor_blob"] = json!(1)).unwrap_err(), E::Malformed, "unknown field");
    assert_eq!(parse_colour_book(b"{\"schema_id\":", StudioBookOrigin::OperatorSupplied, &token()).unwrap_err(), E::Malformed, "truncated bytes");
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(parse_colour_book(&bytes(&book("operator_supplied")), StudioBookOrigin::OperatorSupplied, &canceled).unwrap_err(), E::Canceled);
}

#[test]
fn book_dims_u32_max_and_aggregate_over_budget_are_refused() {
    use StudioColourBookError as E;
    assert_eq!(edit(&|v| v["declared_count"] = json!(u32::MAX)).unwrap_err(), E::BudgetExceeded, "declared count u32::MAX");
    assert_eq!(edit(&|v| v["page_size"] = json!(u32::MAX)).unwrap_err(), E::InvalidPageLayout, "page size u32::MAX");
    assert_eq!(edit(&|v| v["declared_count"] = json!(u64::from(u32::MAX) + 1)).unwrap_err(), E::Malformed, "beyond u32");
    // Aggregate: more entries than the bound is refused while parsing.
    let entry = json!({"name":"Z","code":"Z","components":[1.0,0.0,0.0]});
    let mut v = book("operator_supplied");
    v["entries"] = Value::Array(vec![entry; BOOK_MAX_ENTRIES + 1]);
    assert_eq!(parse(&v, StudioBookOrigin::OperatorSupplied).unwrap_err(), E::BudgetExceeded, "aggregate over the entry budget");
    assert_eq!(edit(&|v| v["entries"][0]["components"] = json!([1.0, 0.0, 0.0, 0.0, 0.0])).unwrap_err(), E::BudgetExceeded, "components per entry");
    let big = vec![b' '; BOOK_MAX_BYTES + 1];
    assert_eq!(parse_colour_book(&big, StudioBookOrigin::OperatorSupplied, &token()).unwrap_err(), E::BudgetExceeded, "bytes over the bound");
}

#[test]
fn licensing_is_per_book_and_an_operator_book_loads_from_the_same_format() {
    use StudioColourBookError as E;
    // The same resource format for both origins; the declared state must match the origin.
    assert!(parse(&book("bundled"), StudioBookOrigin::Bundled).is_ok());
    assert!(parse(&book("operator_supplied"), StudioBookOrigin::OperatorSupplied).is_ok());
    assert_eq!(parse(&book("bundled"), StudioBookOrigin::OperatorSupplied).unwrap_err(), E::LicenseStateMismatch, "an operator file cannot declare itself bundled");
    assert_eq!(parse(&book("operator_supplied"), StudioBookOrigin::Bundled).unwrap_err(), E::LicenseStateMismatch, "a book that is not bundled never ships");
    assert_eq!(parse(&book("absent"), StudioBookOrigin::OperatorSupplied).unwrap_err(), E::LicenseStateMismatch, "an absent book is never loaded");
    assert_eq!(edit(&|v| v["license_state"] = json!("everything")).unwrap_err(), E::Malformed, "no blanket state");
    let mut lib = StudioColourBookLibrary::new();
    let c = ctx("swatch.book.load", 5);
    lib.load(&c, &bytes(&book("operator_supplied")), StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    let mut other = book("bundled");
    other["book_id"] = json!("zzyzx-test");
    let r = lib.load(&c, &bytes(&other), StudioBookOrigin::Bundled, &token()).unwrap();
    assert_eq!((r.operation, r.entries, r.books, r.correlation), ("book.load", 7, 2, 77));
    // Only the bundled book is in the shippable set; the operator book is referenced, never shipped.
    assert_eq!(lib.bundled_books().map(|b| b.book_id.as_str()).collect::<Vec<_>>(), vec!["zzyzx-test"]);
    assert_eq!(lib.state("glimmerbrook-test"), Some(StudioBookLicenseState::OperatorSupplied));
    // A failed reload keeps the last good book.
    let mut broken = book("operator_supplied");
    broken["declared_count"] = json!(99);
    assert_eq!(lib.load(&c, &bytes(&broken), StudioBookOrigin::OperatorSupplied, &token()).unwrap_err(), E::CountMismatch);
    assert_eq!(lib.lookup("glimmerbrook-test", "QX03").map(|e| e.name.as_str()), Ok("Quuxberry 03"));
}

#[test]
fn an_absent_book_loses_only_lookup_and_never_changes_stored_colours() {
    let mut lib = StudioColourBookLibrary::new();
    let c = ctx("swatch.book.load", 5);
    lib.load(&c, &bytes(&book("operator_supplied")), StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    let b = lib.book("glimmerbrook-test").unwrap().clone();
    let start = StudioVectorRecords::new(identity("SDOC", 1));
    let picks = [("QX02", identity("SSWT", 1)), ("QX05", identity("SSWT", 2))];
    let picks: Vec<(&str, &str)> = picks.iter().map(|(c, s)| (*c, s.as_str())).collect();
    let o = add_colour_book_swatches(&start, &ctx(BOOK_ADD_SWATCHES_COMMAND_ID, 5), 5, &b, &picks, &token()).unwrap();
    assert_eq!((o.report.entries, o.report.books), (2, 1));
    let doc = o.records;
    assert_eq!(validate(&doc), Ok(()));
    let s = doc.swatch(&identity("SSWT", 1)).unwrap();
    assert_eq!(s.swatch_kind, StudioSwatchKind::Spot);
    assert_eq!(s.name, "GTI Quuxberry 02 fake");
    let ink = s.spot.as_ref().unwrap();
    assert_eq!((ink.ink_name.as_str(), ink.measured_lab), ("GTI Quuxberry 02 fake", Some([42.0, -10.0, 12.5])));
    // The document references the book; it never embeds its entries.
    let wire = serde_json::to_value(&doc).unwrap();
    assert_eq!(wire["colour_book_refs"], json!([{"book_id":"glimmerbrook-test","title":"Glimmerbrook Test Inks","license_state":"operator_supplied"}]));
    assert_eq!(wire["swatch_book_links"][1], json!({"swatch_id":identity("SSWT",2),"book_id":"glimmerbrook-test","entry_code":"QX05"}));
    assert!(!wire.to_string().contains("QX07"), "unpicked entries are not copied");
    let mut embed = wire.clone();
    embed["colour_book_refs"][0]["entries"] = json!([{"name":"x","code":"x","components":[1.0,0.0,0.0]}]);
    assert!(serde_json::from_value::<StudioVectorRecords>(embed).is_err(), "a document cannot carry book entries");
    // The book goes absent: lookup is lost, the stored swatches and their colours are not.
    let before = serde_json::to_string(&doc).unwrap();
    lib.mark_absent(&c, "glimmerbrook-test").unwrap();
    assert_eq!(lib.lookup("glimmerbrook-test", "QX02").unwrap_err().code(), "book_absent");
    assert_eq!(lib.state("glimmerbrook-test"), Some(StudioBookLicenseState::Absent));
    assert_eq!(validate(&doc), Ok(()));
    assert_eq!(serde_json::to_string(&doc).unwrap(), before, "stored names and measured definitions unchanged");
    // A later operator-supplied copy activates lookup again through the same format.
    lib.load(&c, &bytes(&book("operator_supplied")), StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    assert!(lib.lookup("glimmerbrook-test", "QX02").is_ok());
    // Population refusals leave the document as it was.
    use StudioColourBookError as E;
    let (id1, id3) = (identity("SSWT", 1), identity("SSWT", 3));
    let again = [("QX03", id1.as_str())];
    assert_eq!(add_colour_book_swatches(&doc, &ctx(BOOK_ADD_SWATCHES_COMMAND_ID, 5), 5, &b, &again, &token()).unwrap_err(), E::InvalidSwatchId);
    let missing = [("QX99", id3.as_str())];
    assert_eq!(add_colour_book_swatches(&doc, &ctx(BOOK_ADD_SWATCHES_COMMAND_ID, 5), 5, &b, &missing, &token()).unwrap_err(), E::EntryMissing);
    assert_eq!(add_colour_book_swatches(&doc, &ctx(BOOK_ADD_SWATCHES_COMMAND_ID, 4), 5, &b, &missing, &token()).unwrap_err(), E::StaleRevision { expected: 4, current: 5 });
    assert_eq!(add_colour_book_swatches(&doc, &ctx("swatch.group.create", 5), 5, &b, &missing, &token()).unwrap_err(), E::CommandMissing);
    // Document link rules.
    let mut v = wire.clone();
    v["swatch_book_links"][0]["book_id"] = json!("unknown-book");
    assert_eq!(validate(&serde_json::from_value(v).unwrap()), Err(Code::DanglingReference));
    let mut v = wire.clone();
    v["swatch_book_links"][1]["swatch_id"] = json!(identity("SSWT", 1));
    assert_eq!(validate(&serde_json::from_value(v).unwrap()), Err(Code::DuplicateIdentity));
    let mut v = wire;
    v["swatch_book_links"][0]["swatch_id"] = json!(identity("SSWT", 9));
    assert_eq!(validate(&serde_json::from_value(v).unwrap()), Err(Code::DanglingReference));
}

#[test]
fn captured_coverage_is_planning_data_and_never_enables_a_book() {
    let rows = &CAPTURED_BOOK_COVERAGE;
    let bundled: u32 = rows.iter().filter(|r| r.bundled_at_capture).filter_map(|r| r.colours).sum();
    assert_eq!(bundled, 5243, "STU-COL-103: twelve books, 5,243 colours");
    assert_eq!(rows.iter().filter(|r| r.bundled_at_capture).count(), 12);
    let families: std::collections::BTreeSet<&str> = rows.iter().filter(|r| r.bundled_at_capture).map(|r| r.family).collect();
    assert_eq!(families.into_iter().collect::<Vec<_>>(), vec!["ANPA", "DIC", "FOCOLTONE", "HKS", "TRUMATCH"]);
    let absent: Vec<_> = rows.iter().filter(|r| !r.bundled_at_capture).map(|r| r.family).collect();
    assert_eq!(absent, vec!["PANTONE"]);
    // No operator decision is inferred: a fresh library knows no book, so nothing is enabled.
    let lib = StudioColourBookLibrary::new();
    for r in rows {
        assert_eq!(lib.state(r.book), None);
        assert_eq!(lib.lookup(r.book, "any").unwrap_err(), StudioBookLookupError::BookUnknown);
    }
    assert_eq!(lib.bundled_books().count(), 0);
    // Native spot works with no book at all (STU-COL-184).
    let mut doc = StudioVectorRecords::new(identity("SDOC", 1));
    doc.swatches.push(serde_json::from_value(json!({"schema_id":"hsk.studio.swatch@1","swatch_id":identity("SSWT",4),"name":"House ink","swatch_kind":"spot","spot":{"ink_name":"House ink","measured_lab":[50.0,10.0,10.0]}})).unwrap());
    assert_eq!(validate(&doc), Ok(()));
}

#[test]
fn named_colour_profile_entries_build_a_bound_book() {
    let profile = StudioNamedColourProfileRef { profile_id: identity("SCPF", 3), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab".repeat(32) } };
    let colours = vec![("Wobblefern 1".to_string(), [61.0, -22.5, 30.25]), ("Wobblefern 2".to_string(), [35.5, 40.0, -12.0])];
    let b = StudioColourBook::from_named_colours("wobblefern-test", "Wobblefern Test", StudioBookLicenseState::OperatorSupplied, "synthetic", profile.clone(), "WF ", "", &colours, &token()).unwrap();
    assert_eq!(b.named_colour_profile.as_ref(), Some(&profile));
    assert_eq!((b.color_space, b.ink_type, b.declared_count, b.page_count()), (StudioBookColorSpace::Lab, StudioBookInkType::Spot, 2, 1));
    let s = b.swatch_for(b.entry("Wobblefern 2").unwrap(), &identity("SSWT", 8));
    assert_eq!((s.name.as_str(), s.spot.unwrap().measured_lab), ("WF Wobblefern 2", Some([35.5, 40.0, -12.0])), "measured values pass through unchanged");
    // The bound book round-trips through the resource format.
    let back = parse_colour_book(&serde_json::to_vec(&b).unwrap(), StudioBookOrigin::OperatorSupplied, &token()).unwrap();
    assert_eq!(back, b);
    // Refusals: a bad profile reference, a duplicate name, an out-of-range Lab value.
    let bad = StudioNamedColourProfileRef { profile_id: identity("SLYR", 3), ..profile.clone() };
    assert_eq!(StudioColourBook::from_named_colours("w", "W", StudioBookLicenseState::OperatorSupplied, "", bad, "", "", &colours, &token()).unwrap_err(), StudioColourBookError::InvalidField);
    let dup = vec![colours[0].clone(), colours[0].clone()];
    assert_eq!(StudioColourBook::from_named_colours("w", "W", StudioBookLicenseState::OperatorSupplied, "", profile.clone(), "", "", &dup, &token()).unwrap_err(), StudioColourBookError::DuplicateEntry);
    let wild = vec![("X".to_string(), [101.0, 0.0, 0.0])];
    assert_eq!(StudioColourBook::from_named_colours("w", "W", StudioBookLicenseState::OperatorSupplied, "", profile, "", "", &wild, &token()).unwrap_err(), StudioColourBookError::InvalidComponents);
}
