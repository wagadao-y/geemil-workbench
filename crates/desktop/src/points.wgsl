struct Camera { matrix: mat4x4<f32>, viewport: vec2<f32>, size: f32, padding: f32, tint: vec4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) depth: f32,
};
@vertex fn vertex(@builtin(vertex_index) id: u32, @location(0) position: vec3<f32>, @location(1) color: vec4<f32>) -> Out {
    let corners = array<vec2<f32>, 6>(vec2(-1.0,-1.0),vec2(1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,-1.0),vec2(1.0,1.0),vec2(-1.0,1.0));
    var out: Out;
    out.position = camera.matrix * vec4(position,1.0);
    out.position = vec4(out.position.xy + corners[id] * camera.size / camera.viewport * out.position.w, out.position.zw);
    out.corner = corners[id];
    // Mix the tint in sRGB, like the colours it replaces.
    out.color = vec4(mix(color.rgb, camera.tint.rgb, camera.tint.a), color.a);
    // View-space distance along the camera axis; screen-aligned splats keep it constant.
    out.depth = out.position.w;
    return out;
}
fn srgb_to_linear(srgb: vec3<f32>) -> vec3<f32> {
    let low = srgb / vec3<f32>(12.92);
    let high = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, srgb <= vec3<f32>(0.04045));
}
struct Target { @location(0) color: vec4<f32>, @location(1) depth: f32 };
@fragment fn fragment(in: Out) -> Target {
    if dot(in.corner,in.corner) > 1.0 { discard; }
    return Target(vec4(srgb_to_linear(in.color.rgb), in.color.a), in.depth);
}
