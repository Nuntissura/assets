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
    ext: u32,        // 1: extended range (lane SMB-RENDER-GPU-EXTENDED-ENVELOPE)
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
    if (p.ext != 0u) {
        // Extended range (blur_math::unpremultiply_in): colour P / A unclamped.
        return vec4<f32>(c.x / a, c.y / a, c.z / a, min(a, 1.0));
    }
    return vec4<f32>(clamp(c.x / a, 0.0, 1.0), clamp(c.y / a, 0.0, 1.0), clamp(c.z / a, 0.0, 1.0), min(a, 1.0));
}

// Extended-range overflow marker shared by the extended passes (lane SMB-RENDER-GPU-EXTENDED-
// ENVELOPE): alpha 2 (no admitted alpha exceeds 1; 1.5 leaves room for rounding). The finiteness
// tests read the exponent bits, independent of the compiler's float NaN / inf assumptions.
fn hsk_finite1(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}
fn hsk_finite4(v: vec4<f32>) -> bool {
    let e = bitcast<vec4<u32>>(v) & vec4<u32>(0x7f800000u);
    return all(e != vec4<u32>(0x7f800000u));
}
fn hsk_marked(v: vec4<f32>) -> bool {
    return v.w > 1.5 || !hsk_finite4(v);
}
const HSK_OVERFLOW: vec4<f32> = vec4<f32>(0.0, 0.0, 0.0, 2.0);
// The output of an extended pass: the marker when an input was marked or the result is not finite.
fn hsk_out(v: vec4<f32>, marked: bool) -> vec4<f32> {
    if (marked || !hsk_finite4(v)) {
        return HSK_OVERFLOW;
    }
    return v;
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
            var marked = false;
            for (var t: u32 = 0u; t < p.taps_n; t = t + 1u) {
                let q = sample_at(x + i32(t) - half, y);
                marked = marked || (p.ext != 0u && hsk_marked(q));
                acc = acc + tap_at(t) * premul(q);
            }
            if (p.ext != 0u) {
                dst[i] = hsk_out(acc, marked);
            } else {
                dst[i] = acc;
            }
        }
        case 1u: {
            var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
            var marked = false;
            for (var t: u32 = 0u; t < p.taps_n; t = t + 1u) {
                let q = sample_at(x, y + i32(t) - half);
                marked = marked || (p.ext != 0u && hsk_marked(q));
                acc = acc + tap_at(t) * q;
            }
            if (p.ext != 0u) {
                dst[i] = hsk_out(unpremul(acc), marked);
            } else {
                dst[i] = unpremul(acc);
            }
        }
        default: {
            if (p.ext != 0u && (hsk_marked(dst[i]) || hsk_marked(src[i]) || hsk_marked(aux[i]))) {
                dst[i] = HSK_OVERFLOW;
                return;
            }
            let k = aux[i].w * p.opacity;
            if (k > 0.0) {
                let b = premul(dst[i]);
                let l = premul(src[i]);
                if (p.ext != 0u) {
                    dst[i] = hsk_out(unpremul(b + (l - b) * min(k, 1.0)), false);
                } else {
                    dst[i] = unpremul(b + (l - b) * min(k, 1.0));
                }
            }
        }
    }
}
