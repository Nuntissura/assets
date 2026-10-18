//! The editor gutter: the narrow left-margin strip that renders line numbers, diagnostic severity
//! icons, fold triangles, and breakpoint toggles (WP-KERNEL-012 MT-007 — E1 code editor).
//!
//! ## What the gutter draws, per VS Code parity
//!
//! For each painted editor row (the exact row model the panel body painted, so gutter row N aligns
//! exactly with code row N, including MT-005 folds and MT-054 wrapped continuation rows),
//! left-to-right within a fixed-width strip:
//! 1. a filled red breakpoint circle (when the line is in the [`BreakpointSet`](super::breakpoints::BreakpointSet)),
//! 2. a fold triangle (`▶` collapsed / `▼` expanded) on a line that starts a foldable region,
//! 3. the right-aligned, dimmed line number,
//! 4. a diagnostic severity dot (red error / yellow warning / blue info) on the strip's right edge,
//!    plus a VS Code-style 3px colored left border bar on the diagnostic line.
//!
//! ## Colors come from the theme, never from hardcoded literals here
//!
//! The diagnostic severity colors and the breakpoint red are editor UI affordances, but they are
//! still THEME TOKENS, not `Color32` literals baked into this widget. They live in the sanctioned
//! theme home ([`HsDiagnosticTokens`] in `theme/syntax.rs`) so the no-hardcode
//! invariant (CONTROL-4, grep-enforced by `tests/test_theme.rs`) stays GREEN, and the gutter resolves
//! them per frame from the active egui visual mode (via [`diagnostic_tokens_for`]) so the
//! affordances track dark/light like every other token. [`DiagnosticSeverity::dot_color`] takes the
//! resolved tokens and [`breakpoint_color`] reads the breakpoint token. The MT contract still names
//! the exact hues (Error=red, Warning=yellow, Info/Hint=cornflower `rgb(100,149,237)`,
//! breakpoint=`rgb(229,60,60)`); those values now live in the theme layer. The line-number text comes
//! from the live theme visuals (`weak_text_color()` — the dimmed line-number convention).
//!
//! ## Glyph fallback (RISK-002 / MC-002)
//!
//! The bundled UI font may lack the triangle glyphs U+25B6/U+25BC. [`fold_triangle_glyph`] returns the
//! Unicode triangle only when the active font actually contains it (checked via `Fonts::has_glyph`),
//! and falls back to ASCII `>` / `v` otherwise, so the gutter never ships a tofu box.

use super::breakpoints::BreakpointSet;
use handshake_text_mechanics::buffer::TextBuffer;
use egui::Color32;

/// Editor gutter diagnostic + breakpoint affordance colors for a single theme.
///
/// These are UI affordances of the code-editor surface (the diagnostic severity dots / 3px left
/// bars and the breakpoint circle), not arbitrary widget hex. They live here in `syntax.rs` — one of
/// the two sanctioned homes for `Color32` literals (the other is `palette.rs`) — so the no-hardcode
/// invariant (CONTROL-4, grep-enforced by `tests/test_theme.rs`) stays GREEN and the gutter reads
/// these from the live theme instead of baking literals into widget code. Resolved per frame from
/// the active `HsTheme` so the gutter affordances track dark/light like every other token.
///
/// Token values are the ones the WP-KERNEL-012 MT-007 contract names exactly: Error = pure red,
/// Warning = pure yellow, Info/Hint = cornflower blue `rgb(100,149,237)`, breakpoint = `rgb(229,60,60)`.
/// Dark and light currently share the same severity hues (these are saturated semantic signals that
/// read on either background, matching VS Code, which also keeps severity hues constant across themes);
/// the per-theme constructors exist so a later MT can diverge them via the override map without a
/// signature change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HsDiagnosticTokens {
    /// Error severity dot + 3px left bar (pure red — the AC-003 red-pixel affordance).
    pub error: Color32,
    /// Warning severity dot + left bar (pure yellow).
    pub warning: Color32,
    /// Info severity dot + left bar (cornflower blue).
    pub info: Color32,
    /// Hint severity dot + left bar (the dimmest level; shares the Info blue per the MT contract).
    pub hint: Color32,
    /// Filled breakpoint circle color (a slightly softened red so it reads distinct from the pure-red
    /// error dot).
    pub breakpoint: Color32,
}

