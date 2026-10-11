use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use serde_json::{Value, json};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}
fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller(Resolution);
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        self.0
    }
    fn profile(&self, _: &str) -> Resolution {
        self.0
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        self.0
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn story_content(&self, _: &str, _: &ContentDigest) -> Resolution {
        self.0
    }
}
fn pt(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn edges(v: f64) -> Value {
    json!({"top": pt(v), "right": pt(v), "bottom": pt(v), "left": pt(v)})
}
fn page(key: &str) -> Value {
    json!({"page_id": key, "width": pt(612.0), "height": pt(792.0), "side": "right_hand",
        "margins": edges(36.0), "bleed": null})
}
fn spread(n: u32, pages: Vec<Value>) -> Value {
    let mut v = json!({"schema_id": "hsk.studio.page_spread@1", "page_spread_id": identity("SPGS", n),
        "name": format!("spread {n}"), "parent": null,
        "bounds": {"x": pt(0.0), "y": pt(0.0), "width": pt(1224.0), "height": pt(792.0)}});
    if !pages.is_empty() {
        v["pages"] = Value::Array(pages);
    }
    v
}
fn frame_json(page: &str, previous: Option<u32>, next: Option<u32>) -> Value {
    json!({"page_id": page,
        "bounds": {"x": pt(36.0), "y": pt(36.0), "width": pt(540.0), "height": pt(720.0)},
        "inset_spacing": edges(4.0), "text_column_count": 2, "text_column_gutter": pt(12.0),
        "first_baseline_offset": "ascent", "minimum_first_baseline_offset": pt(0.0),
        "vertical_justification": "top", "auto_sizing_type": "off",
        "previous_text_frame": previous.map(|n| identity("SLYR", n)),
        "next_text_frame": next.map(|n| identity("SLYR", n))})
}
/// Text layer `n` of story `story` on spread 1 carrying a frame; `frame` of None is a legacy text layer.
fn text_layer(n: u32, story: u32, frame: Option<Value>) -> Value {
    let mut v = json!({"schema_id": "hsk.studio.layer@1", "layer_id": identity("SLYR", n),
        "name": format!("frame {n}"),
        "parent": {"node_kind": "page_spread", "node_id": identity("SPGS", 1)},
        "kind": "text", "payload": {"payload_kind": "text", "story_id": identity("STXT", story)},
        "visible": true});
    if let Some(f) = frame {
        v["text_frame"] = f;
    }
    v
}
fn raster_layer(n: u32) -> Value {
    json!({"schema_id": "hsk.studio.layer@1", "layer_id": identity("SLYR", n), "name": "plain",
        "parent": null, "kind": "raster", "payload": {"payload_kind": "raster", "tiles": []}, "visible": true})
}
fn document(spreads: Vec<Value>, layers: Vec<Value>) -> Value {
    json!({"schema_id": "hsk.studio.document@1", "document_id": identity("SDOC", 1), "revision": 1,
        "geometry_unit": "pt", "colour_mode": 2, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null,
        "working_space_spot": null, "blending_space": null,
        "artboards": [], "page_spreads": spreads, "layers": layers,
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []}})
}
/// Spread 1 with pages p1 and p2; frames 1 -> 2 -> 3 of story 1 over p1, p2, p2; raster layer 4.
fn threaded() -> Value {
    document(
        vec![spread(1, vec![page("p1"), page("p2")])],
        vec![
            text_layer(1, 1, Some(frame_json("p1", None, Some(2)))),
            text_layer(2, 1, Some(frame_json("p2", Some(1), Some(3)))),
            text_layer(3, 1, Some(frame_json("p2", Some(2), None))),
            raster_layer(4),
        ],
    )
}
fn inspect(v: &Value) -> Result<Inspection, Diagnostic> {
    inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        Budget::default(),
        &CancellationToken::default(),
        &Caller(Resolution::Available),
    )
}
fn open(v: &Value) -> Snapshot {
    match inspect(v).expect("valid document") {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("expected an editable snapshot"),
    }
}
fn refused(v: &Value, code: Code, target_suffix: &str) {
    let d = inspect(v).expect_err("must be refused");
    assert_eq!(d.code, code, "{}", d.target);
    assert!(
        d.target.ends_with(target_suffix),
        "target {} lacks {target_suffix}",
        d.target
    );
}
/// Sets (replaces or inserts) the value at the JSON pointer.
fn set(v: &mut Value, pointer: &str, value: Value) {
    let (parent, last) = pointer.rsplit_once('/').expect("pointer");
    match v.pointer_mut(parent).expect("parent") {
        Value::Array(a) => a[last.parse::<usize>().expect("index")] = value,
        Value::Object(o) => {
            o.insert(last.to_owned(), value);
        }
        _ => panic!("pointer parent is a scalar"),
    }
}
/// `threaded()` with the JSON pointer's value set.
fn with(pointer: &str, value: Value) -> Value {
    let mut v = threaded();
    set(&mut v, pointer, value);
    v
}

