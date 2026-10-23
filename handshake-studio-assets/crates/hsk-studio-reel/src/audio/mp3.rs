//! MPEG-1/2/2.5 audio Layers I, II and III decoder (ISO/IEC 11172-3, 13818-3), Studio-owned safe-Rust
//! adaptation of minimp3 (https://github.com/lieff/minimp3, minimp3.h sha256 57e437c5...cb3ad, CC0-1.0
//! public-domain dedication; tables and algorithm structure are minimp3's). Bitstream parsing here is
//! checked: the bit reader never reads outside its slice (bits past the end read as zero, as minimp3's
//! reader does inside its padded buffer), every table lookup is `get`, and the main-data reservoir is a
//! fixed 511 + 2304 byte buffer. Output is f32 (minimp3's MINIMP3_FLOAT_OUTPUT scaling).
#![deny(clippy::arithmetic_side_effects)]
use super::mp3_dsp::{
    SHORT_BLOCK_TYPE, antialias, change_sign, imdct_gr, intensity_stereo_band, midside_stereo,
    reorder, synth_granule,
};
use crate::error::ReelError;

const MAX_FREE_FORMAT_FRAME_SIZE: usize = 2304;
const MAX_BITRESERVOIR_BYTES: usize = 511;
const HDR_SIZE: usize = 4;
const MODE_MONO: u8 = 3;
const MODE_JOINT_STEREO: u8 = 1;
/// Largest magnitude the decoder emits (see `decode_frame`).
pub const OUTPUT_BOUND: f32 = 8.0;

/// MP3 decoder id (receipts carry it as `decoder_id`).
pub const MP3_DECODER_ID: &str = "mpeg-audio-minimp3-port";
pub const MP3_DECODER_VERSION: &str = "1";

// ----- header ---------------------------------------------------------------------------------------

fn h_is_mono(h: &[u8; 4]) -> bool {
    h[3] & 0xC0 == 0xC0
}
fn h_is_ms_stereo(h: &[u8; 4]) -> bool {
    h[3] & 0xE0 == 0x60
}
fn h_is_free_format(h: &[u8; 4]) -> bool {
    h[2] & 0xF0 == 0
}
fn h_is_crc(h: &[u8; 4]) -> bool {
    h[1] & 1 == 0
}
fn h_padding_bit(h: &[u8; 4]) -> bool {
    h[2] & 2 != 0
}
fn h_mpeg1(h: &[u8; 4]) -> bool {
    h[1] & 0x8 != 0
}
fn h_not_mpeg25(h: &[u8; 4]) -> bool {
    h[1] & 0x10 != 0
}
fn h_i_stereo(h: &[u8; 4]) -> bool {
    h[3] & 0x10 != 0
}
fn h_ms_stereo(h: &[u8; 4]) -> bool {
    h[3] & 0x20 != 0
}
fn h_stereo_mode(h: &[u8; 4]) -> u8 {
    (h[3] >> 6) & 3
}
fn h_stereo_mode_ext(h: &[u8; 4]) -> u8 {
    (h[3] >> 4) & 3
}
fn h_layer(h: &[u8; 4]) -> u8 {
    (h[1] >> 1) & 3
}
fn h_bitrate_index(h: &[u8; 4]) -> u8 {
    h[2] >> 4
}
fn h_sample_rate_index(h: &[u8; 4]) -> u8 {
    (h[2] >> 2) & 3
}
fn h_my_sample_rate(h: &[u8; 4]) -> usize {
    usize::from(h_sample_rate_index(h)).saturating_add(
        usize::from((h[1] >> 3) & 1)
            .saturating_add(usize::from((h[1] >> 4) & 1))
            .saturating_mul(3),
    )
}
fn h_frame_576(h: &[u8; 4]) -> bool {
    h[1] & 14 == 2
}
fn h_layer1(h: &[u8; 4]) -> bool {
    h[1] & 6 == 6
}

fn hdr_valid(h: &[u8; 4]) -> bool {
    h[0] == 0xFF
        && (h[1] & 0xF0 == 0xF0 || h[1] & 0xFE == 0xE2)
        && h_layer(h) != 0
        && h_bitrate_index(h) != 15
        && h_sample_rate_index(h) != 3
}

fn hdr_compare(h1: &[u8; 4], h2: &[u8; 4]) -> bool {
    hdr_valid(h2)
        && (h1[1] ^ h2[1]) & 0xFE == 0
        && (h1[2] ^ h2[2]) & 0x0C == 0
        && h_is_free_format(h1) == h_is_free_format(h2)
}

const HALFRATE: [[[u8; 15]; 3]; 2] = [
    [
        [0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 72, 80],
        [0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 72, 80],
        [0, 16, 24, 28, 32, 40, 48, 56, 64, 72, 80, 88, 96, 112, 128],
    ],
    [
        [0, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160],
        [
            0, 16, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192,
        ],
        [
            0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224,
        ],
    ],
];

pub(crate) fn hdr_bitrate_kbps(h: &[u8; 4]) -> u32 {
    let layer = usize::from(h_layer(h)).saturating_sub(1);
    HALFRATE
        .get(usize::from(h_mpeg1(h)))
        .and_then(|t| t.get(layer))
        .and_then(|t| t.get(usize::from(h_bitrate_index(h))))
        .map_or(0, |v| u32::from(*v).saturating_mul(2))
}

pub(crate) fn hdr_sample_rate_hz(h: &[u8; 4]) -> u32 {
    const HZ: [u32; 3] = [44_100, 48_000, 32_000];
    let base = HZ
        .get(usize::from(h_sample_rate_index(h)))
        .copied()
        .unwrap_or(0);
    base >> u32::from(!h_mpeg1(h)) >> u32::from(!h_not_mpeg25(h))
}

pub(crate) fn hdr_frame_samples(h: &[u8; 4]) -> usize {
    if h_layer1(h) {
        384
    } else {
        1152 >> usize::from(h_frame_576(h))
    }
}

pub(crate) fn hdr_channels(h: &[u8; 4]) -> u16 {
    if h_is_mono(h) { 1 } else { 2 }
}

fn hdr_frame_bytes(h: &[u8; 4], free_format_size: usize) -> usize {
    let rate = hdr_sample_rate_hz(h);
    let bytes = (hdr_frame_samples(h) as u32)
        .saturating_mul(hdr_bitrate_kbps(h))
        .saturating_mul(125)
        .checked_div(rate)
        .unwrap_or(0) as usize;
    let bytes = if h_layer1(h) { bytes & !3 } else { bytes };
    if bytes == 0 { free_format_size } else { bytes }
}

fn hdr_padding(h: &[u8; 4]) -> usize {
    if h_padding_bit(h) {
        if h_layer1(h) { 4 } else { 1 }
    } else {
        0
    }
}

fn header_at(buf: &[u8], at: usize) -> Option<[u8; 4]> {
    buf.get(at..at.checked_add(4)?)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
}

