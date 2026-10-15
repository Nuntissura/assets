//! Shared harness of tests/fig_instances_frames.rs (lane SMB-INTEROP-FIG-INSTANCES-FRAMES): fixture
//! mapping, caller-side text shaping (the minimum Type ports, after hsk-studio-composite
//! tests/common/text_shaping.rs), composite + render-cpu rendering and PNG decoding. Fonts are the
//! hash-pinned OFL fixtures of hsk-studio-type (tests/fixtures), read from this repository only.
#![allow(dead_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, SCHEMA_STUDIO_COMPONENT_INSTANCE};
use hsk_studio_composite::{
    BlendSpace, EvalReport, Exactness, ImageInfo, Limits as CLimits, PreparedText, RenderInputs, StepStatus,
    TextLayerInput, TextResolution, TextRun as CTextRun, VectorAssets, VectorResolution, evaluate_with_inputs,
    lower, lower_with_inputs, prepare_text, prepare_vectors,
};
use hsk_studio_folio::{
    Budget, Inspection, Resolution, Resolver, Snapshot, StoryContent, StudioImageSource, TileRef, inspect_bytes,
};
use hsk_studio_interop_fig::{
    FigDocument, FigOutcome, FigTextOutput, NativeConfig, NativeProposal, SequentialIdMint, VersionPolicy,
    decode_document, import_fig, to_native,
};
use hsk_studio_nib::OwnedPath;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, ExecuteRequest, OutputTarget, PlanSources, RenderBackend, SampleFormat, SinkError,
    SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_render_cpu::Renderer;
use hsk_studio_type::*;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub struct Host;
impl Resolver for Host {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}
struct Available;
impl Resolver for Available {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema: &str) -> bool {
        kind == "component_instance" && schema == SCHEMA_STUDIO_COMPONENT_INSTANCE
    }
}

pub fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

pub fn document(name: &str) -> FigDocument {
    let bytes = fixture(name);
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) =
        import_fig(&bytes, &hsk_studio_interop_fig::Limits::default(), &VersionPolicy::observed(), None, &cancel).unwrap()
    else {
        panic!("{name} imports")
    };
    decode_document(&import, &hsk_studio_interop_fig::Limits::default(), &cancel).unwrap()
}

pub fn map(doc: &FigDocument, budget: Budget) -> NativeProposal {
    let cfg = NativeConfig { budget, resolver: &Host };
    to_native(doc, &cfg, &mut SequentialIdMint::new(0xF16), &CancellationToken::default()).unwrap()
}

pub fn layer<'a>(p: &'a NativeProposal, id: &str) -> &'a hsk_studio_folio::StudioLayer {
    p.snapshot.document().layers.iter().find(|l| l.layer_id == id).expect("layer")
}

pub fn story_text(p: &NativeProposal, story_id: &str) -> String {
    let s = p.stories.iter().find(|s| s.story_id == story_id).expect("story");
    match &s.content {
        StoryContent::Inline { text_utf8 } => text_utf8.clone(),
        other => panic!("inline story expected: {other:?}"),
    }
}

// ---------- fonts and shaping ----------

pub struct FontFile {
    pub identity: &'static str,
    pub bytes: Vec<u8>,
    pub hash: [u8; 32],
}
impl FontFile {
    pub fn inter_regular() -> Self {
        let path = format!("{}/../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture font {path}: {e}"));
        let hash = hsk_studio_prism::profile_hash(&bytes);
        Self { identity: "Inter-Regular", bytes, hash }
    }
    pub fn resource(&self) -> FontResource<'_> {
        FontResource {
            identity: self.identity,
            location: self.identity,
            content_hash: self.hash,
            face_index: 0,
            resource_revision: 1,
            grant_revision: 1,
        }
    }
}

