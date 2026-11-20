// Mesh shader: glTF metallic-roughness PBR. Vertex has position + normal +
// uv + tangent. Group 0: camera, light (sun + sky/ground hemisphere + shadow
// matrix), shadow map, environment. Group 2: the material (base colour,
// metallic-roughness, normal and emissive textures, one sampler, factors).
// Group 3: per-view lighting inputs (the SSAO texture) and every GI
// backend's bindings. gi_common.wgsl, ddgi_common.wgsl and the backends'
// shading (gi_ddgi.wgsl, gi_ssgi.wgsl) are prepended.
// Sun: Cook-Torrance (GGX, height-correlated Smith, Schlick Fresnel), shadowed.
// Ambient: diffuse from this pipeline's GI mode (`indirect_diffuse`; the
// hemisphere with GI off) plus a sky-gradient specular, both occluded by
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
    // World -> shadow map clip space (orthographic, from the sun): cascade 0.
    light_view_proj: mat4x4f,
    // Every cascade's matrix, the far view depth of each, (count, shadow
    // distance) and the camera axis the depths are measured along.
    cascade_view_proj: array<mat4x4f, 4>,
    cascade_far: vec4f,
    cascade_info: vec4f,
    cascade_forward: vec4f,
};

struct MaterialUniform {
    base_color: vec4f,
    emissive: vec3f,
    metallic: f32,
    roughness: f32,
    normal_scale: f32,
    // Alpha mask: fragments below it are discarded; 0 = no mask.
    alpha_cutoff: f32,
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
@group(0) @binding(2) var shadow_map: texture_depth_2d_array;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;
@group(0) @binding(4) var<uniform> env: EnvironmentUniform;
// Image-based ambient (ibl.rs): irradiance / pi as SH9, the environment
// prefiltered per roughness (equirect mips) and the split-sum BRDF lookup.
struct IblUniform {
    sh: array<vec4f, 9>,
    // x highest mip.
    params: vec4f,
};
@group(0) @binding(7) var<uniform> ibl: IblUniform;
@group(0) @binding(8) var ibl_specular: texture_2d<f32>;
@group(0) @binding(9) var ibl_sampler: sampler;
@group(0) @binding(10) var ibl_brdf: texture_2d<f32>;
@group(2) @binding(0) var base_color_tex: texture_2d<f32>;
@group(2) @binding(1) var material_sampler: sampler;
// G = roughness, B = metallic (linear).
@group(2) @binding(2) var metallic_roughness_tex: texture_2d<f32>;
@group(2) @binding(3) var normal_tex: texture_2d<f32>;
@group(2) @binding(4) var emissive_tex: texture_2d<f32>;
@group(2) @binding(5) var<uniform> material: MaterialUniform;
// Ambient occlusion per screen pixel (R, 1 = open); 1x1 white when off.
@group(3) @binding(0) var ao_tex: texture_2d<f32>;
// Point and spot lights, clustered forward (see lights.rs): the lights, the
// (offset, count) of each cluster's list, the lists, and the cluster grid.
struct LocalLight {
    // xyz position, w range.
    position_range: vec4f,
    // Linear colour times intensity.
    color: vec4f,
    // xyz spot axis.
    direction: vec4f,
    // x spot scale, y spot offset, z 1 for a spot, w shadow slot.
    spot: vec4f,
};

struct ClusterUniform {
    screen: vec2f,
    near: f32,
    depth_scale: f32,
    dims: vec4u,
    forward: vec4f,
};

// Point / spot shadows (local_shadows.rs): the depth atlas and one record
// per tile (view-projection, atlas rect, texel size per metre).
struct ShadowView {
    view_proj: mat4x4f,
    rect: vec4f,
    params: vec4f,
};
@group(0) @binding(11) var local_shadow_atlas: texture_depth_2d;
@group(0) @binding(12) var<storage, read> shadow_views: array<ShadowView>;

@group(3) @binding(9) var<storage, read> local_lights: array<LocalLight>;
@group(3) @binding(10) var<storage, read> cluster_ranges: array<vec2u>;
@group(3) @binding(11) var<storage, read> cluster_indices: array<u32>;
@group(3) @binding(12) var<uniform> cluster: ClusterUniform;

// Voxel passes only (their own group 0): the voxel radiance volume (see
// VOXEL_SCALE) and the projection axis (x: 0 = X, 1 = Y, 2 = Z).
@group(0) @binding(5) var<storage, read_write> voxels: array<atomic<u32>>;
@group(0) @binding(6) var<uniform> voxel_axis: vec4u;

const PI: f32 = 3.14159265;
// GI mode of this mesh pipeline variant (gi/mod.rs `shader_mode`): each
// variant compiles only its own `indirect_diffuse` path.
const GI_OFF: u32 = 0u;
const GI_SSGI: u32 = 1u;
const GI_DDGI: u32 = 2u;
override GI_MODE: u32 = GI_DDGI;
// Alpha-blended variant: the fragment's alpha goes to the target (the opaque
// variants write 1).
override ALPHA_BLEND: bool = false;

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

// 3x3 PCF over cascade `i` of the sun's shadow map: x = lit factor (1.0 =
// fully lit, 0.0 = fully shadowed; 1.0 outside the cascade's box), y = how
// far out in the box the point is (1.0 at the edge). `normal` is the
// geometric world normal: the lookup moves along it by about 1.5 shadow
// texels, more as the sun grazes the surface (normal-offset sampling), so
// lit faces do not shadow themselves.
fn cascade_shadow(i: u32, world_pos: vec3f, normal: vec3f) -> vec2f {
    let m = light.cascade_view_proj[i];
    // The ortho box maps 2 * radius world units onto the map's width.
    let texel_world = 2.0 / (length(vec3f(m[0].x, m[1].x, m[2].x))
        * f32(textureDimensions(shadow_map).x));
    let ndl = clamp(dot(normal, light.direction), 0.0, 1.0);
    let offset = normal * (1.5 * texel_world * sqrt(1.0 - ndl * ndl));
    let ls = m * vec4f(world_pos + offset, 1.0);
    let ndc = ls.xyz / ls.w;
    // Clip space y points up, texture v points down.
    let uv = ndc.xy * vec2f(0.5, -0.5) + 0.5;
    let edge = max(abs(ndc.x), abs(ndc.y));
    // Outside the shadow box counts as lit.
    if (edge > 1.0 || ndc.z > 1.0 || ndc.z < 0.0) {
        return vec2f(1.0, max(edge, 1.001));
    }
    let texel = 1.0 / vec2f(textureDimensions(shadow_map));
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            // The Level variant is allowed in non-uniform control flow.
            sum += textureSampleCompareLevel(
                shadow_map, shadow_sampler, uv + vec2f(f32(x), f32(y)) * texel, i, ndc.z);
        }
    }
    return vec2f(sum / 9.0, edge);
}

