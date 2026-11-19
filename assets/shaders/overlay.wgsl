// Overlay shapes (gizmos, editor icons): unit meshes placed per instance,
// lit by a "headlight" from the camera, in exact display colours. The target
// is an sRGB view, so the colour is converted to linear here and the hardware
// encodes it back. Output alpha blends as premultiplied in the overlay target.

struct Camera {
    view_proj: mat4x4f,
    eye: vec4f,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @location(2) model_0: vec4f,
    @location(3) model_1: vec4f,
    @location(4) model_2: vec4f,
    @location(5) model_3: vec4f,
    @location(6) normal_0: vec4f,
    @location(7) normal_1: vec4f,
    @location(8) normal_2: vec4f,
    @location(9) color: vec4f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_pos: vec3f,
    @location(1) normal: vec3f,
    @location(2) color: vec4f,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let model = mat4x4f(in.model_0, in.model_1, in.model_2, in.model_3);
    let normal_matrix = mat3x3f(in.normal_0.xyz, in.normal_1.xyz, in.normal_2.xyz);
    let world = model * vec4f(in.position, 1.0);
    var out: VsOut;
    out.clip_pos = camera.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = normal_matrix * in.normal;
    out.color = in.color;
    return out;
}

fn srgb_to_linear(c: vec3f) -> vec3f {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3f(2.4));
    return select(high, low, c <= vec3f(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let n = normalize(in.normal);
    let to_eye = normalize(camera.eye.xyz - in.world_pos);
    // Faces are drawn without culling, so light both sides alike.
    let light = 0.45 + 0.55 * abs(dot(n, to_eye));
    return vec4f(srgb_to_linear(in.color.rgb) * light, in.color.a);
}
