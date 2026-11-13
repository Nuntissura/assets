//! New Document, artboards and pages on the viewer (lane SMB-DOC-NEW-1; [STU-SHL-023], [STU-SHL-026],
//! [STU-VEC-028]): the viewer's methods over the `Run` commands, the frame the view, the tools and the
//! exports work in ([`Viewer::go_to_artboard`], [`Viewer::go_to_page`]; view state, not document state)
//! and the state projection `containers`. No draw code: a panel lane calls these.
//!
//! The render frames the first artboard of a document (`render.rs`), so a frame other than the first
//! is a view copy with that artboard first; the document does not change. Sites that take the first
//! artboard all go through [`Viewer::active_artboard`]: the canvas of the selection and paint tools, the
//! tool canvas, align to artboard, SVG, PDF and separations export.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::render::RenderFail;
use hsk_studio_folio::{self as folio, Snapshot, StudioArtboard, StudioDocument};
use hsk_studio_harness_core::document_new::{ArtboardOptions, ArtboardOptionsEdit, NewDocument};
use serde_json::{Value, json};
use std::path::Path;

impl Viewer {
    /// `document.new`: replaces the open document with a blank one and resets every document-bound
    /// surface. `name` (`file_name` on the drive surface; the first artboard's name is the request's),
    /// when given, is the default save path (`<name>.handshake`): the document stores
    /// no name of its own, its file is its name. A refusal changes nothing. Returns the document id.
    pub fn new_document(&mut self, spec: &NewDocument, name: Option<&str>) -> Result<String, String> {
        if let Some(name) = name
            && (name.is_empty() || Path::new(name).file_name().is_none_or(|f| f != name))
        {
            let e = "new_document: name_invalid".to_owned();
            self.say(e.clone());
            return Err(e);
        }
        if !self.run.new_document(spec) {
            let e = format!("new_document refused: {}", self.op_error());
            self.say(e.clone());
            return Err(e);
        }
        self.reset_document_surfaces();
        self.assets = self.run.render_assets().map_err(|e| format!("new_document: tiles {e}"))?;
        // Lane SMB-BOARD-1 ([STU-WB-001]): a board document lists its first artboard as its board.
        if spec.profile == hsk_studio_harness_core::document_new::DocumentProfile::Board {
            self.register_new_board()?;
        }
        self.dirty = true;
        if let Some(name) = name {
            self.save_input = format!("{name}.handshake");
        }
        let id = self.run.snapshot().map_or_else(String::new, |s| s.document().document_id.clone());
        self.say(format!("new {} document {id}", spec.profile.token()));
        Ok(id)
    }

    fn detail_text(&self, key: &str) -> String {
        self.last_op()["detail"][key].as_str().unwrap_or_default().to_owned()
    }

    /// `artboard.add`: returns the new artboard's id. `at` absent: to the right of the others.
    pub fn add_artboard(&mut self, name: &str, width: f64, height: f64, at: Option<[f64; 2]>) -> Result<String, String> {
        let ok = self.run.add_artboard(name, width, height, at);
        self.after_edit(ok, format!("add_artboard {name}"))?;
        Ok(self.detail_text("artboard_id"))
    }

    /// `artboard.rename`.
    pub fn rename_artboard(&mut self, artboard: &str, name: &str) -> Result<u64, String> {
        let ok = self.run.rename_artboard(artboard, name);
        self.after_edit(ok, format!("rename_artboard {artboard}"))
    }

    /// `artboard.resize`.
    pub fn resize_artboard(&mut self, artboard: &str, width: f64, height: f64) -> Result<u64, String> {
        let ok = self.run.resize_artboard(artboard, width, height);
        self.after_edit(ok, format!("resize_artboard {artboard}"))
    }

    /// `artboard.move`.
    pub fn move_artboard(&mut self, artboard: &str, x: f64, y: f64) -> Result<u64, String> {
        let ok = self.run.move_artboard(artboard, x, y);
        self.after_edit(ok, format!("move_artboard {artboard}"))
    }

