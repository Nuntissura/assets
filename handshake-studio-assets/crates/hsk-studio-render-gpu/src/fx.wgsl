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
    op: u32,         // 0 INIT, 1 DILATE, 2 BLUR_H, 3 BLUR_V, 4 SHIFT, 5 FINISH
    mode: u32,       // INIT: 0 alpha, 1 1-alpha, 2 alpha >= 0.5, 3 alpha < 0.5; FINISH: 0 fin*(1-a), 1 fin*a, 2 (1-fin)*a, 3 min(fin2+fin*a, 1)
    k: u32,          // DILATE: ceil(radius); BLUR: half width of the kernel
    taps_n: u32,     // BLUR: number of taps (2k+1)
    clamp_out: u32,  // final pass of a chain: clamp the matte to [0, 1]
    dx: i32,         // SHIFT offset
    dy: i32,
    r: f32,          // DILATE radius
    outside: f32,    // value of a sample beyond the region
    pad0: u32,
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
        default: {
            let m = fin[li];
            switch (p.mode) {
                case 0u: {
                    v = m * (1.0 - a);
                }
                case 1u: {
                    v = m * a;
                }
                case 2u: {
                    v = (1.0 - m) * a;
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