/// minimp3 `mp3d_match_frame`: `frame_bytes` must lead to up to 10 consistent headers.
fn match_frame(buf: &[u8], at: usize, frame_bytes: usize) -> bool {
    let Some(first) = header_at(buf, at) else {
        return false;
    };
    let mut i = at;
    for nmatch in 0..10 {
        let Some(h) = header_at(buf, i) else {
            return nmatch > 0;
        };
        i = i
            .saturating_add(hdr_frame_bytes(&h, frame_bytes))
            .saturating_add(hdr_padding(&h));
        if i.saturating_add(HDR_SIZE) > buf.len() {
            return nmatch > 0;
        }
        match header_at(buf, i) {
            Some(next) if hdr_compare(&first, &next) => {}
            _ => return false,
        }
    }
    true
}

/// minimp3 `mp3d_find_frame`: offset and total length (with padding) of the first frame in `buf`
/// confirmed by its successors; `free_format` carries the free-format frame size across calls.
pub(crate) fn find_frame(buf: &[u8], free_format: &mut usize) -> Option<(usize, usize)> {
    let n = buf.len();
    for i in 0..n.saturating_sub(HDR_SIZE) {
        let Some(h) = header_at(buf, i) else {
            break;
        };
        if !hdr_valid(&h) {
            continue;
        }
        let mut frame_bytes = hdr_frame_bytes(&h, *free_format);
        let mut frame_and_padding = frame_bytes.saturating_add(hdr_padding(&h));
        let mut k = HDR_SIZE;
        while frame_bytes == 0
            && k < MAX_FREE_FORMAT_FRAME_SIZE
            && i.saturating_add(k.saturating_mul(2)) < n.saturating_sub(HDR_SIZE)
        {
            if let Some(hk) = header_at(buf, i.saturating_add(k))
                && hdr_compare(&h, &hk)
            {
                let fb = k.saturating_sub(hdr_padding(&h));
                let nextfb = fb.saturating_add(hdr_padding(&hk));
                let next_at = i.saturating_add(k).saturating_add(nextfb);
                if next_at.saturating_add(HDR_SIZE) <= n
                    && header_at(buf, next_at).is_some_and(|hn| hdr_compare(&h, &hn))
                {
                    frame_and_padding = k;
                    frame_bytes = fb;
                    *free_format = fb;
                }
            }
            k = k.saturating_add(1);
        }
        if (frame_bytes != 0
            && i.saturating_add(frame_and_padding) <= n
            && match_frame(buf, i, frame_bytes))
            || (i == 0 && frame_and_padding == n)
        {
            return Some((i, frame_and_padding));
        }
        *free_format = 0;
    }
    None
}

/// Total bytes of the frame whose header is `h` (with padding), using the free-format size if needed.
pub(crate) fn frame_len(h: &[u8; 4], free_format: usize) -> usize {
    hdr_frame_bytes(h, free_format).saturating_add(hdr_padding(h))
}

pub(crate) fn header_is_valid(h: &[u8; 4]) -> bool {
    hdr_valid(h)
}

pub(crate) fn headers_compatible(a: &[u8; 4], b: &[u8; 4]) -> bool {
    hdr_compare(a, b)
}

// ----- bit reader -------------------------------------------------------------------------------------

struct Bs<'a> {
    buf: &'a [u8],
    pos: usize,
    limit: usize,
}

impl<'a> Bs<'a> {
    fn new(buf: &'a [u8], bytes: usize) -> Self {
        Self {
            buf,
            pos: 0,
            limit: bytes.min(buf.len()).saturating_mul(8),
        }
    }

    fn byte(&self, at: usize) -> u32 {
        u32::from(self.buf.get(at).copied().unwrap_or(0))
    }

    /// minimp3 `get_bits`: 0 (and the position moved past the limit) when the read would overrun.
    fn get(&mut self, n: u32) -> u32 {
        let start = self.pos;
        self.pos = self.pos.saturating_add(n as usize);
        if self.pos > self.limit || n == 0 {
            return 0;
        }
        let mut value: u32 = 0;
        for k in 0..n as usize {
            let bit = start.saturating_add(k);
            let b = (self.byte(bit >> 3) >> 7u32.wrapping_sub((bit & 7) as u32)) & 1;
            value = (value << 1) | b;
        }
        value
    }
}

// ----- Layer III side info and scale factors -------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct GrInfo {
    sfb_set: usize,
    sfb_kind: u8, // 0 long, 1 short, 2 mixed
    part_23_length: u16,
    big_values: u16,
    scalefac_compress: u16,
    global_gain: u8,
    block_type: u8,
    mixed_block_flag: u8,
    n_long_sfb: u8,
    n_short_sfb: u8,
    table_select: [u8; 3],
    region_count: [u8; 3],
    subblock_gain: [u8; 3],
    preflag: u8,
    scalefac_scale: u8,
    count1_table: u8,
    scfsi: u8,
}

#[rustfmt::skip]
const SCF_LONG: [[u8; 23]; 8] = [
    [6,6,6,6,6,6,8,10,12,14,16,20,24,28,32,38,46,52,60,68,58,54,0],
    [12,12,12,12,12,12,16,20,24,28,32,40,48,56,64,76,90,2,2,2,2,2,0],
    [6,6,6,6,6,6,8,10,12,14,16,20,24,28,32,38,46,52,60,68,58,54,0],
    [6,6,6,6,6,6,8,10,12,14,16,18,22,26,32,38,46,54,62,70,76,36,0],
    [6,6,6,6,6,6,8,10,12,14,16,20,24,28,32,38,46,52,60,68,58,54,0],
    [4,4,4,4,4,4,6,6,8,8,10,12,16,20,24,28,34,42,50,54,76,158,0],
    [4,4,4,4,4,4,6,6,6,8,10,12,16,18,22,28,34,40,46,54,54,192,0],
    [4,4,4,4,4,4,6,6,8,10,12,16,20,24,30,38,46,56,68,84,102,26,0],
];
#[rustfmt::skip]
const SCF_SHORT: [[u8; 40]; 8] = [
    [4,4,4,4,4,4,4,4,4,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,30,30,30,40,40,40,18,18,18,0],
    [8,8,8,8,8,8,8,8,8,12,12,12,16,16,16,20,20,20,24,24,24,28,28,28,36,36,36,2,2,2,2,2,2,2,2,2,26,26,26,0],
    [4,4,4,4,4,4,4,4,4,6,6,6,6,6,6,8,8,8,10,10,10,14,14,14,18,18,18,26,26,26,32,32,32,42,42,42,18,18,18,0],
    [4,4,4,4,4,4,4,4,4,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,32,32,32,44,44,44,12,12,12,0],
    [4,4,4,4,4,4,4,4,4,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,30,30,30,40,40,40,18,18,18,0],
    [4,4,4,4,4,4,4,4,4,4,4,4,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,22,22,22,30,30,30,56,56,56,0],
    [4,4,4,4,4,4,4,4,4,4,4,4,6,6,6,6,6,6,10,10,10,12,12,12,14,14,14,16,16,16,20,20,20,26,26,26,66,66,66,0],
    [4,4,4,4,4,4,4,4,4,4,4,4,6,6,6,8,8,8,12,12,12,16,16,16,20,20,20,26,26,26,34,34,34,42,42,42,12,12,12,0],
];
#[rustfmt::skip]
const SCF_MIXED: [[u8; 40]; 8] = [
    [6,6,6,6,6,6,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,30,30,30,40,40,40,18,18,18,0,0,0,0],
    [12,12,12,4,4,4,8,8,8,12,12,12,16,16,16,20,20,20,24,24,24,28,28,28,36,36,36,2,2,2,2,2,2,2,2,2,26,26,26,0],
    [6,6,6,6,6,6,6,6,6,6,6,6,8,8,8,10,10,10,14,14,14,18,18,18,26,26,26,32,32,32,42,42,42,18,18,18,0,0,0,0],
    [6,6,6,6,6,6,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,32,32,32,44,44,44,12,12,12,0,0,0,0],
    [6,6,6,6,6,6,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,24,24,24,30,30,30,40,40,40,18,18,18,0,0,0,0],
    [4,4,4,4,4,4,6,6,4,4,4,6,6,6,8,8,8,10,10,10,12,12,12,14,14,14,18,18,18,22,22,22,30,30,30,56,56,56,0,0],
    [4,4,4,4,4,4,6,6,4,4,4,6,6,6,6,6,6,10,10,10,12,12,12,14,14,14,16,16,16,20,20,20,26,26,26,66,66,66,0,0],
    [4,4,4,4,4,4,6,6,4,4,4,6,6,6,8,8,8,12,12,12,16,16,16,20,20,20,26,26,26,34,34,34,42,42,42,12,12,12,0,0],
];

