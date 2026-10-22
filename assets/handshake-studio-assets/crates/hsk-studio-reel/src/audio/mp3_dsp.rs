//! MPEG audio synthesis stages: Layer III antialias, IMDCT (36 and 12 point), the polyphase DCT-II and
//! the windowed synthesis filterbank. Safe-Rust adaptation of the scalar paths of minimp3
//! (https://github.com/lieff/minimp3, minimp3.h, CC0-1.0 public-domain dedication; the constants below
//! are minimp3's). Every loop here runs over fixed-size buffers with compile-time bounds (no length or
//! index comes from the bitstream), so integer index arithmetic cannot overflow; float arithmetic is the
//! codec's own.
#![allow(clippy::arithmetic_side_effects, clippy::excessive_precision)]

pub(crate) const SHORT_BLOCK_TYPE: u8 = 2;
pub(crate) const STOP_BLOCK_TYPE: u8 = 3;

pub(crate) fn midside_stereo(left: &mut [f32], n: usize) {
    if left.len() < 576 {
        return;
    }
    let (l, r) = left.split_at_mut(576);
    for i in 0..n.min(r.len()) {
        let a = l[i];
        let b = r[i];
        l[i] = a + b;
        r[i] = a - b;
    }
}

pub(crate) fn intensity_stereo_band(buf: &mut [f32], at: usize, n: usize, kl: f32, kr: f32) {
    for i in at..(at + n).min(576) {
        buf[i + 576] = buf[i] * kr;
        buf[i] *= kl;
    }
}

pub(crate) fn reorder(grbuf: &mut [f32], sfb: &[u8]) {
    let mut scratch = [0f32; 576];
    let mut dst = 0usize;
    let mut src = 0usize;
    let mut k = 0usize;
    while let Some(&len) = sfb.get(k) {
        if len == 0 {
            break;
        }
        let len = usize::from(len);
        for _ in 0..len {
            if src + 2 * len >= grbuf.len() || dst + 3 > scratch.len() {
                return;
            }
            scratch[dst] = grbuf[src];
            scratch[dst + 1] = grbuf[src + len];
            scratch[dst + 2] = grbuf[src + 2 * len];
            dst += 3;
            src += 1;
        }
        src += 2 * len;
        k += 3;
    }
    grbuf[..dst].copy_from_slice(&scratch[..dst]);
}

const AA: [[f32; 8]; 2] = [
    [
        0.85749293, 0.88174200, 0.94962865, 0.98331459, 0.99551782, 0.99916056, 0.99989920,
        0.99999316,
    ],
    [
        0.51449576, 0.47173197, 0.31337745, 0.18191320, 0.09457419, 0.04096558, 0.01419856,
        0.00369997,
    ],
];

pub(crate) fn antialias(grbuf: &mut [f32], nbands: usize) {
    for band in 0..nbands.min(31) {
        let g = band * 18;
        for i in 0..8 {
            let u = grbuf[g + 18 + i];
            let d = grbuf[g + 17 - i];
            grbuf[g + 18 + i] = u * AA[0][i] - d * AA[1][i];
            grbuf[g + 17 - i] = u * AA[1][i] + d * AA[0][i];
        }
    }
}

fn dct3_9(y: &mut [f32; 9]) {
    let (mut s0, mut s2, mut s4, mut s6, mut s8) = (y[0], y[2], y[4], y[6], y[8]);
    let mut t0 = s0 + s6 * 0.5;
    s0 -= s6;
    let mut t4 = (s4 + s2) * 0.93969262;
    let mut t2 = (s8 + s2) * 0.76604444;
    s6 = (s4 - s8) * 0.17364818;
    s4 += s8 - s2;

    s2 = s0 - s4 * 0.5;
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;

    let (mut s1, mut s3, mut s5, mut s7) = (y[1], y[3], y[5], y[7]);

    s3 *= 0.86602540;
    t0 = (s5 + s1) * 0.98480775;
    t4 = (s5 - s7) * 0.34202014;
    t2 = (s1 + s7) * 0.64278761;
    s1 = (s1 - s5 - s7) * 0.86602540;

    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;

    y[0] = s4 - s7;
    y[1] = s2 + s1;
    y[2] = s0 - s3;
    y[3] = s8 + s5;
    y[5] = s8 - s5;
    y[6] = s0 + s3;
    y[7] = s2 - s1;
    y[8] = s4 + s7;
}

