// DDGI declarations shared by mesh.wgsl and gi.wgsl: the renderer prepends
// this file to both sources (WGSL has no include). Mirrors `gi.rs`.

struct GiUniform {
    // Min corner of the GI volume (world, metres).
    origin: vec3f,
    // Scales the probe irradiance; 0 = GI off (hemisphere ambient only).
    intensity: f32,
    probe_counts: vec3u,
    // 1 = the mesh shader outputs indirect diffuse only (debug).
    gi_only: u32,
    voxel_counts: vec3u,
    voxel_size: f32,
    // Metres between probes; probe i sits at origin + (i + 0.5) * spacing.
    spacing: f32,
    // Weight of last frame's probe texels in the blend.
    hysteresis: f32,
    // Ray distances (and misses) clamp to this in the depth moments.
    max_depth: f32,
    _pad: f32,
    // This frame's random rotation of the ray directions.
    ray_rotation: mat4x4f,
};

// Rays per probe per frame; also the trace workgroup size (gi.rs GI_RAYS).
const GI_RAYS: u32 = 64u;
// Interior texels per probe side in the atlases (plus a 1-texel border).
const IRRADIANCE_TEXELS: u32 = 8u;
const DEPTH_TEXELS: u32 = 16u;
// Voxel radiance is stored per channel as u32(radiance * VOXEL_SCALE) + 1
// (0 = empty voxel), merged with atomicMax.
const VOXEL_SCALE: f32 = 4096.0;
const VOXEL_MAX: f32 = 1.0e6;

fn sign_not_zero(v: vec2f) -> vec2f {
    return select(vec2f(-1.0), vec2f(1.0), v >= vec2f(0.0));
}

// Unit direction -> octahedral square [-1, 1]^2.
fn oct_encode(n: vec3f) -> vec2f {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    if (n.z < 0.0) {
        return (1.0 - abs(p.yx)) * sign_not_zero(p);
    }
    return p;
}

// Octahedral square [-1, 1]^2 -> unit direction.
fn oct_decode(e: vec2f) -> vec3f {
    let z = 1.0 - abs(e.x) - abs(e.y);
    var xy = e;
    if (z < 0.0) {
        xy = (1.0 - abs(e.yx)) * sign_not_zero(e);
    }
    return normalize(vec3f(xy, z));
}

// World position of probe `cell`.
fn probe_position(cell: vec3i) -> vec3f {
    return gi.origin + (vec3f(cell) + 0.5) * gi.spacing;
}

// Atlas texel of the top-left border texel of probe `cell`'s tile; tiles
// are laid out as (x + X * y, z).
fn probe_tile(cell: vec3i, tile: u32) -> vec2u {
    return vec2u(u32(cell.x) + gi.probe_counts.x * u32(cell.y), u32(cell.z)) * tile;
}
