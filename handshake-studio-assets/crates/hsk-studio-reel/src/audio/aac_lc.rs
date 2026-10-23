//! AAC-LC raw data block decoder (ISO/IEC 14496-3 subpart 4, General Audio), Studio-owned. Syntax:
//! SCE / CPE / LFE elements (channel configurations 1-7), DSE and FIL skipped (SBR extension payloads
//! in FIL are ignored: the LC core is decoded), CCE and PCE refused; ics_info with window grouping,
//! section data, scale factors (Huffman, intensity positions, PNS energies), pulse data, TNS, spectral
//! Huffman codebooks 1-11 with escapes; dequantisation, M/S, intensity stereo (sign from the codebook,
//! inverted by ms_used only when ms_mask_present == 1, subclause 4.6.8.2.3), PNS, TNS and the filterbank
//! of `aac_dsp`. Huffman tables are ISO/IEC 14496-3 4.A.1 as transcribed by soundkit-aac-lc 0.12.3
//! (MIT, crate NOTICE) and generated into aac_tables.rs. Every read is bounds-checked; every count
//! taken from the bitstream is checked against the frame geometry before it indexes anything.
#![deny(clippy::arithmetic_side_effects)]
#![allow(clippy::needless_range_loop)]
use super::aac_dsp::{EIGHT_SHORT, Filterbank, tns_filter};
use super::bits::Bits;
use crate::error::ReelError;
use std::sync::OnceLock;

include!("aac_tables.rs");

const ZERO_HCB: u8 = 0;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

/// Huffman tree: node children; a negative value is a leaf `-(symbol + 1)`, `i32::MIN` absent.
struct Tree {
    nodes: Vec<[i32; 2]>,
}

impl Tree {
    fn build(entries: impl Iterator<Item = (u32, u8)>) -> Self {
        let mut nodes = vec![[i32::MIN; 2]];
        for (symbol, (code, len)) in entries.enumerate() {
            if len == 0 {
                continue;
            }
            let mut at = 0usize;
            for bit in (0..u32::from(len)).rev() {
                let b = ((code >> bit) & 1) as usize;
                let last = bit == 0;
                let child = nodes.get(at).map_or(i32::MIN, |n| n[b]);
                if last {
                    if let Some(n) = nodes.get_mut(at) {
                        n[b] = (symbol as i32).saturating_add(1).wrapping_neg();
                    }
                } else if child >= 0 && child != i32::MIN {
                    at = child as usize;
                } else {
                    let next = nodes.len();
                    nodes.push([i32::MIN; 2]);
                    if let Some(n) = nodes.get_mut(at) {
                        n[b] = next as i32;
                    }
                    at = next;
                }
            }
        }
        Self { nodes }
    }

    fn decode(&self, b: &mut Bits<'_>) -> Result<usize, ReelError> {
        let mut at = 0usize;
        for _ in 0..32 {
            let bit = usize::from(b.bit().ok_or(corrupt("aac-huffman-truncated"))?);
            let next = self.nodes.get(at).map_or(i32::MIN, |n| n[bit]);
            if next == i32::MIN {
                return Err(corrupt("aac-huffman-code"));
            }
            if next < 0 {
                return Ok(next.wrapping_neg().wrapping_sub(1) as usize);
            }
            at = next as usize;
        }
        Err(corrupt("aac-huffman-code"))
    }
}

struct Book {
    tree: Tree,
    dim: usize,
    side: usize,
    offset: i32,
}

fn books() -> &'static [Book; 11] {
    static BOOKS: OnceLock<[Book; 11]> = OnceLock::new();
    BOOKS.get_or_init(|| {
        let mk = |t: &[(u16, u8)], dim: usize, side: usize, offset: i32| Book {
            tree: Tree::build(t.iter().map(|(c, l)| (u32::from(*c), *l))),
            dim,
            side,
            offset,
        };
        [
            mk(&CB1, CB1_DIM, CB1_SIDE, CB1_OFFSET),
            mk(&CB2, CB2_DIM, CB2_SIDE, CB2_OFFSET),
            mk(&CB3, CB3_DIM, CB3_SIDE, CB3_OFFSET),
            mk(&CB4, CB4_DIM, CB4_SIDE, CB4_OFFSET),
            mk(&CB5, CB5_DIM, CB5_SIDE, CB5_OFFSET),
            mk(&CB6, CB6_DIM, CB6_SIDE, CB6_OFFSET),
            mk(&CB7, CB7_DIM, CB7_SIDE, CB7_OFFSET),
            mk(&CB8, CB8_DIM, CB8_SIDE, CB8_OFFSET),
            mk(&CB9, CB9_DIM, CB9_SIDE, CB9_OFFSET),
            mk(&CB10, CB10_DIM, CB10_SIDE, CB10_OFFSET),
            mk(&CB11, CB11_DIM, CB11_SIDE, CB11_OFFSET),
        ]
    })
}

