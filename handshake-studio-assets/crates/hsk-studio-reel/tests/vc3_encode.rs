//! VC-3 (DNxHR / DNxHD) encoder, lane SMB-REEL-ENCODE: encode -> decode-back proofs.
//!
//! Input: the encoder-input frames of the dnx fixture set (`tests/fixtures/dnx/source-*.yuv.zz`, synthetic
//! ffmpeg lavfi pattern, 264x120 4:2:2, 3 frames; provenance.json). Each frame is encoded by
//! `Vc3Encoder`, decoded back by reel's own VC-3 decoder and compared with the source by PSNR; the same
//! source encoded by ffmpeg's independent DNxHR encoder (the fixture .mov files) decoded by the same
//! decoder is the reference: Studio's encode must not be more than `MARGIN_DB` worse at the same frame
//! size. The bounds were measured on this run and are stated per profile below.
//! An independent decoder (ffmpeg on PATH, black-box) must decode the files Studio writes and agree with
//! reel's decode within 1 LSB (the IDCT rounding tolerance of tests/vc3_decode.rs); without ffmpeg that
//! test prints NOT_PROVEN.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;

const W: u32 = 264;
const H: u32 = 120;
const FRAMES: usize = 3;
const TPF_25: u64 = 10_160_640_000;
/// Studio's encode may trail ffmpeg's encode of the same source at the same frame size by at most this.
const MARGIN_DB: f64 = 1.0;

fn inflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::inflate::decompress_to_vec_zlib(bytes).expect("fixture inflates")
}

fn format(bit_depth: u8) -> VideoFormat {
    VideoFormat {
        width: W,
        height: H,
        bit_depth,
        chroma: ChromaFormat::Yuv422,
        color: ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            origin: ColorOrigin::SequenceHeader,
        },
    }
}

/// Planes (Y, Cb, Cr) of frame `n` of a packed yuv422p / yuv422p10le file.
fn frame_planes(raw: &[u8], bit_depth: u8, n: usize) -> Vec<Vec<u8>> {
    let bps = if bit_depth > 8 { 2 } else { 1 };
    let (y, c) = ((W * H) as usize * bps, (W / 2 * H) as usize * bps);
    let base = n * (y + 2 * c);
    vec![
        raw[base..base + y].to_vec(),
        raw[base + y..base + y + c].to_vec(),
        raw[base + y + c..base + y + 2 * c].to_vec(),
    ]
}

fn samples(plane: &[u8], bit_depth: u8) -> Vec<f64> {
    if bit_depth > 8 {
        plane.chunks_exact(2).map(|p| f64::from(u16::from_le_bytes([p[0], p[1]]))).collect()
    } else {
        plane.iter().map(|b| f64::from(*b)).collect()
    }
}

/// PSNR over all samples of all planes (peak `2^b - 1`).
fn psnr(a: &[Vec<u8>], b: &[Vec<u8>], bit_depth: u8) -> f64 {
    let (mut se, mut n) = (0f64, 0f64);
    for (pa, pb) in a.iter().zip(b) {
        for (x, y) in samples(pa, bit_depth).iter().zip(samples(pb, bit_depth)) {
            se += (x - y) * (x - y);
            n += 1.0;
        }
    }
    let peak = f64::from((1u32 << bit_depth) - 1);
    if se == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / (se / n)).log10() }
}

struct Case {
    profile: Vc3Profile,
    ffmpeg_mov: &'static [u8],
    source_zz: &'static [u8],
    /// PSNR floor measured on this run (dB), rounded down.
    floor_db: f64,
}

