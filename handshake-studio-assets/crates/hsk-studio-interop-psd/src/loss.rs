//! Typed loss report: what the importer holds byte-exact but does not semantically import.
//! Codes are fixed strings; paths contain indices and sanitized four-character keys only,
//! never layer names or other source text.

use crate::blend::BlendMode;
use crate::document::{MergedStatus, PsdDocument};
use crate::header::ColorMode;
use crate::layers::{GroupRole, PsdLayer};
use crate::limits::Limits;
use crate::resources::{RESOURCE_ICC_PROFILE, RESOURCE_ICC_UNTAGGED, RESOURCE_VERSION_INFO};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossKind {
    /// Held and understood; no capability is lost.
    Preserved,
    /// Held byte-exact; meaning not imported.
    PreservedOpaque,
    /// Meaning changed by the mapping (for example a code page assumption).
    Transformed,
    /// Replaced by cached pixels.
    Rasterized,
    /// Capability absent; the caller must not claim parity for it.
    Unsupported,
}

impl LossKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preserved => "preserved",
            Self::PreservedOpaque => "preserved_opaque",
            Self::Transformed => "transformed",
            Self::Rasterized => "rasterized",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossEntry {
    /// For example `layer[2]/tagged/TySh`.
    pub path: String,
    pub kind: LossKind,
    pub code: &'static str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LossReport {
    pub entries: Vec<LossEntry>,
}

impl LossReport {
    fn push(&mut self, path: String, kind: LossKind, code: &'static str) {
        self.entries.push(LossEntry { path, kind, code });
    }

    pub fn has_code(&self, code: &str) -> bool {
        self.entries.iter().any(|e| e.code == code)
    }

    pub fn find(&self, path: &str) -> Option<&LossEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Hand-written JSON: `{"entries":[{"path":..,"kind":..,"code":..}]}`.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"entries\":[");
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"path\":\"");
            for c in e.path.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c => out.push(c),
                }
            }
            out.push_str("\",\"kind\":\"");
            out.push_str(e.kind.as_str());
            out.push_str("\",\"code\":\"");
            out.push_str(e.code);
            out.push_str("\"}");
        }
        out.push_str("]}");
        out
    }
}

