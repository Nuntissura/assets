//! The Vulkan UI pass (SMB-VK-UI-PASS-1): kumihimo-ui's display list, all 11 primitives, drawn on the owner's device
//! into a pane from offline SPIR-V (decision O3: `shaders/ui.wgsl` compiled by `spv-build`, checked in). The
//! [`Recorder`] is plain data (no device): it computes every per-primitive value with the CPU painter's own
//! expressions (kumihimo-ui paint.rs) and plans instances, batches, atlas uploads and pass breaks; [`UiPass`] runs the
//! plan on the owner's Core and submitter, one submit with a bounded fence wait per pass. No ash type crosses this API.
//!
//! Colour: the pane is R8G8B8A8_UNORM and blending is "over" in encoded space, as the painter blends (never an _SRGB
//! format or view). Scissor: every quad is intersected with the painter's clip stack and the pane on the CPU, so no
//! coordinate beyond the pane reaches the GPU; the dynamic scissor is the pane.
use crate::atlas::{Atlas, Placed, Slot};
use crate::owner::{Core, GpuClientPort, HostBuffer, Owner, PaneId, color_layers, color_range, image_barrier, vk_refusal};
use crate::refusal::Refusal;
use ash::vk;
use kumihimo_ui::display::{DisplayList, IconId, ImageId, Prim, Side};
use kumihimo_ui::geom::{IRect, Point, Rgba8, Scale};
use kumihimo_ui::paint::{IconMask, Resources};
use kumihimo_ui::text::{GlyphKey, Text};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Instance kinds (the shader's `kind`).
pub mod kind {
    pub const FLAT: u32 = 0;
    pub const CHECKER: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const GLYPH: u32 = 3;
    pub const ICON: u32 = 4;
    pub const RRECT: u32 = 5;
    pub const LINE: u32 = 6;
    pub const DASH: u32 = 7;
    pub const SHADOW: u32 = 8;
    pub const RAMP: u32 = 9;
}

/// One instanced quad: 4 vertices, no index buffer. `quad` is already intersected with the clip and the pane.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Instance {
    /// x0, y0, x1, y1 in physical px.
    pub quad: [i32; 4],
    pub kind: u32,
    /// Kind-specific selector (icon channel, border or fill, dash with an off colour, checker cell).
    pub sel: u32,
    /// Texture or rect origin: texel = pixel - org for pages; the snapped rect origin otherwise.
    pub org: [i32; 2],
    /// Packed RGBA8 colours, r in the low byte (image: pane w, h and image w, h).
    pub colors: [u32; 4],
    /// Per-kind constants computed on the CPU with the painter's expressions.
    pub params: [f32; 8],
}

/// Bytes per instance.
pub const INSTANCE_BYTES: usize = std::mem::size_of::<Instance>();
/// Distinct images one frame may draw (one descriptor set each); more are skipped and counted as image_over_limit.
pub const MAX_FRAME_IMAGES: usize = 256;

/// Bounds of one pass (proposals in the brief, stated here so tests can force small pages).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiLimits {
    pub glyph_min_side: u32,
    pub glyph_max_side: u32,
    pub icon_min_side: u32,
    pub icon_max_side: u32,
    /// Instance buffer cap per pass, bytes.
    pub instance_cap_bytes: usize,
    /// Passes per frame before `ui_pass_limit`.
    pub max_passes: u32,
    /// The device's maxImageDimension2D (images above it are skipped and counted).
    pub max_image_side: u32,
    /// Pixels of one image texture (brief hostile bound (f): MAX_PIXELS, 64 Mpx); larger images are skipped and counted.
    pub max_image_pixels: u64,
    /// Staging bytes of one upload submit (64 Mpx x 4 = 256 MiB); a pass whose uploads pass it splits them over
    /// several waited submits.
    pub staging_cap_bytes: usize,
}

impl Default for UiLimits {
    fn default() -> Self {
        Self {
            glyph_min_side: 1024,
            glyph_max_side: 4096,
            icon_min_side: 512,
            icon_max_side: 2048,
            instance_cap_bytes: 16 << 20,
            max_passes: 16,
            max_image_side: 16384,
            max_image_pixels: crate::refusal::MAX_PIXELS,
            staging_cap_bytes: 256 << 20,
        }
    }
}

/// Texels to copy into a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub bytes: Vec<u8>,
}

/// A run of instances drawn with one descriptor set: the pages, plus `image` when the run draws an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
    pub image: Option<ImageId>,
    pub first: u32,
    pub count: u32,
}

/// One pass: one submit, one render pass instance; a pass after the first loads the pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PassPlan {
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    /// The glyph page side in this pass and whether it was reset (or newly created) before the pass.
    pub glyph_side: u32,
    pub glyph_reset: bool,
    pub glyph_uploads: Vec<Upload>,
    pub icon_side: u32,
    pub icon_reset: bool,
    pub icon_uploads: Vec<Upload>,
}

impl PassPlan {
    fn push(&mut self, inst: Instance, image: Option<ImageId>) {
        let first = u32::try_from(self.instances.len()).unwrap_or(u32::MAX);
        self.instances.push(inst);
        match self.batches.last_mut() {
            Some(b) if b.image == image => b.count = b.count.saturating_add(1),
            _ => self.batches.push(Batch { image, first, count: 1 }),
        }
    }
}

/// One primitive of the trace (`--ui-trace`, visual debugging): no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    pub kind: &'static str,
    /// Snapped rect (physical px) before clipping; for a line or dash its padded box.
    pub rect: IRect,
    /// `flat`, `translucent`, `aa`, `glyph`, `icon`, `image`, `image_translucent`, `ramp`, `scissor`, `skipped`.
    pub class: &'static str,
    pub instances: u32,
    pub pass: u32,
}

/// What one recording did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordStats {
    pub prims: u64,
    pub glyphs: u64,
    pub glyph_refusals: u64,
    pub missing_resources: u64,
    pub image_over_limit: u64,
    /// Icon masks larger than the icon page's maximum side (skipped, nothing else changes).
    pub icon_over_limit: u64,
    pub skipped_non_finite: u64,
    pub scissor_underflow: u64,
    /// Recordings restarted after a page growth.
    pub restarts: u64,
}

/// The plan of one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub passes: Vec<PassPlan>,
    pub trace: Vec<TraceRecord>,
    pub stats: RecordStats,
    /// Images the frame draws, in first-use order.
    pub images: Vec<ImageId>,
}

fn pack(c: Rgba8) -> u32 {
    u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16) | (u32::from(c.a) << 24)
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Every float a primitive carries is finite (non-finite ones are skipped and counted on the GPU path).
fn prim_finite(p: &Prim) -> bool {
    let r = |r: &kumihimo_ui::geom::Rect| [r.x0, r.y0, r.x1, r.y1];
    let pt = |p: &Point| [p.x, p.y];
    match p {
        Prim::Rect { rect, .. } | Prim::Image { rect, .. } | Prim::Icon { rect, .. } | Prim::Ramp { rect, .. } | Prim::PushScissor(rect) => finite(&r(rect)),
        Prim::Edge { owner, .. } => finite(&r(owner)),
        Prim::RRect { rect, radius, border, .. } => finite(&r(rect)) && radius.is_finite() && border.is_none_or(|b| b.width.is_finite()),
        Prim::Line { a, b, width, .. } => finite(&pt(a)) && finite(&pt(b)) && width.is_finite(),
        Prim::Dash { a, b, width, dash, phase, .. } => finite(&pt(a)) && finite(&pt(b)) && finite(&[*width, *dash, *phase]),
        Prim::Glyphs { origin, glyphs, .. } => finite(&pt(origin)) && glyphs.iter().all(|g| g.x.is_finite() && g.y.is_finite()),
        Prim::Checker { rect, cell, .. } => finite(&r(rect)) && cell.is_finite(),
        Prim::Shadow { rect, radius, offset_y, blur, .. } => finite(&r(rect)) && finite(&[*radius, *offset_y, *blur]),
        Prim::PopScissor => true,
    }
}

/// Why a recording attempt stopped.
enum Stop {
    /// A page grew: record the frame again.
    Again,
    Refused(Refusal),
}

impl From<Refusal> for Stop {
    fn from(r: Refusal) -> Self {
        Stop::Refused(r)
    }
}

