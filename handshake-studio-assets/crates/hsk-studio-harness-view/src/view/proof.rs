//! Soft proofing in the window (lane SMB-PS-SOFT-PROOF-2; STU-COL-240..244, STU-COL-164,
//! STU-SHL-025 / STU-SHL-030): the VIEW > Proof and COLOR > Proof submenus, the proof setup window,
//! the proof preview and gamut overlay on the canvas, and the `proof` inspection state. The engine is
//! Prism's (`prepare_soft_proof`, `SoftProof::run`, `SoftProof::gamut_warning`,
//! `GamutWarning::apply_overlay`); the document-scoped state is Folio's `StudioProofRecords`, kept in
//! the session document entry `proof` and validated by Folio on every write and every read.
//!
//! One implementation, two projections (STU-COL-250): every menu leaf and every window control calls
//! [`Viewer::proof_command`] (the typed model surface) or opens the window page that does; every
//! change writes the whole record set as ONE undo step (`Run::set_tiles_and_entry`, entry only), so
//! undo / redo restore the previous proof state and save / reopen keep it (the package carries the
//! entry `studio/proof.<sha256>`).
//!
//! Canvas (STU-COL-164: an overlay computed from the proof branch, never composited into the
//! document, never in an export): with Proof Colours on, the canvas texture is `SoftProof::run` over
//! the composite read-back; with Gamut Warning on, `apply_overlay` paints the configured colour on the
//! marked pixels of what is shown. The rendered frame (`Viewer::frame`), the document and every export
//! (Export PNG / JPEG encode `Frame::rgba8`) stay unproofed.
//!
//! Studio readings, declared (lane notes `spec_deltas`):
//! - the proof source is the canvas composite in the viewer's working space
//!   (`WORKING_PROFILE_LINEAR_SRGB`, linear-light sRGB, render.rs), read back as the frame's 8-bit
//!   sRGB encoding decoded to linear light ([`proof_source`]); the display profile is sRGB v4
//!   (`develop::SRGB_V4_ICC`); the shipped `working_rgb` preset proofs to the same working space;
//!   `working_cmyk` proofs to the document's CMYK working profile (the document `working_space_cmyk`,
//!   else the vector records' CMYK `StudioIccProfile` marked `working_space`, bytes from the vector
//!   entry), refused with a typed status when the document has none;
//! - an ICC proof profile reference (`StudioProofProfile::Icc`) names a `StudioIccProfile` of the
//!   vector records; its bytes come from the vector entry and must hash to the referenced digest;
//! - choosing a condition (Proof Setup, Custom Proof Condition, Colour-Vision Proof) makes it the
//!   active condition and turns Proof Colours on; Proof Colours or Gamut Warning turned on with no
//!   active condition selects `working_cmyk` when the document has a CMYK working profile, else
//!   `working_rgb` (Photoshop defaults UNVERIFIED);
//! - Save Proof Preset saves the active condition (its six fields) under a new name; the active
//!   condition stays;
//! - the GPU shared-device display path draws straight into a texture with no CPU buffer: while a
//!   proof view is on, the canvas shows the read-back path's frame (render-cpu, as Export does) and the
//!   state names it (`read_back`), never a silent fallback;
//! - one Prism call covers at most `MAX_WIDE_PIXELS` pixels: a larger frame is refused for proof
//!   (`proof_pixel_limit`) and the canvas shows the unproofed frame with the reason in the state;
//! - the gamut warning judges every pixel's colour, transparent ones included (straight alpha; the
//!   overlay keeps each pixel's alpha).
//!
//! No key bindings: the spec declares none for these leaves (vendor bindings UNVERIFIED).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::render::{Frame, Prefer};
use hsk_studio_accord::DomainId;
use hsk_studio_controls::menu::{Availability as A, MenuNode, ReasonCode};
use hsk_studio_folio::{
    self as folio, ProofColourVision, ShippedProofProfile, StudioColorModel, StudioProofConditionRecord, StudioProofIntent, StudioProofProfile,
    StudioProofRecords,
};
use hsk_studio_prism::{
    self as prism, ColourVision, GamutWarning, GamutWarningStyle, Intent, OperatorSimulation, ProfileInput, ProofCondition, ProofProfile, ProofReceipt,
    SoftProof,
};
use serde_json::{Value, json};

/// Session document entry token of the proof records.
pub const ENTRY: &str = "proof";
/// VIEW > Proof: the spec's submenu id (STU-SHL-025).
pub const VIEW_MENU_ID: &str = "studio.color.proof";
/// COLOR > Proof (STU-SHL-030 gives no id; Studio reading: a sibling of the VIEW id, because a menu
/// id appears once in the bar).
pub const COLOR_MENU_ID: &str = "studio.color.proof_menu";
/// Profile ids of the viewer's proof chain (fixed: the bytes are fixed).
pub const DOCUMENT_PROFILE_ID: &str = "SCPF-019abcde-0000-7000-8000-00000000b001";
pub const DISPLAY_PROFILE_ID: &str = "SCPF-019abcde-0000-7000-8000-00000000b002";
pub const WORKING_RGB_PROFILE_ID: &str = "SCPF-019abcde-0000-7000-8000-00000000b003";

/// Stable widget names of the proof window.
pub mod widget {
    pub const WINDOW: &str = "proof.window";
    pub const CLOSE: &str = "proof.close";
    pub const GAMUT_COLOUR: &str = "proof.gamut.colour";
    pub const GAMUT_OPACITY: &str = "proof.gamut.opacity";
    pub const CUSTOM_NAME: &str = "proof.custom.name";
    pub const CUSTOM_PROFILE: &str = "proof.custom.profile";
    pub const CUSTOM_PROOF_INTENT: &str = "proof.custom.proof_intent";
    pub const CUSTOM_DISPLAY_INTENT: &str = "proof.custom.display_intent";
    pub const CUSTOM_BPC: &str = "proof.custom.black_point_compensation";
    pub const CUSTOM_PAPER: &str = "proof.custom.simulate_paper_color";
    pub const CUSTOM_INK: &str = "proof.custom.simulate_black_ink";
    pub const CUSTOM_APPLY: &str = "proof.custom.apply";
    pub const SAVE_NAME: &str = "proof.save.name";
    pub const SAVE_BUTTON: &str = "proof.save.button";
    /// Preset row `i` of the Proof Setup page (shipped presets first, then saved conditions).
    pub fn preset(i: usize) -> String {
        format!("proof.preset.{i}")
    }
    /// Colour-vision button (`protanopia`, `deuteranopia`, `tritanopia`).
    pub fn vision(code: &str) -> String {
        format!("proof.vision.{code}")
    }
}

