//! Global swatches as Folio records (lane SMB-ILLUSTRATOR-TOOLS-2; STU-COL-170..174, STU-VEC-019,
//! STU-VEC-051).
//!
//! A swatch is a Folio `StudioSwatch` in the entry's vector records: `process` or `global` colour
//! (RGB, CMYK, LAB or GRAY components), `spot` (ink name, measured Lab, alternate space and value),
//! `tint` (a percent of a colour swatch) or `gradient`. Applying a swatch binds the layer's fill or
//! stroke row to it (`StudioSwatchPaint`, saved with the document, so the link persists) and writes
//! the resolved colour into the row as its stand-in paint. Editing a swatch rewrites every bound
//! row's stand-in (and those of tints of it) in ONE undo step; detaching drops the binding and
//! keeps the colour. Each swatch has a companion PAINT style (`SSTY-<swatch uuid>`, one fill row
//! bound to it) that network regions name as their region fill style.
//!
//! Colour resolution (into the harness working space, encoded sRGB): RGB as is; GRAY `g` as
//! `(g, g, g)` (Studio rule); LAB through Prism (`WideSource::Lab`, D50 PCS, relative
//! colorimetric) and CMYK through Prism with the document's CMYK profile (lane
//! SMB-ILLUSTRATOR-INTEROP: from Folio — the swatch's `profile_ref`, else the document's
//! `working_space_cmyk`, else the `StudioIccProfile` marked as the CMYK working space; its bytes
//! travel in the entry keyed by the record's sha256; a CMYK swatch without one is refused), both
//! into the CC0 sRGB v4 profile and clipped to 0..1. A spot uses its measured Lab,
//! else its alternate, else it is refused (no process approximation). Tints scale toward paper in
//! the source model (Studio rule): RGB/GRAY `1 - t (1 - c)`, CMYK `t c`, LAB `L = 100 - t (100 - L)`,
//! `a, b = t a, t b`; a per-use tint applies on top. Illustrator/InDesign parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use super::style::{PaintSpec, RowKind, SWATCH_KEY, StrokePanel, fill_row, folio_paint, node_style, paint_style, stroke_row};
use super::VectorEntry;
use hsk_studio_folio::{self as folio, StudioAppearanceRow, StudioColorModel, StudioDesignRecords, StudioPaint, StudioSwatch, StudioSwatchKind};

/// Largest ICC profile the entry carries.
pub const MAX_ICC_BYTES: usize = 8 << 20;

/// A swatch colour definition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwatchColour {
    Rgb([f64; 3]),
    Cmyk([f64; 4]),
    Lab([f64; 3]),
    Gray(f64),
}

impl SwatchColour {
    fn split(self) -> (StudioColorModel, Vec<f64>) {
        match self {
            Self::Rgb(c) => (StudioColorModel::Rgb, c.to_vec()),
            Self::Cmyk(c) => (StudioColorModel::Cmyk, c.to_vec()),
            Self::Lab(c) => (StudioColorModel::Lab, c.to_vec()),
            Self::Gray(g) => (StudioColorModel::Gray, vec![g]),
        }
    }
}

/// What a new or edited swatch is.
#[derive(Clone, Debug, PartialEq)]
pub enum SwatchSpec {
    Process(SwatchColour),
    Global(SwatchColour),
    Spot { ink: String, measured_lab: Option<[f64; 3]>, alternate: Option<SwatchColour> },
    /// A percent (0..=100) of colour swatch `base`.
    Tint { base: String, percent: f64 },
    /// A linear or radial gradient ([`PaintSpec`], document px).
    Gradient(PaintSpec),
}

/// Silent sink for the Prism frames (the harness records its own operation).
struct Quiet;
impl hsk_studio_observe::SinkPort for Quiet {
    fn try_send(&mut self, _class: hsk_studio_observe::DeliveryClass, _bytes: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
        Ok(())
    }
}