/// The CPU half of the pass: residency of the glyph and icon pages across frames, and the per-frame plan.
pub struct Recorder {
    glyphs: Atlas<GlyphKey>,
    icons: Atlas<(IconId, u32)>,
    /// Texels of each resident icon mask, interleaved RG (uploads only when they differ).
    icon_bytes: HashMap<(IconId, u32), Vec<u8>>,
    limits: UiLimits,
    /// The page was created, grown or reset since the last planned pass: the next pass resets it.
    glyph_fresh: bool,
    icon_fresh: bool,
}

/// One recording attempt.
struct Attempt<'p> {
    plan: Plan,
    cur: PassPlan,
    trace: bool,
    limits: &'p UiLimits,
    /// Entries placed or re-uploaded in this attempt (forgotten again when the attempt restarts, since their uploads
    /// were never run).
    new_icons: Vec<(IconId, u32)>,
    new_glyphs: Vec<GlyphKey>,
}

impl Attempt<'_> {
    fn pass_index(&self) -> u32 {
        u32::try_from(self.plan.passes.len()).unwrap_or(u32::MAX)
    }
    /// Ends the current pass; the next one loads the pane.
    fn pass_break(&mut self, glyph_reset: bool, icon_reset: bool, glyph_side: u32, icon_side: u32) -> Result<(), Stop> {
        if self.plan.passes.len().saturating_add(2) > self.limits.max_passes as usize {
            return Err(Stop::Refused(Refusal::UiPassLimit));
        }
        let next = PassPlan { glyph_side, glyph_reset, icon_side, icon_reset, ..PassPlan::default() };
        self.plan.passes.push(std::mem::replace(&mut self.cur, next));
        Ok(())
    }
    /// Adds an instance; a full instance buffer breaks the pass first.
    fn push(&mut self, inst: Instance, image: Option<ImageId>) -> Result<(), Stop> {
        let cap = (self.limits.instance_cap_bytes / INSTANCE_BYTES).max(1);
        if self.cur.instances.len() >= cap {
            let (gs, is) = (self.cur.glyph_side, self.cur.icon_side);
            self.pass_break(false, false, gs, is)?;
        }
        self.cur.push(inst, image);
        Ok(())
    }
    fn trace(&mut self, kind: &'static str, rect: IRect, class: &'static str, instances: u32) {
        if self.trace {
            let pass = self.pass_index();
            self.plan.trace.push(TraceRecord { kind, rect, class, instances, pass });
        }
    }
}

fn identity(text: &Text, scale: Scale) -> u64 {
    let mut h = DefaultHasher::new();
    text.epoch().hash(&mut h);
    text.lut().hash(&mut h);
    scale.get().to_bits().hash(&mut h);
    h.finish() | 1
}

impl Recorder {
    pub fn new(limits: UiLimits) -> Self {
        Self {
            glyphs: Atlas::new(limits.glyph_min_side, limits.glyph_max_side),
            icons: Atlas::new(limits.icon_min_side, limits.icon_max_side),
            icon_bytes: HashMap::new(),
            limits,
            glyph_fresh: true,
            icon_fresh: true,
        }
    }
    pub fn limits(&self) -> UiLimits {
        self.limits
    }
    /// Forgets every resident entry (both pages reset, icon bytes dropped): the next plan uploads everything it uses.
    /// After a refused draw no entry may count as resident without its upload (review F2).
    pub fn invalidate(&mut self) {
        self.glyphs.reset();
        self.icons.reset();
        self.icon_bytes.clear();
        self.glyph_fresh = true;
        self.icon_fresh = true;
    }
    pub fn glyph_atlas(&self) -> &Atlas<GlyphKey> {
        &self.glyphs
    }
    pub fn icon_atlas(&self) -> &Atlas<(IconId, u32)> {
        &self.icons
    }
    /// Plans `lists` (back to front) over a pane of `extent` at `scale`. A page that fills grows (the frame is
    /// planned again from the start) up to its maximum, then breaks the pass and resets; more than `max_passes`
    /// passes is `ui_pass_limit`.
    pub fn record(
        &mut self,
        extent: (u32, u32),
        lists: &[&DisplayList],
        scale: Scale,
        text: &mut Text,
        res: &mut dyn Resources,
        trace: bool,
    ) -> Result<Plan, Refusal> {
        if self.glyphs.set_identity(identity(text, scale)) {
            self.glyph_fresh = true;
        }
        let mut restarts = 0u64;
        loop {
            match self.attempt(extent, lists, scale, text, res, trace) {
                Ok(mut plan) => {
                    plan.stats.restarts = restarts;
                    return Ok(plan);
                }
                Err(Stop::Again) => restarts = restarts.saturating_add(1),
                Err(Stop::Refused(r)) => return Err(r),
            }
        }
    }

    fn attempt(
        &mut self,
        extent: (u32, u32),
        lists: &[&DisplayList],
        scale: Scale,
        text: &mut Text,
        res: &mut dyn Resources,
        trace: bool,
    ) -> Result<Plan, Stop> {
        let limits = self.limits;
        let first = PassPlan {
            glyph_side: self.glyphs.side(),
            glyph_reset: self.glyph_fresh,
            icon_side: self.icons.side(),
            icon_reset: self.icon_fresh,
            ..PassPlan::default()
        };
        let mut a = Attempt { plan: Plan::default(), cur: first, trace, limits: &limits, new_icons: Vec::new(), new_glyphs: Vec::new() };
        let result = self.walk(&mut a, extent, lists, scale, text, res);
        match result {
            Ok(()) => {
                self.glyph_fresh = false;
                self.icon_fresh = false;
                let Attempt { mut plan, cur, .. } = a;
                plan.passes.push(cur);
                Ok(plan)
            }
            Err(e) => {
                // Entries placed in the abandoned (restarted or refused) attempt were never uploaded (review F2).
                for k in a.new_icons {
                    self.icons.remove(&k);
                    self.icon_bytes.remove(&k);
                }
                for k in a.new_glyphs {
                    self.glyphs.remove(&k);
                }
                Err(e)
            }
        }
    }

