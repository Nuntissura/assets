//! Lane SMB-PULSE-PLAYBACK-2: decoded-frame cache, decoder port and prefetch (STU-ARC-008:
//! decoded frames and bytes admitted before allocation, typed overload, observable counters).
//! A fake decoder counts its calls so the tests prove what was (not) decoded.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use hsk_studio_pulse::*;
use std::cell::Cell;

const F: u64 = 10_160_640_000;

fn media(id: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: id.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "00".into(),
        },
    }
}

#[derive(Debug)]
struct Bytes(u64, u64);
impl FrameCost for Bytes {
    fn cost_bytes(&self) -> u64 {
        self.0
    }
}

/// "a": 10 frames of 16 bytes; "nocodec": codec_unavailable; "bomb": declares u64::MAX bytes;
/// "liar": declares 16, delivers 1000.
#[derive(Default)]
struct Fake {
    declared: Cell<u32>,
    decoded: Cell<u32>,
}
impl FrameDecoder for Fake {
    type Frame = Bytes;
    fn declared_bytes(&self, s: &ClipSource, _: &CancellationToken) -> std::result::Result<u64, DecodeRefusal> {
        self.declared.set(self.declared.get() + 1);
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else {
            return Err(DecodeRefusal::new("unsupported", "kind"));
        };
        match id.as_str() {
            "nocodec" => Err(DecodeRefusal::new("codec_unavailable", "h264")),
            "bomb" => Ok(u64::MAX),
            _ => Ok(16),
        }
    }
    fn decode_run(
        &self,
        s: &ClipSource,
        tick: Ticks,
        max: u32,
        _: &CancellationToken,
    ) -> std::result::Result<Vec<DecodedFrame<Bytes>>, DecodeRefusal> {
        self.decoded.set(self.decoded.get() + 1);
        let liar = matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "liar");
        let first = tick.value() / F;
        if first >= 10 {
            return Err(DecodeRefusal::new("out_of_range", "past the end"));
        }
        Ok((first..(first + u64::from(max)).min(10))
            .map(|f| DecodedFrame {
                span: TickRange::new(Ticks::new(f * F), Ticks::new((f + 1) * F)).unwrap(),
                frame: Bytes(if liar { 1000 } else { 16 }, f),
            })
            .collect())
    }
}

fn sequence(frames: u64) -> StudioSequence {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: media("a"),
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
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
            clips: vec![clip],
        }],
        markers: Vec::new(),
        work_area: None,
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(10 * F))
    }
}

