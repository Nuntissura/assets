//! Lane SMB-PR-MEDIA-MARKERS-1, brief item 2: the STU-VID-034 media item marker record set, its key,
//! its bounds, the one marker id space over the three scopes, and its typed commands.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, Ticks};
use hsk_studio_folio::*;
use serde_json::{Value, json};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const F25: u64 = 10_160_640_000;

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn digest(name: &str) -> ContentDigest {
    ContentDigest { algorithm: "sha256".into(), digest: name.into() }
}

fn marker(id: &str, frame: u64) -> StudioMarker {
    StudioMarker {
        marker_id: id.into(),
        name: format!("name of {id}"),
        comment: "line one\nline two".into(),
        color: LabelColor::Teal,
        start: Ticks::new(frame * F25),
        duration: Ticks::new(F25),
        role: None,
    }
}

fn clip(marker_ids: &[&str]) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: ClipSource::Media { artifact_manifest_id: "manifest-a".into(), content_digest: digest("item-a"), asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(10 * F25),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(10 * F25),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: marker_ids.iter().map(|id| marker(id, 1)).collect(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}

/// `seq-a` holding a sequence marker `in-sequence` and a clip with the marker `on-clip`.
fn document() -> StudioDocument {
    let json = json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    let mut d: StudioDocument = serde_json::from_value(json).unwrap();
    d.sequences.push(StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-a".into(),
        name: "seq-a".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F25 },
        duration: Ticks::new(10 * F25),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "track".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            volume: 1.0,
            pan: 0.0,
            clips: vec![clip(&["on-clip"])],
        }],
        markers: vec![marker("in-sequence", 2)],
        work_area: None,
    });
    d
}

fn records(items: Vec<(&str, Vec<StudioMarker>)>) -> StudioMediaMarkerRecords {
    let mut r = StudioMediaMarkerRecords::new(DOC);
    r.media_items = items.into_iter().map(|(name, markers)| StudioMediaMarkerEntry { content_digest: digest(name), markers }).collect();
    r
}

fn valid() -> StudioMediaMarkerRecords {
    records(vec![("item-a", vec![marker("m-1", 3), marker("m-2", 4)]), ("item-b", vec![marker("m-3", 5)])])
}

fn parse(bytes: &[u8]) -> Result<StudioMediaMarkerRecords, Diagnostic> {
    parse_media_marker_records(bytes, &document(), Budget::default(), &token())
}

fn wire_edited(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = serde_json::to_value(valid()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

fn code_of(bytes: Vec<u8>) -> Option<Code> {
    parse(&bytes).err().map(|d| d.code)
}

fn ctx(command: &str, revision: u64) -> CommandContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), Some(CommandId::parse(command).unwrap()), revision, CorrelationId::new(41), None)
}

fn run(r: &StudioMediaMarkerRecords, cmd: &StudioMediaMarkerCommand) -> Result<StudioMediaMarkerOutcome, StudioMediaMarkerError> {
    apply_media_marker_command(&document(), r, &ctx(cmd.command_id(), 7), 7, cmd, Budget::default(), &token())
}

fn add(name: &str, m: StudioMarker) -> StudioMediaMarkerCommand {
    StudioMediaMarkerCommand::Add { content_digest: digest(name), marker: m }
}
fn replace(name: &str, m: StudioMarker) -> StudioMediaMarkerCommand {
    StudioMediaMarkerCommand::Replace { content_digest: digest(name), marker: m }
}
fn remove(name: &str, id: &str) -> StudioMediaMarkerCommand {
    StudioMediaMarkerCommand::Remove { content_digest: digest(name), marker_id: id.into() }
}