fn sf_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| Tree::build(SF_CODES.iter().copied()))
}

fn pow43() -> &'static [f64] {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    TABLE.get_or_init(|| (0..8192u32).map(|i| f64::from(i).powf(4.0 / 3.0)).collect())
}

fn swb_long(sf_index: usize) -> Option<&'static [u16]> {
    Some(match sf_index {
        0 | 1 => &SWB_OFFSET_1024_96[..],
        2 => &SWB_OFFSET_1024_64[..],
        3 | 4 => &SWB_OFFSET_1024_48[..],
        5 => &SWB_OFFSET_1024_32[..],
        6 | 7 => &SWB_OFFSET_1024_24[..],
        8..=10 => &SWB_OFFSET_1024_16[..],
        11 | 12 => &SWB_OFFSET_1024_8[..],
        _ => return None,
    })
}

fn swb_short(sf_index: usize) -> Option<&'static [u16]> {
    Some(match sf_index {
        0..=2 => &SWB_OFFSET_128_96[..],
        3..=5 => &SWB_OFFSET_128_48[..],
        6 | 7 => &SWB_OFFSET_128_24[..],
        8..=10 => &SWB_OFFSET_128_16[..],
        11 | 12 => &SWB_OFFSET_128_8[..],
        _ => return None,
    })
}

/// One TNS filter: (length in bands, order, direction downward, LPC coefficients).
type TnsFilter = (usize, usize, bool, Vec<f64>);

#[derive(Clone)]
struct Tns {
    /// Per window, its filters.
    filters: Vec<Vec<TnsFilter>>,
}

#[derive(Clone)]
struct Ics {
    window_sequence: u8,
    window_shape: bool,
    max_sfb: usize,
    num_windows: usize,
    groups: Vec<usize>,
    sfb_cb: [[u8; 64]; 8],
    sf: [[i32; 64]; 8],
    spec: [f32; 1024],
    q: [i32; 1024],
    tns: Option<Tns>,
}

impl Ics {
    fn new() -> Self {
        Self {
            window_sequence: 0,
            window_shape: false,
            max_sfb: 0,
            num_windows: 1,
            groups: vec![1],
            sfb_cb: [[0; 64]; 8],
            sf: [[0; 64]; 8],
            spec: [0.0; 1024],
            q: [0; 1024],
            tns: None,
        }
    }

    fn short(&self) -> bool {
        self.window_sequence == EIGHT_SHORT
    }
}

struct ChannelState {
    overlap: [f64; 1024],
    prev_shape: bool,
}

/// Elements of channel configurations 1..=7 (ISO/IEC 14496-3 Table 1.19): 1 = SCE, 2 = CPE, 3 = LFE.
fn layout(config: u8) -> Option<&'static [u8]> {
    Some(match config {
        1 => &[1],
        2 => &[2],
        3 => &[1, 2],
        4 => &[1, 2, 1],
        5 => &[1, 2, 2],
        6 => &[1, 2, 2, 3],
        7 => &[1, 2, 2, 2, 3],
        _ => return None,
    })
}

pub(crate) fn config_channels(config: u8) -> Option<usize> {
    layout(config).map(|l| l.iter().map(|e| if *e == 2 { 2usize } else { 1 }).sum())
}

pub(crate) struct AacLc {
    long_swb: &'static [u16],
    short_swb: &'static [u16],
    tns_max_long: usize,
    tns_max_short: usize,
    elements: &'static [u8],
    channels: Vec<ChannelState>,
    fb: Filterbank,
    noise: u32,
}