const TWID9: [f32; 18] = [
    0.73727734, 0.79335334, 0.84339145, 0.88701083, 0.92387953, 0.95371695, 0.97629601, 0.99144486,
    0.99904822, 0.67559021, 0.60876143, 0.53729961, 0.46174861, 0.38268343, 0.30070580, 0.21643961,
    0.13052619, 0.04361938,
];

fn imdct36(grbuf: &mut [f32], overlap: &mut [f32], window: &[f32; 18], nbands: usize) {
    for j in 0..nbands {
        let g = j * 18;
        let o = j * 9;
        if g + 18 > grbuf.len() || o + 9 > overlap.len() {
            return;
        }
        let mut co = [0f32; 9];
        let mut si = [0f32; 9];
        co[0] = -grbuf[g];
        si[0] = grbuf[g + 17];
        for i in 0..4 {
            si[8 - 2 * i] = grbuf[g + 4 * i + 1] - grbuf[g + 4 * i + 2];
            co[1 + 2 * i] = grbuf[g + 4 * i + 1] + grbuf[g + 4 * i + 2];
            si[7 - 2 * i] = grbuf[g + 4 * i + 4] - grbuf[g + 4 * i + 3];
            co[2 + 2 * i] = -(grbuf[g + 4 * i + 3] + grbuf[g + 4 * i + 4]);
        }
        dct3_9(&mut co);
        dct3_9(&mut si);
        si[1] = -si[1];
        si[3] = -si[3];
        si[5] = -si[5];
        si[7] = -si[7];
        for i in 0..9 {
            let ovl = overlap[o + i];
            let sum = co[i] * TWID9[9 + i] + si[i] * TWID9[i];
            overlap[o + i] = co[i] * TWID9[i] - si[i] * TWID9[9 + i];
            grbuf[g + i] = ovl * window[i] - sum * window[9 + i];
            grbuf[g + 17 - i] = ovl * window[9 + i] + sum * window[i];
        }
    }
}

fn idct3(x0: f32, x1: f32, x2: f32) -> [f32; 3] {
    let m1 = x1 * 0.86602540;
    let a1 = x0 - x2 * 0.5;
    [a1 + m1, x0 + x2, a1 - m1]
}

const TWID3: [f32; 6] = [
    0.79335334, 0.92387953, 0.99144486, 0.60876143, 0.38268343, 0.13052619,
];

/// `x` is read with stride 3 starting at `x[0]`; writes 6 values to `dst` and updates `overlap[0..3]`.
fn imdct12(x: &[f32], dst: &mut [f32; 6], overlap: &mut [f32]) {
    let co = idct3(-x[0], x[6] + x[3], x[12] + x[9]);
    let mut si = idct3(x[15], x[12] - x[9], x[6] - x[3]);
    si[1] = -si[1];
    for i in 0..3 {
        let ovl = overlap[i];
        let sum = co[i] * TWID3[3 + i] + si[i] * TWID3[i];
        overlap[i] = co[i] * TWID3[i] - si[i] * TWID3[3 + i];
        dst[i] = ovl * TWID3[2 - i] - sum * TWID3[5 - i];
        dst[5 - i] = ovl * TWID3[5 - i] + sum * TWID3[2 - i];
    }
}

fn imdct_short(grbuf: &mut [f32], overlap: &mut [f32], nbands: usize) {
    for band in 0..nbands {
        let g = band * 18;
        let o = band * 9;
        if g + 18 > grbuf.len() || o + 9 > overlap.len() {
            return;
        }
        let mut tmp = [0f32; 18];
        tmp.copy_from_slice(&grbuf[g..g + 18]);
        grbuf[g..g + 6].copy_from_slice(&overlap[o..o + 6]);
        let ov = &mut overlap[o..o + 9];
        let mut out = [0f32; 6];
        {
            let (_, tail) = ov.split_at_mut(6);
            imdct12(&tmp[0..], &mut out, tail);
        }
        grbuf[g + 6..g + 12].copy_from_slice(&out);
        {
            let (_, tail) = ov.split_at_mut(6);
            imdct12(&tmp[1..], &mut out, tail);
        }
        grbuf[g + 12..g + 18].copy_from_slice(&out);
        {
            let (_, tail) = ov.split_at_mut(6);
            imdct12(&tmp[2..], &mut out, tail);
        }
        ov[..6].copy_from_slice(&out);
    }
}

pub(crate) fn change_sign(grbuf: &mut [f32]) {
    let mut g = 18usize;
    for _ in (0..32).step_by(2) {
        for i in (1..18).step_by(2) {
            if let Some(v) = grbuf.get_mut(g + i) {
                *v = -*v;
            }
        }
        g += 36;
    }
}

