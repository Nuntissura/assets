// Layer filter passes (lane SMB-RENDER-FILTERS slice 2; fourth compute module, separate from the
// composite, effect and blur kernels so their layouts and bytes stay unchanged; same bind group
// layout as blur.wgsl). One entry point `filter_pass` evaluates ONE pass over the whole frame (`fw` x
// `fh` texels, row-major RGBA f32, straight alpha unless noted). Every pass is a binary32
// transcription of hsk_studio_pigment::filter_math (the owner; parity is tested against
// `filter_window` in tests/gpu_filters.rs):
//   op 0 USM:   dst = unsharp(src = o, aux = b)        b = the Gaussian blur of o (blur.wgsl passes)
//   op 1 HP:    dst = high_pass(src = o, aux = b)
//   op 2 COPY:  dst = src
//   op 3 MED:   dst = median over the disc dx^2 + dy^2 <= r^2 of the premultiplied src, unpremultiplied
//   op 4 NOISE: dst = add_noise(src); the SplitMix64 hash of (seed, x, y, channel, draw) is the
//               reference's exactly (64-bit words as u32 pairs); the uniform value is the top 24
//               bits of the hash (the reference uses 53 bits in binary64), so a noise value differs
//               from the reference by at most 2^-24 times its amplitude
// A sample beyond the frame reads transparent (the frame carries the filter reach as margin, so no
// kept pixel reads one). The median radius is bounded (MED_MAX_RADIUS), enforced on the host.

struct FilterP {
    fw: u32,
    fh: u32,
    op: u32,
    radius: u32,      // MED: disc radius
    origin_x: i32,    // plan coordinates of frame texel (0, 0)
    origin_y: i32,
    seed_lo: u32,
    seed_hi: u32,
    amount: f32,      // USM: amount / 100; NOISE: A = amount / 200
    threshold: f32,   // USM: threshold in levels (0..255)
    dist: u32,        // NOISE: 0 uniform, 1 gaussian
    mono: u32,        // NOISE: 1 monochromatic
}

@group(0) @binding(0) var<uniform> p: FilterP;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> aux: array<vec4<f32>>;

const MED_MAX_RADIUS: i32 = 7;
const MED_MAX_N: u32 = 160u;

fn sample_at(x: i32, y: i32) -> vec4<f32> {
    if (x < 0 || y < 0 || x >= i32(p.fw) || y >= i32(p.fh)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return src[u32(y) * p.fw + u32(x)];
}

// blur_math::unpremultiply: A <= 0 is transparent; colour clamp(P / A, 0, 1); alpha min(A, 1).
fn unpremul(c: vec4<f32>) -> vec4<f32> {
    let a = c.w;
    if (!(a > 0.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return vec4<f32>(clamp(c.x / a, 0.0, 1.0), clamp(c.y / a, 0.0, 1.0), clamp(c.z / a, 0.0, 1.0), min(a, 1.0));
}

// ---- 64-bit words as vec2<u32> (x = low, y = high) ----

fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    var carry = 0u;
    if (lo < a.x) {
        carry = 1u;
    }
    return vec2<u32>(lo, a.y + b.y + carry);
}

fn xor64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x ^ b.x, a.y ^ b.y);
}

// Logical right shift by n in 1..=31.
fn shr64(a: vec2<u32>, n: u32) -> vec2<u32> {
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), a.y >> n);
}

// High 32 bits of the 64-bit product of two u32 (16-bit limbs).
fn mulhi32(a: u32, b: u32) -> u32 {
    let a0 = a & 0xFFFFu;
    let a1 = a >> 16u;
    let b0 = b & 0xFFFFu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 16u) + (p01 & 0xFFFFu) + (p10 & 0xFFFFu);
    return p11 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u);
}

// Low 64 bits of the 64-bit product.
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x * b.x;
    let hi = mulhi32(a.x, b.x) + a.x * b.y + a.y * b.x;
    return vec2<u32>(lo, hi);
}

// filter_math::splitmix.
fn splitmix(z0: vec2<u32>) -> vec2<u32> {
    var z = add64(z0, vec2<u32>(0x7F4A7C15u, 0x9E3779B9u));
    z = mul64(xor64(z, shr64(z, 30u)), vec2<u32>(0x1CE4E5B9u, 0xBF58476Du));
    z = mul64(xor64(z, shr64(z, 27u)), vec2<u32>(0x133111EBu, 0x94D049BBu));
    return xor64(z, shr64(z, 31u));
}