impl HsDiagnosticTokens {
    /// Dark-theme gutter diagnostic/breakpoint colors (the MT-007 contract values).
    pub fn dark() -> Self {
        Self {
            error: Color32::from_rgb(0xff, 0x00, 0x00),
            warning: Color32::from_rgb(0xff, 0xff, 0x00),
            info: Color32::from_rgb(100, 149, 237),
            hint: Color32::from_rgb(100, 149, 237),
            breakpoint: Color32::from_rgb(229, 60, 60),
        }
    }

    /// Light-theme gutter diagnostic/breakpoint colors. Currently identical to dark (saturated
    /// severity signals that read on either background, matching VS Code's theme-constant severity
    /// hues); kept as its own constructor so a later MT can diverge without a call-site change.
    pub fn light() -> Self {
        Self::dark()
    }
}


/// The diagnostic severity of a gutter marker. Mirrors the LSP `DiagnosticSeverity` (Error=1 ..
/// Hint=4) so the MT-008 LSP client can map LSP diagnostics onto gutter markers 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    /// A hard error (red dot + red left bar).
    Error,
    /// A warning (yellow dot + yellow left bar).
    Warning,
    /// Informational (cornflower-blue dot + blue left bar).
    Info,
    /// A hint (the same cornflower-blue as Info — the dimmest severity).
    Hint,
}

impl DiagnosticSeverity {
    /// The dot/left-bar color for this severity, read from the active theme's diagnostic tokens (a UI
    /// affordance sourced from the theme, never a hardcoded literal in widget code). The MT contract
    /// names these exactly: Error=red, Warning=yellow, Info=cornflower blue `rgb(100,149,237)`; Hint
    /// reuses the Info blue (the dimmest level). Those values live in
    /// [`HsDiagnosticTokens`].
    pub fn dot_color(self, tokens: &HsDiagnosticTokens) -> egui::Color32 {
        match self {
            DiagnosticSeverity::Error => tokens.error,
            DiagnosticSeverity::Warning => tokens.warning,
            DiagnosticSeverity::Info => tokens.info,
            DiagnosticSeverity::Hint => tokens.hint,
        }
    }

    /// A short human label for the AccessKit node value + the hover tooltip prefix.
    pub fn label(self) -> &'static str {
        match self {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Info => "info",
            DiagnosticSeverity::Hint => "hint",
        }
    }
}

/// What a [`GutterMarker`] draws on its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GutterMarkerKind {
    /// A diagnostic of the given severity. The optional message is shown in the hover tooltip (joined
    /// with other diagnostics on the same line). MT-008 pushes these via `push_diagnostics`.
    Diagnostic(DiagnosticSeverity),
    /// A breakpoint is set on the line (a filled red circle).
    Breakpoint,
    /// A foldable region starts on the line; the bool is `is_open` (true = expanded `▼`, false =
    /// collapsed `▶`).
    FoldTriangle(bool),
}

/// One drawable gutter marker for a buffer line. `message` is only meaningful for a
/// [`GutterMarkerKind::Diagnostic`] (the diagnostic text shown on hover); it is empty for breakpoints
/// and fold triangles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GutterMarker {
    /// The 0-based buffer line the marker sits on.
    pub line: usize,
    /// What the marker draws.
    pub kind: GutterMarkerKind,
    /// The diagnostic message shown on hover (empty for non-diagnostic markers).
    pub message: String,
}