const MDCT_WINDOW: [[f32; 18]; 2] = [
    [
        0.99904822, 0.99144486, 0.97629601, 0.95371695, 0.92387953, 0.88701083, 0.84339145,
        0.79335334, 0.73727734, 0.04361938, 0.13052619, 0.21643961, 0.30070580, 0.38268343,
        0.46174861, 0.53729961, 0.60876143, 0.67559021,
    ],
    [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.99144486, 0.92387953, 0.79335334, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.13052619, 0.38268343, 0.60876143,
    ],
];

pub(crate) fn imdct_gr(
    grbuf: &mut [f32],
    overlap: &mut [f32],
    block_type: u8,
    n_long_bands: usize,
) {
    let n_long = n_long_bands.min(32);
    if n_long > 0 {
        imdct36(grbuf, overlap, &MDCT_WINDOW[0], n_long);
    }
    let (g, o) = (&mut grbuf[18 * n_long..], &mut overlap[9 * n_long..]);
    if block_type == SHORT_BLOCK_TYPE {
        imdct_short(g, o, 32 - n_long);
    } else {
        let w = &MDCT_WINDOW[usize::from(block_type == STOP_BLOCK_TYPE)];
        imdct36(g, o, w, 32 - n_long);
    }
}

const SEC: [f32; 24] = [
    10.19000816,
    0.50060302,
    0.50241929,
    3.40760851,
    0.50547093,
    0.52249861,
    2.05778098,
    0.51544732,
    0.56694406,
    1.48416460,
    0.53104258,
    0.64682180,
    1.16943991,
    0.55310392,
    0.78815460,
    0.97256821,
    0.58293498,
    1.06067765,
    0.83934963,
    0.62250412,
    1.72244716,
    0.74453628,
    0.67480832,
    5.10114861,
];

/// Polyphase DCT-II over `n` columns of one channel's 576 values (stride 18).
fn dct_ii(grbuf: &mut [f32], n: usize) {
    for k in 0..n.min(18) {
        let mut t = [[0f32; 8]; 4];
        for i in 0..8 {
            let x0 = grbuf[k + i * 18];
            let x1 = grbuf[k + (15 - i) * 18];
            let x2 = grbuf[k + (16 + i) * 18];
            let x3 = grbuf[k + (31 - i) * 18];
            let t0 = x0 + x3;
            let t1 = x1 + x2;
            let t2 = (x1 - x2) * SEC[3 * i];
            let t3 = (x0 - x3) * SEC[3 * i + 1];
            t[0][i] = t0 + t1;
            t[1][i] = (t0 - t1) * SEC[3 * i + 2];
            t[2][i] = t3 + t2;
            t[3][i] = (t3 - t2) * SEC[3 * i + 2];
        }
        for x in &mut t {
            let (mut x0, mut x1, mut x2, mut x3, mut x4, mut x5, mut x6, mut x7) =
                (x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]);
            let mut xt = x0 - x7;
            x0 += x7;
            x7 = x1 - x6;
            x1 += x6;
            x6 = x2 - x5;
            x2 += x5;
            x5 = x3 - x4;
            x3 += x4;
            x4 = x0 - x3;
            x0 += x3;
            x3 = x1 - x2;
            x1 += x2;
            x[0] = x0 + x1;
            x[4] = (x0 - x1) * 0.70710677;
            x5 += x6;
            x6 = (x6 + x7) * 0.70710677;
            x7 += xt;
            x3 = (x3 + x4) * 0.70710677;
            x5 -= x7 * 0.198912367;
            x7 += x5 * 0.382683432;
            x5 -= x7 * 0.198912367;
            x0 = xt - x6;
            xt += x6;
            x[1] = (xt + x7) * 0.50979561;
            x[2] = (x4 + x3) * 0.54119611;
            x[3] = (x0 - x5) * 0.60134488;
            x[5] = (x0 + x5) * 0.89997619;
            x[6] = (x4 - x3) * 1.30656302;
            x[7] = (xt - x7) * 2.56291556;
        }
        let mut y = k;
        for i in 0..7 {
            grbuf[y] = t[0][i];
            grbuf[y + 18] = t[2][i] + t[3][i] + t[3][i + 1];
            grbuf[y + 2 * 18] = t[1][i] + t[1][i + 1];
            grbuf[y + 3 * 18] = t[2][i + 1] + t[3][i] + t[3][i + 1];
            y += 4 * 18;
        }
        grbuf[y] = t[0][7];
        grbuf[y + 18] = t[2][7] + t[3][7];
        grbuf[y + 2 * 18] = t[1][7];
        grbuf[y + 3 * 18] = t[3][7];
    }
}

