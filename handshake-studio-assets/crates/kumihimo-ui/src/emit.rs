//! Tree to display list: the toolkit decides what is drawn (O4 layer A); painters only execute. Colours come from the
//! state table of design-system.json through [`crate::tokens::state_style`], never from literals.
use crate::display::{Border, DisplayList, Prim, Side};
use crate::geom::{Point, Rect, Rgba8};
use crate::text::Text;
use crate::tokens::{ColorToken, State, StateStyle, StyleKind, TextToken, Theme, radius, size, state_style, stroke};
use crate::tree::{Align, Flags, Interaction, NodeId, Paint, Tree};

/// The state a node shows (disabled wins, then press, selection, hover).
pub fn state_of(kind: StyleKind, f: &Flags, disabled: bool) -> State {
    let selected_state = match kind {
        StyleKind::Tab | StyleKind::ToolSlot => State::Active,
        _ => State::Selected,
    };
    if disabled {
        State::Disabled
    } else if f.pressed {
        State::Active
    } else if f.selected || f.toggled {
        selected_state
    } else if f.hovered {
        State::Hover
    } else {
        State::Rest
    }
}

/// The resolved style of `kind` in `state`, falling back to rest.
pub fn resolved(kind: StyleKind, state: State) -> StateStyle {
    state_style(kind, state).or_else(|| state_style(kind, State::Rest)).unwrap_or(StateStyle::EMPTY)
}

#[derive(Clone, Copy)]
struct Inherit {
    text: ColorToken,
    icon: ColorToken,
    hot: ColorToken,
    disabled: bool,
}

pub fn emit(tree: &Tree, ui: &Interaction, theme: Theme, text: &mut Text) -> DisplayList {
    let mut list = DisplayList::default();
    if tree.is_empty() {
        return list;
    }
    let inh = Inherit { text: ColorToken::TextPrimary, icon: ColorToken::TextSecondary, hot: ColorToken::TextSecondary, disabled: false };
    let mut rings = Vec::new();
    node(tree, NodeId(0), ui, theme, text, &mut list, inh, &mut rings, 0);
    // Focus rings draw last so no sibling covers them (never clipped by a later fill).
    list.items.extend(rings);
    list
}

fn c(t: ColorToken, theme: Theme) -> Rgba8 {
    t.resolve(theme)
}

