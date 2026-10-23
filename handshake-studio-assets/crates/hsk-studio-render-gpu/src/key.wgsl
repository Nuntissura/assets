// Key pipeline passes (lane SMB-AE-KEY-TRACK-3): a binary32 transcription of pigment `keying`
// (binary64), one dispatch per stage over the keyed layer's plane. Point stages (extractors with
// their matte combine, levels, despill, unmult) read the matte from `matte_in` and write
// `matte_out`; neighbourhood stages (choke/spread, the separable feather, edge-aware smoothing) read
// `matte_in` and write `matte_out`; the host swaps the two matte buffers after every dispatch.
// FXC-safe: no dynamic indexing of vector components.

struct Params {
    size: vec4<u32>,   // width, height, op, combine
    ints: vec4<i32>,   // per op (see key.rs)
    f0: vec4<f32>,
    f1: vec4<f32>,
    f2: vec4<f32>,
    f3: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> colour: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> matte_in: array<f32>;
@group(0) @binding(3) var<storage, read_write> matte_out: array<f32>;
@group(0) @binding(4) var<storage, read> aux: array<f32>;

const W_R: f32 = 0.2126;
const W_G: f32 = 0.7152;
const W_B: f32 = 0.0722;

fn luma(c: vec3<f32>) -> f32 {
    return W_R * c.x + W_G * c.y + W_B * c.z;
}
fn ycbcr(c: vec3<f32>) -> vec3<f32> {
    let y = luma(c);
    return vec3<f32>(y, (c.z - y) / 1.8556, (c.x - y) / 1.5748);
}
fn lab_f(t: f32) -> f32 {
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    if (t > e) {
        return pow(t, 1.0 / 3.0);
    }
    return (k * t + 16.0) / 116.0;
}
fn lab(c: vec3<f32>) -> vec3<f32> {
    let x = (0.4124 * c.x + 0.3576 * c.y + 0.1805 * c.z) / 0.95047;
    let y = 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
    let z = (0.0193 * c.x + 0.1192 * c.y + 0.9505 * c.z) / 1.08883;
    let fx = lab_f(x);
    let fy = lab_f(y);
    let fz = lab_f(z);
    return vec3<f32>((116.0 * fy - 16.0) / 100.0, 5.0 * (fx - fy), 2.0 * (fy - fz));
}
fn to_space(space: i32, c: vec3<f32>) -> vec3<f32> {
    if (space == 1) {
        return ycbcr(c);
    }
    if (space == 2) {
        return lab(c);
    }
    return c;
}
fn ramp(d: f32, t: f32, s: f32) -> f32 {
    if (d <= t) {
        return 0.0;
    }
    if (s <= 0.0) {
        return 1.0;
    }
    return min((d - t) / s, 1.0);
}
fn levels(v: f32, b: f32, w: f32) -> f32 {
    return clamp((v - b) / (w - b), 0.0, 1.0);
}
fn box_distance(v: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> f32 {
    let d = max(max(lo - v, v - hi), vec3<f32>(0.0));
    return max(d.x, max(d.y, d.z));
}
fn hue(c: vec3<f32>) -> f32 {
    let mx = max(c.x, max(c.y, c.z));
    let mn = min(c.x, min(c.y, c.z));
    let d = mx - mn;
    if (d <= 0.0) {
        return -1.0;
    }
    var h: f32;
    if (mx == c.x) {
        h = (c.y - c.z) / d;
        if (h < 0.0) {
            h = h + 6.0;
        }
    } else if (mx == c.y) {
        h = (c.z - c.x) / d + 2.0;
    } else {
        h = (c.x - c.y) / d + 4.0;
    }
    return h * 60.0;
}
fn combine(m: f32, e: f32) -> f32 {
    switch p.size.w {
        case 0u: { return e; }
        case 1u: { return m * e; }
        case 2u: { return min(m, e); }
        default: { return max(m, e); }
    }
}
fn screen_parts(c: vec3<f32>, screen: i32) -> vec3<f32> {
    // (screen channel, other 1, other 2)
    if (screen == 2) {
        return vec3<f32>(c.z, c.x, c.y);
    }
    return vec3<f32>(c.y, c.x, c.z);
}

fn index_of(id: vec3<u32>) -> i32 {
    if (id.x >= p.size.x || id.y >= p.size.y) {
        return -1;
    }
    return i32(id.y * p.size.x + id.x);
}

@compute @workgroup_size(8, 8, 1)
fn key_point(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = index_of(id);
    if (i < 0) {
        return;
    }
    var px = colour[i];
    let c = px.xyz;
    var m = matte_in[i];
    switch p.size.z {
        // chroma: f0.xyz key in the space, f0.w luma weight, f1.x tolerance, f1.y softness.
        case 0u: {
            var v: vec3<f32>;
            if (p.ints.y == 1) { v = lab(c); } else { v = ycbcr(c); }
            let dd = v - p.f0.xyz;
            let d = sqrt(dd.y * dd.y + dd.z * dd.z + (p.f0.w * dd.x) * (p.f0.w * dd.x));
            m = combine(m, ramp(d, p.f1.x, p.f1.y));
        }
        // luma: ints.y kind, f1 = threshold, tolerance, softness.
        case 1u: {
            let y = luma(c);
            var e: f32;
            switch p.ints.y {
                case 0: { e = ramp(y - p.f1.x, 0.0, p.f1.z); }
                case 1: { e = ramp(p.f1.x - y, 0.0, p.f1.z); }
                case 2: { e = ramp(abs(y - p.f1.x), p.f1.y, p.f1.z); }
                default: { e = 1.0 - ramp(abs(y - p.f1.x), p.f1.y, p.f1.z); }
            }
            m = combine(m, e);
        }
        // colour range: ints.y space, ints.z has subtract box, aux = add lo/hi, subtract lo/hi.
        case 2u: {
            let v = to_space(p.ints.y, c);
            let add_lo = vec3<f32>(aux[0], aux[1], aux[2]);
            let add_hi = vec3<f32>(aux[3], aux[4], aux[5]);
            var e = ramp(box_distance(v, add_lo, add_hi), 0.0, p.f1.x);
            if (p.ints.z == 1) {
                let sub_lo = vec3<f32>(aux[6], aux[7], aux[8]);
                let sub_hi = vec3<f32>(aux[9], aux[10], aux[11]);
                e = max(e, 1.0 - ramp(box_distance(v, sub_lo, sub_hi), 0.0, p.f1.x));
            }
            m = combine(m, e);
        }
        // colour key: f0.xyz key rgb, f1.x tolerance.
        case 3u: {
            let d = length(c - p.f0.xyz) / sqrt(3.0);
            var e = 1.0;
            if (d <= p.f1.x) { e = 0.0; }
            m = combine(m, e);
        }
        // colour difference: ints.y screen, f1 = a black, a white, b black, b white.
        case 4u: {
            let s = screen_parts(c, p.ints.y);
            let pa = 1.0 - levels(s.x - max(s.y, s.z), p.f1.x, p.f1.y);
            let pb = 1.0 - levels(s.x - (s.y + s.z) / 2.0, p.f1.z, p.f1.w);
            m = combine(m, max(pa, pb));
        }
        // linear colour: ints.y matching, ints.z keep, f0.xyz key rgb, f0.w key hue (-1 none),
        // f2.xy key Cb Cr, f1.x tolerance, f1.y softness.
        case 5u: {
            var d: f32;
            if (p.ints.y == 0) {
                d = length(c - p.f0.xyz) / sqrt(3.0);
            } else if (p.ints.y == 1) {
                let h = hue(c);
                if (h < 0.0 && p.f0.w < 0.0) {
                    d = 0.0;
                } else if (h < 0.0 || p.f0.w < 0.0) {
                    d = 1.0;
                } else {
                    let x = abs(h - p.f0.w);
                    d = min(x, 360.0 - x) / 180.0;
                }
            } else {
                let v = ycbcr(c);
                d = length(v.yz - p.f2.xy);
            }
            let matched = 1.0 - ramp(d, p.f1.x, p.f1.y);
            var e = 1.0 - matched;
            if (p.ints.z == 1) { e = matched; }
            m = combine(m, e);
        }
        // extract: ints.y channel, ints.z invert, f1 = black, white, black softness, white softness.
        case 6u: {
            var v: f32;
            switch p.ints.y {
                case 0: { v = luma(c); }
                case 1: { v = px.x; }
                case 2: { v = px.y; }
                case 3: { v = px.z; }
                default: { v = px.w; }
            }
            var keep = 1.0;
            if (v < p.f1.x) {
                keep = 0.0;
                if (p.f1.z > 0.0) { keep = max(1.0 - (p.f1.x - v) / p.f1.z, 0.0); }
            } else if (v > p.f1.y) {
                keep = 0.0;
                if (p.f1.w > 0.0) { keep = max(1.0 - (v - p.f1.y) / p.f1.w, 0.0); }
            }
            if (p.ints.z == 1) { keep = 1.0 - keep; }
            m = combine(m, keep);
        }
        // levels: f1.x black, f1.y white.
        case 7u: {
            m = levels(m, p.f1.x, p.f1.y);
        }
        // despill: ints.y screen, ints.z limit, ints.w preserve luminance, f1.x amount.
        case 8u: {
            if (px.w > 0.0) {
                let s = screen_parts(c, p.ints.y);
                var limit = (s.y + s.z) / 2.0;
                if (p.ints.z == 1) { limit = max(s.y, s.z); }
                if (p.ints.z == 2) { limit = min(s.y, s.z); }
                let excess = p.f1.x * max(s.x - limit, 0.0);
                if (excess > 0.0) {
                    var ws = W_G;
                    if (p.ints.y == 2) { ws = W_B; }
                    if (p.ints.y == 2) { px.z = s.x - excess; } else { px.y = s.x - excess; }
                    if (p.ints.w == 1) {
                        let lost = ws * excess;
                        px = vec4<f32>(min(px.x + lost, 1.0), min(px.y + lost, 1.0), min(px.z + lost, 1.0), px.w);
                    }
                    colour[i] = px;
                }
            }
        }
        // unmult: colour / max channel, matte * max channel.
        default: {
            let mx = max(c.x, max(c.y, c.z));
            if (mx > 0.0) {
                colour[i] = vec4<f32>(min(c / mx, vec3<f32>(1.0)), px.w);
            } else {
                colour[i] = vec4<f32>(0.0, 0.0, 0.0, px.w);
            }
            m = m * mx;
        }
    }
    matte_out[i] = m;
}

// Grey-level erosion (ints.y 0) or dilation (1) over the disc of radius ints.x; samples beyond
// the plane are not read.
@compute @workgroup_size(8, 8, 1)
fn key_morph(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = index_of(id);
    if (i < 0) {
        return;
    }
    let r = p.ints.x;
    let w = i32(p.size.x);
    let h = i32(p.size.y);
    let x = i32(id.x);
    let y = i32(id.y);
    var acc = 1.0;
    if (p.ints.y == 1) { acc = 0.0; }
    for (var yy = max(y - r, 0); yy <= min(y + r, h - 1); yy = yy + 1) {
        for (var xx = max(x - r, 0); xx <= min(x + r, w - 1); xx = xx + 1) {
            let dx = xx - x;
            let dy = yy - y;
            if (dx * dx + dy * dy > r * r) {
                continue;
            }
            let v = matte_in[yy * w + xx];
            if (p.ints.y == 1) { acc = max(acc, v); } else { acc = min(acc, v); }
        }
    }
    matte_out[i] = acc;
}

// One pass of the separable feather: taps in `aux` (2 * support + 1), axis ints.y (0 horizontal,
// 1 vertical), edges clamped to the border; the vertical pass clamps to [0, 1].
@compute @workgroup_size(8, 8, 1)
fn key_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = index_of(id);
    if (i < 0) {
        return;
    }
    let s = p.ints.x;
    let w = i32(p.size.x);
    let h = i32(p.size.y);
    let x = i32(id.x);
    let y = i32(id.y);
    var acc = 0.0;
    for (var t = 0; t <= 2 * s; t = t + 1) {
        let o = t - s;
        var v: f32;
        if (p.ints.y == 0) {
            v = matte_in[y * w + clamp(x + o, 0, w - 1)];
        } else {
            v = matte_in[clamp(y + o, 0, h - 1) * w + x];
        }
        acc = acc + aux[t] * v;
    }
    if (p.ints.y == 1) {
        acc = clamp(acc, 0.0, 1.0);
    }
    matte_out[i] = acc;
}

// Cross-bilateral smoothing of the matte guided by the colour: window radius ints.x clipped to the
// plane, weight 1 / (1 + |dC|^2 * f1.x) (f1.x = 1 / sigma^2).
@compute @workgroup_size(8, 8, 1)
fn key_smooth(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = index_of(id);
    if (i < 0) {
        return;
    }
    let r = p.ints.x;
    let w = i32(p.size.x);
    let h = i32(p.size.y);
    let x = i32(id.x);
    let y = i32(id.y);
    let c0 = colour[i].xyz;
    var num = 0.0;
    var den = 0.0;
    for (var yy = max(y - r, 0); yy <= min(y + r, h - 1); yy = yy + 1) {
        for (var xx = max(x - r, 0); xx <= min(x + r, w - 1); xx = xx + 1) {
            let j = yy * w + xx;
            let d = colour[j].xyz - c0;
            let wt = 1.0 / (1.0 + dot(d, d) * p.f1.x);
            num = num + wt * matte_in[j];
            den = den + wt;
        }
    }
    matte_out[i] = clamp(num / den, 0.0, 1.0);
}
