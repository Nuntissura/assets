//! SMB-REEL-RECEIPTS (PLR-066): the receipt of a delivery states what the provider's own loop delivered,
//! not what the request asked for.
//!
//! On every reachable input the loops of the wav, y4m, PCM and mxf providers deliver exactly the requested
//! units (a shortfall returns `Truncated` or another error first), so a receipt computed from the request
//! and one computed from the loop agree and no test over a real file can tell them apart: reverting a call
//! site to `complete: true` or to a count taken from the request stayed green (R11 review, REEL-R11-1).
//! These tests close that gap with the `cfg(test)` seam `model::short_loop`: the loop stops one unit short
//! of its bound without any error on the way, the way a defective loop would, and the receipt must say so.
//! A receipt that still states the requested count, `complete == true`, `Exact` or the requested end fails
//! here. One test per delivery site (the packet site is pinned by
//! `tests/engine_video.rs::repeated_media_sample_is_a_shortfall_not_a_silent_success`, where a repeating
//! edit list makes the shortfall reachable without a seam).
//!
//! Per site, with the loop stopped one unit short of a request for `count` units from `start`:
//! * a Preview is `Ok`, `Partial`, `complete == false`, `actual.units == count - 1`, `actual.end_pts` is
//!   the end of the `count - 1` units delivered, `requested` still names `count` units and their end, and
//!   `outcome_for` reports a loss;
//! * an Export is `Incomplete { requested: count, delivered: count - 1 }` (units that reached the sink
//!   stay delivered; the error says the export must not publish them);
//! * with the seam off, both purposes deliver everything, `Exact`, with the requested end as the actual end.
//!
//! The synthetic fixtures are the integration tests' own builders (`tests/common`), included here so
//! no second copy of them exists.
#[path = "../tests/common/mod.rs"]
mod common;

use crate::model::short_loop;
use crate::{
    Chunk, ChunkSink, ContentId, Disposition, Grant, ISOBMFF_PROVIDER_ID, IsobmffProvider, Limits,
    MXF_PROVIDER_ID, MediaProvider, MxfProvider, OutputMode, Purpose, ReelError, SegmentReceipt,
    SegmentRequest, SourcePreference, WAV_PROVIDER_ID, WavProvider, Y4M_PROVIDER_ID, Y4mProvider,
    outcome_for,
};
use common::{PcmKind, PcmMovSpec, mov_pcm_bytes, wav_pcm16, y4m_bytes};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_observe::{FailureCode, Outcome};
use std::io::Cursor;

/// Ticks per 1/48000 s sample, per 1/30 s frame and per 1/25 s frame.
const TPS_48K: u64 = 5_292_000;
const TPF_30: u64 = 8_467_200_000;
const TPF_25: u64 = 10_160_640_000;

/// What reached the sink: requested units (not pre-roll) and chunks.
#[derive(Default)]
struct Sunk {
    units: u64,
    chunks: u64,
}

impl ChunkSink for Sunk {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks += 1;
        if !c.preroll {
            self.units += c.units;
        }
        Ok(())
    }
}

/// One delivery site: a source, a request of `count` units from tick `start` (`unit` ticks each), and the
/// limits that make the loop deliver in several slices where the provider slices.
struct Site {
    name: &'static str,
    provider: &'static dyn MediaProvider,
    codec: &'static str,
    bytes: Vec<u8>,
    track: Option<u32>,
    output: OutputMode,
    limits: Limits,
    start: u64,
    count: u64,
    unit: u64,
}

