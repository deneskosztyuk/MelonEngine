// Mesh shader: glTF metallic-roughness PBR. Vertex has position + normal +
// uv + tangent. Group 0: camera, light (sun + sky/ground hemisphere + shadow
// matrix), shadow map, environment. Group 2: the material (base colour,
// metallic-roughness, normal and emissive textures, one sampler, factors).
// Group 3: per-view lighting inputs (the SSAO texture) and the DDGI probe
// atlases (shared world data). gi_common.wgsl is prepended.
// Sun: Cook-Torrance (GGX, height-correlated Smith, Schlick Fresnel), shadowed.
// Ambient: diffuse from the DDGI probes inside the GI volume (hemisphere
// outside it, or with GI off) plus a sky-gradient specular, both occluded by
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
// DDGI probe atlases: irradiance (rgb, cosine-weighted mean radiance) and
// distance moments (r = mean, g = mean square), octahedral tiles with a
// 1-texel border; linear clamp sampler; volume and probe grid settings.
@group(3) @binding(1) var irradiance_atlas: texture_2d<f32>;
@group(3) @binding(2) var depth_atlas: texture_2d<f32>;
@group(3) @binding(3) var gi_sampler: sampler;
@group(3) @binding(4) var<uniform> gi: GiUniform;
// The voxel volume (read-only; see VOXEL_SCALE), for probe visibility. The
// voxel passes bind a dummy here and never read it.
@group(3) @binding(5) var<storage, read> voxel_grid: array<u32>;
// Voxel passes only (their own group 0): the voxel radiance volume (see
// VOXEL_SCALE) and the projection axis (x: 0 = X, 1 = Y, 2 = Z).
@group(0) @binding(5) var<storage, read_write> voxels: array<atomic<u32>>;
@group(0) @binding(6) var<uniform> voxel_axis: vec4u;