#[derive(Default)]
struct LedgerState {
    current: u64,
    peak: u64,
    retained: u64,
    next: u64,
}
struct Ledger(Mutex<LedgerState>);
impl RetirementPort for Ledger {
    fn retire(&self, _: u64, n: u64) {
        self.0.lock().unwrap().current -= n;
    }
}
impl GenerationRetirement for Ledger {
    fn retire_generation(&self, _: u64) {
        self.0.lock().unwrap().retained -= 1;
    }
}
impl AdmissionPort for Ledger {
    fn reserve(&self, n: u64, limit: u64) -> Result<Reservation<'_>, Error> {
        let mut s = self.0.lock().unwrap();
        let next = s.current.checked_add(n).ok_or(Error::Overflow)?;
        if n == 0 || next > limit {
            return Err(Error::Budget);
        }
        s.current = next;
        s.peak = s.peak.max(next);
        s.next += 1;
        let handle = s.next;
        drop(s);
        Reservation::new(self, handle, n)
    }
    fn snapshot(&self) -> Result<AllocationSnapshot, Error> {
        let s = self.0.lock().unwrap();
        Ok(AllocationSnapshot {
            current_requested_bytes: s.current,
            operation_peak_requested_bytes: s.peak,
            historical_peak_requested_bytes: s.peak,
            retained_generations: s.retained,
        })
    }
    fn retain_generation(&self, limit: u64) -> Result<Generation<'_>, Error> {
        let mut s = self.0.lock().unwrap();
        if s.retained >= limit {
            return Err(Error::Budget);
        }
        s.retained += 1;
        s.next += 1;
        let handle = s.next;
        drop(s);
        Generation::new(self, handle)
    }
}
struct Fonts<'a> {
    files: &'a [FontFile],
    ledger: &'a Ledger,
}
impl FontResolver for Fonts<'_> {
    fn revision(&self) -> Result<u64, Error> {
        Ok(1)
    }
    fn resolve(&self, r: &FontResource<'_>) -> Result<FontRead<'_>, Error> {
        let f = self.files.iter().find(|f| f.identity == r.identity).ok_or(Error::MissingFont)?;
        Ok(FontRead {
            bytes: &f.bytes,
            resource_revision: 1,
            grant_revision: 1,
            lease: self.ledger.reserve(f.bytes.len() as u64, u64::MAX)?,
        })
    }
    fn verify_lifetime(&self, _: &FontResource<'_>, read: &FontRead<'_>) -> Result<(), Error> {
        if read.lease.requested_bytes() < read.bytes.len() as u64 {
            return Err(Error::LeaseUnavailable);
        }
        Ok(())
    }
}
struct Input(u64);
impl InputLifetime for Input {
    fn verify(&self, _: &Request<'_>) -> Result<InputCharge, Error> {
        Ok(InputCharge { lifetime_id: 1, requested_bytes: self.0 })
    }
}
struct Scene<'a> {
    text: &'a str,
    styles: &'a [Style<'a>],
}
impl EpochPort for Scene<'_> {
    fn current_epoch(&self) -> Result<u64, Error> {
        Ok(1)
    }
}
impl SourceContext for Scene<'_> {
    fn member(&self, _: &Request<'_>) -> Result<(), Error> {
        Ok(())
    }
    fn read(&self, a: ReadAddress<'_>) -> Result<CurrentRead<'_>, Error> {
        let bytes: &[u8] = match a.property {
            "text" => self.text.as_bytes(),
            "result" => b"original-preimage",
            _ => b"resolved-style",
        };
        Ok(CurrentRead { revision: 1, fingerprint: hsk_studio_prism::profile_hash(bytes), bytes })
    }
    fn resolved_style(&self, a: ReadAddress<'_>) -> Result<Style<'_>, Error> {
        let index = a.property.strip_prefix("style").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        self.styles.iter().find(|s| s.read_index == index).copied().ok_or(Error::AbsentRead)
    }
}
fn limits() -> hsk_studio_type::Limits {
    hsk_studio_type::Limits {
        input_bytes: 4096,
        input_scalars: 4096,
        styles: 32,
        features: 64,
        axes: 16,
        font_resources: 32,
        font_bytes: 16 << 20,
        parser_tables: 128,
        lookups: 65536,
        runs: 128,
        glyphs: 4096,
        fallback_attempts: 64,
        recursion: 64,
        work_units: 50_000_000,
        requested_owned_bytes: 1 << 40,
        retained_generations: 64,
    }
}
fn id(prefix: &str) -> DomainId {
    DomainId::parse(&format!("{prefix}-019abcde-0000-7000-8000-000000000001")).unwrap()
}

/// One piece of text shaped on its own: one font, one size (px; shaped at 1 px per pt).
pub struct Piece {
    pub text: String,
    pub font: usize,
    pub size_px: f64,
}

/// Shapes every piece (one paragraph, left to right, Latin) and hands all results to `body`.
pub fn shape_all<R>(files: &[FontFile], pieces: &[Piece], body: impl for<'a> FnOnce(&[Prepared<'a>]) -> R) -> R {
    let (document, story, layer, style_id) = (id("SDOC"), id("STXT"), id("SLYR"), id("STYS"));
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let ledger = Ledger(Mutex::new(LedgerState::default()));
    let resolver = Fonts { files, ledger: &ledger };
    let candidates: Vec<[FontResource<'_>; 1]> = pieces.iter().map(|p| [files[p.font].resource()]).collect();
    let styles: Vec<[Style<'_>; 1]> = pieces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            [Style {
                read_index: 1,
                range: SourceRange { start: 0, end: p.text.len() as u64 },
                font_size_pt: p.size_px,
                requested_identity: files[p.font].identity,
                candidates: &candidates[i],
                substitution: None,
                script: *b"Latn",
                language: "en",
                character_override: CharacterOverride::Default,
                features: &[],
                axes: &[],
            }]
        })
        .collect();
    let reads: Vec<[ReadAddress<'_>; 2]> = pieces
        .iter()
        .map(|p| {
            [
                ReadAddress {
                    owner: &story,
                    property: "text",
                    expected_revision: 1,
                    fingerprint: hsk_studio_prism::profile_hash(p.text.as_bytes()),
                },
                ReadAddress {
                    owner: &style_id,
                    property: "style1",
                    expected_revision: 1,
                    fingerprint: hsk_studio_prism::profile_hash(b"resolved-style"),
                },
            ]
        })
        .collect();
    let paragraphs: Vec<[ParagraphInput; 1]> = pieces
        .iter()
        .map(|p| {
            [ParagraphInput {
                range: SourceRange { start: 0, end: p.text.len() as u64 },
                direction: Direction::LeftToRight,
                provenance: DirectionProvenance::Explicit,
            }]
        })
        .collect();
    let scenes: Vec<Scene<'_>> =
        pieces.iter().enumerate().map(|(i, p)| Scene { text: &p.text, styles: &styles[i] }).collect();
    let input = Input(1 << 20);
    let leases: Vec<_> = pieces.iter().map(|_| ledger.reserve(1 << 20, u64::MAX).unwrap()).collect();
    let ports: Vec<Ports<'_>> = scenes
        .iter()
        .map(|sc| Ports { context: sc, resolver: &resolver, input_lifetime: &input, admission: &ledger })
        .collect();
    let prepared: Vec<Prepared<'_>> = pieces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let request = Request {
                document_id: &document,
                document_revision: 1,
                story_id: &story,
                layer_id: &layer,
                text_utf8: &p.text,
                text_sha256: hsk_studio_prism::profile_hash(p.text.as_bytes()),
                text_read_index: 0,
                reads: &reads[i],
                result_target: ResultTarget {
                    address: ReadAddress {
                        owner: &story,
                        property: "result",
                        expected_revision: 1,
                        fingerprint: hsk_studio_prism::profile_hash(b"original-preimage"),
                    },
                    preimage: b"original-preimage",
                },
                composition_version: COMPOSITION_VERSION,
                normalization: Normalization::Preserve,
                story_direction: StoryDirection::LeftToRight,
                paragraphs: &paragraphs[i],
                styles: &styles[i],
                resolver_revision: 1,
                cancel_epoch: 1,
                cancellation: &cancel,
                actor: &actor,
                correlation_id: "interop-fig-text",
                observation_correlation: 1,
                limits: limits(),
            };
            TextEngine
                .shaped_runs(request, &ports[i])
                .unwrap_or_else(|r| panic!("shaping refused {:?} at {:?}", r.error, r.source))
        })
        .collect();
    let out = body(&prepared);
    drop(prepared);
    drop(leases);
    out
}