#[test]
fn pages_frames_and_threads_round_trip_and_old_documents_are_unchanged() {
    bounded_case(
        "pages_frames_and_threads_round_trip_and_old_documents_are_unchanged",
        || {
            // A document without any layout member stays valid and does not encode the new keys.
            let old = open(&document(
                vec![spread(1, vec![])],
                vec![text_layer(1, 1, None), raster_layer(4)],
            ));
            let encoded = serde_json::to_string(old.document()).unwrap();
            assert!(!encoded.contains("\"pages\"") && !encoded.contains("text_frame"));
            assert!(old.document().text_threads().is_empty());

            let snapshot = open(&threaded());
            let doc = snapshot.document();
            assert_eq!(doc.page_spreads[0].pages.len(), 2);
            assert_eq!(doc.page_spreads[0].pages[1].side, PageSide::RightHand);
            let threads = doc.text_threads();
            assert_eq!(threads.len(), 1);
            let (l1, l2, l3) = (
                identity("SLYR", 1),
                identity("SLYR", 2),
                identity("SLYR", 3),
            );
            let t = &threads[0];
            assert_eq!(t.story_id(), identity("STXT", 1));
            assert_eq!(t.frames(), [l1.clone(), l2.clone(), l3.clone()]);
            assert_eq!(
                (t.start_text_frame(), t.end_text_frame()),
                (l1.as_str(), l3.as_str())
            );
            assert_eq!(t.text_frame_index(&l2), Some(1));
            assert_eq!(
                (t.previous_text_frame(&l2), t.next_text_frame(&l2)),
                (Some(l1.as_str()), Some(l3.as_str()))
            );
            assert_eq!(t.next_text_frame(&l3), None);
            let located = doc.locate_page("p2").expect("page p2");
            assert_eq!(located.spread_index, 0);
            assert_eq!(
                (located.page_index, located.spread.page_spread_id.as_str()),
                (1, identity("SPGS", 1).as_str())
            );
            assert!(doc.locate_page("missing").is_none());

            // The serialized form decodes to an equal document, re-validates, and re-encodes identically.
            let wire = serde_json::to_vec(doc).unwrap();
            let decoded: StudioDocument = serde_json::from_slice(&wire).unwrap();
            assert_eq!(&decoded, doc);
            let again = validate_document(
                decoded,
                Budget::default(),
                &CancellationToken::default(),
                &Caller(Resolution::Available),
            )
            .expect("re-validates");
            assert_eq!(again.encoded_bytes(), wire);
            assert_eq!(
                open(&serde_json::from_slice(&wire).unwrap()).document(),
                doc
            );

            // A frame survives the existing rename edit and the generated schema describes the members.
            let renamed = snapshot
                .rename(
                    &l2,
                    "renamed".into(),
                    1,
                    2,
                    Budget::default(),
                    &CancellationToken::default(),
                    &Caller(Resolution::Available),
                )
                .expect("rename keeps the frame");
            assert_eq!(
                renamed.document().layers[1].text_frame,
                doc.layers[1].text_frame
            );
            let schema = serde_json::to_string(&document_schema()).unwrap();
            for token in [
                "StudioPage",
                "StudioTextFrame",
                "previous_text_frame",
                "inset_spacing",
                "text_column_count",
            ] {
                assert!(schema.contains(token), "schema lacks {token}");
            }
            assert!(DESCRIPTOR.contains("previous/next thread links"));

            // Unknown fields and unknown enum members keep the complete input read-only.
            for mutated in [
                with("/page_spreads/0/pages/0/future", json!(1)),
                with("/page_spreads/0/pages/0/side", json!("diagonal")),
                with(
                    "/layers/0/text_frame/auto_sizing_type",
                    json!("future_mode"),
                ),
                with("/layers/0/text_frame/future", json!(true)),
            ] {
                let bytes = serde_json::to_vec_pretty(&mutated).unwrap();
                match inspect_bytes(
                    &bytes,
                    Budget::default(),
                    &CancellationToken::default(),
                    &Unresolved,
                )
                .unwrap()
                {
                    Inspection::ReadOnly(r) => assert_eq!(r.encoded_bytes(), bytes),
                    Inspection::Editable(_) => panic!("unsupported layout input became editable"),
                }
            }
        },
    );
}

