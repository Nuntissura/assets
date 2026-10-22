//! In-crate file dialog of the desktop shell (lane SMB-HARNESS-SHELL-1): an egui window that lists
//! a directory (folders first, then the files the active filter admits), lets the user type a file
//! name or a full path, and returns the chosen path. No new dependency and no native dialog: the
//! listing is `std::fs::read_dir`, so the dialog runs headlessly in tests exactly as in the window.
//!
//! Filters match by file extension only (case-insensitive); what a file IS stays decided by the
//! importers' magic-byte sniffing when it is opened. Hostile input (V4-01/02): a typed path is
//! refused when empty, longer than [`MAX_PATH_BYTES`] or holding a control character (NUL
//! included); a listing stops at [`MAX_DIALOG_ENTRIES`] entries and says so; unreadable entries
//! are counted, never fatal. Saving never replaces an existing file unless "replace" is ticked.
#![deny(clippy::arithmetic_side_effects)]

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Most entries one listing holds (the rest are reported as truncated).
pub const MAX_DIALOG_ENTRIES: usize = 4096;
/// Longest typed path accepted, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;

/// Stable widget names of the dialog (egui `Id` sources through `view::widget::id`).
pub mod widget {
    pub const WINDOW: &str = "dialog";
    pub const DIR: &str = "dialog.dir";
    pub const UP: &str = "dialog.up";
    pub const FILTER: &str = "dialog.filter";
    pub const LIST: &str = "dialog.list";
    pub const PATH: &str = "dialog.path";
    pub const REPLACE: &str = "dialog.replace";
    pub const ACCEPT: &str = "dialog.accept";
    pub const CANCEL: &str = "dialog.cancel";
    /// One listed entry: `dialog.entry.<name>`.
    pub fn entry(name: &str) -> String {
        format!("dialog.entry.{name}")
    }
}

/// Documents and images the viewer opens (import routes of harness-core plus native packages).
pub const DOCUMENT_EXTENSIONS: &[&str] = &[
    "handshake", "psd", "psb", "tif", "tiff", "dng", "cr2", "cr3", "nef", "nrw", "arw", "rw2", "raf", "orf", "pef", "srw", "sr2", "png", "jpg", "jpeg", "pdf",
];
/// Audio files the viewer opens with feature `media` (reel's audio import).
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "mp3", "flac", "ogg", "oga", "aac", "m4a", "aif", "aiff"];
/// Figma files of the prototype player.
pub const FIG_EXTENSIONS: &[&str] = &["fig"];
/// Media files of the source monitor (feature `media`: reel's in-process decoders; others refuse typed).
pub const VIDEO_EXTENSIONS: &[&str] = &["mov", "mxf", "mkv", "y4m", "mp4", "m4v"];

/// One listing filter: a name and the extensions it admits (empty = every file).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

impl Filter {
    pub const ALL_FILES: Filter = Filter { name: "All files", extensions: &[] };

    /// Whether a file at `path` passes the filter (extension, ASCII case-insensitive).
    pub fn admits(&self, path: &Path) -> bool {
        if self.extensions.is_empty() {
            return true;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        self.extensions.iter().any(|x| x.eq_ignore_ascii_case(ext))
    }
}

/// What the dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// FILE > Open: a document, image or (feature `media`) audio file.
    Open,
    /// FILE > Save As: a native `.handshake` package.
    SaveAs,
    /// FILE > Export PNG / Export JPEG: the rendered composite.
    ExportPng,
    ExportJpeg,
    /// WORKSPACE > Prototype Player: a Figma `.fig`.
    OpenPrototype,
    /// WORKSPACE > Source Monitor: a media file (feature `media`).
    OpenSourceMedia,
}

