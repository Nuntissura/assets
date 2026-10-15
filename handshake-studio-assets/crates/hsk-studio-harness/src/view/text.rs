//! Text layers in the viewer (lane SMB-HARNESS-UI-3 slice 4).
//!
//! Creating a text layer is one chronicle transaction (a `placement` create of a Folio `text` layer
//! whose `Payload::Text` names a `STXT` story and whose single source output is image-typed, Folio
//! G1, so it composites in any stack). The story (`StudioTextStory`, inline UTF-8, one paragraph,
//! one character range) and its paragraph and character `StudioTypeStyle` records (font family,
//! size, fill) are validated by Folio and kept with the document in the session document entry
//! `text` (`studio/text.<sha256>`), with the point-text pen origin per layer (Folio stores no
//! point-text origin: SPEC GAP, recorded in the lane). Rendering shapes each story with
//! `hsk_studio_type::TextEngine` (caller ports adapted from composite's text-lowering test
//! double), and composite `prepare_text` + `lower_to_render_plan_with_text` draw it (render.rs).
//!
//! Units: 1 pt = 1 document pixel (72 ppi). Fonts: the font family is resolved against the
//! viewer's font list, which holds egui's bundled `Ubuntu-Light` (decision 2026-10-14: egui default
//! fonts are allowed in UI crates); `add_font` registers more (TrueType outlines; colour, bitmap and
//! CFF2 faces are refused by Type). Fill: the character style's RGBA is linear light in the RGB
//! working space (the plan's blend space). Line breaking is not done (one line per story).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_composite::{PreparedText, TextLayerInput, TextResolution, TextRun, prepare_text};
use hsk_studio_folio::{self as folio, Payload, StudioDocument, StudioTextStory, StudioTypeStyleRecords};
use hsk_studio_type as ty;
use serde_json::{Value, json};
use std::sync::Mutex;

/// Session document entry token of the text records.
pub const ENTRY: &str = "text";
/// Schema of the entry this module writes.
pub const ENTRY_SCHEMA: &str = "hsk.harness.text_layers@1";
/// The bundled default font family.
pub const DEFAULT_FONT: &str = "Ubuntu-Light";
/// Most text layers one document entry holds.
pub const MAX_TEXT_LAYERS: usize = 256;
/// Longest story the viewer shapes (bytes).
pub const MAX_STORY_BYTES: usize = 4096;

/// Where a text layer draws and which records it uses.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayerRecord {
    pub layer_id: String,
    pub story_id: String,
    /// Pen origin on the first baseline, document pixels.
    pub origin_px: [f64; 2],
    pub paragraph_style: String,
    pub character_style: String,
}

/// The `text` document entry.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEntry {
    pub schema: String,
    pub layers: Vec<TextLayerRecord>,
    pub stories: Vec<StudioTextStory>,
    pub styles: StudioTypeStyleRecords,
}

impl TextEntry {
    pub fn empty(document_id: &str) -> Self {
        Self { schema: ENTRY_SCHEMA.into(), layers: Vec::new(), stories: Vec::new(), styles: StudioTypeStyleRecords::new(document_id) }
    }