/// The menu a proof leaf sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofMenu {
    View,
    Color,
}

/// The proof leaves of STU-SHL-025 (VIEW) and STU-SHL-030 (COLOR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofLeaf {
    Setup,
    CustomCondition,
    Colours,
    GamutWarning,
    ColourVision,
    SavePreset,
}

impl ProofLeaf {
    /// VIEW > Proof in spec order.
    pub const VIEW: [ProofLeaf; 4] = [Self::Setup, Self::Colours, Self::GamutWarning, Self::ColourVision];
    /// COLOR > Proof in spec order.
    pub const COLOR: [ProofLeaf; 6] = [Self::Setup, Self::CustomCondition, Self::Colours, Self::GamutWarning, Self::ColourVision, Self::SavePreset];
    /// Leaf id suffix (declared Studio reading: the spec declares no leaf ids).
    pub fn key(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::CustomCondition => "custom_condition",
            Self::Colours => "colours",
            Self::GamutWarning => "gamut_warning",
            Self::ColourVision => "colour_vision",
            Self::SavePreset => "save_preset",
        }
    }
    /// The spec's leaf label (STU-SHL-008 spellings).
    pub fn label(self) -> &'static str {
        match self {
            Self::Setup => "Proof Setup…",
            Self::CustomCondition => "Custom Proof Condition…",
            Self::Colours => "Proof Colours",
            Self::GamutWarning => "Gamut Warning",
            Self::ColourVision => "Colour-Vision Proof…",
            Self::SavePreset => "Save Proof Preset…",
        }
    }
    /// The window page a dialog leaf opens (`None`: a toggle).
    pub fn page(self) -> Option<ProofPage> {
        match self {
            Self::Setup => Some(ProofPage::Setup),
            Self::CustomCondition => Some(ProofPage::Custom),
            Self::ColourVision => Some(ProofPage::ColourVision),
            Self::SavePreset => Some(ProofPage::SavePreset),
            Self::Colours | Self::GamutWarning => None,
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::Setup => "Chooses the proof condition (shipped presets and the document's saved conditions) and the gamut warning colour and opacity.",
            Self::CustomCondition => "Defines a proof condition: proof profile, proof and display intents, black point compensation, paper colour and black ink.",
            Self::Colours => "Shows the document through the active proof condition; document values and exports stay unproofed.",
            Self::GamutWarning => "Marks the pixels the proof device cannot reproduce with the warning colour.",
            Self::ColourVision => "Shows the document as seen with protanopia, deuteranopia or tritanopia.",
            Self::SavePreset => "Saves the active proof condition under a name in the document.",
        }
    }
}

/// One proof menu leaf: its menu and leaf (the shell command of the leaf).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofMenuLeaf {
    pub menu: ProofMenu,
    pub leaf: ProofLeaf,
}

impl ProofMenuLeaf {
    /// `studio.color.proof.<leaf>` (VIEW) or `studio.color.proof_menu.<leaf>` (COLOR).
    pub fn id(self) -> String {
        let menu = match self.menu {
            ProofMenu::View => VIEW_MENU_ID,
            ProofMenu::Color => COLOR_MENU_ID,
        };
        format!("{menu}.{}", self.leaf.key())
    }
    /// Every proof leaf: VIEW's, then COLOR's.
    pub fn all() -> Vec<ProofMenuLeaf> {
        let view = ProofLeaf::VIEW.into_iter().map(|leaf| ProofMenuLeaf { menu: ProofMenu::View, leaf });
        let color = ProofLeaf::COLOR.into_iter().map(|leaf| ProofMenuLeaf { menu: ProofMenu::Color, leaf });
        view.chain(color).collect()
    }
}

/// Pages of the proof window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofPage {
    Setup,
    Custom,
    ColourVision,
    SavePreset,
}

impl ProofPage {
    pub fn key(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Custom => "custom_condition",
            Self::ColourVision => "colour_vision",
            Self::SavePreset => "save_preset",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Setup => "Proof Setup",
            Self::Custom => "Custom Proof Condition",
            Self::ColourVision => "Colour-Vision Proof",
            Self::SavePreset => "Save Proof Preset",
        }
    }
}

/// The typed proof surface ([`Viewer::proof_command`]); each call is one undo step.
#[derive(Clone, Debug, PartialEq)]
pub enum ProofCommand {
    /// Proof Setup: make a shipped preset or a saved condition active (Proof Colours on).
    Select(String),
    /// Custom Proof Condition: add (or replace by name) a saved condition and make it active (Proof
    /// Colours on).
    Custom(StudioProofConditionRecord),
    /// Proof Colours on / off.
    SetProofColours(bool),
    /// Gamut Warning on / off.
    SetGamutWarning(bool),
    /// Gamut warning colour (display RGB 0..=1) and opacity (percent 0..=100).
    SetGamutStyle { colour: [f64; 3], opacity: u32 },
    /// Colour-Vision Proof: make that shipped condition active (Proof Colours on).
    SelectColourVision(ProofColourVision),
    /// Save Proof Preset: save the active condition under a new name.
    SavePreset(String),
}

impl ProofCommand {
    fn op(&self) -> &'static str {
        match self {
            Self::Select(_) => "proof_select",
            Self::Custom(_) => "proof_custom_condition",
            Self::SetProofColours(_) => "proof_colours",
            Self::SetGamutWarning(_) => "proof_gamut_warning",
            Self::SetGamutStyle { .. } => "proof_gamut_style",
            Self::SelectColourVision(_) => "proof_colour_vision",
            Self::SavePreset(_) => "proof_save_preset",
        }
    }
}

