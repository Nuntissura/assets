// Copied from hsk-studio-composite/tests/common/text_shaping.rs (same workspace, test support only).
//! Caller-side shaping double for the text-lowering tests: the minimum ports `TextEngine` needs
//! (one admission ledger, a fixed-font resolver, a single-story source context). Fonts are the
//! hash-pinned OFL fixtures of hsk-studio-type (tests/fixtures), read from this repository only.
#![allow(dead_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_type::*;
use std::sync::Mutex;

pub struct FontFile {
    pub identity: &'static str,
    pub bytes: Vec<u8>,
    pub hash: [u8; 32],
}
impl FontFile {
    pub fn fixture(identity: &'static str, relative: &str) -> Self {
        let path = format!("{}/../hsk-studio-type/tests/fixtures/{relative}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("fixture font {path}: {e}"));
        let hash = hsk_studio_prism::profile_hash(&bytes);
        Self { identity, bytes, hash }
    }
    pub fn inter() -> Self {
        Self::fixture("Inter-Regular", "fonts/Inter-Regular.ttf")
    }
    pub fn arabic() -> Self {
        Self::fixture("NotoSansArabic-Regular", "fonts/NotoSansArabic-Regular.ttf")
    }
    pub fn han() -> Self {
        Self::fixture("NotoSansSC-Regular", "fonts/NotoSansSC-Regular.otf")
    }
    pub fn inter_variable() -> Self {
        Self::fixture("Inter-Variable", "evidence/Inter-variable-opsz-wght.ttf")
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

/// One style span: byte range, font index into the font list, script, BCP 47 language, axes.
pub struct Seg {
    pub start: u64,
    pub end: u64,
    pub font: usize,
    pub script: [u8; 4],
    pub language: &'static str,
    pub axes: Vec<Axis>,
}

#[derive(Default)]
struct LedgerState {
    current: u64,
    peak: u64,
    retained: u64,
    next: u64,
}
pub struct Ledger(Mutex<LedgerState>);
impl RetirementPort for Ledger {
    fn retire(&self, _: u64, n: u64) {
        let mut s = self.0.lock().unwrap();
        s.current -= n;
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
fn limits() -> Limits {
    Limits {
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
        retained_generations: 8,
    }
}
fn id(prefix: &str) -> DomainId {
    DomainId::parse(&format!("{prefix}-019abcde-0000-7000-8000-000000000001")).unwrap()
}

/// Shapes `text` (one paragraph) with the given style spans and hands the result to `body`.
pub fn shape<R>(
    files: &[FontFile],
    text: &str,
    segs: &[Seg],
    rtl: bool,
    size_pt: f64,
    body: impl for<'a> FnOnce(&Prepared<'a>) -> R,
) -> R {
    let (document, story, layer, style_id) = (id("SDOC"), id("STXT"), id("SLYR"), id("STYS"));
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let ledger = Ledger(Mutex::new(LedgerState::default()));
    let resolver = Fonts { files, ledger: &ledger };
    let candidates: Vec<Vec<FontResource<'_>>> = segs.iter().map(|s| vec![files[s.font].resource()]).collect();
    let styles: Vec<Style<'_>> = segs
        .iter()
        .enumerate()
        .map(|(i, s)| Style {
            read_index: (i + 1) as u32,
            range: SourceRange { start: s.start, end: s.end },
            font_size_pt: size_pt,
            requested_identity: files[s.font].identity,
            candidates: &candidates[i],
            substitution: None,
            script: s.script,
            language: s.language,
            character_override: CharacterOverride::Default,
            features: &[],
            axes: &s.axes,
        })
        .collect();
    let names: Vec<String> = (1..=styles.len()).map(|i| format!("style{i}")).collect();
    let mut reads = vec![ReadAddress {
        owner: &story,
        property: "text",
        expected_revision: 1,
        fingerprint: hsk_studio_prism::profile_hash(text.as_bytes()),
    }];
    for name in &names {
        reads.push(ReadAddress {
            owner: &style_id,
            property: name,
            expected_revision: 1,
            fingerprint: hsk_studio_prism::profile_hash(b"resolved-style"),
        });
    }
    let direction = if rtl { Direction::RightToLeft } else { Direction::LeftToRight };
    let paragraphs = [ParagraphInput {
        range: SourceRange { start: 0, end: text.len() as u64 },
        direction,
        provenance: DirectionProvenance::Explicit,
    }];
    let input_lease = ledger.reserve(1 << 20, u64::MAX).unwrap();
    let input = Input(1 << 20);
    let scene = Scene { text, styles: &styles };
    let ports = Ports { context: &scene, resolver: &resolver, input_lifetime: &input, admission: &ledger };
    let request = Request {
        document_id: &document,
        document_revision: 1,
        story_id: &story,
        layer_id: &layer,
        text_utf8: text,
        text_sha256: hsk_studio_prism::profile_hash(text.as_bytes()),
        text_read_index: 0,
        reads: &reads,
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
        story_direction: if rtl { StoryDirection::RightToLeft } else { StoryDirection::LeftToRight },
        paragraphs: &paragraphs,
        styles: &styles,
        resolver_revision: 1,
        cancel_epoch: 1,
        cancellation: &cancel,
        actor: &actor,
        correlation_id: "interop-layout-pdf",
        observation_correlation: 1,
        limits: limits(),
    };
    let prepared = TextEngine
        .shaped_runs(request, &ports)
        .unwrap_or_else(|r| panic!("shaping refused {:?} at {:?}", r.error, r.source));
    let out = body(&prepared);
    drop(prepared);
    drop(input_lease);
    out
}