    /// `artboard.delete`.
    pub fn delete_artboard(&mut self, artboard: &str) -> Result<u64, String> {
        // Lane SMB-DS-VARIABLES-AUTHOR-1 (review F6): the artboard's explicit variable modes go in its step.
        let ok = self.run.delete_artboard(artboard) && self.container_records_into_step("delete_artboard");
        self.after_edit(ok, format!("delete_artboard {artboard}"))
    }

    /// `artboard.set_options`: the options after the edit.
    pub fn set_artboard_options(&mut self, artboard: &str, edit: &ArtboardOptionsEdit) -> Result<Value, String> {
        let ok = self.run.set_artboard_options(artboard, edit);
        self.after_edit(ok, format!("set_artboard_options {artboard}"))?;
        Ok(self.last_op()["detail"]["options"].clone())
    }

    /// `page.add`: returns the new page's key.
    pub fn add_page(&mut self, after: Option<&str>, new_spread: bool, size: Option<(f64, f64)>, name: Option<&str>) -> Result<String, String> {
        let ok = self.run.add_page(after, new_spread, size, name);
        self.after_edit(ok, "add_page".to_owned())?;
        Ok(self.detail_text("page_id"))
    }

    /// `page.rename`: names the page's spread.
    pub fn rename_page(&mut self, page: &str, name: &str) -> Result<u64, String> {
        let ok = self.run.rename_page(page, name);
        self.after_edit(ok, format!("rename_page {page}"))
    }

    /// `page.resize`.
    pub fn resize_page(&mut self, page: &str, width: f64, height: f64) -> Result<u64, String> {
        let ok = self.run.resize_page(page, width, height);
        self.after_edit(ok, format!("resize_page {page}"))
    }

    /// `page.delete`. The page's content goes with it (lane SMB-LAYOUT-DELETE-CASCADE-1): the raster records and
    /// an open layout editor are re-read after the step.
    pub fn delete_page(&mut self, page: &str) -> Result<u64, String> {
        // Lane SMB-DS-VARIABLES-AUTHOR-1 (review F6): a deleted spread's explicit variable modes go in its step.
        let ok = self.run.delete_page(page) && self.container_records_into_step("delete_page");
        let revision = self.after_edit(ok, format!("delete_page {page}"))?;
        self.place_sync();
        if self.layout_editor().is_some() {
            self.layout_sync();
        }
        Ok(revision)
    }

    /// [STU-SHL-026] Go to Artboard: the view, the tools and the exports frame `artboard`. View state:
    /// no revision, no undo step, not saved. `unknown_artboard` when the document does not hold it.
    pub fn go_to_artboard(&mut self, artboard: &str) -> Result<(), String> {
        let held = self.run.snapshot().is_some_and(|s| s.document().artboards.iter().any(|a| a.artboard_id == artboard));
        if !held {
            let e = format!("go_to_artboard {artboard}: unknown_artboard");
            self.say(e.clone());
            return Err(e);
        }
        self.frame_container(artboard);
        Ok(())
    }

    /// [STU-SHL-026] Go to Page: the view frames the page's spread. `unknown_page`, and
    /// `document_has_artboards` for a document that frames artboards.
    pub fn go_to_page(&mut self, page: &str) -> Result<(), String> {
        let found = self.run.snapshot().map(Snapshot::document).and_then(|d| {
            let spread = d.page_spreads.iter().find(|s| s.pages.iter().any(|p| p.page_id == page))?;
            Some((spread.page_spread_id.clone(), d.artboards.is_empty()))
        });
        let spread = match found {
            Some((spread, true)) => spread,
            Some((_, false)) => {
                let e = format!("go_to_page {page}: document_has_artboards");
                self.say(e.clone());
                return Err(e);
            }
            None => {
                let e = format!("go_to_page {page}: unknown_page");
                self.say(e.clone());
                return Err(e);
            }
        };
        self.frame_container(&spread);
        Ok(())
    }