impl AacLc {
    /// LC decoder for sampling-frequency index `sf_index` (0..=12) and channel configuration 1..=7.
    pub(crate) fn new(sf_index: u8, config: u8) -> Result<Self, ReelError> {
        let i = usize::from(sf_index);
        let (long_swb, short_swb) =
            swb_long(i)
                .zip(swb_short(i))
                .ok_or(ReelError::Unsupported {
                    what: "aac-sampling-frequency-index",
                })?;
        let elements = layout(config).ok_or(ReelError::Unsupported {
            what: "aac-channel-configuration",
        })?;
        let n = config_channels(config).unwrap_or(0);
        Ok(Self {
            long_swb,
            short_swb,
            tns_max_long: usize::from(TNS_MAX_BANDS_1024.get(i).copied().unwrap_or(0)),
            tns_max_short: usize::from(TNS_MAX_BANDS_128.get(i).copied().unwrap_or(0)),
            elements,
            channels: (0..n)
                .map(|_| ChannelState {
                    overlap: [0.0; 1024],
                    prev_shape: false,
                })
                .collect(),
            fb: Filterbank::new(),
            noise: 0x1F2E_3D4C,
        })
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Decode one access unit (raw_data_block); appends 1024 interleaved frames.
    pub(crate) fn decode(&mut self, au: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        let mut b = Bits::new(au);
        let nch = self.channels.len();
        let mut pcm: Vec<[f64; 1024]> = vec![[0.0; 1024]; nch];
        let mut element = 0usize;
        let mut ch = 0usize;
        loop {
            let id = b.read(3).ok_or(corrupt("aac-raw-data-block"))?;
            match id {
                7 => break,
                0 | 3 => {
                    let expected = self.elements.get(element).copied();
                    let want = if id == 0 { 1 } else { 3 };
                    if expected != Some(want) {
                        return Err(corrupt("aac-element-order"));
                    }
                    b.read(4).ok_or(corrupt("aac-element-tag"))?;
                    let mut ics = Ics::new();
                    self.read_ics(&mut b, &mut ics, false)?;
                    self.finish_spectrum(&mut ics, None)?;
                    self.apply_tns(&mut ics);
                    let state = self
                        .channels
                        .get_mut(ch)
                        .ok_or(corrupt("aac-channel-count"))?;
                    let dst = pcm.get_mut(ch).ok_or(corrupt("aac-channel-count"))?;
                    self.fb.synthesize(
                        &ics.spec,
                        ics.window_sequence,
                        ics.window_shape,
                        state.prev_shape,
                        &mut state.overlap,
                        dst,
                    );
                    state.prev_shape = ics.window_shape;
                    ch = ch.saturating_add(1);
                    element = element.saturating_add(1);
                }
                1 => {
                    if self.elements.get(element).copied() != Some(2) {
                        return Err(corrupt("aac-element-order"));
                    }
                    b.read(4).ok_or(corrupt("aac-element-tag"))?;
                    self.decode_cpe(&mut b, &mut pcm, ch)?;
                    ch = ch.saturating_add(2);
                    element = element.saturating_add(1);
                }
                4 => {
                    // data_stream_element: tag, align flag, count (+ esc), [align], bytes.
                    b.read(4).ok_or(corrupt("aac-dse"))?;
                    let align = b.bit().ok_or(corrupt("aac-dse"))?;
                    let mut count = b.read(8).ok_or(corrupt("aac-dse"))? as usize;
                    if count == 255 {
                        count = count.saturating_add(b.read(8).ok_or(corrupt("aac-dse"))? as usize);
                    }
                    if align {
                        b.align();
                    }
                    for _ in 0..count {
                        b.read(8).ok_or(corrupt("aac-dse"))?;
                    }
                }
                6 => {
                    // fill_element: extension payloads (SBR included) are skipped.
                    let mut count = b.read(4).ok_or(corrupt("aac-fil"))? as usize;
                    if count == 15 {
                        count = count
                            .saturating_add(b.read(8).ok_or(corrupt("aac-fil"))? as usize)
                            .saturating_sub(1);
                    }
                    for _ in 0..count {
                        b.read(8).ok_or(corrupt("aac-fil"))?;
                    }
                }
                2 => {
                    return Err(ReelError::Unsupported {
                        what: "aac-coupling-channel-element",
                    });
                }
                _ => {
                    return Err(ReelError::Unsupported {
                        what: "aac-program-config-element",
                    });
                }
            }
        }
        if ch != nch || element != self.elements.len() {
            return Err(corrupt("aac-channel-count"));
        }
        let total = nch.saturating_mul(1024);
        out.try_reserve(total)
            .map_err(|_| ReelError::LimitExceeded {
                what: "audio-alloc",
            })?;
        for i in 0..1024 {
            for p in &pcm {
                let v = p.get(i).copied().unwrap_or(0.0) / 32768.0;
                out.push(if v.is_finite() {
                    v.clamp(-8.0, 8.0) as f32
                } else {
                    0.0
                });
            }
        }
        Ok(1024)
    }

    fn decode_cpe(
        &mut self,
        b: &mut Bits<'_>,
        pcm: &mut [[f64; 1024]],
        ch: usize,
    ) -> Result<(), ReelError> {
        let common = b.bit().ok_or(corrupt("aac-cpe"))?;
        let mut left = Ics::new();
        let mut right = Ics::new();
        let mut ms_present = 0u32;
        let mut ms_used = [[false; 64]; 8];
        if common {
            self.read_ics_info(b, &mut left)?;
            ms_present = b.read(2).ok_or(corrupt("aac-ms-mask"))?;
            if ms_present == 3 {
                return Err(corrupt("aac-ms-mask"));
            }
            if ms_present == 1 {
                for g in 0..left.groups.len() {
                    for sfb in 0..left.max_sfb {
                        ms_used[g][sfb] = b.bit().ok_or(corrupt("aac-ms-mask"))?;
                    }
                }
            } else if ms_present == 2 {
                ms_used = [[true; 64]; 8];
            }
            right.window_sequence = left.window_sequence;
            right.window_shape = left.window_shape;
            right.max_sfb = left.max_sfb;
            right.num_windows = left.num_windows;
            right.groups = left.groups.clone();
        }
        self.read_ics(b, &mut left, common)?;
        self.read_ics(b, &mut right, common)?;
        // PNS bands shared by both channels under M/S get the same noise (4.6.13.3).
        self.finish_spectrum(&mut left, None)?;
        let shared = if common && ms_present != 0 {
            Some((&left, &ms_used))
        } else {
            None
        };
        let left_copy = shared.map(|(l, m)| (l.clone(), *m));
        self.finish_spectrum(&mut right, left_copy.as_ref().map(|(l, m)| (l, m)))?;
        if common {
            self.stereo(&mut left, &mut right, ms_present, &ms_used)?;
        }
        self.apply_tns(&mut left);
        self.apply_tns(&mut right);
        for (k, ics) in [(0usize, &left), (1, &right)] {
            let idx = ch.saturating_add(k);
            let state = self
                .channels
                .get_mut(idx)
                .ok_or(corrupt("aac-channel-count"))?;
            let dst = pcm.get_mut(idx).ok_or(corrupt("aac-channel-count"))?;
            self.fb.synthesize(
                &ics.spec,
                ics.window_sequence,
                ics.window_shape,
                state.prev_shape,
                &mut state.overlap,
                dst,
            );
            state.prev_shape = ics.window_shape;
        }
        Ok(())
    }

    fn swb(&self, ics: &Ics) -> &'static [u16] {
        if ics.short() {
            self.short_swb
        } else {
            self.long_swb
        }
    }

