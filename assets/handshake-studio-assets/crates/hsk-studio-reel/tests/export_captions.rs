//! Lane SMB-PR-SOURCE-MONITOR slice 4: caption streams in export (STU-VID-037 facet `captions`; formats
//! are the STU-VID-085 gap, Studio's choice: `tx3g` timed text, FFmpeg's `mov_text`, plus a WebVTT
//! sidecar) and export presets (PR-24, STU-VID-063) with the D24-A OS-encode refusal. The embedded track
//! is read back with reel's own demuxer (sample table, durations, text bytes) and the sidecar compared
//! byte for byte with the hand-written WebVTT. Sources are synthetic (generated here). Premiere / Media
//! Encoder parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::captions::{CaptionCue, CaptionLimits, ExportCaptions, TICKS_PER_MS};
use hsk_studio_reel::presets::{EXPORT_PRESETS, export_preset};
use hsk_studio_reel::*;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

const TPF_25: u64 = 10_160_640_000;
const W: u32 = 264;
const H: u32 = 120;

struct Video {
    frames: u64,
}
fn format() -> VideoFormat {
    VideoFormat {
        width: W,
        height: H,
        bit_depth: 8,
        chroma: ChromaFormat::Yuv422,
        color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
    }
}
impl ExportVideoSource for Video {
    fn format(&self) -> VideoFormat {
        format()
    }
    fn ticks_per_frame(&self) -> u64 {
        TPF_25
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn frame(&mut self, index: u64, _cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        Ok(format()
            .plane_dims()
            .iter()
            .map(|&(w, h)| (0..u64::from(w) * u64::from(h)).map(|i| (64 + (i + index * 7) % 128) as u8).collect())
            .collect())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("reel-captions-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn ms(v: u64) -> u64 {
    v * TICKS_PER_MS
}
fn cue(a: u64, b: u64, text: &str) -> CaptionCue {
    CaptionCue { start_ticks: a, end_ticks: b, text: text.into() }
}
fn job(output: PathBuf, frames: u64) -> ExportJob {
    ExportJob {
        recipe: export_preset("studio.dnxhr.lb.mov").unwrap().recipe(false).map(|mut r| {
            r.audio = None;
            r
        }).unwrap(),
        video: Some(Box::new(Video { frames })),
        audio: None,
        span: SourceSpan { start_ticks: 0, end_ticks: frames * TPF_25 },
        output,
        overwrite: false,
        artifact: "captions.mov".into(),
        limits: ExportLimits::default(),
    }
}
fn captions(cues: Vec<CaptionCue>) -> ExportCaptions {
    ExportCaptions { language: "nld".into(), cues, embed: true, sidecar: true, limits: CaptionLimits::default() }
}

#[test]
fn captions_embed_as_timed_text_and_publish_a_webvtt_sidecar() {
    let d = dir("embed");
    let out = d.join("captions.mov");
    // 10 frames = 400 ms. Cue 1 at 40..120 ms, cue 2 at 200 ms + 1 tick .. 280 ms (rounds to 200 ms).
    let cues = vec![cue(ms(40), ms(120), "Hallo <wereld> & co"), cue(ms(200) + 1, ms(280), "twee\nregels")];
    let mut q = ExportQueue::new();
    q.submit_with_captions(job(out.clone(), 10), captions(cues));
    let r = q.run().remove(0);
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    let c = r.captions.clone().expect("caption receipt");
    assert_eq!((c.cues, c.track_samples, c.exact), (2, 5, false), "gap, cue, gap, cue, tail; one boundary rounded");
    assert_eq!(r.mux.as_ref().unwrap().text_samples, 5);
    // The embedded track, read back by reel's demuxer: handler, entry, language, timing and text.
    let mut f = std::fs::File::open(&out).unwrap();
    let len = f.metadata().unwrap().len();
    let index = build_index(&mut f, len, &Limits::default(), &CancellationToken::default()).unwrap();
    let t = index.tracks.iter().find(|t| &t.handler == b"sbtl").expect("timed-text track");
    assert_eq!((&t.codec, t.media_timescale), (b"tx3g", 1000));
    let durations: Vec<u32> = t.samples().iter().map(|s| s.duration).collect();
    assert_eq!(durations, [40, 80, 80, 80, 120]);
    let texts: Vec<String> = t
        .samples()
        .iter()
        .map(|s| {
            let mut b = vec![0u8; s.size as usize];
            f.seek(SeekFrom::Start(s.offset)).unwrap();
            f.read_exact(&mut b).unwrap();
            let n = u16::from_be_bytes([b[0], b[1]]) as usize;
            String::from_utf8(b[2..2 + n].to_vec()).unwrap()
        })
        .collect();
    assert_eq!(texts, ["", "Hallo <wereld> & co", "", "twee\nregels", ""]);
    // mdhd language `nld` packed as ISO 639-2/T.
    let bytes = std::fs::read(&out).unwrap();
    let packed = (((b'n' - 0x60) as u16) << 10 | ((b'l' - 0x60) as u16) << 5 | (b'd' - 0x60) as u16).to_be_bytes();
    assert!(bytes.windows(2).any(|w| w == packed), "language in mdhd");
    // The sidecar.
    let vtt = std::fs::read_to_string(d.join("captions.vtt")).unwrap();
    assert_eq!(vtt, "WEBVTT\n\n00:00:00.040 --> 00:00:00.120\nHallo &lt;wereld&gt; &amp; co\n\n00:00:00.200 --> 00:00:00.280\ntwee\nregels\n");
    let sha = hsk_studio_reel::hex(&<sha2::Sha256 as sha2::Digest>::digest(vtt.as_bytes()));
    assert_eq!(c.sidecar_sha256.as_deref(), Some(sha.as_str()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hostile_captions_are_refused_before_the_first_frame_and_publish_nothing() {
    let d = dir("hostile");
    let span = 10 * TPF_25;
    let cases: Vec<(ExportCaptions, &str)> = vec![
        (captions(vec![cue(ms(40), ms(120), "a"), cue(ms(100), ms(140), "b")]), "caption-cue-overlap"),
        (captions(vec![cue(ms(200), ms(240), "a"), cue(ms(40), ms(80), "b")]), "caption-cue-order"),
        (captions(vec![cue(ms(40), span + 1, "a")]), "caption-cue-span"),
        (captions(vec![cue(ms(40), ms(40), "a")]), "caption-cue-empty"),
        (captions(vec![cue(ms(40), ms(80), "a --> b")]), "caption-cue-text"),
        (captions(vec![cue(ms(40), ms(80), "a\n\nb")]), "caption-cue-text"),
        (ExportCaptions { language: "NL".into(), ..captions(vec![]) }, "caption-language"),
        (ExportCaptions { language: "en-".into(), ..captions(vec![]) }, "caption-language"),
        (captions(vec![cue(ms(40), ms(80), &"x".repeat(4097))]), "caption-cue-bytes"),
        (
            ExportCaptions { limits: CaptionLimits { max_cues: 1, ..CaptionLimits::default() }, ..captions(vec![cue(0, ms(1), "a"), cue(ms(2), ms(3), "b")]) },
            "caption-cues",
        ),
        (ExportCaptions { embed: false, sidecar: false, ..captions(vec![]) }, "caption-no-output"),
    ];
    for (i, (c, what)) in cases.into_iter().enumerate() {
        let out = d.join(format!("h{i}.mov"));
        let mut q = ExportQueue::new();
        q.submit_with_captions(job(out.clone(), 10), c);
        let r = q.run().remove(0);
        match &r.outcome {
            ExportOutcome::Failed { detail, .. } => assert!(detail.contains(what), "case {i}: {detail} (want {what})"),
            other => panic!("case {i}: {other:?}"),
        }
        assert_eq!(r.frames_encoded, 0, "case {i}: refused before the first frame");
        assert!(!out.exists() && !out.with_extension("vtt").exists(), "case {i}: nothing published");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn presets_have_stable_ids_and_os_encode_presets_are_refused_without_the_os_encoder() {
    let mut ids: Vec<&str> = EXPORT_PRESETS.iter().map(|p| p.preset_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), EXPORT_PRESETS.len(), "preset ids are unique");
    let h264 = export_preset("studio.h264.high-20mbps").unwrap();
    assert!(h264.needs_os_encoder());
    assert_eq!(h264.recipe(false).unwrap_err(), ReelError::CodecUnavailable { codec: "h264".into() });
    let r = h264.recipe(true).unwrap();
    assert_eq!(r.recipe_id, "studio.h264.high-20mbps");
    assert!(matches!(r.video, Some(VideoCodecChoice::H264(s)) if s.bitrate == 20_000_000));
    let hevc = export_preset("studio.hevc.main-15mbps").unwrap();
    assert_eq!(hevc.recipe(false).unwrap_err(), ReelError::CodecUnavailable { codec: "hevc".into() });
    assert!(matches!(hevc.recipe(true).unwrap().video, Some(VideoCodecChoice::Hevc(_))));
    let dnx = export_preset("studio.dnxhr.hq.mov").unwrap().recipe(false).unwrap();
    assert_eq!(dnx.container, Container::Mov);
    let av1 = export_preset("studio.av1.mp4").unwrap().recipe(false);
    if cfg!(feature = "av1-encode") {
        assert!(av1.is_ok());
    } else {
        assert_eq!(av1.unwrap_err(), ReelError::CodecUnavailable { codec: "av1".into() });
    }
    assert!(export_preset("no.such.preset").is_none());
    // An H.264 preset resolved for a queue WITHOUT an encoder is still refused by the queue itself,
    // before the first frame (the D24-A rule holds even when a caller lies about the encoder).
    let d = dir("preset");
    let out = d.join("h264.mp4");
    let mut j = job(out.clone(), 2);
    j.recipe = h264.recipe(true).unwrap();
    j.recipe.audio = None;
    let mut q = ExportQueue::new();
    q.submit(j);
    let r = q.run().remove(0);
    assert!(matches!(&r.outcome, ExportOutcome::Failed { code, .. } if *code == "MEDIA_CODEC_UNAVAILABLE"), "{r:?}");
    assert_eq!(r.frames_encoded, 0);
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&d);
}