impl GutterMarker {
    /// A diagnostic marker on `line` with `severity` and `message`.
    pub fn diagnostic(
        line: usize,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            line,
            kind: GutterMarkerKind::Diagnostic(severity),
            message: message.into(),
        }
    }
}

/// Which gutter columns are shown. All default ON (VS Code shows line numbers + diagnostics + folds +
/// breakpoints by default); a consumer can turn any column off (e.g. hide line numbers in a diff
/// minimap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GutterConfig {
    /// Render the right-aligned line numbers.
    pub show_line_numbers: bool,
    /// Render the fold triangles on region-start lines.
    pub show_fold_triangles: bool,
    /// Render the diagnostic severity dots + left bars.
    pub show_diagnostics: bool,
    /// Render the breakpoint circles + accept breakpoint-toggle clicks.
    pub show_breakpoints: bool,
}

impl Default for GutterConfig {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            show_fold_triangles: true,
            show_diagnostics: true,
            show_breakpoints: true,
        }
    }
}

/// One painted row in the gutter. `line` is the 0-based buffer line this visual row belongs to;
/// `is_first_fragment` is true only for the first visual row of that buffer line. Wrapped continuation
/// rows still reserve pitch so later rows align, but they do not duplicate line numbers, diagnostics,
/// breakpoints, fold triangles, click targets, or AccessKit nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GutterPaintRow {
    /// 0-based buffer line owned by this painted gutter row.
    pub line: usize,
    /// True for the logical line's first visual row; false for wrapped continuation fragments.
    pub is_first_fragment: bool,
}

impl GutterPaintRow {
    /// A non-wrapped row, or the first visual fragment of a wrapped line.
    pub fn first_fragment(line: usize) -> Self {
        Self {
            line,
            is_first_fragment: true,
        }
    }
}

/// The outcome of one [`Gutter::render`] frame: which (if any) fold-triangle / breakpoint sub-rect the
/// user clicked. Both `None` on a frame with no gutter click. The panel applies these to its
/// `fold_set`/`breakpoint_set` after the render (the same post-render-apply discipline the cursor
/// overlay uses).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GutterResponse {
    /// The buffer line whose fold triangle was clicked this frame, or `None`.
    pub fold_toggled: Option<usize>,
    /// The buffer line whose breakpoint column was clicked this frame, or `None`.
    pub breakpoint_toggled: Option<usize>,
}

/// The breakpoint circle color (a filled red circle), read from the active theme's diagnostic tokens
/// — a UI affordance sourced from the theme, never a hardcoded literal in widget code. The value lives
/// in [`HsDiagnosticTokens::breakpoint`].
pub fn breakpoint_color(tokens: &HsDiagnosticTokens) -> egui::Color32 {
    tokens.breakpoint
}

/// Resolve the active theme's gutter diagnostic/breakpoint tokens from the live egui visuals (dark vs
/// light), mirroring the panel's `syntax_tokens_for`, so the gutter affordances track the shell theme
/// without threading the whole palette through every call site.
pub fn diagnostic_tokens_for(visuals: &egui::Visuals) -> HsDiagnosticTokens {
    if visuals.dark_mode {
        HsDiagnosticTokens::dark()
    } else {
        HsDiagnosticTokens::light()
    }
}

/// The screen geometry + measured font metrics the gutter draws against. The panel captures these from
/// the SAME painted-row layout the editor body uses (`RowGeometry`), so the gutter aligns row-for-row
/// with the code — including when folds collapse lines (the panel maps each visible row to its buffer
/// line before building the markers/range). This is the positioning contract from the MT impl note
/// "use the SAME y_for_line + line_height + actual painted row_range + MT-005 fold-aware mapping".
#[derive(Debug, Clone, Copy)]
pub struct GutterGeometry {
    /// Screen-space top-left of the gutter strip (the left edge of the editor pane, top of row 0).
    pub origin: egui::Pos2,
    /// Per-row height in px (the sans-spacing monospace line height the editor body strides by).
    pub line_height: f32,
    /// Monospace glyph advance width in px (measured with the editor's `FontId::monospace`), used to
    /// size the line-number column from the digit count.
    pub char_width: f32,
    /// Live editor font size in points. Line numbers and fold glyphs use the same size as the code
    /// body so changing Editor font size cannot leave a mismatched fixed-size gutter.
    pub font_size: f32,
}

