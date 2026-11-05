//! Lane SMB-PR-TRACK-DISPLAY-1, brief item 1: the STU-VID-036 track display record set, its declared
//! defaults, bounds and typed commands. Test names carry the MT they prove.
use hsk_studio_accord::{ActorContext, CancellationToken, ClientSessionId, CommandContext, CommandId, CorrelationId, Ticks};
use hsk_studio_folio::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const F25: u64 = 10_160_640_000;

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn sequence(sequence_id: &str) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: sequence_id.into(),
        name: sequence_id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F25 },
        duration: Ticks::new(0),
        tracks: Vec::new(),
        markers: Vec::new(),
        work_area: None,
    }
}

/// A document with `seq-a` and `seq-b`.
fn document() -> StudioDocument {
    let json = json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    let mut d: StudioDocument = serde_json::from_value(json).unwrap();
    d.sequences.push(sequence("seq-a"));
    d.sequences.push(sequence("seq-b"));
    d
}

fn entry(sequence_id: &str) -> StudioTrackDisplayEntry {
    StudioTrackDisplayEntry {
        sequence_id: sequence_id.into(),
        switches: StudioTrackDisplaySwitches { video_thumbnails: StudioVideoThumbnails::Continuous, clip_names: false, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT },
        heights: StudioTrackHeights { video: 60, audio: 70, caption: 80, submix: 90 },
        height_presets: vec![StudioTrackHeightPreset { preset_id: "tall".into(), name: "Tall tracks".into(), heights: StudioTrackHeights::all(300) }],
    }
}

fn records(entries: Vec<StudioTrackDisplayEntry>) -> StudioTrackDisplayRecords {
    let mut r = StudioTrackDisplayRecords::new(DOC);
    r.sequences = entries;
    r
}

fn parse(bytes: &[u8]) -> Result<StudioTrackDisplayRecords, Diagnostic> {
    parse_track_display_records(bytes, &document(), Budget::default(), &token())
}

fn wire_edited(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = serde_json::to_value(records(vec![entry("seq-a")])).unwrap();
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

fn run(r: &StudioTrackDisplayRecords, cmd: &StudioTrackDisplayCommand) -> Result<StudioTrackDisplayOutcome, StudioTrackDisplayError> {
    apply_track_display_command(&document(), r, &ctx(cmd.command_id(), 7), 7, cmd, Budget::default(), &token())
}

fn empty() -> StudioTrackDisplayRecords {
    StudioTrackDisplayRecords::new(DOC)
}

fn sid() -> String {
    "seq-a".to_owned()
}

#[test]
fn every_declared_switch_is_on_the_wire_by_its_spec_name_mt_33739() {
    let bytes = serde_json::to_vec(&records(vec![entry("seq-a")])).unwrap();
    assert_eq!(parse(&bytes).unwrap(), records(vec![entry("seq-a")]), "the wire form reads back");
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schema_id"], "hsk.studio.track_display@1");
    let switches: BTreeSet<&str> = v["sequences"][0]["switches"].as_object().unwrap().keys().map(String::as_str).collect();
    let declared: BTreeSet<&str> = [
        "video_thumbnails",
        "clip_names",
        "fx_badges",
        "proxy_badges",
        "content_credential_badges",
        "duplicate_frame_markers",
        "clip_markers",
        "source_clip_name_and_label",
        "through_edits",
        "audio_waveforms",
        "audio_names",
        "audio_channel_labels",
        "audio_type_badges",
        "audio_fade_handles",
        "video_fade_handles",
        "audio_keyframes",
        "video_keyframes",
        "audio_clip_headers_on_small_tracks",
    ]
    .into_iter()
    .collect();
    assert_eq!(switches, declared);
    // The closed enumerations, each member by its spec words, and nothing else.
    for (member, wire) in [
        (StudioVideoThumbnails::Off, "off"),
        (StudioVideoThumbnails::HeadOnly, "head_only"),
        (StudioVideoThumbnails::HeadAndTail, "head_and_tail"),
        (StudioVideoThumbnails::Continuous, "continuous"),
    ] {
        assert_eq!(serde_json::to_value(member).unwrap(), wire);
        assert_eq!(parse(&wire_edited(|v| v["sequences"][0]["switches"]["video_thumbnails"] = json!(wire))).unwrap().sequences[0].switches.video_thumbnails, member);
    }
    for (member, wire) in [(StudioWaveformScaling::Logarithmic, "logarithmic"), (StudioWaveformScaling::Dynamic, "dynamic")] {
        assert_eq!(serde_json::to_value(member).unwrap(), wire);
    }
    let waveforms = |w: Value| wire_edited(|v| v["sequences"][0]["switches"]["audio_waveforms"] = w);
    let rectified = parse(&waveforms(json!({"shown": true, "scaling": "dynamic", "rectified": true}))).unwrap();
    assert_eq!(rectified.sequences[0].switches.audio_waveforms, StudioAudioWaveforms { shown: true, scaling: StudioWaveformScaling::Dynamic, rectified: true });
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["switches"]["video_thumbnails"] = json!("sometimes"))), Some(Code::InvalidJson));
    assert_eq!(code_of(waveforms(json!({"shown": true, "scaling": "linear", "rectified": false}))), Some(Code::InvalidJson));
}

