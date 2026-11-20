// DDGI probe update (compute), after the voxel passes. gi_common.wgsl is
// prepended. One workgroup per probe: each invocation traces one of the
// GI_RAYS rays through the voxel radiance volume (3D DDA, early exit on the
// first occupied voxel; a miss reads the sky light) into workgroup memory, then
// the workgroup blends them into every texel of the probe's irradiance and
// depth tiles with hysteresis. Border texels compute the interior texel
// they mirror, so the octahedral border needs no extra pass.

struct LightUniform {
    direction: vec3f,
    ambient: f32,
    color: vec3f,
    _pad: f32,
    sky_color: vec3f,
    _pad1: f32,
    ground_color: vec3f,
    _pad2: f32,
    light_view_proj: mat4x4f,
};

@group(0) @binding(0) var<uniform> gi: GiUniform;
@group(0) @binding(1) var<uniform> light: LightUniform;
// 3 u32 per voxel (see VOXEL_SCALE), x fastest, then y, then z.
@group(0) @binding(3) var<storage, read> voxels: array<u32>;
// Last frame's atlases (read) and this frame's (written, then copied over them).
@group(0) @binding(4) var irradiance_prev: texture_2d<f32>;
@group(0) @binding(5) var depth_prev: texture_2d<f32>;
@group(0) @binding(6) var irradiance_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var depth_out: texture_storage_2d<rgba16float, write>;

// This probe's rays: direction, and radiance (rgb) + hit distance (a).
var<workgroup> ray_dirs: array<vec3f, GI_RAYS>;
var<workgroup> ray_hits: array<vec4f, GI_RAYS>;

const PI: f32 = 3.14159265;
const GOLDEN_RATIO: f32 = 1.61803399;
const MISS_DISTANCE: f32 = 1.0e4;
// Sharpness of the depth moments' cosine lobe (DDGI's default).
const DEPTH_SHARPNESS: f32 = 50.0;

// Ray `i` of GI_RAYS: a spherical-Fibonacci point (gi.rs
// `spherical_fibonacci`) turned by this frame's random rotation.
fn ray_direction(i: u32) -> vec3f {
    let phi = 2.0 * PI * fract(f32(i) / GOLDEN_RATIO);
    let cos_theta = 1.0 - (2.0 * f32(i) + 1.0) / f32(GI_RAYS);
    let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    let d = vec3f(cos(phi) * sin_theta, sin(phi) * sin_theta, cos_theta);
    return normalize((gi.ray_rotation * vec4f(d, 0.0)).xyz);
}

// Sky light along a ray that leaves the volume: the hemisphere ambient's
// colours, `light.ambient * sky_color` above the horizon, `ground_color`
// below. Cosine-weighted over a normal's hemisphere this is exactly the
// GI-off ambient `ambient * mix(ground, sky, n.y * 0.5 + 0.5)` (an open
// plane tilted by t sees (1 + cos t) / 2 of the sky), so an unoccluded
// surface keeps the GI-off brightness. The drawn sky (sky.wgsl) is the
// background, several times brighter, and would wash the scene out.
fn sky(dir: vec3f) -> vec3f {
    return light.ambient * select(light.ground_color, light.sky_color, dir.y >= 0.0);
}

// 3D DDA from `o` along unit `d` through the voxel grid: the first occupied
// voxel's radiance and the distance to it, or the sky light and MISS_DISTANCE.
fn trace(o: vec3f, d: vec3f) -> vec4f {
    let counts = vec3i(gi.voxel_counts);
    let g = (o - gi.origin) / gi.voxel_size;
    var cell = vec3i(floor(g));
    let step = vec3i(sign(d));
    let t_delta = 1.0 / max(abs(d), vec3f(1e-8));
    let boundary = select(floor(g), floor(g) + 1.0, d > vec3f(0.0));
    var t_max = abs(boundary - g) * t_delta;
    var t = 0.0;
    let steps = counts.x + counts.y + counts.z;
    for (var i = 0; i < steps; i++) {
        if (any(cell < vec3i(0)) || any(cell >= counts)) {
            break;
        }
        let v = 3u * u32(cell.x + counts.x * (cell.y + counts.y * cell.z));
        if (voxels[v] != 0u) {
            let q = vec3u(voxels[v], voxels[v + 1u], voxels[v + 2u]) - 1u;
            return vec4f(vec3f(q) / VOXEL_SCALE, t * gi.voxel_size);
        }
        if (t_max.x < t_max.y && t_max.x < t_max.z) {
            cell.x += step.x;
            t = t_max.x;
            t_max.x += t_delta.x;
        } else if (t_max.y < t_max.z) {
            cell.y += step.y;
            t = t_max.y;
            t_max.y += t_delta.y;
        } else {
            cell.z += step.z;
            t = t_max.z;
            t_max.z += t_delta.z;
        }
    }
    return vec4f(sky(d), MISS_DISTANCE);
}