// Sun shadow at a point seen by the camera: the cascade is picked by view
// depth and blended into the next one over its last 15%; the last cascade
// fades to lit at the shadow distance.
fn shadow_factor(world_pos: vec3f, normal: vec3f) -> f32 {
    let count = u32(light.cascade_info.x);
    let depth = dot(world_pos - camera.eye, light.cascade_forward.xyz);
    for (var i = 0u; i < count; i++) {
        let far = light.cascade_far[i];
        if (depth <= far) {
            let here = cascade_shadow(i, world_pos, normal).x;
            let fade = smoothstep(0.85 * far, far, depth);
            if (fade <= 0.0) {
                return here;
            }
            if (i + 1u < count) {
                return mix(here, cascade_shadow(i + 1u, world_pos, normal).x, fade);
            }
            return mix(here, 1.0, fade);
        }
    }
    return 1.0;
}

// Sun shadow at a point without a camera (the GI voxel passes): the finest
// cascade whose box holds it.
fn shadow_factor_volume(world_pos: vec3f, normal: vec3f) -> f32 {
    let count = u32(light.cascade_info.x);
    for (var i = 0u; i < count; i++) {
        let s = cascade_shadow(i, world_pos, normal);
        if (s.y <= 1.0) {
            return s.x;
        }
    }
    return 1.0;
}