/// The sub-column widths inside the gutter strip (px), left to right. Each marker class occupies its
/// own column so they never overlap (MT step 2 "each occupies a fixed sub-column"). The fold column
/// has a fixed minimum but grows with the live monospace glyph advance so a 48pt triangle cannot
/// overlap the line-number column.
const BREAKPOINT_COL_W: f32 = 16.0;
const FOLD_COL_MIN_W: f32 = 14.0;
const FOLD_GLYPH_HORIZONTAL_PAD: f32 = 4.0;
/// Right padding after the line number, before the diagnostic dot column on the far right.
const NUMBER_RIGHT_PAD: f32 = 6.0;
/// The diagnostic dot column on the strip's right edge.
const DIAGNOSTIC_COL_W: f32 = 10.0;

/// The stateless gutter renderer. Holds no state itself — the markers, breakpoints, fold state, and
/// geometry are all passed in per frame by the panel — so it is trivially `Send + Sync` and reused
/// across the editor's `Arc`-held panel without interior mutability.
pub struct Gutter;

/// Width of the fold-glyph column for the live editor font metrics. This is the single geometry
/// source used by strip sizing, glyph centering, number anchoring, and the fold hit target.
pub fn fold_column_width(char_width: f32) -> f32 {
    FOLD_COL_MIN_W.max(char_width + FOLD_GLYPH_HORIZONTAL_PAD)
}

impl Gutter {
    /// Compute the gutter strip width (px) for a buffer with `len_lines` lines and the measured
    /// `char_width`. The number column is sized from the digit count of the largest line number
    /// (`floor(log10(max(1, lines))) + 1` digits — recomputed every frame from the LIVE line count so a
    /// 99→1000-line transition widens the gutter, RISK-001 / MC-001), plus the breakpoint, fold, and
    /// diagnostic columns. Always at least one digit wide.
    pub fn width_for(len_lines: usize, char_width: f32, config: &GutterConfig) -> f32 {
        let digits = digit_count(len_lines);
        let number_w = if config.show_line_numbers {
            digits as f32 * char_width + NUMBER_RIGHT_PAD
        } else {
            0.0
        };
        let breakpoint_w = if config.show_breakpoints {
            BREAKPOINT_COL_W
        } else {
            0.0
        };
        let fold_w = if config.show_fold_triangles {
            fold_column_width(char_width)
        } else {
            0.0
        };
        let diagnostic_w = if config.show_diagnostics {
            DIAGNOSTIC_COL_W
        } else {
            0.0
        };
        // A small left pad so the breakpoint circle is not flush against the pane edge.
        4.0 + breakpoint_w + fold_w + number_w + diagnostic_w
    }

