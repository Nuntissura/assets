// Copyright 2017 The Servo Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Bidi Embedding Level
//!
//! See [`Level`](struct.Level.html) for more details.
//!
//! <http://www.unicode.org/reports/tr9/#BD2>

use super::char_data::BidiClass;

/// Embedding Level
///
/// Embedding Levels are numbers between 0 and 126 (inclusive), where even values denote a
/// left-to-right (LTR) direction and odd values a right-to-left (RTL) direction.
///
/// This struct maintains a *valid* status for level numbers, meaning that creating a new level, or
/// mutating an existing level, with the value smaller than `0` (before conversion to `u8`) or
/// larger than 125 results in an `Error`.
///
/// <http://www.unicode.org/reports/tr9/#BD2>
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]

#[repr(transparent)]
pub struct Level(u8);

pub const LTR_LEVEL: Level = Level(0);
pub const RTL_LEVEL: Level = Level(1);

const MAX_DEPTH: u8 = 125;
/// During explicit level resolution, embedding level can go as high as `max_depth`.
pub const MAX_EXPLICIT_DEPTH: u8 = MAX_DEPTH;
/// During implicit level resolution, embedding level can go as high as `max_depth + 1`.
pub const MAX_IMPLICIT_DEPTH: u8 = MAX_DEPTH + 1;

/// Errors that can occur on Level creation or mutation
#[derive(Debug, PartialEq)]
pub enum Error {
    /// Out-of-range (invalid) embedding level number.
    OutOfRangeNumber,
}

impl Level {
    /// New LTR level with smallest number value (0).
    #[inline]
    pub fn ltr() -> Level {
        LTR_LEVEL
    }

    /// New RTL level with smallest number value (1).
    #[inline]
    pub fn rtl() -> Level {
        RTL_LEVEL
    }

    /// Maximum depth of the directional status stack during implicit resolutions.
    pub fn max_implicit_depth() -> u8 {
        MAX_IMPLICIT_DEPTH
    }

    /// Maximum depth of the directional status stack during explicit resolutions.
    pub fn max_explicit_depth() -> u8 {
        MAX_EXPLICIT_DEPTH
    }

    // == Inquiries ==

    /// Create new level, fail if number is larger than `max_depth + 1`.
    #[inline]
    pub fn new(number: u8) -> Result<Level, Error> {
        if number <= MAX_IMPLICIT_DEPTH {
            Ok(Level(number))
        } else {
            Err(Error::OutOfRangeNumber)
        }
    }

    /// Create new level, fail if number is larger than `max_depth`.
    #[inline]
    pub fn new_explicit(number: u8) -> Result<Level, Error> {
        if number <= MAX_EXPLICIT_DEPTH {
            Ok(Level(number))
        } else {
            Err(Error::OutOfRangeNumber)
        }
    }

    // == Inquiries ==

    /// The level number.
    #[inline]
    pub fn number(&self) -> u8 {
        self.0
    }

    /// If this level is left-to-right.
    #[inline]
    pub fn is_ltr(&self) -> bool {
        self.0 % 2 == 0
    }

    /// If this level is right-to-left.
    #[inline]
    pub fn is_rtl(&self) -> bool {
        self.0 % 2 == 1
    }

    // == Mutators ==

    /// Raise level by `amount`, fail if number is larger than `max_depth + 1`.
    #[inline]
    pub fn raise(&mut self, amount: u8) -> Result<(), Error> {
        match self.0.checked_add(amount) {
            Some(number) => {
                if number <= MAX_IMPLICIT_DEPTH {
                    self.0 = number;
                    Ok(())
                } else {
                    Err(Error::OutOfRangeNumber)
                }
            }
            None => Err(Error::OutOfRangeNumber),
        }
    }

    /// Raise level by `amount`, fail if number is larger than `max_depth`.
    #[inline]
    pub fn raise_explicit(&mut self, amount: u8) -> Result<(), Error> {
        match self.0.checked_add(amount) {
            Some(number) => {
                if number <= MAX_EXPLICIT_DEPTH {
                    self.0 = number;
                    Ok(())
                } else {
                    Err(Error::OutOfRangeNumber)
                }
            }
            None => Err(Error::OutOfRangeNumber),
        }
    }

    /// Lower level by `amount`, fail if number goes below zero.
    #[inline]
    pub fn lower(&mut self, amount: u8) -> Result<(), Error> {
        match self.0.checked_sub(amount) {
            Some(number) => {
                self.0 = number;
                Ok(())
            }
            None => Err(Error::OutOfRangeNumber),
        }
    }

    // == Helpers ==

    /// The next LTR (even) level greater than this, or fail if number is larger than `max_depth`.
    #[inline]
    pub fn new_explicit_next_ltr(&self) -> Result<Level, Error> {
        Level::new_explicit((self.0 + 2) & !1)
    }

    /// The next RTL (odd) level greater than this, or fail if number is larger than `max_depth`.
    #[inline]
    pub fn new_explicit_next_rtl(&self) -> Result<Level, Error> {
        Level::new_explicit((self.0 + 1) | 1)
    }

    /// The lowest RTL (odd) level greater than or equal to this, or fail if number is larger than
    /// `max_depth + 1`.
    #[inline]
    pub fn new_lowest_ge_rtl(&self) -> Result<Level, Error> {
        Level::new(self.0 | 1)
    }

    /// Generate a character type based on a level (as specified in steps X10 and N2).
    #[inline]
    pub fn bidi_class(&self) -> BidiClass {
        if self.is_rtl() {
            BidiClass::R
        } else {
            BidiClass::L
        }
    }


}

/// If levels has any RTL (odd) level
///
/// This information is usually used to skip re-ordering of text when no RTL level is present
#[inline]
pub fn has_rtl(levels: &[Level]) -> bool {
    levels.iter().any(|&lvl| lvl.is_rtl())
}

impl From<Level> for u8 {
    /// Convert to the level number
    #[inline]
    fn from(val: Level) -> Self {
        val.number()
    }
}