    /// Decodes and validates the entry against the document (stories and styles through Folio).
    pub fn decode(bytes: &[u8], doc: &StudioDocument, cancel: &CancellationToken) -> Result<Self, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("text_entry_json: {e}"))?;
        let Some(object) = v.as_object() else {
            return Err("text_entry_json: not an object".into());
        };
        if object.keys().any(|k| !["schema", "layers", "stories", "styles"].contains(&k.as_str())) {
            return Err("text_entry_unknown_field".into());
        }
        if v["schema"] != ENTRY_SCHEMA {
            return Err("text_entry_schema".into());
        }
        let (layers, stories) = (v["layers"].as_array().ok_or("text_entry_layers")?, v["stories"].as_array().ok_or("text_entry_stories")?);
        if layers.len() > MAX_TEXT_LAYERS || stories.len() > MAX_TEXT_LAYERS {
            return Err("text_entry_over_budget".into());
        }
        let field = |l: &Value, k: &str| l[k].as_str().map(str::to_string).ok_or_else(|| format!("text_entry_layer_field: {k}"));
        let layers = layers
            .iter()
            .map(|l| {
                let origin = l["origin_px"].as_array().filter(|a| a.len() == 2).ok_or("text_entry_layer_field: origin_px")?;
                Ok(TextLayerRecord {
                    layer_id: field(l, "layer_id")?,
                    story_id: field(l, "story_id")?,
                    origin_px: [origin[0].as_f64().ok_or("text_origin")?, origin[1].as_f64().ok_or("text_origin")?],
                    paragraph_style: field(l, "paragraph_style")?,
                    character_style: field(l, "character_style")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let stories = stories.iter().map(|s| serde_json::from_value(s.clone()).map_err(|e| format!("text_story_json: {e}"))).collect::<Result<Vec<StudioTextStory>, String>>()?;
        let styles: StudioTypeStyleRecords = serde_json::from_value(v["styles"].clone()).map_err(|e| format!("text_styles_json: {e}"))?;
        let entry = TextEntry { schema: ENTRY_SCHEMA.into(), layers, stories, styles };
        entry.validate(doc, cancel)?;
        Ok(entry)
    }

    pub fn validate(&self, doc: &StudioDocument, cancel: &CancellationToken) -> Result<(), String> {
        let budget = folio::Budget::default();
        folio::validate_type_style_records(&self.styles, doc, budget, cancel, &folio::Unresolved).map_err(|d| format!("text_styles: {:?} {}", d.code, d.target))?;
        for story in &self.stories {
            folio::validate_text_story(story, budget, cancel, &StylesOnly(&self.styles)).map_err(|d| format!("text_story: {:?} {}", d.code, d.target))?;
            folio::check_story_style_scopes(story, &self.styles).map_err(|d| format!("text_story_scope: {:?} {}", d.code, d.target))?;
            if story_text(story).is_none_or(|t| t.len() > MAX_STORY_BYTES) {
                return Err(format!("text_story_content: {} (inline text up to {MAX_STORY_BYTES} bytes)", story.story_id));
            }
        }
        for l in &self.layers {
            if !l.origin_px.iter().all(|v| v.is_finite()) {
                return Err(format!("text_origin: {}", l.layer_id));
            }
            if !self.stories.iter().any(|s| s.story_id == l.story_id) || self.styles.style(&l.character_style).is_none() || self.styles.style(&l.paragraph_style).is_none() {
                return Err(format!("text_layer_records: {}", l.layer_id));
            }
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let layers: Vec<Value> = self
            .layers
            .iter()
            .map(|l| json!({"layer_id": l.layer_id, "story_id": l.story_id, "origin_px": l.origin_px, "paragraph_style": l.paragraph_style, "character_style": l.character_style}))
            .collect();
        let v = json!({"schema": self.schema, "layers": layers, "stories": self.stories, "styles": self.styles});
        serde_json::to_vec(&v).map_err(|e| format!("text_entry_json: {e}"))
    }
}

/// Answers type-style lookups from the record set; everything else is absent.
struct StylesOnly<'a>(&'a StudioTypeStyleRecords);
impl folio::Resolver for StylesOnly<'_> {
    fn primitive(&self, schema_id: &str, object_id: &str) -> folio::Resolution {
        if schema_id == hsk_studio_accord::SCHEMA_STUDIO_TYPE_STYLE && self.0.style(object_id).is_some() {
            folio::Resolution::Available
        } else {
            folio::Resolution::Absent
        }
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

pub fn story_text(story: &StudioTextStory) -> Option<&str> {
    match &story.content {
        folio::StoryContent::Inline { text_utf8 } => Some(text_utf8),
        folio::StoryContent::Stored { .. } => None,
    }
}

/// One font the viewer can draw with.
pub struct FontFile {
    pub family: String,
    pub bytes: Vec<u8>,
    pub hash: [u8; 32],
}

impl FontFile {
    pub fn new(family: &str, bytes: Vec<u8>) -> Self {
        let hash = hsk_studio_prism::profile_hash(&bytes);
        Self { family: family.into(), bytes, hash }
    }

    /// egui's bundled `Ubuntu-Light` (TrueType outlines).
    pub fn bundled() -> Option<Self> {
        let defs = egui::FontDefinitions::default();
        let data = defs.font_data.get(DEFAULT_FONT)?;
        Some(Self::new(DEFAULT_FONT, data.font.to_vec()))
    }

    fn resource(&self) -> ty::FontResource<'_> {
        ty::FontResource { identity: &self.family, location: &self.family, content_hash: self.hash, face_index: 0, resource_revision: 1, grant_revision: 1 }
    }
}

// ----- Type caller ports (adapted from composite tests/common/text_shaping.rs) ----------------

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
    files: &'a [FontFile],
    ledger: &'a Ledger,
}
impl ty::FontResolver for Fonts<'_> {
    fn revision(&self) -> Result<u64, ty::Error> {
        Ok(1)
    }
    fn resolve(&self, r: &ty::FontResource<'_>) -> Result<ty::FontRead<'_>, ty::Error> {
        let f = self.files.iter().find(|f| f.family == r.identity).ok_or(ty::Error::MissingFont)?;
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
            "result" => b"harness-text-result",
            _ => b"resolved-style",
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
        input_bytes: 8192,
        input_scalars: 8192,
        styles: 32,
        features: 64,
        axes: 16,
        font_resources: 32,
        font_bytes: 32 << 20,
        parser_tables: 128,
        lookups: 65536,
        runs: 256,
        glyphs: 8192,
        fallback_attempts: 64,
        recursion: 64,
        work_units: 50_000_000,
        requested_owned_bytes: 1 << 40,
        retained_generations: 8,
    }
}

/// What one text layer draws.
struct Draw<'a> {
    layer_id: &'a str,
    text: &'a str,
    font: &'a FontFile,
    size_pt: f64,
    origin: [f64; 2],
    fill: [f32; 4],
}