#[test]
fn thread_cycles_and_broken_chains_are_refused() {
    bounded_case("thread_cycles_and_broken_chains_are_refused", || {
        let two = |a: (Option<u32>, Option<u32>), b: (Option<u32>, Option<u32>)| {
            document(
                vec![spread(1, vec![page("p1")])],
                vec![
                    text_layer(1, 1, Some(frame_json("p1", a.0, a.1))),
                    text_layer(2, 1, Some(frame_json("p1", b.0, b.1))),
                ],
            )
        };
        // Reciprocal loop, one-way loops (no predecessors), a predecessor-only loop and a self link.
        refused(
            &two((Some(2), Some(2)), (Some(1), Some(1))),
            Code::ThreadCycle,
            "",
        );
        refused(
            &two((None, Some(2)), (None, Some(1))),
            Code::ThreadCycle,
            "",
        );
        refused(
            &two((Some(2), None), (Some(1), None)),
            Code::ThreadCycle,
            "",
        );
        refused(
            &two((None, Some(1)), (None, None)),
            Code::ThreadCycle,
            &identity("SLYR", 1),
        );
        // A chain that is not reciprocal is not a thread.
        refused(
            &two((None, Some(2)), (None, None)),
            Code::InvalidThread,
            "/next",
        );
        refused(
            &two((None, None), (Some(1), None)),
            Code::InvalidThread,
            "/previous",
        );
        // One story per chain: a chain may not mix stories, and a story is one chain.
        refused(
            &with("/layers/1/payload/story_id", json!(identity("STXT", 2))),
            Code::InvalidThread,
            "/story",
        );
        let split = document(
            vec![spread(1, vec![page("p1")])],
            vec![
                text_layer(1, 1, Some(frame_json("p1", None, None))),
                text_layer(2, 1, Some(frame_json("p1", None, Some(3)))),
                text_layer(3, 1, Some(frame_json("p1", Some(2), None))),
            ],
        );
        refused(&split, Code::InvalidThread, "/story");
        // Distinct stories in separate chains are fine.
        let mut ok = split.clone();
        ok["layers"][0]["payload"]["story_id"] = json!(identity("STXT", 2));
        assert_eq!(open(&ok).document().text_threads().len(), 2);
    });
}

