// Layer-effect matte passes (second compute module, separate from the composite kernels so the v1
// pipeline layout and every v1 byte stay unchanged). One entry point `fx` evaluates ONE pass of the
// matte chain over a region of the frame; fx.rs lists the passes of each effect and the parameter
// block. Every pass is a transcription of a private helper of hsk_studio_pigment::effect_math (the
// owner; parity is tested against `render_layer_reference`): INIT = the source plane of a chain,
// DILATE = `dilate`, BLUR_H / BLUR_V = the two passes of `blur_ext`, SHIFT = `shift_ext`,
// FINISH = the combine step of `reference_matte` (knockout, clip by alpha, glow centre, stroke).
//
// The planes (`fin`, `fin2`, `fout`) are region-sized f32 arrays with stride `rw`; a sample outside
// the region reads `p.outside`, which is exactly the reference's rule for a sample beyond its
// plane (the plane is the layer bounds plus the effect margin). `fcontent` is the layer content
// frame (RGBA f32, stride `fw`), whose alpha is the shape alpha the matte is derived from.
//
// Binding budget: four storage buffers is the downlevel limit of the device, so the blur taps
// travel in the uniform block (at most 2 * 1500 + 1 taps = 751 vec4).

struct FxP {
    fw: u32,         // frame width: stride of `fcontent`
    rx: u32,         // region origin inside the frame
    ry: u32,
    rw: u32,         // region size: the planes are rw x rh, stride rw
    rh: u32,
    op: u32,         // 0 INIT, 1 DILATE, 2 BLUR_H, 3 BLUR_V, 4 SHIFT, 5 FINISH, 6 BEVEL, 7 CHISEL
    mode: u32,       // INIT: 0 alpha, 1 1-alpha, 2 alpha >= 0.5, 3 alpha < 0.5, 4 one; FINISH: 0 fin*(1-a)/(1-knock*a) (knockout), 1 fin*a, 2 (1-fin)*a, 3 min(fin2+fin*a, 1), 4 |fin-fin2|*a, 5 (1-|fin-fin2|)*a, 6 max(fin, a), 7 1-fin, 8 |fin-fin2|, 9 1-|fin-fin2|; BEVEL: style*2+part
    k: u32,          // DILATE: ceil(radius); BLUR: half width of the kernel
    taps_n: u32,     // BLUR: number of taps (2k+1)
    clamp_out: u32,  // final pass of a chain: clamp the matte to [0, 1]
    dx: i32,         // SHIFT offset
    dy: i32,
    r: f32,          // DILATE radius
    outside: f32,    // value of a sample beyond the region
    knock: f32,      // FINISH 0: the layer's opacity * fill (pigment knockout_coverage)
    pad1: u32,
    taps: array<vec4<f32>, 751>,
}

@group(0) @binding(0) var<uniform> p: FxP;
@group(0) @binding(1) var<storage, read> fin: array<f32>;
@group(0) @binding(2) var<storage, read> fin2: array<f32>;
@group(0) @binding(3) var<storage, read_write> fout: array<f32>;
@group(0) @binding(4) var<storage, read> fcontent: array<vec4<f32>>;

fn sample_at(lx: i32, ly: i32) -> f32 {
    if (lx < 0 || ly < 0 || lx >= i32(p.rw) || ly >= i32(p.rh)) {
        return p.outside;
    }
    return fin[u32(ly) * p.rw + u32(lx)];
}

fn tap_at(i: u32) -> f32 {
    return p.taps[i >> 2u][i & 3u];
}

// effect_math::dilate: out(x) = max over y of src(y) * ramp(dist(x, y)), ramp = clamp(r + 0.5 - d,
// 0, 1), d the distance from the centre of pixel x to the unit square of pixel y; a pixel covers
// itself fully. The skip of a sample that cannot raise the maximum is exact (ramp <= 1).
fn dilate_at(x: i32, y: i32) -> f32 {
    let kk = i32(p.k);
    var best: f32 = 0.0;
    for (var oy: i32 = -kk; oy <= kk; oy = oy + 1) {
        for (var ox: i32 = -kk; ox <= kk; ox = ox + 1) {
            let v = sample_at(x + ox, y + oy);
            if (v <= best) {
                continue;
            }
            var cov: f32 = 1.0;
            if (ox != 0 || oy != 0) {
                let ddx = max(f32(abs(ox)) - 0.5, 0.0);
                let ddy = max(f32(abs(oy)) - 0.5, 0.0);
                cov = clamp(p.r + 0.5 - sqrt(ddx * ddx + ddy * ddy), 0.0, 1.0);
            }
            best = max(best, v * cov);
        }
    }
    return best;
}

// effect_math::blur_ext, one pass: taps accumulated in ascending tap order (the reference
// accumulates in binary64; this pass accumulates in binary32).
fn blur_at(x: i32, y: i32, horizontal: bool) -> f32 {
    let half = i32(p.k);
    var acc: f32 = 0.0;
    for (var i: u32 = 0u; i < p.taps_n; i = i + 1u) {
        let off = i32(i) - half;
        var v: f32;
        if (horizontal) {
            v = sample_at(x + off, y);
        } else {
            v = sample_at(x, y + off);
        }
        acc = acc + tap_at(i) * v;
    }
    return acc;
}

// ---- lane SMB-PS-ADJUST-STYLES-2: bevel shading and the chisel field (effect_math::style). ----