fn style_of<'a>(styles: &'a StudioTypeStyleRecords, id: &str) -> Result<&'a folio::StudioTypeStyle, String> {
    styles.style(id).ok_or_else(|| format!("text_style_missing: {id}"))
}

/// Shapes and rasterises every text layer of `doc` that the entry describes (layers without a
/// record are left to composite, which refuses them `text_layer_not_prepared`).
pub fn prepare(doc: &StudioDocument, entry: &TextEntry, fonts: &[FontFile], cancel: &CancellationToken) -> Result<PreparedText, String> {
    shape_layers(doc, entry, fonts, cancel, |inputs, resolution| {
        prepare_text(inputs, resolution, cancel).map_err(|e| format!("text_prepare: {e:?}"))
    })
}

/// Text-animator glyphs of every text layer the entry describes (lane SMB-AE-MOTION-RECORDS-2;
/// STU-MOT-090/094): the SAME shaping as [`prepare`] (Type's `TextEngine`, the entry's styles and
/// fonts, one run per paragraph at the record's origin), then composite's `glyphs_from_runs` places
/// each shaped glyph's Type outline at its pen position; the layer fill comes from the character
/// style. Keyed by layer id, ready for `CompositionTemporalSource::with_motion_records`.
#[cfg(feature = "media")]
#[allow(clippy::type_complexity)]
pub fn animated_glyphs(
    doc: &StudioDocument,
    entry: &TextEntry,
    fonts: &[FontFile],
    cancel: &CancellationToken,
) -> Result<std::collections::BTreeMap<String, (Vec<hsk_studio_composite::text::animate::AnimatedGlyph>, [f32; 4])>, String> {
    shape_layers(doc, entry, fonts, cancel, |inputs, resolution| {
        let mut out = std::collections::BTreeMap::new();
        for input in inputs {
            let glyphs = hsk_studio_composite::text::animate::glyphs_from_runs(input.runs, resolution, cancel)
                .map_err(|e| format!("text_animator_glyphs {}: {e:?}", input.layer_id))?;
            out.insert(input.layer_id.to_owned(), (glyphs, input.fill_rgba));
        }
        Ok(out)
    })
}