#[test]
fn a_media_item_holds_markers_with_every_field_of_the_clause_mt_33736() {
    let roles = [
        None,
        Some(MarkerRole::Chapter),
        Some(MarkerRole::WebLink { url: "https://example.test/a".into() }),
        Some(MarkerRole::CuePoint),
        Some(MarkerRole::Caption { text: "a caption".into() }),
    ];
    let markers: Vec<StudioMarker> = roles
        .iter()
        .enumerate()
        .map(|(i, role)| StudioMarker { role: role.clone(), start: Ticks::new(i as u64 * F25), duration: Ticks::new(if i == 0 { 0 } else { F25 }), ..marker(&format!("m-{i}"), 0) })
        .collect();
    let r = records(vec![("item-a", markers)]);
    let bytes = serde_json::to_vec(&r).unwrap();
    assert_eq!(parse(&bytes).unwrap(), r, "the wire form reads back");
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schema_id"], "hsk.studio.media_markers@1");
    assert_eq!(v["media_items"][0]["content_digest"], json!({"algorithm": "sha256", "digest": "item-a"}));
    let first = &v["media_items"][0]["markers"][0];
    assert_eq!((first["name"].as_str(), first["comment"].as_str(), first["color"].as_str()), (Some("name of m-0"), Some("line one\nline two"), Some("teal")));
    assert_eq!((first["duration"].as_u64(), v["media_items"][0]["markers"][1]["duration"].as_u64()), (Some(0), Some(F25)), "a point and a span");
    assert_eq!(first["role"], Value::Null);
    assert_eq!(v["media_items"][0]["markers"][2]["role"], json!({"role": "web_link", "url": "https://example.test/a"}));
    assert_eq!(r.entry(&digest("item-a")).unwrap().markers.len(), 5);
    assert_eq!(r.entry(&digest("item-b")), None);
    assert_eq!(r.marker_count(), 5);
}

#[test]
fn the_records_name_no_sequence_or_clip_and_copy_no_document_value() {
    let bytes = serde_json::to_vec(&valid()).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    for name in ["seq-a", "SCLP-", "STRK-", "manifest-a", "sequence_id", "clip_id", "asset_id", "artifact_manifest_id", "frame_rate"] {
        assert!(!text.contains(name), "{name} in {text}");
    }
    // The set stays valid against a document without the clip or the sequence: nothing can dangle.
    let mut no_clip = document();
    no_clip.sequences[0].tracks[0].clips.clear();
    assert_eq!(parse_media_marker_records(&bytes, &no_clip, Budget::default(), &token()).unwrap(), valid());
    let mut no_sequence = document();
    no_sequence.sequences.clear();
    assert_eq!(parse_media_marker_records(&bytes, &no_sequence, Budget::default(), &token()).unwrap(), valid());
    // The key is the bytes, compared exactly: a different case or algorithm is another item.
    assert_eq!(valid().entry(&digest("ITEM-A")), None);
    assert_eq!(valid().entry(&ContentDigest { algorithm: "sha512".into(), digest: "item-a".into() }), None);
    let same_digest_two_algorithms = records(vec![("item-a", vec![marker("m-1", 3)])]);
    let mut both = same_digest_two_algorithms.clone();
    both.media_items.push(StudioMediaMarkerEntry { content_digest: ContentDigest { algorithm: "sha512".into(), digest: "item-a".into() }, markers: vec![marker("m-2", 3)] });
    assert_eq!(parse(&serde_json::to_vec(&both).unwrap()).unwrap(), both);
}