    fn walk(&mut self, a: &mut Attempt<'_>, extent: (u32, u32), lists: &[&DisplayList], scale: Scale, text: &mut Text, res: &mut dyn Resources) -> Result<(), Stop> {
        let pane = IRect { x0: 0, y0: 0, x1: i32::try_from(extent.0).unwrap_or(i32::MAX), y1: i32::try_from(extent.1).unwrap_or(i32::MAX) };
        let s = scale.get();
        let mut frame_icons: HashMap<(IconId, u32), Option<IconMask>> = HashMap::new();
        for list in lists {
            let mut clips: Vec<IRect> = vec![pane];
            for item in &list.items {
                a.plan.stats.prims = a.plan.stats.prims.saturating_add(1);
                let clip = *clips.last().unwrap_or(&IRect::default());
                if !prim_finite(item) {
                    a.plan.stats.skipped_non_finite = a.plan.stats.skipped_non_finite.saturating_add(1);
                    a.trace(item.kind(), IRect::default(), "skipped", 0);
                    continue;
                }
                match item {
                    Prim::Rect { rect, color } => {
                        let p = scale.snap(*rect);
                        let n = flat(a, p.intersect(&clip), *color)?;
                        a.trace("rect", p, if color.a == 255 { "flat" } else { "translucent" }, n);
                    }
                    Prim::Edge { owner, side, px, color } => {
                        let o = scale.snap(*owner);
                        let t = scale.edge_px(*px);
                        let r = match side {
                            Side::Left => IRect { x1: o.x0.saturating_add(t).min(o.x1), ..o },
                            Side::Right => IRect { x0: o.x1.saturating_sub(t).max(o.x0), ..o },
                            Side::Top => IRect { y1: o.y0.saturating_add(t).min(o.y1), ..o },
                            Side::Bottom => IRect { y0: o.y1.saturating_sub(t).max(o.y0), ..o },
                        };
                        let n = flat(a, r.intersect(&clip), *color)?;
                        a.trace("rect", r, if color.a == 255 { "flat" } else { "translucent" }, n);
                    }
                    Prim::RRect { rect, radius, fill, border } => {
                        let p = scale.snap(*rect);
                        if p.is_empty() {
                            a.trace("rrect", p, "aa", 0);
                            continue;
                        }
                        let (cx, cy) = ((p.x0.saturating_add(p.x1)) as f32 / 2.0, (p.y0.saturating_add(p.y1)) as f32 / 2.0);
                        let (hx, hy) = (p.w() as f32 / 2.0, p.h() as f32 / 2.0);
                        let rad = (radius * s).round();
                        let bw = border.map(|b| if b.width <= 1.0 { scale.hairline() as f32 } else { (b.width * s).round().max(b.width) });
                        let r = p.intersect(&clip);
                        let mut n = 0;
                        if !r.is_empty() {
                            let params = [cx, cy, hx, hy, rad, bw.unwrap_or(0.0), 0.0, 0.0];
                            let base = Instance { quad: quad(r), kind: kind::RRECT, params, ..Instance::default() };
                            match (border, bw) {
                                (Some(b), Some(_)) => {
                                    a.push(Instance { sel: 1, colors: [pack(b.color), 0, 0, 0], ..base }, None)?;
                                    a.push(Instance { sel: 2, colors: [pack(*fill), 0, 0, 0], ..base }, None)?;
                                    n = 2;
                                }
                                _ => {
                                    a.push(Instance { sel: 0, colors: [pack(*fill), 0, 0, 0], ..base }, None)?;
                                    n = 1;
                                }
                            }
                        }
                        a.trace("rrect", p, "aa", n);
                    }
                    Prim::Line { a: pa, b: pb, width, color } => {
                        let (area, r, inst) = line(scale, &clip, *pa, *pb, *width);
                        let mut n = 0;
                        if !r.is_empty() {
                            a.push(Instance { kind: kind::LINE, colors: [pack(*color), 0, 0, 0], ..inst }, None)?;
                            n = 1;
                        }
                        a.trace("line", area, "aa", n);
                    }
                    Prim::Dash { a: pa, b: pb, width, dash, phase, on, off } => {
                        let period = (dash * 2.0 * s).max(2.0);
                        let on_len = (dash * s).max(1.0);
                        let ph = phase * s;
                        let (area, r, mut inst) = line(scale, &clip, *pa, *pb, *width);
                        let mut n = 0;
                        if !r.is_empty() {
                            inst.kind = kind::DASH;
                            inst.sel = u32::from(off.is_some());
                            inst.colors = [pack(*on), off.map_or(0, pack), 0, 0];
                            inst.org = [period.to_bits() as i32, on_len.to_bits() as i32];
                            inst.params[7] = ph;
                            a.push(inst, None)?;
                            n = 1;
                        }
                        a.trace("dash", area, "aa", n);
                    }
                    Prim::Glyphs { origin, token, glyphs, color } => {
                        let (n, bbox) = self.glyphs_prim(a, &clip, scale, *origin, token.style().size, glyphs, *color, text)?;
                        a.trace("glyph", bbox, "glyph", n);
                    }
                    Prim::Icon { rect, icon, form, hot } => {
                        let p = scale.snap(*rect);
                        let px = p.w().min(p.h()).max(1) as u32;
                        let key = (*icon, px);
                        let mask = frame_icons.entry(key).or_insert_with(|| res.icon(*icon, px));
                        let Some(mask) = mask.as_ref().filter(|m| m.rg.len() == (m.width as usize).saturating_mul(m.height as usize)) else {
                            a.plan.stats.missing_resources = a.plan.stats.missing_resources.saturating_add(1);
                            a.trace("icon", p, "icon", 0);
                            continue;
                        };
                        if mask.width > self.icons.max_side() || mask.height > self.icons.max_side() {
                            // Larger than the page can ever be: this icon alone is refused (review F4).
                            a.plan.stats.icon_over_limit = a.plan.stats.icon_over_limit.saturating_add(1);
                            a.trace("icon", p, "icon", 0);
                            continue;
                        }
                        let (mw, mh) = (i32::try_from(mask.width).unwrap_or(i32::MAX), i32::try_from(mask.height).unwrap_or(i32::MAX));
                        let r = IRect { x0: p.x0, y0: p.y0, x1: p.x0.saturating_add(mw), y1: p.y0.saturating_add(mh) }.intersect(&clip);
                        let mut n = 0;
                        if !r.is_empty() {
                            let slot = self.icon_slot(a, key, mask)?;
                            let org = [p.x0.saturating_sub(slot.x as i32), p.y0.saturating_sub(slot.y as i32)];
                            let base = Instance { quad: quad(r), kind: kind::ICON, org, ..Instance::default() };
                            a.push(Instance { sel: 0, colors: [pack(*form), 0, 0, 0], ..base }, None)?;
                            a.push(Instance { sel: 1, colors: [pack(*hot), 0, 0, 0], ..base }, None)?;
                            n = 2;
                        }
                        a.trace("icon", p, "icon", n);
                    }
                    Prim::Image { rect, image } => {
                        let p = scale.snap(*rect);
                        let Some(img) = res.image(*image) else {
                            a.plan.stats.missing_resources = a.plan.stats.missing_resources.saturating_add(1);
                            a.trace("image", p, "image", 0);
                            continue;
                        };
                        let need = (img.width as usize).saturating_mul(img.height as usize).saturating_mul(4);
                        if img.width == 0 || img.height == 0 || img.rgba.len() < need || p.is_empty() {
                            a.plan.stats.missing_resources = a.plan.stats.missing_resources.saturating_add(1);
                            a.trace("image", p, "image", 0);
                            continue;
                        }
                        let known = a.plan.images.contains(image);
                        let pixels = u64::from(img.width).saturating_mul(u64::from(img.height));
                        if img.width > a.limits.max_image_side
                            || img.height > a.limits.max_image_side
                            || pixels > a.limits.max_image_pixels
                            || need > a.limits.staging_cap_bytes
                            || (!known && a.plan.images.len() >= MAX_FRAME_IMAGES)
                        {
                            a.plan.stats.image_over_limit = a.plan.stats.image_over_limit.saturating_add(1);
                            a.trace("image", p, "image", 0);
                            continue;
                        }
                        let opaque = img.rgba[..need].chunks_exact(4).all(|px| px[3] == 255);
                        let r = p.intersect(&clip);
                        let mut n = 0;
                        if !r.is_empty() {
                            if !known {
                                a.plan.images.push(*image);
                            }
                            let colors = [p.w() as u32, p.h() as u32, img.width, img.height];
                            a.push(Instance { quad: quad(r), kind: kind::IMAGE, org: [p.x0, p.y0], colors, ..Instance::default() }, Some(*image))?;
                            n = 1;
                        }
                        a.trace("image", p, if opaque { "image" } else { "image_translucent" }, n);
                    }
                    Prim::Checker { rect, a: ca, b: cb, cell } => {
                        let p = scale.snap(*rect);
                        let c = ((cell * s).round() as i32).max(1);
                        let r = p.intersect(&clip);
                        let mut n = 0;
                        if !r.is_empty() {
                            let inst = Instance { quad: quad(r), kind: kind::CHECKER, sel: c as u32, org: [p.x0, p.y0], colors: [pack(*ca), pack(*cb), 0, 0], ..Instance::default() };
                            a.push(inst, None)?;
                            n = 1;
                        }
                        let opaque = ca.a == 255 && cb.a == 255;
                        a.trace("checker", p, if opaque { "flat" } else { "translucent" }, n);
                    }
                    Prim::Ramp { rect, corners } => {
                        let p = scale.snap(*rect);
                        let r = p.intersect(&clip);
                        let (w, h) = ((p.w() - 1).max(1) as f32, (p.h() - 1).max(1) as f32);
                        let mut n = 0;
                        if !r.is_empty() {
                            let colors = [pack(corners[0]), pack(corners[1]), pack(corners[2]), pack(corners[3])];
                            let inst = Instance { quad: quad(r), kind: kind::RAMP, org: [p.x0, p.y0], colors, params: [w, h, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Instance::default() };
                            a.push(inst, None)?;
                            n = 1;
                        }
                        a.trace("ramp", p, "ramp", n);
                    }
                    Prim::Shadow { rect, radius, offset_y, blur, color } => {
                        let p = scale.snap(rect.translate(0.0, *offset_y));
                        let bl = (blur * s).max(1.0);
                        let grow = bl.ceil() as i32;
                        let area = IRect { x0: p.x0.saturating_sub(grow), y0: p.y0.saturating_sub(grow), x1: p.x1.saturating_add(grow), y1: p.y1.saturating_add(grow) };
                        let r = area.intersect(&clip);
                        let (cx, cy) = ((p.x0.saturating_add(p.x1)) as f32 / 2.0, (p.y0.saturating_add(p.y1)) as f32 / 2.0);
                        let (hx, hy) = (p.w() as f32 / 2.0, p.h() as f32 / 2.0);
                        let mut n = 0;
                        if !r.is_empty() {
                            let params = [cx, cy, hx, hy, radius * s, bl, 0.0, 0.0];
                            a.push(Instance { quad: quad(r), kind: kind::SHADOW, colors: [pack(*color), 0, 0, 0], params, ..Instance::default() }, None)?;
                            n = 1;
                        }
                        a.trace("shadow", area, "aa", n);
                    }
                    Prim::PushScissor(r) => {
                        let next = scale.snap(*r).intersect(&clip);
                        clips.push(next);
                        a.trace("scissor", next, "scissor", 0);
                    }
                    Prim::PopScissor => {
                        if clips.len() > 1 {
                            clips.pop();
                        } else {
                            a.plan.stats.scissor_underflow = a.plan.stats.scissor_underflow.saturating_add(1);
                        }
                        a.trace("scissor", IRect::default(), "scissor", 0);
                    }
                }
            }
        }
        Ok(())
    }

    /// The painter's glyph placement (paint.rs Glyphs): px_size_64, quarter-pixel bin with carry, baseline through
    /// Scale::edge; masks through Text::mask, so both painters see the same rasters. Only glyphs whose quad meets the
    /// clip are placed.
    #[allow(clippy::too_many_arguments)]
    fn glyphs_prim(
        &mut self,
        a: &mut Attempt<'_>,
        clip: &IRect,
        scale: Scale,
        origin: Point,
        size: f32,
        glyphs: &[kumihimo_ui::text::ShapedGlyph],
        color: Rgba8,
        text: &mut Text,
    ) -> Result<(u32, IRect), Stop> {
        let s = scale.get();
        let mut bbox: Option<IRect> = None;
        let px_size_64 = (size * s * 64.0).round() as u32;
        let base_y = scale.edge(origin.y);
        let lut = *text.lut();
        let mut n = 0u32;
        for g in glyphs {
            a.plan.stats.glyphs = a.plan.stats.glyphs.saturating_add(1);
            let fx = (origin.x + g.x) * s;
            let mut ix = fx.floor() as i32;
            let mut bin = ((fx - fx.floor()) * 4.0).round() as u8;
            if bin >= 4 {
                bin = 0;
                ix = ix.saturating_add(1);
            }
            let gy = base_y.saturating_sub((g.y * s).round() as i32);
            let key = GlyphKey { face: g.face, glyph_id: g.glyph_id, px_size_64, subpx_x: bin };
            let Ok(mask) = text.mask(key) else {
                a.plan.stats.glyph_refusals = a.plan.stats.glyph_refusals.saturating_add(1);
                continue;
            };
            if mask.width == 0 || mask.height == 0 {
                continue;
            }
            let (x0, y0) = (ix.saturating_add(mask.left), gy.saturating_add(mask.top));
            let (mw, mh) = (i32::try_from(mask.width).unwrap_or(i32::MAX), i32::try_from(mask.height).unwrap_or(i32::MAX));
            let full = IRect { x0, y0, x1: x0.saturating_add(mw), y1: y0.saturating_add(mh) };
            let r = full.intersect(clip);
            if r.is_empty() {
                continue;
            }
            bbox = Some(bbox.map_or(full, |b| IRect { x0: b.x0.min(full.x0), y0: b.y0.min(full.y0), x1: b.x1.max(full.x1), y1: b.y1.max(full.y1) }));
            let (w, h) = (mask.width, mask.height);
            if w > self.glyphs.max_side() || h > self.glyphs.max_side() {
                // Larger than the page can ever be: this glyph alone is refused, no reset, no pass break (F4).
                a.plan.stats.glyph_refusals = a.plan.stats.glyph_refusals.saturating_add(1);
                continue;
            }
            let slot = match self.glyphs.place(key, w, h) {
                Placed::Resident(slot) => slot,
                Placed::New(slot) => {
                    a.cur.glyph_uploads.push(Upload { x: slot.x, y: slot.y, w, h, bytes: mask.data.iter().map(|&c| lut[c as usize]).collect() });
                    a.new_glyphs.push(key);
                    slot
                }
                Placed::Full => {
                    if self.glyphs.grow() {
                        self.glyph_fresh = true;
                        return Err(Stop::Again);
                    }
                    // At the maximum: break the pass and start a fresh page.
                    self.glyphs.reset();
                    let is = a.cur.icon_side;
                    a.pass_break(true, false, self.glyphs.side(), is)?;
                    match self.glyphs.place(key, w, h) {
                        Placed::New(slot) | Placed::Resident(slot) => {
                            a.cur.glyph_uploads.push(Upload { x: slot.x, y: slot.y, w, h, bytes: mask.data.iter().map(|&c| lut[c as usize]).collect() });
                            a.new_glyphs.push(key);
                            slot
                        }
                        Placed::Full => {
                            a.plan.stats.glyph_refusals = a.plan.stats.glyph_refusals.saturating_add(1);
                            continue;
                        }
                    }
                }
            };
            let org = [x0.saturating_sub(slot.x as i32), y0.saturating_sub(slot.y as i32)];
            a.push(Instance { quad: quad(r), kind: kind::GLYPH, org, colors: [pack(color), 0, 0, 0], ..Instance::default() }, None)?;
            n = n.saturating_add(1);
        }
        Ok((n, bbox.unwrap_or_default()))
    }

    /// The slot of an icon mask, uploading it when it is new or its bytes changed.
    fn icon_slot(&mut self, a: &mut Attempt<'_>, key: (IconId, u32), mask: &IconMask) -> Result<Slot, Stop> {
        let bytes: Vec<u8> = mask.rg.iter().flat_map(|p| *p).collect();
        if let Some(slot) = self.icons.get(&key) {
            if (slot.w, slot.h) == (mask.width, mask.height) {
                if self.icon_bytes.get(&key) != Some(&bytes) {
                    a.cur.icon_uploads.push(Upload { x: slot.x, y: slot.y, w: slot.w, h: slot.h, bytes: bytes.clone() });
                    self.icon_bytes.insert(key, bytes);
                    a.new_icons.push(key);
                }
                return Ok(slot);
            }
            self.icons.remove(&key);
        }
        let slot = match self.icons.place(key, mask.width, mask.height) {
            Placed::New(slot) | Placed::Resident(slot) => slot,
            Placed::Full => {
                if self.icons.grow() {
                    self.icon_bytes.clear();
                    self.icon_fresh = true;
                    return Err(Stop::Again);
                }
                self.icons.reset();
                self.icon_bytes.clear();
                let gs = a.cur.glyph_side;
                a.pass_break(false, true, gs, self.icons.side())?;
                match self.icons.place(key, mask.width, mask.height) {
                    Placed::New(slot) | Placed::Resident(slot) => slot,
                    Placed::Full => return Err(Stop::Refused(Refusal::UiPassLimit)),
                }
            }
        };
        a.new_icons.push(key);
        a.cur.icon_uploads.push(Upload { x: slot.x, y: slot.y, w: slot.w, h: slot.h, bytes: bytes.clone() });
        self.icon_bytes.insert(key, bytes);
        Ok(slot)
    }
}

fn quad(r: IRect) -> [i32; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

fn flat(a: &mut Attempt<'_>, r: IRect, color: Rgba8) -> Result<u32, Stop> {
    if r.is_empty() {
        return Ok(0);
    }
    a.push(Instance { quad: quad(r), kind: kind::FLAT, colors: [pack(color), 0, 0, 0], ..Instance::default() }, None)?;
    Ok(1)
}

/// The painter's segment setup (paint.rs line()): physical end points, half width, len2, len and the padded box.
fn line(scale: Scale, clip: &IRect, a: Point, b: Point, width: f32) -> (IRect, IRect, Instance) {
    let s = scale.get();
    let (ax, ay, bx, by) = (a.x * s, a.y * s, b.x * s, b.y * s);
    let hw = if width <= 1.0 { scale.hairline() as f32 / 2.0 } else { width * s / 2.0 };
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let len = len2.sqrt();
    let pad = hw + 1.0;
    let area = IRect {
        x0: (ax.min(bx) - pad).floor() as i32,
        y0: (ay.min(by) - pad).floor() as i32,
        x1: (ax.max(bx) + pad).ceil() as i32,
        y1: (ay.max(by) + pad).ceil() as i32,
    };
    let r = area.intersect(clip);
    (area, r, Instance { quad: quad(r), params: [ax, ay, bx, by, hw, len2, len, 0.0], ..Instance::default() })
}

/// One frame for [`UiPass::draw`]: the frame clear, the ordered lists, the scale, the text and the resources.
pub struct UiFrame<'a> {
    pub clear: Rgba8,
    pub lists: &'a [&'a DisplayList],
    pub scale: Scale,
    pub text: &'a mut Text,
    pub res: &'a mut dyn Resources,
}

