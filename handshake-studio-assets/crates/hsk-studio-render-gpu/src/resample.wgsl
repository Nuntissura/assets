// Layer-transform resampling (lane SMB-PIGMENT-TRANSFORM): a binary32 transcription of pigment
// `transform_math::PreparedTransform::sample_rgba`. The geometry (exact-texel rule, nearest choice,
// hard edge of the clamp mode, first tap, fractions) is decided on the host by pigment
// `sample_site` in binary64 and uploaded per pixel; this pass evaluates the filter weights, the
// taps and the premultiplied accumulation, writing straight RGBA into the region-sized source
// buffer the composite pass then reads.

struct ResampleParams {
    // region size (texels written)
    rw: u32,
    rh: u32,
    // the uploaded source window: origin in tile texels and size
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    // the whole tile
    tw: u32,
    th: u32,
    // 1 = bilinear (2 taps per axis), 2 = bicubic Catmull-Rom (4 taps per axis)
    filter_kind: u32,
    // 0 = transparent, 1 = clamp
    edge: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var<uniform> rp: ResampleParams;
// Per pixel: (first column | texel column, first row | texel row, tx bits, ty bits);
// tx = -1 transparent, tx = -2 exact texel.
@group(0) @binding(1) var<storage, read> sites: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> src_window: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> dst_out: array<vec4<f32>>;

// Tap index along an axis of `len` texels, or -1 when the tap reads nothing (transparent edge).
fn tap(i: i32, len: u32) -> i32 {
    if (rp.edge == 0u) {
        if (i < 0 || i >= i32(len)) {
            return -1;
        }
        return i;
    }
    return clamp(i, 0, i32(len) - 1);
}

fn texel(i: i32, j: i32) -> vec4<f32> {
    let lx = u32(i - rp.sx);
    let ly = u32(j - rp.sy);
    return src_window[ly * rp.sw + lx];
}

fn weights(t: f32) -> vec4<f32> {
    if (rp.filter_kind == 1u) {
        return vec4<f32>(1.0 - t, t, 0.0, 0.0);
    }
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        (-t3 + 2.0 * t2 - t) * 0.5,
        (3.0 * t3 - 5.0 * t2 + 2.0) * 0.5,
        (-3.0 * t3 + 4.0 * t2 + t) * 0.5,
        (t3 - t2) * 0.5,
    );
}

@compute @workgroup_size(8, 8, 1)
fn resample(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= rp.rw || gid.y >= rp.rh) {
        return;
    }
    let idx = gid.y * rp.rw + gid.x;
    let site = sites[idx];
    let tx = bitcast<f32>(site.z);
    if (tx == -1.0) {
        dst_out[idx] = vec4<f32>(0.0);
        return;
    }
    let c0 = bitcast<i32>(site.x);
    let r0 = bitcast<i32>(site.y);
    if (tx == -2.0) {
        dst_out[idx] = texel(c0, r0);
        return;
    }
    let ty = bitcast<f32>(site.w);
    let wx = weights(tx);
    let wy = weights(ty);
    var n = 4;
    if (rp.filter_kind == 1u) {
        n = 2;
    }
    var a = 0.0;
    var p = vec3<f32>(0.0);
    for (var dj = 0; dj < n; dj = dj + 1) {
        let j = tap(r0 + dj, rp.th);
        if (j < 0) {
            continue;
        }
        for (var di = 0; di < n; di = di + 1) {
            let i = tap(c0 + di, rp.tw);
            if (i < 0) {
                continue;
            }
            let t = texel(i, j);
            let wa = wx[di] * wy[dj] * t.w;
            a = a + wa;
            p = p + wa * t.xyz;
        }
    }
    if (a <= 0.0) {
        dst_out[idx] = vec4<f32>(0.0);
        return;
    }
    dst_out[idx] = vec4<f32>(clamp(p / a, vec3<f32>(0.0), vec3<f32>(1.0)), min(a, 1.0));
}
