/// The egui memory `Id` of the context-menu popup for `response_id`. Mirrors
/// [`egui::Popup::default_response_id`] (`response_id.with("popup")`) — the SAME key
/// [`egui::Popup::context_menu`] reads — so a keyboard opener writes the id the popup actually renders.
fn popup_id_for(response_id: egui::Id) -> egui::Id {
    response_id.with("popup")
}

/// Open the context menu for `response_id` WITHOUT a pointer event (the keyboard path: Shift+F10 / the
/// Menu key, or an out-of-band / swarm-agent caller), anchored at `at` (screen-space position — pass the
/// right-clickable widget's rect corner/center so the keyboard-opened menu appears at the widget).
///
/// ## Why a position is REQUIRED (the bug this fixes)
///
/// [`crate::ContextMenu::show_on`] builds the popup with [`egui::Popup::context_menu`], which uses the
/// `PointerFixed` anchor. A `PointerFixed` popup reads its anchor rect from the position STORED when the
/// popup was opened ([`egui::Popup::position_of_id`]); the right-click path stores the pointer position
/// (`open_popup_at(id, hover_pos)`). Plain [`egui::Popup::open_id`] opens the popup in memory with NO
/// stored position, so a `PointerFixed` popup opened that way has `anchor.rect == None` and egui SILENTLY
/// renders nothing — the keyboard menu never appears. This was the untested keyboard-open defect. So this
/// helper stores an explicit anchor position (the SAME mechanism egui's own `Popup::show` uses for
/// `PointerFixed`), guaranteeing the keyboard-opened popup has a valid anchor and actually renders.
///
/// It uses the SAME popup id the pointer path uses, so the keyboard and right-click opens drive one
/// popup (single-open invariant).
pub fn request_open(ctx: &egui::Context, response_id: egui::Id, at: egui::Pos2) {
    let popup_id = popup_id_for(response_id);
    // `open_popup_at` is the position-storing open egui itself calls for a PointerFixed popup (see
    // `egui::containers::popup::Popup::show`); plain `open_popup`/`open_id` stores no position and a
    // PointerFixed popup then cannot anchor. The deprecation points to `PopupAnchor::Position`, but
    // `show_on` (via `context_menu`) is `PointerFixed`, so the position-storing memory open is the
    // matching primitive for this id.
    #[allow(deprecated)]
    ctx.memory_mut(|mem| mem.open_popup_at(popup_id, Some(at)));
}

/// Dismiss the context menu for `response_id` (the explicit programmatic close path).
pub fn dismiss(ctx: &egui::Context, response_id: egui::Id) {
    egui::Popup::close_id(ctx, popup_id_for(response_id));
}

/// True when the context menu for `response_id` is currently open.
pub fn is_open(ctx: &egui::Context, response_id: egui::Id) -> bool {
    egui::Popup::is_id_open(ctx, popup_id_for(response_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Memory open/dismiss round-trip: `request_open` marks the popup open, `dismiss` closes it, using
    /// the same popup-id derivation egui's `Popup::context_menu` reads. Proven against a headless
    /// `egui::Context` driven through a real frame (memory is only observable mid-pass).
    #[test]
    fn request_open_then_dismiss_round_trips() {
        let ctx = egui::Context::default();
        let response_id = egui::Id::new("ctx-menu-test-surface");

        // Frame 1: request open at an anchor position, then assert it reads as open AND stores the
        // anchor position (the keyboard-open path needs the stored position to anchor a PointerFixed
        // popup — opening with no position silently fails to render).
        let mut seen_open = false;
        let mut seen_pos = None;
        let anchor = egui::pos2(120.0, 48.0);
        let _ = ctx.run(Default::default(), |ctx| {
            request_open(ctx, response_id, anchor);
            seen_open = is_open(ctx, response_id);
            seen_pos = egui::Popup::position_of_id(ctx, popup_id_for(response_id));
        });
        assert!(seen_open, "request_open marked the context-menu popup open");
        assert_eq!(
            seen_pos,
            Some(anchor),
            "request_open stored the anchor position so a PointerFixed popup can render"
        );

        // Frame 2: dismiss, then assert it reads as closed within the same pass.
        let mut seen_open_after_dismiss = true;
        let _ = ctx.run(Default::default(), |ctx| {
            dismiss(ctx, response_id);
            seen_open_after_dismiss = is_open(ctx, response_id);
        });
        assert!(
            !seen_open_after_dismiss,
            "dismiss closed the context-menu popup (open == false)"
        );
    }

    /// The popup id derives from the response id exactly as egui's `Popup::default_response_id` does
    /// (`response_id.with(\"popup\")`), so the keyboard opener and the `show_on` popup share one id.
    #[test]
    fn popup_id_matches_egui_derivation() {
        let response_id = egui::Id::new("some-surface");
        assert_eq!(popup_id_for(response_id), response_id.with("popup"));
    }
}