    fn read_ics_info(&self, b: &mut Bits<'_>, ics: &mut Ics) -> Result<(), ReelError> {
        if b.bit().ok_or(corrupt("aac-ics-info"))? {
            return Err(corrupt("aac-ics-reserved-bit"));
        }
        ics.window_sequence = b.read(2).ok_or(corrupt("aac-ics-info"))? as u8;
        ics.window_shape = b.bit().ok_or(corrupt("aac-ics-info"))?;
        if ics.short() {
            ics.max_sfb = b.read(4).ok_or(corrupt("aac-ics-info"))? as usize;
            let grouping = b.read(7).ok_or(corrupt("aac-ics-info"))?;
            ics.num_windows = 8;
            ics.groups = vec![1];
            for i in (0..7).rev() {
                if (grouping >> i) & 1 == 1 {
                    if let Some(last) = ics.groups.last_mut() {
                        *last = last.saturating_add(1);
                    }
                } else {
                    ics.groups.push(1);
                }
            }
        } else {
            ics.max_sfb = b.read(6).ok_or(corrupt("aac-ics-info"))? as usize;
            ics.num_windows = 1;
            ics.groups = vec![1];
            if b.bit().ok_or(corrupt("aac-ics-info"))? {
                return Err(ReelError::Unsupported {
                    what: "aac-prediction-not-lc",
                });
            }
        }
        let bands = self.swb(ics).len().saturating_sub(1);
        if ics.max_sfb > bands {
            return Err(corrupt("aac-max-sfb"));
        }
        Ok(())
    }

