//! Atlas pages for the UI pass (B3; decision O4: the atlas lives in the renderer): a shelf packer and a residency map.
//! Plain data, no Vulkan type, unit-tested with no device. An entry's pixels never depend on where it sits (the pass
//! fetches texel for texel), so any reset, growth or re-placement draws the same frame.
#![deny(clippy::arithmetic_side_effects)]
use std::collections::HashMap;
use std::hash::Hash;

/// Shelf heights round up to this many texels.
pub const SHELF_STEP: u32 = 4;

/// Where an entry sits on its page, in texels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shelf {
    y: u32,
    h: u32,
    /// Next free x on this shelf.
    x: u32,
}

/// Shelves of height `h` rounded up to [`SHELF_STEP`], filled left to right; an entry goes on the first shelf it fits,
/// else on a new shelf below the last.
#[derive(Clone, Debug, Default)]
pub struct ShelfPacker {
    side: u32,
    shelves: Vec<Shelf>,
    /// Top of the next new shelf.
    bottom: u32,
}

impl ShelfPacker {
    pub fn new(side: u32) -> Self {
        Self { side, shelves: Vec::new(), bottom: 0 }
    }
    pub fn side(&self) -> u32 {
        self.side
    }
    /// The top-left texel of a `w x h` entry, or None when the page has no room (or the entry is empty or too big).
    pub fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w > self.side || h > self.side {
            return None;
        }
        for s in &mut self.shelves {
            if h <= s.h && w <= self.side.saturating_sub(s.x) {
                let at = (s.x, s.y);
                s.x = s.x.saturating_add(w);
                return Some(at);
            }
        }
        let sh = h.div_ceil(SHELF_STEP).saturating_mul(SHELF_STEP).min(self.side);
        if sh > self.side.saturating_sub(self.bottom) {
            return None;
        }
        let at = (0, self.bottom);
        self.shelves.push(Shelf { y: self.bottom, h: sh, x: w });
        self.bottom = self.bottom.saturating_add(sh);
        Some(at)
    }
    pub fn reset(&mut self) {
        self.shelves.clear();
        self.bottom = 0;
    }
    pub fn shelves(&self) -> usize {
        self.shelves.len()
    }
}

/// Counters of one atlas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    /// Entries placed (each needs one upload).
    pub placed: u64,
    /// Whole-page resets (identity change, pass break, explicit reset).
    pub resets: u64,
    /// Page growths (each clears the page at the doubled side).
    pub growths: u64,
}

/// What [`Atlas::place`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    /// Already on the page: no upload.
    Resident(Slot),
    /// Newly placed: the caller uploads its texels.
    New(Slot),
    /// No room at the current side.
    Full,
}

/// One square page with a residency map. Sides double from `min_side` up to `max_side`.
#[derive(Clone, Debug)]
pub struct Atlas<K> {
    packer: ShelfPacker,
    min_side: u32,
    max_side: u32,
    identity: u64,
    entries: HashMap<K, Slot>,
    stats: AtlasStats,
}

impl<K: Copy + Eq + Hash> Atlas<K> {
    pub fn new(min_side: u32, max_side: u32) -> Self {
        let min_side = min_side.max(1);
        Self {
            packer: ShelfPacker::new(min_side),
            min_side,
            max_side: max_side.max(min_side),
            identity: 0,
            entries: HashMap::new(),
            stats: AtlasStats::default(),
        }
    }
    pub fn side(&self) -> u32 {
        self.packer.side()
    }
    pub fn min_side(&self) -> u32 {
        self.min_side
    }
    pub fn max_side(&self) -> u32 {
        self.max_side
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn stats(&self) -> AtlasStats {
        self.stats
    }
    pub fn get(&self, key: &K) -> Option<Slot> {
        self.entries.get(key).copied()
    }
    pub fn place(&mut self, key: K, w: u32, h: u32) -> Placed {
        if let Some(s) = self.entries.get(&key) {
            return Placed::Resident(*s);
        }
        match self.packer.place(w, h) {
            Some((x, y)) => {
                let slot = Slot { x, y, w, h };
                self.entries.insert(key, slot);
                self.stats.placed = self.stats.placed.saturating_add(1);
                Placed::New(slot)
            }
            None => Placed::Full,
        }
    }
    /// Drops `key` (an icon whose mask changed size); its texels stay until the page resets.
    pub fn remove(&mut self, key: &K) {
        self.entries.remove(key);
    }
    /// Doubles the side (capped at `max_side`) and clears the page; false when already at the maximum.
    pub fn grow(&mut self) -> bool {
        let side = self.packer.side();
        if side >= self.max_side {
            return false;
        }
        self.packer = ShelfPacker::new(side.saturating_mul(2).min(self.max_side));
        self.entries.clear();
        self.stats.growths = self.stats.growths.saturating_add(1);
        true
    }
    /// Clears every entry; the side stays.
    pub fn reset(&mut self) {
        self.packer.reset();
        self.entries.clear();
        self.stats.resets = self.stats.resets.saturating_add(1);
    }
    /// Resets when the page identity (text epoch, LUT, scale) changed; true when it did.
    pub fn set_identity(&mut self, identity: u64) -> bool {
        if identity == self.identity {
            return false;
        }
        self.identity = identity;
        self.reset();
        true
    }
}