#[rustfmt::skip]
const WIN: [f32; 240] = [
    -1.,26.,-31.,208.,218.,401.,-519.,2063.,2000.,4788.,-5517.,7134.,5959.,35640.,-39336.,74992.,
    -1.,24.,-35.,202.,222.,347.,-581.,2080.,1952.,4425.,-5879.,7640.,5288.,33791.,-41176.,74856.,
    -1.,21.,-38.,196.,225.,294.,-645.,2087.,1893.,4063.,-6237.,8092.,4561.,31947.,-43006.,74630.,
    -1.,19.,-41.,190.,227.,244.,-711.,2085.,1822.,3705.,-6589.,8492.,3776.,30112.,-44821.,74313.,
    -1.,17.,-45.,183.,228.,197.,-779.,2075.,1739.,3351.,-6935.,8840.,2935.,28289.,-46617.,73908.,
    -1.,16.,-49.,176.,228.,153.,-848.,2057.,1644.,3004.,-7271.,9139.,2037.,26482.,-48390.,73415.,
    -2.,14.,-53.,169.,227.,111.,-919.,2032.,1535.,2663.,-7597.,9389.,1082.,24694.,-50137.,72835.,
    -2.,13.,-58.,161.,224.,72.,-991.,2001.,1414.,2330.,-7910.,9592.,70.,22929.,-51853.,72169.,
    -2.,11.,-63.,154.,221.,36.,-1064.,1962.,1280.,2006.,-8209.,9750.,-998.,21189.,-53534.,71420.,
    -2.,10.,-68.,147.,215.,2.,-1137.,1919.,1131.,1692.,-8491.,9863.,-2122.,19478.,-55178.,70590.,
    -3.,9.,-73.,139.,208.,-29.,-1210.,1870.,970.,1388.,-8755.,9935.,-3300.,17799.,-56778.,69679.,
    -3.,8.,-79.,132.,200.,-57.,-1283.,1817.,794.,1095.,-8998.,9966.,-4533.,16155.,-58333.,68692.,
    -4.,7.,-85.,125.,189.,-83.,-1356.,1759.,605.,814.,-9219.,9959.,-5818.,14548.,-59838.,67629.,
    -4.,7.,-91.,117.,177.,-106.,-1428.,1698.,402.,545.,-9416.,9916.,-7154.,12980.,-61289.,66494.,
    -5.,6.,-97.,111.,163.,-127.,-1498.,1634.,185.,288.,-9585.,9838.,-8540.,11455.,-62684.,65290.,
];

const SCALE: f32 = 1.0 / 32768.0;

/// Output sample `pcm[at]`, if inside the frame buffer.
fn put(pcm: &mut [f32], at: usize, v: f32) {
    if let Some(dst) = pcm.get_mut(at) {
        *dst = v * SCALE;
    }
}

fn synth_pair(pcm: &mut [f32], at: usize, nch: usize, lins: &[f32], z: usize) {
    let zz = |k: usize| lins[z + k * 64];
    let mut a = (zz(14) - zz(0)) * 29.0;
    a += (zz(1) + zz(13)) * 213.0;
    a += (zz(12) - zz(2)) * 459.0;
    a += (zz(3) + zz(11)) * 2037.0;
    a += (zz(10) - zz(4)) * 5153.0;
    a += (zz(5) + zz(9)) * 6574.0;
    a += (zz(8) - zz(6)) * 37489.0;
    a += zz(7) * 75038.0;
    put(pcm, at, a);
    let zz = |k: usize| lins[z + 2 + k * 64];
    let mut a = zz(14) * 104.0;
    a += zz(12) * 1567.0;
    a += zz(10) * 9727.0;
    a += zz(8) * 64019.0;
    a += zz(6) * -9975.0;
    a += zz(4) * -45.0;
    a += zz(2) * 146.0;
    a += zz(0) * -5.0;
    put(pcm, at + 16 * nch, a);
}