impl GrInfo {
    fn sfbtab(&self) -> &'static [u8] {
        match self.sfb_kind {
            1 => SCF_SHORT.get(self.sfb_set).map_or(&[0u8][..], |t| &t[..]),
            2 => SCF_MIXED.get(self.sfb_set).map_or(&[0u8][..], |t| &t[..]),
            _ => SCF_LONG.get(self.sfb_set).map_or(&[0u8][..], |t| &t[..]),
        }
    }
}

/// minimp3 `L3_read_side_info`: `None` for an invalid side info; else main_data_begin.
fn read_side_info(bs: &mut Bs<'_>, gr: &mut [GrInfo; 4], hdr: &[u8; 4]) -> Option<usize> {
    let mut sr_idx = h_my_sample_rate(hdr);
    sr_idx = sr_idx.saturating_sub(usize::from(sr_idx != 0));
    let mut gr_count: usize = if h_is_mono(hdr) { 1 } else { 2 };
    let mut scfsi: u32 = 0;
    let main_data_begin;
    if h_mpeg1(hdr) {
        gr_count = gr_count.saturating_mul(2);
        main_data_begin = bs.get(9) as usize;
        scfsi = bs.get(7u32.saturating_add(gr_count as u32));
    } else {
        main_data_begin = (bs.get(8u32.saturating_add(gr_count as u32)) >> gr_count) as usize;
    }
    let mut part_23_sum: usize = 0;
    for g in gr.iter_mut().take(gr_count) {
        if h_is_mono(hdr) {
            scfsi <<= 4;
        }
        g.part_23_length = bs.get(12) as u16;
        part_23_sum = part_23_sum.saturating_add(usize::from(g.part_23_length));
        g.big_values = bs.get(9) as u16;
        if g.big_values > 288 {
            return None;
        }
        g.global_gain = bs.get(8) as u8;
        g.scalefac_compress = bs.get(if h_mpeg1(hdr) { 4 } else { 9 }) as u16;
        g.sfb_set = sr_idx;
        g.sfb_kind = 0;
        g.n_long_sfb = 22;
        g.n_short_sfb = 0;
        let tables;
        if bs.get(1) != 0 {
            g.block_type = bs.get(2) as u8;
            if g.block_type == 0 {
                return None;
            }
            g.mixed_block_flag = bs.get(1) as u8;
            g.region_count[0] = 7;
            g.region_count[1] = 255;
            if g.block_type == SHORT_BLOCK_TYPE {
                scfsi &= 0x0F0F;
                if g.mixed_block_flag == 0 {
                    g.region_count[0] = 8;
                    g.sfb_kind = 1;
                    g.n_long_sfb = 0;
                    g.n_short_sfb = 39;
                } else {
                    g.sfb_kind = 2;
                    g.n_long_sfb = if h_mpeg1(hdr) { 8 } else { 6 };
                    g.n_short_sfb = 30;
                }
            }
            tables = bs.get(10) << 5;
            g.subblock_gain = [bs.get(3) as u8, bs.get(3) as u8, bs.get(3) as u8];
        } else {
            g.block_type = 0;
            g.mixed_block_flag = 0;
            tables = bs.get(15);
            g.region_count[0] = bs.get(4) as u8;
            g.region_count[1] = bs.get(3) as u8;
            g.region_count[2] = 255;
        }
        g.table_select = [
            (tables >> 10) as u8,
            ((tables >> 5) & 31) as u8,
            (tables & 31) as u8,
        ];
        g.preflag = if h_mpeg1(hdr) {
            bs.get(1) as u8
        } else {
            u8::from(g.scalefac_compress >= 500)
        };
        g.scalefac_scale = bs.get(1) as u8;
        g.count1_table = bs.get(1) as u8;
        g.scfsi = ((scfsi >> 12) & 15) as u8;
        scfsi <<= 4;
    }
    if part_23_sum.saturating_add(bs.pos)
        > bs.limit.saturating_add(main_data_begin.saturating_mul(8))
    {
        return None;
    }
    Some(main_data_begin)
}

/// minimp3 `L3_read_scalefactors`.
fn read_scalefactors(
    scf: &mut [u8; 48],
    ist_pos: &mut [u8; 48],
    scf_size: &[u8; 4],
    scf_count: &[u8],
    bs: &mut Bs<'_>,
    mut scfsi: i32,
) {
    let mut at = 0usize;
    for i in 0..4 {
        let cnt = usize::from(scf_count.get(i).copied().unwrap_or(0));
        if cnt == 0 {
            break;
        }
        let end = at.saturating_add(cnt).min(45);
        if scfsi & 8 != 0 {
            if let (Some(dst), Some(src)) = (scf.get_mut(at..end), ist_pos.get(at..end)) {
                dst.copy_from_slice(src);
            }
        } else {
            let bits = u32::from(scf_size.get(i).copied().unwrap_or(0));
            if bits == 0 {
                for k in at..end {
                    scf[k] = 0;
                    ist_pos[k] = 0;
                }
            } else {
                let max_scf: i64 = if scfsi < 0 {
                    (1i64 << bits).saturating_sub(1)
                } else {
                    -1
                };
                for k in at..end {
                    let s = bs.get(bits);
                    ist_pos[k] = if i64::from(s) == max_scf {
                        0xFF
                    } else {
                        s as u8
                    };
                    scf[k] = s as u8;
                }
            }
        }
        at = end;
        scfsi = scfsi.wrapping_mul(2);
    }
    if let Some(tail) = scf.get_mut(at..at.saturating_add(3).min(48)) {
        tail.fill(0);
    }
}