fn cases() -> [Case; 3] {
    [
        Case {
            profile: Vc3Profile::DnxhrLb,
            ffmpeg_mov: include_bytes!("fixtures/dnx/dnxhr-lb-264x120.mov"),
            source_zz: include_bytes!("fixtures/dnx/source-8bit-264x120.yuv.zz"),
            floor_db: 59.0, // measured 59.92 / 60.13 / 60.43 (ffmpeg 59.34 / 59.61 / 59.94)
        },
        Case {
            profile: Vc3Profile::DnxhrHq,
            ffmpeg_mov: include_bytes!("fixtures/dnx/dnxhr-hq-264x120.mov"),
            source_zz: include_bytes!("fixtures/dnx/source-8bit-264x120.yuv.zz"),
            floor_db: 61.0, // measured 61.69 / 61.94 / 62.34 (ffmpeg 61.05 / 61.29 / 61.59)
        },
        Case {
            profile: Vc3Profile::DnxhrHqx,
            ffmpeg_mov: include_bytes!("fixtures/dnx/dnxhr-hqx-264x120.mov"),
            source_zz: include_bytes!("fixtures/dnx/source-10bit-264x120.yuv.zz"),
            floor_db: 74.0, // measured 74.76 / 74.94 / 75.23 (ffmpeg 67.16 / 67.32 / 67.60)
        },
    ]
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

fn read(bytes: &[u8], output: OutputMode) -> (SegmentReceipt, Collect) {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count: FRAMES as u64,
        track: None,
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant {
            content_id: ContentId::parse("vc3-encode").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    let receipt = Registry::standard()
        .decode_segment(
            &mut Cursor::new(bytes.to_vec()),
            None,
            &request,
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .expect("reel reads the file");
    (receipt, sink)
}

/// Split a packed decoded frame (reel delivery layout) into its three planes.
fn split(frame: &[u8], bit_depth: u8) -> Vec<Vec<u8>> {
    let bps = if bit_depth > 8 { 2 } else { 1 };
    let (y, c) = ((W * H) as usize * bps, (W / 2 * H) as usize * bps);
    vec![frame[..y].to_vec(), frame[y..y + c].to_vec(), frame[y + c..y + 2 * c].to_vec()]
}

fn encode_mov(profile: Vc3Profile, source: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>, Vec<u32>) {
    let bit_depth = profile.bit_depth();
    let mut encoder = Vc3Encoder::new(profile, format(bit_depth)).unwrap();
    let cancel = CancellationToken::default();
    let mut frames = Vec::new();
    let mut qsf = Vec::new();
    for n in 0..FRAMES {
        let planes = frame_planes(source, bit_depth, n);
        let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
        frames.push(encoder.encode(&refs, &cancel).unwrap());
        qsf.push(encoder.last_qsf());
    }
    let spec = MuxSpec {
        container: Container::Mov,
        video: Some(VideoTrackSpec {
            entry: VideoEntry::Vc3 { dnxhr: profile.is_dnxhr() },
            width: W,
            height: H,
            ticks_per_frame: TPF_25,
            color: Some(NclxColor { primaries: 1, transfer: 1, matrix: 1, full_range: false }),
        }),
        audio: None,
    };
    let mut muxer = Muxer::new(Cursor::new(Vec::new()), spec, MuxLimits::default()).unwrap();
    for (i, f) in frames.iter().enumerate() {
        muxer.write_video(f, i as u64, true).unwrap();
    }
    let (out, _) = muxer.finish().unwrap();
    (out.into_inner(), frames, qsf)
}

/// Encode -> MOV -> reel decode: 3 frames at 0, 1/25, 2/25 s; CBR frame size of equation (7.1); PSNR vs
/// the source at least ffmpeg's encode of the same source minus MARGIN_DB, and above the measured floor.
#[test]
fn vc3_encode_decodes_back_within_psnr_bound_of_ffmpeg_encode() {
    for case in cases() {
        let bit_depth = case.profile.bit_depth();
        let source = inflate(case.source_zz);
        let (mov, frames, qsf) = encode_mov(case.profile, &source);
        let size = vc3_ri_frame_size(case.profile, W, H).unwrap() as usize;
        for f in &frames {
            assert_eq!(f.len(), size, "{:?}: CBR coding unit size", case.profile);
        }
        let (receipt, ours) = read(&mov, OutputMode::Decoded);
        assert_eq!(receipt.video.unwrap().bit_depth, bit_depth);
        assert_eq!(ours.chunks.len(), FRAMES, "{:?}: frame count", case.profile);
        let (_, theirs) = read(case.ffmpeg_mov, OutputMode::Decoded);
        for n in 0..FRAMES {
            assert_eq!(ours.chunks[n].0, (n as u64 * TPF_25) as i64, "{:?}: pts of frame {n}", case.profile);
            let src = frame_planes(&source, bit_depth, n);
            let ours_db = psnr(&split(&ours.chunks[n].1, bit_depth), &src, bit_depth);
            let ffmpeg_db = psnr(&split(&theirs.chunks[n].1, bit_depth), &src, bit_depth);
            println!(
                "{:?} frame {n}: qsf {} studio {ours_db:.2} dB, ffmpeg encode {ffmpeg_db:.2} dB ({} bytes)",
                case.profile, qsf[n], frames[n].len()
            );
            assert!(ours_db >= ffmpeg_db - MARGIN_DB, "{:?} frame {n}: {ours_db:.2} vs {ffmpeg_db:.2}", case.profile);
            assert!(ours_db >= case.floor_db, "{:?} frame {n}: {ours_db:.2} below floor", case.profile);
        }
    }
}

/// Rate control under load: a full-entropy noise frame at DNxHR LB (8192-byte floor raster) does not fit
/// at qsf 1, so the encoder raises the scale factor until the CBR coding unit is met; the frame still
/// decodes to the right raster.
#[test]
fn rate_control_raises_qsf_until_the_coding_unit_fits() {
    let mut encoder = Vc3Encoder::new(Vc3Profile::DnxhrLb, format(8)).unwrap();
    let mut state = 0x1234_5678u32;
    let mut noise = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (16 + state % 220) as u8
            })
            .collect()
    };
    let (y, cb, cr) = (noise((W * H) as usize), noise((W / 2 * H) as usize), noise((W / 2 * H) as usize));
    let frame = encoder.encode(&[&y, &cb, &cr], &CancellationToken::default()).unwrap();
    assert_eq!(frame.len(), 8192);
    let qsf = encoder.last_qsf();
    let (_, planes) = decode_vc3_frame(&frame, (W, H), &Limits::default(), &CancellationToken::default()).unwrap();
    let db = psnr(&planes, &[y.clone(), cb.clone(), cr.clone()], 8);
    println!("DnxhrLb noise frame: qsf {qsf}, {db:.2} dB");
    assert!(qsf > 1, "noise must not fit at qsf 1");
    // Re-encoding the same frame starts at the previous qsf and lands on the same scale.
    let again = encoder.encode(&[&y, &cb, &cr], &CancellationToken::default()).unwrap();
    assert_eq!((again, encoder.last_qsf()), (frame, qsf));
}