#[test]
fn dangling_frames_pages_and_malformed_geometry_are_refused() {
    bounded_case(
        "dangling_frames_pages_and_malformed_geometry_are_refused",
        || {
            refused(
                &with(
                    "/layers/2/text_frame/previous_text_frame",
                    json!(identity("SLYR", 99)),
                ),
                Code::DanglingReference,
                "/previous",
            );
            refused(
                &with(
                    "/layers/1/text_frame/next_text_frame",
                    json!(identity("SLYR", 99)),
                ),
                Code::DanglingReference,
                "/next",
            );
            // An existing layer that is not a frame is not a thread member.
            refused(
                &with(
                    "/layers/2/text_frame/previous_text_frame",
                    json!(identity("SLYR", 4)),
                ),
                Code::DanglingReference,
                "/previous",
            );
            refused(
                &with(
                    "/layers/1/text_frame/next_text_frame",
                    json!(identity("STXT", 3)),
                ),
                Code::InvalidId,
                "/next",
            );
            // Page references: unknown page, wrong spread, frame on a non-text layer.
            refused(
                &with("/layers/0/text_frame/page_id", json!("nope")),
                Code::DanglingReference,
                "/page",
            );
            let mut other = threaded();
            other["page_spreads"]
                .as_array_mut()
                .unwrap()
                .push(spread(2, vec![]));
            other["layers"][0]["parent"] =
                json!({"node_kind": "page_spread", "node_id": identity("SPGS", 2)});
            refused(&other, Code::InvalidField, "/page");
            let mut raster = threaded();
            raster["layers"][3]["text_frame"] = frame_json("p1", None, None);
            refused(&raster, Code::PayloadMismatch, &identity("SLYR", 4));
            // Frame geometry.
            refused(
                &with(
                    "/layers/0/text_frame/bounds/width",
                    json!({"value": 540.0, "unit": "mm"}),
                ),
                Code::InvalidUnit,
                "/bounds",
            );
            refused(
                &with("/layers/0/text_frame/bounds/height", pt(0.0)),
                Code::InvalidField,
                "/bounds",
            );
            refused(
                &with("/layers/0/text_frame/inset_spacing", edges(300.0)),
                Code::InvalidField,
                "/inset_spacing",
            );
            refused(
                &with("/layers/0/text_frame/text_column_count", json!(0)),
                Code::InvalidField,
                "/text_column_count",
            );
            refused(
                &with("/layers/0/text_frame/text_column_gutter", pt(600.0)),
                Code::InvalidField,
                "/text_column_count",
            );
            refused(
                &with(
                    "/layers/0/text_frame/minimum_first_baseline_offset",
                    pt(-1.0),
                ),
                Code::InvalidUnit,
                "/minimum_first_baseline_offset",
            );
            // Pages: duplicate key, degenerate size, margins that leave no box, negative bleed, bad key.
            refused(
                &with("/page_spreads/0/pages/1/page_id", json!("p1")),
                Code::DuplicateIdentity,
                "p1",
            );
            refused(
                &with("/page_spreads/0/pages/0/width", pt(0.0)),
                Code::InvalidField,
                "/size",
            );
            refused(
                &with("/page_spreads/0/pages/0/margins", edges(400.0)),
                Code::InvalidField,
                "/margins",
            );
            let bleed = json!({"top": pt(-3.0), "bottom": pt(9.0), "inside_or_left": pt(9.0), "outside_or_right": pt(9.0)});
            refused(
                &with("/page_spreads/0/pages/0/bleed", bleed),
                Code::InvalidUnit,
                "/bleed",
            );
            refused(
                &with("/page_spreads/0/pages/0/page_id", json!("a/b")),
                Code::InvalidId,
                "page_id",
            );
            let good_bleed = json!({"top": pt(9.0), "bottom": pt(9.0), "inside_or_left": pt(9.0), "outside_or_right": pt(9.0)});
            open(&with("/page_spreads/0/pages/0/bleed", good_bleed));
            // Pages are charged to the caller budget.
            let tight = Budget {
                nodes: 5,
                ..Budget::default()
            };
            let d = inspect_bytes(
                &serde_json::to_vec(&threaded()).unwrap(),
                tight,
                &CancellationToken::default(),
                &Caller(Resolution::Available),
            )
            .expect_err("two pages and four layers exceed five nodes");
            assert_eq!(d.code, Code::BudgetExceeded);
        },
    );
}

fn story_json() -> Value {
    // "héllo\nworld": h(1) é(2) l l o(3) \n(1) = 7 bytes, then world(5) = 12 bytes.
    json!({"schema_id": "hsk.studio.text_story@1", "story_id": identity("STXT", 1), "text_kind": "area",
        "story_direction": "left_to_right",
        "content": {"content_kind": "inline", "text_utf8": "h\u{e9}llo\nworld"},
        "paragraphs": [
            {"start": 0, "end": 7, "style_ref": null, "paragraph_direction": null},
            {"start": 7, "end": 12, "style_ref": identity("STYS", 1), "paragraph_direction": null}],
        "character_ranges": [
            {"start": 0, "end": 6, "style_ref": null, "applied_language": "en-US"},
            {"start": 6, "end": 12, "style_ref": identity("STYS", 2), "applied_language": "nl_NL"}]})
}
fn story_issues(v: &Value, r: &dyn Resolver) -> Result<Vec<ResolutionIssue>, Diagnostic> {
    let s: StudioTextStory = serde_json::from_value(v.clone()).expect("decodes");
    validate_text_story(&s, Budget::default(), &CancellationToken::default(), r)
}
fn story_refused(v: &Value, code: Code, target_suffix: &str) {
    let d = story_issues(v, &Caller(Resolution::Available)).expect_err("must be refused");
    assert_eq!(d.code, code, "{}", d.target);
    assert!(
        d.target.ends_with(target_suffix),
        "target {} lacks {target_suffix}",
        d.target
    );
}
fn story_with(pointer: &str, value: Value) -> Value {
    let mut v = story_json();
    set(&mut v, pointer, value);
    v
}