/// Counters of the pass (receipt member `ui_pass`): allocation sizes come from VkMemoryRequirements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiStats {
    pub draws: u64,
    pub passes: u64,
    pub instances: u64,
    pub glyph_side: u32,
    pub glyph_entries: u64,
    pub glyph_uploads: u64,
    pub glyph_upload_bytes: u64,
    pub glyph_resets: u64,
    pub glyph_growths: u64,
    pub icon_side: u32,
    pub icon_entries: u64,
    pub icon_uploads: u64,
    pub icon_upload_bytes: u64,
    pub icon_resets: u64,
    pub image_textures: u64,
    pub image_uploads: u64,
    pub image_upload_bytes: u64,
    /// Device memory of the pages, image textures, instance and staging buffers (VkMemoryRequirements sizes).
    pub page_bytes: u64,
    pub image_bytes: u64,
    pub instance_bytes: u64,
    pub staging_bytes: u64,
    pub glyph_refusals: u64,
    pub missing_resources: u64,
    pub image_over_limit: u64,
    pub icon_over_limit: u64,
    pub skipped_non_finite: u64,
    pub scissor_underflow: u64,
    /// Waited submits that carried uploads only (a pass's uploads over the staging cap).
    pub upload_submits: u64,
    /// Allocations refused (out_of_memory) by the staging or instance cap or a failed reservation.
    pub alloc_refusals: u64,
    /// Draws refused after recording or during a pass; each invalidated the pages and image uploads.
    pub invalidations: u64,
}

