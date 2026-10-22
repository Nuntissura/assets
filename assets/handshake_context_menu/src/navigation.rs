use crate::{ContextMenuItem, MenuItemKind};

/// True iff `item` is a keyboard-navigable target: an enabled Action or Submenu header. Separators
/// and disabled items are skipped by ArrowUp/ArrowDown (they can never receive the highlight).
pub fn is_navigable(item: &ContextMenuItem) -> bool {
    item.enabled && !matches!(item.kind, MenuItemKind::Separator)
}

/// The first navigable index at or after `from` (used to anchor the highlight on open).
pub fn first_navigable(items: &[ContextMenuItem]) -> Option<usize> {
    items.iter().position(is_navigable)
}

/// Step the highlight from `current` by `+1` (ArrowDown) or `-1` (ArrowUp), skipping
/// non-navigable items (separators + disabled) and WRAPPING at both ends. Returns the new index, or
/// the same index if no navigable item exists. `current` is assumed to already be navigable.
pub fn step_highlight(items: &[ContextMenuItem], current: usize, forward: bool) -> usize {
    let len = items.len();
    if len == 0 {
        return current;
    }
    let mut idx = current;
    for _ in 0..len {
        idx = if forward {
            (idx + 1) % len
        } else {
            (idx + len - 1) % len
        };
        if is_navigable(&items[idx]) {
            return idx;
        }
    }
    current
}