#[test]
fn the_wire_is_closed_and_every_level_refuses_what_it_does_not_declare() {
    let unknown: [&dyn Fn(&mut Value); 4] = [
        &|v| v["vendor"] = json!(1),
        &|v| v["media_items"][0]["vendor"] = json!(1),
        &|v| v["media_items"][0]["content_digest"]["vendor"] = json!(1),
        &|v| v["media_items"][0]["markers"][0]["vendor"] = json!(1),
    ];
    for edit in unknown {
        assert_eq!(code_of(wire_edited(edit)), Some(Code::InvalidJson));
    }
    let missing: [&dyn Fn(&mut Value); 4] = [
        &|v| drop(v.as_object_mut().unwrap().remove("document_id")),
        &|v| drop(v["media_items"][0].as_object_mut().unwrap().remove("markers")),
        &|v| drop(v["media_items"][0].as_object_mut().unwrap().remove("content_digest")),
        &|v| drop(v["media_items"][0]["markers"][0].as_object_mut().unwrap().remove("role")),
    ];
    for edit in missing {
        assert_eq!(code_of(wire_edited(edit)), Some(Code::InvalidJson));
    }
    assert_eq!(code_of(wire_edited(|v| v["schema_id"] = json!("hsk.studio.media_markers@2"))), Some(Code::InvalidJson));
    assert_eq!(code_of(wire_edited(|v| v["media_items"][0]["markers"][0]["start"] = json!(-1))), Some(Code::InvalidJson));
    assert_eq!(code_of(wire_edited(|v| v["media_items"][0]["markers"][0]["start"] = json!(1.5))), Some(Code::InvalidJson));
    let repeated = String::from_utf8(serde_json::to_vec(&valid()).unwrap()).unwrap().replacen('{', &format!("{{\"document_id\":\"{DOC}\","), 1);
    assert_eq!(code_of(repeated.into_bytes()), Some(Code::DuplicateField));
    assert_eq!(code_of(b"{not json".to_vec()), Some(Code::InvalidJson));
    assert_eq!(code_of("[".repeat(64).into_bytes()), Some(Code::BudgetExceeded), "depth before any parse");
    assert_eq!(code_of(wire_edited(|v| v["document_id"] = json!("SDOC-019abcde-0000-7000-8000-000000000002"))), Some(Code::InvalidField));
    assert_eq!(code_of(wire_edited(|v| v["document_id"] = json!("not an id"))), Some(Code::InvalidId));
    let schema = serde_json::to_value(media_marker_records_schema()).unwrap();
    assert_eq!(schema["additionalProperties"], json!(false));
}

#[test]
fn a_digest_is_one_key_and_an_entry_holds_a_marker() {
    let twice = records(vec![("item-a", vec![marker("m-1", 3)]), ("item-a", vec![marker("m-2", 3)])]);
    assert_eq!(parse(&serde_json::to_vec(&twice).unwrap()).unwrap_err().code, Code::DuplicateIdentity);
    let empty = records(vec![("item-a", vec![])]);
    assert_eq!(parse(&serde_json::to_vec(&empty).unwrap()).unwrap_err().code, Code::InvalidField);
    for bad in [String::new(), "a\u{7}b".to_owned(), "x".repeat(MAX_HANDLE_BYTES + 1)] {
        let by_digest = records(vec![(bad.as_str(), vec![marker("m-1", 3)])]);
        assert_eq!(parse(&serde_json::to_vec(&by_digest).unwrap()).unwrap_err().code, Code::InvalidField, "{bad:?}");
        let mut by_algorithm = records(vec![("item-a", vec![marker("m-1", 3)])]);
        by_algorithm.media_items[0].content_digest.algorithm.clone_from(&bad);
        assert_eq!(parse(&serde_json::to_vec(&by_algorithm).unwrap()).unwrap_err().code, Code::InvalidField, "{bad:?}");
    }
    // An empty record set is valid: no media item has a marker.
    assert_eq!(parse(&serde_json::to_vec(&StudioMediaMarkerRecords::new(DOC)).unwrap()).unwrap(), StudioMediaMarkerRecords::new(DOC));
}

