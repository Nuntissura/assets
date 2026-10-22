//! Lane SMB-AU-WAVEFORM-EDITOR (feature `audio-batch`): the waveform editor in the viewer, headless.
//! Every audio-changing edit is ONE undo step of the run (the edit list lives in the session entry
//! `waveform_edit`); the edited audio equals score's WaveBuffer driven with the same operations (the
//! oracle), undo / redo restore earlier states bit for bit, a stale clipboard and bad arguments are
//! refused without a step, the list survives save and reopen, hostile entry bytes are refused, and
//! the panel draws its widgets in one egui frame.
#![cfg(feature = "audio-batch")]

mod support_audio;

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::waveform::{WaveformDocument, WaveformEdit, widget};
use hsk_studio_score::{ALL_CHANNELS, FadeCurve, FadeDirection, PasteMode, SnapDirection, WaveBuffer, WaveSelection};

const RATE: u32 = 48_000;

pub fn package(dir: &std::path::Path) -> std::path::PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000002","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[]
    });
    struct Any;
    impl folio::Resolver for Any {
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
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("waveform.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Stereo source: a 440 Hz sine at 0.5 and a slow ramp, 4800 frames.
pub fn source() -> Vec<Vec<f32>> {
    let sine: Vec<f32> = (0..4800).map(|n| (0.5 * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(RATE)).sin()) as f32).collect();
    let ramp: Vec<f32> = (0..4800).map(|n| (n as f32 / 4800.0) - 0.5).collect();
    vec![sine, ramp]
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

fn bits(b: &WaveBuffer) -> Vec<Vec<u32>> {
    b.channels().iter().map(|c| c.iter().map(|v| v.to_bits()).collect()).collect()
}

#[test]
fn waveform_edits_are_single_undo_steps_replayed_after_reopen() {
    let dir = support_audio::scratch("waveform");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.open_waveform(RATE, source()).unwrap();
    let t = CancellationToken::default();
    let mut oracle = WaveBuffer::new(RATE, source()).unwrap();
    let base = undo_labels(&v).len();

    // Snap: both selection ends move to zero crossings of channel 0 within 10 ms.
    v.set_waveform_snap(true).unwrap();
    let asked = WaveSelection { start: 1000, end: 2000, channels: 0b01 };
    let sel = v.waveform_select(Some(asked)).unwrap().unwrap();
    let near = |p| oracle.snap_to_zero_crossing(p, 0b01, SnapDirection::Nearest, 480, &t).unwrap().unwrap();
    assert_eq!((sel.start, sel.end), (near(1000), near(2000)));
    assert!(sel.start != 1000 && oracle.is_zero_crossing(sel.start, 0b01).unwrap(), "{sel:?}");

    // Copy is not a step; every other edit is exactly one.
    v.waveform_edit(WaveformEdit::Copy(WaveSelection::all(100, 300))).unwrap();
    let clip = oracle.copy(WaveSelection::all(100, 300)).unwrap();
    assert_eq!(undo_labels(&v).len(), base);
    let mut states = vec![oracle.clone()];
    let edits: Vec<WaveformEdit> = vec![
        WaveformEdit::Paste { at: 50, mode: PasteMode::Insert, channels: ALL_CHANNELS },
        WaveformEdit::Cut { start: 400, end: 700 },
        WaveformEdit::Fade { selection: sel, direction: FadeDirection::In, curve: FadeCurve::SCurve },
        WaveformEdit::Amplify { selection: WaveSelection { start: 0, end: 1000, channels: 0b10 }, db: -6.0 },
        WaveformEdit::GainEnvelope { selection: WaveSelection::all(2000, 3000), points: vec![(0, 0.0), (1000, -12.0)] },
        WaveformEdit::InsertSilence { at: 10, frames: 25 },
        WaveformEdit::Normalize { selection: WaveSelection::all(0, 4000), target_dbfs: -1.0, linked: true },
        WaveformEdit::Reverse(WaveSelection { start: 3000, end: 3500, channels: 0b01 }),
        WaveformEdit::Invert(WaveSelection::all(0, 10)),
        WaveformEdit::Silence(WaveSelection { start: 4000, end: 4100, channels: ALL_CHANNELS }),
    ];
    for (i, e) in edits.iter().enumerate() {
        v.waveform_edit(e.clone()).unwrap_or_else(|err| panic!("edit {i}: {err}"));
        match e {
            WaveformEdit::Paste { at, mode, channels } => oracle.paste(*at, &clip, *mode, *channels, &t).map(|_| ()),
            WaveformEdit::Cut { start, end } => oracle.cut(*start, *end, &t).map(|_| ()),
            WaveformEdit::Fade { selection, direction, curve } => oracle.fade(*selection, *direction, *curve, &t).map(|_| ()),
            WaveformEdit::Amplify { selection, db } => oracle.amplify(*selection, *db, &t).map(|_| ()),
            WaveformEdit::GainEnvelope { selection, points } => oracle.gain_envelope(*selection, points, &t).map(|_| ()),
            WaveformEdit::InsertSilence { at, frames } => oracle.insert_silence(*at, *frames, &t).map(|_| ()),
            WaveformEdit::Normalize { selection, target_dbfs, linked } => oracle.normalize(*selection, *target_dbfs, *linked, &t).map(|_| ()),
            WaveformEdit::Reverse(s) => oracle.reverse(*s, &t).map(|_| ()),
            WaveformEdit::Invert(s) => oracle.invert(*s, &t).map(|_| ()),
            WaveformEdit::Silence(s) => oracle.silence(*s, &t).map(|_| ()),
            _ => unreachable!(),
        }
        .unwrap();
        states.push(oracle.clone());
        assert_eq!(undo_labels(&v).len(), base + i + 1, "edit {i} is one step: {:?}", undo_labels(&v));
        assert_eq!(bits(&v.waveform_buffer().unwrap()), bits(&oracle), "edit {i}");
    }
    assert_eq!(undo_labels(&v).last().map(String::as_str), Some("waveform_silence waveform_edit"));
    // Refusals: no step, nothing changes.
    assert!(v.waveform_edit(WaveformEdit::Amplify { selection: WaveSelection::all(0, 10), db: f64::NAN }).is_err());
    assert!(v.waveform_edit(WaveformEdit::Delete { start: 10, end: 1_000_000 }).is_err());
    assert_eq!(undo_labels(&v).len(), base + edits.len());

    // Undo three steps, redo one: bit-equal to the oracle states.
    for _ in 0..3 {
        v.waveform_undo(true).unwrap();
    }
    assert_eq!(bits(&v.waveform_buffer().unwrap()), bits(&states[edits.len() - 3]));
    v.waveform_undo(false).unwrap();
    assert_eq!(bits(&v.waveform_buffer().unwrap()), bits(&states[edits.len() - 2]));
    // A clip copied after edits that are then undone is stale.
    v.waveform_edit(WaveformEdit::Copy(WaveSelection::all(0, 100))).unwrap();
    v.waveform_undo(true).unwrap();
    let stale = v.waveform_edit(WaveformEdit::Paste { at: 0, mode: PasteMode::Insert, channels: ALL_CHANNELS }).unwrap_err();
    assert!(stale.starts_with("waveform_clipboard_stale"), "{stale}");
    let kept = bits(&v.waveform_buffer().unwrap());
    assert_eq!(kept, bits(&states[edits.len() - 3]));

    // One egui frame draws the panel.
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for name in [widget::PANEL, widget::CANVAS, widget::CUT, widget::PASTE, widget::FADE_IN, widget::NORMALIZE, widget::SNAP, widget::UNDO] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn: {:?}", v.drawn());
    }
    assert_eq!(v.state_json()["waveform"]["ops"].as_array().unwrap().len(), edits.len() - 3);

    // Save, reopen: the list replays to the same audio; another source is refused.
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    let mut other = source();
    other[1][7] = 0.125;
    let refused = w.open_waveform(RATE, other).unwrap_err();
    assert!(refused.starts_with("waveform_source_mismatch"), "{refused}");
    w.open_waveform(RATE, source()).unwrap();
    assert_eq!(bits(&w.waveform_buffer().unwrap()), kept);

    // Hostile entry bytes.
    let doc = WaveformDocument::parse(w.run().document_entry("waveform_edit").unwrap()).unwrap();
    let good = String::from_utf8(doc.to_bytes()).unwrap();
    let bad = [
        good.replace("hsk.studio.waveform_edit@1", "hsk.studio.waveform_edit@9"),
        good.replacen("\"op\":\"paste\"", "\"op\":\"explode\"", 1),
        good.replacen("\"after_ops\":0", "\"after_ops\":5", 1),
        good.replacen("\"target_dbfs\":-1.0", "\"target_dbfs\":\"loud\"", 1),
        good.replacen("\"sha256\":\"", "\"sha256\":\"zz", 1),
        "{".to_owned(),
        " ".repeat(4 << 20 | 1),
    ];
    for (i, b) in bad.iter().enumerate() {
        assert_ne!(b, &good, "case {i} must change the bytes");
        assert!(WaveformDocument::parse(b.as_bytes()).is_err(), "case {i} accepted");
    }
    let many = format!("{{\"schema\":\"hsk.studio.waveform_edit@1\",\"source\":{},\"ops\":[{}]}}",
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&good).unwrap()["source"]).unwrap(),
        vec!["{\"op\":\"invert\",\"selection\":{\"start\":0,\"end\":1,\"channels\":1}}"; 4097].join(","));
    assert_eq!(WaveformDocument::parse(many.as_bytes()).unwrap_err(), "waveform_entry_too_many_ops");
}