// Diffuse ambient (irradiance / pi) of the environment around `n`.
fn ibl_irradiance(n: vec3f) -> vec3f {
    var c = ibl.sh[0].rgb * 0.282095;
    c += ibl.sh[1].rgb * (0.488603 * n.y) + ibl.sh[2].rgb * (0.488603 * n.z)
        + ibl.sh[3].rgb * (0.488603 * n.x);
    c += ibl.sh[4].rgb * (1.092548 * n.x * n.y) + ibl.sh[5].rgb * (1.092548 * n.y * n.z)
        + ibl.sh[6].rgb * (0.315392 * (3.0 * n.z * n.z - 1.0))
        + ibl.sh[7].rgb * (1.092548 * n.x * n.z)
        + ibl.sh[8].rgb * (0.546274 * (n.x * n.x - n.y * n.y));
    return max(c, vec3f(0.0));
}

// The environment along `dir`, blurred for `roughness` (equirect: u around
// +Y, v from up to down).
fn ibl_prefiltered(dir: vec3f, roughness: f32) -> vec3f {
    let uv = vec2f(atan2(dir.z, dir.x) / (2.0 * PI) + 0.5, acos(clamp(dir.y, -1.0, 1.0)) / PI);
    return textureSampleLevel(ibl_specular, ibl_sampler, uv, roughness * ibl.params.x).rgb;
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

// Diffuse light at fragment `in` (shading normal `n`, `v` toward the
// viewer) from this pipeline's GI backend, in place of the hemisphere
// ambient `hemi`. A new backend adds its branch here.
fn indirect_diffuse(in: VsOut, n: vec3f, v: vec3f, hemi: vec3f) -> vec3f {
    if (GI_MODE == GI_SSGI) {
        return ssgi_diffuse(in.world_pos, normalize(in.world_normal), in.clip_pos.xy, hemi);
    }
    return ddgi_diffuse(in.world_pos, n, v, hemi);
}

// Debug flag of this pipeline's GI backend: output indirect diffuse only.
fn gi_only() -> bool {
    if (GI_MODE == GI_SSGI) {
        return ssgi.gi_only != 0u;
    }
    return gi.gi_only != 0u;
}

// Shadow of a light with a shadow view (spot.w = its first tile; point lights
// have six, picked by the dominant axis of the vector from the light): 3x3
// PCF in the atlas, 1.0 where the point is outside the view.
fn local_shadow(light: LocalLight, world_pos: vec3f, normal: vec3f) -> f32 {
    let d = world_pos - light.position_range.xyz;
    var index = i32(light.spot.w);
    if (light.spot.z < 0.5) {
        let a = abs(d);
        var face = 0;
        if (a.x >= a.y && a.x >= a.z) {
            face = select(1, 0, d.x > 0.0);
        } else if (a.y >= a.z) {
            face = select(3, 2, d.y > 0.0);
        } else {
            face = select(5, 4, d.z > 0.0);
        }
        index += face;
    }
    let view = shadow_views[index];
    let dist = max(length(d), 1e-3);
    // Normal offset: about 1.5 shadow texels at this distance, more at grazing angles.
    let ndl = clamp(dot(normal, -d / dist), 0.0, 1.0);
    let offset = normal * (1.5 * view.params.x * dist * sqrt(1.0 - ndl * ndl));
    let clip = view.view_proj * vec4f(world_pos + offset, 1.0);
    if (clip.w <= 0.0) {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    if (any(abs(ndc.xy) > vec2f(1.0)) || ndc.z > 1.0 || ndc.z < 0.0) {
        return 1.0;
    }
    let texel = 1.0 / vec2f(textureDimensions(local_shadow_atlas));
    let lo = view.rect.xy + texel;
    let hi = view.rect.xy + view.rect.zw - texel;
    let uv = view.rect.xy + (ndc.xy * vec2f(0.5, -0.5) + 0.5) * view.rect.zw;
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let at = clamp(uv + vec2f(f32(x), f32(y)) * texel, lo, hi);
            sum += textureSampleCompareLevel(local_shadow_atlas, shadow_sampler, at, ndc.z);
        }
    }
    return sum / 9.0;
}

// Direction to the light (xyz) and its attenuation (w): inverse square
// windowed to the range, times the squared smooth cone for spots.
fn local_light_attenuation(light: LocalLight, world_pos: vec3f) -> vec4f {
    let to_light = light.position_range.xyz - world_pos;
    let d2 = max(dot(to_light, to_light), 1e-4);
    let r = light.position_range.w;
    let l = to_light * inverseSqrt(d2);
    let q = d2 / (r * r);
    let window = clamp(1.0 - q * q, 0.0, 1.0);
    var atten = window * window / d2;
    if (light.spot.z > 0.5) {
        let cone = clamp(dot(-l, light.direction.xyz) * light.spot.x + light.spot.y, 0.0, 1.0);
        atten *= cone * cone;
    }
    return vec4f(l, atten);
}

