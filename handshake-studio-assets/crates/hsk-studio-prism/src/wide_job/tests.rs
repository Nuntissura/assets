//! Deterministic cancellation tests (R7 pre-review: the earlier wall-clock test could finish the band before its
//! cancel landed). Each test trips the token at an exact chunk edge through `chunk_trip` (cfg(test) only), so the
//! converted prefix is known to the chunk, with no sleeping and no thread.
//!
//! They prove what `WIDE_DESCRIPTOR.recovery` and the `convert_band` docs say a canceled conversion leaves behind:
//! a request returns no pixels; a band leaves exactly the chunks converted before the edge (a whole number of
//! 64-pixel chunks, all of them when the cancel lands after the last chunk, none at edge 0) and an in-place band
//! leaves the caller's input as converted prefix + untouched rest; a band stays claimed; input-check refusals
//! write nothing.
#![allow(clippy::arithmetic_side_effects)] // test code: literal sizes, no untrusted input
use super::*;
use crate::wide::chunk_trip;
use crate::{WideTransformRequest, profile_hash};
use hsk_studio_observe::{DeliveryClass, DeliveryError};

const SRGB: &[u8] = include_bytes!("../../tests/profiles/sRGB-v4.icc");
const P3: &[u8] = include_bytes!("../../tests/profiles/DisplayP3-v4.icc");
/// 20 whole chunks and a 17-pixel tail: 21 chunk edges before a chunk plus the edge after the last one.
const PIXELS: usize = 20 * CHUNK_PIXELS + 17;
const CHUNKS: u64 = 21;

struct Ids {
    actor: ActorContext,
    source: DomainId,
    destination: DomainId,
    resource: DomainId,
}
fn ids() -> Ids {
    Ids {
        actor: ActorContext::new("account", "principal", "owner-account", "owner-principal", "space", "session").unwrap(),
        source: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        destination: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000002").unwrap(),
        resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000003").unwrap(),
    }
}
struct Null;
impl SinkPort for Null {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}
/// Device values in 0..1 from a 64-bit LCG (fixed seed).
fn pixels(count: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..count.saturating_mul(3))
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32) / 16_777_216.0
        })
        .collect()
}
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}
fn engine(ids: &Ids) -> WideEngine {
    WideEngine::new(ids.actor.clone(), 4).unwrap()
}
fn prepare<'e>(engine: &'e mut WideEngine, ids: &Ids) -> WideJob<'e> {
    let request = WideJobRequest {
        source: WideSource::Profile(ProfileInput { profile_id: &ids.source, bytes: SRGB, expected_sha256: profile_hash(SRGB) }),
        destination: ProfileInput { profile_id: &ids.destination, bytes: P3, expected_sha256: profile_hash(P3) },
        bands: WideBands::Sizes(&[PIXELS]),
        intent: Intent::RelativeColorimetric,
        black_point_compensation: false,
        revision: 1,
        expected_revision: 1,
        correlation_id: 7,
        resource_id: &ids.resource,
        actor: &ids.actor,
    };
    engine.prepare_job(&request, &CancellationToken::default()).unwrap()
}
/// The uninterrupted conversion of `input` (the same band without a cancel).
fn reference(ids: &Ids, input: &[f32]) -> Vec<f32> {
    let mut eng = engine(ids);
    let job = prepare(&mut eng, ids);
    let mut out = vec![f32::NAN; input.len()];
    job.convert_band(0, input, &mut out, &CancellationToken::default()).unwrap();
    out
}
/// Pixels before the first NaN of an output buffer that started as all NaN.
fn converted_pixels(out: &[f32]) -> usize {
    out.iter().position(|v| v.is_nan()).map_or(out.len() / 3, |i| i / 3)
}

#[test]
fn a_band_canceled_at_a_chunk_edge_leaves_exactly_the_chunks_before_it() {
    let ids = ids();
    let input = pixels(PIXELS);
    let want = reference(&ids, &input);
    for edge in [0u64, 1, 5, 20] {
        let mut eng = engine(&ids);
        let job = prepare(&mut eng, &ids);
        let mut out = vec![f32::NAN; input.len()];
        let token = CancellationToken::default();
        let armed = chunk_trip::arm(edge);
        let result = job.convert_band(0, &input, &mut out, &token);
        drop(armed);
        assert_eq!(result.err(), Some(WideError::Canceled), "edge {edge}");
        let done = converted_pixels(&out);
        assert_eq!(done, usize::try_from(edge).unwrap() * CHUNK_PIXELS, "edge {edge}: converted prefix");
        assert!(out[done * 3..].iter().all(|v| v.is_nan()), "edge {edge}: a pixel after the cancel edge was written");
        assert_eq!(bits(&out[..done * 3]), bits(&want[..done * 3]), "edge {edge}: the prefix equals the uninterrupted run");
        // The band is spent: it cannot be converted again on this job, and finish reports the cancellation.
        let mut again = vec![0.0f32; input.len()];
        assert_eq!(job.convert_band(0, &input, &mut again, &CancellationToken::default()).err(), Some(WideError::BandClaimed));
        assert!(again.iter().all(|v| *v == 0.0), "edge {edge}: a refused second call wrote");
        let failure = job.finish(&CancellationToken::default(), &mut Null).expect_err("finish reports the canceled band");
        assert_eq!(failure.error, WideError::Canceled, "edge {edge}");
    }
}