/// What the canvas shows while a proof view is on.
#[derive(Clone, Debug)]
pub struct ProofView {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, display sRGB, straight alpha (the frame's alpha).
    pub rgba8: Vec<u8>,
    /// The proof run's receipt (`None` while only the gamut warning is on).
    pub receipt: Option<ProofReceipt>,
    /// The gamut warning (`None` while it is off).
    pub warning: Option<GamutWarning>,
    /// The active condition (name and the six fields).
    pub condition: StudioProofConditionRecord,
    /// The frame came from the read-back path because the shown frame lives in the display texture.
    pub read_back: bool,
}

/// Cache key: the entry bytes, the frame generation and the document revision.
type Key = (Option<Vec<u8>>, u64, Option<u64>);

/// Proof window state and the canvas cache (module docs).
#[derive(Default)]
pub(super) struct ProofState {
    page: Option<ProofPage>,
    draft: Option<StudioProofConditionRecord>,
    draft_matrix: [f64; 9],
    save_name: String,
    frame_gen: u64,
    cache: Option<(Key, Result<ProofView, String>)>,
    texture: Option<(Key, egui::TextureHandle)>,
    /// The last canvas drew the proof texture.
    shown: bool,
}

impl ProofState {
    /// The canvas frame changed (a render landed or the document closed).
    pub(super) fn frame_changed(&mut self) {
        self.frame_gen = self.frame_gen.wrapping_add(1);
    }
}

fn intent(i: StudioProofIntent) -> Intent {
    match i {
        StudioProofIntent::Perceptual => Intent::Perceptual,
        StudioProofIntent::RelativeColorimetric => Intent::RelativeColorimetric,
        StudioProofIntent::Saturation => Intent::Saturation,
        StudioProofIntent::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
    }
}

fn vision_code(v: ProofColourVision) -> &'static str {
    match v {
        ProofColourVision::Protanopia => "protanopia",
        ProofColourVision::Deuteranopia => "deuteranopia",
        ProofColourVision::Tritanopia => "tritanopia",
    }
}

/// The shipped preset `name` as a condition record with Prism's defaults (relative colorimetric on
/// both stages, black point compensation on, paper and black ink off; `ProofCondition::new`).
pub fn shipped_record(name: &str) -> Option<StudioProofConditionRecord> {
    let proof_profile = match name {
        "working_cmyk" => StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingCmyk },
        "working_rgb" => StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingRgb },
        "legacy_uncompensated_rgb" => StudioProofProfile::Shipped { preset: ShippedProofProfile::LegacyUncompensatedRgb },
        "protanopia" => StudioProofProfile::ColourVision { vision: ProofColourVision::Protanopia },
        "deuteranopia" => StudioProofProfile::ColourVision { vision: ProofColourVision::Deuteranopia },
        "tritanopia" => StudioProofProfile::ColourVision { vision: ProofColourVision::Tritanopia },
        _ => return None,
    };
    Some(StudioProofConditionRecord {
        name: name.to_string(),
        proof_profile,
        proof_intent: StudioProofIntent::RelativeColorimetric,
        display_intent: StudioProofIntent::RelativeColorimetric,
        black_point_compensation: true,
        simulate_paper_color: false,
        simulate_black_ink: false,
    })
}