/// `StudioMarker` says its id is unique among the document's markers: over the three scopes.
#[test]
fn a_marker_id_is_unique_over_the_three_scopes_mt_33736() {
    let table: [(&str, StudioMediaMarkerRecords); 4] = [
        ("the id of a sequence marker", records(vec![("item-a", vec![marker("in-sequence", 3)])])),
        ("the id of a clip marker", records(vec![("item-a", vec![marker("on-clip", 3)])])),
        ("the id in the same item", records(vec![("item-a", vec![marker("m-1", 3), marker("m-1", 4)])])),
        ("the id in another item", records(vec![("item-a", vec![marker("m-1", 3)]), ("item-b", vec![marker("m-1", 4)])])),
    ];
    for (what, r) in table {
        let d = parse(&serde_json::to_vec(&r).unwrap()).unwrap_err();
        assert_eq!(d.code, Code::DuplicateIdentity, "{what}");
        assert_eq!(validate_media_marker_records(&r, &document(), Budget::default(), &token()).unwrap_err().code, Code::DuplicateIdentity, "{what}");
    }
    // A document whose own markers do not collide is valid, and the check reads that document.
    let r = records(vec![("item-a", vec![marker("on-clip", 3)])]);
    let mut other = document();
    other.sequences[0].tracks[0].clips[0].markers.clear();
    assert!(validate_media_marker_records(&r, &other, Budget::default(), &token()).is_ok());
}

