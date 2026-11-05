//! Lane SMB-PR-RETIME-HOLD-1: the clip `retime` record ([STU-VID-031], [STU-VID-031a]): one field on
//! `StudioClip`, omitted at its default so old-shape clips load unchanged and write byte-identically,
//! validated on the Folio path against hostile wire.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use serde_json::{Value, json};

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

fn clip(n: u32) -> Value {
    json!({"schema_id":"hsk.studio.clip@1","clip_id":identity("SCLP",n),
        "source_ref":{"source_kind":"media","artifact_manifest_id":"manifest-a","content_digest":{"algorithm":"sha256","digest":"abc123"}},
        "source_in":0,"source_out":48*TPF,"timeline_start":0,"duration":48*TPF,
        "speed":{"numerator":1,"denominator":1},"enabled":true,"link_group":null})
}
/// One video track (clip 1) and one audio track (clip 2), both source 0..48 at 0.
fn document(mutate: impl FnOnce(&mut Value)) -> Value {
    let track = |n: u32, kind: &str, c: Value| {
        json!({"schema_id":"hsk.studio.track@1","track_id":identity("STRK",n),"kind":kind,"index":0,
            "name":kind,"enabled":true,"locked":false,"sync_locked":true,"targeted":true,"muted":false,"soloed":false,"clips":[c]})
    };
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[{"schema_id":"hsk.studio.sequence@1","sequence_id":"seq-main","name":"main",
            "frame_rate":{"ticks_per_frame":TPF},"duration":48*TPF,
            "tracks":[track(1,"video",clip(1)),track(2,"audio",clip(2))]}]
    });
    mutate(&mut v);
    v
}

fn inspect(v: &Value) -> Result<Snapshot, Diagnostic> {
    match inspect_bytes(&serde_json::to_vec(v).unwrap(), Budget::default(), &CancellationToken::default(), &Available)? {
        Inspection::Editable(s) => Ok(s),
        // Folio keeps an undecodable document read-only; for this record that is a refusal.
        Inspection::ReadOnly(_) => Err(Diagnostic { code: Code::UnknownField, target: "read_only".into() }),
    }
}

fn with_retime(retime: Value) -> Value {
    document(|v| v["sequences"][0]["tracks"][0]["clips"][0]["retime"] = retime)
}

fn first(snapshot: &Snapshot) -> &StudioClip {
    &snapshot.document().sequences[0].tracks[0].clips[0]
}

/// The bytes a save writes for this document: Folio's encoding of the typed value.
fn saved(snapshot: &Snapshot) -> Vec<u8> {
    validate_document(snapshot.document().clone(), Budget::default(), &CancellationToken::default(), &Available).unwrap().encoded_bytes().to_vec()
}

#[test]
fn old_shape_clips_load_unchanged_and_write_byte_identically() {
    let old = inspect(&document(|_| {})).unwrap();
    assert_eq!(first(&old).retime, ClipRetime::default());
    assert!(first(&old).retime.is_default());
    let bytes = saved(&old);
    assert!(!String::from_utf8_lossy(&bytes).contains("retime"), "no retime key on the wire at the default");
    // The same clip spelled with an explicit empty record is the same clip and writes the same bytes.
    let spelled = inspect(&with_retime(json!({}))).unwrap();
    assert_eq!(saved(&spelled), bytes);
    let reread = inspect(&serde_json::from_slice(&bytes).unwrap()).unwrap();
    assert_eq!(saved(&reread), bytes);
}

#[test]
fn retime_record_round_trips_and_omits_each_member_at_its_default() {
    let full = inspect(&with_retime(json!({"frame_interpolation":"optical_flow","maintain_audio_pitch":true,"frame_hold":{"source_tick":3*TPF,"anchor":"clip_marker"}}))).unwrap();
    assert_eq!(
        first(&full).retime,
        ClipRetime {
            frame_interpolation: FrameInterpolation::OpticalFlow,
            maintain_audio_pitch: true,
            frame_hold: Some(ClipFrameHold { source_tick: Ticks::new(3 * TPF), anchor: FrameHoldAnchor::ClipMarker }),
        }
    );
    let bytes = saved(&full);
    let reread = inspect(&serde_json::from_slice(&bytes).unwrap()).unwrap();
    assert_eq!(first(&reread).retime, first(&full).retime);
    assert_eq!(saved(&reread), bytes);

    // A member at its default stays off the wire; only the stated one appears.
    let cases = [
        (json!({"maintain_audio_pitch":true}), json!({"maintain_audio_pitch":true})),
        (json!({"frame_interpolation":"frame_blending"}), json!({"frame_interpolation":"frame_blending"})),
        (json!({"frame_interpolation":"frame_sampling","maintain_audio_pitch":false,"frame_hold":{"source_tick":0,"anchor":"in_point"}}), json!({"frame_hold":{"source_tick":0,"anchor":"in_point"}})),
    ];
    for (input, wire) in cases {
        let s = inspect(&with_retime(input)).unwrap();
        let written: Value = serde_json::from_slice(&saved(&s)).unwrap();
        assert_eq!(written["sequences"][0]["tracks"][0]["clips"][0]["retime"], wire);
    }
    // The other clip stays old-shape next to a clip with the record.
    let written: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(written["sequences"][0]["tracks"][1]["clips"][0].get("retime").is_none());
}