/// Shapes and rasterises the text outputs (each line's runs placed from Figma's baseline, pen
/// advancing by the shaped advances) into composite's prepared text. One fill per layer (its
/// first run's).
pub fn prepare_outputs(p: &NativeProposal, outputs: &[FigTextOutput<'_>], files: &[FontFile]) -> PreparedText {
    struct Placed {
        output: usize,
        piece: usize,
        origin: [f64; 2],
    }
    let mut pieces = Vec::new();
    let mut placed: Vec<(usize, usize, usize, f64)> = Vec::new(); // output, line, piece, baseline y
    for (o, out) in outputs.iter().enumerate() {
        let text = story_text(p, &out.story_id);
        for (li, line) in out.lines.iter().enumerate() {
            for r in &out.runs {
                let (a, b) = (r.start.max(line.start), r.end.min(line.end));
                if a >= b {
                    continue;
                }
                let s = text[a as usize..b as usize].trim_end_matches('\n').to_owned();
                if s.is_empty() {
                    continue;
                }
                let font = files.iter().position(|f| f.identity == r.font.identity).expect("granted font file");
                placed.push((o, li, pieces.len(), line.baseline_px[1]));
                pieces.push(Piece { text: s, font, size_px: r.size_px });
            }
        }
    }
    shape_all(files, &pieces, |prepared| {
        // Pen x per (output, line): starts at the line's baseline x, advances per piece.
        let mut pen: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        let mut per_layer: Vec<Vec<CTextRun<'_>>> = outputs.iter().map(|_| Vec::new()).collect();
        let mut origins: Vec<Placed> = Vec::new();
        for &(o, li, k, y) in &placed {
            let x = *pen.entry((o, li)).or_insert(outputs[o].lines[li].baseline_px[0]);
            origins.push(Placed { output: o, piece: k, origin: [x, y] });
            let adv: f64 = prepared[k].paragraphs()[0].runs().iter().flat_map(|r| r.glyphs()).map(|g| g.x_advance_pt).sum();
            *pen.get_mut(&(o, li)).unwrap() = x + adv;
        }
        for pl in &origins {
            let mut x = pl.origin[0];
            for run in prepared[pl.piece].paragraphs()[0].runs() {
                let file = &files[pieces[pl.piece].font];
                per_layer[pl.output].push(CTextRun {
                    run,
                    font: hsk_studio_type::FontBytes { bytes: &file.bytes, resource: run.resolved() },
                    origin_px: [x, pl.origin[1]],
                });
                x += run.glyphs().iter().map(|g| g.x_advance_pt).sum::<f64>();
            }
        }
        let inputs: Vec<TextLayerInput<'_>> = outputs
            .iter()
            .zip(&per_layer)
            .map(|(o, runs)| TextLayerInput {
                layer_id: &o.layer_id,
                runs,
                fill_rgba: o.runs[0].fill_rgba.map(|c| c as f32),
            })
            .collect();
        prepare_text(&inputs, &TextResolution::at_ppi(72.0), &CancellationToken::default()).expect("text prepares")
    })
}

// ---------- rendering ----------

struct Assets<'a>(&'a NativeProposal);
impl VectorAssets for Assets<'_> {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        self.0.paths.iter().find(|p| p.path_id == path_id).map(|p| p.geometry.clone())
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

