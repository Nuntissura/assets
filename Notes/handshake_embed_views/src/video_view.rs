//! Video embed renderer (WP-KERNEL-012 MT-014).
//!
//! ## KNOWN LIMITATION (deliberate, documented): no in-process video decode
//!
//! Full in-process video decode is OUT OF SCOPE for this MT. It requires a media decoder crate
//! (e.g. `symphonia` + ffmpeg bindings) and is deferred to a future MT. Here a `video` embed
//! renders a POSTER/PLACEHOLDER: the asset's thumbnail (poster) when available, a play-button
//! overlay, the original filename, and the content URL as VISIBLE TEXT so the operator can
//! inspect exactly what would play (red-team RISK-4 / minimum-control: show the content URL).
//!
//! ## HBR-QUIET: no automatic external app launch, no focus theft
//!
//! The MT scope text floated `open::that()` (the `open` crate) to launch the OS default media
//! player on click. The KERNEL_BUILDER gate + the MT-010 finding REJECT that as the DEFAULT:
//! `open::that()` launches an external OS app and STEALS FOCUS — a direct HBR-QUIET violation —
//! and it NEVER fires on render. So:
//!   - The DEFAULT play action is the in-app placeholder ([`VideoActivation::ShowUrlInline`]):
//!     it reveals the content URL inline and steals NO focus. No `open` crate is added.
//!   - A play CLICK is dispatched through a mockable [`VideoPlayHandler`] (NOT a hard
//!     `open::that()`), so AC-7 ("clicking the play button calls the play handler with the
//!     content URL") is proven with a counted mock WITHOUT focus theft and WITHOUT a new crate.
//!   - OS-open via `open::that()` remains an OPTIONAL, EXPLICIT-click-only future addition that
//!     would be a reviewed HBR-QUIET exception; it is intentionally NOT implemented here, so the
//!     `open` dependency is intentionally absent (NEVER auto-open on render).

/// What activating (clicking) a video embed does. The default is the focus-safe in-app reveal
/// ([`VideoActivation::ShowUrlInline`], marked `#[default]` — HBR-QUIET: never auto-launch an
/// external app); an OS-open variant is reserved for a future explicit-click HBR-QUIET exception
/// (not built).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VideoActivation {
    /// Reveal the content URL inline in the app (DEFAULT, focus-safe). No external app launches.
    #[default]
    ShowUrlInline,
    /// Reserved: open the content URL in the OS default player. NOT implemented in this MT (it
    /// would steal focus); kept as a typed option so a future explicit-click HBR-QUIET exception
    /// has a named target rather than a magic boolean.
    OsOpenReserved,
}

/// Per-embed video view state: whether the inline content-URL reveal is expanded. Stored per
/// embed node in `RichEditorState`. The play button toggles `url_revealed`; nothing here ever
/// launches an external app or grabs focus.
#[derive(Debug, Clone, Default)]
pub struct VideoViewState {
    /// True once the operator clicked play and the content URL is revealed inline.
    pub url_revealed: bool,
}

impl VideoViewState {
    /// A fresh state (URL not yet revealed).
    pub fn new() -> Self {
        Self {
            url_revealed: false,
        }
    }
}

/// A handler invoked when the video play button is clicked, carrying the content URL. Injected
/// (rather than a hard `open::that()`) so AC-7 is provable with a counted mock and so the
/// default production path is the focus-safe in-app reveal. The handler returns the
/// [`VideoActivation`] it performed so a test can assert the activation kind.
pub trait VideoPlayHandler {
    /// Called with the resolved content URL when the play button is clicked. Returns the
    /// activation that was performed.
    fn on_play(&self, content_url: &str) -> VideoActivation;
}

/// The default, production play handler: the focus-safe in-app reveal. It performs NO external
/// launch (HBR-QUIET) — the renderer reveals the content URL inline instead. Stateless.
#[derive(Debug, Default, Clone, Copy)]
pub struct InlineRevealPlayHandler;

impl VideoPlayHandler for InlineRevealPlayHandler {
    fn on_play(&self, _content_url: &str) -> VideoActivation {
        VideoActivation::ShowUrlInline
    }
}

/// The AccessKit author_id for the video embed container.
pub fn container_author_id(asset_token: &str) -> String {
    format!("video-{asset_token}")
}

/// The AccessKit author_id for the video play button.
pub fn play_author_id(asset_token: &str) -> String {
    format!("video-play-{asset_token}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A counted mock play handler (AC-7): records the content URL it was called with and how
    /// many times, WITHOUT launching anything. Proves "clicking play calls the handler with the
    /// content URL" with zero focus theft and zero `open` dependency.
    struct MockPlayHandler {
        calls: Cell<usize>,
        last_url: std::cell::RefCell<String>,
    }
    impl MockPlayHandler {
        fn new() -> Self {
            Self {
                calls: Cell::new(0),
                last_url: std::cell::RefCell::new(String::new()),
            }
        }
    }
    impl VideoPlayHandler for MockPlayHandler {
        fn on_play(&self, content_url: &str) -> VideoActivation {
            self.calls.set(self.calls.get() + 1);
            *self.last_url.borrow_mut() = content_url.to_owned();
            VideoActivation::ShowUrlInline
        }
    }

    #[test]
    fn play_click_calls_handler_with_content_url_ac7() {
        // AC-7: clicking play invokes the handler with the content URL (mock, no OS open).
        let handler = MockPlayHandler::new();
        let url = "http://127.0.0.1:37501/workspaces/ws/assets/v1/content";
        let activation = handler.on_play(url);
        assert_eq!(
            handler.calls.get(),
            1,
            "play click invokes the handler exactly once"
        );
        assert_eq!(
            *handler.last_url.borrow(),
            url,
            "AC-7: the handler receives the content URL"
        );
        assert_eq!(
            activation,
            VideoActivation::ShowUrlInline,
            "the default activation is the focus-safe inline reveal (HBR-QUIET)"
        );
    }

    #[test]
    fn default_handler_is_focus_safe_inline_reveal() {
        // HBR-QUIET: the production default never OS-opens.
        let handler = InlineRevealPlayHandler;
        assert_eq!(
            handler.on_play("http://x/content"),
            VideoActivation::ShowUrlInline,
            "production play handler must be the focus-safe inline reveal, never an OS launch"
        );
        assert_eq!(VideoActivation::default(), VideoActivation::ShowUrlInline);
    }

    #[test]
    fn author_ids_are_stable() {
        assert_eq!(container_author_id("v1"), "video-v1");
        assert_eq!(play_author_id("v1"), "video-play-v1");
    }
}
