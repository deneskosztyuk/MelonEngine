// DDGI declarations shared by its probe update (ddgi_update.wgsl) and its
// shading (gi_ddgi.wgsl, mesh.wgsl voxel passes): the renderer prepends
// this file to both (WGSL has no include). Mirrors `gi/ddgi.rs`. The
// including module declares the `gi` uniform.

struct GiUniform {
    // Min corner of the GI volume (world, metres).
    origin: vec3f,
    // Scales the probe irradiance; 0 = DDGI off.
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

// Rays per probe per frame; also the trace workgroup size (ddgi.rs GI_RAYS).
const GI_RAYS: u32 = 64u;
// Interior texels per probe side in the atlases (plus a 1-texel border).
const IRRADIANCE_TEXELS: u32 = 8u;
const DEPTH_TEXELS: u32 = 16u;
// Voxel radiance is stored per channel as u32(radiance * VOXEL_SCALE) + 1
// (0 = empty voxel), merged with atomicMax.
const VOXEL_SCALE: f32 = 4096.0;
const VOXEL_MAX: f32 = 1.0e6;

// World position of probe `cell`.
fn probe_position(cell: vec3i) -> vec3f {
    return gi.origin + (vec3f(cell) + 0.5) * gi.spacing;
}

// Atlas texel of the top-left border texel of probe `cell`'s tile; tiles
// are laid out as (x + X * y, z).
fn probe_tile(cell: vec3i, tile: u32) -> vec2u {
    return vec2u(u32(cell.x) + gi.probe_counts.x * u32(cell.y), u32(cell.z)) * tile;
}