/// minimp3 `L3_ldexp_q2`: y * 2^(-exp_q2 / 4).
fn ldexp_q2(mut y: f32, mut exp_q2: i32) -> f32 {
    #[allow(clippy::excessive_precision)]
    const EXPFRAC: [f32; 4] = [
        9.31322575e-10,
        7.83145814e-10,
        6.58544508e-10,
        5.53767716e-10,
    ];
    loop {
        let e = exp_q2.clamp(0, 120);
        let frac = EXPFRAC.get((e & 3) as usize).copied().unwrap_or(1.0);
        y *= frac * ((1u32 << 30) >> (e >> 2)) as f32;
        exp_q2 = exp_q2.saturating_sub(e);
        if exp_q2 <= 0 || e == 0 {
            return y;
        }
    }
}

#[rustfmt::skip]
const SCF_PARTITIONS: [[u8; 28]; 3] = [
    [6,5,5, 5,6,5,5,5,6,5, 7,3,11,10,0,0, 7, 7, 7,0, 6, 6,6,3, 8, 8,5,0],
    [8,9,6,12,6,9,9,9,6,9,12,6,15,18,0,0, 6,15,12,0, 6,12,9,6, 6,18,9,0],
    [9,9,6,12,9,9,9,9,9,9,12,6,18,18,0,0,12,12,12,0,12, 9,9,6,15,12,9,0],
];

/// minimp3 `L3_decode_scalefactors`: per-band gains into `scf` (40 bands).
fn decode_scalefactors(
    hdr: &[u8; 4],
    ist_pos: &mut [u8; 48],
    bs: &mut Bs<'_>,
    gr: &GrInfo,
    scf: &mut [f32; 48],
    ch: usize,
) {
    let part_row = usize::from(gr.n_short_sfb != 0).saturating_add(usize::from(gr.n_long_sfb == 0));
    let partitions = SCF_PARTITIONS
        .get(part_row)
        .map_or(&[0u8; 28][..], |r| &r[..]);
    let mut scf_size = [0u8; 4];
    let mut iscf = [0u8; 48];
    let scf_shift = u32::from(gr.scalefac_scale).saturating_add(1);
    let mut scfsi = i32::from(gr.scfsi);
    let mut offset = 0usize;
    if h_mpeg1(hdr) {
        const SCFC_DECODE: [u8; 16] = [0, 1, 2, 3, 12, 5, 6, 7, 9, 10, 11, 13, 14, 15, 18, 19];
        let part = SCFC_DECODE
            .get(usize::from(gr.scalefac_compress))
            .copied()
            .unwrap_or(0);
        scf_size = [part >> 2, part >> 2, part & 3, part & 3];
    } else {
        const MOD: [u8; 24] = [
            5, 5, 4, 4, 5, 5, 4, 1, 4, 3, 1, 1, 5, 6, 6, 1, 4, 4, 4, 1, 4, 3, 1, 1,
        ];
        let ist = usize::from(h_i_stereo(hdr) && ch != 0);
        let mut sfc = i32::from(gr.scalefac_compress >> ist);
        let mut k = ist.saturating_mul(12);
        while sfc >= 0 && k.saturating_add(4) <= MOD.len() {
            let mut modprod: i32 = 1;
            for i in (0..4).rev() {
                let m = i32::from(MOD.get(k.saturating_add(i)).copied().unwrap_or(1)).max(1);
                scf_size[i] = (sfc
                    .checked_div(modprod)
                    .unwrap_or(0)
                    .checked_rem(m)
                    .unwrap_or(0)) as u8;
                modprod = modprod.saturating_mul(m);
            }
            sfc = sfc.saturating_sub(modprod);
            k = k.saturating_add(4);
        }
        offset = k;
        scfsi = -16;
    }
    let counts = partitions.get(offset..).unwrap_or(&[]);
    read_scalefactors(&mut iscf, ist_pos, &scf_size, counts, bs, scfsi);

    let n_long = usize::from(gr.n_long_sfb);
    let n_short = usize::from(gr.n_short_sfb);
    if n_short != 0 {
        let sh = 3u32.saturating_sub(scf_shift);
        for i in (0..n_short).step_by(3) {
            for (j, gain) in gr.subblock_gain.iter().enumerate() {
                if let Some(v) = iscf.get_mut(n_long.saturating_add(i).saturating_add(j)) {
                    *v = v.wrapping_add(gain.wrapping_shl(sh));
                }
            }
        }
    } else if gr.preflag != 0 {
        const PREAMP: [u8; 10] = [1, 1, 1, 1, 2, 2, 3, 3, 3, 2];
        for (i, p) in PREAMP.iter().enumerate() {
            if let Some(v) = iscf.get_mut(11usize.saturating_add(i)) {
                *v = v.wrapping_add(*p);
            }
        }
    }
    // BITS_DEQUANTIZER_OUT = -1, MAX_SCFI = 44.
    let gain_exp = i32::from(gr.global_gain)
        .wrapping_sub(214)
        .wrapping_sub(if h_is_ms_stereo(hdr) { 2 } else { 0 });
    let gain = ldexp_q2((1u32 << 11) as f32, 44i32.saturating_sub(gain_exp));
    for i in 0..n_long.saturating_add(n_short).min(48) {
        scf[i] = ldexp_q2(gain, i32::from(iscf[i]) << scf_shift);
    }
}

include!("mp3_tables.rs");

fn pow_43(x: i32) -> f32 {
    if x < 129 {
        return POW43
            .get(usize::try_from(16i32.saturating_add(x)).unwrap_or(0))
            .copied()
            .unwrap_or(0.0);
    }
    let (mut x, mut mult) = (x, 256f32);
    if x < 1024 {
        mult = 16.0;
        x = x.saturating_mul(8);
    }
    let sign = x.saturating_mul(2) & 64;
    let frac = (x & 63).wrapping_sub(sign) as f32 / ((x & !63).saturating_add(sign)) as f32;
    let idx = usize::try_from(16i32.saturating_add(x.saturating_add(sign) >> 6)).unwrap_or(0);
    POW43.get(idx).copied().unwrap_or(0.0)
        * (1.0 + frac * ((4.0 / 3.0) + frac * (2.0 / 9.0)))
        * mult
}

/// The bit cache of minimp3's Huffman decoder over a byte slice; bytes past the end read as zero.
struct Cache<'a> {
    buf: &'a [u8],
    next: usize,
    cache: u32,
    sh: i32,
}

