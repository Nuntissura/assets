// The kumihimo-ui display-list pass of hsk-studio-vulkan (SMB-VK-UI-PASS-1, decision O3: compiled offline by naga
// 27.0.3 in crates/hsk-studio-vulkan/spv-build; the SPIR-V is checked in). One instanced quad per primitive piece, 4
// vertices as a triangle strip, no index buffer. The recorder computes every per-primitive value on the CPU with the
// CPU painter's own expressions (kumihimo-ui paint.rs); this shader does per-pixel work only, in the painter's f32
// order. Coverage is quantized as the painter's cov(): floor(v * 255 + 0.5), never round() (WGSL rounds halfway to
// even). Blending is fixed-function "over" in encoded space on an R8G8B8A8_UNORM pane; textures are read with
// textureLoad (integer texel, no sampler, no filter).

struct Pc {
    // Pane extent in physical px.
    size: vec2<f32>,
}
var<push_constant> pc: Pc;

@group(0) @binding(0) var glyph_page: texture_2d<f32>;
@group(0) @binding(1) var icon_page: texture_2d<f32>;
@group(0) @binding(2) var image_tex: texture_2d<f32>;

const K_FLAT: u32 = 0u;
const K_CHECKER: u32 = 1u;
const K_IMAGE: u32 = 2u;
const K_GLYPH: u32 = 3u;
const K_ICON: u32 = 4u;
const K_RRECT: u32 = 5u;
const K_LINE: u32 = 6u;
const K_DASH: u32 = 7u;
const K_SHADOW: u32 = 8u;
const K_RAMP: u32 = 9u;

struct Inst {
    @location(0) quad: vec4<i32>,
    @location(1) kind_sel: vec2<u32>,
    @location(2) org: vec2<i32>,
    @location(3) colors: vec4<u32>,
    @location(4) p0: vec4<f32>,
    @location(5) p1: vec4<f32>,
}

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) kind_sel: vec2<u32>,
    @location(1) @interpolate(flat) org: vec2<i32>,
    @location(2) @interpolate(flat) colors: vec4<u32>,
    @location(3) @interpolate(flat) p0: vec4<f32>,
    @location(4) @interpolate(flat) p1: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, i: Inst) -> VOut {
    let x = select(i.quad.x, i.quad.z, (vi & 1u) == 1u);
    let y = select(i.quad.y, i.quad.w, (vi & 2u) == 2u);
    // WebGPU clip space (y up); the writer's ADJUST_COORDINATE_SPACE flips y for Vulkan. Built with a constructor, not
    // a function-scope variable (spirv-val VUID-StandaloneSpirv-None-10684 on an explicitly laid out struct).
    let pos = vec4<f32>(f32(x) / pc.size.x * 2.0 - 1.0, 1.0 - f32(y) / pc.size.y * 2.0, 0.0, 1.0);
    return VOut(pos, i.kind_sel, i.org, i.colors, i.p0, i.p1);
}

fn rgba(c: u32) -> vec4<f32> {
    return unpack4x8unorm(c);
}

// The painter's clamp01 then cov(): an integer coverage 0..=255, as a fraction.
fn quant(v: f32) -> f32 {
    return floor(clamp(v, 0.0, 1.0) * 255.0 + 0.5) / 255.0;
}

fn over(c: vec4<f32>, cov: f32) -> vec4<f32> {
    return vec4<f32>(c.rgb, c.a * cov);
}

// The painter's rbox: signed distance from p to a rounded box of half extents h, centre c, radius r0.
fn rbox(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r0: f32) -> f32 {
    let r = max(min(min(r0, hx), hy), 0.0);
    let qx = abs(px - cx) - (hx - r);
    let qy = abs(py - cy) - (hy - r);
    let ox = max(qx, 0.0);
    let oy = max(qy, 0.0);
    return sqrt(ox * ox + oy * oy) + min(max(qx, qy), 0.0) - r;
}

// f32::rem_euclid: r = x % y; r < 0 adds |y|.
fn rem_euclid(x: f32, y: f32) -> f32 {
    let r = x % y;
    return select(r, r + abs(y), r < 0.0);
}

