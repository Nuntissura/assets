// Match harfbuzz code style.
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::comparison_chain)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::upper_case_acronyms)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::wildcard_in_or_patterns)]
#![allow(clippy::identity_op)]
#![allow(clippy::mut_range_bound)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::manual_range_patterns)]
#![allow(clippy::type_complexity)]
#![allow(clippy::wrong_self_convention)]
#![allow(clippy::match_like_matches_macro)]
#![allow(clippy::manual_range_contains)]

// An absent OpenType match is distinct from a typed provider refusal.
macro_rules! lookup_match {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}

mod algs;
#[macro_use]
pub mod buffer;
pub mod common;
pub mod face;

mod machine_cursor;
mod ot;
mod ot_layout;
mod ot_layout_common;
mod ot_layout_gpos_table;
mod ot_layout_gsub_table;
mod ot_layout_gsubgpos;
mod ot_map;
mod ot_shape;

mod ot_shape_normalize;
pub mod ot_shape_plan;
mod ot_shaper;
mod ot_shaper_arabic;
mod ot_shaper_arabic_table;

mod set_digest;
pub mod shape;
mod tag;
mod tag_table;
mod text_parser;
pub(super) mod unicode;
mod unicode_norm;

use crate::provider::ttf::Tag as hb_tag_t;

use self::buffer::hb_glyph_info_t;
use self::face::hb_font_t;

type hb_mask_t = u32;

use self::common::{Direction, Feature, Language, Script, script};