impl Cache<'_> {
    fn byte(&self, at: usize) -> u32 {
        u32::from(self.buf.get(at).copied().unwrap_or(0))
    }
    fn peek(&self, n: u32) -> u32 {
        if n == 0 || n > 32 {
            0
        } else {
            self.cache >> 32u32.wrapping_sub(n)
        }
    }
    fn flush(&mut self, n: u32) {
        self.cache = self.cache.checked_shl(n).unwrap_or(0);
        self.sh = self.sh.saturating_add(n as i32);
    }
    fn check(&mut self) {
        while self.sh >= 0 {
            self.cache |= self
                .byte(self.next)
                .checked_shl(self.sh as u32)
                .unwrap_or(0);
            self.next = self.next.saturating_add(1);
            self.sh = self.sh.saturating_sub(8);
        }
    }
    fn bspos(&self) -> i64 {
        (self.next as i64)
            .saturating_mul(8)
            .saturating_sub(24)
            .saturating_add(i64::from(self.sh))
    }
}

/// minimp3 `L3_huffman` into one channel's 576 coefficients.
fn huffman(dst: &mut [f32], bs: &mut Bs<'_>, gr: &GrInfo, scf: &[f32; 48], limit: usize) {
    let sfb = gr.sfbtab();
    let mut sfb_i = 0usize;
    let mut scf_i = 0usize;
    let p = bs.pos >> 3;
    let mut c = Cache {
        buf: bs.buf,
        next: p,
        cache: 0,
        sh: 0,
    };
    c.cache = ((c.byte(p) << 24)
        | (c.byte(p.saturating_add(1)) << 16)
        | (c.byte(p.saturating_add(2)) << 8)
        | c.byte(p.saturating_add(3)))
        << (bs.pos & 7) as u32;
    c.sh = ((bs.pos & 7) as i32).wrapping_sub(8);
    c.next = p.saturating_add(4);

    let mut d = 0usize;
    let mut big_val_cnt = i32::from(gr.big_values);
    let mut ireg = 0usize;
    let mut one = 0f32;
    let mut np: i32;
    let set = |dst: &mut [f32], at: usize, v: f32| {
        if let Some(x) = dst.get_mut(at) {
            *x = v;
        }
    };
    'big: while big_val_cnt > 0 {
        let Some(&tab_num) = gr.table_select.get(ireg) else {
            break;
        };
        let mut sfb_cnt = i32::from(gr.region_count.get(ireg).copied().unwrap_or(0));
        ireg = ireg.saturating_add(1);
        let base =
            usize::try_from(TABINDEX.get(usize::from(tab_num)).copied().unwrap_or(0)).unwrap_or(0);
        let linbits = u32::from(LINBITS.get(usize::from(tab_num)).copied().unwrap_or(0));
        loop {
            np = i32::from(sfb.get(sfb_i).copied().unwrap_or(0)) / 2;
            sfb_i = sfb_i.saturating_add(1);
            if np == 0 {
                break 'big;
            }
            let mut pairs = big_val_cnt.min(np);
            one = scf.get(scf_i).copied().unwrap_or(0.0);
            scf_i = scf_i.saturating_add(1);
            while pairs > 0 {
                let mut w = 5u32;
                let mut leaf = i32::from(
                    TABS.get(base.saturating_add(c.peek(w) as usize))
                        .copied()
                        .unwrap_or(0),
                );
                let mut guard: u32 = 0;
                while leaf < 0 {
                    c.flush(w);
                    w = (leaf & 7) as u32;
                    let idx = (c.peek(w) as i64).saturating_sub(i64::from(leaf >> 3));
                    leaf = usize::try_from(idx)
                        .ok()
                        .and_then(|i| TABS.get(base.saturating_add(i)))
                        .map_or(0, |v| i32::from(*v));
                    guard = guard.wrapping_add(1);
                    if guard > 32 {
                        leaf = 0;
                    }
                }
                c.flush((leaf >> 8) as u32);
                for _ in 0..2 {
                    let mut lsb = leaf & 0x0F;
                    if linbits != 0 && lsb == 15 {
                        lsb = lsb.saturating_add(c.peek(linbits) as i32);
                        c.flush(linbits);
                        c.check();
                        let sign = if (c.cache as i32) < 0 { -1.0 } else { 1.0 };
                        set(dst, d, one * pow_43(lsb) * sign);
                    } else {
                        let idx = 16i32
                            .saturating_add(lsb)
                            .saturating_sub(16i32.saturating_mul((c.cache >> 31) as i32));
                        set(
                            dst,
                            d,
                            POW43
                                .get(usize::try_from(idx).unwrap_or(0))
                                .copied()
                                .unwrap_or(0.0)
                                * one,
                        );
                    }
                    c.flush(u32::from(lsb != 0));
                    d = d.saturating_add(1);
                    leaf >>= 4;
                }
                c.check();
                pairs = pairs.saturating_sub(1);
            }
            big_val_cnt = big_val_cnt.saturating_sub(np);
            if big_val_cnt <= 0 {
                break;
            }
            sfb_cnt = sfb_cnt.saturating_sub(1);
            if sfb_cnt < 0 {
                break;
            }
        }
    }

    np = 1i32.saturating_sub(big_val_cnt);
    let table: &[u8] = if gr.count1_table != 0 { &TAB33 } else { &TAB32 };
    let mut quads: u32 = 0;
    loop {
        quads = quads.wrapping_add(1);
        if quads > 200 {
            break;
        }
        let mut leaf = i32::from(table.get(c.peek(4) as usize).copied().unwrap_or(0));
        if leaf & 8 == 0 {
            let idx = ((leaf >> 3) as u32)
                .saturating_add((c.cache << 4) >> 32u32.wrapping_sub((leaf & 3) as u32).min(31));
            leaf = i32::from(table.get(idx as usize).copied().unwrap_or(0));
        }
        c.flush((leaf & 7) as u32);
        if c.bspos() > limit as i64 {
            break;
        }
        // RELOAD_SCALEFACTOR
        np = np.saturating_sub(1);
        if np == 0 {
            np = i32::from(sfb.get(sfb_i).copied().unwrap_or(0)) / 2;
            sfb_i = sfb_i.saturating_add(1);
            if np == 0 {
                break;
            }
            one = scf.get(scf_i).copied().unwrap_or(0.0);
            scf_i = scf_i.saturating_add(1);
        }
        for s in 0..2usize {
            if leaf & (128 >> s) != 0 {
                set(
                    dst,
                    d.saturating_add(s),
                    if (c.cache as i32) < 0 { -one } else { one },
                );
                c.flush(1);
            }
        }
        np = np.saturating_sub(1);
        if np == 0 {
            np = i32::from(sfb.get(sfb_i).copied().unwrap_or(0)) / 2;
            sfb_i = sfb_i.saturating_add(1);
            if np == 0 {
                break;
            }
            one = scf.get(scf_i).copied().unwrap_or(0.0);
            scf_i = scf_i.saturating_add(1);
        }
        for s in 2..4usize {
            if leaf & (128 >> s) != 0 {
                set(
                    dst,
                    d.saturating_add(s),
                    if (c.cache as i32) < 0 { -one } else { one },
                );
                c.flush(1);
            }
        }
        c.check();
        d = d.saturating_add(4);
    }
    bs.pos = limit;
}