#[test]
fn the_wire_refuses_as_the_sequence_settings_refuse_mt_33739() {
    // Unknown fields at every level.
    let unknown: [&dyn Fn(&mut Value); 6] = [
        &|v| v["vendor"] = json!(1),
        &|v| v["sequences"][0]["vendor"] = json!(1),
        &|v| v["sequences"][0]["switches"]["vendor"] = json!(1),
        &|v| v["sequences"][0]["switches"]["audio_waveforms"]["vendor"] = json!(1),
        &|v| v["sequences"][0]["heights"]["vendor"] = json!(1),
        &|v| v["sequences"][0]["height_presets"][0]["vendor"] = json!(1),
    ];
    for edit in unknown {
        assert_eq!(code_of(wire_edited(edit)), Some(Code::InvalidJson));
    }
    // Every switch and height is required.
    let reference = serde_json::to_value(entry("seq-a")).unwrap();
    for key in reference["switches"].as_object().unwrap().keys() {
        assert_eq!(code_of(wire_edited(|v| drop(v["sequences"][0]["switches"].as_object_mut().unwrap().remove(key)))), Some(Code::InvalidJson), "{key}");
    }
    for key in reference["heights"].as_object().unwrap().keys() {
        assert_eq!(code_of(wire_edited(|v| drop(v["sequences"][0]["heights"].as_object_mut().unwrap().remove(key)))), Some(Code::InvalidJson), "{key}");
    }
    assert_eq!(code_of(wire_edited(|v| v["schema_id"] = json!("hsk.studio.track_display@2"))), Some(Code::InvalidJson));
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["switches"]["clip_names"] = json!("yes"))), Some(Code::InvalidJson));
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["heights"]["video"] = json!(1.5))), Some(Code::InvalidJson));
    let repeated = String::from_utf8(serde_json::to_vec(&records(vec![entry("seq-a")])).unwrap()).unwrap().replacen("{", &format!("{{\"document_id\":\"{DOC}\","), 1);
    assert_eq!(code_of(repeated.into_bytes()), Some(Code::DuplicateField));
    assert_eq!(code_of(b"{not json".to_vec()), Some(Code::InvalidJson));
    assert_eq!(code_of("[".repeat(64).into_bytes()), Some(Code::BudgetExceeded));
}

#[test]
fn a_sequence_with_no_entry_resolves_to_the_declared_defaults_mt_33739() {
    let d = document();
    // No records at all, and records naming another sequence: the declared defaults, in document order.
    for held in [None, Some(records(vec![entry("seq-b")]))] {
        let resolved = resolve_track_displays(held.as_ref(), &d);
        assert_eq!(resolved.iter().map(|r| r.sequence_id.as_str()).collect::<Vec<_>>(), ["seq-a", "seq-b"]);
        let first = &resolved[0];
        assert!(!first.stored);
        assert_eq!(first.switches, StudioTrackDisplaySwitches::STUDIO_DEFAULT);
        assert_eq!(first.heights, StudioTrackHeights::STUDIO_DEFAULT);
        assert!(first.height_presets.is_empty());
    }
    let resolved = resolve_track_displays(Some(&records(vec![entry("seq-b")])), &d);
    assert!(resolved[1].stored);
    assert_eq!(resolved[1].switches, entry("seq-b").switches);
    assert_eq!(resolved[1].heights, entry("seq-b").heights);
    assert_eq!(resolved[1].height_presets, entry("seq-b").height_presets);
    // The defaults as declared (vendor parity UNVERIFIED, D5).
    let defaults = StudioTrackDisplaySwitches::STUDIO_DEFAULT;
    assert_eq!(defaults.video_thumbnails, StudioVideoThumbnails::HeadOnly);
    assert_eq!(defaults.audio_waveforms, StudioAudioWaveforms { shown: true, scaling: StudioWaveformScaling::Logarithmic, rectified: false });
    assert!(defaults.clip_names && defaults.through_edits && defaults.video_keyframes && defaults.audio_clip_headers_on_small_tracks);
    assert_eq!((MIN_TRACK_HEIGHT, DEFAULT_TRACK_HEIGHT, EXPANDED_TRACK_HEIGHT, MAX_TRACK_HEIGHT), (16, 48, 200, 4096));
    assert_eq!(StudioTrackHeights::STUDIO_DEFAULT, StudioTrackHeights::all(48));
    // A valid set may name no sequence at all.
    validate_track_display_records(&empty(), &d, Budget::default(), &token()).unwrap();
}

