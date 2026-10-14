// Layer blur and backdrop blur passes (lane SMB-COMPOSITE-NEW-KINDS slice 1; third compute module,
// separate from the composite and effect kernels so their layouts and bytes stay unchanged). One
// entry point `blur` evaluates ONE pass over the whole frame (`fw` x `fh` texels, row-major RGBA
// f32). Every pass is a binary32 transcription of hsk_studio_pigment::blur_math (the owner; parity is
// tested against `blur_window` / `backdrop_mix` in tests/gpu_blur.rs):
//   op 0 BLUR_H: dst = sum_t taps[t] * premultiplied(src[x + t - k, y])   (straight src, premul dst)
//   op 1 BLUR_V: dst = unpremultiply(sum_t taps[t] * src[x, y + t - k])   (premul src, straight dst)
//   op 2 MIX:    k = aux.a * opacity; dst = unpremultiply(Pd + (Ps - Pd) * min(k, 1)) where k > 0
//                (dst = the running backdrop, src = the blurred backdrop, aux = the layer's shape)
// A sample beyond the frame reads transparent (the frame carries the blur support as margin, so no
// kept pixel reads one).
//
// Binding budget: four storage buffers is the downlevel limit; the taps travel in the uniform block
// (at most 2 * 1500 + 1 = 3001 taps = 751 vec4).

struct BlurP {
    fw: u32,
    fh: u32,
    k: u32,          // half width of the kernel
    taps_n: u32,     // 2k + 1
    op: u32,
    pad0: u32,
    opacity: f32,    // MIX: the layer opacity
    pad1: u32,
    taps: array<vec4<f32>, 751>,
}

@group(0) @binding(0) var<uniform> p: BlurP;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> aux: array<vec4<f32>>;

fn tap_at(i: u32) -> f32 {
    return p.taps[i >> 2u][i & 3u];
}

fn sample_at(x: i32, y: i32) -> vec4<f32> {
    if (x < 0 || y < 0 || x >= i32(p.fw) || y >= i32(p.fh)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return src[u32(y) * p.fw + u32(x)];
}

fn premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.x * c.w, c.y * c.w, c.z * c.w, c.w);
}

// blur_math::unpremultiply: A <= 0 is transparent; colour clamp(P / A, 0, 1); alpha min(A, 1).
fn unpremul(c: vec4<f32>) -> vec4<f32> {
    let a = c.w;
    if (!(a > 0.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return vec4<f32>(clamp(c.x / a, 0.0, 1.0), clamp(c.y / a, 0.0, 1.0), clamp(c.z / a, 0.0, 1.0), min(a, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn blur(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.fw || gid.y >= p.fh) {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);
    let i = gid.y * p.fw + gid.x;
    let half = i32(p.k);
    switch (p.op) {
        case 0u: {
            var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            for (var t: u32 = 0u; t < p.taps_n; t = t + 1u) {
                acc = acc + tap_at(t) * premul(sample_at(x + i32(t) - half, y));
            }
            dst[i] = acc;
        }
        case 1u: {
            var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            for (var t: u32 = 0u; t < p.taps_n; t = t + 1u) {
                acc = acc + tap_at(t) * sample_at(x, y + i32(t) - half);
            }
            dst[i] = unpremul(acc);
        }
        default: {
            let k = aux[i].w * p.opacity;
            if (k > 0.0) {
                let b = premul(dst[i]);
                let l = premul(src[i]);
                dst[i] = unpremul(b + (l - b) * min(k, 1.0));
            }
        }
    }
}
