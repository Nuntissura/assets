//! Streaming save contract: bounded memory, byte identity with the buffered writer, and
//! all-or-nothing publication when a stream lies. Own test binary so the counting allocator
//! below does not touch the foundation contract tests.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_package::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

// ----- per-thread allocation accounting (memory bound assertions) -----------------------------
// Each test body runs on its own thread, so live/peak bytes are attributable to that body alone.

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn track(delta: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

struct Tracking;
// SAFETY: every method forwards to the system allocator unchanged; the counters only observe.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        track(-(layout.size() as isize));
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

/// Peak bytes allocated by the current thread (above the live level at `probe()`).
struct Probe {
    base: isize,
}
fn probe() -> Probe {
    let base = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(base));
    Probe { base }
}
impl Probe {
    fn peak(&self) -> usize {
        (PEAK.with(Cell::get) - self.base).max(0) as usize
    }
}

fn bounded_case(name: &'static str, seconds: u64, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(seconds)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared {seconds}-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

// ----- fixtures ---------------------------------------------------------------------------

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn address(n: u8, op: &str, port: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":op,"port_key":port})
}
fn source(n: u8, ty: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":ty,"cardinality":"one","semantic":"source"}]})
}
fn composite(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
fn layer(n: u8, kind: &str, payload: Value) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true})
}
fn edge(source: Value, target: Value, n: u64) -> Value {
    json!({"source":source,"target":target,"dependency_kind":"data","input_ordinal":n})
}
fn tile(n: u8, digest: &str, format: &str) -> Value {
    json!({"object_key":format!("tile-{n}"),"layer_id":identity("SLYR",2),"column":n,"row":0,"width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":format,"colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"sha256","digest":digest},"artifact_manifest_id":"caller-owned-existing-handle"})
}

fn mixed(tiles: Vec<Value>) -> Value {
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":identity("SART",1),"name":"board","parent":null,"bounds":{"x":{"value":-1,"unit":"mm"},"y":{"value":0,"unit":"mm"},"width":{"value":100,"unit":"mm"},"height":{"value":50,"unit":"mm"}}}],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":tiles})),layer(3,"raster",json!({"payload_kind":"raster","tiles":[]})),layer(4,"vector",json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),layer(5,"text",json!({"payload_kind":"text","story_id":identity("STXT",1)}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2,"image"),source(3,"image"),source(4,"vector"),source(5,"text"),composite(1)],"edges":[edge(address(3,"source","out"),address(1,"mix","inputs"),1),edge(address(2,"source","out"),address(1,"mix","inputs"),0)],"outputs":[address(1,"mix","out"),address(4,"source","out"),address(5,"source","out")]}
    })
}

struct Granted;
impl folio::Resolver for Granted {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, f: &str) -> bool {
        matches!(f, "caller-selected-format" | "rgb_f32le" | "coverage_u16le")
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn open_doc(v: &Value) -> folio::Snapshot {
    match folio::inspect_bytes(
        &serde_json::to_vec(v).unwrap(),
        folio::Budget::default(),
        &token(),
        &Granted,
    )
    .unwrap()
    {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("known typed document became read-only"),
    }
}

/// Never create test files on C: (operator rule); the studio env script points TMP at D:.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir();
        assert!(
            !cfg!(windows)
                || !base
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("C:"),
            "TMP is on C:; source studio-env.sh so test files land under the artifact root on D:"
        );
        let dir = base.join(format!("hsk-studio-stream-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn listing(&self) -> BTreeSet<String> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn lcg_bytes(n: usize, mut seed: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        })
        .collect()
}

/// Document over the given `(bytes, format)` blobs, one tile each.
fn fixture(blobs: &[(Vec<u8>, &str)]) -> (folio::Snapshot, MemoryAssetSource) {
    let mut assets = MemoryAssetSource::new();
    let tiles = blobs
        .iter()
        .enumerate()
        .map(|(i, (bytes, format))| tile(i as u8, &assets.insert(bytes.clone()).digest, format))
        .collect();
    (open_doc(&mixed(tiles)), assets)
}

fn save_inputs<'a>(
    snapshot: &'a folio::Snapshot,
    assets: &'a dyn AssetStream,
    options: WriteOptions,
) -> StreamSave<'a> {
    StreamSave {
        doc: snapshot,
        assets,
        extras: Extras::default(),
        options,
    }
}