#[test]
fn a_cancel_after_the_last_chunk_still_fails_with_every_chunk_converted() {
    let ids = ids();
    let input = pixels(PIXELS);
    let want = reference(&ids, &input);
    let mut eng = engine(&ids);
    let job = prepare(&mut eng, &ids);
    let mut out = vec![f32::NAN; input.len()];
    let token = CancellationToken::default();
    let armed = chunk_trip::arm(CHUNKS);
    let result = job.convert_band(0, &input, &mut out, &token);
    drop(armed);
    assert_eq!(result.err(), Some(WideError::Canceled));
    assert_eq!(bits(&out), bits(&want), "the whole band was converted although the call failed");
    let failure = job.finish(&CancellationToken::default(), &mut Null).expect_err("canceled band");
    assert_eq!(failure.error, WideError::Canceled);
}

#[test]
fn an_in_place_band_canceled_mid_band_is_partly_overwritten() {
    let ids = ids();
    let input = pixels(PIXELS);
    let want = reference(&ids, &input);
    for edge in [0u64, 3, 20, CHUNKS] {
        let mut eng = engine(&ids);
        let job = prepare(&mut eng, &ids);
        let mut buffer = input.clone();
        let token = CancellationToken::default();
        let armed = chunk_trip::arm(edge);
        let result = job.convert_band_in_place(0, &mut buffer, &token);
        drop(armed);
        assert_eq!(result.err(), Some(WideError::Canceled), "edge {edge}");
        let done = (usize::try_from(edge).unwrap() * CHUNK_PIXELS).min(PIXELS) * 3;
        assert_eq!(bits(&buffer[..done]), bits(&want[..done]), "edge {edge}: converted prefix");
        assert_eq!(bits(&buffer[done..]), bits(&input[done..]), "edge {edge}: the rest of the caller's input is untouched");
        if done > 0 {
            assert_ne!(bits(&buffer[..done]), bits(&input[..done]), "edge {edge}: the prefix really changed (converting it again would be wrong)");
        }
    }
}

#[test]
fn input_refusals_write_nothing_before_any_chunk() {
    let ids = ids();
    let mut input = pixels(PIXELS);
    // The bad value sits in the last chunk: the whole band is checked before the first chunk is converted.
    let last = input.len() - 1;
    input[last] = 1.5;
    let mut eng = engine(&ids);
    let job = prepare(&mut eng, &ids);
    let mut out = vec![f32::NAN; input.len()];
    assert_eq!(job.convert_band(0, &input, &mut out, &CancellationToken::default()).err(), Some(WideError::ChannelRange));
    assert!(out.iter().all(|v| v.is_nan()), "a refused band wrote output");
    let mut eng = engine(&ids);
    let job = prepare(&mut eng, &ids);
    let mut buffer = input.clone();
    assert_eq!(job.convert_band_in_place(0, &mut buffer, &CancellationToken::default()).err(), Some(WideError::ChannelRange));
    assert_eq!(bits(&buffer), bits(&input), "a refused in-place band changed the caller's input");
}

#[test]
fn a_request_canceled_mid_request_returns_no_pixels() {
    let ids = ids();
    let input = pixels(PIXELS);
    for edge in [0u64, 7, CHUNKS] {
        let mut eng = engine(&ids);
        let request = WideTransformRequest {
            source: WideSource::Profile(ProfileInput { profile_id: &ids.source, bytes: SRGB, expected_sha256: profile_hash(SRGB) }),
            destination: ProfileInput { profile_id: &ids.destination, bytes: P3, expected_sha256: profile_hash(P3) },
            pixels: &input,
            intent: Intent::RelativeColorimetric,
            black_point_compensation: false,
            revision: 1,
            expected_revision: 1,
            correlation_id: 7,
            resource_id: &ids.resource,
            actor: &ids.actor,
        };
        let token = CancellationToken::default();
        let armed = chunk_trip::arm(edge);
        let result = eng.transform(&request, &token, &mut Null);
        drop(armed);
        // `Err(WideFailure)` carries no pixel buffer: the output Vec is dropped with the failed call.
        let failure = result.err().unwrap_or_else(|| panic!("edge {edge}: the request completed"));
        assert_eq!(failure.error, WideError::Canceled, "edge {edge}");
        assert_eq!(failure.operation_error, Some(WideError::Canceled), "edge {edge}");
        assert!(failure.delivery.is_ok(), "edge {edge}: the canceled outcome is still delivered");
        assert_eq!(eng.cache_stats().rejected, 1, "edge {edge}");
    }
}

#[test]
fn the_job_chunk_account_reports_the_stage_a_band_really_holds() {
    // The record is derived from the stage type, so it cannot drift from the buffer `convert_band*` allocate.
    assert_eq!(std::mem::size_of::<Stage>(), 1024);
    assert_eq!(band_account().scratch_bytes, std::mem::size_of::<Stage>());
    let ids = ids();
    let input = pixels(PIXELS);
    let mut eng = engine(&ids);
    let job = prepare(&mut eng, &ids);
    let mut out = vec![0.0f32; input.len()];
    let report = job.convert_band(0, &input, &mut out, &CancellationToken::default()).unwrap();
    assert_eq!(report.chunks.scratch_bytes, std::mem::size_of::<Stage>());
    let done = job.finish(&CancellationToken::default(), &mut Null).unwrap();
    assert_eq!(done.receipt.receipt.chunks.scratch_bytes, std::mem::size_of::<Stage>());
    // A request over the same RGB pixels stages 64 x 3 channels x 4 bytes on the heap: a different, smaller buffer.
    assert_eq!(WideChunkAccount::new(3).scratch_bytes, 768);
}
