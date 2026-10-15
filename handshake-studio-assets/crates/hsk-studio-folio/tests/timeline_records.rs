//! Lane SMB-FOLIO-TIMELINE-RECORDS: sequence and clip markers and clip transitions stored in the
//! Folio sequence (STU-VID-010/020/032/034): round trip, unchanged wire for documents without
//! them, and hostile decoder input.
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

struct Available;
impl Resolver for Available {
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
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

fn clip(n: u32, source_in: u64, frames: u64, start: u64) -> Value {
    json!({"schema_id":"hsk.studio.clip@1","clip_id":identity("SCLP",n),
        "source_ref":{"source_kind":"media","artifact_manifest_id":"manifest-a","content_digest":{"algorithm":"sha256","digest":"abc123"}},
        "source_in":source_in*TPF,"source_out":(source_in+frames)*TPF,"timeline_start":start*TPF,"duration":frames*TPF,
        "speed":{"numerator":1,"denominator":1},"enabled":true,"link_group":null})
}
/// V1: A src 0..48 @0, B src 100..148 @48 (cut at frame 48); duration 96 frames.
fn document(mutate: impl FnOnce(&mut Value)) -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[{"schema_id":"hsk.studio.sequence@1","sequence_id":"seq-main","name":"main",
            "frame_rate":{"ticks_per_frame":TPF},"duration":96*TPF,
            "tracks":[{"schema_id":"hsk.studio.track@1","track_id":identity("STRK",1),"kind":"video","index":0,
                "name":"V1","enabled":true,"locked":false,"sync_locked":true,"targeted":true,"muted":false,"soloed":false,
                "clips":[clip(1,0,48,0),clip(2,100,48,48)]}]}]
    });
    mutate(&mut v);
    v
}
fn marker(id: &str, start: u64, frames: u64, role: Value) -> Value {
    json!({"marker_id":id,"name":"Chapter one","comment":"first line\nsecond line","color":"rose",
        "start":start*TPF,"duration":frames*TPF,"role":role})
}
fn dissolve(id: &str, frames: u64) -> Value {
    json!({"schema_id":"hsk.studio.transition@1","transition_id":id,"transition_kind":"cross_dissolve",
        "duration":frames*TPF,"alignment":"centered_on_cut","center_offset":0})
}
/// The records: a chapter span on the sequence, a web-link point marker on clip A (source ticks)
/// and a 24-frame dissolve at the A|B cut stored as A's tail.
fn annotate(v: &mut Value) {
    let seq = &mut v["sequences"][0];
    seq["markers"] = json!([marker("m-seq", 12, 6, json!({"role":"chapter"}))]);
    let a = &mut seq["tracks"][0]["clips"][0];
    a["markers"] = json!([marker(
        "m-a",
        30,
        0,
        json!({"role":"web_link","url":"https://example.invalid/a"})
    )]);
    a["transitions"] = json!({"head":null,"tail":dissolve("t-ab",24)});
}

fn inspect_with(v: &Value, budget: Budget) -> Result<Snapshot, Diagnostic> {
    match inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        budget,
        &CancellationToken::default(),
        &Available,
    )? {
        Inspection::Editable(s) => Ok(s),
        // Folio keeps an undecodable document read-only; for these records that is a refusal.
        Inspection::ReadOnly(_) => Err(Diagnostic {
            code: Code::UnknownField,
            target: "read_only".into(),
        }),
    }
}
fn inspect(v: &Value) -> Result<Snapshot, Diagnostic> {
    inspect_with(v, Budget::default())
}
fn code(v: &Value) -> (Code, String) {
    let d = inspect(v).expect_err("document must be refused");
    (d.code, d.target)
}