#[test]
fn each_track_kind_has_its_own_height_mt_33739() {
    let heights = StudioTrackHeights { video: 60, audio: 70, caption: 80, submix: 90 };
    let kinds = [(TrackKind::Video, 60), (TrackKind::Audio, 70), (TrackKind::Caption, 80), (TrackKind::Submix, 90)];
    for (kind, height) in kinds {
        assert_eq!(heights.of(kind), height);
        let cmd = StudioTrackDisplayCommand::SetHeight { sequence_id: sid(), kind, height };
        let out = run(&empty(), &cmd).unwrap().records;
        let mut expected = StudioTrackHeights::STUDIO_DEFAULT;
        match kind {
            TrackKind::Video => expected.video = height,
            TrackKind::Audio => expected.audio = height,
            TrackKind::Caption => expected.caption = height,
            TrackKind::Submix => expected.submix = height,
        }
        assert_eq!(out.entry("seq-a").unwrap().heights, expected, "{kind:?}");
    }
}

#[test]
fn records_are_bound_to_the_document_and_hostile_input_is_refused() {
    let d = document();
    let check = |r: &StudioTrackDisplayRecords| validate_track_display_records(r, &d, Budget::default(), &token()).err().map(|e| e.code);
    assert_eq!(check(&records(vec![entry("seq-a"), entry("seq-b")])), None);
    let mut other = records(vec![]);
    other.document_id = "SDOC-019abcde-0000-7000-8000-000000000002".into();
    assert_eq!(check(&other), Some(Code::InvalidField));
    let mut malformed = records(vec![]);
    malformed.document_id = "not-a-document".into();
    assert!(check(&malformed).is_some());
    assert_eq!(check(&records(vec![entry("seq-x")])), Some(Code::DanglingReference));
    assert_eq!(check(&records(vec![entry("seq-a"), entry("seq-a")])), Some(Code::DuplicateIdentity));
    assert_eq!(check(&records(vec![entry("bad id!")])), Some(Code::InvalidId));
    let edited = |edit: &dyn Fn(&mut StudioTrackDisplayEntry)| {
        let mut e = entry("seq-a");
        edit(&mut e);
        records(vec![e])
    };
    for height in [0, MIN_TRACK_HEIGHT - 1, MAX_TRACK_HEIGHT + 1, u32::MAX] {
        assert_eq!(check(&edited(&|e| e.heights.video = height)), Some(Code::InvalidField), "video {height}");
        assert_eq!(check(&edited(&|e| e.heights.submix = height)), Some(Code::InvalidField), "submix {height}");
        assert_eq!(check(&edited(&|e| e.height_presets[0].heights.caption = height)), Some(Code::InvalidField), "preset {height}");
    }
    for height in [MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT] {
        assert_eq!(check(&edited(&|e| e.heights = StudioTrackHeights::all(height))), None);
    }
    assert_eq!(check(&edited(&|e| e.height_presets[0].preset_id = "bad id!".into())), Some(Code::InvalidId));
    assert_eq!(check(&edited(&|e| e.height_presets.push(e.height_presets[0].clone()))), Some(Code::DuplicateIdentity));
    let long = "x".repeat(129);
    for name in ["", "   ", "line\nbreak", long.as_str()] {
        assert_eq!(check(&edited(&|e| e.height_presets[0].name = name.into())), Some(Code::InvalidField), "{name:?}");
    }
    let many = |n: usize| {
        let mut e = entry("seq-a");
        e.height_presets = (0..n).map(|i| StudioTrackHeightPreset { preset_id: format!("p{i}"), name: format!("P {i}"), heights: StudioTrackHeights::all(100) }).collect();
        records(vec![e])
    };
    assert_eq!(check(&many(MAX_TRACK_HEIGHT_PRESETS)), None);
    assert_eq!(check(&many(MAX_TRACK_HEIGHT_PRESETS + 1)), Some(Code::BudgetExceeded));
    let entries_over: Vec<_> = (0..=MAX_SEQUENCES).map(|i| entry(&format!("s{i}"))).collect();
    assert_eq!(check(&records(entries_over)), Some(Code::BudgetExceeded), "the entry count is checked before any entry is walked");
}