fn run(site: &Site, purpose: Purpose, short_by: u64) -> (Result<SegmentReceipt, ReelError>, Sunk) {
    let request = SegmentRequest {
        codec: site.codec.into(),
        start_pts: Ticks::new(site.start),
        count: site.count,
        track: site.track,
        output: site.output,
        prefer: SourcePreference::Original,
        purpose,
        grant: Grant {
            content_id: ContentId::parse("delivery-tests").unwrap(),
            revision: 1,
            expected_len: site.bytes.len() as u64,
        },
        expected_revision: 1,
    };
    let mut sunk = Sunk::default();
    let guard = short_loop::engage(short_by);
    let result = site.provider.decode_segment(
        &mut Cursor::new(site.bytes.clone()),
        &request,
        &site.limits,
        &CancellationToken::default(),
        &mut sunk,
    );
    drop(guard);
    (result, sunk)
}

fn ticks(site: &Site, units: u64) -> Ticks {
    Ticks::new(site.start + units * site.unit)
}

/// The assertions every site shares (see the module doc).
fn receipt_states_what_the_loop_delivered(site: &Site) {
    let name = site.name;
    let count = site.count;

    // Seam off: the loop delivers everything, the receipt is the request.
    for purpose in [Purpose::Preview, Purpose::Export] {
        let (result, sunk) = run(site, purpose, 0);
        let receipt = result.unwrap_or_else(|e| panic!("{name} {purpose:?}: {e}"));
        assert!(receipt.complete, "{name} {purpose:?}");
        assert_eq!(
            receipt.disposition,
            Disposition::Exact,
            "{name} {purpose:?}"
        );
        assert_eq!(receipt.actual.units, count, "{name} {purpose:?}");
        assert_eq!(
            receipt.actual.end_pts,
            ticks(site, count),
            "{name} {purpose:?}"
        );
        assert_eq!(
            receipt.requested.end_pts,
            ticks(site, count),
            "{name} {purpose:?}"
        );
        assert_eq!(
            sunk.units, count,
            "{name} {purpose:?}: units that reached the sink"
        );
        assert!(
            sunk.chunks > 1,
            "{name}: the loop must deliver more than one chunk"
        );
    }

    // The loop stops one unit short without any error on the way: a Preview says so.
    let (result, sunk) = run(site, Purpose::Preview, 1);
    let receipt = result.unwrap_or_else(|e| panic!("{name} preview: {e}"));
    assert_eq!(
        sunk.units,
        count - 1,
        "{name}: the loop delivered one unit less"
    );
    assert_eq!(
        receipt.requested.units, count,
        "{name}: the request still names its units"
    );
    assert_eq!(
        receipt.requested.end_pts,
        ticks(site, count),
        "{name}: the requested end is the request's"
    );
    assert_eq!(
        receipt.actual.units,
        count - 1,
        "{name}: actual.units is the loop's count, not the request's"
    );
    assert!(
        !receipt.complete,
        "{name}: complete is read from the delivered count"
    );
    assert_eq!(receipt.disposition, Disposition::Partial, "{name}");
    assert_eq!(
        receipt.actual.end_pts,
        ticks(site, count - 1),
        "{name}: actual.end_pts is the end of what was delivered, not of what was requested"
    );
    assert_eq!(receipt.actual.start_pts, ticks(site, 0), "{name}");
    assert_eq!(
        outcome_for(&Ok(receipt)),
        Outcome::Failure(FailureCode::Loss),
        "{name}: an incomplete receipt is a loss"
    );

    // An Export is exact or it fails: the shortfall is the typed error with the delivered count.
    let (result, sunk) = run(site, Purpose::Export, 1);
    assert_eq!(
        result.unwrap_err(),
        ReelError::Incomplete {
            requested: count,
            delivered: count - 1,
        },
        "{name}: export"
    );
    assert_eq!(sunk.units, count - 1, "{name}: export");
}

/// wav (`wav.rs`): the delivered count was `deliver_end - start`, computed from the request and the file's
/// length (REEL-R11-1); it is the loop cursor's now.
#[test]
fn wav_receipt_states_what_the_loop_delivered() {
    receipt_states_what_the_loop_delivered(&Site {
        name: "wav samples",
        provider: &WavProvider,
        codec: WAV_PROVIDER_ID,
        bytes: wav_pcm16(48000, 1, 480),
        track: None,
        output: OutputMode::Decoded,
        // 32 samples per slice: 100 samples are four slices (three whole, one of 4).
        limits: Limits {
            max_chunk_bytes: 64,
            ..Limits::default()
        },
        start: 100 * TPS_48K,
        count: 100,
        unit: TPS_48K,
    });
}

