//! Studio-side adapter of the `kumihimo-ui` text port (decision O4: shaping and rasterisation stay in
//! `hsk-studio-type`; the toolkit defines the port). [`TypeText`] shapes one UI line with `TextEngine::shaped_runs`
//! and rasterises glyphs with Type's deterministic fixed-point rasteriser (unhinted, grey coverage, STU-TYP-131).
//!
//! UI units: the toolkit shapes at the logical size, so 1 pt here = 1 logical px. Faces are static TrueType faces the
//! host registers with a weight; a style picks the face nearest its weight (no synthetic bold). Type refuses variable
//! instances other than the default, CFF2 and colour faces; those refusals come back as typed [`TextError`]s.
//!
//! The caller ports (ledger, resolver, scene) follow `hsk-studio-harness-view` view/text.rs, reduced to one line of UI
//! text. Every request carries the caller's ActorContext: this adapter never invents an actor.
#![forbid(unsafe_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_type as ty;
use kumihimo_ui::text::{FaceId, GlyphKey, GlyphMask, ShapedGlyph, ShapedLine, TextError, TextPort};
use kumihimo_ui::tokens::TextStyle;
use std::collections::HashMap;
use std::sync::Mutex;

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.ui_text.descriptor@1","crate":"hsk-studio-ui-text","role":"adapter: kumihimo-ui TextPort over hsk-studio-type","shaping":"TextEngine::shaped_runs, one line, Latn/en, tnum when the style asks","raster":"hsk_studio_type::rasterize_glyph, unhinted grey coverage, quarter-pixel x bins","faces":"static faces registered by the host with a weight; nearest weight wins","gaps":"no font fallback chain yet (one family), no script detection (Latn), no weight 500/600 face in the repo fixtures"}"#;

struct Face {
    family: String,
    weight: u16,
    bytes: Vec<u8>,
    hash: [u8; 32],
}
impl Face {
    fn resource(&self) -> ty::FontResource<'_> {
        ty::FontResource { identity: &self.family, location: &self.family, content_hash: self.hash, face_index: 0, resource_revision: 1, grant_revision: 1 }
    }
}

/// The adapter. Register faces with [`TypeText::add_face`], then wrap it: `kumihimo_ui::text::Text::new(Box::new(t))`.
pub struct TypeText {
    faces: Vec<Face>,
    actor: ActorContext,
    cancel: CancellationToken,
    outlines: HashMap<(u32, u32), ty::GlyphOutline>,
}

const DOC: &str = "SDOC-019a0000-0000-7000-8000-0000000000a1";
const LAYER: &str = "SLYR-019a0000-0000-7000-8000-0000000000a2";
const STORY: &str = "STXT-019a0000-0000-7000-8000-0000000000a3";
const STYLE: &str = "STYS-019a0000-0000-7000-8000-0000000000a4";
const RESULT: &[u8] = b"kumihimo-ui-text-result";
const RESOLVED: &[u8] = b"resolved-style";

impl TypeText {
    pub fn new(actor: ActorContext) -> Self {
        Self { faces: Vec::new(), actor, cancel: CancellationToken::default(), outlines: HashMap::new() }
    }
    /// Registers a face of `family` at `weight` (font bytes are hashed once here).
    pub fn add_face(&mut self, family: &str, weight: u16, bytes: Vec<u8>) -> FaceId {
        let hash = hsk_studio_prism::profile_hash(&bytes);
        self.faces.push(Face { family: format!("{family}-{weight}"), weight, bytes, hash });
        FaceId((self.faces.len() - 1) as u32)
    }
    fn pick(&self, weight: u16) -> Option<usize> {
        self.faces.iter().enumerate().min_by_key(|(_, f)| (i32::from(f.weight) - i32::from(weight)).unsigned_abs()).map(|(i, _)| i)
    }
}

// ----- Type caller ports (after hsk-studio-harness-view src/view/text.rs) ----------------------