    fn frame_container(&mut self, id: &str) {
        self.edit.active_container = Some(id.to_owned());
        self.dirty = true;
        self.fit = true;
        self.say(format!("framing {id}"));
    }

    /// The artboard the view, the tools and the exports work in: the one chosen, else the first (a
    /// chosen one that an undo removed falls back).
    pub(super) fn active_artboard<'d>(&self, document: &'d StudioDocument) -> Option<&'d StudioArtboard> {
        let chosen = self.edit.active_container.as_deref();
        document.artboards.iter().find(|a| Some(a.artboard_id.as_str()) == chosen).or_else(|| document.artboards.first())
    }

    /// The view copy of the document whose first container is the chosen one, `None` when the chosen
    /// one is first already (or none is chosen). The document itself does not change.
    pub(super) fn framed_snapshot(&self) -> Result<Option<Snapshot>, RenderFail> {
        let (Some(chosen), Some(snap)) = (self.edit.active_container.as_deref(), self.run.snapshot()) else {
            return Ok(None);
        };
        let doc = snap.document();
        let mut framed = None;
        if let Some(at) = doc.artboards.iter().position(|a| a.artboard_id == chosen).filter(|at| *at > 0) {
            let mut copy = doc.clone();
            let board = copy.artboards.remove(at);
            copy.artboards.insert(0, board);
            framed = Some(copy);
        } else if doc.artboards.is_empty()
            && let Some(at) = doc.page_spreads.iter().position(|s| s.page_spread_id == chosen).filter(|at| *at > 0)
        {
            let mut copy = doc.clone();
            let spread = copy.page_spreads.remove(at);
            copy.page_spreads.insert(0, spread);
            framed = Some(copy);
        }
        let Some(copy) = framed else {
            return Ok(None);
        };
        folio::validate_document(copy, folio::Budget::default(), self.cancel, &crate::harness::TrustAll).map(Some).map_err(|d| RenderFail { code: "framed_view", detail: format!("{:?} at {}", d.code, d.target) })
    }

    /// `state.containers`: every artboard with its options, every spread with its pages, and the frame
    /// in use. Ids and values a model addresses by; `options_error` when the entry does not parse.
    pub(super) fn containers_json(&self) -> Value {
        let Some(snap) = self.run.snapshot() else {
            return Value::Null;
        };
        let doc = snap.document();
        let options = self.run.artboard_options();
        let options_of = |id: &str| options.as_ref().ok().and_then(|rows| rows.iter().find(|o: &&ArtboardOptions| o.artboard_id == id)).map(ArtboardOptions::to_json);
        let active = self.active_artboard(doc).map(|a| a.artboard_id.clone()).or_else(|| {
            let chosen = self.edit.active_container.as_deref();
            doc.page_spreads.iter().find(|s| Some(s.page_spread_id.as_str()) == chosen).or_else(|| doc.page_spreads.first()).map(|s| s.page_spread_id.clone())
        });
        json!({
            "geometry_unit": doc.geometry_unit, "colour_mode": doc.colour_mode, "bits_per_channel": doc.bits_per_channel,
            "artboards": doc.artboards.iter().map(|a| json!({
                "id": a.artboard_id, "name": a.name,
                "bounds": [a.bounds.x.value, a.bounds.y.value, a.bounds.width.value, a.bounds.height.value],
                "options": options_of(&a.artboard_id),
            })).collect::<Vec<_>>(),
            "page_spreads": doc.page_spreads.iter().map(|s| json!({
                "id": s.page_spread_id, "name": s.name,
                "bounds": [s.bounds.x.value, s.bounds.y.value, s.bounds.width.value, s.bounds.height.value],
                "pages": s.pages.iter().map(|p| json!({"id": p.page_id, "width": p.width.value, "height": p.height.value, "side": p.side})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "active": active,
            "options_error": options.as_ref().err(),
        })
    }
}