// Light from the point and spot lights in this fragment's cluster: the same
// Cook-Torrance as the sun, with radiance color / d^2 windowed to reach 0 at
// the light's range (Karis) and, for spots, a squared smooth cone.
fn local_light_radiance(
    frag: vec2f, world_pos: vec3f, n: vec3f, v: vec3f,
    base: vec3f, metallic: f32, alpha: f32, f0: vec3f,
) -> vec3f {
    let depth = max(dot(world_pos - camera.eye, cluster.forward.xyz), cluster.near);
    let z = min(u32(log(depth / cluster.near) * cluster.depth_scale), cluster.dims.z - 1u);
    let tile = min(vec2u(frag / cluster.screen * vec2f(cluster.dims.xy)), cluster.dims.xy - 1u);
    let range = cluster_ranges[tile.x + cluster.dims.x * (tile.y + cluster.dims.y * z)];
    let ndv = max(dot(n, v), 1e-4);
    var sum = vec3f(0.0);
    for (var i = 0u; i < range.y; i++) {
        let light = local_lights[cluster_indices[range.x + i]];
        let la = local_light_attenuation(light, world_pos);
        let l = la.xyz;
        let atten = la.w;
        let ndl = max(dot(n, l), 0.0);
        if (atten <= 0.0 || ndl <= 0.0) {
            continue;
        }
        var shadow = 1.0;
        if (light.spot.w >= 0.0) {
            shadow = local_shadow(light, world_pos, normalize(n));
        }
        let h = normalize(l + v);
        let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
        let spec = distribution_ggx(max(dot(n, h), 0.0), alpha) * visibility_smith_ggx(ndv, ndl, alpha) * f;
        let diff = (1.0 - f) * (1.0 - metallic) * base / PI;
        sum += (diff + spec) * light.color.rgb * atten * ndl * shadow;
    }
    return sum;
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

    let base_rgba = textureSample(base_color_tex, material_sampler, in.uv) * material.base_color;
    let base = base_rgba.rgb;
    let mr = textureSample(metallic_roughness_tex, material_sampler, in.uv);
    let emissive_texel = textureSample(emissive_tex, material_sampler, in.uv).rgb;
    if (material.alpha_cutoff > 0.0 && base_rgba.a < material.alpha_cutoff) {
        discard;
    }
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
        * shadow_factor(in.world_pos, normalize(in.world_normal));

    // Ambient. Diffuse: the sky/ground hemisphere. Specular: the sky in the
    // mirror direction, fading toward the hemisphere around the normal as
    // roughness blurs the reflection. SSAO occludes both.
    let ao = ambient_occlusion(in.clip_pos.xy);
    let hemi = ibl_irradiance(n);
    let f_amb = fresnel_schlick_roughness(ndv, f0, roughness);
    // Split sum: the prefiltered environment times the scale and bias of F0.
    let brdf = textureSampleLevel(ibl_brdf, ibl_sampler, vec2f(ndv, roughness), 0.0).rg;
    let reflected = ibl_prefiltered(reflect(-v, n), roughness) * (f0 * brdf.x + brdf.y);
    var ambient = (1.0 - f_amb) * (1.0 - metallic) * base * hemi * ao
        + reflected * specular_occlusion(ndv, ao, roughness);
    // GI on: the backend's light replaces the hemisphere in the diffuse
    // term. GI off is a pipeline built with GI_MODE GI_OFF: the branch
    // compiles away and the image matches the renderer without GI exactly
    // (a runtime branch alone shifts a few pixels by one step).
    if (GI_MODE != GI_OFF) {
        let diffuse_light = indirect_diffuse(in, n, v, hemi);
        let diffuse = (1.0 - f_amb) * (1.0 - metallic) * base * diffuse_light * ao;
        if (gi_only()) {
            return vec4f(diffuse, 1.0);
        }
        ambient = diffuse + reflected * specular_occlusion(ndv, ao, roughness);
    }

    let locals = local_light_radiance(in.clip_pos.xy, in.world_pos, n, v, base, metallic, alpha, f0);
    let emissive = emissive_texel * material.emissive;
    let color = sun + locals + ambient + emissive;

    let fog = 1.0 - exp(-env.fog_density * distance(in.world_pos, camera.eye));
    return vec4f(mix(color, env.horizon_color, fog), select(1.0, base_rgba.a, ALPHA_BLEND));
}