/// The proof source of a frame: its 8-bit sRGB bytes decoded to linear light, RGB interleaved
/// (module docs; the alpha byte is dropped).
pub fn proof_source(rgba8: &[u8]) -> Vec<f32> {
    let lut: Vec<f32> = (0..=255u8)
        .map(|b| {
            let c = f32::from(b) / 255.0;
            if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    rgba8.chunks_exact(4).flat_map(|p| [lut[usize::from(p[0])], lut[usize::from(p[1])], lut[usize::from(p[2])]]).collect()
}

/// Display RGB floats (unclamped) with the frame's alpha -> 8-bit RGBA.
fn to_rgba8(display: &[f32], frame: &[u8]) -> Vec<u8> {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    display.chunks_exact(3).zip(frame.chunks_exact(4)).flat_map(|(c, p)| [q(c[0]), q(c[1]), q(c[2]), p[3]]).collect()
}

/// `Code::InvalidJson` -> `invalid_json`.
fn snake(code: folio::Code) -> String {
    let text = format!("{code:?}");
    let mut out = String::with_capacity(text.len().saturating_add(4));
    for (i, ch) in text.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn id(text: &str) -> Result<DomainId, String> {
    DomainId::parse(text).map_err(|_| format!("proof: profile id {text}"))
}

fn hex(bytes: &[u8; 32]) -> String {
    prism::hash_hex(bytes)
}

impl Viewer {
    // ----- records ---------------------------------------------------------------------------

    /// The proof records of the open document, validated by Folio (`Ok(None)`: none stored or nothing
    /// open). A stored entry Folio refuses reads as the Folio diagnostic and stays stored unchanged.
    pub fn proof_records(&self) -> Result<Option<StudioProofRecords>, folio::Diagnostic> {
        let Some(snap) = self.run.snapshot() else { return Ok(None) };
        let Some(bytes) = self.run.document_entry(ENTRY) else { return Ok(None) };
        folio::parse_proof_records(bytes, snap.document(), folio::Budget::default(), self.cancel).map(Some)
    }

    /// Runs one typed proof command (module docs): Folio validates the new record set, the session
    /// entry `proof` takes it as ONE undo step. Refusals change nothing and are said in the status log.
    pub fn proof_command(&mut self, command: ProofCommand) -> Result<(), String> {
        let op = command.op();
        let result = self.proof_command_inner(command);
        match &result {
            Ok(()) => {
                let active = self.proof_records().ok().flatten().and_then(|r| r.active_condition).unwrap_or_default();
                self.say(format!("{op}: proof state written (active {active:?})"));
                self.proof_refresh();
            }
            Err(e) => self.say(format!("{op}: {e}")),
        }
        result
    }

    fn proof_command_inner(&mut self, command: ProofCommand) -> Result<(), String> {
        let snap = self.run.snapshot().ok_or("nothing open to proof")?;
        let doc = snap.document();
        let document_id = doc.document_id.clone();
        let mut r = match self.proof_records() {
            Ok(Some(r)) => r,
            Ok(None) => StudioProofRecords::new(document_id.clone()),
            Err(e) => return Err(format!("stored proof records refused by folio: {} at {}", snake(e.code), e.target)),
        };
        match command.clone() {
            ProofCommand::Select(name) => {
                if r.condition(&name).is_none() && !StudioProofRecords::is_shipped(&name) {
                    return Err(format!("no proof condition named {name:?}"));
                }
                r.active_condition = Some(name);
                r.proof_colours = true;
            }
            ProofCommand::Custom(c) => {
                let name = c.name.clone();
                match r.conditions.iter_mut().find(|x| x.name == name) {
                    Some(x) => *x = c,
                    None => r.conditions.push(c),
                }
                r.active_condition = Some(name);
                r.proof_colours = true;
            }
            ProofCommand::SetProofColours(on) => {
                if on && r.active_condition.is_none() {
                    r.active_condition = Some(self.proof_default_condition().into());
                }
                r.proof_colours = on;
            }
            ProofCommand::SetGamutWarning(on) => {
                if on && r.active_condition.is_none() {
                    r.active_condition = Some(self.proof_default_condition().into());
                }
                r.gamut_warning.enabled = on;
            }
            ProofCommand::SetGamutStyle { colour, opacity } => {
                r.gamut_warning.colour = colour;
                r.gamut_warning.opacity = opacity;
            }
            ProofCommand::SelectColourVision(v) => {
                r.active_condition = Some(vision_code(v).into());
                r.proof_colours = true;
            }
            ProofCommand::SavePreset(name) => {
                let active = r.active_condition.clone().ok_or("no active proof condition to save")?;
                let mut c = r.condition(&active).cloned().or_else(|| shipped_record(&active)).ok_or("the active proof condition is gone")?;
                if r.condition(&name).is_some() {
                    return Err(format!("a proof condition named {name:?} exists"));
                }
                c.name = name;
                r.conditions.push(c);
            }
        }
        folio::validate_proof_records(&r, doc, folio::Budget::default(), self.cancel).map_err(|e| format!("folio refused: {} at {}", snake(e.code), e.target))?;
        let bytes = serde_json::to_vec(&r).map_err(|e| format!("proof records encoding: {e}"))?;
        if self.run.set_tiles_and_entry(command.op(), &document_id, None, ENTRY, bytes) {
            Ok(())
        } else {
            Err(self.op_error())
        }
    }

    /// The condition a toggle selects when none is active (module docs).
    fn proof_default_condition(&self) -> &'static str {
        if self.proof_working_cmyk().is_ok() { "working_cmyk" } else { "working_rgb" }
    }

    /// The saved or shipped condition `name` of `records`.
    fn proof_condition_record(records: &StudioProofRecords, name: &str) -> Result<StudioProofConditionRecord, String> {
        records.condition(name).cloned().or_else(|| shipped_record(name)).ok_or_else(|| format!("proof: no condition named {name:?}"))
    }

    // ----- profiles ----------------------------------------------------------------------------

    /// The document's CMYK working profile: its `SCPF` id and bytes (module docs).
    fn proof_working_cmyk(&self) -> Result<(String, Vec<u8>), String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let doc = snap.document();
        let entry = self.vector_entry()?;
        let id = match &doc.working_space_cmyk {
            folio::ProfileBinding::Assigned(id) => Some(id.clone()),
            folio::ProfileBinding::Unassigned(()) => None,
        }
        .or_else(|| entry.records.working_icc(StudioColorModel::Cmyk).map(|p| p.profile_id.clone()))
        .ok_or("proof_no_cmyk_working_profile: the document has no CMYK working profile")?;
        let record = entry.records.icc_profile(&id).filter(|p| p.colour_space == StudioColorModel::Cmyk).ok_or_else(|| format!("proof_profile_unresolved: {id}"))?;
        let bytes = entry.icc.get(&record.content_digest.digest).cloned().ok_or_else(|| format!("proof_profile_bytes_missing: {id}"))?;
        Ok((id, bytes))
    }

    /// The bytes of an ICC proof profile reference (module docs).
    fn proof_icc(&self, profile_id: &str, digest: &folio::ContentDigest) -> Result<Vec<u8>, String> {
        let entry = self.vector_entry()?;
        let record = entry.records.icc_profile(profile_id).ok_or_else(|| format!("proof_profile_unresolved: {profile_id}"))?;
        if record.content_digest != *digest {
            return Err(format!("proof_profile_digest_mismatch: {profile_id}"));
        }
        let bytes = entry.icc.get(&digest.digest).cloned().ok_or_else(|| format!("proof_profile_bytes_missing: {profile_id}"))?;
        if hex(&prism::profile_hash(&bytes)) != digest.digest {
            return Err(format!("proof_profile_digest_mismatch: {profile_id}"));
        }
        Ok(bytes)
    }

    /// Prism's soft proof of `c` from the viewer's working space to the sRGB display.
    pub fn proof_prepare(&self, c: &StudioProofConditionRecord) -> Result<SoftProof, String> {
        let (doc_id, disp_id, rgb_id) = (id(DOCUMENT_PROFILE_ID)?, id(DISPLAY_PROFILE_ID)?, id(WORKING_RGB_PROFILE_ID)?);
        let working = prism::WORKING_PROFILE_LINEAR_SRGB;
        let display = super::develop::SRGB_V4_ICC;
        // Owned profile bytes first; the condition borrows them.
        let icc: Option<(DomainId, Vec<u8>)> = match &c.proof_profile {
            StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingCmyk } => {
                let (pid, bytes) = self.proof_working_cmyk()?;
                Some((id(&pid)?, bytes))
            }
            StudioProofProfile::Icc { profile_id, content_digest } => Some((id(profile_id)?, self.proof_icc(profile_id, content_digest)?)),
            _ => None,
        };
        let proof_profile = match (&c.proof_profile, &icc) {
            (_, Some((pid, bytes))) => ProofProfile::Icc(ProfileInput { profile_id: pid, bytes, expected_sha256: prism::profile_hash(bytes) }),
            (StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingRgb }, None) => {
                ProofProfile::Icc(ProfileInput { profile_id: &rgb_id, bytes: working, expected_sha256: prism::profile_hash(working) })
            }
            (StudioProofProfile::Shipped { .. }, None) => ProofProfile::LegacyUncompensatedRgb,
            (StudioProofProfile::ColourVision { vision }, None) => ProofProfile::ColourVision(match vision {
                ProofColourVision::Protanopia => ColourVision::Protanopia,
                ProofColourVision::Deuteranopia => ColourVision::Deuteranopia,
                ProofColourVision::Tritanopia => ColourVision::Tritanopia,
            }),
            (StudioProofProfile::OperatorMatrix { matrix: m }, None) => {
                let rows = [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]];
                ProofProfile::OperatorSimulation(OperatorSimulation::new(rows).map_err(|e| format!("proof: {}", e.code()))?)
            }
            (StudioProofProfile::Icc { .. }, None) => return Err("proof: unresolved ICC profile".into()),
        };
        let condition = ProofCondition {
            name: c.name.clone(),
            proof_profile,
            proof_intent: intent(c.proof_intent),
            display_intent: intent(c.display_intent),
            black_point_compensation: c.black_point_compensation,
            simulate_paper_color: c.simulate_paper_color,
            simulate_black_ink: c.simulate_black_ink,
        };
        let document = ProfileInput { profile_id: &doc_id, bytes: working, expected_sha256: prism::profile_hash(working) };
        let display = ProfileInput { profile_id: &disp_id, bytes: display, expected_sha256: prism::profile_hash(display) };
        prism::prepare_soft_proof(document, &condition, display, self.cancel).map_err(|e| format!("proof: {}", e.code()))
    }

    // ----- canvas ------------------------------------------------------------------------------

    fn proof_key(&self) -> Key {
        (self.run.document_entry(ENTRY).map(<[u8]>::to_vec), self.proof.frame_gen, self.run.snapshot().map(|s| s.document().revision))
    }

    /// The proof view of the current frame (`Ok(None)`: no proof view is on). Pure: the canvas caches it.
    pub fn proof_view_now(&self) -> Result<Option<ProofView>, String> {
        let records = match self.proof_records() {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(None),
            Err(e) => return Err(format!("stored proof records refused by folio: {} at {}", snake(e.code), e.target)),
        };
        if !records.proof_colours && !records.gamut_warning.enabled {
            return Ok(None);
        }
        let active = records.active_condition.clone().ok_or("proof: no active condition")?;
        let condition = Self::proof_condition_record(&records, &active)?;
        let proof = self.proof_prepare(&condition)?;
        let shown = self.frame.as_ref().ok_or("proof: no rendered frame")?;
        let read_back_frame: Frame;
        let (frame, read_back) = if shown.rgba8.is_empty() {
            let snap = self.run.snapshot().ok_or("nothing open")?;
            let text = self.prepared_text()?;
            let vectors = self.prepared_vectors()?;
            read_back_frame = super::render::render_full_with_records(snap, &self.assets, Some(&text), vectors.as_ref(), self.records.as_ref(), Prefer::Cpu, None, None, self.cancel)
                .map_err(|e| format!("proof read-back: {e}"))?;
            (&read_back_frame, true)
        } else {
            (shown, false)
        };
        let width = usize::try_from(frame.width).map_err(|_| "proof: width")?;
        let pixels = frame.rgba8.len().checked_div(4).unwrap_or(0);
        if pixels > prism::MAX_WIDE_PIXELS {
            return Err(format!("proof_pixel_limit: {pixels} pixels > {}", prism::MAX_WIDE_PIXELS));
        }
        let source = proof_source(&frame.rgba8);
        let mut receipt = None;
        let mut display: Vec<f32> = if records.proof_colours {
            let preview = proof.run(&source, width, true, self.cancel).map_err(|e| format!("proof: {}", e.code()))?;
            receipt = Some(preview.receipt);
            preview.pixels
        } else {
            frame.rgba8.chunks_exact(4).flat_map(|p| [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]).collect()
        };
        let mut warning = None;
        if records.gamut_warning.enabled {
            let g = &records.gamut_warning;
            let colour = [g.colour[0] as f32, g.colour[1] as f32, g.colour[2] as f32];
            let style = GamutWarningStyle::new(colour, g.opacity).map_err(|e| format!("proof: {}", e.code()))?;
            let w = proof.gamut_warning(&source, width, style, self.cancel).map_err(|e| format!("proof: {}", e.code()))?;
            display = w.apply_overlay(&display).map_err(|e| format!("proof: {}", e.code()))?;
            warning = Some(w);
        }
        Ok(Some(ProofView { width: frame.width, height: frame.height, rgba8: to_rgba8(&display, &frame.rgba8), receipt, warning, condition, read_back }))
    }

    /// Recomputes the cached proof view when the entry, the frame or the document changed.
    pub(super) fn proof_refresh(&mut self) {
        let key = self.proof_key();
        if self.proof.cache.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let view = self.proof_view_now().and_then(|v| v.ok_or_else(String::new));
        if let Err(e) = &view
            && !e.is_empty()
        {
            self.say(format!("proof view off: {e}"));
        }
        if let Ok(v) = &view
            && v.read_back
        {
            self.say("proof: the GPU display texture has no CPU buffer; the proof view uses the read-back frame (render-cpu)".into());
        }
        self.proof.cache = Some((key, view));
    }

    /// The canvas bytes of the proof view, as the canvas texture holds them (`Ok(None)`: no proof view
    /// is on, the canvas shows the unproofed frame).
    pub fn proof_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.render_if_dirty();
        self.proof_refresh();
        match self.proof.cache.as_ref().map(|(_, v)| v) {
            Some(Ok(v)) => Ok(Some(v.rgba8.clone())),
            Some(Err(e)) if e.is_empty() => Ok(None),
            Some(Err(e)) => Err(e.clone()),
            None => Ok(None),
        }
    }

    /// The canvas texture of the proof view (`None`: show the frame). Called by the canvas.
    pub(super) fn proof_canvas_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        self.proof_refresh();
        let state = &mut self.proof;
        let Some((key, Ok(view))) = state.cache.as_ref() else {
            state.shown = false;
            return None;
        };
        if state.texture.as_ref().is_none_or(|(k, _)| k != key) {
            let image = egui::ColorImage::from_rgba_unmultiplied([view.width as usize, view.height as usize], &view.rgba8);
            let handle = ctx.load_texture("hsk-studio-ui.view.proof", image, egui::TextureOptions::NEAREST);
            state.texture = Some((key.clone(), handle));
        }
        state.shown = true;
        state.texture.as_ref().map(|(_, t)| t.id())
    }

    /// The canvas showed the frame (no proof view).
    pub(super) fn proof_canvas_unproofed(&mut self) {
        self.proof.shown = false;
    }

    // ----- menu --------------------------------------------------------------------------------

    /// The proof leaves of `menu` with ONE availability evaluation (no document: inapplicable, remedy
    /// File > Open).
    pub(super) fn proof_menu_nodes(&self, menu: ProofMenu) -> Vec<MenuNode> {
        let open = self.run.snapshot().is_some();
        let records = self.proof_records().ok().flatten();
        let colours = records.as_ref().is_some_and(|r| r.proof_colours);
        let gamut = records.as_ref().is_some_and(|r| r.gamut_warning.enabled);
        let leaves: &[ProofLeaf] = match menu {
            ProofMenu::View => &ProofLeaf::VIEW,
            ProofMenu::Color => &ProofLeaf::COLOR,
        };
        leaves
            .iter()
            .map(|leaf| {
                let id = ProofMenuLeaf { menu, leaf: *leaf }.id();
                let mut n = MenuNode::item(&id, leaf.label()).with_help(leaf.help());
                n = match leaf {
                    ProofLeaf::Colours => n.selected(colours),
                    ProofLeaf::GamutWarning => n.selected(gamut),
                    _ => n,
                };
                n.with_availability(if open { A::available() } else { A::inapplicable(ReasonCode::NoSuchContainer, "no open document to proof", &super::shell::ShellCommand::Open.id()) })
            })
            .collect()
    }

    /// Runs one proof menu leaf: the toggles flip through [`Viewer::proof_command`], the dialog leaves
    /// open their window page.
    pub(super) fn proof_menu_leaf(&mut self, leaf: ProofMenuLeaf) -> Result<(), String> {
        if self.run.snapshot().is_none() {
            return Err("no open document to proof".into());
        }
        let records = self.proof_records().map_err(|e| format!("stored proof records refused by folio: {} at {}", snake(e.code), e.target))?;
        match leaf.leaf {
            ProofLeaf::Colours => self.proof_command(ProofCommand::SetProofColours(!records.as_ref().is_some_and(|r| r.proof_colours))),
            ProofLeaf::GamutWarning => self.proof_command(ProofCommand::SetGamutWarning(!records.as_ref().is_some_and(|r| r.gamut_warning.enabled))),
            other => {
                let page = other.page().ok_or("proof: no page")?;
                self.proof_open_page(page);
                Ok(())
            }
        }
    }

    /// Opens the proof window on `page` (the custom page starts from the active condition).
    pub fn proof_open_page(&mut self, page: ProofPage) {
        if page == ProofPage::Custom {
            let records = self.proof_records().ok().flatten();
            let active = records.as_ref().and_then(|r| r.active_condition.clone().and_then(|a| Self::proof_condition_record(r, &a).ok()));
            let mut draft = active.unwrap_or_else(|| shipped_record("working_rgb").unwrap_or_else(unreachable_record));
            if StudioProofRecords::is_shipped(&draft.name) {
                draft.name = "Custom".into();
            }
            if let StudioProofProfile::OperatorMatrix { matrix } = draft.proof_profile {
                self.proof.draft_matrix = matrix;
            } else {
                self.proof.draft_matrix = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
            }
            self.proof.draft = Some(draft);
        }
        self.proof.page = Some(page);
    }

    /// The open proof window page.
    pub fn proof_page(&self) -> Option<ProofPage> {
        self.proof.page
    }

    /// Closes the proof window.
    pub fn proof_close_page(&mut self) {
        self.proof.page = None;
        self.proof.draft = None;
    }

    /// Edits the custom condition draft of the open Custom Proof Condition page (model surface of the
    /// page's fields; Apply is [`Viewer::proof_apply_draft`]).
    pub fn proof_draft_mut(&mut self) -> Option<&mut StudioProofConditionRecord> {
        self.proof.draft.as_mut()
    }

    /// Applies the custom condition draft (the page's Apply button).
    pub fn proof_apply_draft(&mut self) -> Result<(), String> {
        let draft = self.proof.draft.clone().ok_or("proof: the Custom Proof Condition page is not open")?;
        self.proof_command(ProofCommand::Custom(draft))
    }

    /// Text of the Save Proof Preset name field.
    pub fn proof_set_save_name(&mut self, name: &str) {
        self.proof.save_name = name.to_string();
    }

    /// Closes the document's proof state with the document.
    pub(super) fn proof_close_document(&mut self) {
        self.proof = ProofState::default();
    }

    // ----- window ------------------------------------------------------------------------------

    /// The proof window (an in-window egui window, never an OS window).
    pub(super) fn proof_ui(&mut self, ctx: &egui::Context) {
        let Some(page) = self.proof.page else { return };
        let records = self.proof_records().ok().flatten();
        let mut action: Option<ProofCommand> = None;
        let mut close = false;
        let mut apply = false;
        let mut drawn: Vec<String> = vec![widget::WINDOW.into()];
        let icc: Vec<(String, String, folio::ContentDigest)> =
            self.vector_entry().map(|e| e.records.icc_profiles.iter().map(|p| (p.profile_id.clone(), p.name.clone(), p.content_digest.clone())).collect()).unwrap_or_default();
        let mut save_name = self.proof.save_name.clone();
        let mut matrix = self.proof.draft_matrix;
        let mut draft = self.proof.draft.clone();
        egui::Window::new(page.title()).id(super::widget::id(widget::WINDOW)).collapsible(false).resizable(true).show(ctx, |ui| {
            match page {
                ProofPage::Setup => {
                    let active = records.as_ref().and_then(|r| r.active_condition.clone());
                    let mut names: Vec<String> = folio::SHIPPED_PROOF_PRESETS.iter().map(|s| s.to_string()).collect();
                    names.extend(records.iter().flat_map(|r| r.conditions.iter().map(|c| c.name.clone())));
                    for (i, name) in names.iter().enumerate() {
                        let w = widget::preset(i);
                        if ui.push_id(super::widget::id(&w), |ui| ui.selectable_label(active.as_deref() == Some(name.as_str()), name)).inner.clicked() {
                            action = Some(ProofCommand::Select(name.clone()));
                        }
                        drawn.push(w);
                    }
                    ui.separator();
                    let g = records.as_ref().map(|r| r.gamut_warning.clone()).unwrap_or_default();
                    let mut rgb = [g.colour[0] as f32, g.colour[1] as f32, g.colour[2] as f32];
                    let mut opacity = g.opacity;
                    ui.horizontal(|ui| {
                        ui.label("Gamut warning colour");
                        let c = ui.push_id(super::widget::id(widget::GAMUT_COLOUR), |ui| ui.color_edit_button_rgb(&mut rgb)).inner.changed();
                        let o = ui.push_id(super::widget::id(widget::GAMUT_OPACITY), |ui| ui.add(egui::Slider::new(&mut opacity, 0..=100).text("opacity %"))).inner.changed();
                        if c || o {
                            action = Some(ProofCommand::SetGamutStyle { colour: [f64::from(rgb[0]), f64::from(rgb[1]), f64::from(rgb[2])], opacity });
                        }
                    });
                    drawn.push(widget::GAMUT_COLOUR.into());
                    drawn.push(widget::GAMUT_OPACITY.into());
                }
                ProofPage::Custom => {
                    if let Some(d) = draft.as_mut() {
                        ui.push_id(super::widget::id(widget::CUSTOM_NAME), |ui| ui.text_edit_singleline(&mut d.name));
                        drawn.push(widget::CUSTOM_NAME.into());
                        let mut choices: Vec<(String, StudioProofProfile)> = vec![
                            ("Working CMYK".into(), StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingCmyk }),
                            ("Working RGB".into(), StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingRgb }),
                            ("Legacy uncompensated RGB".into(), StudioProofProfile::Shipped { preset: ShippedProofProfile::LegacyUncompensatedRgb }),
                        ];
                        choices.extend(icc.iter().map(|(pid, name, digest)| (format!("ICC: {name}"), StudioProofProfile::Icc { profile_id: pid.clone(), content_digest: digest.clone() })));
                        for v in [ProofColourVision::Protanopia, ProofColourVision::Deuteranopia, ProofColourVision::Tritanopia] {
                            choices.push((format!("Colour vision: {}", vision_code(v)), StudioProofProfile::ColourVision { vision: v }));
                        }
                        choices.push(("Operator matrix".into(), StudioProofProfile::OperatorMatrix { matrix }));
                        let current = choices.iter().find(|(_, p)| std::mem::discriminant(p) == std::mem::discriminant(&d.proof_profile) && (matches!(p, StudioProofProfile::OperatorMatrix { .. }) || *p == d.proof_profile)).map_or_else(String::new, |(l, _)| l.clone());
                        ui.push_id(super::widget::id(widget::CUSTOM_PROFILE), |ui| {
                            egui::ComboBox::from_label("proof profile").selected_text(current).show_ui(ui, |ui| {
                                for (label, p) in &choices {
                                    if ui.selectable_label(false, label).clicked() {
                                        d.proof_profile = p.clone();
                                    }
                                }
                            });
                        });
                        drawn.push(widget::CUSTOM_PROFILE.into());
                        if matches!(d.proof_profile, StudioProofProfile::OperatorMatrix { .. }) {
                            egui::Grid::new(super::widget::id("proof.custom.matrix")).show(ui, |ui| {
                                for (i, v) in matrix.iter_mut().enumerate() {
                                    ui.add(egui::DragValue::new(v).speed(0.01).range(-4.0..=4.0));
                                    if i.checked_rem(3) == Some(2) {
                                        ui.end_row();
                                    }
                                }
                            });
                            d.proof_profile = StudioProofProfile::OperatorMatrix { matrix };
                        }
                        for (w, label, value) in [(widget::CUSTOM_PROOF_INTENT, "proof intent", &mut d.proof_intent), (widget::CUSTOM_DISPLAY_INTENT, "display intent", &mut d.display_intent)] {
                            ui.push_id(super::widget::id(w), |ui| {
                                egui::ComboBox::from_label(label).selected_text(format!("{value:?}")).show_ui(ui, |ui| {
                                    for i in [StudioProofIntent::Perceptual, StudioProofIntent::RelativeColorimetric, StudioProofIntent::Saturation, StudioProofIntent::AbsoluteColorimetric] {
                                        ui.selectable_value(value, i, format!("{i:?}"));
                                    }
                                });
                            });
                            drawn.push(w.into());
                        }
                        for (w, label, value) in [
                            (widget::CUSTOM_BPC, "Black point compensation", &mut d.black_point_compensation),
                            (widget::CUSTOM_PAPER, "Simulate paper colour", &mut d.simulate_paper_color),
                            (widget::CUSTOM_INK, "Simulate black ink", &mut d.simulate_black_ink),
                        ] {
                            ui.push_id(super::widget::id(w), |ui| ui.checkbox(value, label));
                            drawn.push(w.into());
                        }
                        if ui.push_id(super::widget::id(widget::CUSTOM_APPLY), |ui| ui.button("Apply")).inner.clicked() {
                            apply = true;
                        }
                        drawn.push(widget::CUSTOM_APPLY.into());
                    }
                }
                ProofPage::ColourVision => {
                    for v in [ProofColourVision::Protanopia, ProofColourVision::Deuteranopia, ProofColourVision::Tritanopia] {
                        let w = widget::vision(vision_code(v));
                        if ui.push_id(super::widget::id(&w), |ui| ui.button(vision_code(v))).inner.clicked() {
                            action = Some(ProofCommand::SelectColourVision(v));
                        }
                        drawn.push(w);
                    }
                }
                ProofPage::SavePreset => {
                    ui.push_id(super::widget::id(widget::SAVE_NAME), |ui| ui.text_edit_singleline(&mut save_name));
                    drawn.push(widget::SAVE_NAME.into());
                    if ui.push_id(super::widget::id(widget::SAVE_BUTTON), |ui| ui.button("Save")).inner.clicked() {
                        action = Some(ProofCommand::SavePreset(save_name.clone()));
                    }
                    drawn.push(widget::SAVE_BUTTON.into());
                }
            }
            if ui.push_id(super::widget::id(widget::CLOSE), |ui| ui.button("Close")).inner.clicked() {
                close = true;
            }
            drawn.push(widget::CLOSE.into());
        });
        self.proof.save_name = save_name;
        self.proof.draft_matrix = matrix;
        self.proof.draft = draft;
        self.drawn.extend(drawn);
        if apply {
            let _ = self.proof_apply_draft();
        }
        if let Some(a) = action {
            let _ = self.proof_command(a);
        }
        if close {
            self.proof_close_page();
        }
    }

    // ----- inspection --------------------------------------------------------------------------

    /// `state_json()["proof"]` (STU-COL-240 / STU-COL-244 / STU-COL-164: the proof state and the gamut
    /// warning as numbers a headless model reads).
    pub(super) fn proof_json(&self) -> Value {
        let key = self.proof_key();
        let fresh = self.proof.cache.as_ref().filter(|(k, _)| *k == key).map(|(_, v)| v);
        let records = self.proof_records();
        let (records, error) = match records {
            Ok(r) => (r, Value::Null),
            Err(e) => (None, json!({"code": snake(e.code), "target": e.target})),
        };
        let computed;
        let view: Result<Option<&ProofView>, String> = match fresh {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) if e.is_empty() => Ok(None),
            Some(Err(e)) => Err(e.clone()),
            None if records.is_some() => {
                computed = self.proof_view_now();
                match &computed {
                    Ok(v) => Ok(v.as_ref()),
                    Err(e) => Err(e.clone()),
                }
            }
            None => Ok(None),
        };
        let active = records.as_ref().and_then(|r| r.active_condition.clone());
        let condition = match (&records, &active) {
            (Some(r), Some(a)) => Self::proof_condition_record(r, a).ok().and_then(|c| serde_json::to_value(c).ok()),
            _ => None,
        };
        let g = records.as_ref().map(|r| r.gamut_warning.clone()).unwrap_or_default();
        let (view, preview_error) = match view {
            Ok(v) => (v, Value::Null),
            Err(e) => (None, json!(e)),
        };
        let warning = view.and_then(|v| v.warning.as_ref());
        let region = |r: &prism::GamutRegion| json!({"x0": r.x0, "y0": r.y0, "x1": r.x1, "y1": r.y1, "pixels": r.pixels});
        let receipt = view.and_then(|v| v.receipt.as_ref()).map(|r| {
            json!({
                "condition": serde_json::from_str::<Value>(&r.condition.to_json()).unwrap_or(Value::Null),
                "enabled": r.enabled,
                "document_profile_id": r.document_profile_id.as_str(),
                "document_sha256": hex(&r.document_sha256),
                "proof_profile_id": r.proof_profile_id.as_ref().map(|i| i.as_str().to_string()),
                "proof_profile_sha256": r.proof_profile_sha256.as_ref().map(hex),
                "display_profile_id": r.display_profile_id.as_str(),
                "display_sha256": hex(&r.display_sha256),
                "unmanaged": r.unmanaged,
                "pixel_count": r.pixel_count,
                "width": r.width,
                "height": r.height,
                "engine": format!("{} {}", r.engine_name, r.engine_version),
                "output_sha256": hex(&r.output_sha256),
                "proof_clip_pixels": r.proof_clip.pixels_clipped,
            })
        });
        json!({
            "entry": self.run.document_entry(ENTRY).is_some(),
            "error": error,
            "enabled": records.as_ref().is_some_and(|r| r.proof_colours),
            "active_condition": active,
            "condition": condition,
            "conditions": records.as_ref().map(|r| r.conditions.iter().map(|c| c.name.clone()).collect::<Vec<_>>()).unwrap_or_default(),
            "presets": folio::SHIPPED_PROOF_PRESETS,
            "gamut_warning": {
                "enabled": g.enabled,
                "colour": g.colour,
                "opacity": g.opacity,
                "area_fraction": warning.map(|w| w.area_fraction),
                "out_of_gamut_pixels": warning.map(|w| w.out_of_gamut_pixels),
                "pixel_count": warning.map(|w| w.pixel_count),
                "region_count": warning.map(|w| w.region_count),
                "regions_truncated": warning.map(|w| w.regions_truncated),
                "regions": warning.map(|w| w.regions.iter().map(region).collect::<Vec<_>>()),
                "bounds": warning.and_then(|w| w.bounds.as_ref().map(region)),
                "method": warning.map(|w| w.method.code()),
                "gamut_intent": warning.and_then(|w| w.gamut_intent.map(|i| i.code())),
                "threshold_delta_e76": warning.and_then(|w| w.threshold_delta_e76),
                "max_delta_e76": warning.and_then(|w| w.max_delta_e76),
            },
            "receipt": receipt,
            "view_on": view.is_some(),
            "read_back": view.is_some_and(|v| v.read_back),
            "canvas": if self.proof.shown { "proof" } else { "frame" },
            "preview_error": preview_error,
            "window": self.proof.page.map(ProofPage::key),
            "menus": {"view": VIEW_MENU_ID, "color": COLOR_MENU_ID},
            "not_proven": [
                "NOT_PROVEN: GPU shared-device display path with proof on (desktop window): the proof view uses the read-back frame",
                "NOT_PROVEN: equality with Photoshop View > Proof Colors / Gamut Warning output and defaults (no D5 proof case)"
            ],
        })
    }
}

/// Never used: `shipped_record("working_rgb")` always exists; kept total without a panic.
fn unreachable_record() -> StudioProofConditionRecord {
    StudioProofConditionRecord {
        name: "Custom".into(),
        proof_profile: StudioProofProfile::Shipped { preset: ShippedProofProfile::WorkingRgb },
        proof_intent: StudioProofIntent::RelativeColorimetric,
        display_intent: StudioProofIntent::RelativeColorimetric,
        black_point_compensation: true,
        simulate_paper_color: false,
        simulate_black_ink: false,
    }
}
