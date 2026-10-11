//! External-input CLI for hsk-studio-reel. All inputs are explicit arguments; fixtures are generated in
//! memory (nothing is written, nothing is downloaded, no windows).
//!
//! reel-consumer --descriptor | --list-codecs | --encode-roundtrip
//! reel-consumer --probe FILE
//! reel-consumer --decode FILE --start-unit N --units N [--packets] [--track ID]
//! reel-consumer --seek FILE --ticks T
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::fixtures::{y4m_bytes, y4m_frame};
use hsk_studio_reel::*;
use std::{env, fs::File, io::Cursor, process::ExitCode};

struct Summary {
    chunks: u32,
    units: u64,
    bytes: u64,
    peak: usize,
    first_pts: Option<i64>,
    last_pts: Option<i64>,
}

impl ChunkSink for Summary {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks += 1;
        self.units += chunk.units;
        self.bytes += chunk.bytes.len() as u64;
        self.peak = self.peak.max(chunk.bytes.len());
        self.first_pts.get_or_insert(chunk.pts);
        self.last_pts = Some(chunk.pts);
        Ok(())
    }
}

struct Frames {
    next: u64,
    total: u64,
}

impl ChunkSource for Frames {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
        if self.next == self.total {
            return Ok(None);
        }
        buf.extend_from_slice(&y4m_frame(self.next, 64, 36, "420"));
        self.next += 1;
        Ok(Some(1))
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn fail(error: &ReelError) -> ExitCode {
    println!("{{\"error\":\"{}\",\"detail\":\"{error}\"}}", error.code());
    ExitCode::from(2)
}

fn open(path: &str) -> Result<(File, Grant), ReelError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let grant = Grant {
        content_id: ContentId::parse("cli-input")?,
        revision: 1,
        expected_len: len,
    };
    Ok((file, grant))
}

fn receipt_json(r: &SegmentReceipt, s: &Summary) -> String {
    format!(
        "{{\"codec\":\"{}\",\"decoder\":\"{}@{}\",\"disposition\":\"{:?}\",\"source\":\"{:?}\",\"proxy\":\"{:?}\",\"requested\":[{},{},{}],\"actual\":[{},{},{}],\"chunks\":{},\"bytes_read\":{},\"peak_chunk_bytes\":{},\"first_pts\":{:?},\"last_pts\":{:?},\"complete\":{},\"ticks_exact\":{}}}",
        r.codec,
        r.decoder_id,
        r.decoder_version,
        r.disposition,
        r.source,
        r.proxy_note,
        r.requested.start_pts.value(),
        r.requested.end_pts.value(),
        r.requested.units,
        r.actual.start_pts.value(),
        r.actual.end_pts.value(),
        r.actual.units,
        r.chunks,
        r.bytes_read,
        s.peak,
        s.first_pts,
        s.last_pts,
        r.complete,
        r.ticks_exact
    )
}