// ----- stereo ------------------------------------------------------------------------------------------

fn stereo_top_band(right: &[f32], sfb: &[u8], nbands: usize) -> [i32; 3] {
    let mut max_band = [-1i32; 3];
    let mut at = 0usize;
    for i in 0..nbands {
        let len = usize::from(sfb.get(i).copied().unwrap_or(0));
        let mut k = 0usize;
        while k < len {
            let a = right.get(at.saturating_add(k)).copied().unwrap_or(0.0);
            let b = right
                .get(at.saturating_add(k).saturating_add(1))
                .copied()
                .unwrap_or(0.0);
            if a != 0.0 || b != 0.0 {
                if let Some(m) = max_band.get_mut(i % 3) {
                    *m = i as i32;
                }
                break;
            }
            k = k.saturating_add(2);
        }
        at = at.saturating_add(len);
    }
    max_band
}

fn stereo_process(
    buf: &mut [f32; 1152],
    ist_pos: &[u8; 48],
    sfb: &[u8],
    hdr: &[u8; 4],
    max_band: [i32; 3],
    mpeg2_sh: u32,
) {
    #[allow(clippy::excessive_precision)]
    const PAN: [f32; 14] = [
        0.0, 1.0, 0.21132487, 0.78867513, 0.36602540, 0.63397460, 0.5, 0.5, 0.63397460, 0.36602540,
        0.78867513, 0.21132487, 1.0, 0.0,
    ];
    let max_pos: u32 = if h_mpeg1(hdr) { 7 } else { 64 };
    let mut at = 0usize;
    let mut i = 0usize;
    while let Some(&len) = sfb.get(i) {
        if len == 0 {
            break;
        }
        let len = usize::from(len);
        let ipos = u32::from(ist_pos.get(i).copied().unwrap_or(0));
        let top = max_band.get(i % 3).copied().unwrap_or(-1);
        if (i as i32) > top && ipos < max_pos {
            let s = if h_ms_stereo(hdr) {
                std::f32::consts::SQRT_2
            } else {
                1.0
            };
            let (kl, kr) = if h_mpeg1(hdr) {
                let p = ipos as usize;
                (
                    PAN.get(p.saturating_mul(2)).copied().unwrap_or(0.0),
                    PAN.get(p.saturating_mul(2).saturating_add(1))
                        .copied()
                        .unwrap_or(0.0),
                )
            } else {
                let kr = ldexp_q2(1.0, (((ipos.saturating_add(1)) >> 1) << mpeg2_sh) as i32);
                if ipos & 1 != 0 { (kr, 1.0) } else { (1.0, kr) }
            };
            intensity_stereo_band(buf, at, len, kl * s, kr * s);
        } else if h_ms_stereo(hdr) {
            midside_stereo(&mut buf[at.min(576)..], len);
        }
        at = at.saturating_add(len);
        i = i.saturating_add(1);
    }
}

fn intensity_stereo(
    buf: &mut [f32; 1152],
    ist_pos: &mut [u8; 48],
    gr: &[GrInfo; 4],
    gr_at: usize,
    hdr: &[u8; 4],
) {
    let g = gr.get(gr_at).copied().unwrap_or_default();
    let next = gr.get(gr_at.saturating_add(1)).copied().unwrap_or_default();
    let n_sfb = usize::from(g.n_long_sfb).saturating_add(usize::from(g.n_short_sfb));
    let max_blocks = if g.n_short_sfb != 0 { 3 } else { 1 };
    let mut max_band = stereo_top_band(&buf[576..], g.sfbtab(), n_sfb);
    if g.n_long_sfb != 0 {
        let m = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [m; 3];
    }
    for (i, top) in max_band.iter().enumerate().take(max_blocks) {
        let default_pos = if h_mpeg1(hdr) { 3 } else { 0 };
        let itop = n_sfb.saturating_sub(max_blocks).saturating_add(i);
        let prev = itop.saturating_sub(max_blocks);
        let v = if *top >= prev as i32 {
            default_pos
        } else {
            ist_pos.get(prev).copied().unwrap_or(0)
        };
        if let Some(slot) = ist_pos.get_mut(itop) {
            *slot = v;
        }
    }
    stereo_process(
        buf,
        ist_pos,
        g.sfbtab(),
        hdr,
        max_band,
        u32::from(next.scalefac_compress & 1),
    );
}

// ----- Layers I and II ---------------------------------------------------------------------------------

struct L12Scale {
    scf: [f32; 192],
    total_bands: usize,
    stereo_bands: usize,
    bitalloc: [u8; 64],
    scfcod: [u8; 64],
}

fn l12_read_scale_info(hdr: &[u8; 4], bs: &mut Bs<'_>, sci: &mut L12Scale) {
    // (tab_offset, code_tab_width, band_count)
    const L1: [(usize, u32, usize); 1] = [(76, 4, 32)];
    const L2M2: [(usize, u32, usize); 3] = [(60, 4, 4), (44, 3, 7), (44, 2, 19)];
    const L2M1: [(usize, u32, usize); 4] = [(0, 4, 3), (16, 4, 8), (32, 3, 12), (40, 2, 7)];
    const L2M1_LOW: [(usize, u32, usize); 2] = [(44, 4, 2), (44, 3, 10)];
    #[rustfmt::skip]
    const CODE_TAB: [u8; 92] = [
        0,17, 3, 4, 5,6,7, 8,9,10,11,12,13,14,15,16,
        0,17,18, 3,19,4,5, 6,7, 8, 9,10,11,12,13,16,
        0,17,18, 3,19,4,5,16,
        0,17,18,16,
        0,17,18,19, 4,5,6, 7,8, 9,10,11,12,13,14,15,
        0,17,18, 3,19,4,5, 6,7, 8, 9,10,11,12,13,14,
        0, 2, 3, 4, 5,6,7, 8,9,10,11,12,13,14,15,16,
    ];
    let mode = h_stereo_mode(hdr);
    let stereo_bands = if mode == MODE_MONO {
        0
    } else if mode == MODE_JOINT_STEREO {
        (usize::from(h_stereo_mode_ext(hdr)) << 2).saturating_add(4)
    } else {
        32
    };
    let (alloc, nbands): (&[(usize, u32, usize)], usize) = if h_layer1(hdr) {
        (&L1, 32)
    } else if !h_mpeg1(hdr) {
        (&L2M2, 30)
    } else {
        let sr = h_sample_rate_index(hdr);
        let mut kbps = hdr_bitrate_kbps(hdr) >> u32::from(mode != MODE_MONO);
        if kbps == 0 {
            kbps = 192;
        }
        if kbps < 56 {
            (&L2M1_LOW, if sr == 2 { 12 } else { 8 })
        } else if kbps >= 96 && sr != 1 {
            (&L2M1, 30)
        } else {
            (&L2M1, 27)
        }
    };
    sci.total_bands = nbands;
    sci.stereo_bands = stereo_bands.min(nbands);
    let (mut k, mut ba_bits, mut tab, mut a) = (0usize, 0u32, 0usize, 0usize);
    for i in 0..sci.total_bands {
        if i == k {
            let (offset, width, count) = alloc.get(a).copied().unwrap_or((0, 0, 32));
            k = k.saturating_add(count);
            ba_bits = width;
            tab = offset;
            a = a.saturating_add(1);
        }
        let code = |bs: &mut Bs<'_>| {
            CODE_TAB
                .get(tab.saturating_add(bs.get(ba_bits) as usize))
                .copied()
                .unwrap_or(0)
        };
        let mut ba = code(bs);
        let (l, r) = (i.wrapping_mul(2), i.wrapping_mul(2).wrapping_add(1));
        if let Some(slot) = sci.bitalloc.get_mut(l) {
            *slot = ba;
        }
        if i < sci.stereo_bands {
            ba = code(bs);
        }
        if let Some(slot) = sci.bitalloc.get_mut(r) {
            *slot = if sci.stereo_bands != 0 { ba } else { 0 };
        }
    }
    for i in 0..sci.total_bands.saturating_mul(2) {
        sci.scfcod[i] = if sci.bitalloc[i] != 0 {
            if h_layer1(hdr) { 2 } else { bs.get(2) as u8 }
        } else {
            6
        };
    }
    // L12_read_scalefactors
    let mut s_at = 0usize;
    for i in 0..sci.total_bands.saturating_mul(2) {
        let mut s = 0f32;
        let ba = usize::from(sci.bitalloc[i]);
        let mask = if ba != 0 {
            4 | ((19 >> sci.scfcod[i]) & 3)
        } else {
            0
        };
        let mut m = 4;
        while m != 0 {
            if mask & m != 0 {
                let b = bs.get(6) as usize;
                let deq = DEQ_L12
                    .get(ba.saturating_mul(3).saturating_sub(6).saturating_add(b % 3))
                    .copied()
                    .unwrap_or(0.0);
                s = deq * ((1u32 << 21) >> (b / 3)) as f32;
            }
            if let Some(dst) = sci.scf.get_mut(s_at) {
                *dst = s;
            }
            s_at = s_at.saturating_add(1);
            m >>= 1;
        }
    }
    for i in sci.stereo_bands..sci.total_bands {
        if let Some(slot) = sci.bitalloc.get_mut(i.wrapping_mul(2).wrapping_add(1)) {
            *slot = 0;
        }
    }
}

