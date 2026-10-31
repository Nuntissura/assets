//! Lane SMB-PR-SEQ-EXPORT-UI-1 (feature `viewer`; every case needs `media`, which the round unit
//! `["desktop", "audio-batch"]` enables): sequence export from the viewer ([STU-VID-030] nested
//! clips are clips: the export resolves them through SMB-PR-NEST-2's `timeline_export_job_with`).
//!
//! - **Headless** (`Viewer::export_sequence`): a sequence holding a nested video+audio clip exports
//!   through reel's queue; the receipt's frame and sample counts equal the counts computed by hand
//!   from in/out; the decoded frame at a hand-picked tick equals the program monitor's own render
//!   of that tick; no frame decoder is a typed refusal; a nested clip whose inner sound the port
//!   lacks is refused `nest_unresolved` naming the clip; audio clips without sound are never
//!   exported silent.
//! - **Window** (timeline toolbar `timeline.export_media`, the inline form, Export / Cancel): the
//!   export runs on the queue's worker while UI frames keep drawing; the result lands in
//!   `state_json()["timeline"]["export"]`; Cancel writes nothing.
//!
//! Oracles: hand-built solid source frames and a stereo test tone; the program monitor's render of
//! the same tick; counts computed from the range (25 fps, 48 kHz: 1920 samples per frame). Premiere's
//! Export Media dialog is UNVERIFIED (D5): the form is the Studio reading.
#![cfg(feature = "viewer")]

mod support_context;

