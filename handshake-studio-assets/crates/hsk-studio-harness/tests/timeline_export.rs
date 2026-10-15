//! Lane SMB-REEL-ENCODE slice 4 (feature `media`): a pulse timeline range exported through reel's queue.
//! Video: red clip, 10-frame cross dissolve, blue clip (the program monitor's composite path); audio: a
//! stereo ramp clip mixed by pulse's score mixdown. Frames 10..30 go to DNxHR HQ + 32-bit float PCM in
//! MOV; the file is read back through reel: 20 frames at exact presentation times whose colours match the
//! hand-computed dissolve, and audio equal to pulse's mixdown of the same range, sample for sample.
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_folio::TrackKind;
use hsk_studio_harness::view::monitor::{FrameSource, SourceFrame};
use hsk_studio_harness::view::timeline_export::{TimelineExport, timeline_export_job};
use hsk_studio_pulse::{AudioSources, CROSS_DISSOLVE, SourceExtents, StreamLimits, TimelineMixStream};
use hsk_studio_reel::*;
use std::io::Cursor;
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const SIDE: u32 = 4;
const RED: [f32; 3] = [0.8, 0.2, 0.1];
const BLUE: [f32; 3] = [0.1, 0.4, 0.9];
const SPF: u64 = 1920;
const SCALE: f32 = 1.0 / 1_048_576.0;

fn solid(rgb: [f32; 3]) -> Arc<SourceFrame> {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for _ in 0..SIDE * SIDE {
        for c in rgb {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&1.0f32.to_le_bytes());
    }
    Arc::new(SourceFrame {
        width: SIDE,
        height: SIDE,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    })
}

struct Solids(Arc<SourceFrame>, Arc<SourceFrame>);
impl FrameSource for Solids {
    fn frame(&self, source: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        match source {
            ClipSource::Media { artifact_manifest_id, .. } if artifact_manifest_id == "red" => Some(self.0.clone()),
            ClipSource::Media { artifact_manifest_id, .. } if artifact_manifest_id == "blue" => Some(self.1.clone()),
            _ => None,
        }
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
struct Ramp(Arc<Vec<Vec<f32>>>);
impl AudioSources for Ramp {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "ramp").then_some(self.0.as_slice())
    }
}
fn ramp() -> Ramp {
    let n = 40 * SPF as usize;
    Ramp(Arc::new(vec![
        (0..n).map(|i| (i as f32 + 1.0) * SCALE).collect(),
        (0..n).map(|i| -(i as f32 + 1.0) * SCALE).collect(),
    ]))
}

