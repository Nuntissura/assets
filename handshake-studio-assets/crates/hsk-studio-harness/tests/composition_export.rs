//! Lane SMB-AE-MOTION-TOOLS slice 4 (feature `media`): a COMPOSITION goes through reel's render queue
//! (STU-MOT-120/124/125). Video: a 64 x 32 composition with a 16 x 16 red shape layer moving 4 px
//! per frame over a black background; audio: one stereo ramp layer starting at composition frame 4.
//! Frames 2..10 go to DNxHR HQ + 32-bit float PCM in MOV; the file is decoded back through reel: 8
//! frames at exact presentation times with the hand-computed colours, and audio equal to the
//! hand-computed layer placement, sample for sample.
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_composite::vector_nib::shape_layer::{GroupTransform, ShapeFill, ShapeGroup, ShapeItem, shape_layer_content};
use hsk_studio_composite::{BlendSpace, SceneComposition, SceneContent, SceneLayer, ScenePixels};
use hsk_studio_harness::view::timeline_export::composition::{
    CompositionAudioLayer, CompositionExport, CompositionScenes, composition_export_job,
};
use hsk_studio_nib::FillRule;
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_reel::*;
use std::io::Cursor;
use std::sync::Arc;

const F: u64 = 10_160_640_000; // 25 fps
const SPF: u64 = 1920; // 48 kHz samples per frame
const RED: [f32; 3] = [0.8, 0.1, 0.1];
const SPACE: ColourTag = ColourTag { profile_sha256: [0; 32], blend_space: BlendSpace::Encoded };

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

/// The red square shape layer moves to x = 4 * frame, y = 8.
struct Moving(NoPixels);
impl CompositionScenes for Moving {
    fn scene_at(&mut self, tick: u64, cancel: &CancellationToken) -> Result<SceneComposition, String> {
        let frame = tick / F;
        let root = ShapeGroup {
            items: vec![
                ShapeItem::Rect { position: [8.0, 8.0], size: [16.0, 16.0], roundness: 0.0, reversed: false },
                ShapeItem::Fill(ShapeFill { rgba: [RED[0], RED[1], RED[2], 1.0], opacity: 100.0, rule: FillRule::NonZero }),
            ],
            transform: GroupTransform::IDENTITY,
        };
        let content = shape_layer_content("SLYR-square", &root, 0.0, SPACE, 0, &RasterOptions::default(), cancel)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("empty")?;
        Ok(SceneComposition {
            composition_id: "SCOM-019abcde-0000-7000-8000-0000000000c1".into(),
            width: 64,
            height: 32,
            background: Some([0.0, 0.0, 0.0, 1.0]),
            filter: ResampleFilter::Bilinear,
            layers: vec![SceneLayer {
                layer_id: "SLYR-square".into(),
                content: SceneContent::Static(Box::new(content)),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: Affine2 { m: [1.0, 0.0, 4.0 * frame as f64, 0.0, 1.0, 8.0] },
                matte: None,
                clipped: false,
            }],
        })
    }
    fn pixels(&self) -> &dyn ScenePixels {
        &self.0
    }
}

fn ramp() -> Arc<Vec<f32>> {
    Arc::new((0..20_000u32).flat_map(|j| {
        let v = (j % 100) as f32 / 200.0 - 0.25;
        [v, -v]
    }).collect())
}