// Voxel passes: every instance in the GI volume, projected orthographically
// along one axis (voxel_axis.x) onto a viewport of one pixel per voxel, no
// depth test. Each fragment writes the radiance of its surface into the
// voxel it lies in; three passes (X, Y, Z) cover every orientation.
@vertex
fn vs_voxel(in: VsIn, @builtin(instance_index) instance: u32) -> VsOut {
    let model = instances[instance];
    var out: VsOut;
    let world_pos = (model.model * vec4f(in.position, 1.0)).xyz;
    let rel = (world_pos - gi.origin) / (vec3f(gi.voxel_counts) * gi.voxel_size);
    var uv = rel.xy;
    if (voxel_axis.x == 0u) {
        uv = rel.zy;
    } else if (voxel_axis.x == 1u) {
        uv = rel.xz;
    }
    out.clip_pos = vec4f(uv * 2.0 - 1.0, 0.5, 1.0);
    out.world_normal = (model.normal_matrix * vec4f(in.normal, 0.0)).xyz;
    out.uv = in.uv;
    out.world_pos = world_pos;
    out.world_tangent = vec4f(0.0);
    return out;
}

// Outgoing diffuse radiance: albedo x (shadowed sun + last frame's probe
// irradiance, the infinite-bounce feedback) + emissive. Merged per channel
// with atomicMax, so the lit face of a surface thinner than a voxel wins.
@fragment
fn fs_voxel(in: VsOut) -> @location(0) vec4f {
    let n = normalize(in.world_normal);
    let base_rgba = textureSample(base_color_tex, material_sampler, in.uv) * material.base_color;
    let base = base_rgba.rgb;
    let mr = textureSample(metallic_roughness_tex, material_sampler, in.uv);
    let emissive = textureSample(emissive_tex, material_sampler, in.uv).rgb * material.emissive;
    if (material.alpha_cutoff > 0.0 && base_rgba.a < material.alpha_cutoff) {
        discard;
    }
    let metallic = clamp(mr.b * material.metallic, 0.0, 1.0);
    let sun = (1.0 - light.ambient) * light.color * max(dot(n, light.direction), 0.0)
        * shadow_factor_volume(in.world_pos, n);
    // The first update after GI (re)starts (hysteresis 0) has no last
    // frame: the atlases may hold another scene.
    let feedback = select(0.0, gi.intensity, gi.hysteresis > 0.0);
    let bounce = feedback * gi_irradiance(in.world_pos, n, n, false);
    // Shadowed point / spot lights (the first cluster.dims.w of the light
    // list) inject their diffuse light; unshadowed ones do not, so light
    // never leaks through walls.
    var local = vec3f(0.0);
    for (var i = 0u; i < cluster.dims.w; i++) {
        let light = local_lights[i];
        let la = local_light_attenuation(light, in.world_pos);
        let ndl = max(dot(n, la.xyz), 0.0);
        if (la.w > 0.0 && ndl > 0.0) {
            local += light.color.rgb * la.w * ndl * local_shadow(light, in.world_pos, n) / PI;
        }
    }
    let radiance = (1.0 - metallic) * base * (sun + bounce + local) + emissive;
    // A hair inside the surface, so faces on a voxel boundary pick the solid side.
    let cell = vec3i(floor((in.world_pos - 0.01 * n - gi.origin) / gi.voxel_size));
    let counts = vec3i(gi.voxel_counts);
    if (all(cell >= vec3i(0)) && all(cell < counts)) {
        let i = 3u * u32(cell.x + counts.x * (cell.y + counts.y * cell.z));
        let q = vec3u(clamp(radiance, vec3f(0.0), vec3f(VOXEL_MAX)) * VOXEL_SCALE) + 1u;
        atomicMax(&voxels[i], q.x);
        atomicMax(&voxels[i + 1u], q.y);
        atomicMax(&voxels[i + 2u], q.z);
    }
    return vec4f(0.0);
}