#[derive(Default)]
struct LedgerState {
    current: u64,
    peak: u64,
    retained: u64,
    next: u64,
}
struct Ledger(Mutex<LedgerState>);
impl ty::RetirementPort for Ledger {
    fn retire(&self, _: u64, n: u64) {
        if let Ok(mut s) = self.0.lock() {
            s.current = s.current.saturating_sub(n);
        }
    }
}
impl ty::GenerationRetirement for Ledger {
    fn retire_generation(&self, _: u64) {
        if let Ok(mut s) = self.0.lock() {
            s.retained = s.retained.saturating_sub(1);
        }
    }
}
impl ty::AdmissionPort for Ledger {
    fn reserve(&self, n: u64, limit: u64) -> Result<ty::Reservation<'_>, ty::Error> {
        let mut s = self.0.lock().map_err(|_| ty::Error::Budget)?;
        let next = s.current.checked_add(n).ok_or(ty::Error::Overflow)?;
        if n == 0 || next > limit {
            return Err(ty::Error::Budget);
        }
        s.current = next;
        s.peak = s.peak.max(next);
        s.next = s.next.saturating_add(1);
        let handle = s.next;
        drop(s);
        ty::Reservation::new(self, handle, n)
    }
    fn snapshot(&self) -> Result<ty::AllocationSnapshot, ty::Error> {
        let s = self.0.lock().map_err(|_| ty::Error::Budget)?;
        Ok(ty::AllocationSnapshot { current_requested_bytes: s.current, operation_peak_requested_bytes: s.peak, historical_peak_requested_bytes: s.peak, retained_generations: s.retained })
    }
    fn retain_generation(&self, limit: u64) -> Result<ty::Generation<'_>, ty::Error> {
        let mut s = self.0.lock().map_err(|_| ty::Error::Budget)?;
        if s.retained >= limit {
            return Err(ty::Error::Budget);
        }
        s.retained = s.retained.saturating_add(1);
        s.next = s.next.saturating_add(1);
        let handle = s.next;
        drop(s);
        ty::Generation::new(self, handle)
    }
}
struct Fonts<'a> {
    face: &'a Face,
    ledger: &'a Ledger,
}
impl ty::FontResolver for Fonts<'_> {
    fn revision(&self) -> Result<u64, ty::Error> {
        Ok(1)
    }
    fn resolve(&self, r: &ty::FontResource<'_>) -> Result<ty::FontRead<'_>, ty::Error> {
        if r.identity != self.face.family {
            return Err(ty::Error::MissingFont);
        }
        let f = self.face;
        Ok(ty::FontRead { bytes: &f.bytes, resource_revision: 1, grant_revision: 1, lease: ty::AdmissionPort::reserve(self.ledger, f.bytes.len() as u64, u64::MAX)? })
    }
    fn verify_lifetime(&self, _: &ty::FontResource<'_>, read: &ty::FontRead<'_>) -> Result<(), ty::Error> {
        if read.lease.requested_bytes() < read.bytes.len() as u64 {
            return Err(ty::Error::LeaseUnavailable);
        }
        Ok(())
    }
}
struct Input(u64);
impl ty::InputLifetime for Input {
    fn verify(&self, _: &ty::Request<'_>) -> Result<ty::InputCharge, ty::Error> {
        Ok(ty::InputCharge { lifetime_id: 1, requested_bytes: self.0 })
    }
}
struct Scene<'a> {
    text: &'a str,
    styles: &'a [ty::Style<'a>],
}
impl ty::EpochPort for Scene<'_> {
    fn current_epoch(&self) -> Result<u64, ty::Error> {
        Ok(1)
    }
}
impl ty::SourceContext for Scene<'_> {
    fn member(&self, _: &ty::Request<'_>) -> Result<(), ty::Error> {
        Ok(())
    }
    fn read(&self, a: ty::ReadAddress<'_>) -> Result<ty::CurrentRead<'_>, ty::Error> {
        let bytes: &[u8] = match a.property {
            "text" => self.text.as_bytes(),
            "result" => RESULT,
            _ => RESOLVED,
        };
        Ok(ty::CurrentRead { revision: 1, fingerprint: hsk_studio_prism::profile_hash(bytes), bytes })
    }
    fn resolved_style(&self, a: ty::ReadAddress<'_>) -> Result<ty::Style<'_>, ty::Error> {
        let index = a.property.strip_prefix("style").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        self.styles.iter().find(|s| s.read_index == index).copied().ok_or(ty::Error::AbsentRead)
    }
}
fn limits() -> ty::Limits {
    ty::Limits {
        input_bytes: 4096,
        input_scalars: 4096,
        styles: 4,
        features: 8,
        axes: 4,
        font_resources: 4,
        font_bytes: 32 << 20,
        parser_tables: 128,
        lookups: 65536,
        runs: 64,
        glyphs: 4096,
        fallback_attempts: 8,
        recursion: 64,
        work_units: 20_000_000,
        requested_owned_bytes: 1 << 40,
        retained_generations: 8,
    }
}

fn refused(e: ty::Error) -> TextError {
    match e {
        ty::Error::Budget | ty::Error::Overflow => TextError::OverLimit,
        ty::Error::MissingFont | ty::Error::UnavailableFont | ty::Error::RevokedFont => TextError::NoFace,
        _ => TextError::Refused("type_shaping_refused"),
    }
}

impl TextPort for TypeText {
    fn shape(&mut self, text: &str, style: &TextStyle) -> Result<ShapedLine, TextError> {
        let fi = self.pick(style.weight).ok_or(TextError::NoFace)?;
        if fi < usize::MAX {
            return Err(TextError::NoFace); // RED STUB
        }
        if text.is_empty() {
            return Ok(ShapedLine::default());
        }
        let face = &self.faces[fi];
        let ids = |s: &str| DomainId::parse(s).map_err(|_| TextError::Refused("ui_text_id"));
        let (document, layer, story, style_id) = (ids(DOC)?, ids(LAYER)?, ids(STORY)?, ids(STYLE)?);
        let ledger = Ledger(Mutex::new(LedgerState::default()));
        let resolver = Fonts { face, ledger: &ledger };
        let range = ty::SourceRange { start: 0, end: text.len() as u64 };
        let candidates = [face.resource()];
        let tnum = [ty::Feature { tag: *b"tnum", value: 1, range, declared: true }];
        let styles = [ty::Style {
            read_index: 1,
            range,
            font_size_pt: f64::from(style.size),
            requested_identity: &face.family,
            candidates: &candidates,
            substitution: None,
            script: *b"Latn",
            language: "en",
            character_override: ty::CharacterOverride::Default,
            features: if style.tnum { &tnum } else { &[] },
            axes: &[],
        }];
        let scene = Scene { text, styles: &styles };
        let text_hash = hsk_studio_prism::profile_hash(text.as_bytes());
        let reads = [
            ty::ReadAddress { owner: &story, property: "text", expected_revision: 1, fingerprint: text_hash },
            ty::ReadAddress { owner: &style_id, property: "style1", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(RESOLVED) },
        ];
        let paragraphs = [ty::ParagraphInput { range, direction: ty::Direction::LeftToRight, provenance: ty::DirectionProvenance::Explicit }];
        let input_lease = ty::AdmissionPort::reserve(&ledger, 1 << 16, u64::MAX).map_err(refused)?;
        let input = Input(1 << 16);
        let ports = ty::Ports { context: &scene, resolver: &resolver, input_lifetime: &input, admission: &ledger };
        let request = ty::Request {
            document_id: &document,
            document_revision: 1,
            story_id: &story,
            layer_id: &layer,
            text_utf8: text,
            text_sha256: text_hash,
            text_read_index: 0,
            reads: &reads,
            result_target: ty::ResultTarget {
                address: ty::ReadAddress { owner: &story, property: "result", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(RESULT) },
                preimage: RESULT,
            },
            composition_version: ty::COMPOSITION_VERSION,
            normalization: ty::Normalization::Preserve,
            story_direction: ty::StoryDirection::LeftToRight,
            paragraphs: &paragraphs,
            styles: &styles,
            resolver_revision: 1,
            cancel_epoch: 1,
            cancellation: &self.cancel,
            actor: &self.actor,
            correlation_id: "kumihimo-ui-text",
            observation_correlation: 1,
            limits: limits(),
        };
        let prepared = ty::TextEngine.shaped_runs(request, &ports).map_err(|r| refused(r.error))?;
        let mut line = ShapedLine::default();
        let mut x = 0.0f64;
        for p in prepared.paragraphs() {
            for run in p.runs() {
                if let Some(cap) = run.cap_height_pt() {
                    line.cap_height = line.cap_height.max(cap as f32);
                }
                for g in run.glyphs() {
                    line.glyphs.push(ShapedGlyph {
                        face: FaceId(fi as u32),
                        glyph_id: g.glyph_id,
                        x: (x + g.x_offset_pt) as f32,
                        y: g.y_offset_pt as f32,
                        advance: g.x_advance_pt as f32,
                        cluster: (g.cluster.start as u32, g.cluster.end as u32),
                    });
                    x += g.x_advance_pt;
                }
            }
        }
        line.width = x as f32;
        drop(prepared);
        drop(input_lease);
        Ok(line)
    }

    fn rasterize(&mut self, key: GlyphKey) -> Result<GlyphMask, TextError> {
        let face = self.faces.get(key.face.0 as usize).ok_or(TextError::NoFace)?;
        if key.px_size_64 == 0 || key.subpx_x > 3 {
            return Err(TextError::Refused("ui_text_key"));
        }
        let ok = (key.face.0, key.glyph_id);
        if !self.outlines.contains_key(&ok) {
            let bytes = ty::FontBytes { bytes: &face.bytes, resource: face.resource() };
            let mut out = ty::glyph_outlines(&bytes, &[key.glyph_id], &ty::OutlineLimits::default(), &self.cancel).map_err(|e| TextError::Refused(e.code()))?;
            let outline = out.pop().ok_or(TextError::Refused("glyph_outline_unavailable"))?;
            self.outlines.insert(ok, outline);
        }
        let outline = &self.outlines[&ok];
        if outline.commands().is_empty() {
            return Ok(GlyphMask::default());
        }
        let px = f64::from(key.px_size_64) / 64.0;
        let per_unit = px / f64::from(outline.units_per_em());
        let origin = [f64::from(key.subpx_x) / 4.0, 0.0];
        let cov = ty::rasterize_glyph(outline, origin, per_unit, &ty::RasterLimits::default(), &self.cancel).map_err(|e| TextError::Refused(e.code()))?;
        let left = i32::try_from(cov.x()).map_err(|_| TextError::OverLimit)?;
        let top = i32::try_from(cov.y()).map_err(|_| TextError::OverLimit)?;
        Ok(GlyphMask { left, top, width: cov.width(), height: cov.height(), data: cov.data().to_vec() })
    }
}