fn run(args: &[String]) -> Result<(), ReelError> {
    let registry = Registry::standard();
    let limits = Limits::default();
    let cancel = CancellationToken::default();
    if args.iter().any(|a| a == "--descriptor") {
        println!("{DESCRIPTOR}");
    } else if args.iter().any(|a| a == "--list-codecs") {
        println!("{}", registry.descriptors_json());
    } else if args.iter().any(|a| a == "--encode-roundtrip") {
        let spec = EncodeSpec {
            codec: Y4M_PROVIDER_ID.into(),
            params: EncodeParams::Video {
                width: 64,
                height: 36,
                ticks_per_frame: 10_160_640_000,
                chroma: "420",
            },
            total_units: 10,
        };
        let mut out = Vec::new();
        let done = registry.resolve_id(Y4M_PROVIDER_ID)?.encode_segment(
            &spec,
            &mut Frames { next: 0, total: 10 },
            &mut out,
            &limits,
            &cancel,
        )?;
        let same = out == y4m_bytes(64, 36, (25, 1), "420", 10);
        println!(
            "{{\"complete\":{},\"units\":{},\"bytes\":{},\"matches_independent_fixture\":{same}}}",
            done.complete, done.units_written, done.bytes_written
        );
    } else if let Some(path) = arg(args, "--probe") {
        let (mut file, _) = open(&path)?;
        let provider = registry.detect(&mut file)?;
        let l = provider.probe(&mut file, &limits, &cancel)?;
        println!(
            "{{\"provider\":\"{}\",\"codec\":\"{}\",\"kind\":\"{}\",\"width\":{},\"height\":{},\"ticks_per_unit\":{},\"units\":{},\"channels\":{},\"tracks\":{}}}",
            provider.descriptor().id,
            l.codec,
            l.kind.as_str(),
            l.width,
            l.height,
            l.ticks_per_unit,
            l.units,
            l.channels,
            l.tracks
        );
    } else if let Some(path) = arg(args, "--seek") {
        let ticks: u64 = arg(args, "--ticks")
            .and_then(|t| t.parse().ok())
            .ok_or(ReelError::Unsupported { what: "--ticks" })?;
        let (mut file, grant) = open(&path)?;
        let index = build_index(&mut file, grant.expected_len, &limits, &cancel)?;
        let track = index
            .default_track()
            .ok_or(ReelError::Unsupported { what: "no-track" })?;
        let r = track.seek(Ticks::new(ticks))?;
        println!(
            "{{\"requested\":{},\"target_sample\":{},\"target_pts\":{},\"decode_from\":{},\"samples_to_decode\":{},\"disposition\":\"{:?}\"}}",
            r.requested.value(),
            r.target.sample,
            r.target.pts,
            r.decode_from.sample,
            r.samples_to_decode,
            r.disposition
        );
    } else if let Some(path) = arg(args, "--decode") {
        let (mut file, grant) = open(&path)?;
        let provider = registry.detect(&mut file)?;
        let layout = provider.probe(&mut file, &limits, &cancel)?;
        let unit =
            |name: &str| -> u64 { arg(args, name).and_then(|v| v.parse().ok()).unwrap_or(0) };
        let count = unit("--units").max(1);
        let request = SegmentRequest {
            codec: provider.descriptor().id.into(),
            start_pts: Ticks::new(unit("--start-unit") * layout.ticks_per_unit),
            count,
            track: arg(args, "--track").and_then(|t| t.parse().ok()),
            output: if args.iter().any(|a| a == "--packets") {
                OutputMode::Packets
            } else {
                OutputMode::Decoded
            },
            prefer: SourcePreference::Original,
            purpose: Purpose::Preview,
            grant,
            expected_revision: 1,
        };
        let mut summary = Summary {
            chunks: 0,
            units: 0,
            bytes: 0,
            peak: 0,
            first_pts: None,
            last_pts: None,
        };
        let receipt =
            registry.decode_segment(&mut file, None, &request, &limits, &cancel, &mut summary)?;
        println!("{}", receipt_json(&receipt, &summary));
    } else {
        // Default self-check: generate a Y4M fixture in memory and decode a bounded segment.
        let bytes = y4m_bytes(64, 36, (25, 1), "420", 10);
        let grant = Grant {
            content_id: ContentId::parse("generated")?,
            revision: 1,
            expected_len: bytes.len() as u64,
        };
        let request = SegmentRequest {
            codec: Y4M_PROVIDER_ID.into(),
            start_pts: Ticks::new(3 * 10_160_640_000),
            count: 3,
            track: None,
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Preview,
            grant,
            expected_revision: 1,
        };
        let mut summary = Summary {
            chunks: 0,
            units: 0,
            bytes: 0,
            peak: 0,
            first_pts: None,
            last_pts: None,
        };
        let receipt = registry.decode_segment(
            &mut Cursor::new(bytes),
            None,
            &request,
            &limits,
            &cancel,
            &mut summary,
        )?;
        println!("{}", receipt_json(&receipt, &summary));
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}