// ----- generated asset: bytes exist only as a stream ---------------------------------------

/// Deterministic bytes addressed by position, so any read chunking yields the same stream.
struct Generated {
    len: u64,
    seed: u64,
    pos: u64,
    cached: (u64, u64),
}
impl Generated {
    fn new(len: u64, seed: u64) -> Self {
        Self {
            len,
            seed,
            pos: 0,
            cached: (u64::MAX, 0),
        }
    }
    fn word(&self, index: u64) -> u64 {
        let mut z = self
            .seed
            .wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
impl Read for Generated {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min((self.len - self.pos) as usize);
        for (i, byte) in buf[..n].iter_mut().enumerate() {
            let at = self.pos + i as u64;
            if self.cached.0 != at / 8 {
                self.cached = (at / 8, self.word(at / 8));
            }
            *byte = (self.cached.1 >> ((at % 8) * 8)) as u8;
        }
        self.pos += n as u64;
        Ok(n)
    }
}

struct GeneratedAsset {
    digest: String,
    len: u64,
    seed: u64,
}
impl GeneratedAsset {
    fn new(len: u64, seed: u64) -> Self {
        let mut hasher = Sha256::new();
        let mut reader = Generated::new(len, seed);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            hasher.update(&chunk[..n]);
        }
        let digest = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Self { digest, len, seed }
    }
}
impl AssetStream for GeneratedAsset {
    fn open(
        &self,
        digest: &folio::ContentDigest,
        _max: u64,
    ) -> Result<AssetReader<'_>, AssetError> {
        if digest.digest != self.digest {
            return Err(AssetError::Absent);
        }
        Ok(AssetReader {
            len: self.len,
            reader: Box::new(Generated::new(self.len, self.seed)),
        })
    }
}

// ----- 1. byte identity with the buffered writer, strict reader acceptance --------------------

