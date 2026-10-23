// Copyright 2015 The Servo Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Accessor for `Bidi_Class` property from Unicode Character Database (UCD)

mod tables;
use crate::provider::{Context,ProviderResult};

pub use self::tables::{BidiClass, UNICODE_VERSION};

#[cfg(all())]
use self::tables::bidi_class_table;
use crate::provider::bidi::data_source::BidiMatchedOpeningBracket;
use crate::provider::bidi::BidiClass::*;
#[cfg(all())]
use crate::provider::bidi::BidiDataSource;
/// Hardcoded Bidi data that ships with the unicode-bidi crate.
///
/// This can be enabled with the default `hardcoded-data` Cargo feature.
#[cfg(all())]
pub struct HardcodedBidiData;

#[cfg(all())]
impl BidiDataSource for HardcodedBidiData {
    fn bidi_class(&self, c: char, ctx:&Context<'_>) -> ProviderResult<BidiClass> {
        bsearch_range_value_table(c, bidi_class_table, ctx)
    }
}

/// Find the `BidiClass` of a single char.
#[cfg(all())]
pub fn bidi_class(c: char, ctx:&Context<'_>) -> ProviderResult<BidiClass> {
    bsearch_range_value_table(c, bidi_class_table, ctx)
}

/// If this character is a bracket according to BidiBrackets.txt,
/// return the corresponding *normalized* *opening bracket* of the pair,
/// and whether or not it itself is an opening bracket.
pub(crate) fn bidi_matched_opening_bracket(c:char,ctx:&Context<'_>) -> ProviderResult<Option<BidiMatchedOpeningBracket>> {
    for pair in self::tables::bidi_pairs_table {
        ctx.step()?;
        if pair.0==c || pair.1==c { return Ok(Some(BidiMatchedOpeningBracket { opening:pair.2.unwrap_or(pair.0),is_open:pair.0==c })); }
    }
    Ok(None)
}

pub fn is_rtl(bidi_class: BidiClass) -> bool {
    matches!(bidi_class, RLE | RLO | RLI)
}

#[cfg(all())]
fn bsearch_range_value_table(c:char,table:&'static [(char,char,BidiClass)],ctx:&Context<'_>) -> ProviderResult<BidiClass> {
    let mut left=0; let mut right=table.len();
    while left<right {
        ctx.step()?;
        let mid=left+(right-left)/2;
        let (lo,hi,class)=table[mid];
        if c<lo { right=mid; } else if c>hi { left=mid+1; } else { return Ok(class); }
    }
    Ok(BidiClass::L)
}