#[test]
fn markers_and_transitions_round_trip_and_old_wire_is_unchanged() {
    bounded_case("round_trip", || {
        // A document without records keeps its wire: no new keys appear and it re-decodes equal.
        let old = inspect(&document(|_| {})).unwrap();
        // Folio's own encoding of the typed document (the bytes a save writes).
        let canonical = validate_document(
            old.document().clone(),
            Budget::default(),
            &CancellationToken::default(),
            &Available,
        )
        .unwrap();
        let old_bytes = String::from_utf8(canonical.encoded_bytes().to_vec()).unwrap();
        assert!(!old_bytes.contains("\"markers\"") && !old_bytes.contains("\"transitions\""));
        let seq = &old.document().sequences[0];
        assert!(seq.markers.is_empty() && seq.tracks[0].clips[0].transitions.is_empty());
        let again = inspect(&serde_json::from_slice(old.encoded_bytes()).unwrap()).unwrap();
        assert_eq!(again.encoded_bytes(), old.encoded_bytes());

        // Records decode typed, survive encode -> decode exactly, and only they add bytes.
        let snap = inspect(&document(annotate)).unwrap();
        let seq = &snap.document().sequences[0];
        assert_eq!(
            seq.markers,
            vec![StudioMarker {
                marker_id: "m-seq".into(),
                name: "Chapter one".into(),
                comment: "first line\nsecond line".into(),
                color: LabelColor::Rose,
                start: Ticks::new(12 * TPF),
                duration: Ticks::new(6 * TPF),
                role: Some(MarkerRole::Chapter),
            }]
        );
        let a = &seq.tracks[0].clips[0];
        assert_eq!(
            a.markers[0].role,
            Some(MarkerRole::WebLink {
                url: "https://example.invalid/a".into()
            })
        );
        assert_eq!(
            a.transitions.tail,
            Some(TimelineTransition {
                schema_id: TransitionSchema::V1,
                transition_id: "t-ab".into(),
                transition_kind: "cross_dissolve".into(),
                duration: Ticks::new(24 * TPF),
                alignment: TransitionAlignment::CenteredOnCut,
                center_offset: 0,
            })
        );
        assert!(a.transitions.head.is_none() && seq.tracks[0].clips[1].transitions.is_empty());
        let round = inspect(&serde_json::from_slice(snap.encoded_bytes()).unwrap()).unwrap();
        assert_eq!(round.document(), snap.document());
        assert_eq!(round.encoded_bytes(), snap.encoded_bytes());
        let text = String::from_utf8(snap.encoded_bytes().to_vec()).unwrap();
        assert!(text.contains("hsk.studio.transition@1"));
        // Typed values serialize back to the same records (Rust-built documents too).
        let mut built = old.document().clone();
        built.revision = 3;
        built.sequences[0].markers = seq.markers.clone();
        built.sequences[0].tracks[0].clips[0].markers = a.markers.clone();
        built.sequences[0].tracks[0].clips[0].transitions = a.transitions.clone();
        let rebuilt = validate_document(
            built,
            Budget::default(),
            &CancellationToken::default(),
            &Available,
        )
        .unwrap();
        assert_eq!(rebuilt.document(), snap.document());
        let again = inspect(&serde_json::from_slice(rebuilt.encoded_bytes()).unwrap()).unwrap();
        assert_eq!(again.document(), snap.document());
    });
}