#[test]
fn dims_u32_max_and_aggregate_over_budget_are_refused() {
    // dims_u32_max: the largest value the wire type holds, and the first it does not.
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["heights"]["audio"] = json!(u32::MAX))), Some(Code::InvalidField));
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["heights"]["audio"] = json!(u64::from(u32::MAX) + 1))), Some(Code::InvalidJson));
    assert_eq!(code_of(wire_edited(|v| v["sequences"][0]["heights"]["audio"] = json!(-1))), Some(Code::InvalidJson));
    // aggregate_over_budget: entries and presets count together against the node budget, and the
    // preset texts against the payload budget.
    let d = document();
    let r = records(vec![entry("seq-a"), entry("seq-b")]);
    let with = |b: Budget| validate_track_display_records(&r, &d, b, &token()).err().map(|e| e.code);
    assert_eq!(with(Budget { nodes: 4, ..Budget::default() }), None);
    assert_eq!(with(Budget { nodes: 3, ..Budget::default() }), Some(Code::BudgetExceeded), "two entries and two presets are four nodes");
    assert_eq!(with(Budget { payload_bytes: 1, ..Budget::default() }), Some(Code::BudgetExceeded));
    // The declared byte limit holds whatever budget the caller passes.
    let padded = {
        let mut body = serde_json::to_vec(&records(vec![entry("seq-a")])).unwrap();
        body.resize(MAX_TRACK_DISPLAY_INPUT_BYTES + 1, b' ');
        body
    };
    let generous = Budget { input_bytes: MAX_TRACK_DISPLAY_INPUT_BYTES * 4, ..Budget::default() };
    assert_eq!(parse_track_display_records(&padded, &d, generous, &token()).unwrap_err().code, Code::BudgetExceeded);
    let body = serde_json::to_vec(&records(vec![entry("seq-a")])).unwrap();
    let tight = Budget { input_bytes: body.len() - 1, ..Budget::default() };
    assert_eq!(parse_track_display_records(&body, &d, tight, &token()).unwrap_err().code, Code::BudgetExceeded);
}

#[test]
fn the_records_hold_no_copy_of_a_document_value() {
    // F-R55-4(d) must not come back here: only the sequence id ties an entry to the document.
    let r = records(vec![entry("seq-a")]);
    let mut d = document();
    d.sequences[0].frame_rate = SequenceFrameRate { ticks_per_frame: 8_475_667_200 };
    d.sequences[0].name = "renamed".into();
    d.sequences[0].duration = Ticks::new(F25);
    validate_track_display_records(&r, &d, Budget::default(), &token()).unwrap();
    d.sequences.remove(0);
    assert_eq!(validate_track_display_records(&r, &d, Budget::default(), &token()).unwrap_err().code, Code::DanglingReference);
}

