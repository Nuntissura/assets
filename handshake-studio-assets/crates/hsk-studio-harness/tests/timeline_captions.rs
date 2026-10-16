//! Lane SMB-PR-SOURCE-MONITOR slice 4 (feature `media`): a timeline range exported through a Studio
//! preset with the sequence's Folio caption track as a caption stream (embedded `tx3g` timed text plus a
//! WebVTT sidecar), and the D24-A rule for H.264 presets: refused typed without the OS encoder; with
//! feature `os-encode` and the `reel-native-mf-worker` built beside the test target, encoded by the OS
//! encoder (otherwise NOT_PROVEN). Premiere / Media Encoder parity UNVERIFIED (D5).
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_folio::TrackKind;
use hsk_studio_harness::view::monitor::{FrameSource, SourceFrame};
use hsk_studio_harness::view::timeline_export::{
    TimelineExport, export_captions, export_queue, iso639_2, preset_recipe, timeline_export_job,
};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_reel::*;
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const MS: u64 = 254_016_000;

struct Grey(u32, u32);
impl FrameSource for Grey {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for _ in 0..self.0 * self.1 {
            for c in [0.5f32, 0.4, 0.3] {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
        Some(Arc::new(SourceFrame {
            width: self.0,
            height: self.1,
            colour_sha256: hsk_studio_prism::profile_hash(&colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
            colour,
            alpha,
        }))
    }
}
#[derive(Clone)]
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
#[derive(Clone)]
struct NoAudio;
impl AudioSources for NoAudio {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        None
    }
}

fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(20 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![StudioClip {
                schema_id: ClipSchema::V1,
                clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
                source_ref: ClipSource::Media {
                    artifact_manifest_id: "grey".into(),
                    content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
                },
                source_in: Ticks::new(0),
                source_out: Ticks::new(20 * F),
                timeline_start: Ticks::new(0),
                duration: Ticks::new(20 * F),
                speed: Speed::UNIT,
                enabled: true,
                link_group: None,
                markers: Vec::new(),
                transitions: ClipTransitions::default(),
            }],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

fn caption_track() -> StudioCaptionTrack {
    let seg = |start: u64, end: u64, text: &str| StudioCaptionSegment { start, end, text: text.into(), style: StudioCaptionStyle::default() };
    StudioCaptionTrack {
        track_id: "STRK-019abcde-0000-7000-8000-0000000000c1".into(),
        language: "nl-BE".into(),
        active: true,
        constraints: StudioCaptionConstraints::default(),
        // Frames 4..14 are exported (160..560 ms): the first segment is cut at the in point, the last is
        // outside the span.
        segments: vec![seg(100 * MS, 240 * MS, "eerste"), seg(300 * MS, 400 * MS, "tweede"), seg(600 * MS, 700 * MS, "buiten")],
    }
}

fn job(recipe: ExportRecipe, side: u32, out: std::path::PathBuf, frames: (u64, u64)) -> ExportJob {
    timeline_export_job(
        TimelineExport {
            sequence: sequence(),
            in_tick: frames.0 * F,
            out_tick: frames.1 * F,
            recipe,
            width: side,
            height: side,
            bit_depth: 8,
            chroma: if side % 16 == 0 { ChromaFormat::Yuv420 } else { ChromaFormat::Yuv422 },
            video_sources: Grey(side, side),
            video_extents: Extent,
            audio: None::<(NoAudio, u32)>,
            audio_extents: Extent,
            output: out,
            overwrite: false,
            artifact: "captions".into(),
        },
        &CancellationToken::default(),
    )
    .unwrap()
}

#[test]
fn timeline_export_carries_the_caption_track_through_a_studio_preset() {
    assert_eq!((iso639_2("nl-BE").as_str(), iso639_2("eng").as_str(), iso639_2("x-klingon").as_str()), ("nld", "eng", "und"));
    let c = export_captions(&caption_track(), 4 * F, 14 * F, true, true);
    assert_eq!(c.language, "nld");
    let cues: Vec<(u64, u64, &str)> = c.cues.iter().map(|q| (q.start_ticks / MS, q.end_ticks / MS, q.text.as_str())).collect();
    assert_eq!(cues, [(0, 80, "eerste"), (140, 240, "tweede")], "clipped to the span, span-relative");

    let dir = std::env::temp_dir().join(format!("hsk-timeline-captions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (mut queue, os) = export_queue(None);
    let mut recipe = preset_recipe("studio.dnxhr.lb.mov", os).unwrap();
    recipe.audio = None;
    let out = dir.join("captions.mov");
    queue.submit_with_captions(job(recipe, 8, out.clone(), (4, 14)), c);
    let r = queue.run().remove(0);
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    assert_eq!(r.recipe_id, "studio.dnxhr.lb.mov");
    let receipt = r.captions.unwrap();
    assert_eq!((receipt.language.as_str(), receipt.cues, receipt.track_samples, receipt.exact), ("nld", 2, 4, true));
    let vtt = std::fs::read_to_string(dir.join("captions.vtt")).unwrap();
    assert_eq!(vtt, "WEBVTT\n\n00:00:00.000 --> 00:00:00.080\neerste\n\n00:00:00.140 --> 00:00:00.240\ntweede\n");

    // D24-A: an H.264 preset is a typed refusal without the OS encoder.
    if !os {
        assert_eq!(preset_recipe("studio.h264.high-8mbps", false).unwrap_err().code, "codec_unavailable");
    }
    assert_eq!(preset_recipe("no.such", false).unwrap_err().code, "unknown_preset");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Feature `os-encode`: the H.264 preset runs through reel-native's OS encoder worker.
#[cfg(feature = "os-encode")]
#[test]
fn h264_preset_encodes_through_the_os_encoder_with_captions() {
    let exe = std::env::current_exe().unwrap();
    let worker = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|dir| dir.join(format!("reel-native-mf-worker{}", std::env::consts::EXE_SUFFIX)));
    let Some(worker) = worker.filter(|w| w.is_file()) else {
        println!("NOT_PROVEN: reel-native-mf-worker not built beside the test target; OS H.264 preset unproven");
        return;
    };
    let (mut queue, os) = export_queue(Some(worker));
    assert!(os, "the OS encoder is registered");
    let mut recipe = preset_recipe("studio.h264.high-8mbps", os).unwrap();
    recipe.audio = None;
    let dir = std::env::temp_dir().join(format!("hsk-timeline-h264-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("h264.mp4");
    queue.submit_with_captions(job(recipe, 64, out.clone(), (4, 14)), export_captions(&caption_track(), 4 * F, 14 * F, true, false));
    let r = queue.run().remove(0);
    match &r.outcome {
        ExportOutcome::Complete => {
            assert_eq!(r.video_codec, Some("h264"));
            assert_eq!(r.frames_encoded, 10);
            assert_eq!(r.captions.as_ref().unwrap().track_samples, 4);
            assert!(r.verified);
        }
        ExportOutcome::Failed { code, detail } if *code == "MEDIA_CODEC_UNAVAILABLE" => {
            println!("NOT_PROVEN: no OS H.264 encoder on this host: {detail}");
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
