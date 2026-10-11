// One layer composited into the chunk destination: W3C Compositing 1 straight-alpha source-over
// with blend (s5.1, s9.1.4), the same formula as hsk_studio_pigment::blend_math::composite_over.
// Colours are straight RGB in [0,1] (the host refuses anything else), the destination chunk is
// RGBA f32 and starts transparent. One invocation owns one destination pixel, so the in-place
// read-modify-write is race free. Sources arrive already in the plan's blending space (STU-COL-163).

struct Params {
    chunk_w: u32,
    chunk_h: u32,
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
    mode: u32,     // 0 normal, 1 multiply, 2 screen
    kind: u32,     // 0 solid fill, 1 buffer source (rw*rh texels, row-major)
    opacity: f32,  // layer opacity * fill opacity
    pad0: u32,
    pad1: u32,
    pad2: u32,
    solid: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;

fn blend(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    var raw: vec3<f32>;
    if (mode == 1u) {
        raw = cb * cs;
    } else if (mode == 2u) {
        raw = cb + cs - cb * cs;
    } else {
        raw = cs;
    }
    return clamp(raw, vec3<f32>(0.0), vec3<f32>(1.0));
}

@compute @workgroup_size(8, 8, 1)
fn composite(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let di = (p.ry + gid.y) * p.chunk_w + (p.rx + gid.x);
    var s: vec4<f32>;
    if (p.kind == 0u) {
        s = p.solid;
    } else {
        s = src[gid.y * p.rw + gid.x];
    }
    if (p.opacity <= 0.0 || s.a <= 0.0) {
        return;
    }
    let a_s = s.a * p.opacity;
    let b = dst[di];
    let a_b = b.a;
    let blended = blend(p.mode, b.rgb, s.rgb);
    let a_o = a_s + a_b * (1.0 - a_s);
    if (a_o <= 0.0) {
        dst[di] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }
    let cs_prime = (1.0 - a_b) * s.rgb + a_b * blended;
    let co = a_s * cs_prime + (1.0 - a_s) * a_b * b.rgb;
    dst[di] = vec4<f32>(clamp(co / a_o, vec3<f32>(0.0), vec3<f32>(1.0)), min(a_o, 1.0));
}