#[test]
fn hostile_timeline_records_are_refused_typed() {
    bounded_case("hostile", || {
        let with = |f: &dyn Fn(&mut Value)| {
            document(|v| {
                annotate(v);
                f(v);
            })
        };
        let a = |v: &mut Value, key: &str, value: Value| {
            v["sequences"][0]["tracks"][0]["clips"][0][key] = value
        };
        let seq_marker =
            |v: &mut Value, key: &str, value: Value| v["sequences"][0]["markers"][0][key] = value;

        // Identity: duplicate ids across scopes, non-key ids.
        assert_eq!(
            code(&with(&|v| seq_marker(v, "marker_id", json!("m-a")))).0,
            Code::DuplicateIdentity
        );
        assert_eq!(
            code(&with(&|v| seq_marker(v, "marker_id", json!("bad id!")))).0,
            Code::InvalidId
        );
        assert_eq!(
            code(&with(
                &|v| v["sequences"][0]["tracks"][0]["clips"][1]["transitions"] =
                    json!({"head":null,"tail":dissolve("t-ab",4)})
            ))
            .0,
            Code::DuplicateIdentity
        );
        // Text: control characters in a name, oversize comment, empty web link.
        assert_eq!(
            code(&with(&|v| seq_marker(v, "name", json!("bell\u{7}")))).0,
            Code::InvalidField
        );
        assert_eq!(
            code(&with(&|v| seq_marker(
                v,
                "comment",
                json!("x".repeat(MAX_MARKER_TEXT_BYTES + 1))
            )))
            .0,
            Code::InvalidField
        );
        assert_eq!(
            code(&with(&|v| seq_marker(
                v,
                "role",
                json!({"role":"web_link","url":""})
            )))
            .0,
            Code::InvalidField
        );
        // Ticks: start + duration past u64::MAX.
        let (c, target) = code(&with(&|v| {
            seq_marker(v, "start", json!(u64::MAX));
            seq_marker(v, "duration", json!(1));
        }));
        assert_eq!((c, target.as_str()), (Code::InvalidField, "m-seq/span"));
        // Transitions: zero duration, stray or oversized centre offset, bad kind key, two at one cut.
        let tail = |v: &mut Value, key: &str, value: Value| {
            v["sequences"][0]["tracks"][0]["clips"][0]["transitions"]["tail"][key] = value
        };
        assert_eq!(
            code(&with(&|v| tail(v, "duration", json!(0)))).1,
            "t-ab/timing"
        );
        assert_eq!(
            code(&with(&|v| tail(v, "center_offset", json!(5)))).1,
            "t-ab/timing"
        );
        assert_eq!(
            code(&with(&|v| {
                tail(v, "alignment", json!("custom"));
                tail(v, "center_offset", json!(i64::MIN));
            }))
            .1,
            "t-ab/timing"
        );
        assert_eq!(
            code(&with(&|v| tail(v, "transition_kind", json!("")))).1,
            "t-ab/transition_kind"
        );
        let (c, target) = code(&with(&|v| {
            v["sequences"][0]["tracks"][0]["clips"][1]["transitions"] =
                json!({"head":dissolve("t-b",4),"tail":null})
        }));
        assert_eq!(
            (c, target),
            (
                Code::InvalidField,
                format!("{}/transitions", identity("SCLP", 2))
            )
        );
        // Wire shape: unknown keys and a missing explicit-null role are decoder errors.
        assert!(inspect(&with(&|v| seq_marker(v, "extra", json!(1)))).is_err());
        assert!(inspect(&with(&|v| tail(v, "parameters", json!({})))).is_err());
        assert!(
            inspect(&with(&|v| {
                v["sequences"][0]["markers"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("role");
            }))
            .is_err()
        );
        assert!(inspect(&with(&|v| a(v, "transitions", json!({"tail":null})))).is_err());
        // Count: one marker over the per-sequence bound is refused on the count alone (all share
        // one id, so examining any pair would have answered duplicate_identity instead).
        let big = Budget {
            input_bytes: 64 << 20,
            nodes: 1 << 20,
            payload_bytes: 64 << 20,
            ..Budget::default()
        };
        let many = document(|v| {
            let m = marker("same", 0, 0, Value::Null);
            v["sequences"][0]["markers"] = Value::Array(vec![m; MAX_MARKERS_PER_SEQUENCE + 1]);
        });
        let d = inspect_with(&many, big).expect_err("over the marker bound");
        assert_eq!(
            (d.code, d.target.as_str()),
            (Code::BudgetExceeded, "seq-main/markers")
        );
        // The default node budget counts markers and transitions too.
        let crowd = document(|v| {
            let list: Vec<Value> = (0..4100)
                .map(|i| marker(&format!("m{i}"), 0, 0, Value::Null))
                .collect();
            v["sequences"][0]["markers"] = Value::Array(list);
        });
        assert_eq!(
            inspect_with(
                &crowd,
                Budget {
                    input_bytes: 64 << 20,
                    ..Budget::default()
                }
            )
            .unwrap_err()
            .code,
            Code::BudgetExceeded
        );
    });
}