/// One colour through Prism into encoded sRGB (CMYK with `icc`, else Lab).
fn prism_rgb(icc: Option<&[u8]>, pixel: &[f32]) -> Result<[f64; 3], String> {
    use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
    use hsk_studio_prism::{Intent, ProfileInput, WideEngine, WideSource, WideTransformRequest, profile_hash};
    let fail = |c: &str| format!("swatch_convert: {c}");
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "harness-swatch").map_err(|_| fail("actor"))?;
    let destination_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000a001").map_err(|_| fail("id"))?;
    let source_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000a002").map_err(|_| fail("id"))?;
    let resource = DomainId::parse("SSWT-019abcde-0000-7000-8000-00000000a003").map_err(|_| fail("id"))?;
    let srgb = super::super::develop::SRGB_V4_ICC;
    let destination = ProfileInput { profile_id: &destination_id, bytes: srgb, expected_sha256: profile_hash(srgb) };
    let source = match icc {
        Some(bytes) => WideSource::Profile(ProfileInput { profile_id: &source_id, bytes, expected_sha256: profile_hash(bytes) }),
        None => WideSource::Lab,
    };
    let mut engine = WideEngine::new(actor.clone(), 2).map_err(|e| fail(e.code()))?;
    let request = WideTransformRequest { source, destination, pixels: pixel, intent: Intent::RelativeColorimetric, black_point_compensation: false, revision: 0, expected_revision: 0, correlation_id: 1, resource_id: &resource, actor: &actor };
    let out = engine.transform(&request, &CancellationToken::default(), &mut Quiet).map_err(|f| fail(f.error.code()))?;
    match out.pixels.as_slice() {
        [r, g, b, ..] => Ok([f64::from(*r).clamp(0.0, 1.0), f64::from(*g).clamp(0.0, 1.0), f64::from(*b).clamp(0.0, 1.0)]),
        _ => Err(fail("short_output")),
    }
}

/// Components tinted toward paper by `t` (0..=1) in their own model.
fn tinted(model: StudioColorModel, c: &[f64], t: f64) -> Vec<f64> {
    c.iter()
        .enumerate()
        .map(|(i, v)| match model {
            StudioColorModel::Rgb | StudioColorModel::Gray => 1.0 - t * (1.0 - v),
            StudioColorModel::Cmyk => t * v,
            StudioColorModel::Lab if i == 0 => 100.0 - t * (100.0 - v),
            StudioColorModel::Lab => t * v,
        })
        .collect()
}

/// The CMYK profile bytes for a swatch profile reference (module docs).
pub(super) fn cmyk_bytes(entry: &VectorEntry, doc: &folio::StudioDocument, profile_ref: Option<&str>) -> Option<Vec<u8>> {
    let id = profile_ref
        .map(str::to_string)
        .or_else(|| match &doc.working_space_cmyk {
            folio::ProfileBinding::Assigned(id) => Some(id.clone()),
            folio::ProfileBinding::Unassigned(()) => None,
        })
        .or_else(|| entry.records.working_icc(StudioColorModel::Cmyk).map(|p| p.profile_id.clone()))?;
    let record = entry.records.icc_profile(&id).filter(|p| p.colour_space == StudioColorModel::Cmyk)?;
    entry.icc.get(&record.content_digest.digest).cloned()
}

/// The source model and components of a colour swatch (tints resolved, `extra` tint on top) and
/// the profile reference of the swatch whose components they are.
fn source(records: &folio::StudioVectorRecords, s: &StudioSwatch, extra: f64) -> Result<(StudioColorModel, Vec<f64>, Option<String>), String> {
    let (model, comps, t, profile) = match s.swatch_kind {
        StudioSwatchKind::Process | StudioSwatchKind::Global => (s.color_model.ok_or("swatch: colour model")?, s.components.clone(), 1.0, s.profile_ref.clone()),
        StudioSwatchKind::Spot => {
            let ink = s.spot.as_ref().ok_or("swatch: spot ink")?;
            match (ink.measured_lab, ink.alternate_space) {
                (Some(lab), _) => (StudioColorModel::Lab, lab.to_vec(), 1.0, None),
                (None, Some(m)) => (m, ink.alternate_color_value.clone(), 1.0, s.profile_ref.clone()),
                (None, None) => return Err(format!("swatch: spot {} has no process approximation", ink.ink_name)),
            }
        }
        StudioSwatchKind::Tint => {
            let base = records.swatch(s.base_swatch_ref.as_deref().unwrap_or("")).ok_or("swatch: tint base")?;
            let (m, c, profile) = source(records, base, 1.0)?;
            (m, c, s.tint_value.unwrap_or(100.0) / 100.0, profile)
        }
        StudioSwatchKind::Gradient => return Err("swatch: a gradient swatch has no single colour".into()),
    };
    Ok((model, tinted(model, &comps, t * extra), profile))
}