#[cfg(feature = "media")]
mod export_media {
    use hsk_studio_accord::{CancellationToken, Ticks};
    use hsk_studio_folio::{self as folio, *};
    use hsk_studio_harness_view::view::Viewer;
    use hsk_studio_harness_view::view::monitor::{FrameSource, SourceFrame};
    use hsk_studio_harness_view::view::render::Prefer;
    use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
    use hsk_studio_harness_view::view::timeline::{ExportSequenceRequest, TimelineCommand, widget};
    use hsk_studio_pulse::SourceExtents;
    use hsk_studio_reel as reel;
    use serde_json::Value;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// 25 fps ticks per frame (STU-VID-013); 48 kHz has 1920 samples per frame.
    const F: u64 = 10_160_640_000;
    const SPF: u64 = 1920;
    const RED: [f32; 3] = [0.8, 0.2, 0.1];
    const BLUE: [f32; 3] = [0.1, 0.4, 0.9];
    const PRESET: &str = "studio.dnxhr.hq.mov";

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir();
            let text = base.to_string_lossy().to_ascii_uppercase();
            assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
            let dir = base.join(format!("hsk-harness-view-export-media-{}-{label}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cid(n: u32) -> String {
        format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
    }
    fn media(name: &str) -> ClipSource {
        ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
    }
    fn clip(n: u32, source: ClipSource, si: u64, so: u64, start: u64, group: Option<&str>) -> StudioClip {
        StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: cid(n),
            source_ref: source,
            source_in: Ticks::new(si * F),
            source_out: Ticks::new(so * F),
            timeline_start: Ticks::new(start * F),
            duration: Ticks::new((so - si) * F),
            speed: Speed::UNIT,
            enabled: true,
            link_group: group.map(str::to_owned),
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
        }
    }
    fn track(n: u32, kind: TrackKind, name: &str, clips: Vec<StudioClip>) -> StudioTrack {
        StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
            kind,
            index: 0,
            name: name.into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips,
            volume: 1.0,
            pan: 0.0,
        }
    }
    fn sequence(id: &str, tracks: Vec<StudioTrack>) -> StudioSequence {
        let mut s = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: id.into(),
            name: id.into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(0),
            tracks,
            markers: Vec::new(),
            work_area: None,
        };
        s.duration = s.content_end().unwrap();
        s
    }
    /// V1 clip 1 `a` source [10, 30) at frames [0, 20) linked `lg-1` with A1 clip 3 `d` source
    /// [0, 20) at [0, 20); V1 clip 2 `b` source [0, 10) at [30, 40). Work area [5, 25).
    fn main_sequence() -> StudioSequence {
        let mut s = sequence(
            "seq-main",
            vec![
                track(1, TrackKind::Video, "V1", vec![clip(1, media("a"), 10, 30, 0, Some("lg-1")), clip(2, media("b"), 0, 10, 30, None)]),
                track(2, TrackKind::Audio, "A1", vec![clip(3, media("d"), 0, 20, 0, Some("lg-1"))]),
            ],
        );
        s.work_area = Some(WorkArea { start: Ticks::new(5 * F), end: Ticks::new(25 * F) });
        s
    }

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

    fn package(dir: &Path, sequences: Vec<StudioSequence>) -> PathBuf {
        let doc = serde_json::json!({
            "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000009","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
            "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
            "artboards":[],"page_spreads":[],
            "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
            "sequences":sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>()
        });
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
        let path = dir.join("export-media.handshake");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn status(v: &Viewer) -> Value {
        v.state_json()["status"].clone()
    }
    fn sequences(v: &Viewer) -> Vec<StudioSequence> {
        v.run().snapshot().unwrap().document().sequences.clone()
    }

    /// 4x4 solid source frame.
    fn solid(rgb: [f32; 3]) -> Arc<SourceFrame> {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for _ in 0..16 {
            for c in rgb {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
        Arc::new(SourceFrame { width: 4, height: 4, colour_sha256: hsk_studio_prism::profile_hash(&colour), alpha_sha256: hsk_studio_prism::profile_hash(&alpha), colour, alpha })
    }
    /// Frames by media name: `a` red, `b` blue.
    struct Solids;
    impl FrameSource for Solids {
        fn frame(&self, source: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
            let ClipSource::Media { artifact_manifest_id: id, .. } = source else { return None };
            match id.as_str() {
                "a" => Some(solid(RED)),
                "b" => Some(solid(BLUE)),
                _ => None,
            }
        }
    }
    #[derive(Clone, Copy)]
    struct Known;
    impl SourceExtents for Known {
        fn extent(&self, _: &ClipSource) -> Option<Ticks> {
            Some(Ticks::new(1000 * F))
        }
    }
    /// Stereo test tone of 40 frames for media `d` (`present` false: the port has no sound at all).
    #[derive(Clone)]
    struct Tone(Arc<Vec<Vec<f32>>>, bool);
    impl hsk_studio_pulse::AudioSources for Tone {
        fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
            (self.1 && matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "d")).then_some(self.0.as_slice())
        }
    }
    fn tone(present: bool) -> Tone {
        let n = (40 * SPF) as usize;
        Tone(Arc::new(vec![(0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect(), (0..n).map(|i| ((i as f32) * 0.013).cos() * 0.25).collect()]), present)
    }

    /// The fixture open, the program monitor on `seq-main` at 64x32 with the solid frames, the tone
    /// as the export's sound port, and clip 1 (with linked clip 3) nested.
    fn nested_viewer(label: &str, sound: bool) -> (Scratch, Viewer) {
        let scratch = Scratch::new(label);
        let mut v = crate::support_context::viewer(Prefer::Cpu);
        v.open(&package(&scratch.0, vec![main_sequence()])).unwrap_or_else(|e| panic!("{e}"));
        v.open_monitor("seq-main", 64, 32).unwrap();
        v.set_monitor_sources(Box::new(Solids), Box::new(Known));
        v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
        v.set_export_audio(tone(sound), Arc::new(Known), 48_000).unwrap();
        v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        (scratch, v)
    }

    fn read(bytes: &[u8], track: u32, count: u64) -> Vec<(i64, Vec<u8>)> {
        #[derive(Default)]
        struct Collect(Vec<(i64, Vec<u8>)>);
        impl reel::ChunkSink for Collect {
            fn accept(&mut self, c: reel::Chunk<'_>) -> Result<(), reel::ReelError> {
                self.0.push((c.pts, c.bytes.to_vec()));
                Ok(())
            }
        }
        let request = reel::SegmentRequest {
            codec: "isobmff".to_owned(),
            start_pts: Ticks::new(0),
            count,
            track: Some(track),
            output: reel::OutputMode::Decoded,
            prefer: reel::SourcePreference::Original,
            purpose: reel::Purpose::Export,
            grant: reel::Grant { content_id: reel::ContentId::parse("export-media").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        reel::Registry::standard()
            .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &reel::Limits::default(), &CancellationToken::default(), &mut sink)
            .expect("reel reads the export");
        sink.0
    }
    /// BT.709 limited-range 8-bit Y'CbCr back to R'G'B'.
    fn rgb(y: u8, cb: u8, cr: u8) -> [f32; 3] {
        let y = (f32::from(y) - 16.0) / 219.0;
        let cb = (f32::from(cb) - 128.0) / 224.0;
        let cr = (f32::from(cr) - 128.0) / 224.0;
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r, g, b]
    }

    fn request(out: &Path) -> ExportSequenceRequest {
        let mut r = ExportSequenceRequest::new(PRESET, out.to_path_buf());
        r.width = 64;
        r.height = 32;
        r
    }

    /// `seq-main` after Nest of clip 1 (video `a` and audio `d` in the nested sequence) exports
    /// with no in/out given: the work area [5, 25) = 20 frames and 20 x 1920 = 38400 samples per
    /// channel (hand-computed); the decoded frame at timeline frame 8 (file frame 3) equals the
    /// program monitor's own render of that tick (8-bit VC-3 bound 0.02); the receipt and the
    /// range are in the status log.
    #[test]
    fn nested_sequence_exports_through_export_sequence_like_the_program_monitor() {
        let (s, mut v) = nested_viewer("nested", true);
        let out = s.0.join("nested.mov");
        let result = v.export_sequence("seq-main", request(&out)).unwrap_or_else(|e| panic!("{} {}: {}", e.code, e.detail, status(&v)));
        let receipt = &result["receipt"];
        assert_eq!(receipt["outcome"], "complete", "{result}");
        assert_eq!((result["in_tick"].as_u64(), result["out_tick"].as_u64()), (Some(5 * F), Some(25 * F)), "{result}");
        assert_eq!(receipt["frames_encoded"].as_u64(), Some(20), "{result}");
        assert_eq!(receipt["audio_frames"].as_u64(), Some(20 * SPF), "{result}");
        assert_eq!(receipt["recipe_id"], PRESET);
        let log = status(&v).to_string();
        assert!(log.contains("export media") && log.contains("seq-main") && log.contains("complete"), "{log}");
        let bytes = std::fs::read(&out).unwrap();
        // Frame 8 of the timeline is file frame 3; the monitor renders the same tick with the
        // document's nested sequence.
        let cancel = CancellationToken::default();
        // Any timeline command first syncs the monitor with the document (the nested sequence).
        v.timeline_command(TimelineCommand::Select(None)).unwrap();
        let shown = v.monitor().unwrap().render_at(Ticks::new(8 * F), &Solids, &Known, &cancel).unwrap();
        let video = read(&bytes, 1, 20);
        assert_eq!(video.len(), 20);
        let (w, h) = (64usize, 32usize);
        let (pts, f3) = &video[3];
        assert_eq!(*pts, (3 * F) as i64);
        let (yp, cb, cr) = (&f3[..w * h], &f3[w * h..w * h + w / 2 * h], &f3[w * h + w / 2 * h..]);
        for (x, y) in [(24usize, 8usize), (32, 16), (40, 24)] {
            let want = shown.pixel(x as u32, y as u32).unwrap();
            assert!((0..3).all(|c| (want[c] - RED[c]).abs() <= 1e-3), "the monitor shows the nested source: {want:?}");
            let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() <= 0.02, "pixel ({x},{y}) channel {c}: decoded {got:?} vs monitor {want:?}");
            }
        }
        // The file's sound is not silent (the nested clip's tone).
        let audio = read(&bytes, 2, 20 * SPF);
        assert!(audio.iter().any(|(_, b)| b.iter().any(|x| *x != 0)), "the nested sound is in the file");
    }

    /// A viewer whose program monitor has no frame decoder refuses typed `frame_decoder_missing`
    /// before anything renders (never black frames); no file appears; the refusal is in the
    /// status log.
    #[test]
    fn export_without_a_frame_decoder_is_refused_typed() {
        let scratch = Scratch::new("no-decoder");
        let mut v = crate::support_context::viewer(Prefer::Cpu);
        v.open(&package(&scratch.0, vec![main_sequence()])).unwrap_or_else(|e| panic!("{e}"));
        let out = scratch.0.join("never.mov");
        let e = v.export_sequence("seq-main", request(&out)).expect_err("no decoder");
        assert_eq!(e.code, "frame_decoder_missing", "{}", e.detail);
        v.open_monitor("seq-main", 64, 32).unwrap();
        let e = v.export_sequence("seq-main", request(&out)).expect_err("a monitor without a decoder");
        assert_eq!(e.code, "frame_decoder_missing", "{}", e.detail);
        assert!(!out.exists(), "no file");
        assert!(status(&v).to_string().contains("frame_decoder_missing"), "{}", status(&v));
    }

    /// A nested clip whose inner sound the port does not hold is refused `nest_unresolved` naming
    /// the nested clip and its sequence (never silence). Brief change (lead-accepted): a nested
    /// sequence missing from the document cannot be opened (folio refuses the dangling reference),
    /// so this is the reachable form.
    #[test]
    fn nested_clip_without_its_sound_is_refused_naming_the_clip() {
        let (s, mut v) = nested_viewer("unresolved", false);
        let out = s.0.join("never.mov");
        let e = v.export_sequence("seq-main", request(&out)).expect_err("inner sound missing");
        assert_eq!(e.code, "nest_unresolved", "{}", e.detail);
        // The nested sequence clip that replaced clip 3 on A1 of seq-main is named.
        let main = sequences(&v).into_iter().find(|q| q.sequence_id == "seq-main").unwrap();
        let nested_audio = main.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clips.iter().find(|c| matches!(c.source_ref, ClipSource::Sequence { .. })).unwrap().clip_id.clone();
        assert!(e.detail.contains(&nested_audio), "names the nested clip: {}", e.detail);
        let inner = sequences(&v)[1].sequence_id.clone();
        assert!(e.detail.contains(&inner), "names the nested sequence: {}", e.detail);
        assert!(!out.exists());
    }

    /// A sequence with audio clips is never exported silent: no sound port (`set_export_audio`
    /// never called) is refused `audio_sources_missing` naming the audio clip; a port that lacks
    /// the clip's sound is refused by pulse's mix (`mixdown`). Silence is exported only for a
    /// sequence without audio clips (the CLI test's video-only sequence). No file in any case.
    #[test]
    fn sequence_with_audio_clips_and_no_sound_is_refused_never_silent() {
        let scratch = Scratch::new("no-sound");
        let out = scratch.0.join("never.mov");
        let mut v = crate::support_context::viewer(Prefer::Cpu);
        v.open(&package(&scratch.0, vec![main_sequence()])).unwrap();
        v.open_monitor("seq-main", 64, 32).unwrap();
        v.set_monitor_sources(Box::new(Solids), Box::new(Known));
        let e = v.export_sequence("seq-main", request(&out)).expect_err("no sound port");
        assert_eq!(e.code, "audio_sources_missing", "{}", e.detail);
        assert!(e.detail.contains(&cid(3)), "{}", e.detail);
        v.set_export_audio(tone(false), Arc::new(Known), 48_000).unwrap();
        let e = v.export_sequence("seq-main", request(&out)).expect_err("the port lacks the clip's sound");
        assert_eq!(e.code, "mixdown", "{}", e.detail);
        assert!(!out.exists());
        assert!(status(&v).to_string().contains("audio_sources_missing"), "{}", status(&v));
    }

    fn frame(v: &mut Viewer, ctx: &egui::Context, time: f64) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1200.0))), time: Some(time), ..Default::default() };
        let _ = ctx.run(input, |ctx| v.ui(ctx));
    }

    /// Window: the toolbar button `timeline.export_media` opens the inline form (drawn, prefilled
    /// from the work area: in 5, out 24 inclusive); fields set by id (output relative to the save
    /// root, in/out frames, raster); Export starts the job on the queue's worker and returns at
    /// once; UI frames keep drawing while the UI thread serves decoded frames; the result
    /// (`complete`, 10 frames for [10, 19]) lands in `state_json()["timeline"]["export"]["last"]`
    /// and the file exists next to the save path.
    #[test]
    fn export_media_form_runs_in_the_background_and_reports_in_state_json() {
        let (s, mut v) = nested_viewer("window", true);
        let ctx = egui::Context::default();
        v.save_input = s.0.join("project.handshake").to_string_lossy().into_owned();
        frame(&mut v, &ctx, 1.0);
        assert!(v.drawn().iter().any(|d| d == widget::EXPORT_MEDIA), "toolbar button drawn: {:?}", v.drawn());
        v.timeline_command(TimelineCommand::ExportMediaOpen).unwrap();
        frame(&mut v, &ctx, 2.0);
        assert!(v.drawn().iter().any(|d| d == widget::EXPORT_FORM), "form drawn");
        let form = v.state_json()["timeline"]["export"]["form"].clone();
        assert_eq!((form["in_frame"].as_str(), form["out_frame"].as_str(), form["preset_id"].as_str()), (Some("5"), Some("24"), Some(PRESET)), "{form}");
        for (id, value) in [(widget::EXPORT_IN, "10"), (widget::EXPORT_OUT, "19"), (widget::EXPORT_WIDTH, "64"), (widget::EXPORT_HEIGHT, "32"), (widget::EXPORT_OUTPUT, "window.mov")] {
            v.set_timeline_export_field(id, value).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        assert!(v.set_timeline_export_field(widget::EXPORT_PRESET, "no.such.preset").is_err(), "an unknown preset is refused");
        v.timeline_command(TimelineCommand::ExportMediaRun).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        let running = v.state_json()["timeline"]["export"]["running"].clone();
        assert!(running.is_object(), "running after Export: {}", v.state_json()["timeline"]["export"]);
        let mut t = 3.0;
        let started = std::time::Instant::now();
        while v.state_json()["timeline"]["export"]["running"].is_object() {
            assert!(started.elapsed().as_secs() < 120, "export finished: {}", v.state_json()["timeline"]["export"]);
            frame(&mut v, &ctx, t);
            t += 0.016;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let last = v.state_json()["timeline"]["export"]["last"].clone();
        assert_eq!(last["receipt"]["outcome"], "complete", "{last}");
        assert_eq!(last["receipt"]["frames_encoded"].as_u64(), Some(10), "{last}");
        assert_eq!(last["receipt"]["audio_frames"].as_u64(), Some(10 * SPF), "{last}");
        assert!(s.0.join("window.mov").exists(), "written next to the save path");
        assert!(status(&v).to_string().contains("complete"), "{}", status(&v));
    }

    /// Hiding the timeline panel pauses a window export (frames decode on the UI thread while the
    /// panel is drawn; declared gap): `state_json` shows `paused` while hidden and nothing is
    /// published; showing the panel again resumes it, the status log says it was paused, and the
    /// export completes.
    #[test]
    fn hidden_timeline_pauses_the_export_and_showing_it_resumes() {
        let (s, mut v) = nested_viewer("pause", true);
        let ctx = egui::Context::default();
        v.save_input = s.0.join("project.handshake").to_string_lossy().into_owned();
        v.timeline_command(TimelineCommand::ExportMediaOpen).unwrap();
        for (id, value) in [(widget::EXPORT_WIDTH, "64"), (widget::EXPORT_HEIGHT, "32"), (widget::EXPORT_OUTPUT, "paused.mov")] {
            v.set_timeline_export_field(id, value).unwrap();
        }
        v.timeline_command(TimelineCommand::ExportMediaRun).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        assert!(status(&v).to_string().contains("hiding it pauses the export"), "{}", status(&v));
        v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
        assert!(!v.panel_visible(Panel::Timeline), "timeline hidden");
        let started = std::time::Instant::now();
        let mut t = 1.0;
        while v.state_json()["timeline"]["export"]["running"]["paused"] != true {
            assert!(started.elapsed().as_secs() < 30, "paused while hidden: {}", v.state_json()["timeline"]["export"]);
            frame(&mut v, &ctx, t);
            t += 0.016;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!s.0.join("paused.mov").exists(), "nothing published while paused");
        v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
        while v.state_json()["timeline"]["export"]["running"].is_object() {
            assert!(started.elapsed().as_secs() < 120, "export finished: {}", v.state_json()["timeline"]["export"]);
            frame(&mut v, &ctx, t);
            t += 0.016;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(status(&v).to_string().contains("resumed after"), "{}", status(&v));
        assert_eq!(v.state_json()["timeline"]["export"]["last"]["receipt"]["outcome"], "complete");
        assert!(s.0.join("paused.mov").exists());
    }

    /// Cancel closes the form and writes nothing.
    #[test]
    fn export_media_cancel_writes_nothing() {
        let (s, mut v) = nested_viewer("cancel", true);
        v.save_input = s.0.join("project.handshake").to_string_lossy().into_owned();
        v.timeline_command(TimelineCommand::ExportMediaOpen).unwrap();
        v.set_timeline_export_field(widget::EXPORT_OUTPUT, "cancelled.mov").unwrap();
        v.timeline_command(TimelineCommand::ExportMediaCancel).unwrap();
        let export = v.state_json()["timeline"]["export"].clone();
        assert!(export["form"].is_null() && export["running"].is_null() && export["last"].is_null(), "{export}");
        assert!(!s.0.join("cancelled.mov").exists());
        assert!(v.timeline_command(TimelineCommand::ExportMediaRun).is_err(), "no form open");
        assert!(std::fs::read_dir(&s.0).unwrap().all(|e| e.unwrap().path().extension().is_none_or(|x| x != "mov")), "no media file at all");
    }
}