fn shape_layers<R>(
    doc: &StudioDocument,
    entry: &TextEntry,
    fonts: &[FontFile],
    cancel: &CancellationToken,
    finish: impl FnOnce(&[TextLayerInput<'_>], &TextResolution) -> Result<R, String>,
) -> Result<R, String> {
    let mut draws = Vec::new();
    for layer in &doc.layers {
        let Payload::Text { story_id } = &layer.payload else { continue };
        let Some(record) = entry.layers.iter().find(|r| r.layer_id == layer.layer_id && &r.story_id == story_id) else {
            continue;
        };
        let story = entry.stories.iter().find(|s| &s.story_id == story_id).ok_or_else(|| format!("text_story_missing: {story_id}"))?;
        let text = story_text(story).ok_or("text_story_stored_content_unsupported")?;
        let character = style_of(&entry.styles, &record.character_style)?;
        let family = character.font_family.as_deref().unwrap_or(DEFAULT_FONT);
        let font = fonts.iter().find(|f| f.family == family).ok_or_else(|| format!("text_font_unavailable: {family}"))?;
        let size_pt = character.font_size.as_ref().map_or(Ok(12.0), |l| match l.unit {
            folio::GeometryUnit::Pt | folio::GeometryUnit::Px => Ok(l.value),
            _ => Err(format!("text_font_size_unit: {}", record.character_style)),
        })?;
        let fill = character.fill.as_ref().map_or([0.0, 0.0, 0.0, 1.0], |c| [c.r as f32, c.g as f32, c.b as f32, c.a as f32]);
        draws.push(Draw { layer_id: &layer.layer_id, text, font, size_pt, origin: record.origin_px, fill });
    }
    let resolution = TextResolution::at_ppi(72.0);
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "harness-text").map_err(|_| "text_actor")?;
    let document = DomainId::parse(&doc.document_id).map_err(|_| "text_document_id")?;
    let ledger = Ledger(Mutex::new(LedgerState::default()));
    let resolver = Fonts { files: fonts, ledger: &ledger };
    // Every owned input of every request lives here, so the shaped results borrow one scope.
    let ids: Vec<(DomainId, DomainId, DomainId)> = draws
        .iter()
        .map(|d| {
            let uuid = d.layer_id.get(4..).ok_or("text_layer_id")?;
            Ok((DomainId::parse(d.layer_id).map_err(|_| "text_layer_id")?, DomainId::parse(&format!("STXT{uuid}")).map_err(|_| "text_story_id")?, DomainId::parse(&format!("STYS{uuid}")).map_err(|_| "text_style_id")?))
        })
        .collect::<Result<_, String>>()?;
    let candidates: Vec<[ty::FontResource<'_>; 1]> = draws.iter().map(|d| [d.font.resource()]).collect();
    let styles: Vec<[ty::Style<'_>; 1]> = draws
        .iter()
        .zip(&candidates)
        .map(|(d, c)| {
            [ty::Style {
                read_index: 1,
                range: ty::SourceRange { start: 0, end: d.text.len() as u64 },
                font_size_pt: d.size_pt,
                requested_identity: &d.font.family,
                candidates: c,
                substitution: None,
                script: *b"Latn",
                language: "en",
                character_override: ty::CharacterOverride::Default,
                features: &[],
                axes: &[],
            }]
        })
        .collect();
    let scenes: Vec<Scene<'_>> = draws.iter().zip(&styles).map(|(d, s)| Scene { text: d.text, styles: s }).collect();
    let reads: Vec<[ty::ReadAddress<'_>; 2]> = draws
        .iter()
        .zip(&ids)
        .map(|(d, (_, story, style))| {
            [
                ty::ReadAddress { owner: story, property: "text", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(d.text.as_bytes()) },
                ty::ReadAddress { owner: style, property: "style1", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(b"resolved-style") },
            ]
        })
        .collect();
    let paragraphs: Vec<[ty::ParagraphInput; 1]> = draws
        .iter()
        .map(|d| [ty::ParagraphInput { range: ty::SourceRange { start: 0, end: d.text.len() as u64 }, direction: ty::Direction::LeftToRight, provenance: ty::DirectionProvenance::Explicit }])
        .collect();
    let input_lease = ty::AdmissionPort::reserve(&ledger, 1 << 20, u64::MAX).map_err(|e| format!("text_admission: {e:?}"))?;
    let input = Input(1 << 20);
    let mut shaped = Vec::with_capacity(draws.len());
    for (k, d) in draws.iter().enumerate() {
        let (Some((layer, story, _)), Some(scene), Some(read), Some(paragraph), Some(style)) = (ids.get(k), scenes.get(k), reads.get(k), paragraphs.get(k), styles.get(k)) else {
            return Err("text_internal".into());
        };
        let ports = ty::Ports { context: scene, resolver: &resolver, input_lifetime: &input, admission: &ledger };
        let request = ty::Request {
            document_id: &document,
            document_revision: 1,
            story_id: story,
            layer_id: layer,
            text_utf8: d.text,
            text_sha256: hsk_studio_prism::profile_hash(d.text.as_bytes()),
            text_read_index: 0,
            reads: read,
            result_target: ty::ResultTarget {
                address: ty::ReadAddress { owner: story, property: "result", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(b"harness-text-result") },
                preimage: b"harness-text-result",
            },
            composition_version: ty::COMPOSITION_VERSION,
            normalization: ty::Normalization::Preserve,
            story_direction: ty::StoryDirection::LeftToRight,
            paragraphs: paragraph,
            styles: style,
            resolver_revision: 1,
            cancel_epoch: 1,
            cancellation: cancel,
            actor: &actor,
            correlation_id: "harness-text",
            observation_correlation: 1,
            limits: limits(),
        };
        let prepared = ty::TextEngine.shaped_runs(request, &ports).map_err(|r| format!("text_shaping {}: {:?}", d.layer_id, r.error))?;
        shaped.push(prepared);
    }
    let mut runs_per_layer: Vec<Vec<TextRun<'_>>> = Vec::with_capacity(draws.len());
    for (d, prepared) in draws.iter().zip(&shaped) {
        let mut x = d.origin[0];
        let mut runs = Vec::new();
        for paragraph in prepared.paragraphs() {
            for run in paragraph.runs() {
                runs.push(TextRun { run, font: ty::FontBytes { bytes: &d.font.bytes, resource: run.resolved() }, origin_px: [x, d.origin[1]] });
                let advance: f64 = run.glyphs().iter().map(|g| g.x_advance_pt).sum();
                x += advance * resolution.px_per_pt;
            }
        }
        runs_per_layer.push(runs);
    }
    let inputs: Vec<TextLayerInput<'_>> = draws.iter().zip(&runs_per_layer).map(|(d, runs)| TextLayerInput { layer_id: d.layer_id, runs, fill_rgba: d.fill }).collect();
    let prepared = finish(&inputs, &resolution)?;
    drop(inputs);
    drop(runs_per_layer);
    drop(shaped);
    drop(input_lease);
    Ok(prepared)
}

impl super::Viewer {
    /// Registers a font family for the text tool (TrueType outlines).
    pub fn add_font(&mut self, family: &str, bytes: Vec<u8>) {
        self.fonts.retain(|f| f.family != family);
        self.fonts.push(FontFile::new(family, bytes));
    }

    /// The text records of the open document (empty when it has none).
    pub fn text_entry(&self) -> Result<TextEntry, String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        match self.run.document_entry(ENTRY) {
            Some(bytes) => TextEntry::decode(bytes, snap.document(), self.cancel),
            None => Ok(TextEntry::empty(&snap.document().document_id)),
        }
    }

    /// Shaped and rasterised text of the open document (empty without text layers).
    pub(super) fn prepared_text(&self) -> Result<PreparedText, String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        if !snap.document().layers.iter().any(|l| matches!(l.payload, Payload::Text { .. })) {
            return Ok(PreparedText::default());
        }
        prepare(snap.document(), &self.text_entry()?, &self.fonts, self.cancel)
    }

    /// Places a point-text layer whose first baseline starts at document pixel (`x`, `y`): a Folio
    /// `text` layer created by one chronicle transaction above the selected layer (undoable), its
    /// story and paragraph/character styles (font `font_family`, `text_size_pt`, fill = the brush
    /// colour in linear light) validated by Folio and stored in the document entry `text`. Selects
    /// the layer and returns its id.
    pub fn place_text(&mut self, x: f64, y: f64, text: &str) -> Result<String, String> {
        let result = self.place_text_inner(x, y, text, DEFAULT_FONT);
        if let Err(e) = &result {
            self.say(format!("text: {e}"));
        }
        result
    }

    /// [`Self::place_text`] with an explicit font family (one of the viewer's fonts).
    pub fn place_text_with_font(&mut self, x: f64, y: f64, text: &str, family: &str) -> Result<String, String> {
        let result = self.place_text_inner(x, y, text, family);
        if let Err(e) = &result {
            self.say(format!("text: {e}"));
        }
        result
    }

    fn place_text_inner(&mut self, x: f64, y: f64, text: &str, family: &str) -> Result<String, String> {
        if text.is_empty() || text.len() > MAX_STORY_BYTES || text.contains(['\n', '\r']) {
            return Err(format!("text_content: one line of 1..={MAX_STORY_BYTES} bytes"));
        }
        if !(x.is_finite() && y.is_finite()) {
            return Err("text_origin".into());
        }
        if !self.fonts.iter().any(|f| f.family == family) {
            return Err(format!("text_font_unavailable: {family}"));
        }
        let (parent, place) = self.insertion_point()?;
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let doc = snap.document();
        let id = super::layers::mint_layer_id(doc, u64::try_from(doc.layers.len()).unwrap_or(0));
        let uuid = id.get(5..).ok_or("text_layer_id")?;
        let story_id = format!("STXT-{uuid}");
        let paragraph_style = format!("STYS-{uuid}");
        // Same uuid with the other RFC variant nibble: a second, distinct style id.
        let character_style = format!("STYS-{}", uuid.replacen("-9000-", "-a000-", 1));
        let profile = serde_json::to_value(&doc.working_space_rgb).unwrap_or(Value::Null);
        let [r, g, b] = self.edit.brush.rgb8.map(|v| {
            let c = f64::from(v) / 255.0;
            if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        });
        let len = text.len();
        let story: StudioTextStory = serde_json::from_value(json!({
            "schema_id": "hsk.studio.text_story@1", "story_id": story_id, "text_kind": "point", "story_direction": "left_to_right",
            "content": {"content_kind": "inline", "text_utf8": text},
            "paragraphs": [{"start": 0, "end": len, "style_ref": paragraph_style, "paragraph_direction": null}],
            "character_ranges": [{"start": 0, "end": len, "style_ref": character_style, "applied_language": "en"}],
        }))
        .map_err(|e| format!("text_story: {e}"))?;
        let styles: Vec<folio::StudioTypeStyle> = serde_json::from_value(json!([
            {"schema_id": "hsk.studio.type_style@1", "style_id": paragraph_style, "scope": "paragraph", "name": "Text/Paragraph", "align": "left"},
            {"schema_id": "hsk.studio.type_style@1", "style_id": character_style, "scope": "character", "name": "Text/Character",
             "font_family": family, "font_size": {"value": self.text_size_pt, "unit": "pt"},
             "fill": {"r": r, "g": g, "b": b, "a": 1.0, "colour_profile_id": profile}},
        ]))
        .map_err(|e| format!("text_style: {e}"))?;
        let mut entry = self.text_entry()?;
        entry.stories.retain(|s| s.story_id != story_id);
        entry.stories.push(story);
        entry.styles.styles.retain(|s| s.style_id != paragraph_style && s.style_id != character_style);
        entry.styles.styles.extend(styles);
        entry.layers.retain(|l| l.layer_id != id);
        entry.layers.push(TextLayerRecord { layer_id: id.clone(), story_id: story_id.clone(), origin_px: [x, y], paragraph_style, character_style });
        entry.validate(doc, self.cancel)?;
        let bytes = entry.to_bytes()?;
        let name: String = text.chars().take(32).collect();
        let layer = super::layers::new_layer(&id, &name, &parent, "text", json!({"payload_kind": "text", "story_id": story_id}))?;
        // An image-typed source (Folio G1): the text layer composites in any stack.
        let operation = super::layers::operation_for(&id, false)?;
        // Records first (the layer then renders at once), then the undoable layer creation.
        self.run.set_document_entry(ENTRY, Some(bytes)).map_err(|e| format!("text_entry: {e}"))?;
        let write = crate::PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((layer, operation)) };
        self.structure("place_text", vec![write], format!("text layer {id}"))?;
        self.selected = Some(id.clone());
        Ok(id)
    }
}