    fn read_ics(&self, b: &mut Bits<'_>, ics: &mut Ics, common: bool) -> Result<(), ReelError> {
        let global_gain = b.read(8).ok_or(corrupt("aac-ics"))? as i32;
        if !common {
            self.read_ics_info(b, ics)?;
        }
        let swb = self.swb(ics);
        // section_data
        let (sect_bits, esc) = if ics.short() { (3u32, 7u32) } else { (5, 31) };
        for g in 0..ics.groups.len() {
            let mut k = 0usize;
            while k < ics.max_sfb {
                let cb = b.read(4).ok_or(corrupt("aac-section"))? as u8;
                if cb == 12 {
                    return Err(corrupt("aac-section-codebook"));
                }
                let mut len = 0usize;
                loop {
                    let incr = b.read(sect_bits).ok_or(corrupt("aac-section"))?;
                    len = len.saturating_add(incr as usize);
                    if incr != esc {
                        break;
                    }
                }
                let end = k.saturating_add(len);
                if end > ics.max_sfb {
                    return Err(corrupt("aac-section-length"));
                }
                for sfb in k..end {
                    ics.sfb_cb[g][sfb] = cb;
                }
                if len == 0 {
                    return Err(corrupt("aac-section-length"));
                }
                k = end;
            }
        }
        // scale_factor_data
        let tree = sf_tree();
        let mut scale = global_gain;
        let mut is_pos = 0i32;
        let mut noise = global_gain.wrapping_sub(90);
        let mut noise_first = true;
        for g in 0..ics.groups.len() {
            for sfb in 0..ics.max_sfb {
                let cb = ics.sfb_cb[g][sfb];
                let v = match cb {
                    ZERO_HCB => 0,
                    INTENSITY_HCB | INTENSITY_HCB2 => {
                        is_pos = is_pos.wrapping_add(tree.decode(b)? as i32).wrapping_sub(60);
                        is_pos
                    }
                    NOISE_HCB => {
                        if noise_first {
                            noise_first = false;
                            noise = noise
                                .wrapping_add(b.read(9).ok_or(corrupt("aac-noise"))? as i32)
                                .wrapping_sub(256);
                        } else {
                            noise = noise.wrapping_add(tree.decode(b)? as i32).wrapping_sub(60);
                        }
                        noise
                    }
                    _ => {
                        scale = scale.wrapping_add(tree.decode(b)? as i32).wrapping_sub(60);
                        if !(0..=255).contains(&scale) {
                            return Err(corrupt("aac-scale-factor"));
                        }
                        scale
                    }
                };
                if !(-512..=512).contains(&v) {
                    return Err(corrupt("aac-scale-factor"));
                }
                ics.sf[g][sfb] = v;
            }
        }
        // pulse_data
        let mut pulses: Vec<(usize, i32)> = Vec::new();
        if b.bit().ok_or(corrupt("aac-ics"))? {
            if ics.short() {
                return Err(corrupt("aac-pulse-in-short-window"));
            }
            let n = (b.read(2).ok_or(corrupt("aac-pulse"))? as usize).saturating_add(1);
            let start_sfb = b.read(6).ok_or(corrupt("aac-pulse"))? as usize;
            let mut k = usize::from(*swb.get(start_sfb).ok_or(corrupt("aac-pulse"))?);
            for _ in 0..n {
                k = k.saturating_add(b.read(5).ok_or(corrupt("aac-pulse"))? as usize);
                let amp = b.read(4).ok_or(corrupt("aac-pulse"))? as i32;
                if k >= 1024 {
                    return Err(corrupt("aac-pulse"));
                }
                pulses.push((k, amp));
            }
        }
        // tns_data
        let tns = if b.bit().ok_or(corrupt("aac-ics"))? {
            Some(self.read_tns(b, ics)?)
        } else {
            None
        };
        if b.bit().ok_or(corrupt("aac-ics"))? {
            return Err(ReelError::Unsupported {
                what: "aac-gain-control-not-lc",
            });
        }
        // spectral_data
        let books = books();
        let mut w0 = 0usize;
        for (g, &glen) in ics.groups.iter().enumerate() {
            for sfb in 0..ics.max_sfb {
                let cb = ics.sfb_cb[g][sfb];
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let book = books
                    .get(usize::from(cb).saturating_sub(1))
                    .ok_or(corrupt("aac-codebook"))?;
                let lo = usize::from(*swb.get(sfb).ok_or(corrupt("aac-swb"))?);
                let hi = usize::from(*swb.get(sfb.saturating_add(1)).ok_or(corrupt("aac-swb"))?);
                for win in 0..glen {
                    let base = w0.saturating_add(win).saturating_mul(128);
                    let mut k = lo;
                    while k < hi {
                        let mut vals = [0i32; 4];
                        decode_tuple(b, book, cb, &mut vals)?;
                        for (i, v) in vals.iter().take(book.dim).enumerate() {
                            let at = if ics.short() {
                                base.saturating_add(k).saturating_add(i)
                            } else {
                                k.saturating_add(i)
                            };
                            if let Some(slot) = ics.q.get_mut(at) {
                                *slot = *v;
                            }
                        }
                        k = k.saturating_add(book.dim);
                    }
                }
            }
            w0 = w0.saturating_add(glen);
        }
        for (k, amp) in pulses {
            if let Some(q) = ics.q.get_mut(k) {
                *q = if *q > 0 {
                    q.saturating_add(amp)
                } else {
                    q.saturating_sub(amp)
                };
            }
        }
        // Dequantise regular bands into `spec` (window-major for short windows).
        let p43 = pow43();
        let mut w0 = 0usize;
        for (g, &glen) in ics.groups.iter().enumerate() {
            for sfb in 0..ics.max_sfb {
                let cb = ics.sfb_cb[g][sfb];
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let gain = 2f64.powf(0.25 * f64::from(ics.sf[g][sfb].wrapping_sub(100)));
                let lo = usize::from(swb.get(sfb).copied().unwrap_or(0));
                let hi = usize::from(swb.get(sfb.saturating_add(1)).copied().unwrap_or(0));
                for win in 0..glen {
                    let base = if ics.short() {
                        w0.saturating_add(win).saturating_mul(128)
                    } else {
                        0
                    };
                    for k in lo..hi {
                        let at = base.saturating_add(k);
                        let q = ics.q.get(at).copied().unwrap_or(0);
                        let mag = p43.get(q.unsigned_abs() as usize).copied().unwrap_or(0.0);
                        if let Some(s) = ics.spec.get_mut(at) {
                            *s = (f64::from(q.signum()) * mag * gain) as f32;
                        }
                    }
                }
            }
            w0 = w0.saturating_add(glen);
        }
        ics.tns = tns;
        Ok(())
    }

