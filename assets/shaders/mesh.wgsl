// M4 mesh shader. Vertex has position + normal + uv. Camera uniform carries
// view_proj. Light uniform carries direction + ambient + the sun's shadow
// matrix. Texture + sampler in group 2. Per-vertex color is dropped — replaced
// by textured PBR-ish shading. Output is linear HDR; the tonemap pass maps it
// to the display.

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
    // World -> shadow map clip space (orthographic, from the sun).
    light_view_proj: mat4x4f,
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
    @location(2) world_pos: vec3f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<uniform> model: ModelUniform;
@group(0) @binding(1) var<uniform> light: LightUniform;
@group(0) @binding(2) var shadow_map: texture_depth_2d;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;
@group(2) @binding(0) var albedo_tex: texture_2d<f32>;
@group(2) @binding(1) var albedo_sampler: sampler;

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world_pos = model.model * vec4f(in.position, 1.0);
    out.clip_pos = camera.view_proj * world_pos;
    // Normal matrix transforms normals from object to world space. Use the
    // upper-left 3x3 (normals are directions, ignore translation).
    out.world_normal = (model.normal_matrix * vec4f(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    out.world_pos = world_pos.xyz;
    return out;
}

// 1.0 = fully lit, 0.0 = fully shadowed. 3x3 PCF over the sun's shadow map.
fn shadow_factor(world_pos: vec3f) -> f32 {
    let ls = light.light_view_proj * vec4f(world_pos, 1.0);
    let ndc = ls.xyz / ls.w;
    // Clip space y points up, texture v points down.
    let uv = ndc.xy * vec2f(0.5, -0.5) + 0.5;
    // Outside the shadow box counts as lit.
    if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) || ndc.z > 1.0) {
        return 1.0;
    }
    let texel = 1.0 / vec2f(textureDimensions(shadow_map));
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            // The Level variant is allowed in non-uniform control flow.
            sum += textureSampleCompareLevel(
                shadow_map, shadow_sampler, uv + vec2f(f32(x), f32(y)) * texel, ndc.z);
        }
    }
    return sum / 9.0;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let n = normalize(in.world_normal);
    let ndl = max(dot(n, light.direction), 0.0);
    let albedo = textureSample(albedo_tex, albedo_sampler, in.uv).rgb;

    let ambient = light.ambient * albedo;
    let diffuse = (1.0 - light.ambient) * ndl * albedo * light.color
        * shadow_factor(in.world_pos);

    return vec4f(ambient + diffuse, 1.0);
}