/// HD (DNxHD) header path: CID 1252 at its fixed 1280x720 raster, one synthetic frame, decoded back by
/// reel; and every refusal happens in `Vc3Encoder::new` / before the frame is coded.
#[test]
fn dnxhd_raster_and_hostile_inputs() {
    let hd = VideoFormat { width: 1280, height: 720, ..format(8) };
    let mut encoder = Vc3Encoder::new(Vc3Profile::Dnxhd720pMid, hd).unwrap();
    assert_eq!(encoder.frame_bytes(), 303_104);
    let luma: Vec<u8> = (0..1280 * 720).map(|i| ((i % 1280) / 6 + (i / 1280) / 4) as u8).collect();
    let chroma: Vec<u8> = (0..640 * 720).map(|i| (128 + (i % 640 - 320) / 8) as u8).collect();
    let frame = encoder.encode(&[&luma, &chroma, &chroma], &CancellationToken::default()).unwrap();
    assert_eq!(frame.len(), 303_104);
    let (got, planes) = decode_vc3_frame(&frame, (1280, 720), &Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((got.width, got.height, got.bit_depth), (1280, 720, 8));
    let db = psnr(&planes, &[luma.clone(), chroma.clone(), chroma.clone()], 8);
    println!("CID 1252 synthetic 1280x720: {db:.2} dB, qsf {}", encoder.last_qsf());
    assert!(db > 40.0, "smooth synthetic frame: {db:.2} dB");

    let new = |p: Vc3Profile, f: VideoFormat| Vc3Encoder::new(p, f).err().expect("refused");
    let unsupported = |e: ReelError, what: &str| matches!(e, ReelError::Unsupported { what: w } | ReelError::LimitExceeded { what: w } if w == what);
    assert!(unsupported(new(Vc3Profile::DnxhrHq, VideoFormat { width: u32::MAX, height: u32::MAX, ..format(8) }), "vc3-encode-dimensions"));
    assert!(unsupported(new(Vc3Profile::DnxhrHq, VideoFormat { width: 16386, height: 2, ..format(8) }), "vc3-encode-dimensions"));
    assert!(unsupported(new(Vc3Profile::DnxhrHq, VideoFormat { width: 0, ..format(8) }), "vc3-encode-zero-dimension"));
    assert!(unsupported(new(Vc3Profile::DnxhrHq, VideoFormat { width: 263, ..format(8) }), "vc3-encode-odd-dimension"));
    assert!(unsupported(new(Vc3Profile::DnxhrHq, format(10)), "vc3-encode-bit-depth"));
    assert!(unsupported(new(Vc3Profile::Dnxhd720pMid, format(8)), "vc3-encode-profile-raster"));
    assert!(unsupported(new(Vc3Profile::Dnxhd720pMid, VideoFormat { chroma: ChromaFormat::Yuv420, ..hd }), "vc3-encode-chroma"));
    assert!(unsupported(new(Vc3Profile::DnxhrHq, VideoFormat { chroma: ChromaFormat::Yuv444, ..format(8) }), "vc3-encode-chroma"));
    // Plane sizes and 10-bit sample range are checked before coding.
    let mut e = Vc3Encoder::new(Vc3Profile::DnxhrHqx, format(10)).unwrap();
    let y = vec![0u8; (W * H * 2) as usize];
    let c = vec![0u8; (W / 2 * H * 2) as usize];
    assert!(unsupported(e.encode(&[&y, &c], &CancellationToken::default()).unwrap_err(), "vc3-encode-planes"));
    assert!(unsupported(e.encode(&[&y[1..], &c, &c], &CancellationToken::default()).unwrap_err(), "vc3-encode-plane-size"));
    let mut hot = y.clone();
    hot[0..2].copy_from_slice(&1024u16.to_le_bytes());
    assert!(unsupported(e.encode(&[&hot, &c, &c], &CancellationToken::default()).unwrap_err(), "vc3-encode-sample-range"));
    let cancel = CancellationToken::default();
    cancel.cancel();
    assert_eq!(e.encode(&[&y, &c, &c], &cancel).unwrap_err(), ReelError::Canceled);
}

fn ffmpeg() -> Option<&'static str> {
    let ok = std::process::Command::new("ffmpeg").arg("-version").output().ok()?.status.success();
    ok.then_some("ffmpeg")
}