#[allow(clippy::too_many_arguments)]
fn node(tree: &Tree, id: NodeId, ui: &Interaction, theme: Theme, text: &mut Text, list: &mut DisplayList, mut inh: Inherit, rings: &mut Vec<Prim>, depth: usize) {
    if depth >= crate::layout::MAX_DEPTH {
        return;
    }
    let n = tree.get(id);
    let b = n.bounds;
    inh.disabled |= n.flags.disabled;
    let mut edges: Vec<Prim> = Vec::new();
    match &n.paint {
        Paint::None => {}
        Paint::Fill(t) => list.push(Prim::Rect { rect: b, color: c(*t, theme) }),
        Paint::Surface { kind, radius: r } => {
            let st = state_of(*kind, &n.flags, inh.disabled);
            let s = resolved(*kind, st);
            if let Some(f) = s.fill {
                if *r > 0.0 {
                    list.push(Prim::RRect { rect: b, radius: *r, fill: c(f, theme), border: None });
                } else {
                    list.push(Prim::Rect { rect: b, color: c(f, theme) });
                }
            }
            // State edges draw over the widget's content (a selected row's accent edge covers its colour tag).
            // Edge thickness follows scale_rules.hairline at any position (F1).
            let mut edge = |t: Option<ColorToken>, side: Side, w: f32| {
                if let Some(t) = t {
                    edges.push(Prim::Edge { owner: b, side, px: w as u8, color: c(t, theme) });
                }
            };
            edge(s.edge_left_2.or(s.gutter_left_2), Side::Left, stroke::EDGE_STATE);
            edge(s.edge_top_2, Side::Top, stroke::EDGE_STATE);
            edge(s.edge_bottom_1, Side::Bottom, stroke::HAIRLINE);
            edge(s.edge_bottom_2, Side::Bottom, stroke::EDGE_STATE);
            if let Some(t) = s.text {
                inh.text = t;
            }
            if let Some(t) = s.icon {
                inh.icon = t;
            }
            inh.hot = s.hot_point.or(s.icon).unwrap_or(inh.icon);
            if n.flags.focused && ui.focus_visible && !inh.disabled {
                let f = resolved(*kind, State::Focus);
                if let Some(ring) = f.ring {
                    let w = if f.ring_w == 0 { stroke::FOCUS_RING } else { f32::from(f.ring_w) };
                    let rect = if f.inset { b } else { Rect::new(b.x0 - w - 1.0, b.y0 - w - 1.0, b.x1 + w + 1.0, b.y1 + w + 1.0) };
                    if let (Some(h), false) = (f.halo, f.inset) {
                        let hr = Rect::new(rect.x0 - stroke::FOCUS_HALO, rect.y0 - stroke::FOCUS_HALO, rect.x1 + stroke::FOCUS_HALO, rect.y1 + stroke::FOCUS_HALO);
                        rings.push(Prim::RRect { rect: hr, radius: r + w + 2.0, fill: Rgba8::TRANSPARENT, border: Some(Border { width: stroke::FOCUS_HALO, color: c(h, theme) }) });
                    }
                    let rr = if f.inset { *r } else { r + w + 1.0 };
                    rings.push(Prim::RRect { rect, radius: rr, fill: Rgba8::TRANSPARENT, border: Some(Border { width: w, color: c(ring, theme) }) });
                }
            }
        }
        Paint::Label { text: s, token, weight, color, align, ellipsis } => {
            // The label's own weight is the one layout and AC-TEXT-01 measured (F2); no weight is inherited.
            let col = if inh.disabled { ColorToken::TextDisabled } else { color.unwrap_or(inh.text) };
            let shown = if *ellipsis { text.ellipsize(s, *token, *weight, b.w()) } else { s.clone() };
            label(list, text, &shown, *token, *weight, b, *align, c(col, theme));
        }
        Paint::Value { text: s, unit } => {
            let col = if inh.disabled { ColorToken::TextDisabled } else { inh.text };
            let pad = crate::tokens::spacing::S3;
            let mut r = b.inset(pad, 0.0, pad, 0.0);
            if let Some(u) = unit {
                let uw = text.measure(u, TextToken::Caption, 0).ceil();
                let ur = Rect::new(r.x1 - uw, r.y0, r.x1, r.y1);
                let ucol = if inh.disabled { ColorToken::TextDisabled } else { ColorToken::TextTertiary };
                label(list, text, u, TextToken::Caption, 0, ur, Align::End, c(ucol, theme));
                r = Rect::new(r.x0, r.y0, (r.x1 - uw - crate::tokens::spacing::S1).max(r.x0), r.y1);
            }
            label(list, text, s, TextToken::Value, 0, r, Align::End, c(col, theme));
        }
        Paint::Icon { icon, size: sz } => {
            let x = b.x0 + ((b.w() - sz) / 2.0).round();
            let y = b.y0 + ((b.h() - sz) / 2.0).round();
            let form = if inh.disabled { ColorToken::TextDisabled } else { inh.icon };
            let hot = if inh.disabled { ColorToken::TextDisabled } else { inh.hot };
            list.push(Prim::Icon { rect: Rect::xywh(x, y, *sz, *sz), icon: *icon, form: c(form, theme), hot: c(hot, theme) });
        }
        Paint::Rail { frac, soft } => {
            let rail_h = crate::widgets::component::SLIDER_RAIL_H;
            let thumb = crate::widgets::component::SLIDER_THUMB;
            let cy = b.y0 + (b.h() / 2.0).round();
            let x0 = b.x0 + thumb / 2.0;
            let x1 = (b.x1 - thumb / 2.0).max(x0);
            let fx = |t: f32| x0 + (x1 - x0) * t.clamp(0.0, 1.0);
            let track = if inh.disabled { ColorToken::TextDisabled } else { ColorToken::SurfaceRailTrack };
            list.push(Prim::Rect { rect: Rect::new(x0, cy - rail_h / 2.0, x1, cy + rail_h / 2.0), color: c(track, theme) });
            if !inh.disabled {
                let (a, z) = (fx(soft.0.min(*frac)), fx(*frac));
                list.push(Prim::Rect { rect: Rect::new(a, cy - rail_h / 2.0, z, cy + rail_h / 2.0), color: c(ColorToken::TextTertiary, theme) });
            }
            let tx = fx(*frac).round();
            let tcol = if inh.disabled {
                ColorToken::TextDisabled
            } else if n.flags.pressed {
                ColorToken::AccentBase
            } else {
                ColorToken::TextPrimary
            };
            let tr = Rect::xywh(tx - thumb / 2.0, cy - thumb / 2.0, thumb, thumb);
            let border = (n.flags.hovered && !inh.disabled).then(|| Border { width: 1.0, color: c(ColorToken::AccentBase, theme) });
            list.push(Prim::RRect { rect: tr, radius: thumb / 2.0, fill: c(tcol, theme), border });
            let _ = soft.1;
        }
        Paint::Divider => list.push(Prim::Edge { owner: b, side: Side::Left, px: 1, color: c(ColorToken::EdgeSubtle, theme) }),
        Paint::TagEdge(t) => list.push(Prim::Rect { rect: b, color: c(*t, theme) }),
        Paint::Thumbnail(img) => {
            match img {
                Some(i) => list.push(Prim::Image { rect: b, image: *i }),
                None => list.push(Prim::Checker {
                    rect: b,
                    a: c(ColorToken::CanvasCheckerA, theme),
                    b: c(ColorToken::CanvasCheckerB, theme),
                    cell: size::ROW_LAYER_THUMB / 4.0,
                }),
            }
            list.push(Prim::RRect { rect: b, radius: radius::R0_DOCKED, fill: Rgba8::TRANSPARENT, border: Some(Border { width: 1.0, color: c(ColorToken::EdgeSubtle, theme) }) });
        }
    }
    if n.flags.clip {
        list.push(Prim::PushScissor(b));
    }
    for &ch in &n.children {
        node(tree, ch, ui, theme, text, list, inh, rings, depth + 1);
    }
    if n.flags.clip {
        list.push(Prim::PopScissor);
    }
    list.items.extend(edges);
}

/// One line of text in `b`, caps centred vertically, baseline on a physical pixel at paint.
#[allow(clippy::too_many_arguments)]
fn label(list: &mut DisplayList, text: &mut Text, s: &str, token: TextToken, weight: u16, b: Rect, align: Align, color: Rgba8) {
    let Ok(line) = text.shape(s, token, weight) else {
        return;
    };
    let cap = if line.cap_height > 0.0 { line.cap_height } else { token.style().size * 0.7 };
    let x = match align {
        Align::Start => b.x0,
        Align::Center => b.x0 + ((b.w() - line.width) / 2.0).round(),
        Align::End => b.x1 - line.width,
    };
    let y = b.y0 + ((b.h() + cap) / 2.0);
    list.push(Prim::Glyphs { origin: Point::new(x, y), token, glyphs: line.glyphs, color });
}