#[test]
fn streaming_output_is_byte_identical_and_strictly_readable() {
    bounded_case("streaming_output_is_byte_identical", 60, || {
        let gradient: Vec<u8> = (0..65_536u32)
            .flat_map(|i| (i as f32 * 0.001).to_le_bytes())
            .collect();
        let mut ramp: Vec<u8> = (0..10_000u32)
            .flat_map(|i| ((i * 3) as u16).to_le_bytes())
            .collect();
        ramp.push(0x5a);
        let rows: Vec<u8> = (0..4096u32).map(|i| (i % 64) as u8).collect();
        let blobs = [
            (gradient, "rgb_f32le"),
            (ramp, "coverage_u16le"),
            (lcg_bytes(4096, 11), "rgb_f32le"),
            (rows, "caller-selected-format"),
            (Vec::new(), "caller-selected-format"),
            (vec![7u8; 10], "caller-selected-format"),
        ];
        let (snapshot, assets) = fixture(&blobs);
        let held = [Quarantined {
            name: "opaque/x.bin".to_owned(),
            bytes: lcg_bytes(300, 5),
            sha256_hex: String::new(),
        }];
        let previews = [PreviewImage {
            info: PreviewInfo {
                format: PixelFormat::Rgba8,
                width: 32,
                height: 16,
                color: "srgb".to_owned(),
            },
            pixels: (0..32 * 16 * 4).map(|i| (i / 8) as u8).collect(),
        }];
        let extras = Extras {
            quarantined: &held,
            previews: &previews,
        };
        let limits = Limits::default();
        for codec in [Codec::Raw, Codec::Zstd, Codec::ShuffleZstd] {
            for zip64 in [Zip64Policy::Auto, Zip64Policy::Always] {
                let options = WriteOptions {
                    zip64,
                    asset_codec: codec,
                };
                let mut buffered = Vec::new();
                let a = write_package_with(
                    &snapshot,
                    &assets,
                    &extras,
                    &options,
                    &limits,
                    &token(),
                    &mut buffered,
                )
                .unwrap();
                let mut streamed = Cursor::new(Vec::new());
                let b = write_package_streaming(
                    &snapshot,
                    &assets,
                    &extras,
                    &options,
                    &limits,
                    &token(),
                    &mut streamed,
                )
                .unwrap();
                let streamed = streamed.into_inner();
                assert_eq!(a, b, "{codec:?}/{zip64:?}: receipts differ");
                assert!(
                    buffered == streamed,
                    "{codec:?}/{zip64:?}: streamed bytes differ from the buffered writer"
                );
                // The strict canonical-form reader accepts the streamed bytes and returns the inputs.
                let opened = read_package(
                    &streamed,
                    &limits,
                    folio::Budget::default(),
                    &Granted,
                    &token(),
                )
                .unwrap();
                assert_eq!(opened.assets.len(), blobs.len());
                for (bytes, _) in &blobs {
                    assert_eq!(opened.assets[&sha256_hex_of(bytes)], *bytes);
                }
                assert_eq!(opened.quarantined.len(), 1);
                assert_eq!(opened.previews.len(), 1);
                // `Opened` never reads preview bodies; the preview still verifies on request.
                let preview = &opened.previews[0];
                assert_eq!(
                    read_preview(&streamed, &limits, &preview.name, &token())
                        .unwrap()
                        .pixels,
                    previews[0].pixels
                );
            }
        }
        // To disk through the atomic streaming save: same bytes again, no leftovers.
        let dir = TempDir::new("identity");
        let target = dir.0.join("doc.handshake");
        let options = WriteOptions {
            asset_codec: Codec::ShuffleZstd,
            ..WriteOptions::default()
        };
        let save = StreamSave {
            doc: &snapshot,
            assets: &assets,
            extras,
            options,
        };
        let receipt = save_atomic_streaming(&target, &save, &limits, &token()).unwrap();
        let mut expected = Vec::new();
        write_package_with(
            &snapshot,
            &assets,
            &extras,
            &options,
            &limits,
            &token(),
            &mut expected,
        )
        .unwrap();
        assert!(fs::read(&target).unwrap() == expected);
        assert_eq!(receipt.bytes_written, expected.len() as u64);
        assert_eq!(dir.listing(), BTreeSet::from(["doc.handshake".to_owned()]));
        let reread = read_package_stream(
            File::open(&target).unwrap(),
            &limits,
            folio::Budget::default(),
            &Granted,
            &token(),
        )
        .unwrap();
        assert_eq!(reread.manifest.revision, 7);
    });
}

// ----- 2. bounded peak heap for a large streamed asset ----------------------------------------

/// Asset size and the stated heap bound: the save may allocate at most 1/8 of the asset at once
/// (the buffered path peaks at 4.25x the asset: ~272 MiB here). Observed: one 1 MiB copy chunk,
/// one 1 MiB verification chunk and the 256 KiB write buffer, ~2.5 MiB.
const BIG_ASSET: u64 = 64 << 20;
const PEAK_BOUND: usize = 8 << 20;

#[test]
fn streaming_save_peak_heap_is_bounded_for_large_asset() {
    bounded_case("streaming_save_peak_heap_is_bounded", 300, || {
        let dir = TempDir::new("peak");
        let target = dir.0.join("big.handshake");
        let asset = GeneratedAsset::new(BIG_ASSET, 0x5eed);
        let snapshot = open_doc(&mixed(vec![tile(
            0,
            &asset.digest,
            "caller-selected-format",
        )]));
        let save = save_inputs(&snapshot, &asset, WriteOptions::default());
        let limits = Limits::default();

        let meter = probe();
        let receipt = save_atomic_streaming(&target, &save, &limits, &token()).unwrap();
        let peak = meter.peak();
        eprintln!(
            "MEASURED streaming save of a {} MiB asset: peak heap {} KiB (bound {} KiB)",
            BIG_ASSET >> 20,
            peak >> 10,
            PEAK_BOUND >> 10
        );
        assert!(
            peak < PEAK_BOUND,
            "peak heap {peak} B exceeds the {PEAK_BOUND} B bound for a {BIG_ASSET} B asset"
        );

        // The file is the real thing: right size, strictly readable, the asset entry intact.
        let on_disk = fs::metadata(&target).unwrap().len();
        assert_eq!(on_disk, receipt.bytes_written);
        assert!(on_disk > BIG_ASSET && on_disk < BIG_ASSET + (1 << 20));
        let reader = PackageReader::open(File::open(&target).unwrap(), &limits, &token()).unwrap();
        let entry = reader
            .manifest()
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::Asset)
            .unwrap();
        assert_eq!(entry.sha256_hex, asset.digest);
        assert_eq!((entry.len, entry.codec), (BIG_ASSET, Codec::Raw));
        assert_eq!(dir.listing(), BTreeSet::from(["big.handshake".to_owned()]));
    });
}

