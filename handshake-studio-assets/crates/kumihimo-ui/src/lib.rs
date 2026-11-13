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
pub const DESCRIPTOR: &str = r#"{"schema":"kumihimo.ui.descriptor@1","crate":"kumihimo-ui","slice":2,"state":"toolkit core: tokens, node tree, stack layout, display list, CPU painter, hit test, focus order, semantic tree, text port with caches; first widgets toolbar, tab row, slider, layer row; slice 2 (SMB-GUI-FRAME-1): frame roles (MenuBar, MenuItem, TabPanel, Status, ComboBox, SearchInput, Canvas, ScrollBar, Region), node seams, ruler, round and tinted paints, the AC-GEOM-01 partition check, fit helpers (fit_labels, fit_tabs, Text::ellipsize_middle), frame widgets (menu_title_row, document_tab, search_field, key_chip, presence_chip, context_chip, dropdown_face, tool_slot, icon_rail_button, rail_caption_button, ghost_icon_button, group_header, status_readouts, ruler) and the contact sheet; B6 (SMB-GUI-MENU-1): Menu, Tooltip and MenuItemCheckBox roles, availability as node properties (STU-SHL-019) in the semantic tree, menu_popup_begin, menu_item (greyed rows keep a visible highlight and a text.secondary reason line), menu_scroll_button, menu_title_row_states, tooltip (L0 and L1), the designer's icon set as CPU two-tint masks (icons::IconSet over design/icons.json, generated into src/icon_data.rs by tools/gen_icons.py)","owns":"widgets, display list, focus, gestures, IME composition state","not_yet":"IME composition, gesture arbiter, docking behaviour, AccessKit adapter (host), Vulkan pass (host renderer), glyph and icon atlases","closure":"no hsk-studio-* crate, checked by the workspace quarantine gate","decisions":"O4 O5 layout_and_text","design_source":"design/design-system.json, generated into src/tokens.rs by tools/gen_tokens.py; design/icons.json (a copy of the designer's mock_icons.py with its hash), generated into src/icon_data.rs by tools/gen_icons.py"}"#;