fn l12_dequantize_granule(
    grbuf: &mut [f32; 1152],
    at: usize,
    bs: &mut Bs<'_>,
    sci: &L12Scale,
    group: usize,
) -> usize {
    for j in 0..4usize {
        let base = at.saturating_add(group.saturating_mul(j));
        for i in 0..sci.total_bands.saturating_mul(2) {
            // Band i/2 of channel i%2: left at base + 18 * band, right 576 further.
            let dst = base
                .saturating_add((i >> 1).saturating_mul(18))
                .saturating_add((i & 1).saturating_mul(576));
            let ba = u32::from(sci.bitalloc[i]);
            if ba != 0 {
                if ba < 17 {
                    let half = (1i32 << ba.wrapping_sub(1)).wrapping_sub(1);
                    for k in 0..group {
                        if let Some(v) = grbuf.get_mut(dst.saturating_add(k)) {
                            *v = (bs.get(ba) as i32).wrapping_sub(half) as f32;
                        }
                    }
                } else {
                    let modv = (2u32 << ba.wrapping_sub(17)).wrapping_add(1);
                    let mut code = bs.get(modv.wrapping_add(2).wrapping_sub(modv >> 3));
                    for k in 0..group {
                        if let Some(v) = grbuf.get_mut(dst.saturating_add(k)) {
                            *v = (code.checked_rem(modv).unwrap_or(0) as i32)
                                .wrapping_sub((modv >> 1) as i32)
                                as f32;
                        }
                        code = code.checked_div(modv).unwrap_or(0);
                    }
                }
            }
        }
    }
    group.saturating_mul(4)
}

fn l12_apply_scf_384(sci: &L12Scale, scf_at: usize, grbuf: &mut [f32; 1152]) {
    let sb = sci.stereo_bands.saturating_mul(18);
    let n = sci
        .total_bands
        .saturating_sub(sci.stereo_bands)
        .saturating_mul(18);
    let (left, right) = grbuf.split_at_mut(576);
    if let (Some(src), Some(dst)) = (
        left.get(sb..sb.saturating_add(n)),
        right.get_mut(sb..sb.saturating_add(n)),
    ) {
        dst.copy_from_slice(src);
    }
    for i in 0..sci.total_bands {
        let s = scf_at.saturating_add(i.saturating_mul(6));
        let (s0, s3) = (
            sci.scf.get(s).copied().unwrap_or(0.0),
            sci.scf.get(s.saturating_add(3)).copied().unwrap_or(0.0),
        );
        for k in 0..12usize {
            let at = i.saturating_mul(18).saturating_add(k);
            if let Some(v) = left.get_mut(at) {
                *v *= s0;
            }
            if let Some(v) = right.get_mut(at) {
                *v *= s3;
            }
        }
    }
}

// ----- decoder state ----------------------------------------------------------------------------------

pub(crate) struct Mp3Decoder {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: usize,
    reserv_buf: [u8; MAX_BITRESERVOIR_BYTES],
    maindata: Vec<u8>,
}

impl Mp3Decoder {
    pub(crate) fn new() -> Self {
        Self {
            mdct_overlap: [[0.0; 288]; 2],
            qmf_state: [0.0; 960],
            reserv: 0,
            reserv_buf: [0; MAX_BITRESERVOIR_BYTES],
            maindata: vec![0; MAX_BITRESERVOIR_BYTES + MAX_FREE_FORMAT_FRAME_SIZE],
        }
    }