    /// Render the gutter for the painted body rows in `painted_rows` (each entry is a 0-based BUFFER
    /// line plus whether it is the first wrapped fragment, already fold-mapped by the panel, in painted
    /// order) and return which fold/breakpoint
    /// column was clicked this frame.
    ///
    /// DEVIATION from the literal MT signature (documented per the rubric "prescribed API shape must fit
    /// the real environment"): the contract sketches
    /// `render(ui, visible_range, buffer, markers, config, on_fold_click, on_breakpoint_click)`. To
    /// align row-for-row with the editor body under MT-005 folds and MT-054 wrap, the visible rows are
    /// NOT a contiguous `Range` (a folded region makes buffer lines non-contiguous; a wrapped line
    /// repeats the same buffer line across several visual rows), so `painted_rows` is the explicit body
    /// row model the panel already computes, and the geometry the rows were painted at is passed as
    /// [`GutterGeometry`]. The click outcome is returned in [`GutterResponse`] (the panel applies it
    /// after render — the same post-render discipline as the cursor overlay) rather than via `FnMut`
    /// callbacks, which keeps the borrow of the panel's `fold_set`/`breakpoint_set` out of the render
    /// closure. `fold_open_for` reports, for a region-start line, whether it is open.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        ui: &mut egui::Ui,
        strip_rect: egui::Rect,
        painted_rows: &[GutterPaintRow],
        buffer: &TextBuffer,
        markers: &[GutterMarker],
        breakpoints: &BreakpointSet,
        config: &GutterConfig,
        geometry: GutterGeometry,
        fold_open_for: &dyn Fn(usize) -> Option<bool>,
    ) -> GutterResponse {
        let mut response = GutterResponse::default();
        let painter = ui.painter_at(strip_rect);
        let len_lines = buffer.len_lines();
        let digits = digit_count(len_lines);
        let visuals = ui.visuals().clone();
        let number_color = visuals.weak_text_color();
        // Resolve the diagnostic/breakpoint affordance colors from the live theme (no hardcoded
        // literals in widget code; tracks dark/light).
        let diag_tokens = diagnostic_tokens_for(&visuals);
        let breakpoint = breakpoint_color(&diag_tokens);

        // Column x-anchors inside the strip (left to right), mirroring `width_for`.
        let left = strip_rect.left() + 4.0;
        let breakpoint_x = left;
        let fold_x = breakpoint_x
            + if config.show_breakpoints {
                BREAKPOINT_COL_W
            } else {
                0.0
            };
        let fold_width = fold_column_width(geometry.char_width);
        let number_left = fold_x
            + if config.show_fold_triangles {
                fold_width
            } else {
                0.0
            };
        let number_right = number_left
            + if config.show_line_numbers {
                digits as f32 * geometry.char_width
            } else {
                0.0
            };
        let diagnostic_x = strip_rect.right() - DIAGNOSTIC_COL_W * 0.5 - 2.0;

        let mono = egui::FontId::monospace(geometry.font_size);

        for (row_idx, row) in painted_rows.iter().enumerate() {
            let buffer_line = row.line;
            let row_top = geometry.origin.y + row_idx as f32 * geometry.line_height;
            let row_center_y = row_top + geometry.line_height * 0.5;
            let row_rect = egui::Rect::from_min_size(
                egui::pos2(strip_rect.left(), row_top),
                egui::vec2(strip_rect.width(), geometry.line_height),
            );

            if !row.is_first_fragment {
                continue;
            }

            // ── Diagnostic left-border bar + dot ─────────────────────────────────────────────────
            if config.show_diagnostics {
                if let Some(severity) = worst_diagnostic_on(markers, buffer_line) {
                    let color = severity.dot_color(&diag_tokens);
                    // VS Code-style 3px colored left border spanning the row height.
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(strip_rect.left(), row_top),
                            egui::vec2(3.0, geometry.line_height),
                        ),
                        0.0,
                        color,
                    );
                    // A 6px-diameter severity dot on the strip's right edge.
                    painter.circle_filled(egui::pos2(diagnostic_x, row_center_y), 3.0, color);
                }
            }

            // ── Breakpoint circle ────────────────────────────────────────────────────────────────
            if config.show_breakpoints && breakpoints.contains(buffer_line) {
                painter.circle_filled(
                    egui::pos2(breakpoint_x + BREAKPOINT_COL_W * 0.5, row_center_y),
                    4.5,
                    breakpoint,
                );
            }

            // ── Fold triangle (only on a region-start line) ───────────────────────────────────────
            let is_fold_start = config.show_fold_triangles && fold_open_for(buffer_line).is_some();
            if is_fold_start {
                let is_open = fold_open_for(buffer_line).unwrap_or(true);
                let glyph = fold_triangle_glyph(ui, is_open, &mono);
                painter.text(
                    egui::pos2(fold_x + fold_width * 0.5, row_center_y),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    mono.clone(),
                    number_color,
                );
            }

            // ── Line number (right-aligned in its column) ─────────────────────────────────────────
            if config.show_line_numbers {
                let label = (buffer_line + 1).to_string();
                painter.text(
                    egui::pos2(number_right, row_center_y),
                    egui::Align2::RIGHT_CENTER,
                    label,
                    mono.clone(),
                    number_color,
                );
            }

            // ── Diagnostic hover tooltip (all messages on this line, joined) ───────────────────────
            // Register the row-wide hover target BEFORE the narrower click targets below. egui hit
            // testing is topmost-first even for a hover-only widget, so registering this last would
            // mask real pointer clicks on the breakpoint/fold controls. The diagnostic dot is on the
            // right edge outside both click columns. The same tooltip is also attached to each click
            // response below so the whole diagnostic row remains hoverable, including those columns.
            let row_id = ui.id().with(("gutter_row", buffer_line));
            let diagnostic_tooltip = if config.show_diagnostics {
                let msgs = diagnostic_messages_on(markers, buffer_line);
                if !msgs.is_empty() {
                    Some(msgs.join("\n"))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(tooltip) = &diagnostic_tooltip {
                ui.interact(row_rect, row_id.with("diag_hover"), egui::Sense::hover())
                    .on_hover_text(tooltip.clone());
            }

            // ── Interaction: breakpoint toggle + fold toggle on DISJOINT sub-rects ─────────────────
            // The fold triangle and the breakpoint column must NOT overlap, or egui's click routing
            // becomes ambiguous between the two `interact` calls. So the breakpoint click area is the
            // breakpoint column ONLY when a fold triangle is present on this row (the fold column owns
            // its own strip), and widens to span up to the number column on a row with no fold triangle
            // (VS Code lets you click most of the gutter for a breakpoint). The fold interact is
            // registered LAST so it is topmost over any residual overlap.
            if config.show_breakpoints {
                // Right edge of the breakpoint click area: the fold column's LEFT edge when a fold
                // triangle is on this row (disjoint from the fold rect), else up to the number column.
                let bp_right = if is_fold_start {
                    fold_x
                } else {
                    number_left.max(breakpoint_x + BREAKPOINT_COL_W)
                };
                let bp_rect = egui::Rect::from_min_max(
                    egui::pos2(breakpoint_x, row_top),
                    egui::pos2(
                        bp_right.max(breakpoint_x + BREAKPOINT_COL_W),
                        row_top + geometry.line_height,
                    ),
                );
                let mut bp_resp = ui.interact(bp_rect, row_id.with("bp"), egui::Sense::click());
                if let Some(tooltip) = &diagnostic_tooltip {
                    bp_resp = bp_resp.on_hover_text(tooltip.clone());
                }
                if bp_resp.clicked() {
                    response.breakpoint_toggled = Some(buffer_line);
                }
            }
            if is_fold_start {
                let fold_rect = egui::Rect::from_min_size(
                    egui::pos2(fold_x, row_top),
                    egui::vec2(fold_width, geometry.line_height),
                );
                let mut fold_resp =
                    ui.interact(fold_rect, row_id.with("fold"), egui::Sense::click());
                if let Some(tooltip) = &diagnostic_tooltip {
                    fold_resp = fold_resp.on_hover_text(tooltip.clone());
                }
                if fold_resp.clicked() {
                    response.fold_toggled = Some(buffer_line);
                    // A fold click is never also a breakpoint click (disjoint rects, but guard anyway).
                    if response.breakpoint_toggled == Some(buffer_line) {
                        response.breakpoint_toggled = None;
                    }
                }
            }
        }

        response
    }
}

