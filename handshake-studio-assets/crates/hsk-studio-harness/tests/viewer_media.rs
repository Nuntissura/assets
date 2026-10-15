//! Lane SMB-PULSE-PLAYBACK-2 (feature `media`): the program monitor shows real decoded clip
//! frames. Media: reel's committed fixtures (ffmpeg-made, provenance in
//! `hsk-studio-reel/tests/fixtures/*/provenance.json`), read through the crate tree, never a
//! machine path. Oracle: ffmpeg's own decode of the ProRes fixture (`.ref.yuv`, yuv422p10le)
//! converted to R'G'B' by the formula written out here (H.273 matrix, limited range); the
//! monitor pixels match it within 0.01 (reel's ProRes decode is within 1 code value of ffmpeg),
//! and every presented frame is the expected source frame (closer to it than to any other).
//! Frame hashes (SHA-256 of the monitor's composited RGBA f32 pixels) are pinned at stated ticks.
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_harness::view::media::{MediaFrames, ReelFrameDecoder};
use hsk_studio_harness::view::monitor::{DropPolicy, FrameSource, HeadlessClock, ProgramMonitor};
use hsk_studio_pulse::{FrameCacheLimits, SourceExtents};
use std::sync::Arc;

/// 25 fps.
const F: u64 = 10_160_640_000;
const SPF: u64 = 1920;
const PRORES: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/prores/prores-422hq-40x24-p.mov");
const PRORES_REF: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/prores/prores-422hq-40x24-p.ref.yuv");
const H264: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/mov/mov-h264-bf2-negcts-tc-df-64x36.mov");
const H264_PYRAMID: &[u8] = include_bytes!("../../hsk-studio-reel-native/tests/fixtures/h264/h264-high-bf3-pyramid-128x72-24f.mp4");
const H264_PYRAMID_FRAMEHASH: &str = include_str!("../../hsk-studio-reel-native/tests/fixtures/h264/h264-high-bf3-pyramid-128x72-24f.framehash");
/// 29.97 fps (STU-VID-013 table).
const F2997: u64 = 8_475_667_200;
const DNX: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/dnx/dnxhr-lb-264x120.mov");

fn media(id: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: id.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: format!("{id}00"),
        },
        asset_id: None,
    }
}

fn clip(n: u32, src: &str, source_in: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: media(src),
        source_in: Ticks::new(source_in * F),
        source_out: Ticks::new((source_in + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

fn sequence(clips: Vec<StudioClip>, frames: u64) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(frames * F),
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
            volume: 1.0,
            pan: 0.0,
            clips,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1_000_000 * F))
    }
}

fn frames_with(items: &[(&str, &[u8])]) -> MediaFrames {
    let mut decoder = ReelFrameDecoder::new();
    for (id, bytes) in items {
        decoder.register(&media(id), Arc::from(*bytes)).unwrap();
    }
    MediaFrames::new(decoder, FrameCacheLimits::default(), 4).unwrap()
}

