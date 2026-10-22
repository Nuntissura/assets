// Clip colour grade kernel (lane SMB-PR-SOURCE-MONITOR): a binary32 transcription of composite's
// `ClipGrade::apply` over a Prism `GradeProgram` (plan_sink/grade.rs packs it). The formulas are
// their owners': Prism `CubeLut::apply` (domain clamp, 1D shaper, tetrahedral 3D), `apply_wheels`,
// `band_weights`, `key_weight`, `vignette_gain`; Pigment `PreparedCurve::eval` (natural cubic
// spline), `rgb_to_hsl` / `hsl_to_rgb`, vibrance (`lum` 0.3/0.59/0.11, chroma max - min). One
// invocation per texel; texels whose alpha is not above 0 are left unchanged.

@group(0) @binding(0) var<storage, read> hdr: array<u32>;
@group(0) @binding(1) var<storage, read> prog: array<f32>;
@group(0) @binding(2) var<storage, read_write> colour: array<f32>;
@group(0) @binding(3) var<storage, read> alpha: array<f32>;

// hdr: 0 width, 1 height, 2 flags, 3 secondaries, 4..7 input LUT (3D offset, 3D size, 1D offset,
// 1D size), 8..11 look LUT, 12..21 spline offsets, 22..31 spline knot counts (0 = absent).
// prog: 0..2 luma, 3 exposure gain, 4 saturation, 5 vibrance, 6 look intensity, 7 faded floor,
// 8..13 input LUT domain min/max, 14..19 look LUT domain min/max, 20..23 vignette, 24 primary
// wheels (49), 73 wheels (49), 122 secondaries (8 x 60).
const F_INPUT_LUT: u32 = 1u;
const F_EXPOSURE: u32 = 2u;
const F_PRIMARY: u32 = 4u;
const F_TONE: u32 = 8u;
const F_SATURATION: u32 = 16u;
const F_VIBRANCE: u32 = 32u;
const F_LOOK: u32 = 64u;
const F_FADED: u32 = 128u;
const F_RGB_CURVES: u32 = 256u;
const F_HSL_CURVES: u32 = 512u;
const F_WHEELS: u32 = 1024u;
const F_VIGNETTE: u32 = 2048u;
const PRIMARY: u32 = 24u;
const WHEELS: u32 = 73u;
const SECONDARIES: u32 = 122u;
const SECONDARY_STRIDE: u32 = 60u;