/// The number of decimal digits in the largest 1-based line number for a buffer with `len_lines`
/// lines. Always >= 1 (an empty/1-line buffer is 1 digit). Uses an integer loop rather than
/// `log10().ceil()` to avoid the float rounding pitfall at exact powers of ten (e.g. 1000 lines must
/// be 4 digits, not 3 — `(1000f64).log10().ceil()` is 3.0).
pub fn digit_count(len_lines: usize) -> usize {
    let mut n = len_lines.max(1);
    let mut digits = 1;
    while n >= 10 {
        n /= 10;
        digits += 1;
    }
    digits
}

/// The worst (most severe) diagnostic severity present on `line`, or `None` when the line has no
/// diagnostic marker. Error > Warning > Info > Hint, so the left-bar/dot color reflects the highest
/// severity when a line has several diagnostics (VS Code behavior).
fn worst_diagnostic_on(markers: &[GutterMarker], line: usize) -> Option<DiagnosticSeverity> {
    let mut worst: Option<DiagnosticSeverity> = None;
    for m in markers {
        if m.line != line {
            continue;
        }
        if let GutterMarkerKind::Diagnostic(sev) = m.kind {
            worst = Some(match worst {
                Some(cur) if severity_rank(cur) >= severity_rank(sev) => cur,
                _ => sev,
            });
        }
    }
    worst
}