    /// Decode one complete frame (header included); appends `frames * channels` interleaved samples.
    /// Returns 0 frames (and appends nothing) when the bit reservoir does not yet hold the main data this
    /// frame refers to (stream start or after a cut), as minimp3 does.
    pub(crate) fn decode_frame(
        &mut self,
        frame: &[u8],
        out: &mut Vec<f32>,
    ) -> Result<usize, ReelError> {
        let hdr = header_at(frame, 0)
            .filter(hdr_valid)
            .ok_or(ReelError::Corrupt {
                what: "mpeg-audio-header",
            })?;
        if frame.len() > MAX_FREE_FORMAT_FRAME_SIZE.saturating_add(HDR_SIZE) {
            return Err(ReelError::LimitExceeded {
                what: "mpeg-audio-frame-size",
            });
        }
        let nch = usize::from(hdr_channels(&hdr));
        let samples = hdr_frame_samples(&hdr);
        let body = frame.get(HDR_SIZE..).unwrap_or(&[]);
        let mut bs = Bs::new(body, body.len());
        if h_is_crc(&hdr) {
            bs.get(16);
        }
        let mut pcm = vec![0f32; samples.saturating_mul(nch)];
        let mut grbuf = [0f32; 1152];
        let layer = 4u8.saturating_sub(h_layer(&hdr));
        let produced = if layer == 3 {
            let mut gr = [GrInfo::default(); 4];
            let Some(main_data_begin) = read_side_info(&mut bs, &mut gr, &hdr) else {
                self.reserv = 0;
                return Err(ReelError::Corrupt {
                    what: "mp3-side-info",
                });
            };
            if bs.pos > bs.limit {
                self.reserv = 0;
                return Err(ReelError::Corrupt {
                    what: "mp3-side-info",
                });
            }
            // L3_restore_reservoir
            let side_bytes = bs.pos >> 3;
            let frame_bytes = body.len().saturating_sub(side_bytes);
            let have = self.reserv.min(main_data_begin);
            let from = self.reserv.saturating_sub(main_data_begin);
            let ok = self.reserv >= main_data_begin;
            self.maindata.fill(0);
            if let (Some(dst), Some(src)) = (
                self.maindata.get_mut(..have),
                self.reserv_buf.get(from..from.saturating_add(have)),
            ) {
                dst.copy_from_slice(src);
            }
            if let (Some(dst), Some(src)) = (
                self.maindata
                    .get_mut(have..have.saturating_add(frame_bytes)),
                body.get(side_bytes..side_bytes.saturating_add(frame_bytes)),
            ) {
                dst.copy_from_slice(src);
            }
            let total = have.saturating_add(frame_bytes);
            let maindata = std::mem::take(&mut self.maindata);
            let mut mbs = Bs::new(&maindata, total);
            let granules: usize = if h_mpeg1(&hdr) { 2 } else { 1 };
            // Scale factors of granule 0 live on in `ist_pos` for granule 1's scfsi copies.
            let mut ist_pos = [[0u8; 48]; 2];
            if ok {
                for igr in 0..granules {
                    grbuf.fill(0.0);
                    self.l3_decode(
                        &mut mbs,
                        &gr,
                        igr.saturating_mul(nch),
                        nch,
                        &hdr,
                        &mut grbuf,
                        &mut ist_pos,
                    );
                    let at = igr.saturating_mul(576).saturating_mul(nch);
                    let slot = pcm.get_mut(at..).unwrap_or(&mut []);
                    synth_granule(&mut self.qmf_state, &mut grbuf, 18, nch, slot);
                }
            }
            // L3_save_reservoir
            let mut pos = mbs.pos.div_ceil(8);
            let mut remains = (mbs.limit / 8).saturating_sub(pos);
            if remains > MAX_BITRESERVOIR_BYTES {
                pos = pos.saturating_add(remains.saturating_sub(MAX_BITRESERVOIR_BYTES));
                remains = MAX_BITRESERVOIR_BYTES;
            }
            if let (Some(dst), Some(src)) = (
                self.reserv_buf.get_mut(..remains),
                maindata.get(pos..pos.saturating_add(remains)),
            ) {
                dst.copy_from_slice(src);
                self.reserv = remains;
            } else {
                self.reserv = 0;
            }
            self.maindata = maindata;
            if ok { samples } else { 0 }
        } else {
            let mut sci = L12Scale {
                scf: [0.0; 192],
                total_bands: 0,
                stereo_bands: 0,
                bitalloc: [0; 64],
                scfcod: [0; 64],
            };
            l12_read_scale_info(&hdr, &mut bs, &mut sci);
            let group = usize::from(layer | 1);
            let mut i = 0usize;
            let mut pcm_at = 0usize;
            for igr in 0..3usize {
                i = i.saturating_add(l12_dequantize_granule(&mut grbuf, i, &mut bs, &sci, group));
                if i == 12 {
                    i = 0;
                    l12_apply_scf_384(&sci, igr, &mut grbuf);
                    let slot = pcm.get_mut(pcm_at..).unwrap_or(&mut []);
                    synth_granule(&mut self.qmf_state, &mut grbuf, 12, nch, slot);
                    grbuf.fill(0.0);
                    pcm_at = pcm_at.saturating_add(384usize.saturating_mul(nch));
                }
                if bs.pos > bs.limit {
                    return Err(ReelError::Corrupt {
                        what: "mpeg-audio-layer-1-2-frame",
                    });
                }
            }
            samples
        };
        let values = produced.saturating_mul(nch).min(pcm.len());
        out.try_reserve(values)
            .map_err(|_| ReelError::LimitExceeded {
                what: "audio-alloc",
            })?;
        // A corrupt frame can scale coefficients without bound (global gain and scale factors come
        // from the bitstream): keep the output finite and within OUTPUT_BOUND (+18 dB over full scale,
        // far above any legitimate decode, which stays near [-1, 1]).
        out.extend(pcm.iter().take(values).map(|v| {
            if v.is_finite() {
                v.clamp(-OUTPUT_BOUND, OUTPUT_BOUND)
            } else {
                0.0
            }
        }));
        Ok(produced)
    }

    #[allow(clippy::too_many_arguments)]
    fn l3_decode(
        &mut self,
        bs: &mut Bs<'_>,
        gr: &[GrInfo; 4],
        gr_at: usize,
        nch: usize,
        hdr: &[u8; 4],
        grbuf: &mut [f32; 1152],
        ist_pos: &mut [[u8; 48]; 2],
    ) {
        let mut scf = [0f32; 48];
        for (ch, ist) in ist_pos.iter_mut().enumerate().take(nch) {
            let g = gr
                .get(gr_at.saturating_add(ch))
                .copied()
                .unwrap_or_default();
            let limit = bs.pos.saturating_add(usize::from(g.part_23_length));
            decode_scalefactors(hdr, ist, bs, &g, &mut scf, ch);
            let dst = grbuf.chunks_exact_mut(576).nth(ch).unwrap_or(&mut []);
            huffman(dst, bs, &g, &scf, limit);
        }
        if h_i_stereo(hdr) {
            intensity_stereo(grbuf, &mut ist_pos[1], gr, gr_at, hdr);
        } else if h_is_ms_stereo(hdr) {
            midside_stereo(grbuf, 576);
        }
        for ch in 0..nch {
            let g = gr
                .get(gr_at.saturating_add(ch))
                .copied()
                .unwrap_or_default();
            let mut aa_bands = 31usize;
            let n_long_bands = usize::from(if g.mixed_block_flag != 0 { 2u8 } else { 0 })
                << usize::from(h_my_sample_rate(hdr) == 2);
            let chan = grbuf.chunks_exact_mut(576).nth(ch).unwrap_or(&mut []);
            if g.n_short_sfb != 0 {
                aa_bands = n_long_bands.saturating_sub(1);
                let sfb = g.sfbtab().get(usize::from(g.n_long_sfb)..).unwrap_or(&[]);
                reorder(&mut chan[n_long_bands.saturating_mul(18).min(576)..], sfb);
            }
            antialias(chan, aa_bands);
            imdct_gr(chan, &mut self.mdct_overlap[ch], g.block_type, n_long_bands);
            change_sign(chan);
        }
    }
}