// Probe index -> grid cell (x fastest, then y, then z).
fn probe_cell(probe: u32) -> vec3i {
    let c = gi.probe_counts;
    return vec3i(vec3u(probe % c.x, (probe / c.x) % c.y, probe / (c.x * c.y)));
}

// Tile-local texel (0..n+1 per axis) -> the interior texel (1..n) it
// copies: a border texel mirrors across its edge (octahedral wrap), a
// corner takes the opposite corner.
fn interior_texel(local: vec2u, n: u32) -> vec2u {
    var p = local;
    if (p.x == 0u || p.x == n + 1u) {
        p = vec2u(clamp(p.x, 1u, n), n + 1u - p.y);
    }
    if (p.y == 0u || p.y == n + 1u) {
        p = vec2u(n + 1u - p.x, clamp(p.y, 1u, n));
    }
    return p;
}

// Direction of interior texel `local` (1..n) of an n x n octahedral tile.
fn texel_direction(local: vec2u, n: u32) -> vec3f {
    return oct_decode((vec2f(local) - 0.5) / f32(n) * 2.0 - 1.0);
}

// One workgroup per probe, one invocation per ray.
@compute @workgroup_size(64)
fn cs_update(@builtin(workgroup_id) wg: vec3u, @builtin(local_invocation_index) ray: u32) {
    let cell = probe_cell(wg.x);
    let dir = ray_direction(ray);
    ray_dirs[ray] = dir;
    ray_hits[ray] = trace(probe_position(cell), dir);
    workgroupBarrier();

    // Irradiance as the cosine-weighted mean radiance (irradiance / PI): the
    // mesh shader multiplies it by the albedo, like the hemisphere ambient.
    let n = IRRADIANCE_TEXELS;
    let tile = n + 2u;
    let origin = probe_tile(cell, tile);
    for (var t = ray; t < tile * tile; t += GI_RAYS) {
        let local = vec2u(t % tile, t / tile);
        let source = interior_texel(local, n);
        let texel_dir = texel_direction(source, n);
        var sum = vec3f(0.0);
        var weight = 0.0;
        for (var i = 0u; i < GI_RAYS; i++) {
            let w = max(dot(texel_dir, ray_dirs[i]), 0.0);
            sum += w * ray_hits[i].rgb;
            weight += w;
        }
        let prev = textureLoad(irradiance_prev, origin + source, 0).rgb;
        var value = prev;
        if (weight > 1e-4) {
            value = mix(sum / weight, prev, gi.hysteresis);
        }
        textureStore(irradiance_out, origin + local, vec4f(value, 1.0));
    }

    // Mean and mean square of the ray distance around each direction
    // (Chebyshev visibility in mesh.wgsl).
    let dn = DEPTH_TEXELS;
    let dtile = dn + 2u;
    let dorigin = probe_tile(cell, dtile);
    for (var t = ray; t < dtile * dtile; t += GI_RAYS) {
        let local = vec2u(t % dtile, t / dtile);
        let source = interior_texel(local, dn);
        let texel_dir = texel_direction(source, dn);
        var sum = vec2f(0.0);
        var weight = 0.0;
        for (var i = 0u; i < GI_RAYS; i++) {
            let w = pow(max(dot(texel_dir, ray_dirs[i]), 0.0), DEPTH_SHARPNESS);
            let d = min(ray_hits[i].a, gi.max_depth);
            sum += w * vec2f(d, d * d);
            weight += w;
        }
        let prev = textureLoad(depth_prev, dorigin + source, 0).rg;
        var value = prev;
        if (weight > 1e-4) {
            value = mix(sum / weight, prev, gi.hysteresis);
        }
        textureStore(depth_out, dorigin + local, vec4f(value, 0.0, 1.0));
    }
}