fn channel(c: u32, shift: u32) -> f32 {
    return f32((c >> shift) & 0xffu);
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    // The pixel the painter writes: centre at x + 0.5.
    let p = vec2<i32>(floor(v.pos.xy));
    let px = f32(p.x) + 0.5;
    let py = f32(p.y) + 0.5;
    let kind = v.kind_sel.x;
    let sel = v.kind_sel.y;
    let c0 = rgba(v.colors.x);
    switch kind {
        case 0u: {
            return over(c0, 1.0);
        }
        case 1u: {
            // Checker: cell parity from the integer pixel; org = snapped rect origin, sel = cell.
            let cell = i32(sel);
            let odd = (((p.x - v.org.x) / cell) + ((p.y - v.org.y) / cell)) % 2 == 1;
            return over(select(c0, rgba(v.colors.y), odd), 1.0);
        }
        case 2u: {
            // Image: colors = (pw, ph, iw, ih); the painter's integer pixel-to-texel map.
            let pw = i32(v.colors.x);
            let ph = i32(v.colors.y);
            let iw = i32(v.colors.z);
            let ih = i32(v.colors.w);
            let sx = ((p.x - v.org.x) * iw) / pw;
            let sy = ((p.y - v.org.y) * ih) / ph;
            return over(textureLoad(image_tex, vec2<i32>(sx, sy), 0), 1.0);
        }
        case 3u: {
            // Glyph: the page holds lut[c], applied at upload.
            let t = textureLoad(glyph_page, p - v.org, 0);
            return over(c0, t.r);
        }
        case 4u: {
            // Icon: sel 0 form (R), 1 hot point (G); two instances, the painter's two blends.
            let t = textureLoad(icon_page, p - v.org, 0);
            return over(c0, select(t.r, t.g, sel == 1u));
        }
        case 5u: {
            // RRect: p0 = centre, half extents; p1.x radius, p1.y border width. sel 0 fill (no border), 1 border,
            // 2 fill inside a border.
            let d = rbox(px, py, v.p0.x, v.p0.y, v.p0.z, v.p0.w, v.p1.x);
            let outer = clamp(0.5 - d, 0.0, 1.0);
            if outer <= 0.0 {
                discard;
            }
            if sel == 0u {
                return over(c0, quant(outer));
            }
            let w = v.p1.y;
            let din = rbox(px, py, v.p0.x, v.p0.y, v.p0.z - w, v.p0.w - w, max(v.p1.x - w, 0.0));
            let inner = clamp(0.5 - din, 0.0, 1.0);
            if sel == 1u {
                return over(c0, quant(outer - inner));
            }
            return over(c0, quant(inner));
        }
        case 6u, 7u: {
            // Line and dash: p0 = a, b (physical); p1 = half width, len2, len; dash: colors.y off, p1.w phase,
            // sel bit 0 = has off colour; the period and on length ride in org as f32 bits.
            let ax = v.p0.x;
            let ay = v.p0.y;
            let dx = v.p0.z - ax;
            let dy = v.p0.w - ay;
            let len2 = v.p1.y;
            var t = 0.0;
            if len2 > 0.0 {
                t = clamp(((px - ax) * dx + (py - ay) * dy) / len2, 0.0, 1.0);
            }
            let qx = ax + dx * t - px;
            let qy = ay + dy * t - py;
            let d = sqrt(qx * qx + qy * qy);
            let cov = clamp(v.p1.x + 0.5 - d, 0.0, 1.0);
            if cov <= 0.0 {
                discard;
            }
            if kind == K_LINE {
                return over(c0, quant(cov));
            }
            let period = bitcast<f32>(v.org.x);
            let on_len = bitcast<f32>(v.org.y);
            if rem_euclid(t * v.p1.z + v.p1.w, period) < on_len {
                return over(c0, quant(cov));
            }
            if (sel & 1u) == 0u {
                discard;
            }
            return over(rgba(v.colors.y), quant(cov));
        }
        case 8u: {
            // Shadow: the painter's analytic smoothstep; p0 = centre, half extents; p1.x radius * s, p1.y blur.
            let d = rbox(px, py, v.p0.x, v.p0.y, v.p0.z, v.p0.w, v.p1.x);
            let t = clamp(0.5 - d / v.p1.y, 0.0, 1.0);
            let k = t * t * (3.0 - 2.0 * t);
            return over(c0, quant(k));
        }
        case 9u: {
            // Ramp: org = snapped origin, p0.xy = the painter's denominators; colors = the four corners.
            let u = f32(p.x - v.org.x) / v.p0.x;
            let w = f32(p.y - v.org.y) / v.p0.y;
            var ramp = vec4<f32>(0.0);
            for (var k = 0u; k < 4u; k++) {
                let s = k * 8u;
                let top = channel(v.colors.x, s) * (1.0 - u) + channel(v.colors.y, s) * u;
                let bot = channel(v.colors.z, s) * (1.0 - u) + channel(v.colors.w, s) * u;
                ramp[k] = clamp(floor(top * (1.0 - w) + bot * w + 0.5), 0.0, 255.0) / 255.0;
            }
            return ramp;
        }
        default: {
            discard;
        }
    }
    return vec4<f32>(0.0);
}
