//! egui rendering and popup adapter for the shared context-menu model.

pub use handshake_context_menu::{ContextMenuItem, MenuItemKind};

mod popup;
mod render;

pub use popup::{dismiss, is_open, request_open};

#[derive(Debug, Clone)]
pub struct ContextMenu {
    model: handshake_context_menu::ContextMenu,
}

impl ContextMenu {
    pub fn new(surface_id: &'static str) -> Self {
        Self { model: handshake_context_menu::ContextMenu::new(surface_id) }
    }

    pub fn item(mut self, item: ContextMenuItem) -> Self {
        self.model = self.model.item(item);
        self
    }

    pub fn separator(mut self) -> Self {
        self.model = self.model.separator();
        self
    }

    pub fn items(mut self, items: impl IntoIterator<Item = ContextMenuItem>) -> Self {
        self.model = self.model.items(items);
        self
    }

    pub fn surface_id(&self) -> &'static str {
        self.model.surface_id()
    }

    pub fn entries(&self) -> &[ContextMenuItem] {
        self.model.entries()
    }

    pub fn into_items(self) -> Vec<ContextMenuItem> {
        self.model.into_items()
    }
}
