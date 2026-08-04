// M4 mesh shader. Vertex has position + normal + uv. Camera uniform carries
// view_proj. Light uniform carries direction + ambient. Texture + sampler in
// group 2. Per-vertex color is dropped — replaced by textured PBR-ish shading.

struct CameraUniform {
    view_proj: mat4x4f,
};

struct ModelUniform {
    model: mat4x4f,
    normal_matrix: mat4x4f,
};

struct LightUniform {
    // Direction TOWARD the light (world space, normalized on CPU).
    direction: vec3f,
    // Ambient intensity (0..1).
    ambient: f32,
    // Light color (linear, 0..1).
    color: vec3f,
};

struct VsIn {
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @location(2) uv: vec2f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_normal: vec3f,
    @location(1) uv: vec2f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> model: ModelUniform;
@group(2) @binding(0) var<uniform> light: LightUniform;
@group(3) @binding(0) var albedo_tex: texture_2d<f32>;
@group(3) @binding(1) var albedo_sampler: sampler;

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world_pos = model.model * vec4f(in.position, 1.0);
    out.clip_pos = camera.view_proj * world_pos;
    // Normal matrix transforms normals from object to world space. Use the
    // upper-left 3x3 (normals are directions, ignore translation).
    out.world_normal = (model.normal_matrix * vec4f(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let n = normalize(in.world_normal);
    let ndl = max(dot(n, light.direction), 0.0);
    let albedo = textureSample(albedo_tex, albedo_sampler, in.uv).rgb;

    let ambient = light.ambient * albedo;
    let diffuse = (1.0 - light.ambient) * ndl * albedo * light.color;

    return vec4f(ambient + diffuse, 1.0);
}