// ----- 3. a lying stream never publishes ------------------------------------------------------

enum Fault {
    FlipByte,
    Short,
    Long,
}

/// Declares the honest length but delivers different bytes.
struct Tampered<'a> {
    inner: &'a MemoryAssetSource,
    fault: Fault,
}
impl AssetStream for Tampered<'_> {
    fn open(&self, digest: &folio::ContentDigest, max: u64) -> Result<AssetReader<'_>, AssetError> {
        let honest = self.inner.open(digest, max)?;
        let len = honest.len;
        let mut bytes = Vec::new();
        honest.reader.take(len).read_to_end(&mut bytes).unwrap();
        match self.fault {
            Fault::FlipByte => {
                let middle = bytes.len() / 2;
                bytes[middle] ^= 1;
            }
            Fault::Short => {
                bytes.pop();
            }
            Fault::Long => bytes.push(0),
        }
        Ok(AssetReader {
            len,
            reader: Box::new(Cursor::new(bytes)),
        })
    }
}

#[test]
fn lying_stream_aborts_typed_and_keeps_last_good() {
    bounded_case("lying_stream_aborts_typed", 60, || {
        let blobs = [
            (lcg_bytes(70_000, 3), "rgb_f32le"),
            (lcg_bytes(2048, 4), "caller-selected-format"),
        ];
        let (snapshot, assets) = fixture(&blobs);
        let limits = Limits::default();
        let dir = TempDir::new("lies");
        let target = dir.0.join("doc.handshake");
        let only_target = BTreeSet::from(["doc.handshake".to_owned()]);

        let good = save_inputs(&snapshot, &assets, WriteOptions::default());
        save_atomic_streaming(&target, &good, &limits, &token()).unwrap();
        let last_good = fs::read(&target).unwrap();

        let intact = |label: &str| {
            assert!(
                fs::read(&target).unwrap() == last_good,
                "{label}: destination changed"
            );
            assert_eq!(
                dir.listing(),
                only_target,
                "{label}: temp or marker left behind"
            );
        };

        for codec in [Codec::Raw, Codec::ShuffleZstd] {
            let options = WriteOptions {
                asset_codec: codec,
                ..WriteOptions::default()
            };
            for (fault, expected) in [
                (Fault::FlipByte, "hash_mismatch"),
                (Fault::Short, "length_mismatch"),
                (Fault::Long, "length_mismatch"),
            ] {
                let lying = Tampered {
                    inner: &assets,
                    fault,
                };
                let save = save_inputs(&snapshot, &lying, options);
                let error = save_atomic_streaming(&target, &save, &limits, &token()).unwrap_err();
                assert_eq!(error.code(), expected, "{codec:?}");
                intact(&format!("{codec:?} {expected}"));
            }
        }

        // Missing asset, pre-cancelled token, and a directory target: typed, nothing written.
        let empty = MemoryAssetSource::new();
        let missing = save_inputs(&snapshot, &empty, WriteOptions::default());
        assert_eq!(
            save_atomic_streaming(&target, &missing, &limits, &token())
                .unwrap_err()
                .code(),
            "asset_absent"
        );
        intact("absent asset");
        let canceled = token();
        canceled.cancel();
        assert_eq!(
            save_atomic_streaming(&target, &good, &limits, &canceled).unwrap_err(),
            PackageError::Canceled
        );
        intact("pre-cancelled");
        fs::create_dir(dir.0.join("sub")).unwrap();
        assert_eq!(
            save_atomic_streaming(&dir.0.join("sub"), &good, &limits, &token()).unwrap_err(),
            PackageError::DestinationInvalid
        );
        fs::remove_dir(dir.0.join("sub")).unwrap();

        // The raw writer insists on a sink positioned at its start.
        let mut sink = Cursor::new(vec![0u8; 8]);
        sink.seek(SeekFrom::Start(3)).unwrap();
        assert_eq!(
            write_package_streaming(
                &snapshot,
                &assets,
                &Extras::default(),
                &WriteOptions::default(),
                &limits,
                &token(),
                &mut sink,
            )
            .unwrap_err()
            .code(),
            "io"
        );

        // The honest save still lands and replaces the last-good file.
        save_atomic_streaming(&target, &good, &limits, &token()).unwrap();
        intact("honest resave");
    });
}

