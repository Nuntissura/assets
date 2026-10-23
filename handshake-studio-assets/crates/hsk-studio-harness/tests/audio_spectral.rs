//! Lane SMB-AU-WAVEFORM-EDITOR (feature `audio-batch`): the spectral frequency display of the
//! waveform editor (slice 2) and spectral selection edits as undo steps (slice 3), headless.
//! Oracles: the calibrated dB of a known tone on its display row, the log-axis row of a frequency,
//! score's cache equal to a fresh computation, and score's spectral_mask_edit driven directly.
#![cfg(feature = "audio-batch")]

mod support_audio;

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::spectral_display::widget;
use hsk_studio_score::{FrequencyScale, SpectralViewParams, SpectralWindow, row_at_frequency};

const RATE: u32 = 48_000;

fn package(dir: &std::path::Path) -> std::path::PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000003","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
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
    let path = dir.join("spectral.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn sine(hz: f64, amp: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|n| (amp * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(RATE)).sin()) as f32).collect()
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

#[test]
fn spectral_display_reads_calibrated_rows_and_follows_edits() {
    let dir = support_audio::scratch("spectral-display");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap();
    // Bin 100 of 2048 (2343.75 Hz) at amplitude 0.5: -6.0206 dB on the linear row of that bin.
    let f = 100.0 * f64::from(RATE) / 2048.0;
    v.open_waveform(RATE, vec![sine(f, 0.5, 48_000)]).unwrap();
    let linear = SpectralViewParams { fft_size: 2048, hop: 512, window: SpectralWindow::Hann, scale: FrequencyScale::Linear, rows: 1024, floor_db: -120.0 };
    v.open_spectral_display(linear).unwrap();
    let tile = v.spectral_tile(0, 0).unwrap();
    assert!((f64::from(tile[10 * 1024 + 100]) + 6.0206).abs() < 1e-3, "{}", tile[10 * 1024 + 100]);
    let (w, h, db) = v.spectral_columns(0, 50).unwrap();
    assert_eq!((w, h), (50, 1024));
    let row = 1023 - 100; // row 0 is the top (highest frequency)
    assert!((f64::from(db[row * 50 + 25]) + 6.0206).abs() < 1e-3);
    // Log axis: the brightest row of a column is the row holding 2343.75 Hz.
    let log = SpectralViewParams { scale: FrequencyScale::Log { min_hz: 20.0 }, rows: 256, ..linear };
    v.set_spectral_params(log).unwrap();
    let (_, h, db) = v.spectral_columns(0, 50).unwrap();
    let col: Vec<f32> = (0..h).map(|r| db[r * 50 + 25]).collect();
    let top = (0..h).max_by(|a, b| col[*a].total_cmp(&col[*b])).unwrap();
    let expect = 255 - row_at_frequency(FrequencyScale::Log { min_hz: 20.0 }, f, 256, 24_000.0).floor() as usize;
    assert!(top.abs_diff(expect) <= 1, "brightest row {top}, expected about {expect}");
    let s = v.state_json();
    assert_eq!(s["spectral"]["tiles"], 2); // ceil(ceil(48000 / 512) / 64)
    assert!(s["spectral"]["cache"][0]["misses"].as_u64().unwrap() >= 2);
    // An edit (silence) invalidates: the column falls to the floor; undo brings the tone back.
    v.waveform_edit(hsk_studio_harness::view::waveform::WaveformEdit::Silence(hsk_studio_score::WaveSelection::all(0, 48_000))).unwrap();
    let (_, _, db) = v.spectral_columns(0, 50).unwrap();
    assert!(db.iter().all(|x| (f64::from(*x) + 120.0).abs() < 1e-3));
    v.waveform_undo(true).unwrap();
    let (_, _, db) = v.spectral_columns(0, 50).unwrap();
    assert_eq!(db[top * 50 + 25], col[top]);
    // The image and the panel.
    let (iw, ih, rgba) = v.spectral_image(0, 64).unwrap();
    assert_eq!(rgba.len(), iw * ih * 4);
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for name in [widget::PANEL, widget::WINDOW, widget::SCALE, widget::FFT, widget::IMAGE] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    // Refusals.
    assert!(v.set_spectral_params(SpectralViewParams { rows: 3, ..linear }).is_err());
    assert!(v.spectral_columns(0, 0).is_err());
    assert!(v.spectral_tile(5, 0).is_err());
}

#[test]
fn spectral_selection_edits_are_undo_steps_equal_to_score() {
    use hsk_studio_harness::view::waveform::WaveformDocument;
    use hsk_studio_score::{SpectralMaskOp, SpectralRegion, spectral_mask_edit};
    let dir = support_audio::scratch("spectral-edit");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap();
    let mut x = sine(1000.0, 0.3, 48_000);
    let burst = sine(4400.0, 0.4, 2000);
    for (i, b) in burst.iter().enumerate() {
        x[20_000 + i] += b;
    }
    let y: Vec<f32> = x.iter().map(|s| s * 0.5).collect();
    v.open_waveform(RATE, vec![x.clone(), y.clone()]).unwrap();
    let params = SpectralViewParams { fft_size: 2048, hop: 512, window: SpectralWindow::Hann, scale: FrequencyScale::Linear, rows: 512, floor_db: -120.0 };
    v.open_spectral_display(params).unwrap();
    let base = undo_len(&v);
    assert!(v.apply_spectral(SpectralMaskOp::Remove).is_err(), "no selection yet");
    let lasso = SpectralRegion::Lasso(vec![(18_500.0, 4400.0), (21_000.0, 3000.0), (23_500.0, 4400.0), (21_000.0, 5800.0)]);
    v.set_spectral_region(Some(lasso.clone())).unwrap();
    v.apply_spectral(SpectralMaskOp::Remove).unwrap();
    assert_eq!(undo_len(&v), base + 1);
    // Oracle: score's spectral_mask_edit on each source channel.
    let t = CancellationToken::default();
    let oracle = |input: &[f32], op| {
        let mut out = vec![0.0_f32; input.len()];
        spectral_mask_edit(input, &mut out, RATE, 2048, SpectralWindow::Hann, &lasso, op, &t).unwrap();
        out
    };
    let removed = [oracle(&x, SpectralMaskOp::Remove), oracle(&y, SpectralMaskOp::Remove)];
    let got = v.waveform_buffer().unwrap();
    assert_eq!(got.channels()[0], removed[0]);
    assert_eq!(got.channels()[1], removed[1]);
    v.apply_spectral(SpectralMaskOp::Heal).unwrap();
    let healed = oracle(&removed[0], SpectralMaskOp::Heal);
    assert_eq!(v.waveform_buffer().unwrap().channels()[0], healed);
    assert_eq!(undo_len(&v), base + 2);
    // Refused edit: no step.
    assert!(v.apply_spectral(SpectralMaskOp::Attenuate { gain_db: 5.0 }).is_err());
    assert_eq!(undo_len(&v), base + 2);
    v.waveform_undo(true).unwrap();
    assert_eq!(v.waveform_buffer().unwrap().channels()[0], removed[0]);
    // Panel widgets.
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for name in [widget::LASSO, widget::REMOVE, widget::ATTENUATE, widget::HEAL] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    // Save, reopen, replay; hostile spectral entries are refused.
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.open_waveform(RATE, vec![x.clone(), y.clone()]).unwrap();
    assert_eq!(w.waveform_buffer().unwrap().channels()[1], removed[1]);
    let good = String::from_utf8(WaveformDocument::parse(w.run().document_entry("waveform_edit").unwrap()).unwrap().to_bytes()).unwrap();
    let many = format!("[{}]", vec!["[1.0,1.0]"; 4097].join(","));
    let lasso_json = "[[18500.0,4400.0],[21000.0,3000.0],[23500.0,4400.0],[21000.0,5800.0]]";
    assert!(good.contains(lasso_json), "{good}");
    for (i, bad) in [
        good.replace(lasso_json, &many),
        good.replace("\"window\":\"hann\"", "\"window\":\"kaiser\""),
        good.replace("\"edit\":\"remove\"", "\"edit\":\"explode\""),
        good.replace(lasso_json, "[[1.0,\"x\"]]"),
    ]
    .iter()
    .enumerate()
    {
        assert_ne!(bad, &good, "case {i}");
        assert!(WaveformDocument::parse(bad.as_bytes()).is_err(), "case {i} accepted");
    }
}