/// The resolved encoded-sRGB colour of a colour swatch.
fn resolve_rgb(entry: &VectorEntry, doc: &folio::StudioDocument, s: &StudioSwatch, extra: f64) -> Result<[f64; 3], String> {
    let (model, c, profile) = source(&entry.records, s, extra)?;
    let f = |v: &[f64]| -> Vec<f32> { v.iter().map(|x| *x as f32).collect() };
    match model {
        StudioColorModel::Rgb => Ok([c[0], c[1], c[2]]),
        StudioColorModel::Gray => Ok([c[0], c[0], c[0]]),
        StudioColorModel::Lab => prism_rgb(None, &f(&c)),
        StudioColorModel::Cmyk => {
            let icc = cmyk_bytes(entry, doc, profile.as_deref()).ok_or("swatch: the document has no CMYK profile (set_document_cmyk_profile)")?;
            prism_rgb(Some(&icc), &f(&c))
        }
    }
}

/// The stand-in paint of a swatch for one binding (`tint` percent per use).
pub(super) fn stand_in(entry: &VectorEntry, doc: &folio::StudioDocument, swatch_id: &str, tint: Option<f64>) -> Result<StudioPaint, String> {
    let s = entry.records.swatch(swatch_id).ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
    if s.swatch_kind == StudioSwatchKind::Gradient {
        let g = s.gradient.clone().ok_or("swatch: gradient")?;
        return Ok(StudioPaint::Gradient { gradient: g });
    }
    let [r, g, b] = resolve_rgb(entry, doc, s, tint.unwrap_or(100.0) / 100.0)?;
    Ok(StudioPaint::Solid { colour: folio::StudioColor { r, g, b, colour_profile_id: folio::ProfileBinding::Unassigned(()) } })
}

/// Rewrites the stand-in paint of every row bound to `swatch_id` or to a tint of it.
fn propagate(entry: &mut VectorEntry, doc: &folio::StudioDocument, swatch_id: &str) -> Result<(), String> {
    let tints: Vec<String> = entry.records.swatches.iter().filter(|s| s.base_swatch_ref.as_deref() == Some(swatch_id)).map(|s| s.swatch_id.clone()).collect();
    let bound: Vec<folio::StudioSwatchPaint> = entry.records.swatch_paints.iter().filter(|p| p.swatch_id == swatch_id || tints.contains(&p.swatch_id)).cloned().collect();
    for p in bound {
        let paint = stand_in(entry, doc, &p.swatch_id, p.tint)?;
        let design = entry.design.as_mut().ok_or("swatch: design")?;
        let Some(style) = design.paint_styles.iter_mut().find(|s| s.style_id == p.style_id) else { continue };
        if let Some(row) = usize::try_from(p.row).ok().and_then(|i| style.rows.get_mut(i)) {
            match row {
                StudioAppearanceRow::Fill { paint: q, .. } | StudioAppearanceRow::Stroke { paint: q, .. } => q.clone_from(&paint),
            }
        }
        super::style::validate_style(style)?;
    }
    Ok(())
}