    fn read_tns(&self, b: &mut Bits<'_>, ics: &Ics) -> Result<Tns, ReelError> {
        let short = ics.short();
        let (filt_bits, len_bits, order_bits, max_order) = if short {
            (1, 4, 3, 7usize)
        } else {
            (2, 6, 5, 12)
        };
        let mut filters = Vec::new();
        for _ in 0..ics.num_windows {
            let n_filt = b.read(filt_bits).ok_or(corrupt("aac-tns"))?;
            let mut list = Vec::new();
            if n_filt > 0 {
                let coef_res = b.read(1).ok_or(corrupt("aac-tns"))?;
                for _ in 0..n_filt {
                    let length = b.read(len_bits).ok_or(corrupt("aac-tns"))? as usize;
                    let order = b.read(order_bits).ok_or(corrupt("aac-tns"))? as usize;
                    if order > max_order {
                        return Err(corrupt("aac-tns-order"));
                    }
                    if order == 0 {
                        list.push((length, 0, false, Vec::new()));
                        continue;
                    }
                    let direction = b.bit().ok_or(corrupt("aac-tns"))?;
                    let compress = b.read(1).ok_or(corrupt("aac-tns"))?;
                    let bits = coef_res.saturating_add(3).saturating_sub(compress);
                    let res_bits = coef_res.saturating_add(3);
                    let iqfac = (f64::from(1u32 << res_bits.saturating_sub(1)) - 0.5)
                        / (std::f64::consts::PI / 2.0);
                    let iqfac_m = (f64::from(1u32 << res_bits.saturating_sub(1)) + 0.5)
                        / (std::f64::consts::PI / 2.0);
                    let mut refl = Vec::with_capacity(order);
                    for _ in 0..order {
                        let raw = b.read(bits).ok_or(corrupt("aac-tns"))?;
                        let shift = 32u32.saturating_sub(bits);
                        let c = ((raw << shift) as i32) >> shift;
                        let v = f64::from(c);
                        refl.push(if c >= 0 {
                            (v / iqfac).sin()
                        } else {
                            (v / iqfac_m).sin()
                        });
                    }
                    // Reflection -> LPC coefficients (4.6.9.3, tns_decode_coef).
                    let mut lpc = vec![0.0f64; order];
                    let mut tmp = vec![0.0f64; order];
                    for m in 0..order {
                        tmp[..m].copy_from_slice(&lpc[..m]);
                        for i in 0..m {
                            lpc[i] = tmp[i] + refl[m] * tmp[m.wrapping_sub(1).wrapping_sub(i)];
                        }
                        lpc[m] = refl[m];
                    }
                    list.push((length, order, direction, lpc));
                }
            }
            filters.push(list);
        }
        Ok(Tns { filters })
    }