#[test]
fn the_commands_are_typed_attributed_and_atomic_mt_33739() {
    assert_eq!(
        TRACK_DISPLAY_COMMAND_IDS,
        ["track_display.switches.set", "track_display.height.set", "track_display.expand_all", "track_display.minimize_all", "track_display.height_preset.save", "track_display.height_preset.recall"]
    );
    let switches = StudioTrackDisplaySwitches { fx_badges: false, ..StudioTrackDisplaySwitches::STUDIO_DEFAULT };
    let out = run(&empty(), &StudioTrackDisplayCommand::SetSwitches { sequence_id: sid(), switches }).unwrap();
    let held = out.records.entry("seq-a").unwrap();
    assert_eq!((held.switches, held.heights), (switches, StudioTrackHeights::STUDIO_DEFAULT));
    assert_eq!(out.report, StudioTrackDisplayReport { command: "track_display.switches.set", base_revision: 7, correlation: 41, entries: 1, height_presets: 0, changed: true });

    // Expand-all and minimize-all set every kind, and keep the rest of the entry.
    let start = records(vec![entry("seq-a")]);
    let expanded = run(&start, &StudioTrackDisplayCommand::ExpandAll { sequence_id: sid() }).unwrap().records;
    assert_eq!(expanded.entry("seq-a").unwrap().heights, StudioTrackHeights::all(EXPANDED_TRACK_HEIGHT));
    assert_eq!(expanded.entry("seq-a").unwrap().switches, entry("seq-a").switches);
    assert_eq!(expanded.entry("seq-a").unwrap().height_presets, entry("seq-a").height_presets);
    let minimized = run(&start, &StudioTrackDisplayCommand::MinimizeAll { sequence_id: sid() }).unwrap().records;
    assert_eq!(minimized.entry("seq-a").unwrap().heights, StudioTrackHeights::all(MIN_TRACK_HEIGHT));

    // Save the current heights under a name, change them, recall them.
    let save = StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: sid(), preset_id: "mine".into(), name: "My tracks".into() };
    let saved = run(&start, &save).unwrap();
    assert_eq!(saved.report.height_presets, 2);
    let preset = saved.records.entry("seq-a").unwrap().height_presets[1].clone();
    assert_eq!(preset, StudioTrackHeightPreset { preset_id: "mine".into(), name: "My tracks".into(), heights: entry("seq-a").heights });
    let moved = run(&saved.records, &StudioTrackDisplayCommand::ExpandAll { sequence_id: sid() }).unwrap().records;
    let recall = StudioTrackDisplayCommand::RecallHeightPreset { sequence_id: sid(), preset_id: "mine".into() };
    let back = run(&moved, &recall).unwrap().records;
    assert_eq!(back, saved.records, "recall restores the saved heights and keeps the preset");

    // A sequence without an entry can save and expand from the defaults, and its neighbour is untouched.
    let from_default = run(&records(vec![entry("seq-b")]), &save).unwrap().records;
    assert_eq!(from_default.entry("seq-a").unwrap().height_presets[0].heights, StudioTrackHeights::STUDIO_DEFAULT);
    assert_eq!(from_default.entry("seq-b"), Some(&entry("seq-b")));

    // Refusals, each with its code.
    let cases: Vec<(StudioTrackDisplayCommand, &str)> = vec![
        (StudioTrackDisplayCommand::ExpandAll { sequence_id: "seq-x".into() }, "sequence_missing"),
        (StudioTrackDisplayCommand::SetHeight { sequence_id: sid(), kind: TrackKind::Video, height: 0 }, "invalid_height"),
        (StudioTrackDisplayCommand::SetHeight { sequence_id: sid(), kind: TrackKind::Audio, height: u32::MAX }, "invalid_height"),
        (StudioTrackDisplayCommand::RecallHeightPreset { sequence_id: sid(), preset_id: "none".into() }, "preset_missing"),
        (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: sid(), preset_id: "tall".into(), name: "Again".into() }, "duplicate_preset"),
        (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: sid(), preset_id: "bad id!".into(), name: "Bad".into() }, "invalid_preset_id"),
        (StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: sid(), preset_id: "ok".into(), name: "  ".into() }, "invalid_name"),
    ];
    for (cmd, code) in cases {
        assert_eq!(run(&start, &cmd).unwrap_err().code(), code, "{cmd:?}");
    }
    let mut full = entry("seq-a");
    full.height_presets = (0..MAX_TRACK_HEIGHT_PRESETS).map(|i| StudioTrackHeightPreset { preset_id: format!("p{i}"), name: format!("P {i}"), heights: StudioTrackHeights::all(100) }).collect();
    assert_eq!(run(&records(vec![full]), &save).unwrap_err(), StudioTrackDisplayError::BudgetExceeded, "a preset over the declared count");

    // The context decides who and when, as the sequence settings commands.
    let expand = StudioTrackDisplayCommand::ExpandAll { sequence_id: sid() };
    let wrong = apply_track_display_command(&document(), &start, &ctx("track_display.minimize_all", 7), 7, &expand, Budget::default(), &token());
    assert_eq!(wrong.unwrap_err(), StudioTrackDisplayError::CommandMismatch);
    let stale = apply_track_display_command(&document(), &start, &ctx("track_display.expand_all", 6), 7, &expand, Budget::default(), &token());
    assert_eq!(stale.unwrap_err(), StudioTrackDisplayError::StaleRevision { expected: 6, current: 7 });
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let no_command = CommandContext::new(actor, ClientSessionId::parse("window-1").unwrap(), None, 7, CorrelationId::new(41), None);
    let missing = apply_track_display_command(&document(), &start, &no_command, 7, &expand, Budget::default(), &token());
    assert_eq!(missing.unwrap_err(), StudioTrackDisplayError::CommandMissing);
}

