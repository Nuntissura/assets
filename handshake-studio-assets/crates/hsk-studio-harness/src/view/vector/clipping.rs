//! Clipping masks and vector masks in the viewer (lane SMB-ILLUSTRATOR-INTEROP-2; Folio
//! STU-VEC-053/153 records, drawn by composite). A clip is a Folio `StudioVectorClip` whose source
//! is a vector or group layer (its geometry clips, under its fill rule); a mask is a
//! `StudioVectorMask` (ALPHA, LUMINANCE or VECTOR, optionally inverted) whose source is any layer;
//! the clipped or masked layer carries the `StudioMaskAttachment`. Making either hides the source
//! (it is drawn only through what it clips or masks, as in Illustrator's Make Clipping Mask) and
//! stores the records: two undo steps (the visibility change, then the records). Releasing removes
//! the attachment (and records nothing else uses) and shows the source again.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use hsk_studio_folio::{Payload, StudioClipRule, StudioMaskAttachment, StudioMaskMode, StudioVectorClip, StudioVectorMask};

/// The first `{stem}-{n}` key no record of `taken` uses.
fn free_key(stem: &str, taken: &[&str]) -> String {
    let mut n = 1_u64;
    loop {
        let key = format!("{stem}-{n}");
        if !taken.contains(&key.as_str()) {
            return key;
        }
        n = n.saturating_add(1);
    }
}

impl Viewer {
    fn mask_pair(&self, target: &str, source: &str) -> Result<(), String> {
        let doc = self.vdoc()?;
        if target == source {
            return Err("mask: a layer cannot clip or mask itself".into());
        }
        for id in [target, source] {
            if !doc.layers.iter().any(|l| l.layer_id == id) {
                return Err(format!("mask: no layer {id}"));
            }
        }
        if super::masking::subtree(doc, source).iter().any(|l| l == target) || super::masking::subtree(doc, target).iter().any(|l| l == source) {
            return Err("mask: source and target may not contain each other".into());
        }
        Ok(())
    }

    fn attach(&mut self, op: &str, target: &str, source: &str, set: impl FnOnce(&mut super::VectorEntry, &mut StudioMaskAttachment)) -> Result<String, String> {
        self.mask_pair(target, source)?;
        let mut entry = self.vector_entry()?;
        let mut a = entry.records.attachments.iter().find(|a| a.layer_id == target).cloned().unwrap_or(StudioMaskAttachment { layer_id: target.to_string(), clip_id: None, mask_id: None });
        set(&mut entry, &mut a);
        entry.records.attachments.retain(|x| x.layer_id != target);
        entry.records.attachments.push(a);
        let doc = self.vdoc()?;
        entry.validate_records(doc, self.cancel)?;
        if doc.layers.iter().any(|l| l.layer_id == source && l.visible) {
            self.set_visible(source, false)?;
        }
        self.commit_entry(op, target, &entry)?;
        Ok(target.to_string())
    }

    /// Clips `target` by the geometry of `source` (a vector or group layer; `even_odd` = its fill
    /// rule), replacing a clip `target` had (module docs).
    pub fn make_clip(&mut self, target: &str, source: &str, even_odd: bool) -> Result<String, String> {
        let doc = self.vdoc()?;
        if !doc.layers.iter().any(|l| l.layer_id == source && matches!(l.payload, Payload::Vector { .. } | Payload::Group)) {
            return Err("make_clip: the source must be a vector or group layer".into());
        }
        let r = self.attach("make_clip", target, source, |entry, a| {
            let taken: Vec<&str> = entry.records.clips.iter().map(|c| c.clip_id.as_str()).collect();
            let clip_id = free_key("clip", &taken);
            let clip_rule = if even_odd { StudioClipRule::EvenOdd } else { StudioClipRule::NonZero };
            entry.records.clips.push(StudioVectorClip { clip_id: clip_id.clone(), source_layer_id: source.to_string(), clip_rule });
            a.clip_id = Some(clip_id);
        });
        self.report(r)
    }

    /// Masks `target` with the content of `source` (`mode`, inverted or not), replacing a mask
    /// `target` had (module docs).
    pub fn make_vector_mask(&mut self, target: &str, source: &str, mode: StudioMaskMode, invert: bool) -> Result<String, String> {
        let r = self.attach("make_vector_mask", target, source, |entry, a| {
            let taken: Vec<&str> = entry.records.masks.iter().map(|m| m.mask_id.as_str()).collect();
            let mask_id = free_key("mask", &taken);
            entry.records.masks.push(StudioVectorMask { mask_id: mask_id.clone(), source_layer_id: source.to_string(), mode, mask_clip: false, invert_mask: invert, region: None });
            a.mask_id = Some(mask_id);
        });
        self.report(r)
    }

    /// Removes the clip and mask of `target`; records nothing else uses go too and their sources
    /// are shown again.
    pub fn release_masks(&mut self, target: &str) -> Result<(), String> {
        let r = (|| {
            let mut entry = self.vector_entry()?;
            let Some(a) = entry.records.attachments.iter().find(|a| a.layer_id == target).cloned() else {
                return Err(format!("release_masks: {target} has no clip or mask"));
            };
            entry.records.attachments.retain(|x| x.layer_id != target);
            let used = |entry: &super::VectorEntry, clip: Option<&str>, mask: Option<&str>| entry.records.attachments.iter().any(|x| (clip.is_some() && x.clip_id.as_deref() == clip) || (mask.is_some() && x.mask_id.as_deref() == mask));
            let mut sources = Vec::new();
            if let Some(c) = a.clip_id.as_deref()
                && !used(&entry, Some(c), None)
            {
                sources.extend(entry.records.clips.iter().filter(|x| x.clip_id == c).map(|x| x.source_layer_id.clone()));
                entry.records.clips.retain(|x| x.clip_id != c);
            }
            if let Some(m) = a.mask_id.as_deref()
                && !used(&entry, None, Some(m))
            {
                sources.extend(entry.records.masks.iter().filter(|x| x.mask_id == m).map(|x| x.source_layer_id.clone()));
                entry.records.masks.retain(|x| x.mask_id != m);
                entry.records.mask_parameters.retain(|x| x.mask_id != m);
            }
            self.commit_entry("release_masks", target, &entry)?;
            for s in sources {
                self.set_visible(&s, true)?;
            }
            Ok(())
        })();
        self.report(r)
    }
}
