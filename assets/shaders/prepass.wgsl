// Depth/normal prepass for SSAO: every mesh once, single-sample, before the
// 3D pass. Writes the geometric world normal (xyz) and the distance to the
// eye (w, for the depth-aware blur); depth goes to its own Depth32Float target.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

struct ModelUniform {
    model: mat4x4f,
    normal_matrix: mat4x4f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_normal: vec3f,
    @location(1) world_pos: vec3f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> model: ModelUniform;

@vertex
fn vs_main(@location(0) position: vec3f, @location(1) normal: vec3f) -> VsOut {
    var out: VsOut;
    let world_pos = model.model * vec4f(position, 1.0);
    out.clip_pos = camera.view_proj * world_pos;
    out.world_normal = (model.normal_matrix * vec4f(normal, 0.0)).xyz;
    out.world_pos = world_pos.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    return vec4f(normalize(in.world_normal), distance(in.world_pos, camera.eye));
}