fn clamp01(c: vec3<f32>) -> vec3<f32> {
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn luma_of(c: vec3<f32>, w: vec3<f32>) -> f32 {
    return c.x * w.x + c.y * w.y + c.z * w.z;
}

fn saturate3(c: vec3<f32>, y: f32, k: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(y + (c.x - y) * k.x, y + (c.y - y) * k.y, y + (c.z - y) * k.z);
}

fn powp(x: f32, e: f32) -> f32 {
    if (x <= 0.0) {
        return 0.0;
    }
    return exp2(e * log2(x));
}

// Prism `locate`: (cell, fraction) of t in 0..=1 over n >= 2 samples.
fn locate(t: f32, n: u32) -> vec2<f32> {
    let last = f32(n - 1u);
    let x = t * last;
    let i = min(floor(x), last - 1.0);
    return vec2<f32>(i, x - i);
}

fn lut_row(o: u32, n: u32, r: u32, g: u32, b: u32) -> vec3<f32> {
    let i = o + ((b * n + g) * n + r) * 3u;
    return vec3<f32>(prog[i], prog[i + 1u], prog[i + 2u]);
}

// One channel of a 1D shaper table (`n1` rows of three at `o1`), linear between rows.
fn shaper(o1: u32, n1: u32, t: f32, k: u32) -> f32 {
    let l = locate(t, n1);
    let i = u32(l.x);
    let a = prog[o1 + i * 3u + k];
    let b = prog[o1 + (i + 1u) * 3u + k];
    return a + l.y * (b - a);
}

// Prism `CubeLut::apply`; `which` 0 = input LUT, 1 = look LUT.
fn lut_apply(which: u32, c: vec3<f32>) -> vec3<f32> {
    let mb = 4u + which * 4u;
    let pb = 8u + which * 6u;
    let lo = vec3<f32>(prog[pb], prog[pb + 1u], prog[pb + 2u]);
    let hi = vec3<f32>(prog[pb + 3u], prog[pb + 4u], prog[pb + 5u]);
    var t = (clamp(c, lo, hi) - lo) / (hi - lo);
    let n1 = hdr[mb + 3u];
    let n = hdr[mb + 1u];
    if (n1 > 0u) {
        let o1 = hdr[mb + 2u];
        t = vec3<f32>(shaper(o1, n1, t.x, 0u), shaper(o1, n1, t.y, 1u), shaper(o1, n1, t.z, 2u));
        if (n == 0u) {
            return t;
        }
        t = clamp01(t);
    }
    let o = hdr[mb];
    let lr = locate(t.x, n);
    let lg = locate(t.y, n);
    let lb = locate(t.z, n);
    let r0 = u32(lr.x);
    let g0 = u32(lg.x);
    let b0 = u32(lb.x);
    let fr = lr.y;
    let fg = lg.y;
    let fb = lb.y;
    let r1 = r0 + 1u;
    let g1 = g0 + 1u;
    let b1 = b0 + 1u;
    let c000 = lut_row(o, n, r0, g0, b0);
    let c111 = lut_row(o, n, r1, g1, b1);
    var w = vec3<f32>(0.0);
    var p1 = vec3<f32>(0.0);
    var p2 = vec3<f32>(0.0);
    if (fr >= fg && fg >= fb) {
        w = vec3<f32>(fr, fg, fb);
        p1 = lut_row(o, n, r1, g0, b0);
        p2 = lut_row(o, n, r1, g1, b0);
    } else if (fr >= fb && fb >= fg) {
        w = vec3<f32>(fr, fb, fg);
        p1 = lut_row(o, n, r1, g0, b0);
        p2 = lut_row(o, n, r1, g0, b1);
    } else if (fb >= fr && fr >= fg) {
        w = vec3<f32>(fb, fr, fg);
        p1 = lut_row(o, n, r0, g0, b1);
        p2 = lut_row(o, n, r1, g0, b1);
    } else if (fg >= fr && fr >= fb) {
        w = vec3<f32>(fg, fr, fb);
        p1 = lut_row(o, n, r0, g1, b0);
        p2 = lut_row(o, n, r1, g1, b0);
    } else if (fg >= fb && fb >= fr) {
        w = vec3<f32>(fg, fb, fr);
        p1 = lut_row(o, n, r0, g1, b0);
        p2 = lut_row(o, n, r0, g1, b1);
    } else {
        w = vec3<f32>(fb, fg, fr);
        p1 = lut_row(o, n, r0, g0, b1);
        p2 = lut_row(o, n, r0, g1, b1);
    }
    return c000 + w.x * (p1 - c000) + w.y * (p2 - p1) + w.z * (c111 - p2);
}

// Pigment `PreparedCurve::eval` of spline `s` (xs, ys, m consecutive at its offset).
fn spline(s: u32, x: f32) -> f32 {
    let o = hdr[12u + s];
    let n = hdr[22u + s];
    if (x <= prog[o]) {
        return prog[o + n];
    }
    if (x >= prog[o + n - 1u]) {
        return prog[o + 2u * n - 1u];
    }
    var i = 0u;
    loop {
        if (i + 2u >= n) {
            break;
        }
        if (prog[o + i + 1u] > x) {
            break;
        }
        i = i + 1u;
    }
    let x0 = prog[o + i];
    let x1 = prog[o + i + 1u];
    let h = x1 - x0;
    let a = (x1 - x) / h;
    let b = (x - x0) / h;
    let y = a * prog[o + n + i] + b * prog[o + n + i + 1u]
        + ((a * a * a - a) * prog[o + 2u * n + i] + (b * b * b - b) * prog[o + 2u * n + i + 1u]) * h * h / 6.0;
    return clamp(y, 0.0, 1.0);
}

fn has_spline(s: u32) -> bool {
    return hdr[22u + s] > 0u;
}

// Pigment curves of one channel: the channel curve `s`, then the composite curve (spline 1).
fn rgb_curve(s: u32, x: f32) -> f32 {
    var v = x;
    if (has_spline(s)) {
        v = spline(s, v);
    }
    if (has_spline(1u)) {
        v = spline(1u, v);
    }
    return v;
}

// Prism `band_weights`.
fn band_weights(y0: f32, low_mid: f32, mid_high: f32) -> vec3<f32> {
    let y = clamp(y0, 0.0, 1.0);
    let s1 = clamp(1.0 - y / (2.0 * low_mid), 0.0, 1.0);
    let ws = s1 * s1;
    let h1 = clamp(1.0 - (1.0 - y) / (2.0 * (1.0 - mid_high)), 0.0, 1.0);
    let wh = h1 * h1;
    let wm = 1.0 - ws - wh;
    if (wm >= 0.0) {
        return vec3<f32>(ws, wm, wh);
    }
    let s = ws + wh;
    return vec3<f32>(ws / s, 0.0, wh / s);
}

// One wheel parameter (k: 0 offset, 1 gamma, 2 gain) of one band for channel ch.
fn band_param(wb: u32, band: u32, k: u32, ch: u32) -> f32 {
    return prog[wb + band * 9u + k * 3u + ch];
}

// One band-blended wheel parameter (k: 0 offset, 1 gamma, 2 gain) for channel ch.
fn blended(wb: u32, bw: vec3<f32>, k: u32, ch: u32) -> f32 {
    return band_param(wb, 0u, k, ch) + bw.x * band_param(wb, 1u, k, ch) + bw.y * band_param(wb, 2u, k, ch)
        + bw.z * band_param(wb, 3u, k, ch);
}

// One channel of Prism `apply_wheels` (after input saturation).
fn wheel_channel(wb: u32, bw: vec3<f32>, ch: u32, temp: f32, contrast: f32, pivot: f32, x: f32) -> f32 {
    let temp_gain = exp2(blended(wb, bw, 2u, ch) + temp);
    let offset_p = temp_gain * (blended(wb, bw, 0u, ch) + pivot - contrast * pivot);
    let gain_p = temp_gain * contrast;
    let gamma_p = exp2(blended(wb, bw, 1u, ch) * log2(3.0));
    return powp(max(gain_p * x + offset_p, 0.0), 1.0 / gamma_p);
}

// Prism `apply_wheels` of the wheels packed at `wb`.
fn wheels_apply(wb: u32, c0: vec3<f32>) -> vec3<f32> {
    let lw = vec3<f32>(prog[wb + 46u], prog[wb + 47u], prog[wb + 48u]);
    let y0 = luma_of(c0, lw);
    let isat = max(prog[wb + 40u], 0.0);
    let c = saturate3(c0, y0, vec3<f32>(isat));
    let bw = band_weights(luma_of(c, lw), prog[wb + 44u], prog[wb + 45u]);
    let temperature = prog[wb + 36u];
    let magenta = prog[wb + 37u];
    let temp = vec3<f32>(temperature + magenta * 0.5, -magenta, -temperature + magenta * 0.5);
    let contrast = exp2(prog[wb + 38u]);
    let pivot = prog[wb + 39u];
    let res = vec3<f32>(
        wheel_channel(wb, bw, 0u, temp.x, contrast, pivot, c.x),
        wheel_channel(wb, bw, 1u, temp.y, contrast, pivot, c.y),
        wheel_channel(wb, bw, 2u, temp.z, contrast, pivot, c.z),
    );
    let post = vec3<f32>(max(prog[wb + 41u], 0.0), max(prog[wb + 42u], 0.0), max(prog[wb + 43u], 0.0));
    return saturate3(res, luma_of(res, lw), post);
}

// Pigment `rgb_to_hsl` (h, s, l in 0..=1).
fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.x, c.y), c.z);
    let mn = min(min(c.x, c.y), c.z);
    let l = (mx + mn) / 2.0;
    if (mx == mn) {
        return vec3<f32>(0.0, 0.0, l);
    }
    let d = mx - mn;
    var s = 0.0;
    if (l <= 0.5) {
        s = d / (mx + mn);
    } else {
        s = d / (2.0 - mx - mn);
    }
    var h = 0.0;
    if (mx == c.x) {
        h = (c.y - c.z) / d;
    } else if (mx == c.y) {
        h = 2.0 + (c.z - c.x) / d;
    } else {
        h = 4.0 + (c.x - c.y) / d;
    }
    h = h / 6.0;
    if (h < 0.0) {
        h = h + 1.0;
    }
    return vec3<f32>(h, s, l);
}