    /// TNS after the stereo tools (ISO/IEC 14496-3 decoder order: M/S, intensity, then TNS).
    fn apply_tns(&self, ics: &mut Ics) {
        let Some(tns) = ics.tns.take() else {
            return;
        };
        let short = ics.short();
        let swb = self.swb(ics);
        let bands = swb.len().saturating_sub(1);
        let max_bands = if short {
            self.tns_max_short
        } else {
            self.tns_max_long
        };
        let limit = max_bands.min(ics.max_sfb);
        for (w, list) in tns.filters.iter().enumerate() {
            let mut bottom = bands;
            for (length, order, downward, lpc) in list {
                let top = bottom;
                bottom = top.saturating_sub(*length);
                if *order == 0 {
                    continue;
                }
                let start = usize::from(swb.get(bottom.min(limit)).copied().unwrap_or(0));
                let end = usize::from(swb.get(top.min(limit)).copied().unwrap_or(0));
                let base = if short { w.saturating_mul(128) } else { 0 };
                tns_filter(
                    &mut ics.spec,
                    base.saturating_add(start),
                    base.saturating_add(end),
                    *downward,
                    lpc,
                );
            }
        }
    }

    /// PNS bands of `ics` (noise from the generator; under M/S with both bands noise, `shared` gives
    /// the left channel's normalised noise).
    fn finish_spectrum(
        &mut self,
        ics: &mut Ics,
        shared: Option<(&Ics, &[[bool; 64]; 8])>,
    ) -> Result<(), ReelError> {
        let swb = self.swb(ics);
        let mut w0 = 0usize;
        for (g, &glen) in ics.groups.clone().iter().enumerate() {
            for sfb in 0..ics.max_sfb {
                if ics.sfb_cb[g][sfb] != NOISE_HCB {
                    continue;
                }
                let lo = usize::from(swb.get(sfb).copied().unwrap_or(0));
                let hi = usize::from(swb.get(sfb.saturating_add(1)).copied().unwrap_or(0));
                let amp = 2f64.powf(0.25 * f64::from(ics.sf[g][sfb]));
                for win in 0..glen {
                    let base = if ics.short() {
                        w0.saturating_add(win).saturating_mul(128)
                    } else {
                        0
                    };
                    let copy = shared.and_then(|(l, m)| {
                        (m[g][sfb] && l.sfb_cb[g][sfb] == NOISE_HCB).then_some(l)
                    });
                    let mut energy = 0f64;
                    for k in lo..hi {
                        let at = base.saturating_add(k);
                        let r = match copy {
                            Some(l) => f64::from(l.spec.get(at).copied().unwrap_or(0.0)),
                            None => {
                                self.noise = self
                                    .noise
                                    .wrapping_mul(1_664_525)
                                    .wrapping_add(1_013_904_223);
                                f64::from(self.noise as i32)
                            }
                        };
                        energy += r * r;
                        if let Some(s) = ics.spec.get_mut(at) {
                            *s = r as f32;
                        }
                    }
                    let scale = if energy > 0.0 {
                        amp / energy.sqrt()
                    } else {
                        0.0
                    };
                    for k in lo..hi {
                        if let Some(s) = ics.spec.get_mut(base.saturating_add(k)) {
                            *s = (f64::from(*s) * scale) as f32;
                        }
                    }
                }
            }
            w0 = w0.saturating_add(glen);
        }
        Ok(())
    }