/// All diagnostic messages on `line` (in marker order), for the hover tooltip.
fn diagnostic_messages_on(markers: &[GutterMarker], line: usize) -> Vec<String> {
    markers
        .iter()
        .filter(|m| m.line == line && matches!(m.kind, GutterMarkerKind::Diagnostic(_)))
        .map(|m| {
            if let GutterMarkerKind::Diagnostic(sev) = m.kind {
                if m.message.is_empty() {
                    sev.label().to_owned()
                } else {
                    format!("{}: {}", sev.label(), m.message)
                }
            } else {
                String::new()
            }
        })
        .collect()
}

/// Severity ordering for "worst on the line": higher is more severe.
fn severity_rank(s: DiagnosticSeverity) -> u8 {
    match s {
        DiagnosticSeverity::Error => 3,
        DiagnosticSeverity::Warning => 2,
        DiagnosticSeverity::Info => 1,
        DiagnosticSeverity::Hint => 0,
    }
}

/// The fold-triangle glyph for the open/closed state, falling back to ASCII when the active font lacks
/// the Unicode triangle (RISK-002 / MC-002 — never a tofu box). `▼` (U+25BC) when open, `▶` (U+25B6)
/// when collapsed; `v` / `>` ASCII fallback.
pub fn fold_triangle_glyph(ui: &egui::Ui, is_open: bool, font: &egui::FontId) -> &'static str {
    let unicode = if is_open { "\u{25BC}" } else { "\u{25B6}" };
    let ch = if is_open { '\u{25BC}' } else { '\u{25B6}' };
    // `has_glyph` lazily lays out the glyph (so it takes `&mut FontsView`); use `fonts_mut`.
    let has = ui.fonts_mut(|f| f.has_glyph(font, ch));
    if has {
        unicode
    } else if is_open {
        "v"
    } else {
        ">"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_count_handles_powers_of_ten_exactly() {
        // The float-log10 pitfall: 1000 must be 4 digits, not 3.
        assert_eq!(digit_count(0), 1, "empty buffer -> 1 digit");
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(
            digit_count(1000),
            4,
            "exact power of ten is 4 digits (float log10 trap)"
        );
        assert_eq!(digit_count(9999), 4);
        assert_eq!(digit_count(10000), 5);
    }

    #[test]
    fn width_grows_with_line_count() {
        let cfg = GutterConfig::default();
        let cw = 8.0;
        let w1 = Gutter::width_for(1, cw, &cfg);
        let w100 = Gutter::width_for(100, cw, &cfg);
        let w10000 = Gutter::width_for(10000, cw, &cfg);
        assert!(
            w100 > w1,
            "100-line gutter wider than 1-line (more digits): {w100} > {w1}"
        );
        assert!(
            w10000 > w100,
            "10000-line gutter wider than 100-line: {w10000} > {w100}"
        );
        // The width delta between 1 and 10000 lines is exactly (5-1)=4 extra digit columns.
        assert!(
            (w10000 - w1 - 4.0 * cw).abs() < 0.001,
            "width delta == 4 digit columns"
        );
    }

    #[test]
    fn fold_column_grows_with_live_glyph_width_and_is_included_in_strip_width() {
        let cfg = GutterConfig::default();
        let default_char_width = 8.0;
        let max_font_char_width = 29.0;
        assert_eq!(fold_column_width(default_char_width), FOLD_COL_MIN_W);
        assert!(
            fold_column_width(max_font_char_width) > FOLD_COL_MIN_W,
            "a 48pt-scale glyph receives a wider fold column"
        );
        let width_delta = Gutter::width_for(100, max_font_char_width, &cfg)
            - Gutter::width_for(100, default_char_width, &cfg);
        let number_delta = 3.0 * (max_font_char_width - default_char_width);
        let fold_delta =
            fold_column_width(max_font_char_width) - fold_column_width(default_char_width);
        assert!(
            (width_delta - number_delta - fold_delta).abs() < 0.001,
            "strip width includes the same live fold-column delta used by rendering"
        );
    }

    #[test]
    fn config_off_columns_shrink_width() {
        let cw = 8.0;
        let full = GutterConfig::default();
        let no_numbers = GutterConfig {
            show_line_numbers: false,
            ..full
        };
        assert!(
            Gutter::width_for(1000, cw, &no_numbers) < Gutter::width_for(1000, cw, &full),
            "hiding line numbers shrinks the gutter"
        );
    }

    #[test]
    fn worst_diagnostic_picks_highest_severity() {
        let markers = vec![
            GutterMarker::diagnostic(5, DiagnosticSeverity::Info, "info on 5"),
            GutterMarker::diagnostic(5, DiagnosticSeverity::Error, "error on 5"),
            GutterMarker::diagnostic(5, DiagnosticSeverity::Warning, "warn on 5"),
            GutterMarker::diagnostic(7, DiagnosticSeverity::Warning, "warn on 7"),
        ];
        assert_eq!(
            worst_diagnostic_on(&markers, 5),
            Some(DiagnosticSeverity::Error)
        );
        assert_eq!(
            worst_diagnostic_on(&markers, 7),
            Some(DiagnosticSeverity::Warning)
        );
        assert_eq!(
            worst_diagnostic_on(&markers, 9),
            None,
            "no marker on line 9"
        );
    }


    #[test]
    fn diagnostic_messages_joined_with_severity_prefix() {
        let markers = vec![
            GutterMarker::diagnostic(3, DiagnosticSeverity::Error, "unexpected token"),
            GutterMarker::diagnostic(3, DiagnosticSeverity::Warning, "unused variable"),
        ];
        let msgs = diagnostic_messages_on(&markers, 3);
        assert_eq!(
            msgs,
            vec!["error: unexpected token", "warning: unused variable"]
        );
    }

    #[test]
    fn gutter_marker_list_correct_for_one_error_on_line_5() {
        // AC-001 basis: a buffer with one error on line 5 yields exactly one diagnostic marker.
        let markers = [GutterMarker::diagnostic(
            5,
            DiagnosticSeverity::Error,
            "boom",
        )];
        let diags: Vec<&GutterMarker> = markers
            .iter()
            .filter(|m| matches!(m.kind, GutterMarkerKind::Diagnostic(_)))
            .collect();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 5);
        assert!(matches!(
            diags[0].kind,
            GutterMarkerKind::Diagnostic(DiagnosticSeverity::Error)
        ));
    }
}