#[test]
fn text_story_content_partitions_and_refs_round_trip_and_validate() {
    bounded_case(
        "text_story_content_partitions_and_refs_round_trip_and_validate",
        || {
            let v = story_json();
            assert!(
                story_issues(&v, &Caller(Resolution::Available))
                    .expect("valid story")
                    .is_empty()
            );
            let story: StudioTextStory = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(
                (story.byte_len(), story.inline_text()),
                (12, Some("h\u{e9}llo\nworld"))
            );
            assert_eq!(serde_json::to_value(&story).unwrap(), v);
            let schema = serde_json::to_string(&text_story_schema()).unwrap();
            assert!(
                schema.contains("hsk.studio.text_story@1") && schema.contains("applied_language")
            );
            // Unresolved style bindings are reported once per style, never silently accepted.
            let issues = story_issues(&v, &Unresolved).expect("valid story, unresolved refs");
            assert_eq!(issues.len(), 2);
            assert!(
                issues
                    .iter()
                    .all(|i| i.disposition == Resolution::Absent && i.target.contains("/style/"))
            );
            // Partitions must tile the stream exactly, on scalar boundaries.
            story_refused(
                &story_with("/paragraphs/1/start", json!(8)),
                Code::InvalidField,
                "/paragraphs/1",
            );
            story_refused(
                &story_with("/paragraphs/1/end", json!(11)),
                Code::InvalidField,
                "/paragraphs",
            );
            story_refused(
                &story_with("/character_ranges/0/end", json!(2)),
                Code::InvalidField,
                "/character_ranges/0",
            );
            story_refused(
                &story_with("/character_ranges/1/end", json!(13)),
                Code::InvalidField,
                "/character_ranges/1",
            );
            // Identities, bindings and direction.
            story_refused(
                &story_with("/story_id", json!(identity("SLYR", 1))),
                Code::InvalidId,
                "story_id",
            );
            story_refused(
                &story_with("/paragraphs/1/style_ref", json!(identity("SLYR", 1))),
                Code::InvalidId,
                "/style_ref",
            );
            story_refused(
                &story_with("/character_ranges/0/applied_language", json!("en US")),
                Code::InvalidField,
                "/applied_language",
            );
            story_refused(
                &story_with("/story_direction", json!("unknown")),
                Code::InvalidField,
                "/paragraph_direction",
            );
            let mut explicit = story_with("/story_direction", json!("unknown"));
            explicit["paragraphs"][0]["paragraph_direction"] = json!("left_to_right");
            explicit["paragraphs"][1]["paragraph_direction"] = json!("right_to_left");
            story_issues(&explicit, &Caller(Resolution::Available)).expect("explicit directions");
            // Stored content is a content-addressed handle, never a path, and is resolved by its owner.
            let stored = |manifest: &str| {
                story_with(
                    "/content",
                    json!({"content_kind": "stored", "artifact_manifest_id": manifest,
                    "content_digest": {"algorithm": "sha256", "digest": "ab12"}, "byte_len": 12}),
                )
            };
            assert!(
                story_issues(&stored("manifest-a"), &Caller(Resolution::Available))
                    .unwrap()
                    .is_empty()
            );
            let unresolved = story_issues(&stored("manifest-a"), &Unresolved).unwrap();
            assert!(unresolved.iter().any(|i| i.target.ends_with("/content")));
            story_refused(&stored("media/a.txt"), Code::InvalidField, "/content");
            // Unknown fields and a missing explicit-null field are decode errors, not defaults.
            let mut extra = story_json();
            extra["future"] = json!(1);
            assert!(serde_json::from_value::<StudioTextStory>(extra).is_err());
            let mut missing = story_json();
            missing["paragraphs"][0]
                .as_object_mut()
                .unwrap()
                .remove("style_ref");
            assert!(serde_json::from_value::<StudioTextStory>(missing).is_err());
        },
    );
}
