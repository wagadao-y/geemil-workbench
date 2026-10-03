struct Camera {
    matrix: mat4x4<f32>, viewport: vec2<f32>, size: f32, padding: f32, tint: vec4<f32>,
    depth: vec4<f32>, clip: mat4x4<f32>, clip_on: f32, ramp_on: f32, pad_b: f32, pad_c: f32,
    // Height in the ramp's 0..1 range as dot(ramp, (position, 1)).
    ramp: vec4<f32>,
};
// Turbo colour map (Google), polynomial fit, sRGB output.
fn turbo(t: f32) -> vec3<f32> {
    let r = 0.13572138 + t * (4.61539260 + t * (-42.66032258 + t * (132.13108234 + t * (-152.94239396 + t * 59.28637943))));
    let g = 0.09140261 + t * (2.19418839 + t * (4.84296658 + t * (-14.18503333 + t * (4.27729857 + t * 2.82956604))));
    let b = 0.10667330 + t * (12.64194608 + t * (-60.58204836 + t * (110.36276771 + t * (-89.90310912 + t * 27.34824973))));
    return clamp(vec3(r, g, b), vec3(0.0), vec3(1.0));
}
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
    var base = color.rgb;
    if camera.ramp_on > 0.5 {
        base = turbo(clamp(dot(camera.ramp, vec4(position, 1.0)), 0.0, 1.0));
    }
    // Mix the tint in sRGB, like the colours it replaces.
    out.color = vec4(mix(base, camera.tint.rgb, camera.tint.a), color.a);
    // View-space distance along the camera axis; screen-aligned splats keep it
    // constant. 0 marks background, so keep drawn points above it.
    out.depth = max(dot(camera.depth, vec4(position, 1.0)), 1e-6);
    // Outside the clipping box: put the splat behind the near plane.
    if camera.clip_on > 0.5 && any(abs((camera.clip * vec4(position, 1.0)).xyz) > vec3(1.0)) {
        out.position = vec4(0.0, 0.0, -2.0, 1.0);
    }
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
