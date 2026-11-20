// Depth/normal prepass for SSAO: every mesh once, single-sample, before the
// 3D pass. Writes the geometric world normal (xyz) and the distance to the
// eye (w, for the depth-aware blur); depth goes to its own Depth32Float target.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

// One record per drawn instance (group 1), indexed by instance_index.
struct Instance {
    model: mat4x4f,
    normal_matrix: mat4x4f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_normal: vec3f,
    @location(1) world_pos: vec3f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<storage, read> instances: array<Instance>;

@vertex
fn vs_main(
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @builtin(instance_index) instance: u32,
) -> VsOut {
    let model = instances[instance];
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

// Alpha-masked meshes: the same normal + distance output, discarding
// fragments whose base colour alpha is below the cutoff of the material
// (group 2).
struct MaterialUniform {
    base_color: vec4f,
    emissive: vec3f,
    metallic: f32,
    roughness: f32,
    normal_scale: f32,
    alpha_cutoff: f32,
};

@group(2) @binding(0) var base_color_tex: texture_2d<f32>;
@group(2) @binding(1) var material_sampler: sampler;
@group(2) @binding(5) var<uniform> material: MaterialUniform;

struct MaskOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_normal: vec3f,
    @location(1) world_pos: vec3f,
    @location(2) uv: vec2f,
};

@vertex
fn vs_mask(
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @location(2) uv: vec2f,
    @builtin(instance_index) instance: u32,
) -> MaskOut {
    let model = instances[instance];
    var out: MaskOut;
    let world_pos = model.model * vec4f(position, 1.0);
    out.clip_pos = camera.view_proj * world_pos;
    out.world_normal = (model.normal_matrix * vec4f(normal, 0.0)).xyz;
    out.world_pos = world_pos.xyz;
    out.uv = uv;
    return out;
}

@fragment
fn fs_mask(in: MaskOut) -> @location(0) vec4f {
    let alpha = textureSample(base_color_tex, material_sampler, in.uv).a * material.base_color.a;
    if (alpha < material.alpha_cutoff) {
        discard;
    }
    return vec4f(normalize(in.world_normal), distance(in.world_pos, camera.eye));
}