/// A device image the pass samples: a page or an image texture.
struct Tex {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    layout: vk::ImageLayout,
    alloc: u64,
}

impl Tex {
    fn new(core: &Core, width: u32, height: u32, format: vk::Format) -> Result<Self, Refusal> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: valid device; the format's SAMPLED_IMAGE and TRANSFER_DST features were checked in UiPass::new;
        // every failure path destroys what it created.
        unsafe {
            let image = core.device.create_image(&info, None).map_err(vk_refusal)?;
            let req = core.device.get_image_memory_requirements(image);
            let Some(kind) = core.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL) else {
                core.device.destroy_image(image, None);
                return Err(Refusal::OutOfMemory);
            };
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(kind);
            let memory = match core.device.allocate_memory(&alloc, None) {
                Ok(m) => m,
                Err(e) => {
                    core.device.destroy_image(image, None);
                    return Err(vk_refusal(e));
                }
            };
            if let Err(e) = core.device.bind_image_memory(image, memory, 0) {
                core.device.destroy_image(image, None);
                core.device.free_memory(memory, None);
                return Err(vk_refusal(e));
            }
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range());
            let view = match core.device.create_image_view(&view_info, None) {
                Ok(v) => v,
                Err(e) => {
                    core.device.destroy_image(image, None);
                    core.device.free_memory(memory, None);
                    return Err(vk_refusal(e));
                }
            };
            Ok(Self { image, memory, view, width, height, layout: vk::ImageLayout::UNDEFINED, alloc: req.size })
        }
    }
    fn destroy(&mut self, core: &Core) {
        // SAFETY: the device is idle for this image (every submit that used it was waited on).
        unsafe {
            core.device.destroy_image_view(self.view, None);
            core.device.destroy_image(self.image, None);
            core.device.free_memory(self.memory, None);
        }
        self.view = vk::ImageView::null();
        self.image = vk::Image::null();
    }
}

/// An image texture with the bytes of its last upload (re-uploaded only when they differ).
struct ImageTex {
    tex: Tex,
    bytes: Vec<u8>,
}

/// One copy into a texture, from the staging buffer.
struct Copy {
    target: Target,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Glyph,
    Icon,
    Dummy,
    Image(ImageId),
}

/// Appends `bytes` to the staging blob at a 4-byte aligned offset (texel-size aligned for R8, R8G8 and RGBA8). The
/// reservation is fallible: a failed one is `out_of_memory`, never an abort (review F3).
fn stage(blob: &mut Vec<u8>, bytes: &[u8]) -> Result<u64, Refusal> {
    let pad = blob.len().next_multiple_of(4) - blob.len();
    blob.try_reserve(pad.saturating_add(bytes.len())).map_err(|_| Refusal::OutOfMemory)?;
    blob.resize(blob.len() + pad, 0);
    let at = blob.len() as u64;
    blob.extend_from_slice(bytes);
    Ok(at)
}

/// Bytes `bytes` adds to `blob` with its alignment.
fn staged_len(blob: &[u8], bytes: &[u8]) -> usize {
    blob.len().next_multiple_of(4).saturating_add(bytes.len())
}

const VS: &[u8] = include_bytes!("../shaders/ui.vs_main.spv");
const FS: &[u8] = include_bytes!("../shaders/ui.fs_main.spv");
/// The checked-in SPIR-V modules (entry point, bytes) the pipeline is built from, and their manifest: hosts hash them
/// with their own SHA-256 and cross-check the manifest in receipts.
pub const SPIRV: [(&str, &[u8]); 2] = [("vs_main", VS), ("fs_main", FS)];
pub const MANIFEST: &str = include_str!("../shaders/manifest.json");
/// Smallest instance and staging buffers; both double up to their caps.
const MIN_BUFFER: usize = 64 << 10;

/// The GPU half: pipeline, pages, image textures, instance and staging buffers, on the owner's device.
pub struct UiPass {
    core: Arc<Core>,
    generation: u32,
    recorder: Recorder,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    glyph: Option<Tex>,
    icon: Option<Tex>,
    dummy: Option<Tex>,
    images: HashMap<ImageId, ImageTex>,
    instances: Option<HostBuffer>,
    staging: Option<HostBuffer>,
    stats: UiStats,
    trace: Vec<TraceRecord>,
    trace_on: bool,
    /// The largest render target: min(maxFramebufferWidth/Height, maxViewportDimensions) (review F5).
    max_target: (u32, u32),
}

fn shader(core: &Core, bytes: &[u8]) -> Result<vk::ShaderModule, Refusal> {
    // include_bytes! is not u32-aligned: read_spv copies into words and checks the magic and the length.
    let words = ash::util::read_spv(&mut std::io::Cursor::new(bytes)).map_err(|_| Refusal::Vk(vk::Result::ERROR_INVALID_SHADER_NV.as_raw()))?;
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: valid device; the words are a validated SPIR-V 1.6 module (tests/ui_shaders.rs, spv-build).
    unsafe { core.device.create_shader_module(&info, None) }.map_err(vk_refusal)
}

impl UiPass {
    /// Builds the pipeline on `owner`'s device. Refusals: `format_unsupported`, `feature_missing`, Vulkan errors.
    pub fn new(owner: &Owner) -> Result<Self, Refusal> {
        Self::with_limits(owner, UiLimits::default())
    }