/// ffmpeg's frame `n` of the ProRes fixture as R'G'B' (yuv422p10le, limited range).
fn oracle(n: usize, x: usize, y: usize, kr: f64, kb: f64) -> [f64; 3] {
    let (w, h, cw) = (40usize, 24usize, 20usize);
    let frame = (w * h + 2 * cw * h) * 2;
    let base = n * frame;
    let at = |i: usize| f64::from(u16::from_le_bytes([PRORES_REF[base + 2 * i], PRORES_REF[base + 2 * i + 1]]));
    let yy = at(y * w + x);
    let cb = at(w * h + y * cw + x / 2);
    let cr = at(w * h + cw * h + y * cw + x / 2);
    let l = (yy - 64.0) / 876.0;
    let (pb, pr) = ((cb - 512.0) / 896.0, (cr - 512.0) / 896.0);
    let r = l + 2.0 * (1.0 - kr) * pr;
    let b = l + 2.0 * (1.0 - kb) * pb;
    let g = (l - kr * r - kb * b) / (1.0 - kr - kb);
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

fn sha256_hex(bytes: &[u8]) -> String {
    hsk_studio_prism::profile_hash(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Max |monitor - oracle frame n| over all pixels and channels.
fn distance(m: &ProgramMonitor, n: usize, k: (f64, f64)) -> f64 {
    let img = &m.shown().unwrap().image;
    let mut worst = 0.0f64;
    for y in 0..24u32 {
        for x in 0..40u32 {
            let p = img.pixel(x, y).unwrap();
            let o = oracle(n, x as usize, y as usize, k.0, k.1);
            for c in 0..3 {
                worst = worst.max((f64::from(p[c]) - o[c]).abs());
            }
            assert_eq!(p[3], 1.0);
        }
    }
    worst
}

#[test]
fn program_monitor_shows_decoded_prores_frames_at_stated_ticks() {
    let cancel = CancellationToken::default();
    // V1: source frames 0..4 at 0..4, then source frames 1..3 again at 4..6.
    let seq = sequence(vec![clip(1, "prores", 0, 4, 0), clip(2, "prores", 1, 2, 4)], 6);
    let sources = frames_with(&[("prores", PRORES)]);
    let mut m = ProgramMonitor::new(seq, 40, 24, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    let expected_source = [0usize, 1, 2, 3, 1, 2];
    let mut hashes = Vec::new();
    m.play(&clock).unwrap();
    for (k, &want) in expected_source.iter().enumerate() {
        assert_eq!(m.refresh(&clock, &sources, &Ext, &cancel).unwrap(), Some(k as u64), "{}", m.state_json());
        let note = sources.with_pipeline(|p| p.decoder().note());
        let coeff = match note.matrix {
            1 => (0.2126, 0.0722),
            6 | 5 => (0.299, 0.114),
            other => panic!("unexpected matrix {other}: {note:?}"),
        };
        let d: Vec<f64> = (0..4).map(|n| distance(&m, n, coeff)).collect();
        assert!(d[want] <= 0.01, "timeline frame {k}: {d:?}");
        assert!((0..4).all(|n| n == want || d[n] > d[want] + 0.01), "frame {k} is source frame {want}: {d:?}");
        let img = &m.shown().unwrap().image;
        let mut rgba = Vec::new();
        for y in 0..24 {
            for x in 0..40 {
                for c in img.pixel(x, y).unwrap() {
                    rgba.extend_from_slice(&c.to_le_bytes());
                }
            }
        }
        hashes.push(sha256_hex(&rgba));
        clock.advance(SPF);
    }
    println!("monitor frame hashes at ticks k*{F}: {hashes:#?}");
    // Pinned (regression; correctness is the oracle comparison above). Timeline frame k at tick
    // k * 10160640000 shows source frames 0, 1, 2, 3, 1, 2.
    const PINNED: [&str; 4] = [
        "f6ac8b44e137beb5765c3abaaa80c29d6c55416b47152915fda0d48bccd76314",
        "d0293a477924f6ffb2ec54ecb9647693e21d4e8ecb401a176e49b5f1c3de065d",
        "dfd17609ce78cc7f875e63d0f46e4cabee245cec8871f62712eb150e50986539",
        "866e1f6af852e4f601c7c015ca64cb5a223f9fa6f7eb80cfaadd2120d87fdf4c",
    ];
    for (k, &src) in expected_source.iter().enumerate() {
        assert_eq!(hashes[k], PINNED[src], "timeline frame {k}");
    }
    // Same source frame at timeline frames 1 and 4 (source 1), 2 and 5 (source 2): identical pixels.
    assert_eq!(hashes[1], hashes[4]);
    assert_eq!(hashes[2], hashes[5]);
    assert_eq!(hashes.iter().collect::<std::collections::BTreeSet<_>>().len(), 4);
    // The decode ran in runs of 4 with prefetch: far fewer decoder calls than frames shown, no
    // refusal, and frames came from the cache.
    let stats = sources.with_pipeline(|p| p.cache().stats());
    assert!(stats.decodes <= 3 && stats.hits >= 6, "{stats:?}");
    assert!(m.state_json()["error"].is_null());

    // A smaller frame than the monitor is fitted (2x, bilinear): its centre pixel keeps the colour.
    let seq = sequence(vec![clip(1, "prores", 0, 4, 0)], 4);
    let mut big = ProgramMonitor::new(seq, 80, 48, DropPolicy::DropLate).unwrap();
    assert_eq!(big.refresh(&clock, &sources, &Ext, &cancel).unwrap(), Some(0));
    let small = sources.frame(&media("prores"), Ticks::new(0)).unwrap();
    let px = |x: usize, y: usize| -> [f32; 3] {
        let i = (y * 40 + x) * 12;
        let f = |o: usize| f32::from_le_bytes(small.colour[i + o..i + o + 4].try_into().unwrap());
        [f(0), f(4), f(8)]
    };
    // Uniform region check: where the 2x2 source neighbourhood is flat, the fitted pixel equals it.
    let got = big.shown().unwrap().image.pixel(41, 25).unwrap();
    let want = px(20, 12);
    assert!((0..3).all(|c| (got[c] - want[c]).abs() <= 0.08), "{got:?} vs {want:?}");
}

#[test]
fn missing_codecs_hostile_media_and_absurd_seeks_are_typed_refusals() {
    let cancel = CancellationToken::default();
    let clock = HeadlessClock::new(48_000);
    // Y4M header that declares a 65536 x 65536 picture: refused before any frame is read.
    let bomb = b"YUV4MPEG2 W65536 H65536 F25:1 Ip A1:1 C420\nFRAME\n".to_vec();
    let truncated = PRORES[..PRORES.len() / 3].to_vec();
    let sources = frames_with(&[
        ("h264", H264),
        ("bomb", &bomb),
        ("cut", &truncated),
        ("junk", b"definitely not media at all"),
        ("prores", PRORES),
    ]);
    let refused = |src: &str, source_in: u64, w: u32, h: u32| -> String {
        let seq = sequence(vec![clip(1, src, source_in, 2, 0)], 2);
        let mut m = ProgramMonitor::new(seq, w, h, DropPolicy::DropLate).unwrap();
        let e = m.refresh(&clock, &sources, &Ext, &cancel).unwrap_err();
        assert_eq!(m.last_error().unwrap().code, e.code);
        assert_eq!(m.state_json()["error"], e.code);
        assert!(m.shown().is_none(), "no empty frame is shown for {src}");
        e.code.to_owned()
    };
    // H.264 without the isolated worker registered: reel's MEDIA_CODEC_UNAVAILABLE, typed.
    assert_eq!(refused("h264", 0, 64, 36), "codec_unavailable");
    assert_eq!(refused("bomb", 0, 64, 36), "limit_exceeded");
    let code = refused("cut", 0, 40, 24);
    assert!(["corrupt", "truncated", "io", "out_of_range"].contains(&code.as_str()), "{code}");
    assert_eq!(refused("junk", 0, 40, 24), "import_unsupported");
    assert_eq!(refused("nobody", 0, 40, 24), "media_unregistered");
    // Absurd seeks: far past the 4-frame source, and at the tick ceiling.
    assert_eq!(refused("prores", 1_000, 40, 24), "out_of_range");
    let mut far = clip(1, "prores", 0, 2, 0);
    far.source_in = Ticks::new(u64::MAX - 4 * F);
    far.source_out = Ticks::new(u64::MAX - 2 * F);
    let mut m = ProgramMonitor::new(sequence(vec![far], 2), 40, 24, DropPolicy::DropLate).unwrap();
    assert!(m.refresh(&clock, &sources, &Ext, &cancel).is_err());
    // Nothing was decoded for the bomb or the missing codec; the refusals are remembered.
    sources.with_pipeline(|p| {
        assert_eq!(p.refusal(&media("h264")).unwrap().code, "codec_unavailable");
        assert_eq!(p.refusal(&media("bomb")).unwrap().code, "limit_exceeded");
        assert!(p.cache().used_bytes() <= FrameCacheLimits::default().max_bytes);
    });
    // The good source still plays afterwards.
    let mut ok = ProgramMonitor::new(sequence(vec![clip(1, "prores", 0, 4, 0)], 4), 40, 24, DropPolicy::DropLate).unwrap();
    assert_eq!(ok.refresh(&clock, &sources, &Ext, &cancel).unwrap(), Some(0));
    // A cache too small for one frame refuses from the declared size, before decoding.
    let mut tiny = ReelFrameDecoder::new();
    tiny.register(&media("prores"), Arc::from(PRORES)).unwrap();
    let small = MediaFrames::new(
        tiny,
        FrameCacheLimits { max_bytes: 1024, max_entries: 4, max_frame_bytes: 1024 },
        2,
    )
    .unwrap();
    let mut m = ProgramMonitor::new(sequence(vec![clip(1, "prores", 0, 4, 0)], 4), 40, 24, DropPolicy::DropLate).unwrap();
    assert_eq!(m.refresh(&clock, &small, &Ext, &cancel).unwrap_err().code, "limit_exceeded");
    assert_eq!(small.with_pipeline(|p| p.cache().stats().decodes), 0);
    // VC-3 (DNxHR LB, in every build) decodes too: 264x120 frames, three distinct.
    let dnx = frames_with(&[("dnx", DNX)]);
    let mut d = ProgramMonitor::new(sequence(vec![clip(1, "dnx", 0, 3, 0)], 3), 264, 120, DropPolicy::DropLate).unwrap();
    let mut seen = Vec::new();
    for k in 0..3u64 {
        d.scrub(Ticks::new(k * F), &clock).unwrap();
        assert_eq!(d.refresh(&clock, &dnx, &Ext, &cancel).unwrap(), Some(k));
        seen.push(d.shown().unwrap().image.pixel(100, 60).unwrap());
    }
    assert!(seen[0] != seen[1] && seen[1] != seen[2], "{seen:?}");
}

/// H.264 through the isolated Media Foundation worker (feature `media-native`, Windows). The
/// worker executable must have been built next to the test's target directory
/// (`cargo build -p hsk-studio-reel-native --features mf-worker --bin reel-native-mf-worker`);
/// otherwise this prints NOT_PROVEN and proves nothing.
#[cfg(feature = "media-native")]
#[test]
fn h264_frames_decode_through_the_media_foundation_worker() {
    use hsk_studio_reel_native::{MfWorkerOptions, WorkerMfProvider};
    let exe = std::env::current_exe().unwrap();
    let worker = exe
        .parent()
        .and_then(|deps| deps.parent())
        .map(|dir| dir.join(format!("reel-native-mf-worker{}", std::env::consts::EXE_SUFFIX)));
    let Some(worker) = worker.filter(|w| w.is_file()) else {
        println!("NOT_PROVEN: reel-native-mf-worker not built beside the test target; H.264 monitor decode unproven");
        return;
    };
    let provider = match WorkerMfProvider::new(MfWorkerOptions::default().with_worker_program(worker)) {
        Ok(p) => p,
        Err(e) => {
            println!("NOT_PROVEN: Media Foundation worker unavailable: {e:?}");
            return;
        }
    };
    let mut decoder = ReelFrameDecoder::new().with_external(Arc::new(provider));
    decoder.register(&media("h264"), Arc::from(H264_PYRAMID)).unwrap();
    let sources = MediaFrames::new(decoder, FrameCacheLimits::default(), 4).unwrap();
    let cancel = CancellationToken::default();
    let clock = HeadlessClock::new(48_000);
    // ffmpeg's per-frame SHA-256 of the yuv420p frames (provenance in reel-native's fixtures).
    let oracle: Vec<String> = H264_PYRAMID_FRAMEHASH
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_owned())
        .collect();
    assert_eq!(oracle.len(), 24);
    let mut seq = sequence(vec![clip(1, "h264", 0, 24, 0)], 24);
    seq.frame_rate = SequenceFrameRate { ticks_per_frame: F2997 };
    for c in &mut seq.tracks[0].clips {
        c.source_out = Ticks::new(24 * F2997);
        c.duration = Ticks::new(24 * F2997);
    }
    seq.duration = Ticks::new(24 * F2997);
    let mut m = ProgramMonitor::new(seq, 128, 72, DropPolicy::DropLate).unwrap();
    // No prefetch here, so the decoder note of each refresh is the run that served it.
    m.set_prefetch_ahead(0).unwrap();
    // Stated ticks: frames 0, 5, 11 (inside B runs of the pyramid), 23 (last), then back to 2.
    let mut verified = std::collections::BTreeSet::new();
    for k in [0u64, 5, 11, 23, 2] {
        m.scrub(Ticks::new(k * F2997), &clock).unwrap();
        match m.refresh(&clock, &sources, &Ext, &cancel) {
            Ok(Some(f)) => assert_eq!(f, k),
            other => {
                let code = m.last_error().map(|e| e.code).unwrap_or("none");
                if code == "codec_unavailable" {
                    println!("NOT_PROVEN: Media Foundation H.264 decoder absent on this host");
                    return;
                }
                panic!("frame {k}: {other:?} {}", m.state_json());
            }
        }
        // Every frame decoded so far equals ffmpeg's frame at the same presentation index.
        let note = sources.with_pipeline(|p| p.decoder().note());
        assert_eq!(note.decoder_id, "mf-h264", "{note:?}");
        for (pts, digest) in &note.planes {
            assert_eq!(pts % F2997, 0);
            assert_eq!(digest, &oracle[(pts / F2997) as usize], "frame at tick {pts}");
            verified.insert(pts / F2997);
        }
    }
    for k in [0u64, 5, 11, 23, 2] {
        assert!(verified.contains(&k), "frame {k} decoded and verified: {verified:?}");
    }
    let checked = verified.len();
    println!("h264 planes verified vs ffmpeg framehash: {checked} frames; cache {:?}", sources.with_pipeline(|p| p.cache().stats()));
}
