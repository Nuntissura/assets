// One layer composited into a chunk destination: W3C Compositing 1 straight-alpha source-over
// with blend (s5.1, s9.1.4), the same formula as hsk_studio_pigment::blend_math::composite_over.
// Colours are straight RGB in [0,1] (the host refuses anything else), destinations are RGBA f32
// chunk buffers that start transparent. One invocation owns one destination pixel, so the
// in-place read-modify-write is race free. Sources arrive already in the plan's blending space
// (STU-COL-163).
//
// This text is a TEMPLATE: kernel.rs instantiates it twice, as the entry point `composite` (the v1
// entry point, which reads only bindings 0-3) and as `composite_ext` (which adds the Blend-If
// weight of the plan-extension envelope at the marker line below and reads bindings 0-4). Two
// instances keep the v1 pipeline layout and every v1 byte unchanged.
@compute @workgroup_size(8, 8, 1)
fn @ENTRY@(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.rw || gid.y >= p.rh) {
        return;
    }
    let di = (p.ry + gid.y) * p.chunk_w + (p.rx + gid.x);
    let extended = (p.flags & 4u) != 0u;
    if (extended && dst[di].a > 1.5) {
        return; // an overflow marker stays
    }
    var s: vec4<f32>;
    if (p.kind == 0u) {
        s = p.solid;
    } else if (p.kind == 1u) {
        s = src[gid.y * p.rw + gid.x];
    } else {
        s = src[di];
    }
    if (extended && hsk_marked(s)) {
        // A group result, frame or envelope pass output that overflowed (marker or non-finite).
        dst[di] = HSK_OVERFLOW;
        return;
    }
    var m: f32 = 1.0;
    if (p.has_mask != 0u) {
        m = mask[gid.y * p.rw + gid.x];
        // coverage_math::mask_coverage density step (inversion is applied on the host before upload);
        // exactly 1.0 is the plain mask and leaves the host-converted coverage bit-for-bit.
        if (p.density != 1.0) {
            m = clamp(1.0 - p.density * (1.0 - m), 0.0, 1.0);
        }
    }
    if ((p.flags & 1u) != 0u) {
        // Base of a clipping run: G := the base content with its own mask applied (no opacity,
        // fill or mode). G was cleared, so a transparent texel is left alone.
        let a_base = s.a * m;
        if (a_base > 0.0) {
            dst[di] = vec4<f32>(s.rgb, a_base);
        }
        return;
    }
    let b = dst[di];
    let a_b = b.a;
    let clip = (p.flags & 2u) != 0u;
    // Clipped layer: its source alpha is multiplied by the base alpha, which is the alpha G holds
    // (G's alpha is held at the base alpha after every clipped composite).
    var s_a = s.a;
    if (clip) {
        s_a = s_a * a_b;
    }
    var opacity = p.opacity * m;
    //@BLEND_IF@
    if (opacity <= 0.0 || s_a <= 0.0) {
        return;
    }
    let a_s = s_a * opacity;
    let blended = blend(p.mode, b.rgb, s.rgb);
    let a_o = a_s + a_b * (1.0 - a_s);
    if (a_o <= 0.0) {
        dst[di] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }
    var rgb: vec3<f32>;
    if (a_b == 0.0) {
        // Nothing underneath: the result colour is the source colour. Taken directly instead of as
        // (a_s * cs) / a_s so it never depends on the division unit's last bit (GPU division is
        // allowed 1-2.5 ulp: 1.0 / 1.0 style quotients can come out one ulp low, which flips the
        // discontinuous colour burn/dodge cases at exact 0.0 and 1.0).
        rgb = s.rgb;
    } else {
        let cs_prime = (1.0 - a_b) * s.rgb + a_b * blended;
        let co = a_s * cs_prime + (1.0 - a_s) * a_b * b.rgb;
        if (extended) {
            // Extended-range output stage: no unit clamp (pigment composite_over_extended).
            rgb = co / a_o;
            if (!hsk_finite3(rgb)) {
                dst[di] = HSK_OVERFLOW;
                return;
            }
        } else {
            rgb = clamp(co / a_o, vec3<f32>(0.0), vec3<f32>(1.0));
        }
    }
    var out_a = min(a_o, 1.0);
    if (clip) {
        out_a = a_b; // clipped content never extends beyond the base
    }
    dst[di] = vec4<f32>(rgb, out_a);
}