// ----- 4. codec blobs: one blob at a time, independent of the blob count ----------------------

/// Output sink that keeps nothing (so its size is not counted as heap) but honours seeks.
#[derive(Default)]
struct Discard {
    pos: u64,
    end: u64,
}
impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pos += buf.len() as u64;
        self.end = self.end.max(self.pos);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for Discard {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
            SeekFrom::End(d) => self.end.saturating_add_signed(d),
        };
        Ok(self.pos)
    }
}

const BLOB: usize = 1 << 20;
/// Allowed growth of the peak when the blob count quadruples (per-entry metadata only).
const COUNT_SLACK: usize = 256 << 10;

/// Even blobs are smooth f32 data (shuffle+zstd wins, so they take the encode path), odd blobs are
/// noise (every codec loses, so they fall back to a raw stream); all have distinct digests.
fn mixed_blobs(count: usize) -> Vec<(Vec<u8>, &'static str)> {
    (0..count)
        .map(|k| {
            let bytes = if k % 2 == 0 {
                let mut smooth: Vec<u8> = (0..(BLOB / 4) as u32)
                    .flat_map(|i| (i as f32 * 0.001).to_le_bytes())
                    .collect();
                smooth[..4].copy_from_slice(&(k as f32 + 0.5).to_le_bytes());
                smooth
            } else {
                lcg_bytes(BLOB, k as u64)
            };
            (bytes, "rgb_f32le")
        })
        .collect()
}

#[test]
fn codec_blobs_peak_is_independent_of_blob_count() {
    bounded_case("codec_blobs_peak_is_independent", 120, || {
        let peak_with = |count: usize, codec: Codec| {
            let (snapshot, assets) = fixture(&mixed_blobs(count));
            let options = WriteOptions {
                asset_codec: codec,
                ..WriteOptions::default()
            };
            let mut sink = Discard::default();
            let meter = probe();
            write_package_streaming(
                &snapshot,
                &assets,
                &Extras::default(),
                &options,
                &Limits::default(),
                &token(),
                &mut sink,
            )
            .unwrap();
            let peak = meter.peak();
            let raw_total = (count * BLOB) as u64;
            match codec {
                Codec::Raw => assert!(sink.end > raw_total),
                // Smooth blobs shrink (the encode path ran), noise blobs stay raw (stream path).
                _ => assert!(sink.end > raw_total / 2 && sink.end < raw_total),
            }
            peak
        };
        for codec in [Codec::Raw, Codec::ShuffleZstd] {
            let few = peak_with(2, codec);
            let many = peak_with(8, codec);
            eprintln!(
                "MEASURED {codec:?}: peak heap {} KiB for 2 blobs, {} KiB for 8 blobs (1 MiB each, half noise)",
                few >> 10,
                many >> 10
            );
            assert!(
                many <= few + COUNT_SLACK,
                "{codec:?}: peak grew with the blob count ({few} B -> {many} B)"
            );
            if codec == Codec::Raw {
                assert!(many < 2 * BLOB, "raw stream peak {many} B");
            }
        }
    });
}