fn reopened(p: &NativeProposal) -> Snapshot {
    let mut v: serde_json::Value = serde_json::from_slice(p.snapshot.encoded_bytes()).unwrap();
    for slot in ["working_space_rgb", "working_space_cmyk", "working_space_gray", "working_space_spot", "blending_space"] {
        v[slot] = serde_json::json!("SCPF-019abcde-0000-7000-8000-000000000001");
    }
    let budget = Budget { nodes: 1 << 20, edges: 1 << 20, input_bytes: 1 << 28, payload_bytes: 1 << 26, depth: 64 };
    match inspect_bytes(&serde_json::to_vec(&v).unwrap(), budget, &CancellationToken::default(), &Available).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read only"),
    }
}

struct Sources<'a>(
    &'a hsk_studio_composite::InputsLowering,
    &'a hsk_studio_composite::PreparedVectors,
    Option<&'a PreparedText>,
    ColourTag,
);
impl PlanSources for Sources<'_> {
    /// Painted (gradient) row tiles of the vector layers.
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        self.0.vector_paint(key, self.1, self.3)
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        if let Some(m) = self.0.vector_mask(key, self.1) {
            return Some(m);
        }
        let text = self.2?;
        let (_, layer) = self.0.text_masks.iter().find(|(k, _)| *k == key)?;
        text.plane(layer)?.mask()
    }
}
struct Grid {
    w: usize,
    px: Vec<[f32; 4]>,
}
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                self.px[(r.y as usize + y) * self.w + r.x as usize + x] =
                    [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

pub struct Render {
    pub w: usize,
    pub px: Vec<[f32; 4]>,
    pub refused: BTreeMap<String, u32>,
    pub outputs: usize,
    pub rendered: usize,
}
pub const W: usize = 1400;
pub const H: usize = 900;

/// Composites the ready graph outputs (with `text` planes when given) on render-cpu at 1 px per
/// document px; refused outputs are counted by reason and left out.
pub fn render(p: &NativeProposal, text: Option<&PreparedText>) -> Render {
    let snap = reopened(p);
    let cancel = CancellationToken::default();
    let vectors = prepare_vectors(
        &snap,
        &p.design,
        &p.vector_records,
        &Assets(p),
        BlendSpace::Encoded,
        // Exact nib coverage (composite VectorCoverage::Exact, 5898a82) on the interop-fig render path.
        &VectorResolution { coverage: hsk_studio_composite::VectorCoverage::Exact, ..VectorResolution::default() },
        &cancel,
    )
    .unwrap();
    let mut plan = lower(&snap, snap.document().revision, BlendSpace::Encoded, &CLimits::default(), &cancel).unwrap();
    let outputs = plan.outputs.len();
    let mut refused: BTreeMap<String, u32> = BTreeMap::new();
    let inputs = RenderInputs { vectors: Some(&vectors), records: Some(&p.raster_records), text };
    let probe: EvalReport = evaluate_with_inputs(&plan, &snap, &inputs, &cancel).unwrap();
    plan.outputs.retain(|o| match probe.step(o).map(|s| &s.status) {
        Some(StepStatus::Unavailable(reason)) => {
            let key = reason.rsplit(':').next().unwrap_or(reason).to_owned();
            *refused.entry(key).or_insert(0) += 1;
            false
        }
        _ => true,
    });
    let rendered = plan.outputs.len();
    let mut px = vec![[0.0f32; 4]; W * H];
    if rendered > 0 {
        let plan = plan.with_document_root().unwrap();
        let report: EvalReport = evaluate_with_inputs(&plan, &snap, &inputs, &cancel).unwrap();
        let target = OutputTarget {
            extent: Rect { x: 0, y: 0, width: W as u32, height: H as u32 },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space: ColourTag {
                profile_sha256: hsk_studio_prism::profile_hash(b"working-profile"),
                blend_space: BlendSpace::Encoded,
            },
        };
        let no_tiles = |_: &TileRef| None;
        let space = target.space;
        let low = lower_with_inputs(&plan, &report, &snap, &inputs, &plan.document_root(), target, &no_tiles)
            .unwrap_or_else(|e| panic!("lowering: {e}"));
        let mut grid = Grid { w: W, px: vec![[0.0; 4]; W * H] };
        let request = ExecuteRequest {
            expected_revision: snap.document().revision,
            min_exactness: Exactness::Approximate,
            deadline: None,
            max_working_bytes: 1 << 28,
        };
        Renderer::with_threads(256, 4)
            .unwrap()
            .execute_extended(&low.plan, &low.extensions, &request, &Sources(&low, &vectors, text, space), &cancel, &mut grid)
            .unwrap();
        px = grid.px;
    }
    Render { w: W, px, refused, outputs, rendered }
}

/// Our pixel at canvas (x, y) over a background colour, 0..=255 per channel.
pub fn over(r: &Render, x: usize, y: usize, bg: [u8; 3]) -> [f64; 3] {
    let p = r.px[y * r.w + x];
    let a = f64::from(p[3]);
    [0, 1, 2].map(|c| f64::from(p[c]) * 255.0 * a + f64::from(bg[c]) * (1.0 - a))
}

pub fn decode_png(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let (mut at, mut idat) = (8usize, Vec::new());
    let (mut w, mut h, mut ct) = (0usize, 0usize, 0u8);
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body = &bytes[at + 8..at + 8 + len];
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                h = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                assert_eq!((body[8], body[12]), (8, 0), "8-bit, not interlaced");
                ct = body[9];
            }
            b"IDAT" => idat.extend_from_slice(body),
            _ => {}
        }
        at += 12 + len;
    }
    let bpp = match ct {
        2 => 3,
        6 => 4,
        other => panic!("colour type {other}"),
    };
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).expect("inflate");
    let stride = w * bpp;
    let mut out = vec![0u8; h * stride];
    for y in 0..h {
        let f = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { out[y * stride + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= bpp { out[(y - 1) * stride + x - bpp] } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let (pa, pb, pc) = (
                        (i32::from(b) - i32::from(c)).abs(),
                        (i32::from(a) - i32::from(c)).abs(),
                        (i32::from(a) + i32::from(b) - 2 * i32::from(c)).abs(),
                    );
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                other => panic!("filter {other}"),
            };
            out[y * stride + x] = line[x].wrapping_add(pred);
        }
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in out.chunks(bpp) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], if bpp == 4 { px[3] } else { 255 }]);
    }
    (w, h, rgba)
}

/// Per-pixel comparison of our render over `origin` with a whole PNG (over the PNG's top-left
/// colour): (max channel difference per pixel, as a sorted list).
pub fn diffs(r: &Render, png: &(usize, usize, Vec<u8>), origin: (usize, usize)) -> Vec<f64> {
    let bg = [png.2[0], png.2[1], png.2[2]];
    let mut out = Vec::with_capacity(png.0 * png.1);
    for y in 0..png.1 {
        for x in 0..png.0 {
            let ours = over(r, origin.0 + x, origin.1 + y, bg);
            let o = (y * png.0 + x) * 4;
            out.push((0..3).map(|c| (ours[c] - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max));
        }
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap());
    out
}
