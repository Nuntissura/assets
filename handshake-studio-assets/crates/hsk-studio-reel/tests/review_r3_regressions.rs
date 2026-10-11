//! One hostile-input regression per confirmed STUDIO-R3 reel finding (SMB-REEL, commit 404ff3a):
//! typed errors, never an arithmetic-overflow panic.
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
use std::io::Cursor;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}

fn index_of(spec: &MkvSpec) -> Result<MkvIndex, ReelError> {
    let bytes = mkv_bytes(spec).bytes;
    build_matroska_index(
        &mut Cursor::new(bytes.clone()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

fn block(track: u8, rel_ts: i16, key: bool, frames: &[u32], lacing: Lacing) -> MkvBlock {
    MkvBlock {
        track,
        rel_ts,
        key,
        frames: frames.to_vec(),
        lacing,
        group: None,
        raw_lace_header: None,
    }
}

fn base(clusters: Vec<MkvCluster>) -> MkvSpec {
    MkvSpec {
        doc_type: "matroska",
        timecode_scale: 1_000_000,
        unknown_sizes: false,
        with_cues: false,
        video_default_duration_ns: None,
        content_encoding_on_video: false,
        clusters,
    }
}

/// R3 finding 1 (matroska.rs:886): EBML lace sizes that each pass the per-delta check but whose sum
/// exceeds u64 must be `Corrupt`, not an overflow panic (debug) or a wrapped sum (release).
#[test]
fn r3_1_ebml_lace_size_sum_cannot_overflow() {
    bounded_case("r3_1_ebml_lace_size_sum_cannot_overflow", || {
        // 8-byte vints: first size 2^56-2, then 30 deltas of +(2^55-1) (raw 2^56-2, bias 2^55-1).
        let vint8 = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE];
        let mut raw = Vec::new();
        for _ in 0..31 {
            raw.extend_from_slice(&vint8);
        }
        let mut hostile = block(2, 0, true, &[1; 32], Lacing::Ebml);
        hostile.raw_lace_header = Some(raw);
        let spec = base(vec![MkvCluster {
            timecode: 0,
            blocks: vec![block(1, 0, true, &[8], Lacing::None), hostile],
        }]);
        assert_eq!(
            index_of(&spec).unwrap_err(),
            ReelError::Corrupt {
                what: "lace-sizes-exceed-block"
            }
        );
    });
}

/// R3 finding 2 (matroska.rs:1045,1049): a frame at pts -32768 followed by one at i64::MAX made
/// `finish_durations` subtract unchecked. The index must build or fail with a typed error, never panic.
#[test]
fn r3_2_duration_fill_cannot_overflow_on_extreme_timestamps() {
    bounded_case(
        "r3_2_duration_fill_cannot_overflow_on_extreme_timestamps",
        || {
            let spec = base(vec![
                MkvCluster {
                    timecode: 0,
                    blocks: vec![block(1, i16::MIN, true, &[8], Lacing::None)],
                },
                MkvCluster {
                    timecode: i64::MAX as u64,
                    blocks: vec![block(1, 0, true, &[8], Lacing::None)],
                },
            ]);
            match index_of(&spec) {
                Ok(index) => assert_eq!(index.tracks[0].index.samples().len(), 2),
                Err(error) => assert!(error.code().starts_with("MEDIA_"), "{error}"),
            }
        },
    );
}
