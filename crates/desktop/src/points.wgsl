struct Camera { matrix: mat4x4<f32>, viewport: vec2<f32>, size: f32, padding: f32 };
@group(0) @binding(0) var<uniform> camera: Camera;
struct Out { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32>, @location(1) corner: vec2<f32> };
@vertex fn vertex(@builtin(vertex_index) id: u32, @location(0) position: vec3<f32>, @location(1) color: vec4<f32>) -> Out {
    let corners = array<vec2<f32>, 6>(vec2(-1.0,-1.0),vec2(1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,1.0));
    var out: Out;
    out.position = camera.matrix * vec4(position,1.0);
    out.position = vec4(out.position.xy + corners[id] * camera.size / camera.viewport * out.position.w, out.position.zw);
    out.corner = corners[id]; out.color = color;
    return out;
}
fn srgb_to_linear(srgb: vec3<f32>) -> vec3<f32> {
    let low = srgb / vec3<f32>(12.92);
    let high = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, srgb <= vec3<f32>(0.04045));
}
@fragment fn fragment(in: Out) -> @location(0) vec4<f32> {
    if dot(in.corner,in.corner) > 1.0 { discard; }
    return vec4(srgb_to_linear(in.color.rgb), in.color.a);
}