fn record(doc_id: &str, n: u64, name: &str, spec: &SwatchSpec, swatch_id: Option<&str>) -> Result<StudioSwatch, String> {
    let uuid = doc_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let id = swatch_id.map_or_else(|| format!("SSWT-{high}-{low}-7ede-8000-{n:012x}"), str::to_string);
    let mut s = StudioSwatch {
        schema_id: folio::SwatchSchema::V1,
        swatch_id: id,
        name: name.to_string(),
        swatch_kind: StudioSwatchKind::Process,
        color_model: None,
        components: Vec::new(),
        profile_ref: None,
        spot: None,
        base_swatch_ref: None,
        tint_value: None,
        gradient: None,
        group_ref: None,
    };
    match spec {
        SwatchSpec::Process(c) | SwatchSpec::Global(c) => {
            s.swatch_kind = if matches!(spec, SwatchSpec::Global(_)) { StudioSwatchKind::Global } else { StudioSwatchKind::Process };
            let (m, v) = c.split();
            s.color_model = Some(m);
            s.components = v;
        }
        SwatchSpec::Spot { ink, measured_lab, alternate } => {
            s.swatch_kind = StudioSwatchKind::Spot;
            let (alternate_space, alternate_color_value) = alternate.map_or((None, Vec::new()), |a| {
                let (m, v) = a.split();
                (Some(m), v)
            });
            s.spot = Some(folio::StudioSpotInk { ink_name: ink.clone(), measured_lab: *measured_lab, alternate_space, alternate_color_value });
        }
        SwatchSpec::Tint { base, percent } => {
            s.swatch_kind = StudioSwatchKind::Tint;
            s.base_swatch_ref = Some(base.clone());
            s.tint_value = Some(*percent);
        }
        SwatchSpec::Gradient(p) => {
            s.swatch_kind = StudioSwatchKind::Gradient;
            match folio_paint(p)? {
                StudioPaint::Gradient { gradient } => s.gradient = Some(gradient),
                StudioPaint::Solid { .. } => return Err("swatch: a gradient swatch needs a gradient paint".into()),
            }
        }
    }
    Ok(s)
}

/// The paint spec of the compatibility API as a swatch spec (solid = global RGB).
fn compat(paint: &PaintSpec) -> SwatchSpec {
    match paint {
        PaintSpec::Solid(c) => SwatchSpec::Global(SwatchColour::Rgb([f64::from(c[0]) / 255.0, f64::from(c[1]) / 255.0, f64::from(c[2]) / 255.0])),
        g => SwatchSpec::Gradient(g.clone()),
    }
}