    pub fn with_limits(owner: &Owner, limits: UiLimits) -> Result<Self, Refusal> {
        if !owner.device_info().features.contains(&"dynamicRendering") {
            return Err(Refusal::FeatureMissing);
        }
        let core = Arc::clone(owner.core());
        // SAFETY: read-only queries on the owner's physical device.
        let (props, fmt) = unsafe {
            let f = |format| core.instance.get_physical_device_format_properties(core.physical, format).optimal_tiling_features;
            (core.instance.get_physical_device_properties(core.physical), [f(vk::Format::R8G8B8A8_UNORM), f(vk::Format::R8_UNORM), f(vk::Format::R8G8_UNORM)])
        };
        let sampled = vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_DST;
        if !fmt[0].contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND | sampled) || !fmt[1].contains(sampled) || !fmt[2].contains(sampled) {
            return Err(Refusal::FormatUnsupported);
        }
        let limits = UiLimits { max_image_side: limits.max_image_side.min(props.limits.max_image_dimension2_d), ..limits };
        let l = &props.limits;
        let max_target = (l.max_framebuffer_width.min(l.max_viewport_dimensions[0]), l.max_framebuffer_height.min(l.max_viewport_dimensions[1]));
        let mut pass = Self {
            core,
            generation: owner.lease().generation,
            recorder: Recorder::new(limits),
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            pool: vk::DescriptorPool::null(),
            glyph: None,
            icon: None,
            dummy: None,
            images: HashMap::new(),
            instances: None,
            staging: None,
            stats: UiStats::default(),
            trace: Vec::new(),
            trace_on: false,
            max_target,
        };
        // On failure, Drop destroys whatever was created.
        pass.build()?;
        Ok(pass)
    }

    fn build(&mut self) -> Result<(), Refusal> {
        let core = Arc::clone(&self.core);
        let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..3)
            .map(|b| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            })
            .collect();
        let pc = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::VERTEX).offset(0).size(8)];
        let sets = u32::try_from(MAX_FRAME_IMAGES).unwrap_or(256).saturating_add(1);
        let sizes = [vk::DescriptorPoolSize::default().ty(vk::DescriptorType::SAMPLED_IMAGE).descriptor_count(sets.saturating_mul(3))];
        // SAFETY: valid device; every create info lives for its call; partial results are kept in `self` and
        // destroyed by Drop on failure.
        unsafe {
            self.set_layout = core
                .device
                .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
                .map_err(vk_refusal)?;
            let set_layouts = [self.set_layout];
            self.layout = core
                .device
                .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts).push_constant_ranges(&pc), None)
                .map_err(vk_refusal)?;
            self.pool = core
                .device
                .create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(sets).pool_sizes(&sizes), None)
                .map_err(vk_refusal)?;
        }
        let vs = shader(&core, VS)?;
        let fs = match shader(&core, FS) {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: the module is unused.
                unsafe { core.device.destroy_shader_module(vs, None) };
                return Err(e);
            }
        };
        let result = self.pipeline(vs, fs);
        // SAFETY: modules may be destroyed once the pipeline is created (or failed).
        unsafe {
            core.device.destroy_shader_module(vs, None);
            core.device.destroy_shader_module(fs, None);
        }
        self.pipeline = result?;
        self.dummy = Some(Tex::new(&core, 1, 1, vk::Format::R8G8B8A8_UNORM)?);
        Ok(())
    }

    fn pipeline(&self, vs: vk::ShaderModule, fs: vk::ShaderModule) -> Result<vk::Pipeline, Refusal> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vs).name(c"vs_main"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(fs).name(c"fs_main"),
        ];
        let binding = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(INSTANCE_BYTES as u32)
            .input_rate(vk::VertexInputRate::INSTANCE)];
        let attr = |location, format, offset| vk::VertexInputAttributeDescription::default().location(location).binding(0).format(format).offset(offset);
        let attrs = [
            attr(0, vk::Format::R32G32B32A32_SINT, 0),
            attr(1, vk::Format::R32G32_UINT, 16),
            attr(2, vk::Format::R32G32_SINT, 24),
            attr(3, vk::Format::R32G32B32A32_UINT, 32),
            attr(4, vk::Format::R32G32B32A32_SFLOAT, 48),
            attr(5, vk::Format::R32G32B32A32_SFLOAT, 64),
        ];
        let vertex = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&binding).vertex_attribute_descriptions(&attrs);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // The painter's "over" in encoded space (paint.rs blend): colour SRC_ALPHA / ONE_MINUS_SRC_ALPHA, alpha ONE /
        // ONE_MINUS_SRC_ALPHA.
        let blend = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let formats = [vk::Format::R8G8B8A8_UNORM];
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blending)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .push_next(&mut rendering);
        // SAFETY: valid device and layout; dynamicRendering is enabled (Owner::open_device); every struct lives for the
        // call.
        let pipelines = unsafe { self.core.device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }.map_err(|(_, e)| vk_refusal(e))?;
        pipelines.first().copied().ok_or(Refusal::Vk(vk::Result::ERROR_UNKNOWN.as_raw()))
    }

    pub fn stats(&self) -> UiStats {
        self.stats
    }
    pub fn recorder(&self) -> &Recorder {
        &self.recorder
    }
    /// Keep one trace record per primitive of the next draws.
    pub fn set_trace(&mut self, on: bool) {
        self.trace_on = on;
    }
    /// The trace of the last draw.
    pub fn trace(&self) -> &[TraceRecord] {
        &self.trace
    }

    /// Draws one frame into `pane`: clear, every list in order, pane left in TRANSFER_SRC_OPTIMAL. A pass whose Core
    /// or owner generation differs from `owner`'s refuses `invalid_pane`; a poisoned submitter refuses with its own
    /// refusal; nothing retries.
    pub fn draw(&mut self, owner: &mut Owner, pane: PaneId, frame: UiFrame<'_>) -> Result<(), Refusal> {
        if !Arc::ptr_eq(owner.core(), &self.core) || owner.lease().generation != self.generation {
            return Err(Refusal::InvalidPane);
        }
        if let Some(r) = owner.poisoned() {
            return Err(r);
        }
        let (pw, ph) = owner.pane_extent(pane)?;
        if pw > self.max_target.0 || ph > self.max_target.1 {
            return Err(Refusal::InvalidExtent);
        }
        let UiFrame { clear, lists, scale, text, res } = frame;
        let result = self.recorder.record((pw, ph), lists, scale, text, res, self.trace_on).and_then(|plan| {
            for (i, pass) in plan.passes.iter().enumerate() {
                self.run_pass(owner, pane, (pw, ph), clear, pass, i == 0, &*res)?;
            }
            Ok(plan)
        });
        let plan = match result {
            Ok(plan) => plan,
            Err(r) => {
                // Uploads planned or recorded for this frame may not have run: nothing stays resident on their
                // account (review F2). Image textures re-upload on their next use.
                self.recorder.invalidate();
                for t in self.images.values_mut() {
                    t.bytes = Vec::new();
                }
                self.stats.invalidations = self.stats.invalidations.saturating_add(1);
                if r == Refusal::OutOfMemory {
                    self.stats.alloc_refusals = self.stats.alloc_refusals.saturating_add(1);
                }
                return Err(r);
            }
        };
        let st = plan.stats;
        // Image textures the frame did not draw are released.
        let used: Vec<ImageId> = plan.images.clone();
        let core = Arc::clone(&self.core);
        self.images.retain(|id, t| {
            let keep = used.contains(id);
            if !keep {
                t.tex.destroy(&core);
            }
            keep
        });
        let g = self.recorder.glyph_atlas();
        let ic = self.recorder.icon_atlas();
        let s = &mut self.stats;
        s.draws = s.draws.saturating_add(1);
        s.glyph_side = g.side();
        s.glyph_entries = g.len() as u64;
        s.glyph_resets = g.stats().resets;
        s.glyph_growths = g.stats().growths;
        s.icon_side = ic.side();
        s.icon_entries = ic.len() as u64;
        s.icon_resets = ic.stats().resets;
        s.image_textures = self.images.len() as u64;
        s.image_bytes = self.images.values().map(|t| t.tex.alloc).sum();
        s.page_bytes = [&self.glyph, &self.icon, &self.dummy].iter().filter_map(|t| t.as_ref()).map(|t| t.alloc).sum();
        s.instance_bytes = self.instances.as_ref().map_or(0, |b| b.alloc);
        s.staging_bytes = self.staging.as_ref().map_or(0, |b| b.alloc);
        s.glyph_refusals = s.glyph_refusals.saturating_add(st.glyph_refusals);
        s.missing_resources = s.missing_resources.saturating_add(st.missing_resources);
        s.image_over_limit = s.image_over_limit.saturating_add(st.image_over_limit);
        s.icon_over_limit = s.icon_over_limit.saturating_add(st.icon_over_limit);
        s.skipped_non_finite = s.skipped_non_finite.saturating_add(st.skipped_non_finite);
        s.scissor_underflow = s.scissor_underflow.saturating_add(st.scissor_underflow);
        self.trace = plan.trace;
        Ok(())
    }

    /// A page of `side` (recreated when the side changed or the plan resets it on a first use).
    fn page(core: &Core, slot: &mut Option<Tex>, side: u32, format: vk::Format) -> Result<(), Refusal> {
        if slot.as_ref().is_some_and(|t| t.width == side) {
            return Ok(());
        }
        if let Some(mut old) = slot.take() {
            old.destroy(core);
        }
        *slot = Some(Tex::new(core, side, side, format)?);
        Ok(())
    }

    /// A buffer of at least `need` bytes, doubling from MIN_BUFFER up to `cap`; kept across frames. Over the cap is
    /// `out_of_memory` (review F3).
    fn buffer(core: &Core, slot: &mut Option<HostBuffer>, need: usize, cap: usize, usage: vk::BufferUsageFlags) -> Result<(), Refusal> {
        if need > cap.max(MIN_BUFFER) {
            return Err(Refusal::OutOfMemory);
        }
        if slot.as_ref().is_some_and(|b| b.len >= need) {
            return Ok(());
        }
        let mut len = slot.as_ref().map_or(MIN_BUFFER, |b| b.len).max(MIN_BUFFER);
        while len < need {
            len = len.checked_mul(2).ok_or(Refusal::OutOfMemory)?;
        }
        let len = len.min(cap.max(MIN_BUFFER));
        if let Some(mut old) = slot.take() {
            old.destroy(core);
        }
        *slot = Some(HostBuffer::new(core, len, usage)?);
        Ok(())
    }

    /// Adds one upload to the staging blob; when it would pass the staging cap, the uploads so far go out first in a
    /// waited submit. One upload larger than the cap is `out_of_memory` (review F3).
    #[allow(clippy::too_many_arguments)]
    fn stage_copy(&mut self, owner: &Owner, images: &[ImageId], copies: &mut Vec<Copy>, blob: &mut Vec<u8>, copy: Copy, bytes: &[u8]) -> Result<(), Refusal> {
        let cap = self.recorder.limits().staging_cap_bytes;
        if bytes.len() > cap {
            return Err(Refusal::OutOfMemory);
        }
        if staged_len(blob, bytes) > cap {
            self.flush(owner, images, copies, blob)?;
        }
        let offset = stage(blob, bytes)?;
        copies.push(Copy { offset, ..copy });
        Ok(())
    }

    /// Runs the staged uploads in their own waited submit and empties the blob.
    fn flush(&mut self, owner: &Owner, images: &[ImageId], copies: &mut Vec<Copy>, blob: &mut Vec<u8>) -> Result<(), Refusal> {
        if copies.is_empty() {
            return Ok(());
        }
        let core = Arc::clone(&self.core);
        self.write_staging(blob)?;
        let submit = owner.submitter();
        submit.begin(&core)?;
        self.record_textures(&core, submit.cmd, copies, images);
        submit.submit_wait(&core, &[], &[])?;
        self.stats.upload_submits = self.stats.upload_submits.saturating_add(1);
        copies.clear();
        blob.clear();
        Ok(())
    }

    /// Copies the blob into the staging buffer (grown up to the staging cap).
    fn write_staging(&mut self, blob: &[u8]) -> Result<(), Refusal> {
        if blob.is_empty() {
            return Ok(());
        }
        let core = Arc::clone(&self.core);
        let cap = self.recorder.limits().staging_cap_bytes;
        Self::buffer(&core, &mut self.staging, blob.len(), cap, vk::BufferUsageFlags::TRANSFER_SRC)?;
        if let Some(b) = self.staging.as_mut() {
            b.write_at(0, blob)?;
        }
        Ok(())
    }

    /// Records the uploads of `copies` and leaves every page, the dummy and the pass's image textures in
    /// SHADER_READ_ONLY_OPTIMAL.
    fn record_textures(&mut self, core: &Core, cmd: vk::CommandBuffer, copies: &[Copy], images: &[ImageId]) {
        let staging = self.staging.as_ref().map(|b| b.buffer).unwrap_or_default();
        let mut targets: Vec<(Target, &mut Tex)> = Vec::new();
        if let Some(t) = self.glyph.as_mut() {
            targets.push((Target::Glyph, t));
        }
        if let Some(t) = self.icon.as_mut() {
            targets.push((Target::Icon, t));
        }
        if let Some(t) = self.dummy.as_mut() {
            targets.push((Target::Dummy, t));
        }
        for (id, t) in self.images.iter_mut() {
            if images.contains(id) {
                targets.push((Target::Image(*id), &mut t.tex));
            }
        }
        for (target, tex) in targets {
            let mine: Vec<&Copy> = copies.iter().filter(|c| c.target == target).collect();
            if mine.is_empty() && tex.layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
                continue;
            }
            if !mine.is_empty() {
                image_barrier(
                    core,
                    cmd,
                    tex.image,
                    tex.layout,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::SHADER_READ,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                );
                let regions: Vec<vk::BufferImageCopy> = mine
                    .iter()
                    .map(|c| {
                        vk::BufferImageCopy::default()
                            .buffer_offset(c.offset)
                            .buffer_row_length(0)
                            .buffer_image_height(0)
                            .image_subresource(color_layers())
                            .image_offset(vk::Offset3D { x: c.x as i32, y: c.y as i32, z: 0 })
                            .image_extent(vk::Extent3D { width: c.w, height: c.h, depth: 1 })
                    })
                    .collect();
                // SAFETY: recording; the staging buffer holds every region (offsets and lengths from `blob`); the
                // image is TRANSFER_DST_OPTIMAL after the barrier; regions lie inside the image (atlas slots, image
                // extent).
                unsafe { core.device.cmd_copy_buffer_to_image(cmd, staging, tex.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &regions) };
                tex.layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
            }
            image_barrier(
                core,
                cmd,
                tex.image,
                tex.layout,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
            );
            tex.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_pass(&mut self, owner: &mut Owner, pane: PaneId, (pw, ph): (u32, u32), clear: Rgba8, pass: &PassPlan, first: bool, res: &dyn Resources) -> Result<(), Refusal> {
        let core = Arc::clone(&self.core);
        Self::page(&core, &mut self.glyph, pass.glyph_side, vk::Format::R8_UNORM)?;
        Self::page(&core, &mut self.icon, pass.icon_side, vk::Format::R8G8_UNORM)?;
        // Staging: page uploads, then image uploads whose bytes changed; offsets 4-aligned. Uploads that would pass the
        // staging cap go out first in their own waited submits (review F3).
        let mut images: Vec<ImageId> = Vec::new();
        for b in &pass.batches {
            if let Some(id) = b.image
                && !images.contains(&id)
            {
                images.push(id);
            }
        }
        let mut copies: Vec<Copy> = Vec::new();
        let mut blob: Vec<u8> = Vec::new();
        let pages = pass.glyph_uploads.iter().map(|u| (Target::Glyph, u)).chain(pass.icon_uploads.iter().map(|u| (Target::Icon, u)));
        for (target, u) in pages {
            self.stage_copy(owner, &images, &mut copies, &mut blob, Copy { target, offset: 0, x: u.x, y: u.y, w: u.w, h: u.h }, &u.bytes)?;
        }
        for id in &images {
            let Some(img) = res.image(*id) else { continue };
            let need = (img.width as usize).saturating_mul(img.height as usize).saturating_mul(4);
            let bytes = &img.rgba[..need.min(img.rgba.len())];
            let fresh = match self.images.get(id) {
                Some(t) if (t.tex.width, t.tex.height) == (img.width, img.height) => t.bytes != bytes,
                _ => {
                    if let Some(mut old) = self.images.remove(id) {
                        old.tex.destroy(&core);
                    }
                    let tex = Tex::new(&core, img.width, img.height, vk::Format::R8G8B8A8_UNORM)?;
                    self.images.insert(*id, ImageTex { tex, bytes: Vec::new() });
                    true
                }
            };
            if fresh {
                let mut kept = Vec::new();
                kept.try_reserve_exact(bytes.len()).map_err(|_| Refusal::OutOfMemory)?;
                kept.extend_from_slice(bytes);
                self.stage_copy(owner, &images, &mut copies, &mut blob, Copy { target: Target::Image(*id), offset: 0, x: 0, y: 0, w: img.width, h: img.height }, bytes)?;
                // Kept before the submit runs: a failed pass invalidates every image's bytes (draw, review F2).
                if let Some(t) = self.images.get_mut(id) {
                    t.bytes = kept;
                }
                self.stats.image_uploads = self.stats.image_uploads.saturating_add(1);
                self.stats.image_upload_bytes = self.stats.image_upload_bytes.saturating_add(bytes.len() as u64);
            }
        }
        self.stats.glyph_uploads = self.stats.glyph_uploads.saturating_add(pass.glyph_uploads.len() as u64);
        self.stats.glyph_upload_bytes = self.stats.glyph_upload_bytes.saturating_add(pass.glyph_uploads.iter().map(|u| u.bytes.len() as u64).sum());
        self.stats.icon_uploads = self.stats.icon_uploads.saturating_add(pass.icon_uploads.len() as u64);
        self.stats.icon_upload_bytes = self.stats.icon_upload_bytes.saturating_add(pass.icon_uploads.iter().map(|u| u.bytes.len() as u64).sum());
        self.write_staging(&blob)?;
        let inst_bytes: Vec<u8> = pass.instances.iter().flat_map(instance_bytes).collect();
        if !inst_bytes.is_empty() {
            let cap = self.recorder.limits().instance_cap_bytes;
            Self::buffer(&core, &mut self.instances, inst_bytes.len(), cap, vk::BufferUsageFlags::VERTEX_BUFFER)?;
            if let Some(b) = self.instances.as_mut() {
                b.write_at(0, &inst_bytes)?;
            }
        }
        // Descriptor sets: one for the page-only batches, one per image; the pool resets per pass (the previous
        // pass's submit was waited on).
        // SAFETY: no set of this pool is in use (every submit is waited on before the next pass records).
        unsafe { core.device.reset_descriptor_pool(self.pool, vk::DescriptorPoolResetFlags::empty()) }.map_err(vk_refusal)?;
        let (glyph_view, icon_view, dummy_view) = (
            self.glyph.as_ref().map(|t| t.view).unwrap_or_default(),
            self.icon.as_ref().map(|t| t.view).unwrap_or_default(),
            self.dummy.as_ref().map(|t| t.view).unwrap_or_default(),
        );
        let mut sets: Vec<(Option<ImageId>, vk::DescriptorSet)> = Vec::new();
        let mut wanted: Vec<(Option<ImageId>, vk::ImageView)> = vec![(None, dummy_view)];
        for id in &images {
            if let Some(t) = self.images.get(id) {
                wanted.push((Some(*id), t.tex.view));
            }
        }
        let layouts = vec![self.set_layout; wanted.len()];
        // SAFETY: the pool holds MAX_FRAME_IMAGES + 1 sets; a frame plans at most MAX_FRAME_IMAGES images.
        let allocated = unsafe { core.device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts)) }.map_err(vk_refusal)?;
        for ((key, view), set) in wanted.iter().zip(&allocated) {
            let infos: Vec<[vk::DescriptorImageInfo; 1]> = [glyph_view, icon_view, *view]
                .iter()
                .map(|v| [vk::DescriptorImageInfo::default().image_view(*v).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)])
                .collect();
            let writes: Vec<vk::WriteDescriptorSet> = infos
                .iter()
                .enumerate()
                .map(|(b, info)| {
                    vk::WriteDescriptorSet::default().dst_set(*set).dst_binding(b as u32).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(info)
                })
                .collect();
            // SAFETY: the set is not in use; the views are live.
            unsafe { core.device.update_descriptor_sets(&writes, &[]) };
            sets.push((*key, *set));
        }

        let (pane_image, pane_view, pane_layout) = owner.pane_target(pane)?;
        let submit = owner.submitter();
        submit.begin(&core)?;
        let cmd = submit.cmd;
        // Uploads and layouts of every sampled texture.
        self.record_textures(&core, cmd, &copies, &images);
        // The pane: from its recorded layout to COLOR_ATTACHMENT_OPTIMAL (the first pass clears, so UNDEFINED is fine).
        image_barrier(
            &core,
            cmd,
            pane_image,
            if first { vk::ImageLayout::UNDEFINED } else { pane_layout },
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE,
            vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        );
        let clear_value = vk::ClearValue { color: vk::ClearColorValue { float32: [clear.r, clear.g, clear.b, clear.a].map(|c| f32::from(c) / 255.0) } };
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(pane_view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(if first { vk::AttachmentLoadOp::CLEAR } else { vk::AttachmentLoadOp::LOAD })
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(clear_value)];
        let area = vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: pw, height: ph } };
        let rendering = vk::RenderingInfo::default().render_area(area).layer_count(1).color_attachments(&attachment);
        let viewport = [vk::Viewport { x: 0.0, y: 0.0, width: pw as f32, height: ph as f32, min_depth: 0.0, max_depth: 1.0 }];
        let size = [(pw as f32).to_le_bytes(), (ph as f32).to_le_bytes()].concat();
        let vertex = self.instances.as_ref().map(|b| b.buffer).unwrap_or_default();
        // SAFETY: recording; the pane view is a COLOR_ATTACHMENT R8G8B8A8_UNORM view of a live pane in
        // COLOR_ATTACHMENT_OPTIMAL; dynamicRendering is enabled; the pipeline, layout, sets and the vertex buffer are
        // live; every batch range lies inside the instances written above; the scissor is the pane (offset 0,
        // VUID-vkCmdSetScissor-x-00595).
        unsafe {
            core.device.cmd_begin_rendering(cmd, &rendering);
            core.device.cmd_set_viewport(cmd, 0, &viewport);
            core.device.cmd_set_scissor(cmd, 0, &[area]);
            if !pass.instances.is_empty() {
                core.device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
                core.device.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::VERTEX, 0, &size);
                core.device.cmd_bind_vertex_buffers(cmd, 0, &[vertex], &[0]);
                let mut bound: Option<Option<ImageId>> = None;
                for b in &pass.batches {
                    if bound != Some(b.image) {
                        let set = sets.iter().find(|(k, _)| *k == b.image).map(|(_, s)| *s);
                        let Some(set) = set else { continue };
                        core.device.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &[set], &[]);
                        bound = Some(b.image);
                    }
                    core.device.cmd_draw(cmd, 4, b.count, 0, b.first);
                }
            }
            core.device.cmd_end_rendering(cmd);
        }
        image_barrier(
            &core,
            cmd,
            pane_image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::AccessFlags::TRANSFER_READ,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::PipelineStageFlags::TRANSFER,
        );
        submit.submit_wait(&core, &[], &[])?;
        owner.set_pane_layout(pane, vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
        self.stats.passes = self.stats.passes.saturating_add(1);
        self.stats.instances = self.stats.instances.saturating_add(pass.instances.len() as u64);
        Ok(())
    }
}

