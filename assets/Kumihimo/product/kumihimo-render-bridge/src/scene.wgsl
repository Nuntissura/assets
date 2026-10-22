struct Camera { aspect: f32, selected: f32, padding: vec2<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
struct VertexOut { @builtin(position) position: vec4<f32>, @location(0) color: vec3<f32> }
@vertex fn scene_vertex(@builtin(vertex_index) index: u32) -> VertexOut {
    // Near green triangle is deliberately submitted before the far amber triangle.
    // The latter cannot obscure it when the depth attachment is functioning.
    let positions = array<vec3<f32>, 6>(
        vec3<f32>(-0.65,-0.55,2.0), vec3<f32>(0.65,-0.55,2.0), vec3<f32>(0.0,0.70,2.0),
        vec3<f32>(-1.1,-0.85,3.0), vec3<f32>(1.1,-0.85,3.0), vec3<f32>(0.0,1.0,3.0));
    let p = positions[index];
    var out: VertexOut;
    // w=z performs the perspective divide. z maps the positive viewing range to 0..1.
    out.position = vec4<f32>(p.x * 1.8 / camera.aspect, p.y * 1.8, (p.z-0.1)*10.0/9.9, p.z);
    out.color = select(vec3<f32>(0.96,0.57,0.10),
        select(vec3<f32>(0.10,0.85,0.40), vec3<f32>(0.10,0.55,0.98), camera.selected > 0.5), index < 3u);
    return out;
}
@fragment fn scene_fragment(in: VertexOut) -> @location(0) vec4<f32> { return vec4<f32>(in.color,1.0); }
@group(0) @binding(0) var scene_texture: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
struct QuadOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn quad_vertex(@builtin(vertex_index) index: u32) -> QuadOut {
    let positions = array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    let p = positions[index];
    var out: QuadOut;
    out.position=vec4<f32>(p,0.0,1.0);
    out.uv=vec2<f32>((p.x+1.0)*0.5,(1.0-p.y)*0.5);
    return out;
}
@fragment fn quad_fragment(in: QuadOut) -> @location(0) vec4<f32> { return textureSample(scene_texture,scene_sampler,in.uv); }
