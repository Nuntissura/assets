use hsk_studio_accord::{CancellationToken, Ticks};
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
/// 24 fps in ticks per frame (STU-VID-013).
const TPF: u64 = 10_584_000_000;
fn clip(n: u32, source_in: u64, source_out: u64, start: u64, duration: u64) -> Value {
    json!({"schema_id":"hsk.studio.clip@1","clip_id":identity("SCLP",n),
        "source_ref":{"source_kind":"media","artifact_manifest_id":"manifest-a","content_digest":{"algorithm":"sha256","digest":"abc123"}},
        "source_in":source_in,"source_out":source_out,"timeline_start":start,"duration":duration,
        "speed":{"numerator":1,"denominator":1},"enabled":true,"link_group":null})
}
fn track(n: u32, kind: &str, index: u32, clips: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.track@1","track_id":identity("STRK",n),"kind":kind,"index":index,
        "name":format!("track-{n}"),"enabled":true,"locked":false,"sync_locked":true,"targeted":true,
        "muted":false,"soloed":false,"clips":clips})
}
fn sequence(key: &str, rate: u64, tracks: Vec<Value>, duration: u64) -> Value {
    json!({"schema_id":"hsk.studio.sequence@1","sequence_id":key,"name":"main",
        "frame_rate":{"ticks_per_frame":rate},"duration":duration,"tracks":tracks})
}
fn document(sequences: Vec<Value>) -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    if !sequences.is_empty() {
        v["sequences"] = Value::Array(sequences);
    }
    v
}
/// Video track [A 0..1000 @0, B 500..2000 @1000 touching A], audio track [reversed @200, 2x @1200].
fn standard() -> Value {
    let mut reversed = clip(3, 0, 1000, 200, 1000);
    reversed["speed"] = json!({"numerator":-1,"denominator":1});
    let mut double = clip(4, 0, 1000, 1200, 500);
    double["speed"] = json!({"numerator":2,"denominator":1});
    document(vec![sequence(
        "seq-main",
        TPF,
        vec![
            track(
                1,
                "video",
                0,
                vec![clip(1, 0, 1000, 0, 1000), clip(2, 500, 2000, 1000, 1500)],
            ),
            track(2, "audio", 0, vec![reversed, double]),
        ],
        2500,
    )])
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
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        self.0
    }
}
fn inspect(v: &Value, b: Budget, r: &dyn Resolver) -> Result<Inspection, Diagnostic> {
    inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        b,
        &CancellationToken::default(),
        r,
    )
}
fn open(v: &Value) -> Snapshot {
    match inspect(v, Budget::default(), &Caller(Resolution::Available)).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("known typed document became read-only"),
    }
}
fn expect(v: &Value, code: Code, suffix: &str) {
    let d = inspect(v, Budget::default(), &Caller(Resolution::Available)).unwrap_err();
    assert_eq!(d.code, code, "{v}");
    assert!(
        d.target.ends_with(suffix),
        "target {} should end with {suffix}",
        d.target
    );
}
#[test]
fn sequences_roundtrip_as_integer_ticks_and_stay_optional() {
    bounded_case(
        "sequences_roundtrip_as_integer_ticks_and_stay_optional",
        || {
            let input = serde_json::to_vec(&standard()).unwrap();
            let snapshot = open(&standard());
            assert!(snapshot.encoded_bytes() == input);
            let seq =&snapshot.document().sequences[0];
            assert_eq!(seq.duration, Ticks::new(2500));
            assert_eq!(seq.content_end(), Some(Ticks::new(2500)));
            assert_eq!(
                seq.tracks[0].clips[1].timeline_end(),
                Some(Ticks::new(2500))
            );
            assert!(seq.tracks[1].clips[0].speed.is_reversed());
            assert_eq!(seq.frame_rate.fps_fraction(), Some((24, 1)));
            assert_eq!(
                SequenceFrameRate {
                    ticks_per_frame: 10_594_584_000
                }
                .fps_fraction(),
                Some((24_000, 1001))
            );
            // Validated successor re-encodes to the same wire form, with ticks as plain integers.
            let again = validate_document(
                snapshot.document().clone(),
                Budget::default(),
                &CancellationToken::default(),
                &Caller(Resolution::Available),
            )
            .unwrap();
            let decoded: StudioDocument = serde_json::from_slice(again.encoded_bytes()).unwrap();
            assert!(&decoded == snapshot.document());
            let wire: Value = serde_json::from_slice(again.encoded_bytes()).unwrap();
            assert_eq!(wire["sequences"][0]["tracks"][0]["clips"][1]["timeline_start"], 1000);
            // Existing mutation path preserves sequences; documents without them never encode the key.
            let renamed = snapshot
                .rename(
                    &identity("SLYR", 1),
                    "renamed".into(),
                    3,
                    4,
                    Budget::default(),
                    &CancellationToken::default(),
                    &Caller(Resolution::Available),
                )
                .unwrap();
            assert_eq!(renamed.document().sequences, snapshot.document().sequences);
            let plain = open(&document(vec![]));
            assert!(plain.document().sequences.is_empty());
            let wire: Value = serde_json::from_slice(
                validate_document(
                    plain.document().clone(),
                    Budget::default(),
                    &CancellationToken::default(),
                    &Unresolved,
                )
                .unwrap()
                .encoded_bytes(),
            )
            .unwrap();
            assert!(wire.get("sequences").is_none());
        },
    );
}
#[test]
fn invalid_timelines_reject_with_stable_codes_and_targets() {
    bounded_case(
        "invalid_timelines_reject_with_stable_codes_and_targets",
        || {
            let c = |ptr: &str, v: Value| {
                let mut d = standard();
                *d["sequences"][0].pointer_mut(ptr).unwrap() = v;
                d
            };
            let p = |tail: &str| format!("/tracks/0/clips/0/{tail}");
            expect(&c(&p("source_out"), json!(0)), Code::InvalidField, "/source_window");
            expect(&c(&p("duration"), json!(999)), Code::InvalidField, "/duration");
            expect(&c(&p("speed"), json!({"numerator":3,"denominator":1})), Code::InvalidField, "/duration");
            expect(&c(&p("speed"), json!({"numerator":0,"denominator":1})), Code::InvalidField, "/speed");
            expect(&c(&p("speed"), json!({"numerator":2,"denominator":4})), Code::InvalidField, "/speed");
            expect(&c(&p("clip_id"), json!(identity("SLYR", 1))), Code::InvalidId, &identity("SLYR", 1));
            expect(&c(&p("clip_id"), json!(identity("SCLP", 2))), Code::DuplicateIdentity, &identity("SCLP", 2));
            expect(&c(&p("link_group"), json!("bad group!")), Code::InvalidField, "/link_group");
            for path in ["C:\\media\\a.mov", "/abs/a.mov", "D:/a.mov", ""] {
                let mut d = standard();
                d["sequences"][0]["tracks"][0]["clips"][0]["source_ref"]["artifact_manifest_id"] = json!(path);
                expect(&d, Code::InvalidField, "/source");
            }
            // Second video clip pulled into the first (overlap) and ordering reversed.
            expect(&c("/tracks/0/clips/1/timeline_start", json!(999)), Code::InvalidField, "/overlap");
            let mut d = standard();
            d["sequences"][0]["tracks"][0]["clips"].as_array_mut().unwrap().reverse();
            expect(&d, Code::InvalidField, "/unordered");
            // Track stack index, track id, sequence duration, frame rate.
            expect(&c("/tracks/1/index", json!(1)), Code::InvalidField, "/index");
            expect(&c("/tracks/1/track_id", json!(identity("STRK", 1))), Code::DuplicateIdentity, &identity("STRK", 1));
            expect(&c("/duration", json!(2499)), Code::InvalidField, "seq-main/duration");
            for rate in [10_594_594_594u64, 12_345, 0] {
                expect(&c("/frame_rate/ticks_per_frame", json!(rate)), Code::InvalidField, "/frame_rate");
            }
            let mut d = standard();
            d["sequences"][0]["tracks"][0]["clips"][0]["timeline_start"] = json!(u64::MAX - 10);
            d["sequences"][0]["tracks"][0]["clips"].as_array_mut().unwrap().truncate(1);
            expect(&d, Code::InvalidField, "/timeline_end");
            // Nested sequences: dangling and cyclic references.
            let nested = |from: &str, to: &str| {
                let mut c = clip(9, 0, 100, 0, 100);
                c["source_ref"] = json!({"source_kind":"sequence","sequence_id":to});
                sequence(from, TPF, vec![track(9, "video", 0, vec![c])], 100)
            };
            expect(&document(vec![nested("a", "missing")]), Code::DanglingReference, "/source");
            expect(&document(vec![nested("a", "a")]), Code::ContainmentCycle, "sequences");
            let mut b = nested("b", "a");
            b["tracks"][0]["track_id"] = json!(identity("STRK", 10));
            b["tracks"][0]["clips"][0]["clip_id"] = json!(identity("SCLP", 10));
            expect(&document(vec![nested("a", "b"), b]), Code::ContainmentCycle, "sequences");
        },
    );
}
#[test]
fn budgets_cancellation_and_hard_ceilings() {
    bounded_case("budgets_cancellation_and_hard_ceilings", || {
        let r = Caller(Resolution::Available);
        // 1 layer + 1 sequence + 2 tracks + 4 clips = 8 nodes.
        let tight = Budget {
            nodes: 8,
            ..Budget::default()
        };
        assert!(matches!(
            inspect(&standard(), tight, &r).unwrap(),
            Inspection::Editable(_)
        ));
        let d = inspect(
            &standard(),
            Budget {
                nodes: 7,
                ..Budget::default()
            },
            &r,
        )
        .unwrap_err();
        assert_eq!((d.code, d.target.as_str()), (Code::BudgetExceeded, "document"));
        let d = inspect(
            &standard(),
            Budget {
                payload_bytes: 4,
                ..Budget::default()
            },
            &r,
        )
        .unwrap_err();
        assert_eq!(d.code, Code::BudgetExceeded);
        let token = CancellationToken::default();
        token.cancel();
        let typed: StudioDocument = serde_json::from_value(standard()).unwrap();
        assert_eq!(
            validate_document(typed.clone(), Budget::default(), &token, &r)
                .unwrap_err()
                .code,
            Code::Canceled
        );
        // Hard ceilings hold even with an enlarged caller budget.
        let huge = Budget {
            nodes: 100_000,
            ..Budget::default()
        };
        let tracks: Vec<Value> = (0..=MAX_TRACKS_PER_SEQUENCE as u32)
            .map(|n| track(n + 1, "video", n, vec![]))
            .collect();
        let d = inspect(&document(vec![sequence("big", TPF, tracks, 0)]), huge, &r).unwrap_err();
        assert_eq!(d.code, Code::BudgetExceeded);
        assert!(d.target.ends_with("/tracks"));
        let many: Vec<Value> = (0..=MAX_SEQUENCES)
            .map(|n| sequence(&format!("s{n}"), TPF, vec![], 0))
            .collect();
        let d = inspect(&document(many), huge, &r).unwrap_err();
        assert_eq!((d.code, d.target.as_str()), (Code::BudgetExceeded, "sequences"));
    });
}
#[test]
fn resolution_schema_and_forward_compatibility() {
    bounded_case("resolution_schema_and_forward_compatibility", || {
        for (resolver, expected) in [
            (&Unresolved as &dyn Resolver, Resolution::Absent),
            (&Caller(Resolution::HashMismatch), Resolution::HashMismatch),
        ] {
            let s = match inspect(&standard(), Budget::default(), resolver).unwrap() {
                Inspection::Editable(s) => s,
                _ => panic!("structurally valid timeline was lost"),
            };
            let media: Vec<_> = s
                .resolution_issues()
                .iter()
                .filter(|i| i.target.ends_with("/source"))
                .collect();
            assert_eq!(media.len(), 4);
            assert!(media.iter().all(|i| i.disposition == expected));
            assert_eq!(s.require_available().unwrap_err().code, Code::Unavailable);
        }
        assert!(
            open(&standard())
                .resolution_issues()
                .iter()
                .all(|i| !i.target.ends_with("/source"))
        );
        // Typed primitive sources resolve through the existing primitive resolver; unknown schemas reject.
        let mut d = standard();
        d["sequences"][0]["tracks"][0]["clips"][0]["source_ref"] = json!({"source_kind":"primitive","schema_id":"hsk.studio.motion_timeline@1","object_id":"comp-1"});
        assert!(matches!(
            inspect(&d, Budget::default(), &Caller(Resolution::Available)).unwrap(),
            Inspection::Editable(_)
        ));
        d["sequences"][0]["tracks"][0]["clips"][0]["source_ref"]["schema_id"] = json!("hsk.studio.nope@1");
        expect(&d, Code::UnsupportedSchema, "/source");
        // Generated schema carries the optional timeline member; link_group is explicit-null required.
        let schema = serde_json::to_value(document_schema()).unwrap();
        assert!(schema["properties"]["sequences"].is_object());
        assert!(!schema["required"].as_array().unwrap().contains(&json!("sequences")));
        let text = serde_json::to_string(&schema).unwrap();
        for token in ["hsk.studio.sequence@1", "hsk.studio.track@1", "hsk.studio.clip@1", "link_group"] {
            assert!(text.contains(token), "schema lacks {token}");
        }
        assert!(schema["$defs"]["StudioClip"]["required"].as_array().unwrap().contains(&json!("link_group")));
        // Future schema versions and unknown fields retain the complete input read-only.
        let mut cases = Vec::new();
        for (path, field, value) in [
            ("sequences/0", "schema_id", json!("hsk.studio.sequence@2")),
            ("sequences/0/tracks/0", "schema_id", json!("hsk.studio.track@2")),
            ("sequences/0/tracks/0/clips/0", "schema_id", json!("hsk.studio.clip@2")),
            ("sequences/0/tracks/0/clips/0", "future_field", json!(1)),
        ] {
            let mut d = standard();
            d.pointer_mut(&format!("/{path}")).unwrap()[field] = value;
            cases.push(d);
        }
        for v in cases {
            let bytes = serde_json::to_vec_pretty(&v).unwrap();
            match inspect_bytes(&bytes, Budget::default(), &CancellationToken::default(), &Unresolved).unwrap() {
                Inspection::ReadOnly(r) => assert_eq!(r.encoded_bytes(), bytes),
                _ => panic!("unsupported timeline input became editable"),
            }
        }
    });
}