// style::bevel_matte `part`: N = (-k gx, -k gy, 1), s = N . L / |N| against the flat s0;
// highlight (s - s0) / (1 - s0), shadow (s0 - s) / s0, both clamped. The constants (k, L, s0) are
// pigment's binary64 values rounded to binary32, in taps[0] = (k, Lx, Ly, Lz) and taps[1].x = s0.
fn bevel_part(gx: f32, gy: f32, k: f32, shadow: bool) -> f32 {
    let l = p.taps[0];
    let flat = p.taps[1].x;
    let nx = -k * gx;
    let ny = -k * gy;
    let len = sqrt(nx * nx + ny * ny + 1.0);
    let shade = (nx * l.y + ny * l.z + l.w) / len;
    if (!shadow) {
        if (flat >= 1.0) {
            return 0.0;
        }
        return clamp((shade - flat) / (1.0 - flat), 0.0, 1.0);
    }
    if (flat <= 0.0) {
        return clamp(-shade, 0.0, 1.0);
    }
    return clamp((flat - shade) / flat, 0.0, 1.0);
}

// OP 6 BEVEL: `fin` is the height field (beyond the region: p.outside), `fin2` the stroke matte of
// a stroke emboss; mode = style * 2 + part (style 0 inner, 1 outer, 2 emboss, 3 pillow, 4 stroke;
// part 0 highlight, 1 shadow). Central differences as pigment.
fn bevel_at(x: i32, y: i32, li: u32, a: f32) -> f32 {
    let gx = (sample_at(x + 1, y) - sample_at(x - 1, y)) * 0.5;
    let gy = (sample_at(x, y + 1) - sample_at(x, y - 1)) * 0.5;
    let k = p.taps[0].x;
    let shadow = (p.mode & 1u) != 0u;
    var v: f32;
    switch (p.mode >> 1u) {
        case 0u: {
            v = bevel_part(gx, gy, k, shadow) * a;
        }
        case 1u: {
            v = bevel_part(gx, gy, k, shadow) * (1.0 - a);
        }
        case 2u: {
            v = bevel_part(gx, gy, k, shadow);
        }
        case 3u: {
            v = bevel_part(gx, gy, k, shadow) * a + bevel_part(gx, gy, -k, shadow) * (1.0 - a);
        }
        default: {
            v = bevel_part(gx, gy, k, shadow) * fin2[li];
        }
    }
    return clamp(v, 0.0, 1.0);
}

// OP 7 CHISEL (style::chisel_field): region fin >= 0.5 (beyond the region: outside, p.outside 0);
// d = centre distance to the nearest pixel of the other side within p.k (none: the cap p.r);
// field = +-min(d - 0.5, size) / size; size 0 is flat.
fn chisel_at(x: i32, y: i32) -> f32 {
    if (p.r <= 0.0) {
        return 0.0;
    }
    let kk = i32(p.k);
    let inside = sample_at(x, y) >= 0.5;
    var best: f32 = p.r + 0.5;
    var found = false;
    for (var oy: i32 = -kk; oy <= kk; oy = oy + 1) {
        for (var ox: i32 = -kk; ox <= kk; ox = ox + 1) {
            if ((sample_at(x + ox, y + oy) >= 0.5) != inside) {
                let d = sqrt(f32(ox * ox + oy * oy));
                if (!found || d < best) {
                    best = d;
                    found = true;
                }
            }
        }
    }
    var d = p.r;
    if (found) {
        d = min(best - 0.5, p.r);
    }
    var sv = d;
    if (!inside) {
        sv = -d;
    }
    return sv / p.r;
}

@compute @workgroup_size(8, 8, 1)
fn fx(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let li = gid.y * p.rw + gid.x;
    let a = fcontent[(p.ry + gid.y) * p.fw + p.rx + gid.x].w;
    var v: f32 = 0.0;
    switch (p.op) {
        case 0u: {
            switch (p.mode) {
                case 0u: {
                    v = a;
                }
                case 1u: {
                    v = 1.0 - a;
                }
                case 2u: {
                    v = select(0.0, 1.0, a >= 0.5);
                }
                case 4u: {
                    v = 1.0;
                }
                default: {
                    v = 1.0 - select(0.0, 1.0, a >= 0.5);
                }
            }
        }
        case 1u: {
            v = dilate_at(x, y);
        }
        case 2u: {
            v = blur_at(x, y, true);
        }
        case 3u: {
            v = blur_at(x, y, false);
        }
        case 4u: {
            v = sample_at(x - p.dx, y - p.dy);
        }
        case 6u: {
            v = bevel_at(x, y, li, a);
        }
        case 7u: {
            v = chisel_at(x, y);
        }
        default: {
            let m = fin[li];
            switch (p.mode) {
                case 0u: {
                    // drop-shadow knockout: m (1 - a), then pigment knockout_coverage
                    let kk = 1.0 - p.knock * a;
                    v = select(0.0, clamp(m * (1.0 - a), 0.0, 1.0) / kk, kk > 0.0);
                }
                case 1u: {
                    v = m * a;
                }
                case 2u: {
                    v = (1.0 - m) * a;
                }
                case 4u: {
                    v = clamp(abs(m - fin2[li]), 0.0, 1.0) * a;
                }
                case 5u: {
                    v = (1.0 - clamp(abs(m - fin2[li]), 0.0, 1.0)) * a;
                }
                case 6u: {
                    v = max(a, m);
                }
                case 7u: {
                    v = 1.0 - m;
                }
                case 8u: {
                    v = clamp(abs(m - fin2[li]), 0.0, 1.0);
                }
                case 9u: {
                    v = 1.0 - clamp(abs(m - fin2[li]), 0.0, 1.0);
                }
                default: {
                    v = min(fin2[li] + m * a, 1.0);
                }
            }
        }
    }
    if (p.clamp_out != 0u) {
        v = clamp(v, 0.0, 1.0);
    }
    fout[li] = v;
}