/// Independent decoder: ffmpeg decodes every Studio-written MOV (LB, HQ, HQX) to the same pictures as
/// reel's decoder within 1 LSB. NOT_PROVEN without ffmpeg on PATH.
#[test]
fn ffmpeg_decodes_studio_vc3_like_reel() {
    let Some(ffmpeg) = ffmpeg() else {
        println!("NOT_PROVEN: ffmpeg not on PATH; independent VC-3 decode not checked");
        return;
    };
    let dir = std::env::temp_dir().join(format!("reel-vc3-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for case in cases() {
        let bit_depth = case.profile.bit_depth();
        let (mov, _, _) = encode_mov(case.profile, &inflate(case.source_zz));
        let path = dir.join(format!("{}.mov", case.profile.token()));
        std::fs::write(&path, &mov).unwrap();
        let out = std::process::Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}: {}", case.profile, String::from_utf8_lossy(&out.stderr));
        let (_, ours) = read(&mov, OutputMode::Decoded);
        let reel: Vec<u8> = ours.chunks.iter().flat_map(|c| c.1.clone()).collect();
        assert_eq!(out.stdout.len(), reel.len(), "{:?}: ffmpeg frame bytes", case.profile);
        let (a, b) = (samples(&out.stdout, bit_depth), samples(&reel, bit_depth));
        let max = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        println!("{:?}: ffmpeg vs reel decode of Studio's file: max delta {max}", case.profile);
        if case.profile == Vc3Profile::DnxhrHqx {
            // Same class as tests/vc3_decode.rs (lane-accepted NOT_PROVEN R24-R26): ffmpeg's CID 1271 luma
            // weights differ from ST 2019-1 Table D.1, which this encoder and reel's decoder use.
            println!("NOT_PROVEN: DnxhrHqx (CID 1271) ffmpeg agreement (max delta {max}); ffmpeg's CID 1271 weights differ from ST 2019-1 Table D.1");
            assert!(max <= 8.0, "{:?}: max delta {max} beyond the known weight-table difference", case.profile);
        } else {
            assert!(max <= 1.0, "{:?}: max delta {max}", case.profile);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
