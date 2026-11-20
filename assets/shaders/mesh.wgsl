// Mesh shader: glTF metallic-roughness PBR. Vertex has position + normal +
// uv + tangent. Group 0: camera, light (sun + sky/ground hemisphere + shadow
// matrix), shadow map, environment. Group 2: the material (base colour,
// metallic-roughness, normal and emissive textures, one sampler, factors).
// Group 3: per-view lighting inputs (the SSAO texture).
// Sun: Cook-Torrance (GGX, height-correlated Smith, Schlick Fresnel), shadowed.
// Ambient: hemisphere diffuse plus a sky-gradient specular, both occluded by
// SSAO (never the sun). Emissive is added,
// then exponential distance fog fades toward the horizon colour. Output is
// linear HDR; the tonemap pass maps it to the display.

struct CameraUniform {
    view_proj: mat4x4f,
    inv_view_proj: mat4x4f,
    eye: vec3f,
};

struct EnvironmentUniform {
    zenith_color: vec3f,
    // Exponential fog per metre; the fog colour is the horizon colour.
    fog_density: f32,
    horizon_color: vec3f,
};

// One record per drawn instance (group 1), indexed by instance_index.
struct Instance {
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
    _pad: f32,
    // Ambient from above / below (linear RGB), blended by the world normal.
    sky_color: vec3f,
    _pad1: f32,
    ground_color: vec3f,
    _pad2: f32,
    // World -> shadow map clip space (orthographic, from the sun).
    light_view_proj: mat4x4f,
};

struct MaterialUniform {
    base_color: vec4f,
    emissive: vec3f,
    metallic: f32,
    roughness: f32,
    normal_scale: f32,
};

struct VsIn {
    @location(0) position: vec3f,
    @location(1) normal: vec3f,
    @location(2) uv: vec2f,
    // xyz along +U, w = bitangent sign.
    @location(3) tangent: vec4f,
};

struct VsOut {
    @builtin(position) clip_pos: vec4f,
    @location(0) world_normal: vec3f,
    @location(1) uv: vec2f,
    @location(2) world_pos: vec3f,
    @location(3) world_tangent: vec4f,
};

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(0) @binding(1) var<uniform> light: LightUniform;
@group(0) @binding(2) var shadow_map: texture_depth_2d;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;
@group(0) @binding(4) var<uniform> env: EnvironmentUniform;
@group(2) @binding(0) var base_color_tex: texture_2d<f32>;
@group(2) @binding(1) var material_sampler: sampler;
// G = roughness, B = metallic (linear).
@group(2) @binding(2) var metallic_roughness_tex: texture_2d<f32>;
@group(2) @binding(3) var normal_tex: texture_2d<f32>;
@group(2) @binding(4) var emissive_tex: texture_2d<f32>;
@group(2) @binding(5) var<uniform> material: MaterialUniform;
// Ambient occlusion per screen pixel (R, 1 = open); 1x1 white when off.
@group(3) @binding(0) var ao_tex: texture_2d<f32>;

const PI: f32 = 3.14159265;