fn hsl_channel(m1: f32, m2: f32, t0: f32) -> f32 {
    var t = t0;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    var v = m1;
    if (t < 1.0 / 6.0) {
        v = m1 + (m2 - m1) * t * 6.0;
    } else if (t < 0.5) {
        v = m2;
    } else if (t < 2.0 / 3.0) {
        v = m1 + (m2 - m1) * (2.0 / 3.0 - t) * 6.0;
    }
    return clamp(v, 0.0, 1.0);
}

// Pigment `hsl_to_rgb`.
fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s == 0.0) {
        return vec3<f32>(l);
    }
    var m2 = 0.0;
    if (l <= 0.5) {
        m2 = l * (1.0 + s);
    } else {
        m2 = l + s - l * s;
    }
    let m1 = 2.0 * l - m2;
    return vec3<f32>(hsl_channel(m1, m2, h + 1.0 / 3.0), hsl_channel(m1, m2, h), hsl_channel(m1, m2, h - 1.0 / 3.0));
}

// composite `HslCurves::apply` (splines 5..9: hue vs hue, hue vs sat, hue vs lum, lum vs sat, sat vs sat).
fn hsl_curves(c: vec3<f32>) -> vec3<f32> {
    let hsl = rgb_to_hsl(c);
    var h2 = hsl.x;
    var s2 = hsl.y;
    var l2 = hsl.z;
    if (has_spline(5u)) {
        let v = hsl.x + (spline(5u, hsl.x) - 0.5);
        h2 = v - floor(v);
    }
    if (has_spline(6u)) {
        s2 = s2 * 2.0 * spline(6u, hsl.x);
    }
    if (has_spline(7u)) {
        l2 = l2 * 2.0 * spline(7u, hsl.x);
    }
    if (has_spline(8u)) {
        s2 = s2 * 2.0 * spline(8u, hsl.z);
    }
    if (has_spline(9u)) {
        s2 = s2 * 2.0 * spline(9u, hsl.y);
    }
    return hsl_to_rgb(vec3<f32>(h2, clamp(s2, 0.0, 1.0), clamp(l2, 0.0, 1.0)));
}