#[test]
fn frame_pipeline_caches_prefetches_and_refuses_before_allocation() {
    let cancel = CancellationToken::default();
    let limits = FrameCacheLimits {
        max_bytes: 64,
        max_entries: 3,
        max_frame_bytes: 32,
    };
    let mut p = FramePipeline::new(Fake::default(), limits, 2).unwrap();

    // Miss decodes a run of 2 (frames 3, 4); the second frame is then a hit; mid-frame ticks hit.
    assert_eq!(p.frame(&media("a"), Ticks::new(3 * F + 5), &cancel).unwrap().1, 3);
    assert_eq!(p.frame(&media("a"), Ticks::new(4 * F + F - 1), &cancel).unwrap().1, 4);
    assert_eq!(p.decoder().decoded.get(), 1);
    let st = p.cache().stats();
    // (the first ask misses, decodes and then reads its own frame: one miss, two hits in all)
    assert_eq!((st.hits, st.misses, st.inserts, st.decodes), (2, 1, 2, 1));
    // Entry cap 3: decoding frames 7, 8 evicts the least recently used (frame 3).
    p.frame(&media("a"), Ticks::new(7 * F), &cancel).unwrap();
    assert_eq!((p.cache().len(), p.cache().stats().evictions), (3, 1));
    assert!(!p.cache().contains(&media("a"), Ticks::new(3 * F)));
    assert!(p.cache().contains(&media("a"), Ticks::new(4 * F)));
    assert!(p.cache().used_bytes() <= 64);

    // A missing codec is a typed refusal remembered per source: one decoder probe for 3 asks.
    for _ in 0..3 {
        assert_eq!(p.frame(&media("nocodec"), Ticks::new(0), &cancel).unwrap_err().code, "codec_unavailable");
    }
    assert_eq!(p.refusal(&media("nocodec")).unwrap().code, "codec_unavailable");
    // Cache bomb: refused from the declared size before any decode call.
    let decoded = p.decoder().decoded.get();
    assert_eq!(p.frame(&media("bomb"), Ticks::new(0), &cancel).unwrap_err().code, "limit_exceeded");
    assert_eq!(p.decoder().decoded.get(), decoded, "nothing decoded for the bomb");
    // A decoder whose frames exceed what it declared is refused, nothing cached.
    let before = p.cache().len();
    assert_eq!(p.frame(&media("liar"), Ticks::new(0), &cancel).unwrap_err().code, "limit_exceeded");
    assert_eq!(p.cache().len(), before);
    // Absurd seek: a typed refusal, not remembered (the source still plays).
    assert_eq!(p.frame(&media("a"), Ticks::new(u64::MAX), &cancel).unwrap_err().code, "out_of_range");
    assert!(p.refusal(&media("a")).is_none());
    // Hostile limits and run sizes.
    assert!(FrameCache::<Bytes>::new(FrameCacheLimits { max_bytes: 8, max_entries: 1, max_frame_bytes: 9 }).is_err());
    assert!(FramePipeline::new(Fake::default(), limits, 0).is_err());
    assert!(FramePipeline::new(Fake::default(), limits, 100_000).is_err());
    let mut c = FrameCache::new(limits).unwrap();
    assert_eq!(
        c.insert(&media("a"), TickRange::new(Ticks::new(0), Ticks::new(F)).unwrap(), std::sync::Arc::new(Bytes(33, 0))),
        Err(PulseError::LimitExceeded)
    );
    let long = media(&"x".repeat(MAX_SOURCE_KEY_BYTES + 1));
    assert!(source_key(&long).is_none());
    assert!(c.get(&long, Ticks::new(0)).is_none());

    // Prefetch follows the scheduler: forward from frame 0 names 1..4; loop wraps; reverse and 4x.
    let seq = sequence(10);
    let mut s = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let clock = HeadlessClock::new(48_000);
    assert_eq!(s.upcoming_frames(4).unwrap(), [1, 2, 3, 4]);
    s.scrub(Ticks::new(8 * F), &clock).unwrap();
    assert_eq!(s.upcoming_frames(4).unwrap(), [9], "stops at the range end");
    s.set_looping(true, &clock).unwrap();
    assert_eq!(s.upcoming_frames(4).unwrap(), [9, 0, 1, 2]);
    s.set_speed(PlaySpeed::new(-4, 1).unwrap(), &clock).unwrap();
    assert_eq!(s.upcoming_frames(3).unwrap(), [4, 0, 6]);
    assert_eq!(s.upcoming_frames(MAX_PREFETCH_FRAMES + 1).unwrap_err(), PulseError::LimitExceeded);

    let mut q = FramePipeline::new(Fake::default(), FrameCacheLimits::default(), 2).unwrap();
    s.set_speed(PlaySpeed::FORWARD, &clock).unwrap();
    s.set_looping(false, &clock).unwrap();
    s.scrub(Ticks::new(0), &clock).unwrap();
    let layers = prefetch_layers(&seq, &s, &s.upcoming_frames(6).unwrap(), &Ext, &cancel).unwrap();
    assert_eq!(layers.len(), 6);
    // Budget of 2 decoder calls of 2 frames: frames 1..4 cached, 5 and 6 not.
    assert_eq!(q.prefetch(&layers, 2, &cancel), 4);
    assert_eq!(q.decoder().decoded.get(), 2);
    assert!((1..=4).all(|f| q.cache().contains(&media("a"), Ticks::new(f * F))));
    assert!(!q.cache().contains(&media("a"), Ticks::new(5 * F)));
    assert_eq!(q.cache().stats().prefetched, 4);
    // Presenting frame 1 now costs no decode.
    q.frame(&media("a"), Ticks::new(F), &cancel).unwrap();
    assert_eq!(q.decoder().decoded.get(), 2);
}
