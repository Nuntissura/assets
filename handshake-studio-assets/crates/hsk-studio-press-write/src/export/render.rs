//! Render-back of one page of a PDF (typically one `PdfExport` wrote) into an RGB raster, through
//! hayro-interpret 0.7.0 and the crate's paint device (`paint.rs`): the same third-party interpreter
//! the redaction flatten uses, independent of the writer's code. It proves drawability by one
//! interpreter, not conformance (STU-PDF-002). Bounds (V4-02): the canvas is checked against
//! `Limits::max_flatten_pixels` before allocation; image decoding is charged to a budget of 64
//! canvases; the interpreter runs on its own thread with a `RENDER_STACK_BYTES` stack, and a panic
//! there is a typed refusal. Tests: `tests/export.rs` `render_back_reads_the_export`.
#![deny(clippy::arithmetic_side_effects)]
use super::ExportError;
use crate::error::Limits;
use crate::flatten::RENDER_STACK_BYTES;
use crate::paint::Painter;
use crate::pdf_edit::PdfDoc;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::{Context, InterpreterCache, InterpreterSettings, InterpreterWarning, interpret_page};
use crate::syntax::Object;
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeSet;
use kurbo::Affine;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// An RGB raster of one page's media box, rows top to bottom, opaque white where nothing draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes.
    pub rgb: Vec<u8>,
    /// hayro `UnsupportedFont` warnings (a font it could not read and substituted or skipped).
    pub font_warnings: u32,
    /// hayro `ImageDecodeFailure` warnings.
    pub image_warnings: u32,
    /// What the paint device did not draw or approximated (`FlattenNote::as_str`, sorted), e.g.
    /// `soft_mask` (content under a soft mask is skipped) or `blend_mode_approximated`.
    pub notes: Vec<&'static str>,
}

impl RenderedPage {
    /// The pixel at device `(x, y)` (y down), `None` outside the raster.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = usize::try_from(u64::from(y).checked_mul(u64::from(self.width))?.checked_add(u64::from(x))?.checked_mul(3)?).ok()?;
        let s = self.rgb.get(i..i.checked_add(3)?)?;
        Some([s[0], s[1], s[2]])
    }
}

/// The `/I` and `/K` flags of the document's transparency groups (soft-mask groups excluded): the
/// interpreter reports groups without them, so one pair is given to the paint device. When the groups
/// disagree the ISO defaults (non-isolated, non-knockout) are used and `group_flags_mixed` is noted.
fn group_flags(doc: &PdfDoc) -> ((bool, bool), bool) {
    let mut mask_groups = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for e in doc.seam_objects().values() {
        let mut stack: Vec<&Object> = vec![&e.obj];
        let mut visited = 0usize;
        while let Some(o) = stack.pop() {
            visited = visited.saturating_add(1);
            if visited > 1 << 16 {
                break;
            }
            match o {
                Object::Array(a) => stack.extend(a.iter()),
                Object::Dict(_) | Object::Stream(_) => {
                    let Some(d) = o.as_dict() else { continue };
                    let is_mask = d.get("S").is_some_and(|s| s.is_name("Luminosity") || s.is_name("Alpha"));
                    if is_mask && let Some(r) = d.get("G").and_then(Object::as_ref) {
                        mask_groups.insert(r.num);
                    }
                    stack.extend(d.entries.iter().map(|(_, v)| v));
                }
                _ => {}
            }
        }
    }
    for (num, e) in doc.seam_objects() {
        let Object::Stream(s) = &e.obj else { continue };
        let Some(g) = s.dict.get("Group").map(|g| doc.resolve(g)).and_then(Object::as_dict) else { continue };
        if !g.get("S").is_some_and(|v| v.is_name("Transparency")) || mask_groups.contains(num) {
            continue;
        }
        let flag = |k: &str| matches!(g.get(k), Some(Object::Bool(true)));
        seen.insert((flag("I"), flag("K")));
    }
    match seen.len() {
        0 => ((false, false), false),
        1 => (seen.into_iter().next().unwrap_or((false, false)), false),
        _ => ((false, false), true),
    }
}