const PI: f32 = 3.14159265;
// False in the GI-off mesh pipeline (see the ambient term in fs_main).
override GI_ENABLED: bool = true;

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
// `normal` is the geometric world normal: the lookup moves along it by about
// 1.5 shadow texels, more as the sun grazes the surface (normal-offset
// sampling), so lit faces do not shadow themselves.
fn shadow_factor(world_pos: vec3f, normal: vec3f) -> f32 {
    let m = light.light_view_proj;
    // The ortho box maps 2 * radius world units onto the map's width.
    let texel_world = 2.0 / (length(vec3f(m[0].x, m[1].x, m[2].x))
        * f32(textureDimensions(shadow_map).x));
    let ndl = clamp(dot(normal, light.direction), 0.0, 1.0);
    let offset = normal * (1.5 * texel_world * sqrt(1.0 - ndl * ndl));
    let ls = m * vec4f(world_pos + offset, 1.0);
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

// Atlas uv of direction `dir` in probe `cell`'s tile (`n` interior texels).
fn probe_uv(cell: vec3i, dir: vec3f, n: u32, atlas: texture_2d<f32>) -> vec2f {
    let px = vec2f(probe_tile(cell, n + 2u)) + 1.0 + (oct_encode(dir) * 0.5 + 0.5) * f32(n);
    return px / vec2f(textureDimensions(atlas));
}

// Whether the segment from `a` to probe `b` crosses no occupied voxel
// (3D DDA). The voxel of `a` is skipped: a surface's own voxel, or a
// neighbouring surface's, can stick out past the shading point by up to a
// voxel at inside corners. The march stops a voxel short of the probe:
// probes sit on voxel corners, where the last steps would tie.
fn voxel_clear(a: vec3f, b: vec3f) -> bool {
    let counts = vec3i(gi.voxel_counts);
    let ga = (a - gi.origin) / gi.voxel_size;
    let d = (b - a) / gi.voxel_size;
    var cell = vec3i(floor(ga));
    let step = vec3i(sign(d));
    // In fractions of the segment.
    let t_delta = 1.0 / max(abs(d), vec3f(1e-8));
    var t_max = abs(select(floor(ga), floor(ga) + 1.0, d > vec3f(0.0)) - ga) * t_delta;
    let t_stop = 1.0 - 1.0 / max(length(d), 1.0);
    // A probe is at most 2.5 m * sqrt(3) (14 voxels) from the point.
    for (var i = 0; i < 48; i++) {
        if (min(t_max.x, min(t_max.y, t_max.z)) >= t_stop) {
            break;
        }
        if (t_max.x < t_max.y && t_max.x < t_max.z) {
            cell.x += step.x;
            t_max.x += t_delta.x;
        } else if (t_max.y < t_max.z) {
            cell.y += step.y;
            t_max.y += t_delta.y;
        } else {
            cell.z += step.z;
            t_max.z += t_delta.z;
        }
        if (all(cell >= vec3i(0)) && all(cell < counts)
            && voxel_grid[3u * u32(cell.x + counts.x * (cell.y + counts.y * cell.z))] != 0u) {
            return false;
        }
    }
    return true;
}

// Diffuse irradiance (cosine-weighted mean radiance) at `p` with normal `n`
// from the 8 surrounding probes. Weights: trilinear, backface (probes
// behind the surface count less), Chebyshev visibility from the distance
// moments (probes that cannot see the point drop out; the moments end at
// the hit voxel's near side, up to a voxel before the real surface, so a
// point less than a voxel behind them still counts as seen), and with
// `trace_visibility` a voxel march from the point to each probe: probes
// behind geometry drop out, which stops the variance leaks of probes that
// see past a wall's edge (bright rims in inside corners). If the march
// blocks every probe, the weights without it are used. `v` points to the
// viewer: the lookup point moves toward it and along the normal (DDGI's
// self-shadow bias) so a surface does not shadow itself.
fn gi_irradiance(p: vec3f, n: vec3f, v: vec3f, trace_visibility: bool) -> vec3f {
    let biased = p + (0.2 * n + 0.8 * v) * (0.3 * 0.75 * gi.spacing);
    let grid = (biased - gi.origin) / gi.spacing - 0.5;
    let base = clamp(vec3i(floor(grid)), vec3i(0), vec3i(gi.probe_counts) - 2);
    let alpha = clamp(grid - vec3f(base), vec3f(0.0), vec3f(1.0));
    var sum = vec3f(0.0);
    var weight_sum = 0.0;
    var seen_sum = vec3f(0.0);
    var seen_weight = 0.0;
    for (var i = 0u; i < 8u; i++) {
        let offset = vec3u(i, i >> 1u, i >> 2u) & vec3u(1u);
        let cell = base + vec3i(offset);
        let probe = probe_position(cell);
        let to_probe = normalize(probe - p);
        let wrap = (dot(to_probe, n) + 1.0) * 0.5;
        var w = wrap * wrap + 0.2;
        let from_probe = biased - probe;
        let r = length(from_probe);
        let moments = textureSampleLevel(depth_atlas, gi_sampler,
            probe_uv(cell, from_probe / max(r, 1e-4), DEPTH_TEXELS, depth_atlas), 0.0).rg;
        let d = r - moments.x - gi.voxel_size;
        if (d > 0.0) {
            let variance = abs(moments.y - moments.x * moments.x);
            let chebyshev = variance / (variance + d * d);
            w *= chebyshev * chebyshev * chebyshev;
        }
        w = max(w, 1e-6);
        // Crush tiny weights further, so probes that see the point win
        // over leaky ones.
        if (w < 0.2) {
            w *= w * w / (0.2 * 0.2);
        }
        let t = select(1.0 - alpha, alpha, offset == vec3u(1u));
        w *= t.x * t.y * t.z;
        let irradiance = textureSampleLevel(irradiance_atlas, gi_sampler,
            probe_uv(cell, n, IRRADIANCE_TEXELS, irradiance_atlas), 0.0).rgb;
        sum += w * irradiance;
        weight_sum += w;
        if (trace_visibility && voxel_clear(biased, probe)) {
            seen_sum += w * irradiance;
            seen_weight += w;
        }
    }
    if (seen_weight > 0.0) {
        return seen_sum / seen_weight;
    }
    return sum / max(weight_sum, 1e-30);
}

// 1 inside the GI volume, fading to 0 one probe spacing outside it.
fn gi_volume_weight(p: vec3f) -> f32 {
    let hi = gi.origin + vec3f(gi.probe_counts) * gi.spacing;
    let outside = max(max(gi.origin - p, p - hi), vec3f(0.0));
    return clamp(1.0 - max(outside.x, max(outside.y, outside.z)) / gi.spacing, 0.0, 1.0);
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
        * shadow_factor(in.world_pos, normalize(in.world_normal));

    // Ambient. Diffuse: the sky/ground hemisphere. Specular: the sky in the
    // mirror direction, fading toward the hemisphere around the normal as
    // roughness blurs the reflection. SSAO occludes both.
    let ao = ambient_occlusion(in.clip_pos.xy);
    let hemi = light.ambient * mix(light.ground_color, light.sky_color, n.y * 0.5 + 0.5);
    let f_amb = fresnel_schlick_roughness(ndv, f0, roughness);
    let reflected = mix(sky_radiance(reflect(-v, n)), hemi, roughness);
    var ambient = (1.0 - f_amb) * (1.0 - metallic) * base * hemi * ao
        + f_amb * reflected * specular_occlusion(ndv, ao, roughness);
    // GI on: the probes replace the hemisphere in the diffuse term, blending
    // back to it at the volume border. GI off is a pipeline built with
    // GI_ENABLED false: the branch compiles away and the image matches the
    // renderer without GI exactly (a runtime branch alone shifts a few
    // pixels by one step).
    if (GI_ENABLED) {
        let inside = gi_volume_weight(in.world_pos);
        var diffuse_light = hemi;
        if (inside > 0.0) {
            diffuse_light = mix(hemi, gi.intensity * gi_irradiance(in.world_pos, n, v, true), inside);
        }
        let diffuse = (1.0 - f_amb) * (1.0 - metallic) * base * diffuse_light * ao;
        if (gi.gi_only != 0u) {
            return vec4f(diffuse, 1.0);
        }
        ambient = diffuse + f_amb * reflected * specular_occlusion(ndv, ao, roughness);
    }

    let emissive = (textureSample(emissive_tex, material_sampler, in.uv).rgb * material.emissive);
    let color = sun + ambient + emissive;

    let fog = 1.0 - exp(-env.fog_density * distance(in.world_pos, camera.eye));
    return vec4f(mix(color, env.horizon_color, fog), 1.0);
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
    let base = (textureSample(base_color_tex, material_sampler, in.uv) * material.base_color).rgb;
    let mr = textureSample(metallic_roughness_tex, material_sampler, in.uv);
    let emissive = textureSample(emissive_tex, material_sampler, in.uv).rgb * material.emissive;
    let metallic = clamp(mr.b * material.metallic, 0.0, 1.0);
    let sun = (1.0 - light.ambient) * light.color * max(dot(n, light.direction), 0.0)
        * shadow_factor(in.world_pos, n);
    // The first update after GI (re)starts (hysteresis 0) has no last
    // frame: the atlases may hold another scene.
    let feedback = select(0.0, gi.intensity, gi.hysteresis > 0.0);
    let bounce = feedback * gi_irradiance(in.world_pos, n, n, false);
    let radiance = (1.0 - metallic) * base * (sun + bounce) + emissive;
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