/// y4m (`y4m.rs`): the delivered count is the loop's frame counter; the end of `actual` was the requested
/// end.
#[test]
fn y4m_receipt_states_what_the_loop_delivered() {
    receipt_states_what_the_loop_delivered(&Site {
        name: "y4m frames",
        provider: &Y4mProvider,
        codec: Y4M_PROVIDER_ID,
        bytes: y4m_bytes(8, 8, (30, 1), "420", 6),
        track: None,
        output: OutputMode::Decoded,
        // One chunk per frame.
        limits: Limits::default(),
        start: TPF_30,
        count: 4,
        unit: TPF_30,
    });
}

/// PCM (`isobmff/pcm.rs`, shared by ISOBMFF and Matroska PCM): the delivered count is the loop cursor's;
/// the end of `actual` was the requested end.
#[test]
fn pcm_receipt_states_what_the_loop_delivered() {
    let bytes = mov_pcm_bytes(&PcmMovSpec {
        fourcc: *b"twos",
        kind: PcmKind::S16,
        big_endian: true,
        lpcm_v2: false,
        non_interleaved: false,
        channels: 2,
        rate: 48000,
        frames: 300,
        frames_per_chunk: 128,
        movie_timescale: 48000,
        edits: None,
        moov_first: true,
        sound_version: None,
    });
    receipt_states_what_the_loop_delivered(&Site {
        name: "isobmff pcm",
        provider: &IsobmffProvider,
        codec: ISOBMFF_PROVIDER_ID,
        bytes,
        track: None,
        output: OutputMode::Decoded,
        // 16 frames per slice over chunk runs of 128: frames 100..160 cross a run boundary.
        limits: Limits {
            max_chunk_bytes: 64,
            ..Limits::default()
        },
        start: 100 * TPS_48K,
        count: 60,
        unit: TPS_48K,
    });
}

/// mxf (`mxf/provider.rs`), both loops: the clip-wrapped constant-size path (OP-Atom, slices of whole edit
/// units) and the frame-wrapped coded path (OP1a, a reordered picture track delivered from its random
/// access point). The delivered count is the loop's `delivered`; the end of `actual` was the requested end.
#[test]
fn mxf_receipt_states_what_the_loop_delivered() {
    // Clip wrapped: 1920 samples of 2 bytes, slices of 500 edit units under a 1000 byte chunk limit.
    receipt_states_what_the_loop_delivered(&Site {
        name: "mxf clip wrapped",
        provider: &MxfProvider,
        codec: MXF_PROVIDER_ID,
        bytes: include_bytes!("../tests/fixtures/mxf/pcm-opatom.mxf").to_vec(),
        track: Some(2),
        output: OutputMode::Packets,
        limits: Limits {
            max_chunk_bytes: 1000,
            ..Limits::default()
        },
        start: 0,
        count: 1920,
        unit: TPS_48K,
    });
    // Frame wrapped, reordered picture track: display positions 0..3 are coded 0, 2 and 3, delivered with
    // coded 1 (display 3) as pre-roll, so the chunks outnumber the requested units.
    receipt_states_what_the_loop_delivered(&Site {
        name: "mxf frame wrapped",
        provider: &MxfProvider,
        codec: MXF_PROVIDER_ID,
        bytes: include_bytes!("../tests/fixtures/mxf/mpeg2-pcm-op1a.mxf").to_vec(),
        track: Some(2),
        output: OutputMode::Packets,
        limits: Limits::default(),
        start: 0,
        count: 3,
        unit: TPF_25,
    });
}