/// One synthesis step for column pair `xl_at` (offset into the 2 x 576 granule buffer), writing 64
/// samples per channel at `pcm_at`, with the window history at `lins[l_at..]`.
fn synth(
    grbuf: &[f32],
    xl_at: usize,
    pcm: &mut [f32],
    pcm_at: usize,
    nch: usize,
    lins: &mut [f32],
    l_at: usize,
) {
    let xr_at = xl_at + 576 * (nch - 1);
    let xl = |k: usize| grbuf[xl_at + k];
    let xr = |k: usize| grbuf[xr_at + k];
    let dstl = pcm_at;
    let dstr = pcm_at + (nch - 1);
    let zlin = l_at + 15 * 64;

    lins[zlin + 4 * 15] = xl(18 * 16);
    lins[zlin + 4 * 15 + 1] = xr(18 * 16);
    lins[zlin + 4 * 15 + 2] = xl(0);
    lins[zlin + 4 * 15 + 3] = xr(0);

    lins[zlin + 4 * 31] = xl(1 + 18 * 16);
    lins[zlin + 4 * 31 + 1] = xr(1 + 18 * 16);
    lins[zlin + 4 * 31 + 2] = xl(1);
    lins[zlin + 4 * 31 + 3] = xr(1);

    synth_pair(pcm, dstr, nch, lins, l_at + 4 * 15 + 1);
    synth_pair(pcm, dstr + 32 * nch, nch, lins, l_at + 4 * 15 + 64 + 1);
    synth_pair(pcm, dstl, nch, lins, l_at + 4 * 15);
    synth_pair(pcm, dstl + 32 * nch, nch, lins, l_at + 4 * 15 + 64);

    let mut w = 0usize;
    for i in (0..15usize).rev() {
        let mut a = [0f32; 4];
        let mut b = [0f32; 4];
        lins[zlin + 4 * i] = xl(18 * (31 - i));
        lins[zlin + 4 * i + 1] = xr(18 * (31 - i));
        lins[zlin + 4 * i + 2] = xl(1 + 18 * (31 - i));
        lins[zlin + 4 * i + 3] = xr(1 + 18 * (31 - i));
        lins[zlin + 4 * (i + 16)] = xl(1 + 18 * (1 + i));
        lins[zlin + 4 * (i + 16) + 1] = xr(1 + 18 * (1 + i));
        lins[zlin + 4 * i - 64 + 2] = xl(18 * (1 + i));
        lins[zlin + 4 * i - 64 + 3] = xr(18 * (1 + i));

        // S0(0) S2(1) S1(2) S2(3) S1(4) S2(5) S1(6) S2(7)
        for k in 0..8usize {
            let w0 = WIN[w];
            let w1 = WIN[w + 1];
            w += 2;
            let vz = zlin + 4 * i - k * 64;
            let vy = zlin + 4 * i - (15 - k) * 64;
            for j in 0..4 {
                let z = lins[vz + j];
                let y = lins[vy + j];
                let bb = z * w1 + y * w0;
                if k == 0 {
                    b[j] = bb;
                    a[j] = z * w0 - y * w1;
                } else if k % 2 == 1 {
                    b[j] += bb;
                    a[j] += y * w1 - z * w0;
                } else {
                    b[j] += bb;
                    a[j] += z * w0 - y * w1;
                }
            }
        }
        put(pcm, dstr + (15 - i) * nch, a[1]);
        put(pcm, dstr + (17 + i) * nch, b[1]);
        put(pcm, dstl + (15 - i) * nch, a[0]);
        put(pcm, dstl + (17 + i) * nch, b[0]);
        put(pcm, dstr + (47 - i) * nch, a[3]);
        put(pcm, dstr + (49 + i) * nch, b[3]);
        put(pcm, dstl + (47 - i) * nch, a[2]);
        put(pcm, dstl + (49 + i) * nch, b[2]);
    }
}

/// Synthesize one granule (`nbands` = 18 for Layer III, 12 for Layers I/II) of `nch` channels into
/// interleaved `pcm` (scaled to [-1, 1]); `grbuf` holds 2 x 576 values.
pub(crate) fn synth_granule(
    qmf_state: &mut [f32; 960],
    grbuf: &mut [f32; 1152],
    nbands: usize,
    nch: usize,
    pcm: &mut [f32],
) {
    let nch = nch.clamp(1, 2);
    for ch in 0..nch {
        dct_ii(&mut grbuf[576 * ch..576 * (ch + 1)], nbands);
    }
    let mut lins = [0f32; 33 * 64];
    lins[..960].copy_from_slice(qmf_state);
    for i in (0..nbands.min(18)).step_by(2) {
        synth(grbuf, i, pcm, 32 * nch * i, nch, &mut lins, i * 64);
    }
    let from = nbands.min(18) * 64;
    if nch == 1 {
        for i in (0..960).step_by(2) {
            qmf_state[i] = lins[from + i];
        }
    } else {
        qmf_state.copy_from_slice(&lins[from..from + 960]);
    }
}
