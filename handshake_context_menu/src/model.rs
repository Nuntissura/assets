/// The kind of a single context-menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItemKind {
    /// A leaf that fires its [`ContextMenuItem::id`] back to the caller when confirmed.
    Action,
    /// A nested popup. V1 supports exactly ONE level of nesting (no cascading multi-level submenus);
    /// the contained items are themselves leaves/separators. A submenu header is never itself an
    /// "activated" action — confirming a child returns the child's id.
    Submenu(Vec<ContextMenuItem>),
    /// A non-interactive visual divider. Its `id`/`label` are ignored.
    Separator,
}

/// One context-menu entry: a stable id, a display label, an optional right-aligned shortcut hint, an
/// enabled flag (with a disclosed reason when disabled), and its [`MenuItemKind`].
///
/// `id` is a stable `&'static str` the caller maps to a real action. It MUST be unique within a single
/// menu and SHOULD be namespaced per surface (e.g. `tab.close`) so AccessKit author_ids never collide
/// across surfaces. A disabled item is still rendered + addressable (no fake-enable); it simply cannot
/// be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuItem {
    /// Stable author id the caller maps to an action; also the AccessKit address (`ctx-menu.{id}`).
    pub id: &'static str,
    /// Human/model-readable display label.
    pub label: &'static str,
    /// Optional right-aligned shortcut hint (e.g. `"Ctrl+W"`); rendered dimmed, purely informational.
    pub shortcut_hint: Option<&'static str>,
    /// When false the item renders greyed and cannot be confirmed.
    pub enabled: bool,
    /// Disclosed reason shown on disabled-hover (no silent dead controls). Ignored when `enabled`.
    pub disabled_reason: Option<&'static str>,
    /// Action / one-level submenu / separator.
    pub kind: MenuItemKind,
}

impl ContextMenuItem {
    /// An enabled action leaf.
    pub fn action(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            shortcut_hint: None,
            enabled: true,
            disabled_reason: None,
            kind: MenuItemKind::Action,
        }
    }

    /// A visual separator. Its id is a stable filler so the struct stays uniform; it is never returned.
    pub fn separator() -> Self {
        Self {
            id: "ctx-menu.separator",
            label: "",
            shortcut_hint: None,
            enabled: false,
            disabled_reason: None,
            kind: MenuItemKind::Separator,
        }
    }

    /// A one-level submenu whose header carries `id`/`label` and whose children are `items`.
    pub fn submenu(id: &'static str, label: &'static str, items: Vec<ContextMenuItem>) -> Self {
        Self {
            id,
            label,
            shortcut_hint: None,
            enabled: true,
            disabled_reason: None,
            kind: MenuItemKind::Submenu(items),
        }
    }

    /// Builder: attach a right-aligned shortcut hint.
    pub fn with_shortcut(mut self, hint: &'static str) -> Self {
        self.shortcut_hint = Some(hint);
        self
    }

    /// Builder: render the item disabled with a disclosed reason. The reason is shown on
    /// disabled-hover so a no-context model (or operator) learns WHY it is unavailable.
    pub fn disabled(mut self, reason: &'static str) -> Self {
        self.enabled = false;
        self.disabled_reason = Some(reason);
        self
    }

    /// The stable AccessKit author_id for this item (`ctx-menu.{id}`). Used for out-of-process
    /// addressing and the MT-025 interactive-naming gate.
    pub fn author_id(&self) -> String {
        format!("ctx-menu.{}", self.id)
    }
}

/// A declarative context-menu model: an ordered list of [`ContextMenuItem`]s plus a stable menu id
/// that namespaces the popup in egui memory (so two different surfaces' menus are independent).
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// Stable surface id (e.g. `"tab"`, `"file-tree"`) — namespaces the popup + AccessKit ids.
    surface_id: &'static str,
    items: Vec<ContextMenuItem>,
}

impl ContextMenu {
    /// Start a menu for a named surface. `surface_id` should be a stable kebab-case string unique per
    /// right-clickable surface kind.
    pub fn new(surface_id: &'static str) -> Self {
        Self {
            surface_id,
            items: Vec::new(),
        }
    }

    /// Append one item (builder style).
    pub fn item(mut self, item: ContextMenuItem) -> Self {
        self.items.push(item);
        self
    }

    /// Append a separator (builder style).
    pub fn separator(mut self) -> Self {
        self.items.push(ContextMenuItem::separator());
        self
    }

    /// Append many items at once.
    pub fn items(mut self, items: impl IntoIterator<Item = ContextMenuItem>) -> Self {
        self.items.extend(items);
        self
    }

    /// The surface id this menu was built for.
    pub fn surface_id(&self) -> &'static str {
        self.surface_id
    }

    /// Borrow the items (for tests / introspection).
    pub fn entries(&self) -> &[ContextMenuItem] {
        &self.items
    }

    /// Consume the builder and return just its item list. Lets a per-surface caller
    /// (`context_menu_surfaces`) describe a menu with the builder's ergonomics
    /// (`.item()` / `.separator()`) but hand back a plain `Vec<ContextMenuItem>` the wiring site
    /// re-wraps into a fresh [`ContextMenu`] at `show_on` time with the live `surface_id`.
    pub fn into_items(self) -> Vec<ContextMenuItem> {
        self.items
    }
}
