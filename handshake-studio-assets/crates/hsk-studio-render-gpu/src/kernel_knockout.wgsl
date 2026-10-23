
// ---- knockout passes (lane SMB-PS-ADJUST-STYLES-3): WGSL transcriptions of pigment knockout_math
// (the single owner of the rule; render-cpu runs the same functions per pixel). Appended to the
// composite module by kernel.rs, so `blend` and the overflow helpers are shared; each entry point
// has its own bind group layout of at most four storage buffers (the downlevel limit):
//
//   knockout_s     (bindings 0, 1, 3, 6, 7, 8) over the layer region:
//                  ko_k := clamp(source alpha * mask * layer opacity)          (the shape)
//                  ko_s := composite_over(mode, ko_s, source, opacity * fill)  (S over the initial
//                  backdrop, which the host copied into ko_s: transparent or the start snapshot)
//   knockout_mix   (bindings 0, 2, 6, 7, 8, 9) over the layer region:
//                  dst := knockout_over = lerp(dst, S, shape) on premultiplied colour and alpha;
//                  KO_RECORD: ko_acc := knockout_union(ko_acc, shape) (deep knockout)
//   knockout_clear (bindings 0, 2, 6, 7, 8, 9) over the chunk, when an isolated group holding deep
//                  knockouts is finished: dst (the enclosing running result) := knockout_clear(dst,
//                  ko_k = the group's accumulated shape); KO_OUTER: ko_acc := knockout_union(ko_acc,
//                  ko_k) (the next enclosing isolated buffer's plane)
//
// The arithmetic is the unit form of pigment (knockout_over clamps); in an extended-range plan a
// sample outside [0, 1] that render-cpu would hand to knockout_over is the overflow marker, which
// the host refuses as `sample_range_outside_unit` (render-cpu's refusal for that sample).

struct Ko {
    layer_opacity: f32,
    // bit 0 KO_RECORD (mix: unite the shape into ko_acc), bit 1 KO_OUTER (clear: unite ko_k into
    // ko_acc), bit 2 KO_EXTENDED (extended-range plan: out-of-unit samples become the marker; the
    // params block never carries the extended flag here, so `blend` stays the unit form)
    flags: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(6) var<uniform> ko: Ko;
@group(0) @binding(7) var<storage, read_write> ko_s: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> ko_k: array<f32>;
@group(0) @binding(9) var<storage, read_write> ko_acc: array<f32>;

fn ko_unit(v: vec4<f32>) -> bool {
    return hsk_finite4(v) && all(v >= vec4<f32>(0.0)) && all(v <= vec4<f32>(1.0));
}

// pigment blend_math::composite_over (unit form), the arithmetic of the composite kernel.
fn ko_over(mode: u32, b: vec4<f32>, s: vec4<f32>, opacity: f32) -> vec4<f32> {
    let a_s = s.a * opacity;
    let a_b = b.a;
    let a_o = a_s + a_b * (1.0 - a_s);
    if (a_o <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    var rgb: vec3<f32>;
    if (a_b == 0.0) {
        rgb = s.rgb;
    } else {
        let blended = blend(mode, b.rgb, s.rgb);
        let cs_prime = (1.0 - a_b) * s.rgb + a_b * blended;
        let co = a_s * cs_prime + (1.0 - a_s) * a_b * b.rgb;
        rgb = clamp(co / a_o, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return vec4<f32>(rgb, a_o);
}

// pigment knockout_math::knockout_union.
fn ko_union(k: f32, s: f32) -> f32 {
    return clamp(k + s * (1.0 - k), 0.0, 1.0);
}

@compute @workgroup_size(8, 8, 1)
fn knockout_s(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let di = (p.ry + gid.y) * p.chunk_w + (p.rx + gid.x);
    var s: vec4<f32>;
    if (p.kind == 0u) {
        s = p.solid;
    } else if (p.kind == 1u) {
        s = src[gid.y * p.rw + gid.x];
    } else {
        s = src[di];
    }
    var m: f32 = 1.0;
    if (p.has_mask != 0u) {
        m = mask[gid.y * p.rw + gid.x];
        if (p.density != 1.0) {
            m = clamp(1.0 - p.density * (1.0 - m), 0.0, 1.0);
        }
    }
    let shape = clamp(s.a * m * ko.layer_opacity, 0.0, 1.0);
    ko_k[di] = shape;
    if (shape <= 0.0) {
        return;
    }
    let initial = ko_s[di];
    if ((ko.flags & 4u) != 0u && (!ko_unit(s) || !ko_unit(initial))) {
        ko_s[di] = HSK_OVERFLOW;
        return;
    }
    ko_s[di] = ko_over(p.mode, initial, s, p.opacity);
}

@compute @workgroup_size(8, 8, 1)
fn knockout_mix(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let di = (p.ry + gid.y) * p.chunk_w + (p.rx + gid.x);
    let k = ko_k[di];
    if (k <= 0.0) {
        return;
    }
    let b = dst[di];
    let s = ko_s[di];
    if ((ko.flags & 4u) != 0u) {
        if (hsk_marked(b)) {
            return; // an overflow marker stays
        }
        if (hsk_marked(s) || !ko_unit(b)) {
            dst[di] = HSK_OVERFLOW;
            return;
        }
    }
    if ((ko.flags & 1u) != 0u) {
        ko_acc[di] = ko_union(ko_acc[di], k);
    }
    let a = b.a + (s.a - b.a) * k;
    if (a <= 0.0) {
        dst[di] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }
    let pr = b.rgb * b.a;
    let ps = s.rgb * s.a;
    let rgb = clamp((pr + (ps - pr) * k) / a, vec3<f32>(0.0), vec3<f32>(1.0));
    dst[di] = vec4<f32>(rgb, clamp(a, 0.0, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn knockout_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let di = (p.ry + gid.y) * p.chunk_w + (p.rx + gid.x);
    let k = ko_k[di];
    if (k <= 0.0) {
        return;
    }
    let b = dst[di];
    if ((ko.flags & 4u) == 0u || !hsk_marked(b)) {
        let a = b.a - b.a * k;
        if (a <= 0.0) {
            dst[di] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        } else {
            dst[di] = vec4<f32>(b.rgb, clamp(a, 0.0, 1.0));
        }
    }
    if ((ko.flags & 2u) != 0u) {
        ko_acc[di] = ko_union(ko_acc[di], k);
    }
}