fn job(in_frame: u64, out_frame: u64, output: std::path::PathBuf) -> Result<ExportJob, hsk_studio_harness::view::monitor::MonitorFail> {
    composition_export_job(CompositionExport {
        scenes: Moving(NoPixels),
        width: 64,
        height: 32,
        ticks_per_frame: F,
        in_tick: in_frame * F,
        out_tick: out_frame * F,
        recipe: ExportRecipe {
            recipe_id: "studio.comp.dnxhr-hq.f32.mov@1".to_owned(),
            container: Container::Mov,
            video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)),
            audio: Some(PcmChoice { bits: 32, float: true }),
        },
        bit_depth: 8,
        chroma: ChromaFormat::Yuv422,
        space: SPACE,
        audio: Some((vec![CompositionAudioLayer { samples: ramp(), start_tick: 4 * F, gain: 0.5 }], 48_000)),
        output,
        overwrite: false,
        artifact: "comp square 2-10".to_owned(),
    })
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
        grant: Grant { content_id: ContentId::parse("composition-export").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    Registry::standard()
        .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
        .expect("reel reads the export");
    sink
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

#[test]
fn composition_renders_through_the_queue_and_decodes_back() {
    let dir = std::env::temp_dir().join(format!("harness-composition-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("comp.mov");
    let mut queue = ExportQueue::new();
    let handle = queue.submit(job(2, 10, out.clone()).unwrap());
    let receipt = queue.run().remove(0);
    assert_eq!(receipt.outcome, ExportOutcome::Complete, "{receipt:?}");
    assert!(receipt.verified);
    assert_eq!((receipt.span.start_ticks, receipt.span.end_ticks), (2 * F, 10 * F));
    assert_eq!((receipt.frames_encoded, receipt.audio_frames), (8, 8 * SPF));
    assert_eq!(handle.progress(), ExportProgress { state: ExportState::Complete, done: 8, total: 8 });
    let bytes = std::fs::read(&out).unwrap();

    // Video: 8 frames at presentation times i * F; the square of composition frame n = 2 + i
    // covers x in [4n, 4n + 16), y in [8, 24): red at its centre, black at (1, 1) and (62, 30).
    let video = read(&bytes, 1, 8);
    assert_eq!(video.chunks.len(), 8, "frame count");
    let (w, h) = (64usize, 32usize);
    let mut worst = 0f32;
    for (i, (pts, frame)) in video.chunks.iter().enumerate() {
        assert_eq!(*pts, (i as u64 * F) as i64, "presentation time of exported frame {i}");
        let (yp, cb, cr) = (&frame[..w * h], &frame[w * h..w * h + w / 2 * h], &frame[w * h + w / 2 * h..]);
        let n = 2 + i;
        for ((x, y), want) in [((4 * n + 8, 16usize), RED), ((1, 1), [0.0; 3]), ((62, 30), [0.0; 3])] {
            let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
            for c in 0..3 {
                worst = worst.max((got[c] - want[c]).abs());
            }
        }
    }
    println!("worst R'G'B' error over exported frames: {worst:.4}");
    // 8-bit limited-range quantisation plus VC-3 coding error (the timeline export's bound).
    assert!(worst <= 0.02, "worst R'G'B' error {worst}");

    // Audio: export sample k is composition sample 3840 + k; the layer starts at sample 4 * 1920 =
    // 7680, so k < 3840 is silence and k >= 3840 is 0.5 * layer[k - 3840].
    let audio = read(&bytes, 2, 8 * SPF);
    let got: Vec<f32> = audio.chunks.iter().flat_map(|c| c.1.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<_>>()).collect();
    let layer = ramp();
    let want: Vec<f32> = (0..8 * SPF as usize)
        .flat_map(|k| if k < 3840 { [0.0, 0.0] } else { [0.5 * layer[2 * (k - 3840)], 0.5 * layer[2 * (k - 3840) + 1]] })
        .collect();
    assert_eq!(got.len(), want.len());
    // Numeric equality (the mix starts from +0.0, so a placed -0.0 sample is written as +0.0).
    let first_diff = got.iter().zip(&want).position(|(a, b)| a != b);
    assert_eq!(first_diff, None, "PCM equals the placed layer");
    assert!(want.iter().any(|v| *v != 0.0), "the span is not silent");
    let _ = std::fs::remove_dir_all(&dir);

    // Refused before rendering: partial frames, empty span, an audio start between samples.
    let never = dir.join("never.mov");
    assert_eq!(job(10, 2, never.clone()).err().map(|e| e.code), Some("export_range"));
    let partial = composition_export_job(CompositionExport {
        scenes: Moving(NoPixels), width: 64, height: 32, ticks_per_frame: F, in_tick: 2 * F + 1, out_tick: 10 * F,
        recipe: ExportRecipe { recipe_id: "r".into(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)), audio: None },
        bit_depth: 8, chroma: ChromaFormat::Yuv422, space: SPACE, audio: None, output: never.clone(), overwrite: false, artifact: "x".into(),
    })
    .is_err();
    assert!(partial, "in point not on a frame");
    let odd = composition_export_job(CompositionExport {
        scenes: Moving(NoPixels), width: 64, height: 32, ticks_per_frame: F, in_tick: 2 * F, out_tick: 10 * F,
        recipe: ExportRecipe { recipe_id: "r".into(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)), audio: Some(PcmChoice { bits: 32, float: true }) },
        bit_depth: 8, chroma: ChromaFormat::Yuv422, space: SPACE,
        audio: Some((vec![CompositionAudioLayer { samples: Arc::new(vec![0.0; 3]), start_tick: 1, gain: 1.0 }], 48_000)),
        output: never, overwrite: false, artifact: "x".into(),
    });
    assert_eq!(odd.err().map(|e| e.code), Some("export_audio"));
}