#[test]
fn a_command_that_changes_nothing_adds_no_entry() {
    // An entry a command did not need would stand in the way of removing the sequence for nothing.
    let set = StudioTrackDisplayCommand::SetSwitches { sequence_id: sid(), switches: StudioTrackDisplaySwitches::STUDIO_DEFAULT };
    let out = run(&empty(), &set).unwrap();
    assert_eq!(out.records, empty());
    assert!(!out.report.changed);
    let height = StudioTrackDisplayCommand::SetHeight { sequence_id: sid(), kind: TrackKind::Video, height: DEFAULT_TRACK_HEIGHT };
    assert_eq!(run(&empty(), &height).unwrap().records, empty());
    let start = records(vec![entry("seq-a")]);
    let again = StudioTrackDisplayCommand::SetSwitches { sequence_id: sid(), switches: entry("seq-a").switches };
    let out = run(&start, &again).unwrap();
    assert_eq!(out.records, start);
    assert!(!out.report.changed);
}

#[test]
fn command_codes_tell_cancellation_and_budget_from_bad_input_r55_f6() {
    let start = records(vec![entry("seq-a")]);
    let expand = StudioTrackDisplayCommand::ExpandAll { sequence_id: sid() };
    let with = |r: &StudioTrackDisplayRecords, b: Budget, t: &CancellationToken| apply_track_display_command(&document(), r, &ctx(expand.command_id(), 7), 7, &expand, b, t);
    with(&start, Budget::default(), &token()).unwrap();
    let canceled = token();
    canceled.cancel();
    let stopped = with(&start, Budget::default(), &canceled).unwrap_err();
    assert_eq!((stopped, stopped.code()), (StudioTrackDisplayError::Canceled, "canceled"));
    let over = with(&start, Budget { nodes: 0, ..Budget::default() }, &token()).unwrap_err();
    assert_eq!((over, over.code()), (StudioTrackDisplayError::BudgetExceeded, "budget_exceeded"));
    let mut hostile = start.clone();
    hostile.sequences[0].sequence_id = "seq-x".into();
    let bad = with(&hostile, Budget::default(), &token()).unwrap_err();
    assert_eq!((bad, bad.code()), (StudioTrackDisplayError::InvalidInput(Code::DanglingReference), "invalid_input"));
    // A result that a parse under this budget would refuse is refused here, not at the next open.
    let small = Budget { input_bytes: 64, ..Budget::default() };
    assert_eq!(with(&start, small, &token()).unwrap_err(), StudioTrackDisplayError::BudgetExceeded);
}

#[test]
fn the_report_holds_codes_and_counters_only() {
    let save = StudioTrackDisplayCommand::SaveHeightPreset { sequence_id: sid(), preset_id: "secret-id".into(), name: "Secret name".into() };
    let report = format!("{:?}", run(&empty(), &save).unwrap().report);
    for private in ["seq-a", "secret-id", "Secret name", DOC] {
        assert!(!report.contains(private), "{private} in {report}");
    }
}

#[test]
fn the_schema_requires_every_switch() {
    let schema = serde_json::to_value(track_display_records_schema()).unwrap();
    let required: BTreeSet<&str> = schema["$defs"]["StudioTrackDisplaySwitches"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(required.len(), 18);
    assert_eq!(schema["$defs"]["StudioTrackDisplaySwitches"]["additionalProperties"], false);
    assert_eq!(schema["properties"]["schema_id"]["$ref"], "#/$defs/TrackDisplaySchema");
    assert_eq!(serde_json::to_value(TrackDisplaySchema::V1).unwrap(), "hsk.studio.track_display@1");
}
