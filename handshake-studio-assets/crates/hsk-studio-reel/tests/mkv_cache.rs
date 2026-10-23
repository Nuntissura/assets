//! `IndexCache` over `MkvIndex`: keyed by content id + revision + length + limits, a hit still verifies the
//! grant against the live source, a new revision drops the older ones, ISOBMFF and Matroska indexes share
//! one capacity, a failed build caches nothing.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;
use std::sync::Arc;

const MKV_A: &[u8] = include_bytes!("fixtures/mkv/pcm-s16le-48k-2ch.mkv");
const MKV_B: &[u8] = include_bytes!("fixtures/mkv/pcm-u8-48k-1ch.mkv");
const MP4: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mp4");

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
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 60s deadline: {e}"),
    }
}

fn grant(id: &str, revision: u64, bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse(id).unwrap(),
        revision,
        expected_len: bytes.len() as u64,
    }
}

#[derive(Default)]
struct Collect(Vec<(i64, Vec<u8>)>);

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.0.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn matroska_index_cache_hits_keys_invalidates_and_shares_capacity() {
    bounded_case("matroska_index_cache", || {
        let cancel = CancellationToken::default();
        let limits = Limits::default();
        let other = Limits {
            max_tracks: 3,
            ..limits
        };
        let mut cache = IndexCache::new(3).unwrap();
        let mkv = |cache: &mut IndexCache, id: &str, rev: u64, bytes: &[u8], l: &Limits| {
            cache.get_or_build_matroska(
                &mut Cursor::new(bytes.to_vec()),
                &grant(id, rev, bytes),
                rev,
                l,
                &cancel,
            )
        };

        // Miss, then hit: the very same index.
        let first = mkv(&mut cache, "a", 1, MKV_A, &limits).unwrap();
        let again = mkv(&mut cache, "a", 1, MKV_A, &limits).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let s = cache.stats();
        assert_eq!((s.hits, s.misses, s.entries), (1, 1, 1));
        assert!(s.approx_bytes > 0 && s.approx_bytes == first.approx_bytes());

        // Delivery through the cache equals delivery through a fresh index, and reuses the entry.
        let req = SegmentRequest {
            codec: MATROSKA_PROVIDER_ID.into(),
            start_pts: Ticks::new(0),
            count: 5,
            track: Some(1),
            output: OutputMode::Packets,
            prefer: SourcePreference::Original,
            purpose: Purpose::Preview,
            grant: grant("a", 1, MKV_A),
            expected_revision: 1,
        };
        let (mut cached, mut fresh) = (Collect::default(), Collect::default());
        let cached_receipt = MatroskaProvider
            .decode_cached(
                &mut cache,
                &mut Cursor::new(MKV_A.to_vec()),
                &req,
                &limits,
                &cancel,
                &mut cached,
            )
            .unwrap();
        let fresh_receipt = MatroskaProvider
            .decode_packets(
                &mut Cursor::new(MKV_A.to_vec()),
                &first,
                &req,
                &limits,
                &cancel,
                &mut fresh,
            )
            .unwrap();
        assert_eq!(cached.0, fresh.0);
        assert_eq!(cached.0.len(), 5);
        assert_eq!(cached_receipt, fresh_receipt);
        assert_eq!((cache.stats().hits, cache.stats().misses), (2, 1));

        // The same revision under different limits is a different index.
        let limited = mkv(&mut cache, "a", 1, MKV_A, &other).unwrap();
        assert!(!Arc::ptr_eq(&first, &limited));
        assert_eq!(cache.stats().entries, 2);

        // The live source no longer matches the grant, or the caller's revision moved on: StaleGrant,
        // before any lookup result is served.
        let mut wrong_len = grant("a", 1, MKV_A);
        wrong_len.expected_len += 1;
        assert_eq!(
            cache
                .get_or_build_matroska(
                    &mut Cursor::new(MKV_A.to_vec()),
                    &wrong_len,
                    1,
                    &limits,
                    &cancel
                )
                .unwrap_err(),
            ReelError::StaleGrant
        );
        assert_eq!(
            cache
                .get_or_build_matroska(
                    &mut Cursor::new(MKV_A.to_vec()),
                    &grant("a", 2, MKV_A),
                    1,
                    &limits,
                    &cancel
                )
                .unwrap_err(),
            ReelError::StaleGrant
        );

        // A new revision of the id drops its older entries (both limits variants).
        let newer = mkv(&mut cache, "a", 2, MKV_B, &limits).unwrap();
        assert_eq!(newer.tracks[0].pcm.as_ref().unwrap().format.bits, 8);
        let s = cache.stats();
        assert_eq!((s.invalidated, s.entries), (2, 1));

        // ISOBMFF and Matroska indexes share the cache; an entry only answers its own kind.
        let mp4 = cache
            .get_or_build(
                &mut Cursor::new(MP4.to_vec()),
                &grant("m", 1, MP4),
                1,
                &limits,
                &cancel,
            )
            .unwrap();
        assert_eq!(mp4.tracks.len(), 1);
        assert_eq!(cache.stats().entries, 2);
        let before = cache.stats();
        // The mp4 bytes are not Matroska and the mkv bytes are not ISOBMFF: typed errors, nothing cached.
        assert!(matches!(
            cache.get_or_build_matroska(
                &mut Cursor::new(MP4.to_vec()),
                &grant("m", 1, MP4),
                1,
                &limits,
                &cancel
            ),
            Err(ReelError::ImportUnsupported { .. })
        ));
        assert!(
            cache
                .get_or_build(
                    &mut Cursor::new(MKV_A.to_vec()),
                    &grant("a", 2, MKV_A),
                    2,
                    &limits,
                    &cancel
                )
                .is_err()
        );
        let after = cache.stats();
        assert_eq!(after.entries, before.entries);
        assert_eq!(
            after.misses,
            before.misses + 2,
            "both failed builds are misses"
        );
        let mut corrupt = MKV_A.to_vec();
        corrupt[0] ^= 0xFF;
        assert!(mkv(&mut cache, "bad", 1, &corrupt, &limits).is_err());
        assert_eq!(
            cache.stats().entries,
            after.entries,
            "a failed build caches nothing"
        );

        // Capacity 3 is shared by both kinds: a fourth distinct source evicts the least recently used.
        mkv(&mut cache, "c", 1, MKV_A, &limits).unwrap();
        assert_eq!(cache.stats().entries, 3);
        mkv(&mut cache, "d", 1, MKV_B, &limits).unwrap();
        let s = cache.stats();
        assert_eq!((s.entries, s.evictions), (3, 1));

        // Cancellation is honoured before any lookup.
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            cache
                .get_or_build_matroska(
                    &mut Cursor::new(MKV_A.to_vec()),
                    &grant("c", 1, MKV_A),
                    1,
                    &limits,
                    &stop
                )
                .unwrap_err(),
            ReelError::Canceled
        );
    });
}