impl Purpose {
    pub fn key(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::SaveAs => "save_as",
            Self::ExportPng => "export_png",
            Self::ExportJpeg => "export_jpeg",
            Self::OpenPrototype => "open_prototype",
            Self::OpenSourceMedia => "open_source_media",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::SaveAs => "Save As",
            Self::ExportPng => "Export PNG",
            Self::ExportJpeg => "Export JPEG",
            Self::OpenPrototype => "Open Prototype",
            Self::OpenSourceMedia => "Open Source Media",
        }
    }
    /// Whether the dialog names a file to write (else an existing file to read).
    pub fn saves(self) -> bool {
        matches!(self, Self::SaveAs | Self::ExportPng | Self::ExportJpeg)
    }
    /// Extension added to a typed save name that has none.
    pub fn default_extension(self) -> Option<&'static str> {
        match self {
            Self::SaveAs => Some("handshake"),
            Self::ExportPng => Some("png"),
            Self::ExportJpeg => Some("jpg"),
            _ => None,
        }
    }
    /// Filters offered, the first one active.
    pub fn filters(self) -> Vec<Filter> {
        let mut out = match self {
            Self::Open => {
                #[allow(unused_mut)]
                let mut v = vec![Filter { name: "Documents and images", extensions: DOCUMENT_EXTENSIONS }];
                #[cfg(feature = "media")]
                v.push(Filter { name: "Audio", extensions: AUDIO_EXTENSIONS });
                v
            }
            Self::SaveAs => vec![Filter { name: "Native package (.handshake)", extensions: &["handshake"] }],
            Self::ExportPng => vec![Filter { name: "PNG", extensions: &["png"] }],
            Self::ExportJpeg => vec![Filter { name: "JPEG", extensions: &["jpg", "jpeg"] }],
            Self::OpenPrototype => vec![Filter { name: "Figma (.fig)", extensions: FIG_EXTENSIONS }],
            Self::OpenSourceMedia => vec![Filter { name: "Media", extensions: VIDEO_EXTENSIONS }],
        };
        out.push(Filter::ALL_FILES);
        out
    }
}

/// One listed entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Display name (lossy for names that are not UTF-8).
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub bytes: u64,
}

/// A directory listing under a filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// More than `max` entries passed the filter; only the first `max` are listed.
    pub truncated: bool,
    /// Entries whose metadata could not be read (skipped).
    pub unreadable: usize,
}

/// Lists `dir`: folders always, files the filter admits; folders first, then by case-insensitive
/// name. At most `max` entries. Refused `dir_unreadable` when the directory cannot be read.
pub fn list_dir(dir: &Path, filter: &Filter, max: usize) -> Result<Listing, &'static str> {
    let read = std::fs::read_dir(dir).map_err(|_| "dir_unreadable")?;
    let mut listing = Listing::default();
    for item in read {
        let Ok(item) = item else {
            listing.unreadable = listing.unreadable.saturating_add(1);
            continue;
        };
        let path = item.path();
        // Follows links, so a linked folder lists as a folder.
        let Ok(meta) = std::fs::metadata(&path) else {
            listing.unreadable = listing.unreadable.saturating_add(1);
            continue;
        };
        let is_dir = meta.is_dir();
        if !is_dir && !filter.admits(&path) {
            continue;
        }
        if listing.entries.len() >= max {
            listing.truncated = true;
            break;
        }
        listing.entries.push(Entry { name: item.file_name().to_string_lossy().into_owned(), path, is_dir, bytes: if is_dir { 0 } else { meta.len() } });
    }
    listing.entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
    Ok(listing)
}

/// Validates typed path text: refused `path_empty`, `path_too_long` (over [`MAX_PATH_BYTES`]) or
/// `path_control_character` (any control character, NUL included). Surrounding blanks are trimmed.
pub fn parse_typed(text: &str) -> Result<PathBuf, &'static str> {
    let t = text.trim();
    if t.is_empty() {
        return Err("path_empty");
    }
    if t.len() > MAX_PATH_BYTES {
        return Err("path_too_long");
    }
    if t.chars().any(char::is_control) {
        return Err("path_control_character");
    }
    Ok(PathBuf::from(t))
}

/// How the dialog ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accept(PathBuf),
    Cancel,
}

/// Dialog state (window-free; [`FileDialog::ui`] draws it into any `egui::Context`).
pub struct FileDialog {
    pub purpose: Purpose,
    dir: PathBuf,
    filter: usize,
    /// The path field: a file name (relative to the listed folder) or a full path.
    pub typed: String,
    /// Saving may replace an existing file.
    pub replace: bool,
    listing: Listing,
    /// Last refusal (stable code plus detail), shown in the dialog.
    pub error: Option<String>,
}