    fn stereo(
        &self,
        left: &mut Ics,
        right: &mut Ics,
        ms_present: u32,
        ms_used: &[[bool; 64]; 8],
    ) -> Result<(), ReelError> {
        let swb = self.swb(left);
        let mut w0 = 0usize;
        for (g, &glen) in left.groups.clone().iter().enumerate() {
            for sfb in 0..left.max_sfb {
                let (lcb, rcb) = (left.sfb_cb[g][sfb], right.sfb_cb[g][sfb]);
                let lo = usize::from(swb.get(sfb).copied().unwrap_or(0));
                let hi = usize::from(swb.get(sfb.saturating_add(1)).copied().unwrap_or(0));
                let used = ms_used[g][sfb];
                for win in 0..glen {
                    let base = if left.short() {
                        w0.saturating_add(win).saturating_mul(128)
                    } else {
                        0
                    };
                    if rcb == INTENSITY_HCB || rcb == INTENSITY_HCB2 {
                        let mut sign: f64 = if rcb == INTENSITY_HCB { 1.0 } else { -1.0 };
                        if ms_present == 1 && used {
                            sign = -sign;
                        }
                        let scale = sign * 0.5f64.powf(0.25 * f64::from(right.sf[g][sfb]));
                        for k in lo..hi {
                            let at = base.saturating_add(k);
                            let l = f64::from(left.spec.get(at).copied().unwrap_or(0.0));
                            if let Some(r) = right.spec.get_mut(at) {
                                *r = (l * scale) as f32;
                            }
                        }
                    } else if used && lcb < NOISE_HCB && rcb < NOISE_HCB {
                        for k in lo..hi {
                            let at = base.saturating_add(k);
                            if let (Some(l), Some(r)) =
                                (left.spec.get(at).copied(), right.spec.get(at).copied())
                            {
                                left.spec[at] = l + r;
                                right.spec[at] = l - r;
                            }
                        }
                    }
                }
            }
            w0 = w0.saturating_add(glen);
        }
        Ok(())
    }
}

/// One Huffman tuple of codebook `cb` (1..=11) with its sign bits and escapes.
fn decode_tuple(
    b: &mut Bits<'_>,
    book: &Book,
    cb: u8,
    vals: &mut [i32; 4],
) -> Result<(), ReelError> {
    let idx = book.tree.decode(b)?;
    let side = book.side.max(1);
    let mut rest = idx;
    for i in (0..book.dim).rev() {
        let digit = rest.checked_rem(side).unwrap_or(0) as i32;
        rest = rest.checked_div(side).unwrap_or(0);
        if let Some(v) = vals.get_mut(i) {
            *v = digit.wrapping_add(book.offset);
        }
    }
    if book.offset == 0 {
        for v in vals.iter_mut().take(book.dim) {
            if *v != 0 && b.bit().ok_or(corrupt("aac-sign"))? {
                *v = v.wrapping_neg();
            }
        }
    }
    if cb == 11 {
        for v in vals.iter_mut().take(2) {
            if v.unsigned_abs() == 16 {
                let mut n = 0u32;
                while b.bit().ok_or(corrupt("aac-escape"))? {
                    n = n.saturating_add(1);
                    if n > 8 {
                        return Err(corrupt("aac-escape"));
                    }
                }
                let word = b.read(n.saturating_add(4)).ok_or(corrupt("aac-escape"))? as i32;
                let mag = (1i32 << n.saturating_add(4)).wrapping_add(word);
                *v = if *v < 0 { mag.wrapping_neg() } else { mag };
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every codebook is a complete prefix code: each code decodes back to its own symbol.
    #[test]
    fn huffman_books_decode_every_code() {
        let check = |entries: Vec<(u32, u8)>, tree: &Tree| {
            for (sym, (code, len)) in entries.iter().enumerate() {
                let mut w = super::super::bits::BitWriter::default();
                w.write(u32::from(*len), *code);
                w.write(8, 0);
                let bytes = w.into_bytes();
                let mut r = Bits::new(&bytes);
                assert_eq!(tree.decode(&mut r).unwrap(), sym);
            }
        };
        check(SF_CODES.to_vec(), sf_tree());
        let tables: [&[(u16, u8)]; 11] = [
            &CB1, &CB2, &CB3, &CB4, &CB5, &CB6, &CB7, &CB8, &CB9, &CB10, &CB11,
        ];
        for (i, t) in tables.iter().enumerate() {
            check(
                t.iter().map(|(c, l)| (u32::from(*c), *l)).collect(),
                &books()[i].tree,
            );
        }
    }
}