/// Four-character key as printable ASCII; anything else becomes `_`.
pub fn key_text(key: [u8; 4]) -> String {
    key.iter().map(|&b| if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' { char::from(b) } else { '_' }).collect()
}

enum KeyClass {
    /// Parsed into layer fields; nothing is lost.
    Mapped,
    Opaque(&'static str),
}

fn classify(key: [u8; 4]) -> KeyClass {
    match &key {
        b"lsct" | b"lsdk" | b"luni" | b"lyid" | b"clbl" | b"infx" | b"knko" | b"tsly" => KeyClass::Mapped,
        b"TySh" | b"tySh" | b"Txt2" => KeyClass::Opaque("text_layer_not_editable"),
        b"SoLd" | b"SoLE" | b"PlLd" | b"plLd" | b"lnk2" | b"lnkD" | b"lnk3" | b"lnkE" => {
            KeyClass::Opaque("smart_object_not_editable")
        }
        b"lfx2" | b"lrFX" => KeyClass::Opaque("layer_effects_not_rendered"),
        b"vmsk" | b"vsms" | b"vscg" | b"vstk" | b"vogk" => KeyClass::Opaque("vector_data_not_applied"),
        b"SoCo" | b"GdFl" | b"PtFl" => KeyClass::Opaque("fill_layer_not_applied"),
        b"levl" | b"curv" | b"brit" | b"blnc" | b"blwh" | b"hue " | b"hue2" | b"selc" | b"mixr" | b"grdm"
        | b"phfl" | b"expA" | b"thrs" | b"nvrt" | b"post" | b"vibA" | b"clrL" => {
            KeyClass::Opaque("adjustment_layer_not_applied")
        }
        _ => KeyClass::Opaque("tagged_block_opaque"),
    }
}

fn layer_entries(i: usize, layer: &PsdLayer, report: &mut LossReport) {
    let at = |tail: &str| format!("layer[{i}]/{tail}");
    if layer.blend_mode().is_none() {
        // Never mapped to Normal: the key stays verbatim in the record.
        report.push(at("blend"), LossKind::Unsupported, "blend_mode_unknown");
    }
    if let Some(key) = layer.group_blend_key()
        && key != layer.blend_key
        && !matches!(layer.group_role(), GroupRole::None)
    {
        report.push(at("group_blend"), LossKind::PreservedOpaque, "group_blend_key_differs");
    }
    if let GroupRole::Unknown(_) = layer.group_role() {
        report.push(at("group"), LossKind::Unsupported, "section_divider_type_unknown");
    }
    if !layer.mask_raw.is_empty() {
        report.push(at("mask"), LossKind::PreservedOpaque, "layer_mask_not_applied");
    }
    if layer.blend_ranges().is_some_and(|r| !r.is_default()) {
        report.push(at("blend_ranges"), LossKind::PreservedOpaque, "blend_if_not_applied");
    }
    if layer.block(&crate::layers::KEY_UNICODE_NAME).is_none() && layer.name_pascal.iter().any(|&b| b >= 0x80) {
        report.push(at("name"), LossKind::Transformed, "layer_name_codepage_assumed");
    }
    if layer.channels.iter().any(|c| c.compression > 3) {
        report.push(at("channels"), LossKind::Unsupported, "channel_compression_unknown");
    }
    for block in &layer.blocks {
        if let KeyClass::Opaque(code) = classify(block.key) {
            report.push(at(&format!("tagged/{}", key_text(block.key))), LossKind::PreservedOpaque, code);
        }
    }
    if !layer.extra_trailing.is_empty() {
        report.push(at("extra_trailing"), LossKind::PreservedOpaque, "layer_extra_unparsed");
    }
}

impl PsdDocument {
    /// Everything this crate holds but does not semantically import, per mapping target of an
    /// 8-bit RGB or grayscale layer stack.
    pub fn loss_report(&self, limits: &Limits) -> LossReport {
        let mut report = LossReport::default();
        let h = &self.header;
        if !matches!(h.color_mode, ColorMode::Rgb | ColorMode::Grayscale) {
            report.push("document/color_mode".into(), LossKind::Unsupported, "color_mode_unmapped");
        }
        if h.depth != 8 {
            report.push("document/depth".into(), LossKind::Unsupported, "bit_depth_unmapped");
        }
        if !self.color_mode_data.is_empty() {
            report.push("document/color_mode_data".into(), LossKind::PreservedOpaque, "color_mode_data_opaque");
        }
        for block in &self.resources.blocks {
            let path = format!("resource[{}]", block.id);
            match block.id {
                RESOURCE_ICC_PROFILE => {
                    report.push(path, LossKind::PreservedOpaque, "icc_profile_not_applied");
                }
                RESOURCE_ICC_UNTAGGED | RESOURCE_VERSION_INFO => {}
                _ => report.push(path, LossKind::PreservedOpaque, "image_resource_opaque"),
            }
        }
        if !self.resources.trailing.is_empty() {
            report.push("resources/trailing".into(), LossKind::PreservedOpaque, "image_resources_tail_unparsed");
        }
        match self.merged_status(limits) {
            MergedStatus::Real => {}
            MergedStatus::Placeholder => {
                report.push("merged_image".into(), LossKind::Unsupported, "merged_image_placeholder_no_oracle");
            }
            MergedStatus::Unknown => {
                report.push("merged_image".into(), LossKind::Preserved, "merged_image_oracle_unknown");
            }
        }
        if self.image_data.is_none() {
            report.push("image_data".into(), LossKind::Unsupported, "image_data_missing");
        }
        for (i, layer) in self.layers().iter().enumerate() {
            layer_entries(i, layer, &mut report);
        }
        let section = &self.layer_section;
        if section.global_mask.as_ref().is_some_and(|m| !m.is_empty()) {
            report.push("layer_section/global_mask".into(), LossKind::PreservedOpaque, "global_mask_opaque");
        }
        for block in &section.blocks {
            report.push(
                format!("layer_section/tagged/{}", key_text(block.key)),
                LossKind::PreservedOpaque,
                "tagged_block_opaque",
            );
        }
        if !section.trailing.is_empty() {
            report.push("layer_section/trailing".into(), LossKind::PreservedOpaque, "layer_section_tail_unparsed");
        }
        report
    }

    /// Blend modes present in layers whose keys are not among the 28 known ones.
    pub fn unknown_blend_keys(&self) -> Vec<[u8; 4]> {
        self.layers().iter().filter(|l| BlendMode::from_key(l.blend_key).is_none()).map(|l| l.blend_key).collect()
    }
}