// Prism `ramp`.
fn ramp(distance: f32, soft: f32) -> f32 {
    if (distance <= 0.0) {
        return 1.0;
    }
    if (soft <= 0.0) {
        return 0.0;
    }
    return clamp(1.0 - distance / soft, 0.0, 1.0);
}

fn arc(a: f32, b: f32) -> f32 {
    let x = a - b;
    let d = x - 360.0 * floor(x / 360.0);
    return min(d, 360.0 - d);
}

// Prism `key_weight` of the secondary at `sb` (hue degrees, saturation, lightness).
fn key_weight(sb: u32, h: f32, s: f32, l: f32) -> f32 {
    let lo = prog[sb];
    let hi = prog[sb + 1u];
    var inside = false;
    if (lo <= hi) {
        inside = h >= lo && h <= hi;
    } else {
        inside = h >= lo || h <= hi;
    }
    var hue = 1.0;
    if (!inside) {
        hue = ramp(min(arc(h, lo), arc(h, hi)), prog[sb + 2u]);
    }
    let ws = ramp(max(prog[sb + 3u] - s, s - prog[sb + 4u]), prog[sb + 5u]);
    let wl = ramp(max(prog[sb + 6u] - l, l - prog[sb + 7u]), prog[sb + 8u]);
    let w = hue * ws * wl;
    if (prog[sb + 9u] > 0.5) {
        return 1.0 - w;
    }
    return w;
}

