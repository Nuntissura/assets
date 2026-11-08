//! The renderer-free UI toolkit (decisions O4, O5, `direction.blocker_decisions_20261107`): node tree, stack layout,
//! display list, CPU painter, hit test, focus order, semantic tree and the text port. Toolkit-free crates are named
//! `kumihimo-*` from the first commit and take no `hsk-studio-*` dependency; Studio crates depend on them, not the
//! reverse (`tools/workspace-probe.rs --self-test`). No window, no GPU: the Vulkan pass draws the same display list.
//!
//! Flow for a host: build a panel with [`tree::Panel::update`] (a pure view function over [`tree::Builder`], rebuilt
//! only when its inputs change), paint [`tree::Panel::display_list`] with [`paint::paint`] into a [`paint::Canvas`]
//! (or hand the list to a GPU pass), route pointer and keys through [`tree::Interaction`], and answer `gui.tree` with
//! [`semantic::semantic`]. Text comes from a host [`text::TextPort`] wrapped in [`text::Text`] (caches included).
//!
//! The package keeps literal version fields so it can leave this workspace when it is published.
#![forbid(unsafe_code)]

pub mod checks;
pub mod display;
pub mod emit;
pub mod fit;
pub mod gallery;
pub mod geom;
pub mod hit;
pub mod icon_data;
pub mod icons;
pub mod layout;
pub mod paint;
pub mod semantic;
pub mod text;
pub mod tokens;
pub mod tree;
pub mod widgets;

/// Self description for models and tooling.
pub const DESCRIPTOR: &str = r#"{"schema":"kumihimo.ui.descriptor@1","crate":"kumihimo-ui","slice":1,"state":"toolkit core: tokens, node tree, stack layout, display list, CPU painter, hit test, focus order, semantic tree, text port with caches; first widgets toolbar, tab row, slider, layer row","owns":"widgets, display list, focus, gestures, IME composition state","not_yet":"IME composition, gesture arbiter, menus, docking, AccessKit adapter (host), Vulkan pass (host renderer)","closure":"no hsk-studio-* crate, checked by the workspace quarantine gate","decisions":"O4 O5 layout_and_text","design_source":"design/design-system.json, generated into src/tokens.rs by tools/gen_tokens.py"}"#;