/// The enumeration is exactly the spec's three members, with no member added, renamed or mapped
/// onto another (MT-33731 AC-001..003), and the four hold anchors of [STU-VID-031a].
#[test]
fn enumerations_are_the_declared_members() {
    for (token, member) in [("frame_sampling", FrameInterpolation::FrameSampling), ("frame_blending", FrameInterpolation::FrameBlending), ("optical_flow", FrameInterpolation::OpticalFlow)] {
        let s = inspect(&with_retime(json!({"frame_interpolation":token}))).unwrap();
        assert_eq!(first(&s).retime.frame_interpolation, member);
    }
    assert_eq!(FrameInterpolation::default(), FrameInterpolation::FrameSampling);
    for (token, member) in [("playhead", FrameHoldAnchor::Playhead), ("clip_marker", FrameHoldAnchor::ClipMarker), ("in_point", FrameHoldAnchor::InPoint), ("out_point", FrameHoldAnchor::OutPoint)] {
        let s = inspect(&with_retime(json!({"frame_hold":{"source_tick":0,"anchor":token}}))).unwrap();
        assert_eq!(first(&s).retime.frame_hold.unwrap().anchor, member);
    }
    let schema = serde_json::to_value(document_schema()).unwrap();
    let defs = &schema["$defs"];
    let members = |def: &str| -> Vec<String> {
        let mut list: Vec<String> = defs[def]["enum"].as_array().unwrap_or_else(|| panic!("{def} is a string enum")).iter().map(|v| v.as_str().unwrap().to_string()).collect();
        list.sort();
        list
    };
    assert_eq!(members("FrameInterpolation"), ["frame_blending", "frame_sampling", "optical_flow"]);
    assert_eq!(members("FrameHoldAnchor"), ["clip_marker", "in_point", "out_point", "playhead"]);
    assert!(defs["StudioClip"]["properties"].get("retime").is_some());
    assert!(!defs["StudioClip"]["required"].as_array().unwrap().iter().any(|r| r == "retime"), "an old-shape clip validates against the schema");
}

/// Hostile wire is refused typed, never defaulted (V4-01/02).
#[test]
fn hostile_retime_wire_is_refused() {
    let refused = |retime: Value| inspect(&with_retime(retime)).expect_err("the document must be refused").code;
    let unknown = [
        json!({"frame_interpolation":"nearest"}),
        json!({"frame_interpolation":"Frame_Sampling"}),
        json!({"frame_interpolation":1}),
        json!({"maintain_audio_pitch":"yes"}),
        json!({"reverse":true}),
        json!({"frame_hold":{"source_tick":0}}),
        json!({"frame_hold":{"anchor":"in_point"}}),
        json!({"frame_hold":{"source_tick":0,"anchor":"source_timecode"}}),
        json!({"frame_hold":{"source_tick":0,"anchor":"in_point","extra":1}}),
        json!({"frame_hold":{"source_tick":-1,"anchor":"in_point"}}),
        json!({"frame_hold":{"source_tick":0.5,"anchor":"in_point"}}),
        json!({"frame_hold":{"source_tick":"3","anchor":"in_point"}}),
        json!("frame_sampling"),
        json!(null),
    ];
    for retime in unknown {
        assert_eq!(refused(retime.clone()), Code::UnknownField, "{retime}");
    }
    // No frame starts at the last representable tick: it has no end.
    let at_max = inspect(&with_retime(json!({"frame_hold":{"source_tick":u64::MAX,"anchor":"in_point"}}))).expect_err("refused");
    assert_eq!(at_max.code, Code::InvalidField);
    assert_eq!(at_max.target, format!("{}/retime/frame_hold", identity("SCLP", 1)));
    assert!(inspect(&with_retime(json!({"frame_hold":{"source_tick":u64::MAX - 1,"anchor":"in_point"}}))).is_ok());
    // A frame hold needs a picture: an audio clip cannot carry one; its other choices are its own.
    let audio = |retime: Value| inspect(&document(|v| v["sequences"][0]["tracks"][1]["clips"][0]["retime"] = retime));
    let on_audio = audio(json!({"frame_hold":{"source_tick":0,"anchor":"in_point"}})).expect_err("refused");
    assert_eq!(on_audio.code, Code::InvalidField);
    assert_eq!(on_audio.target, format!("{}/retime/frame_hold", identity("SCLP", 2)));
    assert!(audio(json!({"maintain_audio_pitch":true})).is_ok());
}

/// The hold is not bound to this clip's window on the Folio path: a razor piece legitimately holds
/// a frame of the other piece (the planners refuse a hold outside the window when it is set; see
/// pulse `tests/retime_hold.rs`).
#[test]
fn a_hold_outside_the_window_of_a_cut_piece_loads() {
    let s = inspect(&with_retime(json!({"frame_hold":{"source_tick":100*TPF,"anchor":"in_point"}}))).unwrap();
    assert_eq!(first(&s).retime.frame_hold.unwrap().source_tick, Ticks::new(100 * TPF));
}