// Prism `vignette_gain` at pixel centre (x, y) of a width x height frame.
fn vignette_gain(x: f32, y: f32, width: f32, height: f32) -> f32 {
    let amount = prog[20u];
    let r = prog[22u] / 100.0;
    let short = min(width, height);
    var hw = 0.5 * width;
    var hh = 0.5 * height;
    var p = 2.0;
    if (r >= 0.0) {
        hw = 0.5 * (width + (short - width) * r);
        hh = 0.5 * (height + (short - height) * r);
    } else {
        p = 2.0 + 6.0 * -r;
    }
    let u = abs((x - 0.5 * width) / hw);
    let w = abs((y - 0.5 * height) / hh);
    let d = powp(powp(u, p) + powp(w, p), 1.0 / p);
    let m = prog[21u] / 100.0;
    let f = prog[23u] / 100.0;
    let e0 = m * (1.0 - f);
    let e1 = m * (1.0 + f);
    var t = 0.0;
    if (e1 <= e0) {
        if (d >= e0) {
            t = 1.0;
        }
    } else {
        let s = clamp((d - e0) / (e1 - e0), 0.0, 1.0);
        t = s * s * (3.0 - 2.0 * s);
    }
    return exp2(amount * t);
}

@compute @workgroup_size(8, 8, 1)
fn grade(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = hdr[0];
    let height = hdr[1];
    if (gid.x >= width || gid.y >= height) {
        return;
    }
    let at = gid.y * width + gid.x;
    if (!(alpha[at] > 0.0)) {
        return;
    }
    let flags = hdr[2];
    let luma = vec3<f32>(prog[0], prog[1], prog[2]);
    var c = clamp01(vec3<f32>(colour[at * 3u], colour[at * 3u + 1u], colour[at * 3u + 2u]));
    if ((flags & F_INPUT_LUT) != 0u) {
        c = clamp01(lut_apply(0u, c));
    }
    if ((flags & F_EXPOSURE) != 0u) {
        c = clamp01(c * prog[3]);
    }
    if ((flags & F_PRIMARY) != 0u) {
        c = clamp01(wheels_apply(PRIMARY, c));
    }
    if ((flags & F_TONE) != 0u) {
        c = vec3<f32>(spline(0u, c.x), spline(0u, c.y), spline(0u, c.z));
    }
    if ((flags & F_SATURATION) != 0u) {
        c = clamp01(saturate3(c, luma_of(c, luma), vec3<f32>(prog[4])));
    }
    if ((flags & F_VIBRANCE) != 0u) {
        let lw = vec3<f32>(0.3, 0.59, 0.11);
        let l = luma_of(c, lw);
        let s = max(max(c.x, c.y), c.z) - min(min(c.x, c.y), c.z);
        let c1 = saturate3(c, l, vec3<f32>(1.0 + prog[5] * (1.0 - s)));
        c = clamp01(saturate3(c1, l, vec3<f32>(1.0)));
    }
    if ((flags & F_LOOK) != 0u) {
        let o = lut_apply(1u, c);
        c = clamp01(c + (o - c) * prog[6]);
    }
    if ((flags & F_FADED) != 0u) {
        let d = vec3<f32>(1.0) - c;
        c = clamp01(c + prog[7] * d * d);
    }
    if ((flags & F_RGB_CURVES) != 0u) {
        c = clamp01(vec3<f32>(rgb_curve(2u, c.x), rgb_curve(3u, c.y), rgb_curve(4u, c.z)));
    }
    if ((flags & F_HSL_CURVES) != 0u) {
        c = clamp01(hsl_curves(c));
    }
    if ((flags & F_WHEELS) != 0u) {
        c = clamp01(wheels_apply(WHEELS, c));
    }
    let n_sec = hdr[3];
    for (var k = 0u; k < n_sec; k = k + 1u) {
        let sb = SECONDARIES + k * SECONDARY_STRIDE;
        let hsl = rgb_to_hsl(c);
        let w = key_weight(sb, hsl.x * 360.0, hsl.y, hsl.z);
        if (w <= 0.0) {
            continue;
        }
        let corrected0 = wheels_apply(sb + 11u, c);
        let corrected = clamp01(saturate3(corrected0, luma_of(corrected0, luma), vec3<f32>(prog[sb + 10u])));
        c = clamp01(c + (corrected - c) * w);
    }
    if ((flags & F_VIGNETTE) != 0u) {
        let g = vignette_gain(f32(gid.x) + 0.5, f32(gid.y) + 0.5, f32(width), f32(height));
        c = clamp01(c * g);
    }
    colour[at * 3u] = c.x;
    colour[at * 3u + 1u] = c.y;
    colour[at * 3u + 2u] = c.z;
}