/// Renders page `index` (0-based) at `px_per_pt` device pixels per point. Refusals: `NoSuchPage`,
/// `NonFinite` / `ValueOutOfRange` for the scale, `RenderOverBudget` for a canvas over
/// `max_flatten_pixels`, `RenderRefused` when the interpreter cannot open the file or panics,
/// `Press(..)` when the crate's reader refuses the file.
pub fn render_page(bytes: &[u8], index: usize, px_per_pt: f64, limits: &Limits, cancel: &CancellationToken) -> Result<RenderedPage, ExportError> {
    if !px_per_pt.is_finite() {
        return Err(ExportError::NonFinite);
    }
    if !(px_per_pt > 0.0 && px_per_pt <= 100.0) {
        return Err(ExportError::ValueOutOfRange);
    }
    let doc = PdfDoc::open(bytes, limits, cancel)?;
    let (flags, mixed) = group_flags(&doc);
    let pages = doc.proposal(cancel)?.pages;
    let mb = pages.get(index).ok_or(ExportError::NoSuchPage)?.media_box.ok_or(ExportError::InvalidBox)?;
    let (w_pt, h_pt) = (mb[2] - mb[0], mb[3] - mb[1]);
    let (w, h) = ((w_pt * px_per_pt).ceil(), (h_pt * px_per_pt).ceil());
    if !(w >= 1.0 && h >= 1.0 && w <= f64::from(u32::MAX) && h <= f64::from(u32::MAX) && w * h <= limits.max_flatten_pixels as f64) {
        return Err(ExportError::RenderOverBudget);
    }
    let (wu, hu) = (w as u32, h as u32);
    let canvas = u64::from(wu).checked_mul(u64::from(hu)).ok_or(ExportError::RenderOverBudget)?;
    let decode_budget = canvas.saturating_mul(64);
    let device = Affine::new([px_per_pt, 0.0, 0.0, -px_per_pt, -mb[0] * px_per_pt, mb[3] * px_per_pt]);
    let data = bytes.to_vec();
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new().name("hsk-press-render-back".into()).stack_size(RENDER_STACK_BYTES).spawn_scoped(
            scope,
            move || -> Result<RenderedPage, ExportError> {
                let fonts = Arc::new(AtomicU32::new(0));
                let images = Arc::new(AtomicU32::new(0));
                let (f2, i2) = (fonts.clone(), images.clone());
                let pdf = Pdf::new(data).map_err(|_| ExportError::RenderRefused)?;
                let pages = pdf.pages();
                let page = pages.get(index).ok_or(ExportError::NoSuchPage)?;
                let settings = InterpreterSettings {
                    render_annotations: false,
                    warning_sink: Arc::new(move |warning| {
                        let counter = match warning {
                            InterpreterWarning::UnsupportedFont => &f2,
                            InterpreterWarning::ImageDecodeFailure => &i2,
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                    }),
                    ..InterpreterSettings::default()
                };
                let cache = InterpreterCache::new();
                let rect = kurbo::Rect::new(0.0, 0.0, f64::from(wu), f64::from(hu));
                let mut ctx = Context::new(device, rect, &cache, pdf.xref(), settings);
                let (pw, ph) = (usize::try_from(wu).map_err(|_| ExportError::RenderOverBudget)?, usize::try_from(hu).map_err(|_| ExportError::RenderOverBudget)?);
                let mut painter = Painter::new(pw, ph, decode_budget, cancel).ok_or(ExportError::RenderOverBudget)?;
                painter.group_flags = flags;
                interpret_page(page, &mut ctx, &mut painter);
                if painter.canceled {
                    return Err(ExportError::Canceled);
                }
                Ok(RenderedPage {
                    width: wu,
                    height: hu,
                    rgb: std::mem::take(&mut painter.px),
                    font_warnings: fonts.load(Ordering::Relaxed),
                    image_warnings: images.load(Ordering::Relaxed),
                    notes: painter.notes.iter().map(|n| n.as_str()).chain(mixed.then_some("group_flags_mixed")).collect(),
                })
            },
        );
        match spawned {
            Ok(handle) => handle.join().unwrap_or(Err(ExportError::RenderRefused)),
            Err(_) => Err(ExportError::RenderRefused),
        }
    })
}