fn instance_bytes(i: &Instance) -> Vec<u8> {
    let mut out = Vec::with_capacity(INSTANCE_BYTES);
    for v in i.quad {
        out.extend(v.to_le_bytes());
    }
    out.extend(i.kind.to_le_bytes());
    out.extend(i.sel.to_le_bytes());
    for v in i.org {
        out.extend(v.to_le_bytes());
    }
    for v in i.colors {
        out.extend(v.to_le_bytes());
    }
    for v in i.params {
        out.extend(v.to_le_bytes());
    }
    out
}

impl Drop for UiPass {
    fn drop(&mut self) {
        let core = Arc::clone(&self.core);
        // SAFETY: wait idle, then destroy every object this pass created; the Core outlives it (Arc).
        unsafe {
            let _ = core.device.device_wait_idle();
        }
        for t in [self.glyph.take(), self.icon.take(), self.dummy.take()].into_iter().flatten() {
            let mut t = t;
            t.destroy(&core);
        }
        for (_, mut t) in self.images.drain() {
            t.tex.destroy(&core);
        }
        for b in [self.instances.take(), self.staging.take()].into_iter().flatten() {
            let mut b = b;
            b.destroy(&core);
        }
        // SAFETY: the device is idle; null handles are ignored by the destroy calls.
        unsafe {
            core.device.destroy_pipeline(self.pipeline, None);
            core.device.destroy_pipeline_layout(self.layout, None);
            core.device.destroy_descriptor_pool(self.pool, None);
            core.device.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}