/// The field rules live twice (module docs): one table through both, so they cannot drift unseen.
#[test]
fn marker_fields_are_refused_as_the_document_refuses_them() {
    let bad_ids = |id: &str| StudioMarker { marker_id: id.into(), ..marker("m", 0) };
    let table: Vec<(&str, StudioMarker, Option<Code>)> = vec![
        ("a plain marker", marker("m", 0), None),
        ("an empty name", StudioMarker { name: String::new(), ..marker("m", 0) }, None),
        ("a longest name", StudioMarker { name: "n".repeat(MAX_MARKER_TEXT_BYTES), ..marker("m", 0) }, None),
        ("a longest comment with breaks", StudioMarker { comment: "\n".repeat(MAX_MARKER_TEXT_BYTES), ..marker("m", 0) }, None),
        ("a caption with no text", StudioMarker { role: Some(MarkerRole::Caption { text: String::new() }), ..marker("m", 0) }, None),
        ("the last tick", StudioMarker { start: Ticks::new(u64::MAX), duration: Ticks::new(0), ..marker("m", 0) }, None),
        ("an empty id", bad_ids(""), Some(Code::InvalidId)),
        ("an id off the key grammar", bad_ids("a/b"), Some(Code::InvalidId)),
        ("an id past the key length", bad_ids(&"m".repeat(129)), Some(Code::InvalidId)),
        ("a name too long", StudioMarker { name: "n".repeat(MAX_MARKER_TEXT_BYTES + 1), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a control character in a name", StudioMarker { name: "a\nb".into(), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a comment too long", StudioMarker { comment: "c".repeat(MAX_MARKER_TEXT_BYTES + 1), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a link with no address", StudioMarker { role: Some(MarkerRole::WebLink { url: String::new() }), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a link too long", StudioMarker { role: Some(MarkerRole::WebLink { url: "u".repeat(MAX_MARKER_TEXT_BYTES + 1) }), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a caption with a control character", StudioMarker { role: Some(MarkerRole::Caption { text: "a\u{1}".into() }), ..marker("m", 0) }, Some(Code::InvalidField)),
        ("a span past the last tick", StudioMarker { start: Ticks::new(u64::MAX), duration: Ticks::new(1), ..marker("m", 0) }, Some(Code::InvalidField)),
    ];
    struct Host;
    impl Resolver for Host {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let budget = Budget { payload_bytes: 1 << 20, ..Budget::default() };
    for (what, m, code) in table {
        let mut as_sequence_marker = document();
        as_sequence_marker.sequences[0].markers.push(m.clone());
        let document_says = validate_document(as_sequence_marker, budget, &token(), &Host).err().map(|d| d.code);
        let records_say = validate_media_marker_records(&records(vec![("item-a", vec![m])]), &document(), budget, &token()).err().map(|d| d.code);
        assert_eq!(records_say, code, "{what}");
        assert_eq!(document_says, code, "{what}: the document's own rule");
    }
}

#[test]
fn dims_u32_max_and_aggregate_over_budget_are_refused() {
    // Bytes past the declared limit, whatever the budget allows, before any parse.
    let roomy = Budget { input_bytes: usize::MAX, depth: 64, nodes: usize::MAX, payload_bytes: usize::MAX, ..Budget::default() };
    let huge = vec![b' '; MAX_MEDIA_MARKER_INPUT_BYTES + 1];
    assert_eq!(parse_media_marker_records(&huge, &document(), roomy, &token()).unwrap_err().code, Code::BudgetExceeded);
    // And past the caller's budget.
    let small = Budget { input_bytes: 8, ..Budget::default() };
    assert_eq!(parse_media_marker_records(&serde_json::to_vec(&valid()).unwrap(), &document(), small, &token()).unwrap_err().code, Code::BudgetExceeded);
    // Items, then markers, counted against the declared limits with a roomy budget.
    let plain = |i: usize| StudioMarker { name: String::new(), comment: String::new(), ..marker(&format!("m{i}"), 0) };
    let mut many_items = StudioMediaMarkerRecords::new(DOC);
    many_items.media_items = (0..=MAX_MEDIA_MARKER_ITEMS).map(|i| StudioMediaMarkerEntry { content_digest: digest(&format!("d{i}")), markers: vec![plain(i)] }).collect();
    assert_eq!(validate_media_marker_records(&many_items, &document(), roomy, &token()).unwrap_err().code, Code::BudgetExceeded);
    many_items.media_items.pop();
    assert!(validate_media_marker_records(&many_items, &document(), roomy, &token()).is_ok(), "at the limit");
    let many_markers = records(vec![("item-a", (0..=MAX_MEDIA_MARKERS).map(plain).collect())]);
    assert_eq!(validate_media_marker_records(&many_markers, &document(), roomy, &token()).unwrap_err().code, Code::BudgetExceeded);
    // The aggregate over the entries: each limit met by the sum, not by one list.
    let split: Vec<(String, Vec<StudioMarker>)> = vec![("a".into(), (0..MAX_MEDIA_MARKERS).map(plain).collect()), ("b".into(), vec![plain(MAX_MEDIA_MARKERS)])];
    let split = records(split.iter().map(|(n, m)| (n.as_str(), m.clone())).collect());
    assert_eq!(validate_media_marker_records(&split, &document(), roomy, &token()).unwrap_err().code, Code::BudgetExceeded);
    // Budget::nodes counts items and markers together; payload bytes count their text.
    let two = records(vec![("item-a", vec![marker("m-1", 3)])]);
    assert!(validate_media_marker_records(&two, &document(), Budget { nodes: 2, ..Budget::default() }, &token()).is_ok());
    assert_eq!(validate_media_marker_records(&two, &document(), Budget { nodes: 1, ..Budget::default() }, &token()).unwrap_err().code, Code::BudgetExceeded);
    assert_eq!(validate_media_marker_records(&two, &document(), Budget { payload_bytes: 4, ..Budget::default() }, &token()).unwrap_err().code, Code::BudgetExceeded);
    // A cancel stops the validation.
    let canceled = token();
    canceled.cancel();
    assert_eq!(validate_media_marker_records(&valid(), &document(), Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
    assert_eq!(parse_media_marker_records(&serde_json::to_vec(&valid()).unwrap(), &document(), Budget::default(), &canceled).unwrap_err().code, Code::Canceled);
}

#[test]
fn the_commands_add_replace_and_remove_and_the_entry_follows_its_markers() {
    assert_eq!(MEDIA_MARKER_COMMAND_IDS, ["media_marker.add", "media_marker.replace", "media_marker.remove"]);
    let start = StudioMediaMarkerRecords::new(DOC);
    let first = run(&start, &add("item-a", marker("m-1", 3))).unwrap();
    assert_eq!(first.records, records(vec![("item-a", vec![marker("m-1", 3)])]), "the first marker makes the entry");
    assert_eq!(first.report, StudioMediaMarkerReport { command: "media_marker.add", base_revision: 7, correlation: 41, media_items: 1, markers: 1, changed: true });
    let second = run(&first.records, &add("item-a", marker("m-2", 4))).unwrap();
    assert_eq!(second.records, records(vec![("item-a", vec![marker("m-1", 3), marker("m-2", 4)])]), "an existing item, in the order added");
    let third = run(&second.records, &add("item-b", marker("m-3", 5))).unwrap();
    assert_eq!((third.report.media_items, third.report.markers), (2, 3));

    let moved = StudioMarker { name: "renamed".into(), role: Some(MarkerRole::Chapter), ..marker("m-1", 9) };
    let replaced = run(&third.records, &replace("item-a", moved.clone())).unwrap();
    assert_eq!(replaced.records.entry(&digest("item-a")).unwrap().markers, vec![moved.clone(), marker("m-2", 4)], "in place");
    assert_eq!(replaced.report.command, "media_marker.replace");

    let same = run(&replaced.records, &replace("item-a", moved)).unwrap();
    assert!(!same.report.changed);
    assert_eq!(same.records, replaced.records);

    let gone = run(&replaced.records, &remove("item-b", "m-3")).unwrap();
    assert_eq!(gone.records.entry(&digest("item-b")), None, "the last marker takes the entry with it");
    assert_eq!((gone.report.media_items, gone.report.markers, gone.report.command), (1, 2, "media_marker.remove"));
    assert!(gone.records.entry(&digest("item-a")).is_some());
}

#[test]
fn every_command_refusal_is_typed_and_the_input_is_not_touched() {
    let held = valid();
    let before = held.clone();
    use StudioMediaMarkerError as E;
    let table: Vec<(&str, StudioMediaMarkerCommand, E)> = vec![
        ("an id in the same item", add("item-a", marker("m-1", 9)), E::DuplicateMarker),
        ("an id in another item", add("item-a", marker("m-3", 9)), E::DuplicateMarker),
        ("the id of a clip marker", add("item-a", marker("on-clip", 9)), E::DuplicateMarker),
        ("the id of a sequence marker", add("item-c", marker("in-sequence", 9)), E::DuplicateMarker),
        ("an id off the grammar", add("item-a", marker("a/b", 9)), E::InvalidMarker(Code::InvalidId)),
        ("a span past the last tick", add("item-a", StudioMarker { start: Ticks::new(u64::MAX), ..marker("m-9", 0) }), E::InvalidMarker(Code::InvalidField)),
        ("a digest with a control character", add("a\u{1}", marker("m-9", 9)), E::InvalidDigest),
        ("an empty digest", add("", marker("m-9", 9)), E::InvalidDigest),
        ("a replace on another item", replace("item-b", marker("m-1", 9)), E::MarkerMissing),
        ("a replace of no marker", replace("item-a", marker("m-9", 9)), E::MarkerMissing),
        ("a replace on no item", replace("item-z", marker("m-1", 9)), E::MarkerMissing),
        ("a replace breaking the fields", replace("item-a", StudioMarker { name: "a\nb".into(), ..marker("m-1", 9) }), E::InvalidMarker(Code::InvalidField)),
        ("a remove on another item", remove("item-b", "m-1"), E::MarkerMissing),
        ("a remove of no marker", remove("item-a", "m-9"), E::MarkerMissing),
        ("a remove on no item", remove("item-z", "m-1"), E::MarkerMissing),
        ("a remove of a clip marker's id", remove("item-a", "on-clip"), E::MarkerMissing),
    ];
    for (what, cmd, expected) in table {
        assert_eq!(run(&held, &cmd).unwrap_err(), expected, "{what}");
        assert_eq!(held, before, "{what}");
    }
    assert_eq!(E::DuplicateMarker.code(), "duplicate_marker");
    assert_eq!(E::MarkerMissing.code(), "marker_missing");
    assert_eq!(E::InvalidDigest.code(), "invalid_digest");
    assert_eq!(E::InvalidMarker(Code::InvalidId).code(), "invalid_marker");

    // The context: no command, another command, a stale revision.
    let good = add("item-a", marker("m-9", 9));
    let d = document();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let no_command = CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), None, 7, CorrelationId::new(1), None);
    assert_eq!(apply_media_marker_command(&d, &held, &no_command, 7, &good, Budget::default(), &token()).unwrap_err(), E::CommandMissing);
    assert_eq!(apply_media_marker_command(&d, &held, &ctx("media_marker.remove", 7), 7, &good, Budget::default(), &token()).unwrap_err(), E::CommandMismatch);
    assert_eq!(apply_media_marker_command(&d, &held, &ctx("media_marker.add", 6), 7, &good, Budget::default(), &token()).unwrap_err(), E::StaleRevision { expected: 6, current: 7 });

    // Records that do not validate stop every command.
    let bad = records(vec![("item-a", vec![marker("on-clip", 3)])]);
    assert_eq!(run(&bad, &add("item-b", marker("m-9", 9))).unwrap_err(), E::InvalidInput(Code::DuplicateIdentity));
    assert_eq!(run(&bad, &remove("item-a", "on-clip")).unwrap_err(), E::InvalidInput(Code::DuplicateIdentity));
}

/// R55 F-R55-6 does not come back: a cancel and a budget overrun have their own codes at the boundary.
#[test]
fn command_codes_tell_cancellation_and_budget_from_bad_input_r55_f6() {
    use StudioMediaMarkerError as E;
    let cmd = add("item-a", marker("m-9", 9));
    let canceled = token();
    canceled.cancel();
    let refused = apply_media_marker_command(&document(), &valid(), &ctx("media_marker.add", 7), 7, &cmd, Budget::default(), &canceled).unwrap_err();
    assert_eq!((refused, refused.code()), (E::Canceled, "canceled"));
    let few = Budget { nodes: 2, ..Budget::default() };
    let refused = apply_media_marker_command(&document(), &valid(), &ctx("media_marker.add", 7), 7, &cmd, few, &token()).unwrap_err();
    assert_eq!((refused, refused.code()), (E::BudgetExceeded, "budget_exceeded"), "the held records are over it");
    let exact = Budget { nodes: 5, ..Budget::default() };
    let refused = apply_media_marker_command(&document(), &valid(), &ctx("media_marker.add", 7), 7, &cmd, exact, &token()).unwrap_err();
    assert_eq!((refused, refused.code()), (E::BudgetExceeded, "budget_exceeded"), "the new marker goes over it");
    // The result must fit what a parse under this budget accepts.
    let tight_bytes = serde_json::to_vec(&valid()).unwrap().len();
    let tight = Budget { input_bytes: tight_bytes, ..Budget::default() };
    let refused = apply_media_marker_command(&document(), &valid(), &ctx("media_marker.add", 7), 7, &cmd, tight, &token()).unwrap_err();
    assert_eq!(refused, E::BudgetExceeded);
    assert_eq!(E::InvalidInput(Code::InvalidField).code(), "invalid_input");
}

/// R55 F-R55-7 does not come back: a command returns records and a report and holds no document, so it
/// cannot leave one at an unchanged revision. The session's commit bumps and validates.
#[test]
fn a_command_returns_records_and_never_a_document_r55_f7() {
    let d = document();
    let before = d.clone();
    let outcome: StudioMediaMarkerOutcome = apply_media_marker_command(&d, &valid(), &ctx("media_marker.add", 7), 7, &add("item-a", marker("m-9", 9)), Budget::default(), &token()).unwrap();
    assert_eq!(d, before);
    assert_eq!(outcome.report.base_revision, 7);
    assert!(validate_media_marker_records(&outcome.records, &d, Budget::default(), &token()).is_ok());
}