// An i32 plan coordinate as the reference's `x as u64` (i64 sign-extended).
fn from_i32(v: i32) -> vec2<u32> {
    var hi = 0u;
    if (v < 0) {
        hi = 0xFFFFFFFFu;
    }
    return vec2<u32>(bitcast<u32>(v), hi);
}

// filter_math::noise_uniform, 24-bit: the top 24 bits of the 53-bit value over 2^24.
fn noise_uniform(x: i32, y: i32, channel: u32, draw: u32) -> f32 {
    let seed = vec2<u32>(p.seed_lo, p.seed_hi);
    let inner = splitmix(vec2<u32>((channel << 8u) | draw, 0u));
    let h = splitmix(xor64(seed, splitmix(xor64(from_i32(x), splitmix(xor64(from_i32(y), inner))))));
    return f32(h.y >> 8u) * (1.0 / 16777216.0);
}

fn noise_value(x: i32, y: i32, channel: u32) -> f32 {
    if (p.dist == 0u) {
        return (2.0 * noise_uniform(x, y, channel, 0u) - 1.0) * p.amount;
    }
    let s = noise_uniform(x, y, channel, 0u) + noise_uniform(x, y, channel, 1u)
        + noise_uniform(x, y, channel, 2u) + noise_uniform(x, y, channel, 3u);
    return (s - 2.0) * p.amount;
}

@compute @workgroup_size(8, 8, 1)
fn filter_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.fw || gid.y >= p.fh) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let i = gid.y * p.fw + gid.x;
    switch (p.op) {
        case 0u: {
            let o = src[i];
            let b = aux[i];
            if (o.w == 0.0) {
                dst[i] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            } else {
                var c = o.xyz;
                let d = o.xyz - b.xyz;
                for (var k: u32 = 0u; k < 3u; k = k + 1u) {
                    if (abs(d[k]) * 255.0 >= p.threshold) {
                        c[k] = clamp(o[k] + p.amount * d[k], 0.0, 1.0);
                    }
                }
                dst[i] = vec4<f32>(c, o.w);
            }
        }
        case 1u: {
            let o = src[i];
            let b = aux[i];
            if (o.w == 0.0) {
                dst[i] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            } else {
                dst[i] = vec4<f32>(clamp(o.xyz - b.xyz + vec3<f32>(0.5, 0.5, 0.5), vec3<f32>(0.0), vec3<f32>(1.0)), o.w);
            }
        }
        case 2u: {
            dst[i] = src[i];
        }
        case 3u: {
            let r = i32(p.radius);
            let r2 = r * r;
            var m = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            var n = 0u;
            for (var c: u32 = 0u; c < 4u; c = c + 1u) {
                var vals: array<f32, 160>;
                n = 0u;
                for (var iy: i32 = 0; iy <= 2 * r; iy = iy + 1) {
                    for (var ix: i32 = 0; ix <= 2 * r; ix = ix + 1) {
                        let dx = ix - r;
                        let dy = iy - r;
                        if (dx * dx + dy * dy <= r2) {
                            let q = sample_at(x + dx, y + dy);
                            var v = q.w;
                            if (c < 3u) {
                                v = q[c] * q.w;
                            }
                            // Insertion into the sorted prefix.
                            var j = n;
                            loop {
                                if (j == 0u || vals[j - 1u] <= v) {
                                    break;
                                }
                                vals[j] = vals[j - 1u];
                                j = j - 1u;
                            }
                            vals[j] = v;
                            n = n + 1u;
                        }
                    }
                }
                m[c] = vals[n / 2u];
            }
            dst[i] = unpremul(m);
        }
        case 4u: {
            let o = src[i];
            if (o.w == 0.0) {
                dst[i] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            } else {
                let gx = p.origin_x + x;
                let gy = p.origin_y + y;
                var c = o.xyz;
                for (var k: u32 = 0u; k < 3u; k = k + 1u) {
                    var channel = k;
                    if (p.mono == 1u) {
                        channel = 0u;
                    }
                    c[k] = clamp(o[k] + noise_value(gx, gy, channel), 0.0, 1.0);
                }
                dst[i] = vec4<f32>(c, o.w);
            }
        }
        default: {
            dst[i] = src[i];
        }
    }
}