impl Viewer {
    /// Swatches of the document: `(SSWT id, name)`, in creation order.
    pub fn swatches(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self.vector_entry()?.records.swatches.iter().map(|s| (s.swatch_id.clone(), s.name.clone())).collect())
    }

    /// The Folio swatch record `swatch_id`.
    pub fn swatch(&self, swatch_id: &str) -> Result<StudioSwatch, String> {
        self.vector_entry()?.records.swatch(swatch_id).cloned().ok_or_else(|| format!("swatch: unknown {swatch_id}"))
    }

    /// The resolved encoded-sRGB colour of a colour swatch (module docs).
    pub fn swatch_colour(&self, swatch_id: &str) -> Result<[f64; 3], String> {
        let entry = self.vector_entry()?;
        let s = entry.records.swatch(swatch_id).ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
        resolve_rgb(&entry, self.vdoc()?, s, 1.0)
    }

    /// Creates a global RGB swatch (solid paint) or a gradient swatch: ONE undo step. Returns its
    /// `SSWT` id.
    pub fn add_swatch(&mut self, name: &str, paint: &PaintSpec) -> Result<String, String> {
        self.add_folio_swatch(name, &compat(paint))
    }

    /// Creates a swatch (module docs) and its companion PAINT style: ONE undo step. Returns its
    /// `SSWT` id.
    pub fn add_folio_swatch(&mut self, name: &str, spec: &SwatchSpec) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            if entry.records.swatches.len() >= folio::TOOLS_MAX_ENTRIES {
                return Err("swatch: budget".into());
            }
            let mut n = 0_u64;
            let mut s = record(&doc.document_id, n, name, spec, None)?;
            while entry.records.swatch(&s.swatch_id).is_some() {
                n = n.checked_add(1).ok_or("swatch: id space")?;
                s = record(&doc.document_id, n, name, spec, None)?;
            }
            let id = s.swatch_id.clone();
            entry.records.swatches.push(s);
            let paint = stand_in(&entry, doc, &id, None)?;
            let companion = companion_id(&id)?;
            let design = entry.design.get_or_insert_with(|| StudioDesignRecords::new(doc.document_id.clone()));
            design.paint_styles.push(paint_style(&companion, name, &format!("{SWATCH_KEY}{n}"), &[fill_row(paint)])?);
            entry.records.swatch_paints.push(folio::StudioSwatchPaint { style_id: companion, row: 0, swatch_id: id.clone(), tint: None });
            self.validate_records(&entry)?;
            self.commit_entry("add_swatch", &id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Edits swatch `swatch_id` (solid = RGB colour, else a gradient): every bound row follows in
    /// ONE undo step.
    pub fn edit_swatch(&mut self, swatch_id: &str, paint: &PaintSpec) -> Result<String, String> {
        self.edit_folio_swatch(swatch_id, &compat(paint))
    }

    /// Edits swatch `swatch_id` to `spec` (its kind may change; its name stays): every row bound
    /// to it or to a tint of it follows (global propagation, STU-COL-172) in ONE undo step.
    pub fn edit_folio_swatch(&mut self, swatch_id: &str, spec: &SwatchSpec) -> Result<String, String> {
        let r = (|| {
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let i = entry.records.swatches.iter().position(|s| s.swatch_id == swatch_id).ok_or_else(|| format!("swatch: unknown {swatch_id}"))?;
            let name = entry.records.swatches[i].name.clone();
            entry.records.swatches[i] = record(&doc.document_id, 0, &name, spec, Some(swatch_id))?;
            propagate(&mut entry, doc, swatch_id)?;
            self.validate_records(&entry)?;
            self.commit_entry("edit_swatch", swatch_id, &entry)?;
            Ok(swatch_id.to_string())
        })();
        self.report(r)
    }

    /// Applies swatch `swatch_id` to the `kind` row of `layer_id` (a layer without a stroke gets
    /// one of its 8-bit width, else 1 px) with an optional per-use tint percent, and binds them:
    /// ONE undo step.
    pub fn apply_swatch(&mut self, layer_id: &str, swatch_id: &str, kind: RowKind) -> Result<String, String> {
        self.apply_swatch_tinted(layer_id, swatch_id, kind, None)
    }

    /// [`Viewer::apply_swatch`] with a per-use tint percent (0..=100).
    pub fn apply_swatch_tinted(&mut self, layer_id: &str, swatch_id: &str, kind: RowKind, tint: Option<f64>) -> Result<String, String> {
        let r = (|| {
            if tint.is_some_and(|t| !(t.is_finite() && (0.0..=100.0).contains(&t))) {
                return Err("swatch: tint percent".into());
            }
            let entry = self.vector_entry()?;
            let paint = stand_in(&entry, self.vdoc()?, swatch_id, tint)?;
            let width = self.vector_path(layer_id)?.style.stroke_width;
            let bind = super::style::Bind::Swatch { kind, swatch_id: swatch_id.to_string(), tint };
            self.restyle("apply_swatch", layer_id, Some(bind), |rows, unit| {
                match kind {
                    RowKind::Fill => {
                        rows.retain(|r| !matches!(r, StudioAppearanceRow::Fill { .. }));
                        rows.insert(0, fill_row(paint.clone()));
                    }
                    RowKind::Stroke => {
                        if !rows.iter().any(|r| matches!(r, StudioAppearanceRow::Stroke { .. })) {
                            let weight = if width > 0.0 { width } else { 1.0 };
                            rows.push(stroke_row(&StrokePanel::solid([0, 0, 0], weight), unit)?);
                        }
                        for r in rows.iter_mut() {
                            if let StudioAppearanceRow::Stroke { paint: q, .. } = r {
                                q.clone_from(&paint);
                            }
                        }
                    }
                }
                Ok(Some(kind))
            })
        })();
        self.report(r)
    }

    /// Drops the swatch binding of the `kind` row of `layer_id` (its colour stays as a local
    /// copy): ONE undo step.
    pub fn detach_swatch(&mut self, layer_id: &str, kind: RowKind) -> Result<String, String> {
        let r = (|| {
            let mut entry = self.vector_entry()?;
            let style = entry.design.as_ref().and_then(|d| node_style(d, layer_id)).ok_or_else(|| format!("swatch: {layer_id} is not linked"))?;
            let (style_id, rows) = (style.style_id.clone(), style.rows.clone());
            let before = entry.records.swatch_paints.len();
            entry.records.swatch_paints.retain(|p| {
                let row_kind = usize::try_from(p.row).ok().and_then(|i| rows.get(i)).map(|r| match r {
                    StudioAppearanceRow::Fill { .. } => RowKind::Fill,
                    StudioAppearanceRow::Stroke { .. } => RowKind::Stroke,
                });
                !(p.style_id == style_id && row_kind == Some(kind))
            });
            if entry.records.swatch_paints.len() == before {
                return Err(format!("swatch: {layer_id} {} is not linked", kind.token()));
            }
            self.commit_entry("detach_swatch", layer_id, &entry)?;
            Ok(layer_id.to_string())
        })();
        self.report(r)
    }

    /// Makes `icc` (a CMYK ICC profile) the document's CMYK working profile for swatches: a Folio
    /// `StudioIccProfile` record (new `SCPF` id, sha256 of the bytes, `working_space`) and the bytes in
    /// the entry; every CMYK swatch without its own profile re-resolves (ONE undo step). Prism must
    /// admit the profile. Returns the `SCPF` id.
    pub fn set_document_cmyk_profile(&mut self, name: &str, icc: &[u8]) -> Result<String, String> {
        let r = (|| {
            if icc.is_empty() || icc.len() > MAX_ICC_BYTES {
                return Err("cmyk_profile: size".into());
            }
            // Prism admits it (a CMYK device profile it can convert from).
            prism_rgb(Some(icc), &[0.0, 0.0, 0.0, 0.0])?;
            let doc = self.vdoc()?;
            let mut entry = self.vector_entry()?;
            let digest = hsk_studio_prism::hash_hex(&hsk_studio_prism::profile_hash(icc));
            let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
            let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
            let mut n = 0_u64;
            let id = loop {
                let id = format!("SCPF-{high}-{low}-7ede-8000-{n:012x}");
                if entry.records.icc_profile(&id).is_none() {
                    break id;
                }
                n = n.checked_add(1).ok_or("cmyk_profile: id space")?;
            };
            for p in entry.records.icc_profiles.iter_mut().filter(|p| p.colour_space == StudioColorModel::Cmyk) {
                p.working_space = false;
            }
            entry.records.icc_profiles.push(folio::StudioIccProfile { profile_id: id.clone(), name: name.to_string(), colour_space: StudioColorModel::Cmyk, content_digest: folio::ContentDigest { algorithm: "sha256".into(), digest: digest.clone() }, working_space: true });
            entry.icc.insert(digest, icc.to_vec());
            let cmyk: Vec<String> = entry.records.swatches.iter().filter(|s| s.color_model == Some(StudioColorModel::Cmyk) || s.spot.as_ref().is_some_and(|i| i.measured_lab.is_none() && i.alternate_space == Some(StudioColorModel::Cmyk))).map(|s| s.swatch_id.clone()).collect();
            for sw in cmyk {
                propagate(&mut entry, doc, &sw)?;
            }
            self.validate_records(&entry)?;
            self.commit_entry("set_document_cmyk_profile", &id, &entry)?;
            Ok(id)
        })();
        self.report(r)
    }

    /// Folio validation of the entry's records against the open document (applicable members).
    pub(super) fn validate_records(&self, entry: &VectorEntry) -> Result<(), String> {
        entry.validate_records(self.vdoc()?, &hsk_studio_accord::CancellationToken::default())
    }
}

/// The companion PAINT style id of swatch `swatch_id` (`SSTY-<swatch uuid>`).
pub fn companion_id(swatch_id: &str) -> Result<String, String> {
    let uuid = swatch_id.strip_prefix("SSWT-").ok_or_else(|| format!("swatch: id {swatch_id}"))?;
    Ok(format!("SSTY-{uuid}"))
}
