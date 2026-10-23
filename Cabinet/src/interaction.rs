//! Framework-independent interaction state for host-provided Cabinet GUIs.
use crate::*;
use std::collections::BTreeMap;

pub enum Edit {
    Create { parent: Option<EntryId> },
    Rename { id: EntryId },
}
pub struct ColorDraft {
    pub id: EntryId,
    pub revision: u64,
    pub color: [u8; 3],
    pub hex: String,
}
pub struct TagDraft {
    pub id: EntryId,
    pub revision: u64,
    pub tags: Vec<Tag>,
    pub hexes: BTreeMap<String, String>,
    pub new_id: String,
    pub new_label: String,
}
pub struct MoveTargets {
    pub context: Context,
    pub source: EntryId,
    pub targets: Vec<(Option<EntryId>, bool)>,
}

#[derive(Default)]
pub struct InteractionState {
    pub edit: Option<Edit>,
    pub text: String,
    pub delete: Option<Vec<EntryId>>,
    pub hover: Option<(EntryId, f64)>,
    pub last_error: Option<String>,
    pub context: Option<Context>,
    pub peak: peak::Peak,
    pub available_tags: Vec<Tag>,
    pub catalog_context: Option<Context>,
    pub color_draft: Option<ColorDraft>,
    pub tag_draft: Option<TagDraft>,
    pub moving: Option<EntryId>,
    pub move_targets: Option<MoveTargets>,
    pub click: Option<(EntryId, Context, u64)>,
    pub quick_path: String,
    pub host_search: bool,
    pub workspace_query: String,
    pub scheduler: Option<peak::PreviewScheduler>,
}

impl InteractionState {
    pub fn enable_scheduler(
        &mut self,
        settings: peak::SchedulerSettings,
    ) -> Result<(), peak::SchedulerError> {
        if let Some(scheduler) = self.scheduler.as_mut() {
            return scheduler.configure(settings);
        }
        self.scheduler = Some(peak::PreviewScheduler::new(settings)?);
        Ok(())
    }
    pub fn complete_scheduled(&mut self, token: u64, content: peak::Content) -> bool {
        self.scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.finish(token, content))
            .is_some_and(|completion| self.peak.complete(completion))
    }
    pub fn acknowledge_preview_cancelled(&mut self, token: u64) {
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.acknowledge_cancelled(token);
        }
    }
    pub fn set_tag_catalog(&mut self, context: Context, tags: Vec<Tag>) {
        self.catalog_context = Some(context);
        self.available_tags = tags;
    }
    pub fn begin_create(&mut self, parent: Option<EntryId>) {
        self.edit = Some(Edit::Create { parent });
        self.text.clear();
    }
    pub fn begin_rename(&mut self, cabinet: &Cabinet, id: EntryId) {
        if let Some(e) = cabinet.entry(&id) {
            self.text = e.label.clone();
            self.edit = Some(Edit::Rename { id });
        }
    }
    pub fn submit_edit(&mut self, cabinet: &mut Cabinet) -> Result<Option<Request>, Error> {
        let operation = match self.edit.as_ref() {
            Some(Edit::Create { parent }) => Operation::Create {
                parent: parent.clone(),
                name: self.text.clone(),
                kind: EntryKind::Folder,
            },
            Some(Edit::Rename { id }) => Operation::Rename {
                id: id.clone(),
                name: self.text.clone(),
            },
            None => return Ok(None),
        };
        let request = self.request(cabinet, operation)?;
        self.edit = None;
        self.text.clear();
        Ok(Some(request))
    }
    pub fn request(&mut self, cabinet: &mut Cabinet, op: Operation) -> Result<Request, Error> {
        let op = match op {
            Operation::Move { ids, placement } if !cabinet.snapshot().hierarchy_complete => {
                Operation::MovePreflight { ids, placement }
            }
            other => other,
        };
        match cabinet.request(op) {
            Ok(r) => {
                self.last_error = None;
                Ok(r)
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }
    pub fn expand(
        &mut self,
        cabinet: &mut Cabinet,
        id: &str,
        value: bool,
    ) -> Result<Option<Request>, Error> {
        if let Err(e) = cabinet.expand(id, value) {
            self.last_error = Some(e.to_string());
            return Err(e);
        }
        if value
            && cabinet.entry(id).is_some_and(|e| {
                matches!(
                    e.children,
                    ChildrenState::Unloaded | ChildrenState::Failed(_)
                )
            })
        {
            match cabinet.request_child_page(id) {
                Ok(request) => return Ok(Some(request)),
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return Err(error);
                }
            }
        }
        Ok(None)
    }
    /// Returns whether an edit or confirmation was cancelled by a canonical refresh.
    pub fn sync(&mut self, cabinet: &Cabinet) -> bool {
        if self.catalog_context.as_ref().is_some_and(|context| {
            context.scope != cabinet.snapshot().context.scope
                || context.generation != cabinet.snapshot().context.generation
        }) {
            self.available_tags.clear();
            self.catalog_context = None;
        }
        self.peak.settings = cabinet.settings().peak.clone();
        let mut draft_cancelled = false;
        if self.context.as_ref() != Some(&cabinet.snapshot().context) {
            self.click = None;
            self.color_draft = None;
            self.tag_draft = None;
            self.moving = None;
            self.move_targets = None;
            self.peak.dismiss();
            // Never re-fence an older inline draft against a refreshed canonical revision.
            if self.context.is_some() && self.edit.is_some() {
                draft_cancelled = true;
                self.edit = None;
                self.text.clear();
                self.last_error=Some("Entry changed while editing; draft cancelled. Rename again using the current entry.".into());
            }
            if self.context.is_some() && self.delete.take().is_some() {
                draft_cancelled = true;
                self.last_error = Some("Entries changed; delete confirmation cancelled. Review the current selection before deleting.".into());
            }
            if self.context.as_ref().is_some_and(|c| {
                c.scope != cabinet.snapshot().context.scope
                    || c.generation != cabinet.snapshot().context.generation
            }) {
                self.edit = None;
                self.text.clear();
                self.quick_path.clear();
                self.workspace_query.clear();
                self.host_search = false;
                if let Some(scheduler) = self.scheduler.as_mut() {
                    scheduler.cancel_all();
                    scheduler.clear_cache();
                }
                self.delete = None;
                self.hover = None;
                self.last_error = None;
            }
            self.context = Some(cabinet.snapshot().context.clone());
        }
        draft_cancelled
    }
    pub fn cancel(&mut self) {
        self.edit = None;
        self.delete = None;
        self.hover = None;
        self.click = None;
        self.color_draft = None;
        self.tag_draft = None;
        self.moving = None;
        self.move_targets = None;
        self.peak.dismiss();
    }
}

pub fn rgb_hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}
pub fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let value = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}