@vertex
fn vs_main(in: VsIn, @builtin(instance_index) instance: u32) -> VsOut {
    let model = instances[instance];
    var out: VsOut;
    let world_pos = model.model * vec4f(in.position, 1.0);
    out.clip_pos = camera.view_proj * world_pos;
    // Normal matrix transforms normals from object to world space. Use the
    // upper-left 3x3 (normals are directions, ignore translation).
    out.world_normal = (model.normal_matrix * vec4f(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    out.world_pos = world_pos.xyz;
    // Tangents follow the surface: the model matrix itself. A mirroring
    // matrix flips the bitangent.
    let m = mat3x3f(model.model[0].xyz, model.model[1].xyz, model.model[2].xyz);
    out.world_tangent = vec4f(m * in.tangent.xyz, in.tangent.w * sign(determinant(m)));
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

// Sky radiance along `dir`, as sky.wgsl draws it (without the sun disc):
// horizon-to-zenith above, the dim hemisphere ground below.
fn sky_radiance(dir: vec3f) -> vec3f {
    let above = mix(env.horizon_color, env.zenith_color, sqrt(max(dir.y, 0.0)));
    let ground = light.ambient * light.ground_color;
    let below = mix(env.horizon_color, ground, smoothstep(0.0, 0.3, -dir.y));
    return select(below, above, dir.y >= 0.0);
}

fn fresnel_schlick(cos_theta: f32, f0: vec3f) -> vec3f {
    return f0 + (1.0 - f0) * pow(1.0 - cos_theta, 5.0);
}

// Schlick with roughness (Lagarde): rough surfaces brighten less at grazing angles.
fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3f, roughness: f32) -> vec3f {
    return f0 + (max(vec3f(1.0 - roughness), f0) - f0) * pow(1.0 - cos_theta, 5.0);
}

// GGX / Trowbridge-Reitz normal distribution, alpha = roughness^2.
fn distribution_ggx(ndh: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

// Height-correlated Smith-GGX visibility: G / (4 n.l n.v).
fn visibility_smith_ggx(ndv: f32, ndl: f32, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let gv = ndl * sqrt(ndv * ndv * (1.0 - a2) + a2);
    let gl = ndv * sqrt(ndl * ndl * (1.0 - a2) + a2);
    return 0.5 / max(gv + gl, 1e-5);
}

// SSAO at framebuffer position `frag`; clamped so the 1x1 white fallback works.
fn ambient_occlusion(frag: vec2f) -> f32 {
    let px = min(vec2u(frag), textureDimensions(ao_tex) - 1u);
    return textureLoad(ao_tex, px, 0).r;
}

// Specular occlusion from AO (Lagarde 2014): exactly 1 when ao is 1.
fn specular_occlusion(ndv: f32, ao: f32, roughness: f32) -> f32 {
    return clamp(pow(ndv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao, 0.0, 1.0);
}

// World normal: the vertex normal bent by the tangent-space normal map.
fn surface_normal(in: VsOut) -> vec3f {
    let n = normalize(in.world_normal);
    let t = normalize(in.world_tangent.xyz - n * dot(n, in.world_tangent.xyz));
    let b = cross(n, t) * in.world_tangent.w;
    var tn = textureSample(normal_tex, material_sampler, in.uv).xyz * 2.0 - 1.0;
    tn = vec3f(tn.xy * material.normal_scale, tn.z);
    return normalize(mat3x3f(t, b, n) * tn);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let n = surface_normal(in);
    let v = normalize(camera.eye - in.world_pos);
    let l = light.direction;
    let h = normalize(l + v);
    let ndl = max(dot(n, l), 0.0);
    // Back-facing normals (normal maps, silhouettes) would divide by zero.
    let ndv = max(dot(n, v), 1e-4);
    let ndh = max(dot(n, h), 0.0);

    let base = (textureSample(base_color_tex, material_sampler, in.uv) * material.base_color).rgb;
    let mr = textureSample(metallic_roughness_tex, material_sampler, in.uv);
    let metallic = clamp(mr.b * material.metallic, 0.0, 1.0);
    // Floor keeps GGX's highlight finite on a perfect mirror.
    let roughness = clamp(mr.g * material.roughness, 0.045, 1.0);
    let alpha = roughness * roughness;
    let f0 = mix(vec3f(0.04), base, metallic);

    // Sun. `light.color` is scaled so a white Lambert surface facing the sun
    // shows `(1 - ambient) * color`: radiance = PI * color; `ambient` splits
    // the light between sun and sky as before.
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
    let specular = distribution_ggx(ndh, alpha) * visibility_smith_ggx(ndv, ndl, alpha) * f;
    let diffuse = (1.0 - f) * (1.0 - metallic) * base / PI;
    let sun = (diffuse + specular) * PI * (1.0 - light.ambient) * light.color * ndl
        * shadow_factor(in.world_pos);

    // Ambient. Diffuse: the sky/ground hemisphere. Specular: the sky in the
    // mirror direction, fading toward the hemisphere around the normal as
    // roughness blurs the reflection. SSAO occludes both.
    let ao = ambient_occlusion(in.clip_pos.xy);
    let hemi = light.ambient * mix(light.ground_color, light.sky_color, n.y * 0.5 + 0.5);
    let f_amb = fresnel_schlick_roughness(ndv, f0, roughness);
    let reflected = mix(sky_radiance(reflect(-v, n)), hemi, roughness);
    let ambient = (1.0 - f_amb) * (1.0 - metallic) * base * hemi * ao
        + f_amb * reflected * specular_occlusion(ndv, ao, roughness);

    let emissive = (textureSample(emissive_tex, material_sampler, in.uv).rgb * material.emissive);
    let color = sun + ambient + emissive;

    let fog = 1.0 - exp(-env.fog_density * distance(in.world_pos, camera.eye));
    return vec4f(mix(color, env.horizon_color, fog), 1.0);
}