impl FileDialog {
    /// A dialog for `purpose` listing `start_dir` (made absolute), the path field holding
    /// `suggested` (a file name for save dialogs).
    pub fn new(purpose: Purpose, start_dir: &Path, suggested: Option<&str>) -> Self {
        let dir = std::path::absolute(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
        let mut d = Self { purpose, dir, filter: 0, typed: suggested.unwrap_or_default().to_owned(), replace: false, listing: Listing::default(), error: None };
        d.refresh();
        d
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn listing(&self) -> &Listing {
        &self.listing
    }
    pub fn filters(&self) -> Vec<Filter> {
        self.purpose.filters()
    }
    /// The active filter.
    pub fn filter(&self) -> Filter {
        self.filters().get(self.filter).copied().unwrap_or(Filter::ALL_FILES)
    }
    /// Selects filter `index` of [`FileDialog::filters`] (out of range: refused, nothing changes).
    pub fn set_filter(&mut self, index: usize) -> Result<(), &'static str> {
        if index >= self.filters().len() {
            return Err("filter_out_of_range");
        }
        self.filter = index;
        self.refresh();
        Ok(())
    }

    /// Lists the folder again (a failure leaves an empty listing and the error).
    pub fn refresh(&mut self) {
        match list_dir(&self.dir, &self.filter(), MAX_DIALOG_ENTRIES) {
            Ok(l) => self.listing = l,
            Err(code) => {
                self.listing = Listing::default();
                self.error = Some(format!("{code}: {}", self.dir.display()));
            }
        }
    }

    /// Shows folder `dir`.
    pub fn navigate(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.error = None;
        self.refresh();
    }

    /// Goes to the parent folder; `false` at a root.
    pub fn up(&mut self) -> bool {
        match self.dir.parent().map(Path::to_path_buf) {
            Some(parent) => {
                self.navigate(parent);
                true
            }
            None => false,
        }
    }

    /// Clicks listed entry `index`: a folder opens, a file's name goes into the path field.
    pub fn choose(&mut self, index: usize) -> Result<(), &'static str> {
        let entry = self.listing.entries.get(index).cloned().ok_or("entry_out_of_range")?;
        if entry.is_dir {
            self.navigate(entry.path);
        } else {
            self.typed = entry.name;
            self.error = None;
        }
        Ok(())
    }