fn clip(n: u32, src: &str, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media {
            artifact_manifest_id: src.into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        },
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
        kind,
        index: n - 1,
        name: format!("T{n}"),
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

/// V1: red 0..20, blue 20..40 with a 10-frame dissolve at the cut (frames 15..25); A1: stereo ramp 0..40.
fn sequence() -> StudioSequence {
    let mut red = clip(1, "red", 0, 20, 0);
    red.transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "t1".into(),
        transition_kind: CROSS_DISSOLVE.into(),
        duration: Ticks::new(10 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    });
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(40 * F),
        tracks: vec![
            track(1, TrackKind::Video, vec![red, clip(2, "blue", 100, 20, 20)]),
            track(2, TrackKind::Audio, vec![clip(3, "ramp", 0, 40, 0)]),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}

/// Dissolve progress of sequence frame `n` (as tests/viewer_monitor.rs states it: frame 18 -> 0.3).
fn expected(n: u64) -> [f32; 3] {
    let p = ((n as f32 - 15.0) / 10.0).clamp(0.0, 1.0);
    [0, 1, 2].map(|i| RED[i] * (1.0 - p) + BLUE[i] * p)
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

#[derive(Default)]
struct Collect {
    chunks: Vec<(i64, Vec<u8>)>,
}
impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

fn read(bytes: &[u8], track: u32, count: u64) -> Collect {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: Some(track),
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant { content_id: ContentId::parse("timeline-export").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    Registry::standard()
        .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
        .expect("reel reads the export");
    sink
}

fn export(in_frame: u64, out_frame: u64, output: std::path::PathBuf) -> Result<ExportJob, hsk_studio_harness::view::monitor::MonitorFail> {
    timeline_export_job(
        TimelineExport {
            sequence: sequence(),
            in_tick: in_frame * F,
            out_tick: out_frame * F,
            recipe: ExportRecipe {
                recipe_id: "studio.dnxhr-hq.f32.mov@1".to_owned(),
                container: Container::Mov,
                video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)),
                audio: Some(PcmChoice { bits: 32, float: true }),
            },
            width: 64,
            height: 32,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv422,
            video_sources: Solids(solid(RED), solid(BLUE)),
            video_extents: Extent,
            audio: Some((ramp(), 48_000)),
            audio_extents: Extent,
            output,
            overwrite: false,
            artifact: "seq-main 10-30".to_owned(),
        },
        &CancellationToken::default(),
    )
}

#[test]
fn timeline_range_exports_through_the_render_queue_and_reads_back() {
    let dir = std::env::temp_dir().join(format!("harness-timeline-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("seq-main.mov");
    let mut queue = ExportQueue::new();
    let handle = queue.submit(export(10, 30, out.clone()).unwrap());
    let receipt = queue.run().remove(0);
    assert_eq!(receipt.outcome, ExportOutcome::Complete, "{receipt:?}");
    assert_eq!((receipt.span.start_ticks, receipt.span.end_ticks), (10 * F, 30 * F));
    assert_eq!((receipt.frames_encoded, receipt.audio_frames), (20, 20 * SPF));
    assert_eq!(handle.progress(), ExportProgress { state: ExportState::Complete, done: 20, total: 20 });
    let bytes = std::fs::read(&out).unwrap();

    let video = read(&bytes, 1, 20);
    assert_eq!(video.chunks.len(), 20, "frame count");
    let (w, h) = (64usize, 32usize);
    let mut worst = 0f32;
    for (i, (pts, frame)) in video.chunks.iter().enumerate() {
        assert_eq!(*pts, (i as u64 * F) as i64, "presentation time of exported frame {i}");
        let (yp, cb, cr) = (&frame[..w * h], &frame[w * h..w * h + w / 2 * h], &frame[w * h + w / 2 * h..]);
        let want = expected(10 + i as u64);
        // The 4x4 sources are fitted into 64x32 at scale 8, centred: columns 16..48 carry the picture.
        for (x, y) in [(24usize, 8usize), (32, 16), (40, 24)] {
            let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
            for c in 0..3 {
                worst = worst.max((got[c] - want[c]).abs());
            }
        }
    }
    println!("worst R'G'B' error over exported frames: {worst:.4}");
    // 8-bit limited-range quantization (1/219 luma, 1/224 chroma steps through the inverse matrix) plus
    // VC-3 coding error; measured on this run, bound stated here.
    assert!(worst <= 0.02, "worst R'G'B' error {worst}");

    let audio = read(&bytes, 2, 20 * SPF);
    let got: Vec<f32> = audio.chunks.iter().flat_map(|c| c.1.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<_>>()).collect();
    let cancel = CancellationToken::default();
    let mut mix = TimelineMixStream::new(&sequence(), 48_000, 4800, ramp(), &Extent, StreamLimits::default(), &cancel).unwrap();
    mix.seek(10 * SPF);
    let mut want = Vec::new();
    let (mut l, mut r) = (vec![0f32; 4800], vec![0f32; 4800]);
    for _ in 0..(20 * SPF / 4800) {
        mix.render(&mut l, &mut r, &cancel).unwrap();
        for (a, b) in l.iter().zip(&r) {
            want.push(*a);
            want.push(*b);
        }
    }
    assert_eq!(got.len(), want.len());
    assert!(got.iter().zip(&want).all(|(a, b)| a.to_bits() == b.to_bits()), "PCM equals pulse's mixdown of frames 10..30");
    assert!(want.iter().any(|v| *v != 0.0), "the range is not silent");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timeline_export_refuses_partial_frames_before_rendering() {
    let dir = std::env::temp_dir();
    let job = timeline_export_job(
        TimelineExport {
            sequence: sequence(),
            in_tick: 10 * F + 1,
            out_tick: 30 * F,
            recipe: ExportRecipe { recipe_id: "r".into(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)), audio: None },
            width: 64,
            height: 32,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv422,
            video_sources: Solids(solid(RED), solid(BLUE)),
            video_extents: Extent,
            audio: None::<(Ramp, u32)>,
            audio_extents: Extent,
            output: dir.join("never.mov"),
            overwrite: false,
            artifact: "x".into(),
        },
        &CancellationToken::default(),
    );
    assert_eq!(job.err().map(|e| e.code), Some("export_range"));
    assert!(export(30, 10, dir.join("never.mov")).is_err(), "out before in");
    let huge = timeline_export_job(
        TimelineExport {
            sequence: sequence(),
            in_tick: 0,
            out_tick: 10 * F,
            recipe: ExportRecipe { recipe_id: "r".into(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)), audio: None },
            width: u32::MAX,
            height: u32::MAX,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv422,
            video_sources: Solids(solid(RED), solid(BLUE)),
            video_extents: Extent,
            audio: None::<(Ramp, u32)>,
            audio_extents: Extent,
            output: dir.join("never.mov"),
            overwrite: false,
            artifact: "x".into(),
        },
        &CancellationToken::default(),
    );
    assert_eq!(huge.err().map(|e| e.code), Some("monitor_size"), "absurd raster refused before any allocation");
}
