// Shadow pass: depth-only render of every mesh from the sun. No fragment stage.

struct LightCamera {
    view_proj: mat4x4f,
};

// One record per drawn instance (group 1), indexed by instance_index.
struct Instance {
    model: mat4x4f,
    normal_matrix: mat4x4f,
};

@group(0) @binding(0) var<uniform> light_camera: LightCamera;
@group(1) @binding(0) var<storage, read> instances: array<Instance>;

@vertex
fn vs_main(
    @location(0) position: vec3f,
    @builtin(instance_index) instance: u32,
) -> @builtin(position) vec4f {
    return light_camera.view_proj * instances[instance].model * vec4f(position, 1.0);
}

// Alpha-masked casters: the same depth render, discarding fragments whose
// base colour alpha is below the cutoff of the material (group 2).
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
    @location(0) uv: vec2f,
};

@vertex
fn vs_mask(
    @location(0) position: vec3f,
    @location(2) uv: vec2f,
    @builtin(instance_index) instance: u32,
) -> MaskOut {
    var out: MaskOut;
    out.clip_pos = light_camera.view_proj * instances[instance].model * vec4f(position, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_mask(in: MaskOut) {
    let alpha = textureSample(base_color_tex, material_sampler, in.uv).a * material.base_color.a;
    if (alpha < material.alpha_cutoff) {
        discard;
    }
}