    /// Resolves the path field: a folder is listed (`Ok(None)`); a file is returned when the purpose
    /// allows it. Refusals (also kept in `error`): the [`parse_typed`] codes, `file_not_found`,
    /// `save_dir_missing`, `save_target_is_directory`, `file_exists` (tick replace).
    pub fn submit(&mut self) -> Result<Option<PathBuf>, &'static str> {
        let result = self.submit_inner();
        match &result {
            Err(code) => self.error = Some((*code).to_owned()),
            Ok(_) => self.error = None,
        }
        result
    }

    fn submit_inner(&mut self) -> Result<Option<PathBuf>, &'static str> {
        let typed = parse_typed(&self.typed)?;
        let mut target = if typed.is_absolute() { typed } else { self.dir.join(typed) };
        if target.is_dir() {
            self.navigate(target);
            self.typed.clear();
            return Ok(None);
        }
        if !self.purpose.saves() {
            return if target.is_file() { Ok(Some(target)) } else { Err("file_not_found") };
        }
        if target.extension().is_none()
            && let Some(ext) = self.purpose.default_extension()
        {
            target.set_extension(ext);
        }
        if target.is_dir() {
            return Err("save_target_is_directory");
        }
        if !target.parent().is_some_and(Path::is_dir) {
            return Err("save_dir_missing");
        }
        if target.exists() && !self.replace {
            return Err("file_exists");
        }
        Ok(Some(target))
    }

    /// Structured projection (agents and tests).
    pub fn state_json(&self) -> Value {
        json!({
            "purpose": self.purpose.key(),
            "dir": self.dir.to_string_lossy(),
            "filter": self.filter().name,
            "filters": self.filters().iter().map(|f| f.name).collect::<Vec<_>>(),
            "entries": self.listing.entries.iter().map(|e| json!({"name": e.name, "dir": e.is_dir, "bytes": e.bytes})).collect::<Vec<_>>(),
            "truncated": self.listing.truncated,
            "unreadable": self.listing.unreadable,
            "typed": self.typed,
            "replace": self.replace,
            "error": self.error,
        })
    }

    /// Draws the dialog window; returns how it ended this frame (if it did) and the widget names
    /// drawn. Enter in the path field submits, Escape cancels.
    pub fn ui(&mut self, ctx: &egui::Context) -> (Option<Outcome>, Vec<String>) {
        let id = super::widget::id;
        let mut drawn: Vec<String> = vec![widget::WINDOW.into()];
        let mut open = true;
        let (mut clicked, mut go_up, mut filter_pick, mut submit, mut cancel) = (None, false, None, false, false);
        let filters = self.filters();
        let active = self.filter;
        let saves = self.purpose.saves();
        egui::Window::new(self.purpose.title())
            .id(id(widget::WINDOW))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.push_id(id(widget::UP), |ui| ui.add_enabled(self.dir.parent().is_some(), egui::Button::new("Up"))).inner.on_hover_text("Parent folder").clicked() {
                        go_up = true;
                    }
                    ui.push_id(id(widget::DIR), |ui| ui.label(self.dir.to_string_lossy()));
                });
                drawn.extend([widget::UP, widget::DIR].map(String::from));
                ui.push_id(id(widget::FILTER), |ui| {
                    egui::ComboBox::from_label("filter").selected_text(filters.get(active).map_or("All files", |f| f.name)).show_ui(ui, |ui| {
                        for (i, f) in filters.iter().enumerate() {
                            if ui.selectable_label(i == active, f.name).clicked() {
                                filter_pick = Some(i);
                            }
                        }
                    });
                });
                drawn.push(widget::FILTER.into());
                egui::ScrollArea::vertical().id_salt(id(widget::LIST)).max_height(260.0).show(ui, |ui| {
                    for (i, e) in self.listing.entries.iter().enumerate() {
                        let label = if e.is_dir { format!("[folder]  {}", e.name) } else { format!("{}    {} bytes", e.name, e.bytes) };
                        let name = widget::entry(&e.name);
                        let r = ui.push_id(id(&name), |ui| ui.selectable_label(!e.is_dir && self.typed == e.name, label)).inner;
                        if r.clicked() {
                            clicked = Some(i);
                        }
                        if r.double_clicked() && !e.is_dir {
                            submit = true;
                        }
                        drawn.push(name);
                    }
                    if self.listing.truncated {
                        ui.label(format!("only the first {MAX_DIALOG_ENTRIES} entries are listed; type the name"));
                    }
                    if self.listing.unreadable > 0 {
                        ui.label(format!("{} entries could not be read", self.listing.unreadable));
                    }
                });
                drawn.push(widget::LIST.into());
                let r = ui.add(egui::TextEdit::singleline(&mut self.typed).id(id(widget::PATH)).hint_text("file name or full path").desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                drawn.push(widget::PATH.into());
                if saves {
                    ui.push_id(id(widget::REPLACE), |ui| ui.checkbox(&mut self.replace, "replace an existing file"));
                    drawn.push(widget::REPLACE.into());
                }
                if let Some(e) = &self.error {
                    ui.colored_label(egui::Color32::RED, e);
                }
                ui.horizontal(|ui| {
                    let verb = if saves { "Save" } else { "Open" };
                    if ui.push_id(id(widget::ACCEPT), |ui| ui.button(verb)).inner.clicked() {
                        submit = true;
                    }
                    if ui.push_id(id(widget::CANCEL), |ui| ui.button("Cancel")).inner.clicked() {
                        cancel = true;
                    }
                });
                drawn.extend([widget::ACCEPT, widget::CANCEL].map(String::from));
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if let Some(i) = filter_pick {
            let _ = self.set_filter(i);
        }
        if go_up {
            self.up();
        }
        if let Some(i) = clicked {
            let _ = self.choose(i);
        }
        if cancel {
            return (Some(Outcome::Cancel), drawn);
        }
        if submit && let Ok(Some(path)) = self.submit() {
            return (Some(Outcome::Accept(path)), drawn);
        }
        (None, drawn)
    }
}
